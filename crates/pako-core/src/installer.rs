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
    receipt::{PackageState, Receipt},
    transaction::{activate_symlink, CommitPlan, Journal, PackageLock, Phase, RecoveryAction},
    verify, Error, Result, Sha256Digest,
};

#[derive(Debug, Clone)]
pub struct InstallRequest {
    pub repository: String,
    pub oci_manifest_digest: Sha256Digest,
    pub package_manifest_digest: Sha256Digest,
    pub pack_index_digest: Sha256Digest,
    pub channel: String,
}

/// Coordinates package installation and lifecycle operations.
#[derive(Debug, Clone)]
pub struct Installer {
    layout: Layout,
    store: ObjectStore,
    jobs: usize,
}

impl Installer {
    pub fn new(layout: Layout) -> Result<Self> {
        let jobs = std::thread::available_parallelism().map_or(1, usize::from);
        Self::with_jobs(layout, jobs)
    }

    pub fn with_jobs(layout: Layout, jobs: usize) -> Result<Self> {
        layout.ensure()?;
        let store = ObjectStore::new(layout.objects(), layout.locks().join("objects"));
        Ok(Self {
            layout,
            store,
            jobs: jobs.max(1),
        })
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
        log::info!(
            "installing {} {}-{} for {}",
            manifest.package,
            manifest.upstream_version,
            manifest.release,
            manifest.target
        );

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

        log::debug!("materializing package tree at {}", staging.display());
        materialize::materialize_with_jobs(manifest, &self.store, &staging, self.jobs)?;
        journal.advance(&self.layout, Phase::Materialized)?;

        log::debug!("verifying staged package tree");
        verify::verify_tree_with_jobs(manifest, &staging, self.jobs)?;
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
            installed_at: now_seconds().to_string(),
            exposures: Vec::new(),
        };

        let previous = PackageState::load(&self.layout.package_state(&manifest.package)?).ok();
        let previous_receipt = previous
            .as_ref()
            .map(|state| {
                Receipt::load(
                    &self
                        .layout
                        .version_record(&manifest.package, &state.active)?,
                )
            })
            .transpose()?;
        let mut history = previous
            .as_ref()
            .map(|state| state.history.clone())
            .unwrap_or_default();
        history.retain(|entry| entry != &version);
        history.insert(0, version.clone());
        let state = PackageState {
            schema: 1,
            package: manifest.package.clone(),
            active: version.clone(),
            history,
            channel: request.channel.clone(),
        };

        let mut exposures = integrations::ExposureTransaction::begin(&self.layout, &journal.id)?;
        exposures.preflight(
            manifest,
            &self.layout,
            &staging,
            previous_receipt
                .as_ref()
                .map_or(&[], |receipt| receipt.exposures.as_slice()),
        )?;
        let prepared = exposures.prepare()?.to_vec();
        let mut receipt = receipt;
        receipt.exposures = exposures.published_receipts();
        journal.commit = Some(CommitPlan {
            receipt: receipt.clone(),
            state,
            exposures: prepared,
        });
        journal.save(&self.layout)?;

        commit_tree(&staging, &final_path)?;
        journal.advance(&self.layout, Phase::TreeCommitted)?;

        // This durable intent is the transaction boundary. From here recovery
        // must complete the new version, never infer intent from final_path.
        journal.recovery = RecoveryAction::RollForward;
        journal.advance(&self.layout, Phase::Committing)?;

        log::debug!("activating package tree at {}", final_path.display());
        activate_symlink(&final_path, &current_link)?;
        exposures.commit()?;
        receipt.save_atomic(&self.layout.version_record(&manifest.package, &version)?)?;
        journal
            .commit
            .as_ref()
            .expect("commit plan exists")
            .state
            .save_atomic(&self.layout.package_state(&manifest.package)?)?;

        journal.advance(&self.layout, Phase::Complete)?;
        exposures.finalize()?;
        journal.remove(&self.layout)?;

