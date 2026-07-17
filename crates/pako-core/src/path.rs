use std::{
    fmt,
    path::{Component, Path, PathBuf},
    str::FromStr,
};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{Error, Result};

/// A validated UTF-8 path relative to a package payload root.
///
/// `PackagePath` rejects absolute paths, parent traversal and ambiguous
/// separators before a path can reach extraction or installation code.
#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PackagePath(String);

impl PackagePath {
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();

        if value.is_empty() || value.contains('\0') || value.contains("//") || value.ends_with('/')
        {
            return Err(Error::InvalidPackagePath(value));
        }

        let path = Path::new(&value);
        if path.is_absolute() {
            return Err(Error::InvalidPackagePath(value));
        }

        if path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(Error::InvalidPackagePath(value));
        }

        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn join_to(&self, root: &Path) -> PathBuf {
        root.join(&self.0)
    }

    pub fn parent(&self) -> Option<&Path> {
        Path::new(&self.0).parent()
    }
}

impl fmt::Debug for PackagePath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("PackagePath").field(&self.0).finish()
    }
}

impl fmt::Display for PackagePath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for PackagePath {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self> {
        Self::new(value)
    }
}

impl Serialize for PackagePath {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for PackagePath {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// Validate a single path component controlled by package or repository metadata.
///
/// This is deliberately stricter than a generic Linux filename. Managed names
/// become lock names, state filenames, integration destinations, or directory
/// components and must therefore be portable and unambiguous.
pub fn validate_managed_name(value: &str, field: &str) -> Result<()> {
    let valid = !value.is_empty()
        && value.len() <= 128
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'+'))
        && !value.starts_with('.')
        && !value.ends_with('.');

    if valid {
        Ok(())
    } else {
        Err(Error::InvalidManifest(format!("invalid {field}: {value}")))
    }
}

/// Validate an upstream version before it is embedded in a cellar path.
pub fn validate_upstream_version(value: &str) -> Result<()> {
    validate_managed_name(value, "upstream version")
}

/// Validate the complete local version component (`<upstream>-<release>`).
pub fn validate_local_version(value: &str) -> Result<()> {
    validate_managed_name(value, "local version")
}

/// Validate a repository release channel persisted in local package state.
pub fn validate_channel(value: &str) -> Result<()> {
    validate_managed_name(value, "release channel")
}

/// Ensure a relative symlink target cannot escape the package root.
pub fn validate_symlink_target(link: &PackagePath, target: &str) -> Result<()> {
    if target.is_empty() || target.contains('\0') || Path::new(target).is_absolute() {
        return Err(Error::InvalidPackagePath(target.to_owned()));
    }

    let mut depth = link
        .parent()
        .map_or(0, |parent| parent.components().count());

    for component in Path::new(target).components() {
        match component {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir if depth > 0 => depth -= 1,
            _ => return Err(Error::InvalidPackagePath(target.to_owned())),
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{validate_channel, validate_local_version, validate_managed_name};

    #[test]
    fn managed_names_reject_path_traversal() {
        for value in ["", ".", "..", "../beta", "beta/dev", ".hidden"] {
            assert!(validate_managed_name(value, "test").is_err());
        }
    }

    #[test]
    fn versions_and_channels_accept_portable_values() {
        assert!(validate_local_version("2026.1-1").is_ok());
        assert!(validate_channel("early-access").is_ok());
    }
}
