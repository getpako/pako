use std::{
    fs::OpenOptions,
    io::{Read, Write},
    os::unix::fs::{symlink, PermissionsExt},
    path::Path,
};

use sha2::{Digest as _, Sha256};

use crate::{
    error::IoContext,
    manifest::{Entry, PackageManifest},
    object_store::ObjectStore,
    Error, Result, Sha256Digest,
};

/// Build a clean package tree from verified content-addressed chunks.
///
/// Materialization never writes through a package-provided symlink. Files are
/// created with `create_new` so an unexpected existing path fails closed.
pub fn materialize(
    manifest: &PackageManifest,
    store: &ObjectStore,
    destination: &Path,
) -> Result<()> {
    manifest.validate()?;

    if destination.exists() {
        return Err(Error::InvalidManifest(format!(
            "destination already exists: {}",
            destination.display()
        )));
    }

    std::fs::create_dir_all(destination).at(destination)?;
    create_directories(manifest, destination)?;
    create_files(manifest, store, destination)?;
    create_symlinks(manifest, destination)?;
    Ok(())
}

fn create_directories(manifest: &PackageManifest, destination: &Path) -> Result<()> {
    for entry in &manifest.entries {
        let Entry::Directory { path, mode } = entry else {
            continue;
        };

        let output = path.join_to(destination);
        ensure_no_symlink_ancestor(destination, &output)?;
        std::fs::create_dir(&output).at(&output)?;
        std::fs::set_permissions(&output, std::fs::Permissions::from_mode(u32::from(*mode)))
            .at(&output)?;
    }

    Ok(())
}

fn create_files(manifest: &PackageManifest, store: &ObjectStore, destination: &Path) -> Result<()> {
    for entry in &manifest.entries {
        let Entry::File {
            path,
            mode,
            size,
            digest,
            chunks,
        } = entry
        else {
            continue;
        };

        let output = path.join_to(destination);
        ensure_no_symlink_ancestor(destination, &output)?;

        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent).at(parent)?;
        }

        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&output)
            .at(&output)?;
        let mut file_hash = Sha256::new();
        let mut written = 0_u64;

        for chunk in chunks {
            let mut source = store.open_verified(chunk.digest)?;
            let copied = copy_hashing(&mut source, &mut file, &mut file_hash)?;

            if copied != u64::from(chunk.size) {
                return Err(Error::Integrity {
                    path: output.clone(),
                    expected: chunk.size.to_string(),
                    actual: copied.to_string(),
                });
            }

            written = written
                .checked_add(copied)
                .ok_or_else(|| anyhow::anyhow!("materialized file size overflow"))?;
        }

        file.sync_all().at(&output)?;

        if written != *size {
            return Err(Error::Integrity {
                path: output.clone(),
                expected: size.to_string(),
                actual: written.to_string(),
            });
        }

        let actual_digest = Sha256Digest::from_bytes(file_hash.finalize().into());
        if actual_digest != *digest {
            return Err(Error::Integrity {
                path: output.clone(),
                expected: digest.to_string(),
                actual: actual_digest.to_string(),
            });
        }

        std::fs::set_permissions(&output, std::fs::Permissions::from_mode(u32::from(*mode)))
            .at(&output)?;
    }

    Ok(())
}

fn create_symlinks(manifest: &PackageManifest, destination: &Path) -> Result<()> {
    for entry in &manifest.entries {
        let Entry::Symlink { path, target } = entry else {
            continue;
        };

        let output = path.join_to(destination);
        ensure_no_symlink_ancestor(destination, &output)?;

        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent).at(parent)?;
        }

        symlink(target, &output).at(&output)?;
    }

    Ok(())
}

fn copy_hashing(mut input: impl Read, mut output: impl Write, hash: &mut Sha256) -> Result<u64> {
    let mut buffer = vec![0_u8; 128 * 1024];
    let mut total = 0_u64;

    loop {
        let count = input.read(&mut buffer).map_err(anyhow::Error::from)?;
        if count == 0 {
            break;
        }

        output
            .write_all(&buffer[..count])
            .map_err(anyhow::Error::from)?;
        hash.update(&buffer[..count]);
        total = total
            .checked_add(count as u64)
            .ok_or_else(|| anyhow::anyhow!("copy size overflow"))?;
    }

    Ok(total)
}

fn ensure_no_symlink_ancestor(root: &Path, path: &Path) -> Result<()> {
    let relative = path.strip_prefix(root).map_err(anyhow::Error::from)?;
    let component_count = relative.components().count();
    let mut current = root.to_owned();

    for component in relative
        .components()
        .take(component_count.saturating_sub(1))
    {
        current.push(component);

        if let Ok(metadata) = std::fs::symlink_metadata(&current) {
            if metadata.file_type().is_symlink() {
                return Err(Error::InvalidPackagePath(current.display().to_string()));
            }
        }
    }

    Ok(())
}
