use std::{collections::BTreeSet, str::FromStr};

use serde::{Deserialize, Serialize};
use url::Url;

use crate::{
    path::{
        validate_managed_name, validate_symlink_target, validate_upstream_version, PackagePath,
    },
    Error, Result, Sha256Digest,
};

pub const PACKAGE_MANIFEST_MEDIA_TYPE: &str = "application/vnd.pako.package-manifest.v1+json";
pub const ARCHIVE_MEDIA_TYPE: &str = "application/vnd.pako.archive.v1";

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
    pub artifact: Artifact,
    pub tree_digest: Sha256Digest,
    pub entries: Vec<Entry>,
    #[serde(default)]
    pub transforms: Vec<InstallTransform>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Artifact {
    ExternalArchive {
        urls: Vec<Url>,
        digest: Sha256Digest,
        size: u64,
        archive: ArchiveDescriptor,
    },
    TufArchive {
        target: String,
        digest: Sha256Digest,
        size: u64,
        archive: ArchiveDescriptor,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ArchiveDescriptor {
    pub format: ArchiveFormat,
    #[serde(default)]
    pub strip_components: u32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ArchiveFormat {
    #[serde(rename = "tar")]
    Tar,
    #[serde(rename = "tar.gz")]
    TarGz,
    #[serde(rename = "tar.xz")]
    TarXz,
    #[serde(rename = "tar.zst")]
    TarZst,
    Zip,
}

impl ArchiveFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tar => "tar",
            Self::TarGz => "tar.gz",
            Self::TarXz => "tar.xz",
            Self::TarZst => "tar.zst",
            Self::Zip => "zip",
        }
    }
}

impl FromStr for ArchiveFormat {
    type Err = anyhow::Error;
    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value {
            "tar" => Ok(Self::Tar),
            "tar.gz" => Ok(Self::TarGz),
            "tar.xz" => Ok(Self::TarXz),
            "tar.zst" => Ok(Self::TarZst),
            "zip" => Ok(Self::Zip),
            _ => Err(anyhow::anyhow!("unsupported archive format {value}")),
        }
    }
}

impl Artifact {
    pub fn digest(&self) -> Sha256Digest {
        match self {
            Self::ExternalArchive { digest, .. } | Self::TufArchive { digest, .. } => *digest,
        }
    }
    pub fn size(&self) -> u64 {
        match self {
            Self::ExternalArchive { size, .. } | Self::TufArchive { size, .. } => *size,
        }
    }
    pub fn archive(&self) -> &ArchiveDescriptor {
        match self {
            Self::ExternalArchive { archive, .. } | Self::TufArchive { archive, .. } => archive,
        }
    }
}

impl ArchiveDescriptor {
    pub fn validate(&self) -> Result<()> {
        Ok(())
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
    },
    Symlink {
        path: PackagePath,
        target: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum InstallTransform {
    Remove {
        paths: Vec<PackagePath>,
        #[serde(default = "default_true")]
        required: bool,
    },
    Chmod {
        path: PackagePath,
        mode: u16,
    },
    Move {
        from: PackagePath,
        to: PackagePath,
    },
    Copy {
        from: PackagePath,
        to: PackagePath,
    },
    Write {
        path: PackagePath,
        mode: u16,
        content: String,
    },
    Symlink {
        path: PackagePath,
        target: String,
    },
}
const fn default_true() -> bool {
    true
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

        if self.artifact.size() == 0 {
            return Err(Error::InvalidManifest(
                "artifact size must be positive".into(),
            ));
        }
        match &self.artifact {
            Artifact::ExternalArchive { urls, .. } => {
                if urls.is_empty() {
                    return Err(Error::InvalidManifest(
                        "artifact requires at least one URL".into(),
                    ));
                }
                for url in urls {
                    if url.scheme() != "https"
                        && !(url.scheme() == "http"
                            && url.host_str().is_some_and(|host| {
                                matches!(host, "localhost" | "127.0.0.1" | "::1")
                            }))
                    {
                        return Err(Error::InvalidManifest(format!(
                            "artifact URL must use HTTPS: {url}"
                        )));
                    }
                }
            }
            Artifact::TufArchive { target, .. } => {
                if target.is_empty()
                    || target.starts_with('/')
                    || target
                        .split('/')
                        .any(|part| part.is_empty() || part == "." || part == "..")
                {
                    return Err(Error::InvalidManifest("unsafe TUF artifact target".into()));
                }
            }
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
            Self::validate_entry_content(entry)?;
        }

        self.validate_parent_directories(&paths)
    }

    fn validate_entry_content(entry: &Entry) -> Result<()> {
        match entry {
            Entry::File { size, digest, .. } => {
                if *size == 0 && *digest != Sha256Digest::EMPTY {
                    return Err(Error::InvalidManifest("invalid empty file".into()));
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
    if value
        .chars()
        .any(|character| matches!(character, '\0' | '\n' | '\r'))
    {
        Err(Error::InvalidManifest(format!("invalid {field}")))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base(artifact: Artifact) -> PackageManifest {
        PackageManifest {
            schema_version: 1,
            media_type: PACKAGE_MANIFEST_MEDIA_TYPE.into(),
            package: "demo".into(),
            upstream_version: "1.0".into(),
            release: 1,
            target: "linux/x86_64".into(),
            metadata: PackageMetadata {
                display_name: "Demo".into(),
                summary: "Demo".into(),
                description: "Demo".into(),
                vendor: "Demo".into(),
                homepage: "https://example.invalid".into(),
                license: "MIT".into(),
            },
            artifact,
            tree_digest: Sha256Digest::EMPTY,
            entries: vec![],
            transforms: vec![],
            integrations: Integrations::default(),
            policies: Policies {
                payload_mutation: "deny".into(),
                self_update: "external".into(),
                user_data: "external".into(),
            },
        }
    }

    #[test]
    fn validates_external_archive_and_rejects_insecure_url() {
        let artifact = Artifact::ExternalArchive {
            urls: vec!["https://example.invalid/demo.tar.gz".parse().unwrap()],
            digest: Sha256Digest::EMPTY,
            size: 1,
            archive: ArchiveDescriptor {
                format: ArchiveFormat::TarGz,
                strip_components: 1,
            },
        };
        assert!(base(artifact).validate().is_ok());
        let artifact = Artifact::ExternalArchive {
            urls: vec!["http://example.invalid/demo.tar.gz".parse().unwrap()],
            digest: Sha256Digest::EMPTY,
            size: 1,
            archive: ArchiveDescriptor {
                format: ArchiveFormat::TarGz,
                strip_components: 0,
            },
        };
        assert!(base(artifact).validate().is_err());
    }

    #[test]
    fn validates_tuf_target_and_rejects_traversal() {
        let artifact = Artifact::TufArchive {
            target: "artifacts/demo/a.tar.zst".into(),
            digest: Sha256Digest::EMPTY,
            size: 1,
            archive: ArchiveDescriptor {
                format: ArchiveFormat::TarZst,
                strip_components: 0,
            },
        };
        assert!(base(artifact).validate().is_ok());
        let artifact = Artifact::TufArchive {
            target: "../outside.tar.zst".into(),
            digest: Sha256Digest::EMPTY,
            size: 1,
            archive: ArchiveDescriptor {
                format: ArchiveFormat::TarZst,
                strip_components: 0,
            },
        };
        assert!(base(artifact).validate().is_err());
    }
}
