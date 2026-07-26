use std::{
    ffi::OsStr,
    fs,
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    sync::mpsc::{self, Sender, TryRecvError},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::{context::Context, process, DevCommand};

const TUF_ADDRESS: &str = "127.0.0.1:8080";
const ARCHIVE_ADDRESS: &str = "127.0.0.1:8765";
const STARTUP_TIMEOUT: Duration = Duration::from_secs(45);

pub(crate) fn run(context: &Context, command: DevCommand) -> Result<()> {
    ensure_linux()?;

    match command {
        DevCommand::Up => up(context),
        DevCommand::Down => down(context),
        DevCommand::Reset => reset(context),
        DevCommand::Publish { recipe, target } => {
            up(context)?;
            publish_recipe(context, &recipe, target.as_deref())?;
            Ok(())
        }
        DevCommand::Pako { arguments } => {
            up(context)?;
            run_pako(context, &arguments)
        }
        DevCommand::Smoke => smoke(context),
    }
}

fn up(context: &Context) -> Result<()> {
    build_tools(context)?;
    ensure_development_directories(context)?;
    ensure_tuf(context)?;
    configure_client(context)?;

    let compose = ComposeRuntime::detect()?;
    compose.up(context)?;

    wait_for_http(TUF_ADDRESS, "/metadata/root.json", STARTUP_TIMEOUT)
        .context("local TUF server did not become ready")?;

    println!("Pako development environment is ready");
    println!("TUF: http://{TUF_ADDRESS}");
    println!("State: {}", context.dev().display());
    Ok(())
}

fn down(context: &Context) -> Result<()> {
    ComposeRuntime::detect()?.down(context, false)
}

fn reset(context: &Context) -> Result<()> {
    if let Some(compose) = ComposeRuntime::find() {
        compose.down(context, true)?;
    }

    if context.dev().exists() {
        make_removable(context.dev())?;
        fs::remove_dir_all(context.dev()).with_context(|| {
            format!(
                "failed to remove development state at {}",
                context.dev().display()
            )
        })?;
    }

    up(context)
}

fn make_removable(root: &Path) -> Result<()> {
    for entry in WalkDir::new(root).contents_first(true).follow_links(false) {
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        let mode = if metadata.is_dir() { 0o700 } else { 0o600 };
        fs::set_permissions(entry.path(), fs::Permissions::from_mode(mode))?;
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn smoke(context: &Context) -> Result<()> {
    // Smoke tests must not reuse package or TUF state from a previous
    // development run. In particular, the catalog format can change between
    // revisions and is intentionally not migrated in-place.
    reset(context)?;
    reset_client(context)?;

    let hosted_v1 = context.root().join("examples/hello-local/recipe.toml");
    let hosted_v2 = write_hosted_v2_recipe(context, &hosted_v1)?;
    let published = publish_recipe(context, &hosted_v1, None)?;

    run_pako(
        context,
        &["-y".into(), "install".into(), published.package.clone()],
    )?;
    verify_pako(context, &published.package)?;
    assert_active_version(context, &published.package, "1.0.0-3")?;

    let launcher = context.client().join("home/.local/bin/hello-pako");
    process::run(Command::new(&launcher).current_dir(context.root()))
        .context("installed hello-local launcher failed")?;

    publish_recipe(context, &hosted_v2, None)?;
    run_pako(
        context,
        &["-y".into(), "upgrade".into(), published.package.clone()],
    )?;
    verify_pako(context, &published.package)?;
    assert_active_version(context, &published.package, "2.0.0-3")?;
    assert_tuf_targets(
        context,
        &[
            "manifests/hello-local/1.0.0-3/linux-x86_64.json",
            "manifests/hello-local/2.0.0-3/linux-x86_64.json",
            "artifacts/hello-local/1.0.0-3/linux-x86_64.tar.zst",
            "artifacts/hello-local/2.0.0-3/linux-x86_64.tar.zst",
        ],
    )?;

    run_pako(
        context,
        &["-y".into(), "rollback".into(), published.package.clone()],
    )?;
    run_pako(context, &["verify".into(), published.package.clone()])?;
    assert_active_version(context, &published.package, "1.0.0-3")?;
    run_pako(
        context,
        &[
            "-y".into(),
            "prune".into(),
            published.package.clone(),
            "--keep".into(),
            "1".into(),
        ],
    )?;
    if context
        .client()
        .join("data/pako/cellar/hello-local/2.0.0-3")
        .exists()
    {
        anyhow::bail!("hosted prune retained the removed version");
    }
    run_pako(
        context,
        &["-y".into(), "remove".into(), published.package.clone()],
    )?;
    assert_package_removed(context, &published.package)?;
    assert_path_absent(&context.client().join("home/.local/bin/hello-pako"))?;

    let archive = context
        .root()
        .join("examples/intellij-idea/idea-linux-x86_64.tar");
    let _archive_server = ArchiveServer::start(archive)?;
    let external_v1 = write_external_recipe(context, "2026.1-fixture")?;
    let external = publish_recipe(context, &external_v1, None)?;
    run_pako(
        context,
        &["-y".into(), "install".into(), external.package.clone()],
    )?;
    verify_pako(context, &external.package)?;
    assert_active_version(context, &external.package, "2026.1-fixture-1")?;
    assert_external_transform(context, "2026.1-fixture-1")?;
    let cache_files = count_files(&context.client().join("cache/pako/artifacts/sha256"));

    let external_v2 = write_external_recipe(context, "2026.2-fixture")?;
    publish_recipe(context, &external_v2, None)?;
    run_pako(
        context,
        &["-y".into(), "upgrade".into(), external.package.clone()],
    )?;
    verify_pako(context, &external.package)?;
    assert_active_version(context, &external.package, "2026.2-fixture-1")?;
    assert_external_transform(context, "2026.2-fixture-1")?;
    if count_files(&context.client().join("cache/pako/artifacts/sha256")) != cache_files {
        anyhow::bail!("external upgrade did not reuse the artifact cache");
    }
    run_pako(
        context,
        &["-y".into(), "rollback".into(), external.package.clone()],
    )?;
    verify_pako(context, &external.package)?;
    assert_active_version(context, &external.package, "2026.1-fixture-1")?;
    assert_external_transform(context, "2026.1-fixture-1")?;
    run_pako(
        context,
        &["-y".into(), "remove".into(), external.package.clone()],
    )?;
    assert_package_removed(context, &external.package)?;
    assert_path_absent(&context.client().join("home/.local/bin/intellij-idea"))?;
    assert_path_absent(
        &context
            .client()
            .join("data/pako/applications/pako-intellij-idea.desktop"),
    )?;
    assert_tuf_targets(
        context,
        &[
            "manifests/hello-local/1.0.0-3/linux-x86_64.json",
            "manifests/hello-local/2.0.0-3/linux-x86_64.json",
            "artifacts/hello-local/1.0.0-3/linux-x86_64.tar.zst",
            "artifacts/hello-local/2.0.0-3/linux-x86_64.tar.zst",
            "manifests/intellij-idea/2026.1-fixture-1/linux-x86_64.json",
            "manifests/intellij-idea/2026.2-fixture-1/linux-x86_64.json",
        ],
    )?;

    run_pako(context, &["status".into()])?;
    println!("Pako development lifecycle smoke tests completed successfully");
    Ok(())
}

fn write_hosted_v2_recipe(context: &Context, source: &Path) -> Result<PathBuf> {
    let payload = source
        .parent()
        .context("hosted recipe has no parent")?
        .join("payload/hello-pako")
        .canonicalize()?;
    let smoke_directory = context.build().join("smoke-recipes");
    fs::create_dir_all(smoke_directory.join("payload"))?;
    fs::copy(&payload, smoke_directory.join("payload/hello-pako"))?;
    let recipe = fs::read_to_string(source)?.replace("version = \"1.0.0\"", "version = \"2.0.0\"");
    write_smoke_recipe(context, "hello-local-v2.toml", recipe)
}

fn write_external_recipe(context: &Context, version: &str) -> Result<PathBuf> {
    let mut recipe = fs::read_to_string(context.root().join("examples/intellij-idea/recipe.toml"))?
        .replace(
            "version = \"2026.1-fixture\"",
            &format!("version = \"{version}\""),
        );
    for platform in ["x86_64", "aarch64"] {
        let original = format!("url = \"http://127.0.0.1:8765/idea-linux-{platform}.tar\"");
        let fallback = format!(
            "url = \"http://127.0.0.1:8765/missing-{platform}.tar\"\nmirrors = [\"http://127.0.0.1:8765/idea-linux-{platform}.tar\"]"
        );
        recipe = recipe.replace(&original, &fallback);
    }
    write_smoke_recipe(context, &format!("intellij-idea-{version}.toml"), recipe)
}

fn write_smoke_recipe(context: &Context, name: &str, recipe: String) -> Result<PathBuf> {
    let directory = context.build().join("smoke-recipes");
    fs::create_dir_all(&directory)?;
    let path = directory.join(name);
    fs::write(&path, recipe)?;
    Ok(path)
}

fn assert_active_version(context: &Context, package: &str, expected: &str) -> Result<()> {
    let state = context
        .client()
        .join(format!("state/pako/packages/{package}.json"));
    let value: serde_json::Value = serde_json::from_slice(&fs::read(&state)?)?;
    let actual = value["active"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("package state has no active version"))?;
    if actual != expected {
        anyhow::bail!("expected {package} {expected}, found {actual}");
    }
    Ok(())
}

fn assert_package_removed(context: &Context, package: &str) -> Result<()> {
    let root = context.client().join("data/pako");
    let state = context
        .client()
        .join(format!("state/pako/packages/{package}.json"));
    for path in [
        state,
        root.join(format!("cellar/{package}")),
        root.join(format!("apps/{package}/current")),
        root.join(format!("manifests/{package}")),
    ] {
        if path.exists() {
            anyhow::bail!("remove left package state at {}", path.display());
        }
    }
    Ok(())
}

fn assert_path_absent(path: &Path) -> Result<()> {
    if path.exists() {
        anyhow::bail!("remove left path {}", path.display());
    }
    Ok(())
}

fn assert_external_transform(context: &Context, version: &str) -> Result<()> {
    let path = context.client().join(format!(
        "data/pako/cellar/intellij-idea/{version}/Install-Linux-tar.txt"
    ));
    if path.exists() {
        anyhow::bail!(
            "external remove transform did not remove {}",
            path.display()
        );
    }
    Ok(())
}

fn assert_tuf_targets(context: &Context, expected: &[&str]) -> Result<()> {
    let metadata = context.tuf().join("metadata/targets.json");
    let value: serde_json::Value = serde_json::from_slice(&fs::read(metadata)?)?;
    let targets = value["signed"]["targets"]
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("TUF targets metadata is not a map"))?;
    for target in expected {
        if !targets.contains_key(*target) {
            anyhow::bail!("TUF targets metadata lost {target}");
        }
    }
    Ok(())
}

fn count_files(directory: &Path) -> usize {
    if !directory.exists() {
        return 0;
    }
    WalkDir::new(directory)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .count()
}

fn build_tools(context: &Context) -> Result<()> {
    process::run(
        Command::new("cargo")
            .args(["build", "--package", "pako-build", "--package", "pako-cli"])
            .current_dir(context.root()),
    )
}

fn ensure_development_directories(context: &Context) -> Result<()> {
    for directory in [context.dev().to_path_buf(), context.build()] {
        fs::create_dir_all(&directory)
            .with_context(|| format!("failed to create {}", directory.display()))?;
    }
    Ok(())
}

fn ensure_tuf(context: &Context) -> Result<()> {
    let tuf = context.tuf();
    let root = tuf.join("metadata/root.json");
    let catalog = tuf.join("targets/catalog.json");
    let key = tuf.join("keys/targets-and-metadata.ed25519.pk8");

    if root.is_file() && catalog.is_file() && key.is_file() {
        return Ok(());
    }

    if tuf.exists() {
        let mut entries =
            fs::read_dir(&tuf).with_context(|| format!("failed to inspect {}", tuf.display()))?;
        if entries.next().transpose()?.is_some() {
            anyhow::bail!(
                "incomplete TUF state at {}; run `cargo xtask dev reset`",
                tuf.display()
            );
        }
        fs::remove_dir(&tuf)?;
    }

    process::run(
        Command::new(context.pako_build())
            .arg("tuf")
            .arg("init")
            .arg(&tuf)
            .current_dir(context.root()),
    )
}

fn configure_client(context: &Context) -> Result<()> {
    let client = context.client();
    let config = client.join("config/pako");

    for directory in [
        client.join("home"),
        client.join("data"),
        client.join("state"),
        client.join("cache"),
        config.clone(),
    ] {
        fs::create_dir_all(&directory)
            .with_context(|| format!("failed to create {}", directory.display()))?;
    }

    let source_root = context.tuf().join("metadata/root.json");
    let client_root = config.join("root.json");
    copy_trust_root(&source_root, &client_root)?;

    let repository = RepositoryConfig {
        name: "local",
        root: client_root
            .canonicalize()
            .context("failed to resolve client trust root")?,
        metadata_url: "http://127.0.0.1:8080/metadata/",
        targets_url: "http://127.0.0.1:8080/targets/",
        allow_insecure_http: true,
    };

    fs::write(
        config.join("repository.json"),
        serde_json::to_vec_pretty(&repository)?,
    )?;
    Ok(())
}

fn copy_trust_root(source: &Path, destination: &Path) -> Result<()> {
    let source_bytes = fs::read(source)
        .with_context(|| format!("failed to read trust root {}", source.display()))?;

    if destination.exists() {
        let destination_bytes = fs::read(destination)?;
        if source_bytes != destination_bytes {
            anyhow::bail!(
                "the development TUF root changed; run `cargo xtask dev reset` \
                 to trust the new root"
            );
        }
        return Ok(());
    }

    fs::write(destination, source_bytes)
        .with_context(|| format!("failed to write trust root {}", destination.display()))
}

fn reset_client(context: &Context) -> Result<()> {
    let client = context.client();
    if client.exists() {
        fs::remove_dir_all(&client)
            .with_context(|| format!("failed to reset {}", client.display()))?;
    }
    configure_client(context)
}

fn publish_recipe(
    context: &Context,
    recipe: &Path,
    requested_target: Option<&str>,
) -> Result<PublishedPackage> {
    let recipe = recipe
        .canonicalize()
        .with_context(|| format!("recipe does not exist: {}", recipe.display()))?;
    let target = requested_target.map_or_else(host_target, validate_target)?;
    let output = context.build().join("current");

    if output.exists() {
        fs::remove_dir_all(&output)?;
    }
    fs::create_dir_all(&output)?;

    process::run(
        Command::new(context.pako_build())
            .arg("lint")
            .arg(&recipe)
            .current_dir(context.root()),
    )?;
    process::run(
        Command::new(context.pako_build())
            .arg("build")
            .arg(&recipe)
            .arg("--target")
            .arg(&target)
            .arg("--output")
            .arg(&output)
            .current_dir(context.root()),
    )?;

    let manifest_path = find_single_file(&output, "package-manifest.json")?;
    let artifact = manifest_path
        .parent()
        .context("package manifest has no parent directory")?;
    let manifest: ManifestSummary =
        serde_json::from_slice(&fs::read(&manifest_path)?).context("invalid package manifest")?;
    match manifest.artifact.kind.as_str() {
        "tuf-archive" => {
            if !artifact.join("package.tar.zst").is_file() {
                anyhow::bail!("hosted build did not produce package.tar.zst");
            }
        }
        "external-archive" => {
            if artifact.join("package.tar.zst").exists() {
                anyhow::bail!("external build must not produce package.tar.zst");
            }
        }
        other => anyhow::bail!("unsupported artifact type in manifest: {other:?}"),
    }
    process::run(
        Command::new(context.pako_build())
            .arg("publish")
            .arg(artifact)
            .arg("--tuf")
            .arg(context.tuf())
            .current_dir(context.root()),
    )?;

    println!("Published {} to local TUF", manifest.package);
    Ok(PublishedPackage {
        package: manifest.package,
    })
}

fn run_pako(context: &Context, arguments: &[String]) -> Result<()> {
    let client = context.client();
    let mut command = Command::new(context.pako());
    command
        .args(arguments)
        .current_dir(context.root())
        .env("HOME", client.join("home"))
        .env("XDG_CONFIG_HOME", client.join("config"))
        .env("XDG_DATA_HOME", client.join("data"))
        .env("XDG_STATE_HOME", client.join("state"))
        .env("XDG_CACHE_HOME", client.join("cache"));
    process::run(&mut command)
}

fn verify_pako(context: &Context, package: &str) -> Result<()> {
    let arguments = ["verify".into(), package.to_owned()];
    match run_pako(context, &arguments) {
        Ok(()) => Ok(()),
        Err(first_error) => {
            thread::sleep(Duration::from_millis(100));
            run_pako(context, &arguments)
                .with_context(|| format!("verify failed twice; first failure was: {first_error:#}"))
        }
    }
}

fn find_single_file(root: &Path, name: &str) -> Result<PathBuf> {
    let mut matches = Vec::new();
    collect_files(root, name, &mut matches)?;

    match matches.as_slice() {
        [path] => Ok(path.clone()),
        [] => anyhow::bail!("{name} was not generated below {}", root.display()),
        _ => anyhow::bail!(
            "multiple {name} files were generated below {}; clean the build directory",
            root.display()
        ),
    }
}

fn collect_files(directory: &Path, name: &str, matches: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let path = entry.path();

        if file_type.is_dir() {
            collect_files(&path, name, matches)?;
        } else if file_type.is_file() && entry.file_name() == OsStr::new(name) {
            matches.push(path);
        }
    }
    Ok(())
}

fn host_target() -> Result<String> {
    let architecture = match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        other => anyhow::bail!("unsupported host architecture: {other}"),
    };
    Ok(format!("linux/{architecture}"))
}

