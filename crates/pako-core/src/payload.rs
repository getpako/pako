//! Safe extraction of a package `payload.tar.zst` archive.

use std::{
    fs::File,
    path::{Component, Path},
};

use tar::Archive;

use crate::{
    path::{validate_symlink_target, PackagePath},
    Error, Result,
};

/// Extract a payload into an empty staging directory.
pub fn extract(archive: &Path, destination: &Path) -> Result<()> {
    extract_inner(archive, destination)?;
    Ok(())
}

fn extract_inner(archive: &Path, destination: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(destination)?;
    let decoder = zstd::stream::read::Decoder::new(File::open(archive)?)?;
    let mut archive = Archive::new(decoder);

    for entry in archive.entries()? {
        let mut entry = entry?;
        let kind = entry.header().entry_type();
        if !(kind.is_file() || kind.is_dir() || kind.is_symlink()) {
            anyhow::bail!("unsupported payload entry type");
        }
        let raw_path = entry.path()?.into_owned();
        let relative = validate_path(&raw_path)?;
        let output = destination.join(relative.as_str());
        ensure_no_symlink_ancestor(destination, &output)?;
        if kind.is_symlink() {
            let target = entry
                .link_name()?
                .ok_or_else(|| anyhow::anyhow!("payload symlink has no target"))?;
            let target = target
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("non UTF-8 payload symlink target"))?;
            validate_symlink_target(&relative, target)?;
        }
        entry.unpack(&output)?;
    }
    Ok(())
}

fn validate_path(path: &Path) -> Result<PackagePath> {
    if path
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(Error::InvalidManifest(format!(
            "unsafe payload path: {}",
            path.display()
        )));
    }
    let value = path
        .to_str()
        .ok_or_else(|| Error::InvalidManifest("non UTF-8 payload path".into()))?;
    PackagePath::new(value.to_owned())
}

fn ensure_no_symlink_ancestor(root: &Path, path: &Path) -> anyhow::Result<()> {
    let relative = path.strip_prefix(root)?;
    let mut current = root.to_owned();
    let count = relative.components().count().saturating_sub(1);
    for component in relative.components().take(count) {
        current.push(component);
        if current
            .symlink_metadata()
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            anyhow::bail!("payload entry traverses symlink: {}", current.display());
        }
    }
    Ok(())
}
