use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{
    path::{validate_managed_name, validate_symlink_target, validate_upstream_version, PackagePath},
    Error, Result, Sha256Digest,
};

pub const PACKAGE_MANIFEST_MEDIA_TYPE: &str = "application/vnd.pako.package-manifest.v1+json";
pub const PACK_INDEX_MEDIA_TYPE: &str = "application/vnd.pako.pack-index.v1+json";

/// Complete, immutable description of one package release for one target.
///
/// The manifest is the source of truth for installation and verification. It
/// deliberately contains no registry URLs or mutable tags.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PackageManifest {
    pub schema_version: u32,
    pub media_type: String,
    pub package: String,
    pub upstream_version: String,
    pub release: u32,
    pub target: String,
    pub metadata: PackageMetadata,
    pub chunking: ChunkingProfile,
    pub tree_digest: Sha256Digest,
    pub entries: Vec<Entry>,
    #[serde(default)]
    pub integrations: Integrations,
    pub policies: Policies,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PackageMetadata {
    pub display_name: String,
    pub summary: String,
    pub description: String,
    pub vendor: String,
    pub homepage: String,
    pub license: String,
}

/// Frozen content-defined chunking parameters for schema version 1.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChunkingProfile {
    pub profile: String,
    pub algorithm: String,
    pub small_file_threshold: u32,
    pub minimum: u32,
    pub average: u32,
    pub maximum: u32,
}

