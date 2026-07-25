//! Permission changes for immutable installed package trees.

use std::{os::unix::fs::PermissionsExt, path::Path};

use walkdir::WalkDir;

use crate::{error::IoContext, Result};

/// Remove every write bit from a completed package tree.
///
/// Symlinks are intentionally left unchanged: changing their permissions would
/// affect their targets on Linux. Entries are processed after their children so
/// a directory does not become inaccessible while it is still being traversed.
pub(crate) fn make_immutable(root: &Path) -> Result<()> {
    for entry in WalkDir::new(root).follow_links(false).contents_first(true) {
        set_read_only(&entry.map_err(anyhow::Error::from)?)?;
    }
    Ok(())
}

/// Remove write bits from every entry below a staging root, but retain write
/// access to that root until its atomic rename into the versioned cellar.
pub(crate) fn make_contents_immutable(root: &Path) -> Result<()> {
    for entry in WalkDir::new(root)
        .follow_links(false)
        .min_depth(1)
        .contents_first(true)
    {
        set_read_only(&entry.map_err(anyhow::Error::from)?)?;
    }
    Ok(())
}

fn set_read_only(entry: &walkdir::DirEntry) -> Result<()> {
    if entry.file_type().is_symlink() {
        return Ok(());
    }

    let path = entry.path();
    let metadata = std::fs::symlink_metadata(path).at(path)?;
    let mode = metadata.permissions().mode();
    if mode & 0o222 != 0 {
        let mut permissions = metadata.permissions();
        permissions.set_mode(mode & !0o222);
        std::fs::set_permissions(path, permissions).at(path)?;
    }
    Ok(())
}

/// Temporarily make directories traversable and writable before deletion.
///
/// Installed trees are read-only by design. Removing a version still needs to
/// delete its children, which POSIX permits only when each parent directory is
/// writable and searchable by its owner.
pub(crate) fn make_removable(root: &Path) -> Result<()> {
    for entry in WalkDir::new(root).follow_links(false) {
        let entry = entry.map_err(anyhow::Error::from)?;
        if !entry.file_type().is_dir() {
            continue;
        }

        let path = entry.path();
        let metadata = std::fs::symlink_metadata(path).at(path)?;
        let mode = metadata.permissions().mode();
        if mode & 0o700 != 0o700 {
            let mut permissions = metadata.permissions();
            permissions.set_mode(mode | 0o700);
            std::fs::set_permissions(path, permissions).at(path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::{make_immutable, make_removable};

    #[test]
    fn immutable_tree_has_no_write_bits_and_can_be_removed() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("package");
        let directory = root.join("bin");
        let file = directory.join("example");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(&file, b"#!/bin/sh\n").unwrap();

        make_immutable(&root).unwrap();

        for path in [&root, &directory, &file] {
            let mode = std::fs::symlink_metadata(path)
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o222, 0, "{} remains writable", path.display());
        }

        make_removable(&root).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}
