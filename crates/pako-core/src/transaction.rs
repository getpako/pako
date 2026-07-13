use std::{
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
};

use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::{canonical, error::IoContext, layout::Layout, receipt::sync_directory, Error, Result};

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Phase {
    Prepared,
    Materialized,
    Verified,
    Committed,
    Exposed,
    ReceiptWritten,
    Complete,
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

        match journal.phase {
            Phase::Prepared | Phase::Materialized | Phase::Verified => {
                if staging.exists() {
                    std::fs::remove_dir_all(&staging).at(&staging)?;
                }
            }
            Phase::Committed | Phase::Exposed | Phase::ReceiptWritten | Phase::Complete => {
                recover_activation(&journal, &final_path, &current)?;
            }
        }

        std::fs::remove_file(&path).at(&path)?;
        recovered.push(journal.package);
    }

    Ok(recovered)
}

fn validate_journal(layout: &Layout, journal: &Journal) -> Result<()> {
    if journal.schema != 1 {
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

fn recover_activation(journal: &Journal, final_path: &Path, current: &Path) -> Result<()> {
    if final_path.exists() {
        return activate_symlink(final_path, current);
    }

    if let Some(old_current) = journal.old_current.as_deref() {
        let old_current = Path::new(old_current);
        if old_current.exists() {
            return activate_symlink(old_current, current);
        }
    }

    Err(Error::Transaction(format!(
        "neither new nor previous version exists for {}",
        journal.package
    )))
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
