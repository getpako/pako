use std::{
    collections::VecDeque,
    fs::OpenOptions,
    os::unix::fs::{symlink, OpenOptionsExt as _, PermissionsExt},
    path::Path,
    sync::Mutex,
};

use sha2::{Digest as _, Sha256};

use crate::{
    error::IoContext,
    manifest::{Entry, PackageManifest},
    object_store::ObjectStore,
    Error, Result, Sha256Digest,
};

/// Build a clean package tree from verified content-addressed chunks.
pub fn materialize(
    manifest: &PackageManifest,
    store: &ObjectStore,
    destination: &Path,
) -> Result<()> {
    let jobs = std::thread::available_parallelism().map_or(1, usize::from);
    materialize_with_jobs(manifest, store, destination, jobs)
}

/// Build a package tree with bounded parallel file materialization.
///
/// Directories initially use private writable permissions. Their final modes
/// are applied only after files and symlinks have been created, preventing a
/// read-only package directory from blocking its own materialization.
pub fn materialize_with_jobs(
    manifest: &PackageManifest,
    store: &ObjectStore,
    destination: &Path,
    jobs: usize,
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
    create_files_parallel(manifest, store, destination, jobs)?;
    create_symlinks(manifest, destination)?;
    apply_directory_permissions(manifest, destination)
}

fn create_directories(manifest: &PackageManifest, destination: &Path) -> Result<()> {
    for entry in &manifest.entries {
        let Entry::Directory { path, .. } = entry else {
            continue;
        };

        let output = path.join_to(destination);
        ensure_no_symlink_ancestor(destination, &output)?;
        std::fs::create_dir(&output).at(&output)?;
        std::fs::set_permissions(&output, std::fs::Permissions::from_mode(0o700)).at(&output)?;
    }

    Ok(())
}

fn create_files_parallel(
    manifest: &PackageManifest,
    store: &ObjectStore,
    destination: &Path,
    jobs: usize,
) -> Result<()> {
    let files = manifest
        .entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| matches!(entry, Entry::File { .. }))
        .collect::<Vec<_>>();
    if files.is_empty() {
        return Ok(());
    }

    let worker_count = jobs.max(1).min(files.len());
    log::debug!(
        "materializing {} file(s) with {worker_count} worker(s)",
        files.len()
    );
    let queue = Mutex::new(VecDeque::from(files));
    let results = Mutex::new(Vec::new());

    std::thread::scope(|scope| {
        for _ in 0..worker_count {
            let queue = &queue;
            let results = &results;
            scope.spawn(move || loop {
                let Some((index, entry)) = queue
                    .lock()
                    .expect("materialization queue lock poisoned")
                    .pop_front()
                else {
                    return;
                };

                let result = create_file(entry, store, destination);
                results
                    .lock()
                    .expect("materialization result lock poisoned")
                    .push((index, result));
            });
        }
    });

    let mut completed = results
        .into_inner()
        .expect("materialization result lock poisoned");
    completed.sort_by_key(|(index, _)| *index);
    for (_, result) in completed {
        result?;
    }
    Ok(())
}

fn create_file(entry: &Entry, store: &ObjectStore, destination: &Path) -> Result<()> {
    let Entry::File {
        path,
        mode,
        size,
        digest,
        chunks,
    } = entry
    else {
        return Ok(());
    };

    let output = path.join_to(destination);
    ensure_no_symlink_ancestor(destination, &output)?;

    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent).at(parent)?;
    }

    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&output)
        .at(&output)?;
    let mut file_hash = Sha256::new();
    let mut written = 0_u64;

    for chunk in chunks {
        let copied = store.copy_verified(chunk.digest, &mut file, &mut file_hash)?;
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

    std::fs::set_permissions(&output, std::fs::Permissions::from_mode(u32::from(*mode))).at(&output)
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

fn apply_directory_permissions(manifest: &PackageManifest, destination: &Path) -> Result<()> {
    let mut directories = manifest
        .entries
        .iter()
        .filter_map(|entry| match entry {
            Entry::Directory { path, mode } => Some((path, *mode)),
            Entry::File { .. } | Entry::Symlink { .. } => None,
        })
        .collect::<Vec<_>>();
    directories.sort_by_key(|(path, _)| std::cmp::Reverse(path.as_str().matches('/').count()));

    for (path, mode) in directories {
        let output = path.join_to(destination);
        std::fs::set_permissions(&output, std::fs::Permissions::from_mode(u32::from(mode)))
            .at(&output)?;
    }
    Ok(())
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