fn validate_target(target: &str) -> Result<String> {
    if matches!(target, "linux/x86_64" | "linux/aarch64") {
        Ok(target.to_owned())
    } else {
        anyhow::bail!("unsupported Pako target: {target}")
    }
}

fn ensure_linux() -> Result<()> {
    if cfg!(target_os = "linux") {
        Ok(())
    } else {
        anyhow::bail!("the Pako development environment currently supports Linux hosts only")
    }
}

fn wait_for_http(address: &str, path: &str, timeout: Duration) -> Result<()> {
    let address: SocketAddr = address.parse()?;
    let started = Instant::now();
    let mut last_error = None;

    while started.elapsed() < timeout {
        match request_http(address, path) {
            Ok(()) => return Ok(()),
            Err(error) => last_error = Some(error),
        }
        thread::sleep(Duration::from_millis(250));
    }

    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("HTTP service did not respond")))
}

fn request_http(address: SocketAddr, path: &str) -> Result<()> {
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(1))?;
    stream.set_read_timeout(Some(Duration::from_secs(1)))?;
    stream.set_write_timeout(Some(Duration::from_secs(1)))?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
        address.ip()
    )?;

    let mut response = [0_u8; 128];
    let count = stream.read(&mut response)?;
    let status = String::from_utf8_lossy(&response[..count]);
    let successful = status
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .is_some_and(|code| code.starts_with('2'));

    if successful {
        Ok(())
    } else {
        anyhow::bail!("unexpected HTTP response from {address}: {status}")
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RepositoryConfig<'a> {
    name: &'a str,
    root: PathBuf,
    metadata_url: &'a str,
    targets_url: &'a str,
    allow_insecure_http: bool,
}

