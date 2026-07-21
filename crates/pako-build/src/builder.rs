use std::{
    collections::BTreeMap,
    fs::File,
    os::unix::fs::{symlink, PermissionsExt},
    path::{Path, PathBuf},
    time::Duration,
};

use futures_util::{stream, StreamExt, TryStreamExt};
use pako_core::{
    canonical,
    manifest::{
        DesktopEntry, Entry, Icon, Integrations, Launcher, PackageManifest, PackageMetadata,
        Payload, Policies, PACKAGE_MANIFEST_MEDIA_TYPE, PAYLOAD_MEDIA_TYPE,
    },
    path::{validate_symlink_target, PackagePath},
    verify::compute_tree_digest,
    Sha256Digest,
};
use tempfile::TempDir;
use walkdir::WalkDir;

use crate::{
    archive,
    recipe::{Assertion, Recipe, Target, Transform},
};

#[derive(Debug, Clone)]
pub(crate) struct BuildReport {
    pub package: String,
    pub version: String,
    pub target: String,
    pub package_manifest: PathBuf,
    pub payload: PathBuf,
    pub output: PathBuf,
}
#[derive(Debug)]
pub(crate) struct Builder {
    output: PathBuf,
    http: reqwest::Client,
    jobs: usize,
}
impl Builder {
    pub(crate) fn new(output: PathBuf, jobs: usize) -> Self {
        Self {
            output,
            http: reqwest::Client::new(),
            jobs,
        }
    }
    pub(crate) async fn build(
        &self,
        recipe: &Recipe,
        target_name: &str,
    ) -> anyhow::Result<BuildReport> {
        recipe.validate()?;
        let target = recipe
            .targets
            .iter()
            .find(|target| target.platform == target_name)
            .ok_or_else(|| anyhow::anyhow!("target not found: {target_name}"))?;
        let work = TempDir::new()?;
        let root = work.path().join("payload");
        let source_root = work.path().join("source");
        let build_root = work.path().join("build");
        std::fs::create_dir(&root)?;
        std::fs::create_dir(&source_root)?;
        std::fs::create_dir(&build_root)?;

        let source_destination = if target.build.scripts.is_empty() {
            &root
        } else {
            &source_root
        };
        self.prepare_sources(recipe, target, source_destination, work.path())
            .await?;

        if !target.build.scripts.is_empty() {
            let image = target
                .build
                .environment
                .clone()
                .ok_or_else(|| anyhow::anyhow!("build environment is required"))?;
            let sandbox = crate::sandbox::Sandbox {
                image,
                network: target.build.network,
                timeout: Duration::from_secs(target.build.timeout_seconds.unwrap_or(3600)),
                shell: target.build.shell.clone().unwrap_or_else(|| "bash".into()),
            };
            let environment = BTreeMap::from([
                ("PAKO_DESTDIR".into(), "/pako/dest".into()),
                ("PAKO_SOURCE_DIR".into(), "/pako/source".into()),
                ("PAKO_BUILDDIR".into(), "/pako/build".into()),
            ]);
            for (phase, script) in target
                .build
                .scripts
                .phases()
                .into_iter()
                .filter_map(|(phase, script)| script.map(|script| (phase, script)))
            {
                sandbox
                    .run(
                        phase,
                        script,
                        recipe.recipe_dir(),
                        &source_root,
                        &build_root,
                        &root,
                        &environment,
                    )
                    .await?;
            }
        }

        apply_transforms(
            &root,
            recipe.transforms.iter().chain(target.transforms.iter()),
        )?;
        apply_assertions(
            &root,
            recipe.assertions.iter().chain(target.assertions.iter()),
        )?;
        self.package(recipe, target, &root)
    }

