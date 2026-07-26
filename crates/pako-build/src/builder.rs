use std::{
    collections::BTreeMap,
    fs::File,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::Duration,
};

use futures_util::{stream, StreamExt, TryStreamExt};
use indicatif::{ProgressBar, ProgressStyle};
use pako_core::{
    canonical,
    manifest::{
        ArchiveDescriptor, ArchiveFormat, Artifact, DesktopEntry, Entry, Icon, InstallTransform,
        Integrations, Launcher, PackageManifest, PackageMetadata, Policies,
        PACKAGE_MANIFEST_MEDIA_TYPE,
    },
    path::{validate_symlink_target, PackagePath},
    verify::compute_tree_digest,
    Sha256Digest,
};
use tempfile::TempDir;
use tokio::io::AsyncWriteExt;
use walkdir::WalkDir;

use crate::{
    archive,
    recipe::{Assertion, Distribution, Recipe, Target, Transform},
};

#[derive(Debug, Clone)]
pub(crate) struct BuildReport {
    pub package: String,
    pub version: String,
    pub target: String,
    pub package_manifest: PathBuf,
    pub artifact: Option<PathBuf>,
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
            let work = work.to_owned();
            async move {
                let expected: Sha256Digest = source.hash.parse()?;
                if let Some(path) = &source.path {
                    std::fs::copy(recipe_directory.join(path), &downloaded)?;
                } else {
                    download_source(
                        &client,
                        &source.urls,
                        source.size,
                        expected,
                        &downloaded,
                        &work,
                        number,
                    )
                    .await?;
                }
                let (digest, size) = Sha256Digest::calculate_reader(File::open(&downloaded)?)?;
                if digest != expected {
                    anyhow::bail!("source digest mismatch for source {number}");
                }
                if source.size.is_some_and(|expected| expected != size) {
                    anyhow::bail!(
                        "source size mismatch for source {number}: expected {}, got {size}",
                        source.size.unwrap_or_default()
                    );
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
    #[allow(clippy::too_many_lines)]
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
        let transforms = recipe
            .transforms
            .iter()
            .chain(target.transforms.iter())
            .map(install_transform)
            .collect::<anyhow::Result<Vec<_>>>()?;
        let (artifact, artifact_path) = match target.distribution {
            Distribution::External => {
                let source = target
                    .sources
                    .first()
                    .ok_or_else(|| anyhow::anyhow!("external target has no source"))?;
                let urls = source
                    .urls
                    .iter()
                    .map(|url| url.parse())
                    .collect::<Result<Vec<url::Url>, _>>()?;
                let format = source
                    .format
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("external source has no archive format"))?
                    .parse()?;
                (
                    Artifact::ExternalArchive {
                        urls,
                        digest: source.hash.parse()?,
                        size: source.size.ok_or_else(|| {
                            anyhow::anyhow!("external source has no archive size")
                        })?,
                        archive: ArchiveDescriptor {
                            format,
                            strip_components: source.strip_components,
                        },
                    },
                    None,
                )
            }
            Distribution::Hosted => {
                let archive_path = output.join("package.tar.zst");
                create_payload(root, &archive_path)?;
                let (digest, size) = Sha256Digest::calculate_reader(File::open(&archive_path)?)?;
                (
                    Artifact::TufArchive {
                        target: format!(
                            "artifacts/{}/{}/{}.tar.zst",
                            recipe.package.name,
                            version,
                            target.platform.replace('/', "-")
                        ),
                        digest,
                        size,
                        archive: ArchiveDescriptor {
                            format: ArchiveFormat::TarZst,
                            strip_components: 0,
                        },
                    },
                    Some(archive_path),
                )
            }
        };
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
            artifact,
            tree_digest: compute_tree_digest(&entries),
            entries,
            transforms: if target.distribution == Distribution::External {
                transforms
            } else {
                Vec::new()
            },
            integrations: integrations(recipe)?,
            policies: Policies {
                artifact_mutation: "deny".into(),
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
            artifact: artifact_path,
            output,
        })
    }
}

