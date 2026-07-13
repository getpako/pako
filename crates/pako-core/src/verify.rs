use std::{collections::BTreeSet, fs::File, os::unix::fs::PermissionsExt, path::Path};

use sha2::{Digest as _, Sha256};
use walkdir::WalkDir;

use crate::{
    error::IoContext,
    manifest::{Entry, PackageManifest},
    Error, Result, Sha256Digest,
};

#[derive(Debug, Clone)]
pub struct VerificationReport {
    pub files: u64,
    pub directories: u64,
    pub symlinks: u64,
    pub tree_digest: Sha256Digest,
}

/// Verify that a materialized tree exactly matches its manifest.
pub fn verify_tree(manifest: &PackageManifest, root: &Path) -> Result<VerificationReport> {
    manifest.validate()?;
    reject_undeclared_paths(manifest, root)?;

    let mut tree_hash = new_tree_hash();
    let mut report = VerificationReport {
        files: 0,
        directories: 0,
        symlinks: 0,
        tree_digest: Sha256Digest::EMPTY,
    };

    for entry in &manifest.entries {
        match entry {
            Entry::Directory { path, mode } => {
                verify_directory(root, path, *mode, &mut tree_hash)?;
                report.directories += 1;
            }
            Entry::File {
                path,
                mode,
                size,
                digest,
                ..
            } => {
                verify_file(root, path, *mode, *size, *digest, &mut tree_hash)?;
                report.files += 1;
            }
            Entry::Symlink { path, target } => {
                verify_symlink(root, path, target, &mut tree_hash)?;
                report.symlinks += 1;
            }
        }
    }

    let actual_tree_digest = Sha256Digest::from_bytes(tree_hash.finalize().into());
    if actual_tree_digest != manifest.tree_digest {
        return Err(Error::Integrity {
            path: root.to_owned(),
            expected: manifest.tree_digest.to_string(),
            actual: actual_tree_digest.to_string(),
        });
    }

    report.tree_digest = actual_tree_digest;
    Ok(report)
}

pub fn compute_tree_digest(entries: &[Entry]) -> Sha256Digest {
    let mut tree_hash = new_tree_hash();

    for entry in entries {
        match entry {
            Entry::Directory { path, mode } => {
                hash_directory(&mut tree_hash, path.as_str(), *mode);
            }
            Entry::File {
                path,
                mode,
                size,
                digest,
                ..
            } => {
                hash_file(&mut tree_hash, path.as_str(), *mode, *size, *digest);
            }
            Entry::Symlink { path, target } => {
                hash_symlink(&mut tree_hash, path.as_str(), target);
            }
        }
    }

    Sha256Digest::from_bytes(tree_hash.finalize().into())
}

fn reject_undeclared_paths(manifest: &PackageManifest, root: &Path) -> Result<()> {
    let declared: BTreeSet<_> = manifest
        .entries
        .iter()
        .map(|entry| entry.path().as_str().to_owned())
        .collect();

    for item in WalkDir::new(root).follow_links(false).min_depth(1) {
        let item = item.map_err(anyhow::Error::from)?;
        let relative = item
            .path()
            .strip_prefix(root)
            .map_err(anyhow::Error::from)?
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("non UTF-8 path"))?;

        if !declared.contains(relative) {
            return Err(Error::InvalidManifest(format!(
                "undeclared path: {relative}"
            )));
        }
    }

    Ok(())
}

fn verify_directory(
    root: &Path,
    path: &crate::path::PackagePath,
    mode: u16,
    tree_hash: &mut Sha256,
) -> Result<()> {
    let actual = path.join_to(root);
    let metadata = std::fs::symlink_metadata(&actual).at(&actual)?;

    if !metadata.is_dir() {
        return Err(Error::InvalidManifest(format!(
            "expected directory: {path}"
        )));
    }

    let actual_mode = (metadata.permissions().mode() & 0o777) as u16;
    if normalize_mode(actual_mode) != normalize_mode(mode) {
        return Err(Error::InvalidManifest(format!(
            "directory mode mismatch: {path}"
        )));
    }

    hash_directory(tree_hash, path.as_str(), mode);
    Ok(())
}

fn verify_file(
    root: &Path,
    path: &crate::path::PackagePath,
    mode: u16,
    expected_size: u64,
    expected_digest: Sha256Digest,
    tree_hash: &mut Sha256,
) -> Result<()> {
    let actual = path.join_to(root);
    let metadata = std::fs::symlink_metadata(&actual).at(&actual)?;

    if !metadata.is_file() || metadata.len() != expected_size {
        return Err(Error::InvalidManifest(format!(
            "file metadata mismatch: {path}"
        )));
    }

    let actual_mode = (metadata.permissions().mode() & 0o777) as u16;
    if normalize_mode(actual_mode) != normalize_mode(mode) {
        return Err(Error::InvalidManifest(format!(
            "file mode mismatch: {path}"
        )));
    }

    let (actual_digest, _) = Sha256Digest::calculate_reader(File::open(&actual).at(&actual)?)?;
    if actual_digest != expected_digest {
        return Err(Error::Integrity {
            path: actual,
            expected: expected_digest.to_string(),
            actual: actual_digest.to_string(),
        });
    }

    hash_file(
        tree_hash,
        path.as_str(),
        mode,
        expected_size,
        expected_digest,
    );
    Ok(())
}

fn verify_symlink(
    root: &Path,
    path: &crate::path::PackagePath,
    expected_target: &str,
    tree_hash: &mut Sha256,
) -> Result<()> {
    let actual = path.join_to(root);
    let metadata = std::fs::symlink_metadata(&actual).at(&actual)?;

    if !metadata.file_type().is_symlink() {
        return Err(Error::InvalidManifest(format!("expected symlink: {path}")));
    }

    let actual_target = std::fs::read_link(&actual).at(&actual)?;
    if actual_target.to_string_lossy() != expected_target {
        return Err(Error::InvalidManifest(format!(
            "symlink target mismatch: {path}"
        )));
    }

    hash_symlink(tree_hash, path.as_str(), expected_target);
    Ok(())
}

fn new_tree_hash() -> Sha256 {
    let mut hash = Sha256::new();
    hash.update(b"PAKO-TREE-V1\0\0\0\0");
    hash
}

fn hash_directory(hash: &mut Sha256, path: &str, mode: u16) {
    hash.update([1]);
    write_path(hash, path);
    hash.update(normalize_mode(mode).to_le_bytes());
}

fn hash_file(hash: &mut Sha256, path: &str, mode: u16, size: u64, digest: Sha256Digest) {
    hash.update([2]);
    write_path(hash, path);
    hash.update(normalize_mode(mode).to_le_bytes());
    hash.update(size.to_le_bytes());
    hash.update(digest.as_bytes());
}

fn hash_symlink(hash: &mut Sha256, path: &str, target: &str) {
    hash.update([3]);
    write_path(hash, path);
    write_path(hash, target);
}

fn write_path(hash: &mut Sha256, value: &str) {
    let length = u32::try_from(value.len()).expect("paths longer than 4 GiB are unsupported");
    hash.update(length.to_le_bytes());
    hash.update(value.as_bytes());
}

fn normalize_mode(mode: u16) -> u16 {
    mode & (0o444 | 0o111)
}
