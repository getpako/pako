//! TUF-backed mapping from package names to immutable OCI manifest digests.

use std::{cmp::Ordering, path::PathBuf, str::FromStr};

use pako_core::{manifest::validate_package_name, Sha256Digest};
use serde::{Deserialize, Serialize};
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
    pub releases: Vec<CatalogRelease>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogRelease {
    pub upstream_version: String,
    pub release: u32,
    pub channel: String,
    pub target: String,
    pub oci: String,
    pub manifest_digest: Sha256Digest,
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
                if !matches!(release.target.as_str(), "linux/x86_64" | "linux/aarch64") {
                    anyhow::bail!("unsupported target {} for {}", release.target, package.name);
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

        package
            .releases
            .iter()
            .filter(|release| release.target == target && release.channel == channel)
            .max_by(|left, right| compare_releases(left, right))
            .ok_or_else(|| {
                anyhow::anyhow!("no release for {package_name} on {target} in channel {channel}")
            })
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

        let target_name = TargetName::from_str("catalog.json")?;
        let stream = repository.read_target(&target_name).await?.ok_or_else(|| {
            anyhow::anyhow!("catalog.json is not present in signed targets metadata")
        })?;
        let bytes = stream.into_vec().await?;
        let catalog: ReleaseCatalog = serde_json::from_slice(&bytes)?;
        catalog.validate()?;
        Ok(catalog)
    }
}

fn compare_releases(left: &CatalogRelease, right: &CatalogRelease) -> Ordering {
    natural_compare(&left.upstream_version, &right.upstream_version)
        .then(left.release.cmp(&right.release))
}

/// Compare common dotted or dashed version strings without requiring strict
/// Semantic Versioning from upstream projects.
fn natural_compare(left: &str, right: &str) -> Ordering {
    let mut left_parts = left.split(|character: char| !character.is_ascii_alphanumeric());
    let mut right_parts = right.split(|character: char| !character.is_ascii_alphanumeric());

    loop {
        match (left_parts.next(), right_parts.next()) {
            (Some(left), Some(right)) => {
                let ordering = match (left.parse::<u64>(), right.parse::<u64>()) {
                    (Ok(left), Ok(right)) => left.cmp(&right),
                    _ => left.cmp(right),
                };

                if ordering != Ordering::Equal {
                    return ordering;
                }
            }
            (Some(_), None) => return Ordering::Greater,
            (None, Some(_)) => return Ordering::Less,
            (None, None) => return Ordering::Equal,
        }
    }
}