    async fn prepare_sources(
        &self,
        recipe: &Recipe,
        target: &Target,
        destination: &Path,
        work: &Path,
    ) -> anyhow::Result<()> {
        let client = self.http.clone();
        let recipe_directory = recipe.recipe_dir().to_owned();
        let sources = target.sources.clone();
        let jobs = self.jobs.max(1);
        let mut fetched = stream::iter(sources.into_iter().enumerate().map(|(number, source)| {
            let client = client.clone();
            let recipe_directory = recipe_directory.clone();
            let downloaded = work.join(format!("source-{number}"));
            async move {
                if let Some(path) = &source.path {
                    std::fs::copy(recipe_directory.join(path), &downloaded)?;
                } else {
                    let url = source
                        .urls
                        .first()
                        .ok_or_else(|| anyhow::anyhow!("source has no URL"))?;
                    let bytes = client
                        .get(url)
                        .send()
                        .await?
                        .error_for_status()?
                        .bytes()
                        .await?;
                    tokio::fs::write(&downloaded, bytes).await?;
                }
                let (digest, _) = Sha256Digest::calculate_reader(File::open(&downloaded)?)?;
                let expected: Sha256Digest = source.hash.parse()?;
                if digest != expected {
                    anyhow::bail!("source digest mismatch for source {number}");
                }
                Ok::<_, anyhow::Error>((number, source, downloaded))
            }
        }))
        .buffer_unordered(jobs)
        .try_collect::<Vec<_>>()
        .await?;
        fetched.sort_by_key(|(number, _, _)| *number);

        for (_, source, downloaded) in fetched {
            if let Some(format) = &source.format {
                archive::extract(&downloaded, format, destination, source.strip_components)?;
            } else {
                let path = source
                    .destination
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("plain source requires destination"))?;
                let destination = PackagePath::new(path.to_owned())?.join_to(destination);
                if let Some(parent) = destination.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::copy(downloaded, destination)?;
            }
        }
        Ok(())
    }
    fn package(
        &self,
        recipe: &Recipe,
        target: &Target,
        root: &Path,
    ) -> anyhow::Result<BuildReport> {
        let version = format!("{}-{}", recipe.package.version, recipe.package.release);
        let output = self
            .output
            .join(&recipe.package.name)
            .join(&version)
            .join(target.platform.replace('/', "_"));
        if output.exists() {
            anyhow::bail!("build output already exists: {}", output.display());
        }
        std::fs::create_dir_all(&output)?;
        let mut entries = scan_tree(root)?;
        entries.sort_by(|left, right| left.path().cmp(right.path()));
        let payload_path = output.join("payload.tar.zst");
        create_payload(root, &payload_path)?;
        let (digest, size) = Sha256Digest::calculate_reader(File::open(&payload_path)?)?;
        let manifest = PackageManifest {
            schema_version: 1,
            media_type: PACKAGE_MANIFEST_MEDIA_TYPE.into(),
            package: recipe.package.name.clone(),
            upstream_version: recipe.package.version.clone(),
            release: recipe.package.release,
            target: target.platform.clone(),
            metadata: PackageMetadata {
                display_name: recipe.metadata.display_name.clone(),
                summary: recipe.metadata.summary.clone(),
                description: recipe.metadata.description.clone(),
                vendor: recipe.metadata.vendor.clone(),
                homepage: recipe.metadata.homepage.clone(),
                license: recipe.metadata.license.clone(),
            },
            payload: Payload {
                media_type: PAYLOAD_MEDIA_TYPE.into(),
                digest,
                size,
            },
            tree_digest: compute_tree_digest(&entries),
            entries,
            integrations: integrations(recipe)?,
            policies: Policies {
                payload_mutation: "deny".into(),
                self_update: "external".into(),
                user_data: "external".into(),
            },
        };
        manifest.validate()?;
        let manifest_path = output.join("package-manifest.json");
        std::fs::write(&manifest_path, canonical::to_vec(&manifest)?)?;
        Ok(BuildReport {
            package: recipe.package.name.clone(),
            version,
            target: target.platform.clone(),
            package_manifest: manifest_path,
            payload: payload_path,
            output,
        })
    }
}

