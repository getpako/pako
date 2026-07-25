//! TUF-backed mapping from package names to signed package-manifest targets.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    str::FromStr,
};

use futures_util::StreamExt;
use pako_core::manifest::validate_package_name;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tough::{IntoVec, RepositoryLoader, TargetName};
use url::Url;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReleaseCatalog {
    pub schema: u32,
    pub packages: Vec<CatalogPackage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogPackage {
    pub name: String,
    pub channels: BTreeMap<String, String>,
    pub releases: Vec<CatalogRelease>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogRelease {
    pub upstream_version: String,
    pub release: u32,
    pub channel: String,
    pub target: String,
    pub manifest_target: String,
}

impl ReleaseCatalog {
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.schema != 1 {
            anyhow::bail!("unsupported release catalog schema {}", self.schema);
        }

        for package in &self.packages {
            validate_package_name(&package.name)?;
            for release in &package.releases {
                if release.release == 0 {
                    anyhow::bail!("release number must be positive for {}", package.name);
                }
                validate_target_name(&release.manifest_target)?;
                if !matches!(release.target.as_str(), "linux/x86_64" | "linux/aarch64") {
                    anyhow::bail!("unsupported target {} for {}", release.target, package.name);
                }
            }
            for (channel, release_id) in &package.channels {
                if channel.is_empty()
                    || !package
                        .releases
                        .iter()
                        .any(|release| release.id() == *release_id)
                {
                    anyhow::bail!(
                        "channel {channel} points to an unknown release for {}",
                        package.name
                    );
                }
            }
        }

        Ok(())
    }

    pub fn resolve(
        &self,
        package_name: &str,
        target: &str,
        channel: &str,
    ) -> anyhow::Result<&CatalogRelease> {
        validate_package_name(package_name)?;

        let package = self
            .packages
            .iter()
            .find(|package| package.name == package_name)
            .ok_or_else(|| anyhow::anyhow!("package not found: {package_name}"))?;

        let release_id = package
            .channels
            .get(channel)
            .ok_or_else(|| anyhow::anyhow!("no channel {channel} for {package_name}"))?;
        package
            .releases
            .iter()
            .find(|release| release.target == target && release.id() == *release_id)
            .ok_or_else(|| {
                anyhow::anyhow!("no release for {package_name} on {target} in channel {channel}")
            })
    }
}

impl CatalogRelease {
    fn id(&self) -> String {
        format!("{}-{}", self.upstream_version, self.release)
    }
}

/// Loads signed repository metadata and the signed `catalog.json` target.
#[derive(Debug, Clone)]
pub struct TrustedRepository {
    root: PathBuf,
    metadata_url: Url,
    targets_url: Url,
    datastore: PathBuf,
}

impl TrustedRepository {
    pub fn new(root: PathBuf, metadata_url: Url, targets_url: Url, datastore: PathBuf) -> Self {
        Self {
            root,
            metadata_url,
            targets_url,
            datastore,
        }
    }

    pub async fn refresh_catalog(&self) -> anyhow::Result<ReleaseCatalog> {
        log::debug!(
            "refreshing TUF metadata from {} with targets {}",
            self.metadata_url,
            self.targets_url
        );
        tokio::fs::create_dir_all(&self.datastore).await?;
        let trusted_root = tokio::fs::read(&self.root).await?;

        let repository = RepositoryLoader::new(
            &trusted_root,
            self.metadata_url.clone(),
            self.targets_url.clone(),
        )
        .datastore(self.datastore.clone())
        .load()
        .await?;

        let bytes = read_repository_target(&repository, "catalog.json").await?;
        let catalog: ReleaseCatalog = serde_json::from_slice(&bytes)?;
        catalog.validate()?;
        log::debug!(
            "loaded {} package(s) from signed catalog",
            catalog.packages.len()
        );
        Ok(catalog)
    }

    pub async fn read_target(&self, target: &str) -> anyhow::Result<Vec<u8>> {
        validate_target_name(target)?;
        tokio::fs::create_dir_all(&self.datastore).await?;
        let trusted_root = tokio::fs::read(&self.root).await?;
        let repository = RepositoryLoader::new(
            &trusted_root,
            self.metadata_url.clone(),
            self.targets_url.clone(),
        )
        .datastore(self.datastore.clone())
        .load()
        .await?;
        read_repository_target(&repository, target).await
    }

    pub async fn read_target_to_file(
        &self,
        target: &str,
        destination: &Path,
    ) -> anyhow::Result<()> {
        validate_target_name(target)?;
        tokio::fs::create_dir_all(
            destination
                .parent()
                .ok_or_else(|| anyhow::anyhow!("target destination has no parent"))?,
        )
        .await?;
        let trusted_root = tokio::fs::read(&self.root).await?;
        let repository = RepositoryLoader::new(
            &trusted_root,
            self.metadata_url.clone(),
            self.targets_url.clone(),
        )
        .datastore(self.datastore.clone())
        .load()
        .await?;
        let target_name = TargetName::from_str(target)?;
        let mut stream = repository
            .read_target(&target_name)
            .await?
            .ok_or_else(|| anyhow::anyhow!("signed TUF target is not present: {target}"))?;
        let mut file = tokio::fs::File::create(destination).await?;
        while let Some(chunk) = stream.next().await {
            file.write_all(&chunk?).await?;
        }
        file.flush().await?;
        Ok(())
    }
}

async fn read_repository_target(
    repository: &tough::Repository,
    target: &str,
) -> anyhow::Result<Vec<u8>> {
    let target_name = TargetName::from_str(target)?;
    let stream = repository
        .read_target(&target_name)
        .await?
        .ok_or_else(|| anyhow::anyhow!("signed TUF target is not present: {target}"))?;
    Ok(stream.into_vec().await?)
}

fn validate_target_name(target: &str) -> anyhow::Result<()> {
    if target.is_empty()
        || target.starts_with('/')
        || target
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        anyhow::bail!("unsafe TUF target: {target}");
    }
    Ok(())
}
