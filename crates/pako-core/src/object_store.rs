use std::{
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
};

use sha2::{Digest as _, Sha256};
use tempfile::NamedTempFile;

use crate::{error::IoContext, lock::DigestLock, Error, Result, Sha256Digest};

/// Local content-addressed store for verified raw chunks.
#[derive(Debug, Clone)]
pub struct ObjectStore {
    root: PathBuf,
    lock_root: PathBuf,
}

impl ObjectStore {
    pub fn new(root: PathBuf, lock_root: PathBuf) -> Self {
        Self { root, lock_root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn path(&self, digest: Sha256Digest) -> PathBuf {
        let hex = digest.hex();
        self.root.join("sha256").join(&hex[..2]).join(&hex[2..])
    }

    /// Check whether an object exists and still matches its content address.
    ///
    /// A corrupted cache entry is deleted immediately and treated as missing.
    pub fn contains(&self, digest: Sha256Digest) -> Result<bool> {
        let path = self.path(digest);
        if !path.exists() {
            return Ok(false);
        }

        let (actual, _) = Sha256Digest::calculate_reader(File::open(&path).at(&path)?)?;
        if actual == digest {
            return Ok(true);
        }

        log::warn!("removing corrupted cached object {}", path.display());
        remove_file_if_present(&path)?;
        Ok(false)
    }

    /// Copy one cached object while verifying it in the same read pass.
    ///
    /// The supplied file hash is updated with the raw bytes so materialization
    /// can verify both each chunk and the complete file without rereading the
    /// object store.
    pub fn copy_verified(
        &self,
        digest: Sha256Digest,
        mut output: impl Write,
        file_hash: &mut Sha256,
    ) -> Result<u64> {
        let path = self.path(digest);
        if !path.exists() {
            return Err(Error::MissingChunk(digest.to_string()));
        }

        let mut input = File::open(&path).at(&path)?;
        let mut chunk_hash = Sha256::new();
        let mut total = 0_u64;
        let mut buffer = vec![0_u8; 128 * 1024];

        loop {
            let count = input.read(&mut buffer).at(&path)?;
            if count == 0 {
                break;
            }

            output.write_all(&buffer[..count]).map_err(anyhow::Error::from)?;
            chunk_hash.update(&buffer[..count]);
            file_hash.update(&buffer[..count]);
            total = total
                .checked_add(count as u64)
                .ok_or_else(|| anyhow::anyhow!("object copy size overflow"))?;
        }

        let actual = Sha256Digest::from_bytes(chunk_hash.finalize().into());
        if actual != digest {
            log::warn!("removing corrupted cached object {}", path.display());
            remove_file_if_present(&path)?;
            return Err(Error::Integrity {
                path,
                expected: digest.to_string(),
                actual: actual.to_string(),
            });
        }

        Ok(total)
    }

    /// Import one raw object using an atomic no-clobber publish operation.
    pub fn import(&self, mut reader: impl Read, expected: Sha256Digest) -> Result<PathBuf> {
        let _lock = DigestLock::acquire(&self.lock_root, expected)?;
        let destination = self.path(expected);
        if self.contains(expected)? {
            log::trace!("object {expected} is already present");
            return Ok(destination);
        }

        let parent = destination
            .parent()
            .expect("object store paths always have a parent");
        std::fs::create_dir_all(parent).at(parent)?;

        let mut temporary = NamedTempFile::new_in(parent).at(parent)?;
        let mut hash = Sha256::new();
        let mut buffer = vec![0_u8; 128 * 1024];

        loop {
            let count = reader.read(&mut buffer).map_err(anyhow::Error::from)?;
            if count == 0 {
                break;
            }

            hash.update(&buffer[..count]);
            temporary.write_all(&buffer[..count]).at(temporary.path())?;
        }

        let actual = Sha256Digest::from_bytes(hash.finalize().into());
        if actual != expected {
            return Err(Error::Integrity {
                path: destination,
                expected: expected.to_string(),
                actual: actual.to_string(),
            });
        }

        self.publish_verified_locked(temporary, expected, destination)
    }

    /// Create a temporary object beside its final content-addressed path.
    pub fn create_temp_for(&self, digest: Sha256Digest) -> Result<NamedTempFile> {
        let destination = self.path(digest);
        let parent = destination
            .parent()
            .expect("object store paths always have a parent");
        std::fs::create_dir_all(parent).at(parent)?;
        NamedTempFile::new_in(parent).at(parent)
    }

    /// Publish content which has already been verified by the pack reader.
    pub fn publish_verified(
        &self,
        temporary: NamedTempFile,
        expected: Sha256Digest,
    ) -> Result<PathBuf> {
        let _lock = DigestLock::acquire(&self.lock_root, expected)?;
        let destination = self.path(expected);
        if self.contains(expected)? {
            return Ok(destination);
        }
        self.publish_verified_locked(temporary, expected, destination)
    }

    fn publish_verified_locked(
        &self,
        temporary: NamedTempFile,
        expected: Sha256Digest,
        destination: PathBuf,
    ) -> Result<PathBuf> {
        temporary.as_file().sync_all().at(temporary.path())?;

        match temporary.persist_noclobber(&destination) {
            Ok(_) => Ok(destination),
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                if self.contains(expected)? {
                    Ok(destination)
                } else {
                    Err(Error::Integrity {
                        path: destination,
                        expected: expected.to_string(),
                        actual: "invalid object published concurrently".into(),
                    })
                }
            }
            Err(error) => Err(Error::Io {
                path: destination,
                source: error.error,
            }),
        }
    }

    pub fn remove(&self, digest: Sha256Digest) -> Result<()> {
        remove_file_if_present(&self.path(digest))
    }
}

fn remove_file_if_present(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(Error::Io {
            path: path.to_owned(),
            source,
        }),
    }
}
