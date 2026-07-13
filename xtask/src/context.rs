use std::{
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context as _, Result};
use serde::Deserialize;

#[derive(Debug)]
pub(crate) struct Context {
    root: PathBuf,
    target_directory: PathBuf,
    dev_directory: PathBuf,
}

#[derive(Debug, Deserialize)]
struct CargoMetadata {
    target_directory: PathBuf,
}

impl Context {
    pub(crate) fn discover() -> Result<Self> {
        let manifest_directory = Path::new(env!("CARGO_MANIFEST_DIR"));
        let root = manifest_directory
            .parent()
            .context("xtask must be located directly below the workspace root")?
            .to_path_buf();

        let output = Command::new("cargo")
            .args(["metadata", "--format-version", "1", "--no-deps"])
            .current_dir(&root)
            .output()
            .context("failed to run cargo metadata")?;

        if !output.status.success() {
            anyhow::bail!(
                "cargo metadata failed:\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        let metadata: CargoMetadata =
            serde_json::from_slice(&output.stdout).context("invalid cargo metadata output")?;
        let dev_directory = root.join(".dev");

        Ok(Self {
            root,
            target_directory: metadata.target_directory,
            dev_directory,
        })
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn compose_file(&self) -> PathBuf {
        self.root.join("dev/compose.yml")
    }

    pub(crate) fn pako(&self) -> PathBuf {
        self.target_directory.join("debug/pako")
    }

    pub(crate) fn pako_build(&self) -> PathBuf {
        self.target_directory.join("debug/pako-build")
    }

    pub(crate) fn dev(&self) -> &Path {
        &self.dev_directory
    }

    pub(crate) fn tuf(&self) -> PathBuf {
        self.dev_directory.join("tuf")
    }

    pub(crate) fn build(&self) -> PathBuf {
        self.dev_directory.join("build")
    }

    pub(crate) fn client(&self) -> PathBuf {
        self.dev_directory.join("client")
    }
}
