use std::{
    fs::OpenOptions,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use crate::{
    error::IoContext,
    layout::Layout,
    manifest::{DesktopEntry, Icon, Launcher, PackageManifest},
    receipt::ExposureReceipt,
    Error, Result, Sha256Digest,
};

/// An exposure written under a private name, ready to be published during the
/// transaction commit. Keeping this plan in the journal makes publication
/// idempotent during recovery.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreparedExposure {
    pub temporary: String,
    pub receipt: ExposureReceipt,
}

/// Check every conflict and write all integration files under transaction
/// private names. Nothing is visible at its final destination yet.
pub fn prepare(
    manifest: &PackageManifest,
    layout: &Layout,
    tree: &Path,
    transaction_id: &str,
) -> Result<Vec<PreparedExposure>> {
    let mut planned = Vec::new();

    for launcher in &manifest.integrations.launchers {
        let path = layout.bin.join(&launcher.name);
        let content = render_launcher(&manifest.package, launcher, layout);
        planned.push(("launcher", path, content.into_bytes(), 0o755));
    }
    for desktop_entry in &manifest.integrations.desktop_entries {
        let path = layout
            .applications
            .join(format!("pako-{}.desktop", desktop_entry.id));
        let content = render_desktop_entry(desktop_entry);
        planned.push(("desktop", path, content.into_bytes(), 0o644));
    }
    for icon in &manifest.integrations.icons {
        let (directory, extension) = icon_destination(icon, layout)?;
        let path = directory.join(format!("{}.{}", icon.name, extension));
        let source = tree.join(icon.source.as_str());
        planned.push(("icon", path, std::fs::read(&source).at(&source)?, 0o644));
    }

    // Do the complete conflict pass before creating any temporary file.
    for (_, path, _, _) in &planned {
        ensure_available(path)?;
        ensure_available(&temporary_path(path, transaction_id))?;
    }

    let mut prepared = Vec::new();
    for (kind, path, data, mode) in planned {
        let temporary = temporary_path(&path, transaction_id);
        if let Err(error) = write_exposure(&temporary, &data, mode) {
            cleanup_prepared(&prepared);
            return Err(error);
        }
        prepared.push(PreparedExposure {
            temporary: temporary.display().to_string(),
            receipt: exposure_receipt(kind, &path, &data),
        });
    }
    Ok(prepared)
}

/// Publish prepared files. It is safe to call repeatedly after a crash: an
/// already-published file must match the transaction receipt.
pub fn publish(prepared: &[PreparedExposure]) -> Result<()> {
    for exposure in prepared {
        let destination = Path::new(&exposure.receipt.path);
        let temporary = Path::new(&exposure.temporary);
        if destination.exists() {
            let data = std::fs::read(destination).at(destination)?;
            if Sha256Digest::calculate(&data) != exposure.receipt.digest {
                return Err(Error::ExposureConflict(destination.to_owned()));
            }
            if temporary.symlink_metadata().is_ok() {
                std::fs::remove_file(temporary).at(temporary)?;
            }
            continue;
        }
        if !temporary.exists() {
            return Err(Error::Transaction(format!(
                "prepared exposure is missing: {}",
                temporary.display()
            )));
        }
        // `rename` would replace a file created after preflight. Linking is
        // an atomic no-replace publication because temporary and destination
        // are deliberately siblings in the same filesystem.
        std::fs::hard_link(temporary, destination).at(destination)?;
        std::fs::remove_file(temporary).at(temporary)?;
    }
    Ok(())
}

pub fn cleanup_prepared(prepared: &[PreparedExposure]) {
    for exposure in prepared {
        let path = Path::new(&exposure.temporary);
        if path.symlink_metadata().is_ok() {
            let _ = std::fs::remove_file(path);
        }
    }
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
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("exposure");
    path.with_file_name(format!(".{file_name}.pako-{transaction_id}.new"))
}

fn exposure_receipt(kind: &str, path: &Path, data: &[u8]) -> ExposureReceipt {
    ExposureReceipt {
        kind: kind.into(),
        path: path.display().to_string(),
        digest: Sha256Digest::calculate(data),
    }
}

fn ensure_available(path: &Path) -> Result<()> {
    if path.symlink_metadata().is_ok() {
        return Err(Error::ExposureConflict(path.to_owned()));
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
