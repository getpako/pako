use std::{
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
};

use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::{
    canonical,
    error::IoContext,
    integrations::{self, PreparedExposure},
    layout::Layout,
    receipt::{sync_directory, Receipt},
    Error, Result,
};

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Phase {
    Prepared,
    Materialized,
    Verified,
    // Kept for deserializing schema-1 journals. Those journals are recovered
    // conservatively with rollback because they did not record a commit plan.
    Committed,
    Exposed,
    ReceiptWritten,
    TreeCommitted,
    Committing,
    Complete,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum RecoveryAction {
    #[default]
    Rollback,
    RollForward,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommitPlan {
    pub receipt: Receipt,
    pub exposures: Vec<PreparedExposure>,
}

/// Crash-recovery journal for one package activation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Journal {
    pub schema: u32,
    pub id: String,
    pub package: String,
    pub phase: Phase,
    pub staging: String,
    pub final_path: String,
    pub old_current: Option<String>,
    pub new_current: String,
    #[serde(default)]
    pub recovery: RecoveryAction,
    #[serde(default)]
    pub commit: Option<CommitPlan>,
}

/// Exclusive package-level lock held for the lifetime of a mutating operation.
#[derive(Debug)]
pub struct PackageLock {
    _file: File,
}

impl PackageLock {
    pub fn acquire(layout: &Layout, package: &str) -> Result<Self> {
        let directory = layout.locks();
        std::fs::create_dir_all(&directory).at(&directory)?;

        let path = directory.join(format!("{package}.lock"));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .at(&path)?;
        file.lock_exclusive().at(&path)?;

        Ok(Self { _file: file })
    }
}

impl Journal {
    pub fn path(&self, layout: &Layout) -> PathBuf {
        layout.transactions().join(format!("{}.json", self.id))
    }

    pub fn save(&self, layout: &Layout) -> Result<()> {
        let path = self.path(layout);
        let parent = path.parent().expect("journal path always has a parent");
        std::fs::create_dir_all(parent).at(parent)?;

        let temporary = path.with_extension("json.tmp");
        std::fs::write(&temporary, canonical::to_vec(self)?).at(&temporary)?;
        File::open(&temporary)
            .at(&temporary)?
            .sync_all()
            .at(&temporary)?;
        std::fs::rename(&temporary, &path).at(&path)?;
        sync_directory(parent)
    }

    pub fn advance(&mut self, layout: &Layout, phase: Phase) -> Result<()> {
        self.phase = phase;
        self.save(layout)
    }

    pub fn remove(&self, layout: &Layout) -> Result<()> {
        let path = self.path(layout);
        if path.exists() {
            std::fs::remove_file(&path).at(&path)?;
        }
        Ok(())
    }
}

/// Recover interrupted transactions without executing package-controlled code.
pub fn recover(layout: &Layout) -> Result<Vec<String>> {
    let directory = layout.transactions();
    if !directory.exists() {
        return Ok(Vec::new());
    }

    let mut recovered = Vec::new();
    for entry in std::fs::read_dir(&directory).at(&directory)? {
        let path = entry.at(&directory)?.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }

        let journal: Journal = serde_json::from_slice(&std::fs::read(&path).at(&path)?)?;
        validate_journal(layout, &journal)?;

        let staging = PathBuf::from(&journal.staging);
        let final_path = PathBuf::from(&journal.final_path);
        let current = layout.current_link(&journal.package)?;

        let _lock = PackageLock::acquire(layout, &journal.package)?;
        match journal.recovery {
            RecoveryAction::Rollback => {
                recover_rollback(layout, &journal, &staging, &final_path, &current)?;
            }
            RecoveryAction::RollForward => {
                recover_roll_forward(layout, &journal, &final_path, &current)?;
            }
        }

        std::fs::remove_file(&path).at(&path)?;
        recovered.push(journal.package);
    }

    Ok(recovered)
}

