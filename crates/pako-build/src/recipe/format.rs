//! Human-friendly `recipe.toml` schema and parser.
//!
//! The public format is deliberately smaller than the normalized builder model.
//! This module deserializes the concise representation and delegates expansion of
//! defaults, architecture aliases, sources, transforms, and integrations.

mod normalize;

#[cfg(test)]
mod tests;

use std::{collections::BTreeMap, path::Path};

use serde::Deserialize;

use super::{Assertion, Recipe, Transform};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRecipe {
    schema: u32,
    name: String,
    version: String,
    #[serde(default = "default_release")]
    release: u32,
    #[serde(default)]
    display_name: Option<String>,
    summary: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    vendor: Option<String>,
    #[serde(default)]
    homepage: Option<String>,
    license: String,
    #[serde(default)]
    source: BTreeMap<String, OneOrMany<RawSource>>,
    #[serde(default)]
    build: BTreeMap<String, RawBuild>,
    #[serde(default)]
    remove: Vec<String>,
    #[serde(default)]
    executables: Vec<String>,
    #[serde(default, rename = "move")]
    moves: BTreeMap<String, String>,
    #[serde(default)]
    copy: BTreeMap<String, String>,
    #[serde(default)]
    permissions: BTreeMap<String, String>,
    #[serde(default)]
    symlinks: BTreeMap<String, String>,
    #[serde(default)]
    commands: BTreeMap<String, RawCommand>,
    #[serde(default)]
    desktop: Option<OneOrMany<RawDesktop>>,
    #[serde(default)]
    assert_absent: Vec<String>,
    #[serde(default, rename = "transform")]
    transforms: Vec<Transform>,
    #[serde(default, rename = "assertion")]
    assertions: Vec<Assertion>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum OneOrMany<T> {
    One(T),
    Many(Vec<T>),
}

impl<T> OneOrMany<T> {
    fn into_vec(self) -> Vec<T> {
        match self {
            Self::One(value) => vec![value],
            Self::Many(values) => values,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSource {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    mirrors: Vec<String>,
    #[serde(default)]
    sha256: Option<String>,
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    strip: u32,
    #[serde(default, rename = "to")]
    destination: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBuild {
    image: String,
    #[serde(default)]
    script: Option<String>,
    #[serde(default)]
    steps: RawBuildSteps,
    #[serde(default)]
    network: bool,
    #[serde(default)]
    timeout: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBuildSteps {
    #[serde(default)]
    prepare: Option<String>,
    #[serde(default)]
    configure: Option<String>,
    #[serde(default)]
    build: Option<String>,
    #[serde(default)]
    check: Option<String>,
    #[serde(default)]
    install: Option<String>,
}

impl RawBuildSteps {
    fn is_empty(&self) -> bool {
        self.prepare.is_none()
            && self.configure.is_none()
            && self.build.is_none()
            && self.check.is_none()
            && self.install.is_none()
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawCommand {
    Target(String),
    Detailed(RawCommandDetails),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCommandDetails {
    target: String,
    #[serde(default)]
    arguments: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDesktop {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    command: String,
    #[serde(default)]
    arguments: Option<String>,
    #[serde(default)]
    icon: Option<String>,
    #[serde(default)]
    icon_name: Option<String>,
    #[serde(default)]
    terminal: bool,
    #[serde(default)]
    categories: Vec<String>,
}

pub(super) fn load(path: &Path) -> anyhow::Result<Recipe> {
    let text = std::fs::read_to_string(path)?;
    let raw: RawRecipe = toml::from_str(&text)?;
    if raw.schema != 1 {
        anyhow::bail!("unsupported recipe schema {}", raw.schema);
    }
    let directory = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .canonicalize()?;

    normalize::recipe(raw, directory)
}

const fn default_release() -> u32 {
    1
}
