use std::{
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
};

use fs2::FileExt;

use crate::{error::IoContext, Error, Result, Sha256Digest};

/// An exclusive advisory lock associated with one content digest.
///
/// Digest locks prevent concurrent Pako processes from publishing the same
/// immutable object or pack through a shared temporary path.
#[derive(Debug)]
pub struct DigestLock {
    file: File,
    path: PathBuf,
}

impl DigestLock {
    pub fn acquire(root: &Path, digest: Sha256Digest) -> Result<Self> {
        std::fs::create_dir_all(root).at(root)?;
        let path = root.join(format!("{}.lock", digest.hex()));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .at(&path)?;

        match file.try_lock_exclusive() {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                log::info!("waiting for digest lock {}", path.display());
                file.lock_exclusive().at(&path)?;
            }
            Err(source) => {
                return Err(Error::Io {
                    path: path.clone(),
                    source,
                });
            }
        }

        log::trace!("acquired digest lock {}", path.display());
        Ok(Self { file, path })
    }
}

impl Drop for DigestLock {
    fn drop(&mut self) {
        if let Err(error) = FileExt::unlock(&self.file) {
            log::warn!(
                "failed to release digest lock {}: {error}",
                self.path.display()
            );
        }
    }
}