fn validate_journal(layout: &Layout, journal: &Journal) -> Result<()> {
    if journal.schema != 1 && journal.schema != 2 {
        return Err(Error::UnsupportedSchema(journal.schema));
    }

    let staging = Path::new(&journal.staging);
    let final_path = Path::new(&journal.final_path);
    let new_current = Path::new(&journal.new_current);

    ensure_within(staging, &layout.staging(), "staging")?;
    ensure_within(final_path, &layout.cellar(), "final path")?;
    ensure_within(new_current, &layout.cellar(), "new current")?;

    if let Some(old_current) = journal.old_current.as_deref() {
        ensure_within(Path::new(old_current), &layout.cellar(), "old current")?;
    }

    if let Some(commit) = &journal.commit {
        commit.receipt.validate()?;
        if commit.receipt.package != journal.package {
            return Err(Error::Transaction(
                "journal receipt has another package".into(),
            ));
        }
        if Path::new(&commit.receipt.active_path) != final_path {
            return Err(Error::Transaction(
                "journal receipt has another active path".into(),
            ));
        }
        if commit.receipt.exposures.len() != commit.exposures.len()
            || !commit.exposures.iter().all(|prepared| {
                commit.receipt.exposures.iter().any(|receipt| {
                    receipt.kind == prepared.receipt.kind
                        && receipt.path == prepared.receipt.path
                        && receipt.digest == prepared.receipt.digest
                })
            })
        {
            return Err(Error::Transaction(
                "journal receipt does not match its exposure plan".into(),
            ));
        }
        for exposure in &commit.exposures {
            ensure_exposure_path(layout, Path::new(&exposure.receipt.path))?;
            ensure_exposure_path(layout, Path::new(&exposure.temporary))?;
        }
    } else if journal.recovery == RecoveryAction::RollForward {
        return Err(Error::Transaction(
            "roll-forward journal has no commit plan".into(),
        ));
    }

    Ok(())
}

fn ensure_within(path: &Path, root: &Path, field: &str) -> Result<()> {
    if path.is_absolute() && path.starts_with(root) {
        Ok(())
    } else {
        Err(Error::Transaction(format!(
            "journal {field} is outside the managed root: {}",
            path.display()
        )))
    }
}

fn ensure_exposure_path(layout: &Layout, path: &Path) -> Result<()> {
    if path.is_absolute()
        && (path.starts_with(&layout.bin)
            || path.starts_with(&layout.applications)
            || path.starts_with(&layout.icons))
    {
        Ok(())
    } else {
        Err(Error::Transaction(format!(
            "journal exposure is outside managed roots: {}",
            path.display()
        )))
    }
}

fn recover_rollback(
    layout: &Layout,
    journal: &Journal,
    staging: &Path,
    final_path: &Path,
    current: &Path,
) -> Result<()> {
    if let Some(commit) = &journal.commit {
        integrations::ExposureTransaction::recover_rollback(layout, &commit.exposures)?;
    }
    if staging.exists() {
        std::fs::remove_dir_all(staging).at(staging)?;
    }
    if final_path.exists() {
        std::fs::remove_dir_all(final_path).at(final_path)?;
    }

    if let Some(old_current) = journal.old_current.as_deref() {
        let old_current = Path::new(old_current);
        if old_current.exists() {
            activate_symlink(old_current, current)?;
        } else {
            remove_symlink_if_present(current)?;
        }
    } else {
        remove_symlink_if_present(current)?;
    }
    Ok(())
}

fn recover_roll_forward(
    layout: &Layout,
    journal: &Journal,
    final_path: &Path,
    current: &Path,
) -> Result<()> {
    if !final_path.exists() {
        return Err(Error::Transaction(format!(
            "new version is missing for {}",
            journal.package
        )));
    }
    let commit = journal
        .commit
        .as_ref()
        .ok_or_else(|| Error::Transaction("roll-forward journal has no commit plan".into()))?;
    activate_symlink(final_path, current)?;
    integrations::ExposureTransaction::recover_commit(layout, &commit.exposures)?;
    commit
        .receipt
        .save_atomic(&layout.receipt(&journal.package)?)
}

