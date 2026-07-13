use std::{fmt, str::FromStr};

use pako_core::Sha256Digest;

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct OciReference {
    pub registry: String,
    pub repository: String,
    pub reference: Reference,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum Reference {
    Tag(String),
    Digest(Sha256Digest),
}

impl FromStr for OciReference {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let value = value.strip_prefix("oci://").unwrap_or(value);
        let (name, digest) = match value.rsplit_once('@') {
            Some((name, digest)) => (name, Some(digest.parse()?)),
            None => (value, None),
        };

        let slash = name
            .find('/')
            .ok_or_else(|| anyhow::anyhow!("OCI reference requires registry/repository"))?;
        let registry = &name[..slash];
        let repository_and_tag = &name[slash + 1..];

        let (repository, reference) = if let Some(digest) = digest {
            (repository_and_tag, Reference::Digest(digest))
        } else if let Some((repository, tag)) = repository_and_tag.rsplit_once(':') {
            (repository, Reference::Tag(tag.to_owned()))
        } else {
            (repository_and_tag, Reference::Tag("latest".into()))
        };

        if registry.is_empty() || repository.is_empty() {
            anyhow::bail!("invalid OCI reference");
        }

        Ok(Self {
            registry: registry.into(),
            repository: repository.into(),
            reference,
        })
    }
}

impl fmt::Display for OciReference {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}/{}", self.registry, self.repository)?;
        match &self.reference {
            Reference::Tag(tag) => write!(formatter, ":{tag}"),
            Reference::Digest(digest) => write!(formatter, "@{digest}"),
        }
    }
}

impl OciReference {
    #[must_use]
    pub fn with_digest(&self, digest: Sha256Digest) -> Self {
        Self {
            registry: self.registry.clone(),
            repository: self.repository.clone(),
            reference: Reference::Digest(digest),
        }
    }

    pub fn reference_string(&self) -> String {
        match &self.reference {
            Reference::Tag(tag) => tag.clone(),
            Reference::Digest(digest) => digest.to_string(),
        }
    }
}
