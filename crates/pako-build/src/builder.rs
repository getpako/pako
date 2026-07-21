use std::{
    fs::File,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

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
    recipe::{Recipe, Target},
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
    pub(crate) fn new(output: PathBuf, jobs: usize) -> anyhow::Result<Self> {
        Ok(Self {
            output,
            http: reqwest::Client::new(),
            jobs,
        })
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
        if !target.build.scripts.is_empty() {
            anyhow::bail!("source builds are not yet supported by the payload.tar.zst builder");
        }
        let work = TempDir::new()?;
        let root = work.path().join("payload");
        std::fs::create_dir(&root)?;
        for (number, source) in target.sources.iter().enumerate() {
            let downloaded = work.path().join(format!("source-{number}"));
            if let Some(path) = &source.path {
                let source_path = recipe.recipe_dir().join(path);
                std::fs::copy(source_path, &downloaded)?;
            } else {
                let url = source
                    .urls
                    .first()
                    .ok_or_else(|| anyhow::anyhow!("source has no URL"))?;
                let bytes = self
                    .http
                    .get(url)
                    .send()
                    .await?
                    .error_for_status()?
                    .bytes()
                    .await?;
                std::fs::write(&downloaded, bytes)?;
            }
            let (digest, _) = Sha256Digest::calculate_reader(File::open(&downloaded)?)?;
            let expected: Sha256Digest = source.hash.parse()?;
            if digest != expected {
                anyhow::bail!("source digest mismatch");
            }
            if let Some(format) = &source.format {
                archive::extract(&downloaded, format, &root, source.strip_components)?;
            } else {
                let destination = source
                    .destination
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("plain source requires destination"))?;
                let destination = PackagePath::new(destination.to_owned())?.join_to(&root);
                if let Some(parent) = destination.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::copy(downloaded, destination)?;
            }
        }
        self.package(recipe, target, &root)
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