fn apply_transforms<'a>(
    root: &Path,
    transforms: impl IntoIterator<Item = &'a Transform>,
) -> anyhow::Result<()> {
    for transform in transforms {
        match transform {
            Transform::Remove { paths, required } => {
                for path in paths {
                    let path = payload_path(root, path)?;
                    if !path_exists(&path) {
                        if *required {
                            anyhow::bail!("required path does not exist: {}", path.display());
                        }
                        continue;
                    }
                    remove_path(&path)?;
                }
            }
            Transform::Chmod { path, mode } => {
                let path = payload_path(root, path)?;
                let permissions = parse_mode(mode)?;
                let metadata = std::fs::symlink_metadata(&path)?;
                if metadata.file_type().is_symlink() {
                    anyhow::bail!("cannot change permissions of symlink: {}", path.display());
                }
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(permissions))?;
            }
            Transform::Move { from, to } => {
                let source = payload_path(root, from)?;
                let destination = payload_path(root, to)?;
                ensure_safe_parent(root, &source)?;
                ensure_safe_parent(root, &destination)?;
                if !path_exists(&source) {
                    anyhow::bail!("move source does not exist: {}", source.display());
                }
                if path_exists(&destination) {
                    anyhow::bail!("move destination already exists: {}", destination.display());
                }
                if let Some(parent) = destination.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::rename(source, destination)?;
            }
            Transform::Copy { from, to } => {
                let source = payload_path(root, from)?;
                let destination = payload_path(root, to)?;
                ensure_safe_parent(root, &source)?;
                ensure_safe_parent(root, &destination)?;
                if !path_exists(&source) {
                    anyhow::bail!("copy source does not exist: {}", source.display());
                }
                if path_exists(&destination) {
                    anyhow::bail!("copy destination already exists: {}", destination.display());
                }
                if let Some(parent) = destination.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                copy_path(&source, &destination)?;
            }
            Transform::Write {
                path,
                mode,
                content,
            } => {
                let path = payload_path(root, path)?;
                ensure_safe_parent(root, &path)?;
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&path, content)?;
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(parse_mode(mode)?))?;
            }
            Transform::Symlink { path, target } => {
                let path = PackagePath::new(path.clone())?;
                let destination = path.join_to(root);
                ensure_safe_parent(root, &destination)?;
                validate_symlink_target(&path, target)?;
                if path_exists(&destination) {
                    anyhow::bail!(
                        "symlink destination already exists: {}",
                        destination.display()
                    );
                }
                if let Some(parent) = destination.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                symlink(target, destination)?;
            }
        }
    }
    Ok(())
}

fn apply_assertions<'a>(
    root: &Path,
    assertions: impl IntoIterator<Item = &'a Assertion>,
) -> anyhow::Result<()> {
    for assertion in assertions {
        match assertion {
            Assertion::Path {
                path,
                kind,
                executable,
            } => {
                let path = payload_path(root, path)?;
                let metadata = std::fs::symlink_metadata(&path).map_err(|_| {
                    anyhow::anyhow!("asserted path does not exist: {}", path.display())
                })?;
                let file_type = metadata.file_type();
                let matches_kind = match kind.as_str() {
                    "file" => file_type.is_file(),
                    "directory" => file_type.is_dir(),
                    "symlink" => file_type.is_symlink(),
                    "file-or-symlink" => file_type.is_file() || file_type.is_symlink(),
                    other => anyhow::bail!("unsupported assertion kind: {other}"),
                };
                if !matches_kind {
                    anyhow::bail!("assertion kind mismatch: {}", path.display());
                }
                if *executable && file_type.is_file() && metadata.permissions().mode() & 0o111 == 0
                {
                    anyhow::bail!("asserted file is not executable: {}", path.display());
                }
            }
            Assertion::Absent { path } => {
                let path = payload_path(root, path)?;
                if path_exists(&path) {
                    anyhow::bail!("asserted path is present: {}", path.display());
                }
            }
        }
    }
    Ok(())
}

fn payload_path(root: &Path, value: &str) -> anyhow::Result<PathBuf> {
    Ok(PackagePath::new(value.to_owned())?.join_to(root))
}

fn path_exists(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

fn ensure_safe_parent(root: &Path, path: &Path) -> anyhow::Result<()> {
    let relative = path.strip_prefix(root)?;
    let mut current = root.to_owned();
    for component in relative
        .components()
        .take(relative.components().count().saturating_sub(1))
    {
        current.push(component);
        if std::fs::symlink_metadata(&current)
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            anyhow::bail!("payload path traverses a symlink: {}", current.display());
        }
    }
    Ok(())
}

