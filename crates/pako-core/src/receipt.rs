use std::{fs::File, io::Write, path::Path};

use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

use crate::{
    canonical,
    error::IoContext,
    manifest::validate_package_name,
    path::{
        validate_channel, validate_local_version, validate_managed_name, validate_upstream_version,
    },
    Error, Result, Sha256Digest,
};

/// Immutable provenance for one installed version.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstalledVersionRecord {
    pub schema: u32,
    pub package: String,
    pub upstream_version: String,
    pub release: u32,
    pub target: String,
    pub repository: String,
    pub oci_manifest_digest: Sha256Digest,
    pub package_manifest_digest: Sha256Digest,
    pub pack_index_digest: Sha256Digest,
    pub tree_digest: Sha256Digest,
    pub installed_at: String,
    pub exposures: Vec<ExposureReceipt>,
}

/// Compatibility name for internal transaction code.
pub type Receipt = InstalledVersionRecord;

/// Mutable package-level state. The active path is always derived from this
/// version name and the layout, never trusted from persisted absolute paths.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PackageState {
    pub schema: u32,
    pub package: String,
    pub active: String,
    pub history: Vec<String>,
    pub channel: String,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExposureReceipt {
    pub kind: String,
    pub path: String,
    pub digest: Sha256Digest,
}

impl InstalledVersionRecord {
    pub fn validate(&self) -> Result<()> {
        if self.schema != 1 {
            return Err(Error::UnsupportedSchema(self.schema));
        }

        validate_package_name(&self.package)?;
        validate_upstream_version(&self.upstream_version)?;
        validate_managed_name(&self.repository, "repository name")?;
        if self.release == 0 {
            return Err(Error::InvalidManifest(
                "receipt release must be positive".into(),
            ));
        }

        Ok(())
    }

    pub fn load(path: &Path) -> Result<Self> {
        let file = File::open(path).at(path)?;
        let receipt: Self = serde_json::from_reader(file)?;
        receipt.validate()?;
        Ok(receipt)
    }

    pub fn save_atomic(&self, path: &Path) -> Result<()> {
        self.validate()?;

        let parent = path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("receipt path has no parent"))?;
        std::fs::create_dir_all(parent).at(parent)?;

        let mut temporary = NamedTempFile::new_in(parent).at(parent)?;
        temporary
            .write_all(&canonical::to_vec(self)?)
            .at(temporary.path())?;
        temporary.as_file().sync_all().at(temporary.path())?;

        temporary.persist(path).map_err(|error| Error::Io {
            path: path.to_owned(),
            source: error.error,
        })?;

        sync_directory(parent)
    }
}

impl PackageState {
    pub fn load(path: &Path) -> Result<Self> {
        let file = File::open(path).at(path)?;
        let state: Self = serde_json::from_reader(file)?;
        if state.schema != 1 {
            return Err(Error::UnsupportedSchema(state.schema));
        }
        state.validate()?;
        Ok(state)
    }

    pub fn save_atomic(&self, path: &Path) -> Result<()> {
        self.validate()?;
        save_atomic(self, path)
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema != 1 {
            return Err(Error::UnsupportedSchema(self.schema));
        }
        validate_package_name(&self.package)?;
        validate_local_version(&self.active)?;
        validate_channel(&self.channel)?;
        if self.history.is_empty() || self.history.first() != Some(&self.active) {
            return Err(Error::InvalidManifest(
                "package state history must start with the active version".into(),
            ));
        }
        for version in &self.history {
            validate_local_version(version)?;
        }
        Ok(())
    }
}

fn save_atomic(value: &impl Serialize, path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("state path has no parent"))?;
    std::fs::create_dir_all(parent).at(parent)?;
    let mut temporary = NamedTempFile::new_in(parent).at(parent)?;
    temporary
        .write_all(&canonical::to_vec(value)?)
        .at(temporary.path())?;
    temporary.as_file().sync_all().at(temporary.path())?;
    temporary.persist(path).map_err(|error| Error::Io {
        path: path.to_owned(),
        source: error.error,
    })?;
    sync_directory(parent)
}

pub(crate) fn sync_directory(path: &Path) -> Result<()> {
    File::open(path).at(path)?.sync_all().at(path)
}
