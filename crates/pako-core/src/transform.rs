//! Safe application of declarative package installation transforms.

use std::{fs, os::unix::fs::PermissionsExt, path::Path};

use crate::{manifest::InstallTransform, path::validate_symlink_target};

/// Apply validated transforms to a staging tree.
pub fn apply(transforms: &[InstallTransform], root: &Path) -> anyhow::Result<()> {
    for transform in transforms {
        match transform {
            InstallTransform::Remove { paths, required } => {
                for path in paths {
                    let target = path.join_to(root);
                    if target.symlink_metadata().is_ok() {
                        remove_path(&target)?;
                    } else if *required {
                        anyhow::bail!("required transform path does not exist: {path}");
                    }
                }
            }
            InstallTransform::Chmod { path, mode } => {
                let target = path.join_to(root);
                let metadata = target.symlink_metadata()?;
                if metadata.file_type().is_symlink() {
                    anyhow::bail!("cannot chmod symlink: {path}");
                }
                fs::set_permissions(target, fs::Permissions::from_mode(u32::from(*mode)))?;
            }
            InstallTransform::Move { from, to } => {
                let source = from.join_to(root);
                let destination = to.join_to(root);
                ensure_safe_parent(root, &source)?;
                ensure_safe_parent(root, &destination)?;
                if source.symlink_metadata().is_err() {
                    anyhow::bail!("move source does not exist: {from}");
                }
                if destination.symlink_metadata().is_ok() {
                    anyhow::bail!("move destination already exists: {to}");
                }
                create_parent(&destination)?;
                fs::rename(source, destination)?;
            }
            InstallTransform::Copy { from, to } => {
                let source = from.join_to(root);
                let destination = to.join_to(root);
                ensure_safe_parent(root, &source)?;
                ensure_safe_parent(root, &destination)?;
                if source.symlink_metadata().is_err() {
                    anyhow::bail!("copy source does not exist: {from}");
                }
                if destination.symlink_metadata().is_ok() {
                    anyhow::bail!("copy destination already exists: {to}");
                }
                create_parent(&destination)?;
                copy_path(&source, &destination)?;
            }
            InstallTransform::Write {
                path,
                mode,
                content,
            } => {
                let destination = path.join_to(root);
                ensure_safe_parent(root, &destination)?;
                if destination.symlink_metadata().is_ok() {
                    anyhow::bail!("write destination already exists: {path}");
                }
                create_parent(&destination)?;
                fs::write(&destination, content)?;
                fs::set_permissions(destination, fs::Permissions::from_mode(u32::from(*mode)))?;
            }
            InstallTransform::Symlink { path, target } => {
                validate_symlink_target(path, target)?;
                let destination = path.join_to(root);
                ensure_safe_parent(root, &destination)?;
                if destination.symlink_metadata().is_ok() {
                    anyhow::bail!("symlink destination already exists: {path}");
                }
                create_parent(&destination)?;
                std::os::unix::fs::symlink(target, destination)?;
            }
        }
    }
    Ok(())
}

fn create_parent(path: &Path) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}

fn ensure_safe_parent(root: &Path, path: &Path) -> anyhow::Result<()> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| anyhow::anyhow!("transform escaped staging"))?;
    let mut current = root.to_owned();
    for component in relative
        .components()
        .take(relative.components().count().saturating_sub(1))
    {
        current.push(component);
        if current
            .symlink_metadata()
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            anyhow::bail!("transform path traverses a symlink: {}", current.display());
        }
    }
    Ok(())
}

fn remove_path(path: &Path) -> anyhow::Result<()> {
    let metadata = path.symlink_metadata()?;
    if metadata.is_dir() {
        fs::remove_dir_all(path)?;
    } else {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn copy_path(source: &Path, destination: &Path) -> anyhow::Result<()> {
    let metadata = source.symlink_metadata()?;
    if metadata.file_type().is_symlink() {
        std::os::unix::fs::symlink(fs::read_link(source)?, destination)?;
    } else if metadata.is_dir() {
        fs::create_dir(destination)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            copy_path(&entry.path(), &destination.join(entry.file_name()))?;
        }
    } else {
        fs::copy(source, destination)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::path::PackagePath;
    use tempfile::tempdir;

    #[test]
    fn copy_preserves_directories_and_symlinks() {
        let root = tempdir().unwrap();
        fs::create_dir_all(root.path().join("source/bin")).unwrap();
        fs::write(root.path().join("source/bin/app"), b"app").unwrap();
        std::os::unix::fs::symlink("bin/app", root.path().join("source/current")).unwrap();

        let transforms = vec![InstallTransform::Copy {
            from: PackagePath::new("source").unwrap(),
            to: PackagePath::new("copy").unwrap(),
        }];
        apply(&transforms, root.path()).unwrap();

        assert_eq!(fs::read(root.path().join("copy/bin/app")).unwrap(), b"app");
        assert_eq!(
            fs::read_link(root.path().join("copy/current")).unwrap(),
            Path::new("bin/app")
        );
    }

    #[test]
    fn transforms_reject_overwrite_and_symlink_ancestor_escape() {
        let root = tempdir().unwrap();
        fs::write(root.path().join("source"), b"source").unwrap();
        fs::write(root.path().join("destination"), b"existing").unwrap();
        let overwrite = vec![InstallTransform::Copy {
            from: PackagePath::new("source").unwrap(),
            to: PackagePath::new("destination").unwrap(),
        }];
        assert!(apply(&overwrite, root.path()).is_err());

        fs::create_dir(root.path().join("link")).unwrap();
        fs::remove_dir(root.path().join("link")).unwrap();
        let outside = tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("link")).unwrap();
        let write = vec![InstallTransform::Write {
            path: PackagePath::new("link/file").unwrap(),
            mode: 0o644,
            content: "no escape".into(),
        }];
        assert!(apply(&write, root.path()).is_err());
        assert!(!outside.path().join("file").exists());
    }
}