async fn download_source(
    client: &reqwest::Client,
    urls: &[String],
    expected_size: Option<u64>,
    expected_digest: Sha256Digest,
    destination: &Path,
    work: &Path,
    number: usize,
) -> anyhow::Result<()> {
    if urls.is_empty() {
        anyhow::bail!("source has no URL");
    }

    let mut errors = Vec::new();
    for (mirror, url) in urls.iter().enumerate() {
        let temporary = work.join(format!(
            "source-{number}.partial-{}-{mirror}",
            std::process::id()
        ));
        match download_mirror(client, url, expected_size, &temporary).await {
            Ok(()) => {
                let (digest, size) = Sha256Digest::calculate_reader(File::open(&temporary)?)?;
                if digest != expected_digest {
                    errors.push(format!("{url}: SHA-256 digest mismatch"));
                    let _ = std::fs::remove_file(&temporary);
                    continue;
                }
                if expected_size.is_some_and(|expected| expected != size) {
                    errors.push(format!("{url}: actual size mismatch"));
                    let _ = std::fs::remove_file(&temporary);
                    continue;
                }
                std::fs::rename(&temporary, destination)?;
                return Ok(());
            }
            Err(error) => {
                errors.push(format!("{url}: {error}"));
                let _ = std::fs::remove_file(&temporary);
            }
        }
    }

    anyhow::bail!("all source mirrors failed: {}", errors.join("; "))
}

async fn download_mirror(
    client: &reqwest::Client,
    url: &str,
    expected_size: Option<u64>,
    temporary: &Path,
) -> anyhow::Result<()> {
    let response = client.get(url).send().await?.error_for_status()?;
    if response
        .content_length()
        .zip(expected_size)
        .is_some_and(|(actual, expected)| actual != expected)
    {
        anyhow::bail!("Content-Length does not match declared source size");
    }

    let progress = source_progress(expected_size, url);
    let mut stream = response.bytes_stream();
    let mut file = tokio::fs::File::create(temporary).await?;
    let mut downloaded = 0_u64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        downloaded = downloaded
            .checked_add(u64::try_from(chunk.len())?)
            .ok_or_else(|| anyhow::anyhow!("source size overflow"))?;
        if expected_size.is_some_and(|expected| downloaded > expected) {
            pako_log::abandon_progress(&progress, "Source download exceeded declared size");
            anyhow::bail!("source download exceeded declared size");
        }
        file.write_all(&chunk).await?;
        progress.set_position(downloaded);
    }
    file.flush().await?;
    if expected_size.is_some_and(|expected| downloaded != expected) {
        pako_log::abandon_progress(&progress, "Source download ended early");
        anyhow::bail!("source download ended before declared size");
    }
    pako_log::finish_progress(&progress, format!("Downloaded {url}"));
    Ok(())
}

fn source_progress(expected_size: Option<u64>, url: &str) -> ProgressBar {
    let progress = pako_log::add_progress(
        expected_size.map_or_else(ProgressBar::new_spinner, ProgressBar::new),
    );
    let template = if expected_size.is_some() {
        "{spinner:.green} {msg} [{bar:35.cyan/blue}] {bytes}/{total_bytes} {bytes_per_sec} ETA {eta}"
    } else {
        "{spinner:.green} {msg} {bytes} {bytes_per_sec}"
    };
    progress.set_style(
        ProgressStyle::with_template(template).expect("source download progress template is valid"),
    );
    progress.set_message(format!("Downloading {url}"));
    progress.enable_steady_tick(Duration::from_millis(100));
    progress
}

fn install_transform(transform: &Transform) -> anyhow::Result<InstallTransform> {
    Ok(match transform {
        Transform::Remove { paths, required } => InstallTransform::Remove {
            paths: paths
                .iter()
                .map(|p| PackagePath::new(p.clone()))
                .collect::<pako_core::Result<_>>()?,
            required: *required,
        },
        Transform::Chmod { path, mode } => InstallTransform::Chmod {
            path: PackagePath::new(path.clone())?,
            mode: u16::try_from(parse_mode(mode)?)?,
        },
        Transform::Move { from, to } => InstallTransform::Move {
            from: PackagePath::new(from.clone())?,
            to: PackagePath::new(to.clone())?,
        },
        Transform::Copy { from, to } => InstallTransform::Copy {
            from: PackagePath::new(from.clone())?,
            to: PackagePath::new(to.clone())?,
        },
        Transform::Write {
            path,
            mode,
            content,
        } => InstallTransform::Write {
            path: PackagePath::new(path.clone())?,
            mode: u16::try_from(parse_mode(mode)?)?,
            content: content.clone(),
        },
        Transform::Symlink { path, target } => {
            let path = PackagePath::new(path.clone())?;
            validate_symlink_target(&path, target)?;
            InstallTransform::Symlink {
                path,
                target: target.clone(),
            }
        }
    })
}

fn apply_transforms<'a>(
    root: &Path,
    transforms: impl IntoIterator<Item = &'a Transform>,
) -> anyhow::Result<()> {
    let transforms = transforms
        .into_iter()
        .map(install_transform)
        .collect::<anyhow::Result<Vec<_>>>()?;
    pako_core::transform::apply(&transforms, root)
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
