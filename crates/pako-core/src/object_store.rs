use std::{
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
};

use sha2::{Digest as _, Sha256};
use tempfile::NamedTempFile;

use crate::{error::IoContext, Error, Result, Sha256Digest};

/// Local content-addressed store for verified raw chunks.
#[derive(Debug, Clone)]
pub struct ObjectStore {
    root: PathBuf,
}

impl ObjectStore {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
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

        std::fs::remove_file(&path).at(&path)?;
        Ok(false)
    }

    pub fn open_verified(&self, digest: Sha256Digest) -> Result<File> {
        if !self.contains(digest)? {
            return Err(Error::MissingChunk(digest.to_string()));
        }

        let path = self.path(digest);
        File::open(&path).at(&path)
    }

    /// Import one raw object using an atomic no-clobber publish operation.
    pub fn import(&self, mut reader: impl Read, expected: Sha256Digest) -> Result<PathBuf> {
        let destination = self.path(expected);
        if self.contains(expected)? {
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

        temporary.as_file().sync_all().at(temporary.path())?;

        match temporary.persist_noclobber(&destination) {
            Ok(_) => Ok(destination),
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                // Another process won the race. Validate its object before accepting it.
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
        let path = self.path(digest);
        if path.exists() {
            std::fs::remove_file(&path).at(&path)?;
        }
        Ok(())
    }

    pub fn create_temp(&self, directory: &Path) -> Result<NamedTempFile> {
        std::fs::create_dir_all(directory).at(directory)?;
        NamedTempFile::new_in(directory).at(directory)
    }
}