#[derive(Debug, Deserialize)]
struct ManifestSummary {
    package: String,
    artifact: ManifestArtifact,
}

#[derive(Debug, Deserialize)]
struct ManifestArtifact {
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Debug)]
struct PublishedPackage {
    package: String,
}

struct ArchiveServer {
    stop: Sender<()>,
    thread: Option<thread::JoinHandle<()>>,
}

impl ArchiveServer {
    fn start(archive: PathBuf) -> Result<Self> {
        let listener = TcpListener::bind(ARCHIVE_ADDRESS)
            .with_context(|| format!("failed to bind local archive server at {ARCHIVE_ADDRESS}"))?;
        listener.set_nonblocking(true)?;
        let (stop, receiver) = mpsc::channel();
        let thread = thread::spawn(move || loop {
            match receiver.try_recv() {
                Ok(()) | Err(TryRecvError::Disconnected) => break,
                Err(TryRecvError::Empty) => {}
            }
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let _ = serve_archive_request(&mut stream, &archive);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(_) => break,
            }
        });
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for ArchiveServer {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn serve_archive_request(stream: &mut TcpStream, archive: &Path) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(1)))?;
    let mut request = [0_u8; 4096];
    let count = stream.read(&mut request)?;
    let request = String::from_utf8_lossy(&request[..count]);
    let path = request.split_whitespace().nth(1).unwrap_or_default();
    if path.starts_with("/idea-linux-")
        && Path::new(path)
            .extension()
            .is_some_and(|extension| extension == "tar")
    {
        let bytes = fs::read(archive)?;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n",
            bytes.len()
        )?;
        stream.write_all(&bytes)?;
    } else {
        stream.write_all(
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        )?;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
enum ComposeRuntime {
    Docker,
    Podman,
    DockerCompose,
}

impl ComposeRuntime {
    fn detect() -> Result<Self> {
        Self::find().ok_or_else(|| {
            anyhow::anyhow!(
                "Docker Compose, Podman Compose, or docker-compose is required for local services"
            )
        })
    }

    fn find() -> Option<Self> {
        [Self::Docker, Self::Podman, Self::DockerCompose]
            .into_iter()
            .find(|runtime| runtime.available())
    }

    fn available(self) -> bool {
        let mut command = self.command();
        command.arg("version");
        process::succeeds(&mut command)
    }

    fn up(self, context: &Context) -> Result<()> {
        let mut command = self.compose_command(context);
        command.args(["up", "--detach", "--remove-orphans"]);
        process::run(&mut command)
    }

    fn down(self, context: &Context, remove_volumes: bool) -> Result<()> {
        let mut command = self.compose_command(context);
        command.args(["down", "--remove-orphans"]);
        if remove_volumes {
            command.arg("--volumes");
        }
        process::run(&mut command)
    }

    fn compose_command(self, context: &Context) -> Command {
        let mut command = self.command();
        command
            .arg("--project-name")
            .arg("pako-dev")
            .arg("--file")
            .arg(context.compose_file());
        command.current_dir(context.root());
        command
    }

    fn command(self) -> Command {
        match self {
            Self::Docker => {
                let mut command = Command::new("docker");
                command.arg("compose");
                command
            }
            Self::Podman => {
                let mut command = Command::new("podman");
                command.arg("compose");
                command
            }
            Self::DockerCompose => Command::new("docker-compose"),
        }
    }
}