/// Atomically replace the active-version symlink.
pub fn activate_symlink(target: &Path, current: &Path) -> Result<()> {
    let parent = current
        .parent()
        .ok_or_else(|| anyhow::anyhow!("current link has no parent"))?;
    std::fs::create_dir_all(parent).at(parent)?;

    let temporary = parent.join("current.new");
    if temporary.symlink_metadata().is_ok() {
        std::fs::remove_file(&temporary).at(&temporary)?;
    }

    std::os::unix::fs::symlink(target, &temporary).at(&temporary)?;
    std::fs::rename(&temporary, current).at(current)?;
    sync_directory(parent)
}

fn remove_symlink_if_present(path: &Path) -> Result<()> {
    if path.symlink_metadata().is_ok() {
        std::fs::remove_file(path).at(path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{layout::Layout, receipt::ExposureReceipt, Sha256Digest};

    fn journal(layout: &Layout, recovery: RecoveryAction) -> Journal {
        let old = layout.cellar().join("demo/1.0-1");
        let new = layout.cellar().join("demo/2.0-1");
        Journal {
            schema: 2,
            id: "demo-tx".into(),
            package: "demo".into(),
            phase: Phase::TreeCommitted,
            staging: layout.staging().join("demo-tx").display().to_string(),
            final_path: new.display().to_string(),
            old_current: Some(old.display().to_string()),
            new_current: new.display().to_string(),
            recovery,
            commit: None,
        }
    }

    #[test]
    fn rollback_recovery_never_activates_a_tree_without_commit_intent() {
        let directory = tempfile::tempdir().unwrap();
        let layout = Layout::for_test(directory.path());
        layout.ensure().unwrap();
        let journal = journal(&layout, RecoveryAction::Rollback);
        let old = PathBuf::from(&journal.old_current.clone().unwrap());
        let new = PathBuf::from(&journal.final_path);
        std::fs::create_dir_all(&old).unwrap();
        std::fs::create_dir_all(&new).unwrap();
        activate_symlink(&old, &layout.current_link("demo").unwrap()).unwrap();
        journal.save(&layout).unwrap();

        recover(&layout).unwrap();

        assert!(!new.exists());
        assert_eq!(
            std::fs::read_link(layout.current_link("demo").unwrap()).unwrap(),
            old
        );
    }

    #[test]
    fn roll_forward_recovery_publishes_exposures_and_receipt() {
        let directory = tempfile::tempdir().unwrap();
        let layout = Layout::for_test(directory.path());
        layout.ensure().unwrap();
        let mut journal = journal(&layout, RecoveryAction::RollForward);
        let new = PathBuf::from(&journal.final_path);
        std::fs::create_dir_all(&new).unwrap();
        let path = layout.bin.join("demo");
        let temporary = layout.bin.join(".demo.pako-demo-tx.new");
        let data = b"#!/bin/sh\n".to_vec();
        std::fs::write(&temporary, &data).unwrap();
        let digest = Sha256Digest::calculate(&data);
        let receipt = Receipt {
            schema: 1,
            package: "demo".into(),
            upstream_version: "2.0".into(),
            release: 1,
            target: "x86_64-unknown-linux-gnu".into(),
            repository: "test".into(),
            oci_manifest_digest: digest,
            package_manifest_digest: digest,
            pack_index_digest: digest,
            tree_digest: digest,
            active_path: new.display().to_string(),
            installed_at: "0".into(),
            previous_versions: vec!["1.0-1".into()],
            exposures: vec![ExposureReceipt {
                kind: "launcher".into(),
                path: path.display().to_string(),
                digest,
            }],
        };
        journal.commit = Some(CommitPlan {
            receipt,
            exposures: vec![PreparedExposure {
                temporary: temporary.display().to_string(),
                receipt: ExposureReceipt {
                    kind: "launcher".into(),
                    path: path.display().to_string(),
                    digest,
                },
            }],
        });
        journal.save(&layout).unwrap();

        recover(&layout).unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), data);
        assert_eq!(
            std::fs::read_link(layout.current_link("demo").unwrap()).unwrap(),
            new
        );
        assert_eq!(
            Receipt::load(&layout.receipt("demo").unwrap())
                .unwrap()
                .package,
            "demo"
        );
    }
}