        Ok(receipt)
    }

    pub fn verify(&self, package: &str) -> Result<verify::VerificationReport> {
        let state = PackageState::load(&self.layout.package_state(package)?)?;
        let version = state.active;
        let manifest = self.load_manifest(package, &version)?;
        verify::verify_tree_with_jobs(
            &manifest,
            &self.layout.package_version(package, &version)?,
            self.jobs,
        )
    }

    pub fn rollback(&self, package: &str, requested: Option<&str>) -> Result<String> {
        let _lock = PackageLock::acquire(&self.layout, package)?;
        let mut state = PackageState::load(&self.layout.package_state(package)?)?;
        let active_version = state.active.clone();

        let target_version = requested
            .map(ToOwned::to_owned)
            .or_else(|| {
                state
                    .history
                    .iter()
                    .find(|version| *version != &state.active)
                    .cloned()
            })
            .ok_or_else(|| anyhow::anyhow!("no rollback version available"))?;
        let target_path = self.layout.package_version(package, &target_version)?;

        if !target_path.exists() {
            return Err(anyhow::anyhow!("rollback version is missing").into());
        }

        let manifest = self.load_manifest(package, &target_version)?;
        verify::verify_tree_with_jobs(&manifest, &target_path, self.jobs)?;
        let active_receipt =
            Receipt::load(&self.layout.version_record(package, &active_version)?)?;
        let target_receipt =
            Receipt::load(&self.layout.version_record(package, &target_version)?)?;
        let transaction_id = format!("rollback-{package}-{}", now_seconds());
        let mut exposures = integrations::ExposureTransaction::begin(&self.layout, transaction_id)?;
        exposures.preflight(
            &manifest,
            &self.layout,
            &target_path,
            &active_receipt.exposures,
        )?;
        exposures.prepare()?;
        if exposures.published_receipts() != target_receipt.exposures {
            exposures.rollback()?;
            return Err(Error::Transaction(
                "retained version integrations do not match their receipt".into(),
            ));
        }

        let current_link = self.layout.current_link(package)?;
        let active_path = self.layout.package_version(package, &active_version)?;
        let result = (|| -> Result<()> {
            exposures.commit()?;
            activate_symlink(&target_path, &current_link)?;
            state.history.retain(|version| version != &target_version);
            state.history.insert(0, target_version.clone());
            state.active.clone_from(&target_version);
            state.save_atomic(&self.layout.package_state(package)?)
        })();

        if let Err(error) = result {
            let _ = activate_symlink(&active_path, &current_link);
            let _ = exposures.rollback();
            return Err(error);
        }
        exposures.finalize()?;

        Ok(target_version)
    }

    pub fn versions(&self, package: &str) -> Result<PackageState> {
        PackageState::load(&self.layout.package_state(package)?)
    }

    pub fn prune(&self, package: &str, keep: usize) -> Result<Vec<String>> {
        let _lock = PackageLock::acquire(&self.layout, package)?;
        let mut state = PackageState::load(&self.layout.package_state(package)?)?;
        let keep = keep.max(1);
        let removed = state.history.split_off(keep);
        for version in &removed {
            remove_directory_if_present(&self.layout.package_version(package, version)?)?;
            let record = self.layout.version_record(package, version)?;
            if record.exists() {
                std::fs::remove_file(&record).at(&record)?;
            }
            remove_directory_if_present(&self.layout.manifests().join(package).join(version))?;
        }
        state.save_atomic(&self.layout.package_state(package)?)?;
        Ok(removed)
    }

    pub fn remove(&self, package: &str) -> Result<()> {
        let _lock = PackageLock::acquire(&self.layout, package)?;
        let state_path = self.layout.package_state(package)?;
        let state = PackageState::load(&state_path)?;
        let receipt = Receipt::load(&self.layout.version_record(package, &state.active)?)?;

        let _exposures =
            integrations::ExposureTransaction::begin(&self.layout, format!("remove-{package}"))?;
        integrations::remove(&receipt.exposures)?;
        remove_symlink_if_present(&self.layout.current_link(package)?)?;
        remove_directory_if_present(&self.layout.cellar().join(package))?;
        remove_directory_if_present(&self.layout.manifests().join(package))?;
        remove_directory_if_present(&self.layout.versions().join(package))?;
        std::fs::remove_file(&state_path).at(&state_path)?;
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

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
