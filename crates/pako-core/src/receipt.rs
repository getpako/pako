use std::{fs::File, io::Write, path::Path};

use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

use crate::{
    canonical, error::IoContext, manifest::validate_package_name, Error, Result, Sha256Digest,
};

/// Durable record of the active package release and files exposed outside the
/// managed cellar.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Receipt {
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
    pub active_path: String,
    pub installed_at: String,
    pub previous_versions: Vec<String>,
    pub exposures: Vec<ExposureReceipt>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExposureReceipt {
    pub kind: String,
    pub path: String,
    pub digest: Sha256Digest,
}

impl Receipt {
    pub fn validate(&self) -> Result<()> {
        if self.schema != 1 {
            return Err(Error::UnsupportedSchema(self.schema));
        }

        validate_package_name(&self.package)?;
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

pub(crate) fn sync_directory(path: &Path) -> Result<()> {
    File::open(path).at(path)?.sync_all().at(path)
}
