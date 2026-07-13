use std::{
    fs::File,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use crate::{
    error::IoContext,
    layout::Layout,
    manifest::{DesktopEntry, Entry, Icon, Launcher, PackageManifest},
    receipt::ExposureReceipt,
    Error, Result, Sha256Digest,
};

/// Create launchers, desktop entries and icons outside the managed package
/// tree. Every created file is recorded by digest so removal never deletes a
/// file which has since been replaced by the user.
pub fn install(manifest: &PackageManifest, layout: &Layout) -> Result<Vec<ExposureReceipt>> {
    let mut receipts = Vec::new();

    for launcher in &manifest.integrations.launchers {
        receipts.push(install_launcher(manifest, launcher, layout)?);
    }

    let declared_launchers: std::collections::BTreeSet<_> = manifest
        .integrations
        .launchers
        .iter()
        .map(|launcher| launcher.name.as_str())
        .collect();
    for entry in &manifest.entries {
        let Entry::File { path, mode, .. } = entry else {
            continue;
        };
        let Some(name) = binary_name(path.as_str()) else {
            continue;
        };
        if mode & 0o111 != 0 && !declared_launchers.contains(name) {
            receipts.push(install_binary(manifest, name, path.as_str(), layout)?);
        }
    }

    for desktop_entry in &manifest.integrations.desktop_entries {
        receipts.push(install_desktop_entry(desktop_entry, layout)?);
    }

    for icon in &manifest.integrations.icons {
        receipts.push(install_icon(manifest, icon, layout)?);
    }

    Ok(receipts)
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

fn install_launcher(
    manifest: &PackageManifest,
    launcher: &Launcher,
    layout: &Layout,
) -> Result<ExposureReceipt> {
    let path = layout.bin.join(&launcher.name);
    ensure_available(&path)?;

    let content = render_launcher(&manifest.package, launcher, layout);
    write_exposure(&path, content.as_bytes(), 0o755)?;
    Ok(exposure_receipt("launcher", &path, content.as_bytes()))
}

fn install_binary(
    manifest: &PackageManifest,
    name: &str,
    target: &str,
    layout: &Layout,
) -> Result<ExposureReceipt> {
    let path = layout.bin.join(name);
    ensure_available(&path)?;

    let content = render_binary_launcher(&manifest.package, target, layout);
    write_exposure(&path, content.as_bytes(), 0o755)?;
    Ok(exposure_receipt("bin", &path, content.as_bytes()))
}

fn install_desktop_entry(desktop_entry: &DesktopEntry, layout: &Layout) -> Result<ExposureReceipt> {
    let path = layout
        .applications
        .join(format!("pako-{}.desktop", desktop_entry.id));
    ensure_available(&path)?;

    let content = render_desktop_entry(desktop_entry);
    write_exposure(&path, content.as_bytes(), 0o644)?;
    Ok(exposure_receipt("desktop", &path, content.as_bytes()))
}

fn install_icon(
    manifest: &PackageManifest,
    icon: &Icon,
    layout: &Layout,
) -> Result<ExposureReceipt> {
    let (directory, extension) = icon_destination(icon, layout)?;
    std::fs::create_dir_all(&directory).at(&directory)?;

    let path = directory.join(format!("{}.{}", icon.name, extension));
    ensure_available(&path)?;

    let source = layout
        .current_link(&manifest.package)?
        .join(icon.source.as_str());
    let data = std::fs::read(&source).at(&source)?;
    write_exposure(&path, &data, 0o644)?;

    Ok(exposure_receipt("icon", &path, &data))
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

fn render_binary_launcher(package: &str, target: &str, layout: &Layout) -> String {
    let executable = layout
        .current_link(package)
        .expect("package name was validated with its manifest")
        .join(target);
    format!(
        "#!/bin/sh\nexec {} \"$@\"\n",
        shell_quote(&executable.display().to_string())
    )
}

fn binary_name(path: &str) -> Option<&str> {
    let name = path.strip_prefix("bin/")?;
    (!name.is_empty() && !name.contains('/')).then_some(name)
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

    let temporary = temporary_path(path);
    let mut file = File::create(&temporary).at(&temporary)?;
    file.write_all(data).at(&temporary)?;
    file.sync_all().at(&temporary)?;
    std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(mode)).at(&temporary)?;
    std::fs::rename(&temporary, path).at(path)?;
    Ok(())
}

fn temporary_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("exposure");
    path.with_file_name(format!(".{file_name}.pako-new"))
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
