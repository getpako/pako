use std::{
    collections::BTreeSet,
    fs::{File, OpenOptions},
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use fs2::FileExt;

use crate::{
    error::IoContext,
    layout::Layout,
    manifest::{DesktopEntry, Icon, Launcher, PackageManifest},
    receipt::ExposureReceipt,
    Error, Result, Sha256Digest,
};

/// An exposure prepared under private sibling paths.
///
/// `temporary` contains the new contents for create/replace actions. `backup`
/// preserves the previous package-owned contents until the transaction is
/// durable. Removal actions set `remove` and carry the previous receipt.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreparedExposure {
    pub temporary: String,
    pub receipt: ExposureReceipt,
    #[serde(default)]
    pub previous: Option<ExposureReceipt>,
    #[serde(default)]
    pub backup: Option<String>,
    #[serde(default)]
    pub remove: bool,
}

#[derive(Debug)]
struct PlannedExposure {
    kind: &'static str,
    path: PathBuf,
    data: Vec<u8>,
    mode: u32,
    previous: Option<ExposureReceipt>,
    remove: bool,
}

/// Serializes all package integrations. Package locks cannot prevent two
/// distinct packages from claiming the same launcher or desktop entry.
#[derive(Debug)]
pub struct ExposureTransaction {
    transaction_id: String,
    planned: Vec<PlannedExposure>,
    prepared: Vec<PreparedExposure>,
    _lock: File,
}

impl ExposureTransaction {
    pub fn begin(layout: &Layout, transaction_id: impl Into<String>) -> Result<Self> {
        let directory = layout.locks();
        std::fs::create_dir_all(&directory).at(&directory)?;
        let path = directory.join("exposures.lock");
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .at(&path)?;
        lock.lock_exclusive().at(&path)?;
        Ok(Self {
            transaction_id: transaction_id.into(),
            planned: Vec::new(),
            prepared: Vec::new(),
            _lock: lock,
        })
    }

    /// Calculate every destination and reject conflicts before touching paths
    /// outside the package tree. Existing files may only be replaced when they
    /// still match a receipt owned by the package being upgraded.
    pub fn preflight(
        &mut self,
        manifest: &PackageManifest,
        layout: &Layout,
        tree: &Path,
        previous_receipts: &[ExposureReceipt],
    ) -> Result<()> {
        let mut planned = Vec::new();

        for launcher in &manifest.integrations.launchers {
            let path = layout.bin.join(&launcher.name);
            let content = render_launcher(&manifest.package, launcher, layout);
            planned.push(PlannedExposure {
                kind: "launcher",
                path,
                data: content.into_bytes(),
                mode: 0o755,
                previous: None,
                remove: false,
            });
        }
        for desktop_entry in &manifest.integrations.desktop_entries {
            let path = layout
                .applications
                .join(format!("pako-{}.desktop", desktop_entry.id));
            let content = render_desktop_entry(desktop_entry);
            planned.push(PlannedExposure {
                kind: "desktop",
                path,
                data: content.into_bytes(),
                mode: 0o644,
                previous: None,
                remove: false,
            });
        }
        for icon in &manifest.integrations.icons {
            let (directory, extension) = icon_destination(icon, layout)?;
            let path = directory.join(format!("{}.{}", icon.name, extension));
            let source = icon.source.join_to(tree);
            planned.push(PlannedExposure {
                kind: "icon",
                path,
                data: std::fs::read(&source).at(&source)?,
                mode: 0o644,
                previous: None,
                remove: false,
            });
        }

        let mut destinations = BTreeSet::new();
        for plan in &mut planned {
            if !destinations.insert(plan.path.clone()) {
                return Err(Error::InvalidManifest(format!(
                    "duplicate integration destination: {}",
                    plan.path.display()
                )));
            }

            let previous = previous_receipts
                .iter()
                .find(|receipt| Path::new(&receipt.path) == plan.path.as_path())
                .cloned();
            validate_existing_destination(&plan.path, previous.as_ref())?;
            plan.previous = previous.filter(|_| plan.path.exists());
            ensure_available(&temporary_path(&plan.path, &self.transaction_id))?;
            ensure_available(&backup_path(&plan.path, &self.transaction_id))?;
        }

        for receipt in previous_receipts {
            let path = PathBuf::from(&receipt.path);
            if destinations.contains(&path) {
                continue;
            }
            validate_existing_destination(&path, Some(receipt))?;
            if path.exists() {
                planned.push(PlannedExposure {
                    kind: "removed",
                    path,
                    data: Vec::new(),
                    mode: 0,
                    previous: Some(receipt.clone()),
                    remove: true,
                });
            }
        }

        self.planned = planned;
        Ok(())
    }