impl Default for ChunkingProfile {
    fn default() -> Self {
        Self {
            profile: "pako-fastcdc-v1".into(),
            algorithm: "fastcdc-v2020".into(),
            small_file_threshold: 256 * 1024,
            minimum: 256 * 1024,
            average: 1024 * 1024,
            maximum: 4 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Entry {
    Directory {
        path: PackagePath,
        mode: u16,
    },
    File {
        path: PackagePath,
        mode: u16,
        size: u64,
        digest: Sha256Digest,
        chunks: Vec<ChunkRef>,
    },
    Symlink {
        path: PackagePath,
        target: String,
    },
}

impl Entry {
    pub fn path(&self) -> &PackagePath {
        match self {
            Self::Directory { path, .. } | Self::File { path, .. } | Self::Symlink { path, .. } => {
                path
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChunkRef {
    pub digest: Sha256Digest,
    pub size: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Integrations {
    #[serde(default)]
    pub launchers: Vec<Launcher>,
    #[serde(default)]
    pub desktop_entries: Vec<DesktopEntry>,
    #[serde(default)]
    pub icons: Vec<Icon>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Launcher {
    pub name: String,
    pub target: PackagePath,
    #[serde(default)]
    pub arguments: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DesktopEntry {
    pub id: String,
    pub name: String,
    pub exec: String,
    pub icon: String,
    pub terminal: bool,
    #[serde(default)]
    pub categories: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Icon {
    pub name: String,
    pub source: PackagePath,
    pub context: String,
    pub size: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Policies {
    pub payload_mutation: String,
    pub self_update: String,
    pub user_data: String,
}

impl PackageManifest {
    /// Validate all invariants required by the installer.
    pub fn validate(&self) -> Result<()> {
        self.validate_header()?;
        self.validate_entries()?;
        self.validate_integrations()?;
        Ok(())
    }

    fn validate_header(&self) -> Result<()> {
        if self.schema_version != 1 {
            return Err(Error::UnsupportedSchema(self.schema_version));
        }

        if self.media_type != PACKAGE_MANIFEST_MEDIA_TYPE {
            return Err(Error::InvalidManifest("invalid media type".into()));
        }

        validate_package_name(&self.package)?;
        validate_upstream_version(&self.upstream_version)?;

        if self.release == 0 {
            return Err(Error::InvalidManifest("release must be positive".into()));
        }

        if self.chunking != ChunkingProfile::default() {
            return Err(Error::InvalidManifest(
                "unsupported chunking profile".into(),
            ));
        }

        if !matches!(self.target.as_str(), "linux/x86_64" | "linux/aarch64") {
            return Err(Error::InvalidManifest(format!(
                "unsupported target {}",
                self.target
            )));
        }

        Ok(())
    }

    fn validate_entries(&self) -> Result<()> {
        let mut previous: Option<&PackagePath> = None;
        let mut paths = BTreeSet::new();

        for entry in &self.entries {
            if previous.is_some_and(|candidate| candidate >= entry.path()) {
                return Err(Error::InvalidManifest(
                    "entries are not strictly sorted".into(),
                ));
            }
            previous = Some(entry.path());

            if !paths.insert(entry.path().clone()) {
                return Err(Error::InvalidManifest("duplicate path".into()));
            }

            validate_entry_mode(entry)?;
            self.validate_entry_content(entry)?;
        }

        self.validate_parent_directories(&paths)
    }

    fn validate_entry_content(&self, entry: &Entry) -> Result<()> {
        match entry {
            Entry::File {
                size,
                digest,
                chunks,
                ..
            } => {
                let chunk_total = chunks
                    .iter()
                    .try_fold(0_u64, |sum, chunk| sum.checked_add(u64::from(chunk.size)));
                let chunk_total = chunk_total
                    .ok_or_else(|| Error::InvalidManifest("chunk size overflow".into()))?;

                if chunk_total != *size {
                    return Err(Error::InvalidManifest(format!(
                        "chunk sizes do not match file size for {}",
                        entry.path()
                    )));
                }

                if *size == 0 && (!chunks.is_empty() || *digest != Sha256Digest::EMPTY) {
                    return Err(Error::InvalidManifest("invalid empty file".into()));
                }

                if chunks
                    .iter()
                    .any(|chunk| chunk.size > self.chunking.maximum)
                {
                    return Err(Error::InvalidManifest(
                        "chunk exceeds profile maximum".into(),
                    ));
                }
            }
            Entry::Symlink { path, target } => {
                validate_symlink_target(path, target)?;
            }
            Entry::Directory { .. } => {}
        }

        Ok(())
    }

    fn validate_parent_directories(&self, paths: &BTreeSet<PackagePath>) -> Result<()> {
        for path in paths {
            let mut parent = path.parent().map(std::path::Path::to_path_buf);

            while let Some(candidate) = parent {
                if candidate.as_os_str().is_empty() {
                    break;
                }

                let candidate_text = candidate.to_string_lossy();
                if let Ok(index) = self
                    .entries
                    .binary_search_by(|entry| entry.path().as_str().cmp(candidate_text.as_ref()))
                {
                    if !matches!(self.entries[index], Entry::Directory { .. }) {
                        return Err(Error::InvalidManifest(format!(
                            "non-directory ancestor: {candidate_text}"
                        )));
                    }
                }

                parent = candidate.parent().map(std::path::Path::to_path_buf);
            }
        }

        Ok(())
    }

    fn validate_integrations(&self) -> Result<()> {
        let entry_paths: BTreeSet<_> = self.entries.iter().map(Entry::path).collect();

        for launcher in &self.integrations.launchers {
            validate_managed_name(&launcher.name, "launcher name")?;
            for argument in &launcher.arguments {
                validate_single_line(argument, "launcher argument")?;
            }
            if !entry_paths.contains(&launcher.target) {
                return Err(Error::InvalidManifest(format!(
                    "launcher target missing: {}",
                    launcher.target
                )));
            }
        }

        for desktop_entry in &self.integrations.desktop_entries {
            validate_managed_name(&desktop_entry.id, "desktop entry id")?;
            validate_single_line(&desktop_entry.name, "desktop entry name")?;
            validate_single_line(&desktop_entry.exec, "desktop entry command")?;
            validate_managed_name(&desktop_entry.icon, "desktop entry icon")?;
            for category in &desktop_entry.categories {
                validate_managed_name(category, "desktop entry category")?;
            }
        }

        for icon in &self.integrations.icons {
            validate_managed_name(&icon.name, "icon name")?;
            validate_managed_name(&icon.context, "icon context")?;
            validate_managed_name(&icon.size, "icon size")?;
            if !entry_paths.contains(&icon.source) {
                return Err(Error::InvalidManifest(format!(
                    "icon source missing: {}",
                    icon.source
                )));
            }
        }

        Ok(())
    }
}

fn validate_entry_mode(entry: &Entry) -> Result<()> {
    let mode = match entry {
        Entry::Directory { mode, .. } | Entry::File { mode, .. } => Some(*mode),
        Entry::Symlink { .. } => None,
    };

    if mode.is_some_and(|value| value & !0o777 != 0) {
        return Err(Error::InvalidManifest("forbidden mode bits".into()));
    }

    Ok(())
}

pub fn validate_package_name(name: &str) -> Result<()> {
    let valid = !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--");

    if valid {
        Ok(())
    } else {
        Err(Error::InvalidPackageName(name.into()))
    }
}

fn validate_single_line(value: &str, field: &str) -> Result<()> {
    if value.chars().any(|character| matches!(character, '\0' | '\n' | '\r')) {
        Err(Error::InvalidManifest(format!("invalid {field}")))
    } else {
        Ok(())
    }
}

/// Maps every chunk digest to its immutable pack and byte range.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PackIndex {
    pub schema: String,
    pub package_manifest_digest: Sha256Digest,
    pub packs: BTreeMap<Sha256Digest, PackDescriptor>,
    pub chunks: BTreeMap<Sha256Digest, ChunkLocation>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackDescriptor {
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChunkLocation {
    pub pack: Sha256Digest,
    pub offset: u64,
    pub stored_size: u64,
    pub raw_size: u64,
    pub compression: Compression,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Compression {
    Raw,
    Zstd,
}

impl PackIndex {
    pub fn validate_against(&self, manifest: &PackageManifest) -> Result<()> {
        if self.schema != "pako.pack-index.v1" {
            return Err(Error::InvalidManifest("invalid pack index schema".into()));
        }

        let required = required_chunks(manifest);
        if required.len() != self.chunks.len() {
            return Err(Error::InvalidManifest(
                "pack index chunk set differs from manifest".into(),
            ));
        }

        for (digest, expected_size) in required {
            let location = self
                .chunks
                .get(&digest)
                .ok_or_else(|| Error::MissingChunk(digest.to_string()))?;

            if location.raw_size != u64::from(expected_size) {
                return Err(Error::InvalidManifest(
                    "pack index raw size mismatch".into(),
                ));
            }

            let pack = self
                .packs
                .get(&location.pack)
                .ok_or_else(|| Error::InvalidManifest("chunk references unknown pack".into()))?;

            let end = location
                .offset
                .checked_add(location.stored_size)
                .ok_or_else(|| Error::InvalidManifest("pack range overflow".into()))?;

            if end > pack.size {
                return Err(Error::InvalidManifest(
                    "chunk range exceeds pack size".into(),
                ));
            }
        }

        Ok(())
    }
}

fn required_chunks(manifest: &PackageManifest) -> BTreeMap<Sha256Digest, u32> {
    manifest
        .entries
        .iter()
        .filter_map(|entry| match entry {
            Entry::File { chunks, .. } => Some(chunks),
            Entry::Directory { .. } | Entry::Symlink { .. } => None,
        })
        .flatten()
        .map(|chunk| (chunk.digest, chunk.size))
        .collect()
}
