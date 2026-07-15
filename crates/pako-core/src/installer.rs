use std::{
    fs::File,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{
    canonical,
    error::IoContext,
    integrations,
    layout::Layout,
    manifest::{PackIndex, PackageManifest},
    materialize,
    object_store::ObjectStore,
    receipt::Receipt,
    transaction::{activate_symlink, CommitPlan, Journal, PackageLock, Phase, RecoveryAction},
    verify, Result, Sha256Digest,
};

#[derive(Debug, Clone)]
pub struct InstallRequest {
    pub repository: String,
    pub oci_manifest_digest: Sha256Digest,
    pub package_manifest_digest: Sha256Digest,
    pub pack_index_digest: Sha256Digest,
}

/// Coordinates package installation and lifecycle operations.
#[derive(Debug)]
pub struct Installer {
    layout: Layout,
    store: ObjectStore,
}

impl Installer {
    pub fn new(layout: Layout) -> Result<Self> {
        layout.ensure()?;
        let store = ObjectStore::new(layout.objects());
        Ok(Self { layout, store })
    }

    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    pub fn store(&self) -> &ObjectStore {
        &self.store
    }

    /// Install one fully resolved package release.
    ///
    /// The caller is responsible for downloading and importing all required
    /// chunks. This method performs only local transactional work.
    pub fn install(
        &self,
        manifest: &PackageManifest,
        index: &PackIndex,
        request: &InstallRequest,
    ) -> Result<Receipt> {
        manifest.validate()?;
        index.validate_against(manifest)?;

        let _lock = PackageLock::acquire(&self.layout, &manifest.package)?;
        let version = package_version(manifest);
        let final_path = self.layout.package_version(&manifest.package, &version)?;

        if final_path.exists() {
            return Err(
                anyhow::anyhow!("version already installed: {}", final_path.display()).into(),
            );
        }

        let current_link = self.layout.current_link(&manifest.package)?;
        let old_current = resolve_current_target(&current_link);
        let staging = self.create_staging_path(manifest, &version);
        let mut journal = Self::create_journal(
            manifest,
            &version,
            &staging,
            &final_path,
            old_current.as_deref(),
        );
        journal.save(&self.layout)?;

        materialize::materialize(manifest, &self.store, &staging)?;
        journal.advance(&self.layout, Phase::Materialized)?;

        verify::verify_tree(manifest, &staging)?;
        journal.advance(&self.layout, Phase::Verified)?;

        // All possible integration conflicts are discovered before the new
        // version becomes visible. Their content is staged under private
        // names, allowing recovery to finish publication idempotently.
        self.save_release_metadata(manifest, index, &version)?;
        let receipt = Receipt {
            schema: 1,
            package: manifest.package.clone(),
            upstream_version: manifest.upstream_version.clone(),
            release: manifest.release,
            target: manifest.target.clone(),
            repository: request.repository.clone(),
            oci_manifest_digest: request.oci_manifest_digest,
            package_manifest_digest: request.package_manifest_digest,
            pack_index_digest: request.pack_index_digest,
            tree_digest: manifest.tree_digest,
            active_path: final_path.display().to_string(),
            installed_at: now_seconds().to_string(),
            previous_versions: old_current
                .iter()
                .filter_map(|path| path.file_name())
                .map(|value| value.to_string_lossy().into_owned())
                .collect(),
            exposures: Vec::new(),
        };

        let mut exposures = integrations::ExposureTransaction::begin(&self.layout, &journal.id)?;
        exposures.preflight(manifest, &self.layout, &staging)?;
        let prepared = exposures.prepare()?.to_vec();
        let mut receipt = receipt;
        receipt.exposures = prepared
            .iter()
            .map(|exposure| exposure.receipt.clone())
            .collect();
        journal.commit = Some(CommitPlan {
            receipt: receipt.clone(),
            exposures: prepared,
        });
        journal.save(&self.layout)?;

        commit_tree(&staging, &final_path)?;
        journal.advance(&self.layout, Phase::TreeCommitted)?;

        // This durable intent is the transaction boundary. From here recovery
        // must complete the new version, never infer intent from final_path.
        journal.recovery = RecoveryAction::RollForward;
        journal.advance(&self.layout, Phase::Committing)?;

        activate_symlink(&final_path, &current_link)?;
        exposures.commit()?;
        receipt.save_atomic(&self.layout.receipt(&manifest.package)?)?;

        journal.advance(&self.layout, Phase::Complete)?;
        journal.remove(&self.layout)?;

        Ok(receipt)
    }

    pub fn verify(&self, package: &str) -> Result<verify::VerificationReport> {
        let receipt = Receipt::load(&self.layout.receipt(package)?)?;
        let version = format!("{}-{}", receipt.upstream_version, receipt.release);
        let manifest = self.load_manifest(package, &version)?;
        verify::verify_tree(&manifest, Path::new(&receipt.active_path))
    }

    pub fn rollback(&self, package: &str, requested: Option<&str>) -> Result<String> {
        let _lock = PackageLock::acquire(&self.layout, package)?;
        let mut receipt = Receipt::load(&self.layout.receipt(package)?)?;

        let target_version = requested
            .map(ToOwned::to_owned)
            .or_else(|| receipt.previous_versions.last().cloned())
            .ok_or_else(|| anyhow::anyhow!("no rollback version available"))?;
        let target_path = self.layout.package_version(package, &target_version)?;

        if !target_path.exists() {
            return Err(anyhow::anyhow!("rollback version is missing").into());
        }

        let manifest = self.load_manifest(package, &target_version)?;
        verify::verify_tree(&manifest, &target_path)?;
        activate_symlink(&target_path, &self.layout.current_link(package)?)?;

        let current_version = format!("{}-{}", receipt.upstream_version, receipt.release);
        if current_version != target_version {
            receipt.previous_versions.push(current_version);
        }

        let (upstream_version, release) = split_version(&target_version)?;
        receipt.upstream_version = upstream_version;
        receipt.release = release;
        receipt.active_path = target_path.display().to_string();
        receipt.tree_digest = manifest.tree_digest;
        receipt.save_atomic(&self.layout.receipt(package)?)?;

        Ok(target_version)
    }

    pub fn remove(&self, package: &str) -> Result<()> {
        let _lock = PackageLock::acquire(&self.layout, package)?;
        let receipt_path = self.layout.receipt(package)?;
        let receipt = Receipt::load(&receipt_path)?;

        let _exposures =
            integrations::ExposureTransaction::begin(&self.layout, format!("remove-{package}"))?;
        integrations::remove(&receipt.exposures)?;
        remove_symlink_if_present(&self.layout.current_link(package)?)?;
        remove_directory_if_present(&self.layout.cellar().join(package))?;
        remove_directory_if_present(&self.layout.manifests().join(package))?;
        std::fs::remove_file(&receipt_path).at(&receipt_path)?;
        Ok(())
    }

    fn create_staging_path(&self, manifest: &PackageManifest, version: &str) -> PathBuf {
        self.layout
            .staging()
            .join(format!("{}-{version}-{}", manifest.package, now_seconds()))
    }

    fn create_journal(
        manifest: &PackageManifest,
        version: &str,
        staging: &Path,
        final_path: &Path,
        old_current: Option<&Path>,
    ) -> Journal {
        Journal {
            schema: 2,
            id: format!("{}-{version}-{}", manifest.package, now_seconds()),
            package: manifest.package.clone(),
            phase: Phase::Prepared,
            staging: staging.display().to_string(),
            final_path: final_path.display().to_string(),
            old_current: old_current.map(|path| path.display().to_string()),
            new_current: final_path.display().to_string(),
            recovery: RecoveryAction::Rollback,
            commit: None,
        }
    }

    fn save_release_metadata(
        &self,
        manifest: &PackageManifest,
        index: &PackIndex,
        version: &str,
    ) -> Result<()> {
        let directory = self
            .layout
            .manifests()
            .join(&manifest.package)
            .join(version);
        std::fs::create_dir_all(&directory).at(&directory)?;

        let manifest_path = directory.join("package-manifest.json");
        std::fs::write(&manifest_path, canonical::to_vec(manifest)?).at(&manifest_path)?;

        let index_path = directory.join("pack-index.json");
        std::fs::write(&index_path, canonical::to_vec(index)?).at(&index_path)?;
        Ok(())
    }

    fn load_manifest(&self, package: &str, version: &str) -> Result<PackageManifest> {
        let path = self
            .layout
            .manifests()
            .join(package)
            .join(version)
            .join("package-manifest.json");
        let manifest = serde_json::from_reader(File::open(&path).at(&path)?)?;
        Ok(manifest)
    }
}

fn package_version(manifest: &PackageManifest) -> String {
    format!("{}-{}", manifest.upstream_version, manifest.release)
}

fn resolve_current_target(current: &Path) -> Option<PathBuf> {
    let target = std::fs::read_link(current).ok()?;
    if target.is_absolute() {
        Some(target)
    } else {
        current.parent().map(|parent| parent.join(target))
    }
}

fn commit_tree(staging: &Path, final_path: &Path) -> Result<()> {
    if let Some(parent) = final_path.parent() {
        std::fs::create_dir_all(parent).at(parent)?;
    }
    std::fs::rename(staging, final_path).at(final_path)
}

fn remove_symlink_if_present(path: &Path) -> Result<()> {
    if path.symlink_metadata().is_ok() {
        std::fs::remove_file(path).at(path)?;
    }
    Ok(())
}

fn remove_directory_if_present(path: &Path) -> Result<()> {
    if path.exists() {
        std::fs::remove_dir_all(path).at(path)?;
    }
    Ok(())
}

fn split_version(value: &str) -> Result<(String, u32)> {
    let (upstream, release) = value
        .rsplit_once('-')
        .ok_or_else(|| anyhow::anyhow!("invalid package version: {value}"))?;
    let release = release.parse().map_err(anyhow::Error::from)?;
    Ok((upstream.to_owned(), release))
}

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