    /// Write new artifacts and durable rollback copies under private names.
    pub fn prepare(&mut self) -> Result<&[PreparedExposure]> {
        for plan in &self.planned {
            let temporary = temporary_path(&plan.path, &self.transaction_id);
            let backup = plan
                .previous
                .as_ref()
                .map(|_| backup_path(&plan.path, &self.transaction_id));

            if let Some(backup) = &backup {
                if let Err(error) = std::fs::hard_link(&plan.path, backup).at(backup) {
                    let _ = self.rollback();
                    return Err(error);
                }
            }

            if !plan.remove {
                if let Err(error) = write_exposure(&temporary, &plan.data, plan.mode) {
                    if let Some(backup) = &backup {
                        let _ = remove_file_if_present(backup);
                    }
                    let _ = self.rollback();
                    return Err(error);
                }
            }

            self.prepared.push(PreparedExposure {
                temporary: temporary.display().to_string(),
                receipt: if plan.remove {
                    plan.previous
                        .clone()
                        .expect("removal plans always have a previous receipt")
                } else {
                    exposure_receipt(plan.kind, &plan.path, &plan.data)
                },
                previous: plan.previous.clone(),
                backup: backup.map(|path| path.display().to_string()),
                remove: plan.remove,
            });
        }
        Ok(&self.prepared)
    }

    /// Publish the prepared create, replace, and remove actions while the
    /// global exposure lock is held. Backups remain until `finalize`.
    pub fn commit(&mut self) -> Result<()> {
        publish(&self.prepared)
    }

    /// Restore package-owned files from backups and remove newly-created files.
    pub fn rollback(&mut self) -> Result<()> {
        rollback_prepared(&self.prepared)?;
        self.prepared.clear();
        Ok(())
    }

    /// Delete private temporary and backup paths after the package state is
    /// durable. This operation is idempotent and safe during roll-forward.
    pub fn finalize(&mut self) -> Result<()> {
        finalize_prepared(&self.prepared)?;
        self.prepared.clear();
        Ok(())
    }

    pub fn prepared(&self) -> &[PreparedExposure] {
        &self.prepared
    }

    pub fn published_receipts(&self) -> Vec<ExposureReceipt> {
        self.prepared
            .iter()
            .filter(|exposure| !exposure.remove)
            .map(|exposure| exposure.receipt.clone())
            .collect()
    }

    /// Recovery gets the same global lock before idempotently completing a
    /// journaled publication.
    pub fn recover_commit(layout: &Layout, prepared: &[PreparedExposure]) -> Result<()> {
        let mut transaction = Self::begin(layout, "recovery")?;
        transaction.prepared = prepared.to_vec();
        transaction.commit()
    }

    pub fn recover_finalize(layout: &Layout, prepared: &[PreparedExposure]) -> Result<()> {
        let mut transaction = Self::begin(layout, "recovery")?;
        transaction.prepared = prepared.to_vec();
        transaction.finalize()
    }

    pub fn recover_rollback(layout: &Layout, prepared: &[PreparedExposure]) -> Result<()> {
        let mut transaction = Self::begin(layout, "recovery")?;
        transaction.prepared = prepared.to_vec();
        transaction.rollback()
    }
}

