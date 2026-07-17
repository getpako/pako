use std::path::{Path, PathBuf};

use directories::BaseDirs;

use crate::{manifest::validate_package_name, path::validate_local_version, Result};

/// Filesystem locations used by Pako.
///
/// All package state lives in user-owned XDG directories. Tests can construct
/// the same layout under an isolated temporary root with [`Layout::for_test`].
#[derive(Debug, Clone)]
pub struct Layout {
    pub data: PathBuf,
    pub state: PathBuf,
    pub cache: PathBuf,
    pub config: PathBuf,
    pub bin: PathBuf,
    pub applications: PathBuf,
    pub icons: PathBuf,
}

impl Layout {
    pub fn discover() -> Result<Self> {
        let base = BaseDirs::new()
            .ok_or_else(|| anyhow::anyhow!("unable to determine user directories"))?;

        let state_base = base.state_dir().unwrap_or_else(|| base.data_local_dir());

        Ok(Self {
            data: base.data_local_dir().join("pako"),
            state: state_base.join("pako"),
            cache: base.cache_dir().join("pako"),
            config: base.config_dir().join("pako"),
            bin: base.home_dir().join(".local/bin"),
            applications: base.data_local_dir().join("applications"),
            icons: base.data_local_dir().join("icons/hicolor"),
        })
    }

    pub fn for_test(root: &Path) -> Self {
        Self {
            data: root.join("data/pako"),
            state: root.join("state/pako"),
            cache: root.join("cache/pako"),
            config: root.join("config/pako"),
            bin: root.join("bin"),
            applications: root.join("applications"),
            icons: root.join("icons"),
        }
    }

    pub fn ensure(&self) -> Result<()> {
        for path in [
            &self.data,
            &self.state,
            &self.cache,
            &self.config,
            &self.bin,
            &self.applications,
            &self.icons,
        ] {
            std::fs::create_dir_all(path).map_err(anyhow::Error::from)?;
        }

        Ok(())
    }

    pub fn cellar(&self) -> PathBuf {
        self.data.join("cellar")
    }

    pub fn apps(&self) -> PathBuf {
        self.data.join("apps")
    }

    pub fn manifests(&self) -> PathBuf {
        self.data.join("manifests")
    }

    pub fn staging(&self) -> PathBuf {
        self.data.join("staging")
    }

    pub fn objects(&self) -> PathBuf {
        self.cache.join("objects")
    }

    pub fn packs(&self) -> PathBuf {
        self.cache.join("packs")
    }

    pub fn packages(&self) -> PathBuf {
        self.state.join("packages")
    }

    pub fn versions(&self) -> PathBuf {
        self.state.join("versions")
    }

    pub fn transactions(&self) -> PathBuf {
        self.state.join("transactions")
    }

    pub fn locks(&self) -> PathBuf {
        self.state.join("locks")
    }

    pub fn package_version(&self, package: &str, version: &str) -> Result<PathBuf> {
        validate_package_name(package)?;
        validate_local_version(version)?;
        Ok(self.cellar().join(package).join(version))
    }

    pub fn current_link(&self, package: &str) -> Result<PathBuf> {
        validate_package_name(package)?;
        Ok(self.apps().join(package).join("current"))
    }

    pub fn package_state(&self, package: &str) -> Result<PathBuf> {
        validate_package_name(package)?;
        Ok(self.packages().join(format!("{package}.json")))
    }

    pub fn version_record(&self, package: &str, version: &str) -> Result<PathBuf> {
        validate_package_name(package)?;
        validate_local_version(version)?;
        Ok(self
            .versions()
            .join(package)
            .join(format!("{version}.json")))
    }
}
