use std::{
    ffi::OsStr,
    fs,
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use crate::{context::Context, process, DevCommand};

const REGISTRY_REFERENCE: &str = "localhost:5000/pako";
const REGISTRY_ADDRESS: &str = "127.0.0.1:5000";
const TUF_ADDRESS: &str = "127.0.0.1:8080";
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

    wait_for_http(REGISTRY_ADDRESS, "/v2/", STARTUP_TIMEOUT)
        .context("local OCI registry did not become ready")?;
    wait_for_http(TUF_ADDRESS, "/metadata/root.json", STARTUP_TIMEOUT)
        .context("local TUF server did not become ready")?;

    println!("Pako development environment is ready");
    println!("OCI: http://{REGISTRY_ADDRESS}");
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
        fs::remove_dir_all(context.dev()).with_context(|| {
            format!(
                "failed to remove development state at {}",
                context.dev().display()
            )
        })?;
    }

    up(context)
}

fn smoke(context: &Context) -> Result<()> {
    up(context)?;
    reset_client(context)?;

    let recipe = context.root().join("examples/hello-local/recipe.toml");
    let published = publish_recipe(context, &recipe, None)?;

    run_pako(context, &["install".into(), published.package.clone()])?;
    run_pako(context, &["verify".into(), published.package.clone()])?;

    let launcher = context.client().join("home/.local/bin/hello-pako");
    process::run(Command::new(&launcher).current_dir(context.root()))
        .context("installed hello-local launcher failed")?;

    run_pako(context, &["status".into(), published.package])?;
    println!("Pako development smoke test completed successfully");
    Ok(())
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
    let reference = format!(
        "{REGISTRY_REFERENCE}/{}:dev-{}",
        manifest.package,
        target.replace('/', "-")
    );

    process::run(
        Command::new(context.pako_build())
            .arg("publish")
            .arg(artifact)
            .arg("--reference")
            .arg(&reference)
            .arg("--insecure-http")
            .arg("--tuf")
            .arg(context.tuf())
            .current_dir(context.root()),
    )?;

    println!("Published {} to {reference}", manifest.package);
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
}

#[derive(Debug)]
struct PublishedPackage {
    package: String,
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