fn publish(prepared: &[PreparedExposure]) -> Result<()> {
    for exposure in prepared {
        let destination = Path::new(&exposure.receipt.path);
        let temporary = Path::new(&exposure.temporary);

        if exposure.remove {
            if destination.exists() {
                ensure_digest(destination, exposure.receipt.digest)?;
                std::fs::remove_file(destination).at(destination)?;
            }
            continue;
        }

        if destination.exists() {
            let data = std::fs::read(destination).at(destination)?;
            if Sha256Digest::calculate(&data) == exposure.receipt.digest {
                remove_file_if_present(temporary)?;
                continue;
            }

            let previous = exposure
                .previous
                .as_ref()
                .ok_or_else(|| Error::ExposureConflict(destination.to_owned()))?;
            ensure_digest(destination, previous.digest)?;
            if !temporary.exists() {
                return Err(Error::Transaction(format!(
                    "prepared exposure is missing: {}",
                    temporary.display()
                )));
            }
            std::fs::rename(temporary, destination).at(destination)?;
            continue;
        }

        if !temporary.exists() {
            return Err(Error::Transaction(format!(
                "prepared exposure is missing: {}",
                temporary.display()
            )));
        }
        std::fs::hard_link(temporary, destination).at(destination)?;
        std::fs::remove_file(temporary).at(temporary)?;
    }
    Ok(())
}

fn rollback_prepared(prepared: &[PreparedExposure]) -> Result<()> {
    for exposure in prepared.iter().rev() {
        let destination = Path::new(&exposure.receipt.path);
        let temporary = Path::new(&exposure.temporary);

        if let Some(previous) = &exposure.previous {
            if let Some(backup) = exposure.backup.as_deref().map(Path::new) {
                if backup.exists() {
                    if destination.exists() {
                        let data = std::fs::read(destination).at(destination)?;
                        let digest = Sha256Digest::calculate(&data);
                        if digest != exposure.receipt.digest && digest != previous.digest {
                            return Err(Error::ExposureConflict(destination.to_owned()));
                        }
                    }
                    std::fs::rename(backup, destination).at(destination)?;
                }
            }
        } else if !exposure.remove && destination.exists() {
            ensure_digest(destination, exposure.receipt.digest)?;
            std::fs::remove_file(destination).at(destination)?;
        }

        remove_file_if_present(temporary)?;
        if let Some(backup) = exposure.backup.as_deref().map(Path::new) {
            remove_file_if_present(backup)?;
        }
    }
    Ok(())
}

fn finalize_prepared(prepared: &[PreparedExposure]) -> Result<()> {
    for exposure in prepared {
        remove_file_if_present(Path::new(&exposure.temporary))?;
        if let Some(backup) = exposure.backup.as_deref().map(Path::new) {
            remove_file_if_present(backup)?;
        }
    }
    Ok(())
}

pub fn cleanup_prepared(prepared: &[PreparedExposure]) {
    let _ = finalize_prepared(prepared);
}

/// Remove only exposure files whose content still matches the receipt.
pub fn remove(receipts: &[ExposureReceipt]) -> Result<()> {
    for receipt in receipts {
        let path = Path::new(&receipt.path);
        if !path.exists() {
            continue;
        }

        let data = std::fs::read(path).at(path)?;
        if Sha256Digest::calculate(&data) == receipt.digest {
            std::fs::remove_file(path).at(path)?;
        }
    }

    Ok(())
}

fn render_launcher(package: &str, launcher: &Launcher, layout: &Layout) -> String {
    let executable = layout
        .current_link(package)
        .expect("package name was validated with its manifest")
        .join(launcher.target.as_str());
    let arguments = launcher
        .arguments
        .iter()
        .map(|argument| shell_quote(argument))
        .collect::<Vec<_>>()
        .join(" ");

    if arguments.is_empty() {
        format!(
            "#!/bin/sh\nexec {} \"$@\"\n",
            shell_quote(&executable.display().to_string())
        )
    } else {
        format!(
            "#!/bin/sh\nexec {} {} \"$@\"\n",
            shell_quote(&executable.display().to_string()),
            arguments
        )
    }
}