fn remove_path(path: &Path) -> anyhow::Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.is_dir() {
        std::fs::remove_dir_all(path)?;
    } else {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

fn copy_path(source: &Path, destination: &Path) -> anyhow::Result<()> {
    let metadata = std::fs::symlink_metadata(source)?;
    if metadata.file_type().is_symlink() {
        symlink(std::fs::read_link(source)?, destination)?;
    } else if metadata.is_dir() {
        std::fs::create_dir(destination)?;
        for entry in std::fs::read_dir(source)? {
            let entry = entry?;
            copy_path(&entry.path(), &destination.join(entry.file_name()))?;
        }
    } else {
        std::fs::copy(source, destination)?;
    }
    Ok(())
}

fn parse_mode(value: &str) -> anyhow::Result<u32> {
    let mode =
        u32::from_str_radix(value, 8).map_err(|_| anyhow::anyhow!("invalid file mode: {value}"))?;
    if mode > 0o7777 {
        anyhow::bail!("file mode is too large: {value}");
    }
    Ok(mode)
}

fn scan_tree(root: &Path) -> anyhow::Result<Vec<Entry>> {
    let mut entries = Vec::new();
    for item in WalkDir::new(root)
        .follow_links(false)
        .min_depth(1)
        .sort_by_file_name()
    {
        let item = item?;
        let path = item.path();
        let relative = PackagePath::new(
            path.strip_prefix(root)?
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("non UTF-8 path"))?
                .to_owned(),
        )?;
        let metadata = std::fs::symlink_metadata(path)?;
        let mode = (metadata.permissions().mode() & 0o777) as u16;
        if metadata.is_dir() {
            entries.push(Entry::Directory {
                path: relative,
                mode,
            });
        } else if metadata.file_type().is_symlink() {
            let target = std::fs::read_link(path)?
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("non UTF-8 symlink target"))?
                .to_owned();
            validate_symlink_target(&relative, &target)?;
            entries.push(Entry::Symlink {
                path: relative,
                target,
            });
        } else if metadata.is_file() {
            let (digest, size) = Sha256Digest::calculate_reader(File::open(path)?)?;
            entries.push(Entry::File {
                path: relative,
                mode,
                size,
                digest,
            });
        } else {
            anyhow::bail!("unsupported filesystem entry: {}", path.display());
        }
    }
    Ok(entries)
}
fn create_payload(root: &Path, output: &Path) -> anyhow::Result<()> {
    let file = File::create(output)?;
    let encoder = zstd::stream::write::Encoder::new(file, 19)?;
    let mut archive = tar::Builder::new(encoder.auto_finish());
    for item in WalkDir::new(root)
        .follow_links(false)
        .min_depth(1)
        .sort_by_file_name()
    {
        let item = item?;
        let relative = item.path().strip_prefix(root)?;
        archive.append_path_with_name(item.path(), relative)?;
    }
    archive.finish()?;
    Ok(())
}
fn integrations(recipe: &Recipe) -> anyhow::Result<Integrations> {
    Ok(Integrations {
        launchers: recipe
            .integrations
            .launchers
            .iter()
            .map(|item| {
                Ok(Launcher {
                    name: item.name.clone(),
                    target: PackagePath::new(item.target.clone())?,
                    arguments: item.arguments.clone(),
                })
            })
            .collect::<anyhow::Result<_>>()?,
        desktop_entries: recipe
            .integrations
            .desktop_entries
            .iter()
            .map(|item| DesktopEntry {
                id: item.id.clone(),
                name: item.name.clone(),
                exec: item.exec.clone(),
                icon: item.icon.clone(),
                terminal: item.terminal,
                categories: item.categories.clone(),
            })
            .collect(),
        icons: recipe
            .integrations
            .icons
            .iter()
            .map(|item| {
                Ok(Icon {
                    name: item.name.clone(),
                    source: PackagePath::new(item.source.clone())?,
                    context: item.context.clone(),
                    size: item.size.clone(),
                })
            })
            .collect::<anyhow::Result<_>>()?,
    })
}