fn render_desktop_entry(entry: &DesktopEntry) -> String {
    format!(
        concat!(
            "[Desktop Entry]\n",
            "Type=Application\n",
            "Name={}\n",
            "Exec={}\n",
            "Icon={}\n",
            "Terminal={}\n",
            "Categories={};\n",
        ),
        escape_desktop_value(&entry.name),
        escape_desktop_value(&entry.exec),
        escape_desktop_value(&entry.icon),
        if entry.terminal { "true" } else { "false" },
        entry.categories.join(";"),
    )
}

fn write_exposure(path: &Path, data: &[u8], mode: u32) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).at(parent)?;
    }

    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .at(path)?;
    file.write_all(data).at(path)?;
    file.sync_all().at(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).at(path)?;
    Ok(())
}

fn temporary_path(path: &Path, transaction_id: &str) -> PathBuf {
    private_path(path, transaction_id, "new")
}

fn backup_path(path: &Path, transaction_id: &str) -> PathBuf {
    private_path(path, transaction_id, "old")
}

fn private_path(path: &Path, transaction_id: &str, suffix: &str) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("exposure");
    path.with_file_name(format!(
        ".{file_name}.pako-{transaction_id}.{suffix}"
    ))
}

fn exposure_receipt(kind: &str, path: &Path, data: &[u8]) -> ExposureReceipt {
    ExposureReceipt {
        kind: kind.into(),
        path: path.display().to_string(),
        digest: Sha256Digest::calculate(data),
    }
}

fn validate_existing_destination(
    path: &Path,
    previous: Option<&ExposureReceipt>,
) -> Result<()> {
    let Ok(metadata) = path.symlink_metadata() else {
        return Ok(());
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(Error::ExposureConflict(path.to_owned()));
    }

    let previous = previous.ok_or_else(|| Error::ExposureConflict(path.to_owned()))?;
    ensure_digest(path, previous.digest)
}

fn ensure_digest(path: &Path, expected: Sha256Digest) -> Result<()> {
    let data = std::fs::read(path).at(path)?;
    if Sha256Digest::calculate(&data) == expected {
        Ok(())
    } else {
        Err(Error::ExposureConflict(path.to_owned()))
    }
}

fn ensure_available(path: &Path) -> Result<()> {
    if path.symlink_metadata().is_ok() {
        return Err(Error::ExposureConflict(path.to_owned()));
    }
    Ok(())
}

fn remove_file_if_present(path: &Path) -> Result<()> {
    if path.symlink_metadata().is_ok() {
        std::fs::remove_file(path).at(path)?;
    }
    Ok(())
}

fn icon_destination(icon: &Icon, layout: &Layout) -> Result<(PathBuf, &'static str)> {
    let extension = Path::new(icon.source.as_str())
        .extension()
        .and_then(|value| value.to_str())
        .ok_or_else(|| anyhow::anyhow!("icon source has no extension"))?;

    let extension = match extension {
        "svg" => "svg",
        "png" => "png",
        _ => return Err(anyhow::anyhow!("unsupported icon type").into()),
    };

    Ok((layout.icons.join(&icon.size).join(&icon.context), extension))
}

fn shell_quote(value: &str) -> String {
    if value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"/_-.".contains(&byte))
    {
        return value.to_owned();
    }

    format!("'{}'", value.replace('\'', "'\\''"))
}

fn escape_desktop_value(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\r', "")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_exposure_lock_excludes_another_transaction() {
        let directory = tempfile::tempdir().unwrap();
        let layout = Layout::for_test(directory.path());
        layout.ensure().unwrap();
        let _transaction = ExposureTransaction::begin(&layout, "first").unwrap();

        let path = layout.locks().join("exposures.lock");
        let second = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .unwrap();
        assert!(second.try_lock_exclusive().is_err());
    }
}
