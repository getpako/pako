use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use pako_core::{manifest::validate_package_name, path::PackagePath, Sha256Digest};
use serde::Deserialize;

/// User-maintained package build recipe.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Recipe {
    pub schema: u32,
    pub package: Package,
    pub metadata: Metadata,
    pub targets: Vec<Target>,
    #[serde(default)]
    pub transforms: Vec<Transform>,
    #[serde(default)]
    pub assertions: Vec<Assertion>,
    #[serde(default)]
    pub integrations: Integrations,
    #[serde(skip)]
    directory: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Package {
    pub name: String,
    pub version: String,
    pub release: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Metadata {
    pub display_name: String,
    pub summary: String,
    pub description: String,
    pub vendor: String,
    pub homepage: String,
    pub license: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Target {
    #[serde(rename = "target")]
    pub platform: String,
    #[serde(default)]
    pub build: Build,
    pub sources: Vec<Source>,
    #[serde(default)]
    pub transforms: Vec<Transform>,
    #[serde(default)]
    pub assertions: Vec<Assertion>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Build {
    #[serde(default)]
    pub environment: Option<String>,
    #[serde(default)]
    pub shell: Option<String>,
    #[serde(default)]
    pub network: bool,
    #[serde(default)]
    pub timeout_seconds: Option<u64>,
    #[serde(default)]
    pub scripts: Scripts,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Scripts {
    pub prepare: Option<String>,
    pub configure: Option<String>,
    pub build: Option<String>,
    pub check: Option<String>,
    pub install: Option<String>,
}

impl Scripts {
    pub(crate) fn phases(&self) -> [(&'static str, Option<&str>); 5] {
        [
            ("prepare", self.prepare.as_deref()),
            ("configure", self.configure.as_deref()),
            ("build", self.build.as_deref()),
            ("check", self.check.as_deref()),
            ("install", self.install.as_deref()),
        ]
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.phases().iter().all(|(_, script)| script.is_none())
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Source {
    pub id: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub urls: Vec<String>,
    pub hash: String,
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub strip_components: u32,
    #[serde(default)]
    pub destination: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub(crate) enum Transform {
    Remove {
        paths: Vec<String>,
        #[serde(default = "default_true")]
        required: bool,
    },
    Chmod {
        path: String,
        mode: String,
    },
    Move {
        from: String,
        to: String,
    },
    Copy {
        from: String,
        to: String,
    },
    Write {
        path: String,
        mode: String,
        content: String,
    },
    Symlink {
        path: String,
        target: String,
    },
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub(crate) enum Assertion {
    Path {
        path: String,
        kind: String,
        #[serde(default)]
        executable: bool,
    },
    Absent {
        path: String,
    },
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Integrations {
    #[serde(default)]
    pub launchers: Vec<Launcher>,
    #[serde(default)]
    pub desktop_entries: Vec<DesktopEntry>,
    #[serde(default)]
    pub icons: Vec<Icon>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Launcher {
    pub name: String,
    pub target: String,
    #[serde(default)]
    pub arguments: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DesktopEntry {
    pub id: String,
    pub name: String,
    pub exec: String,
    pub icon: String,
    pub terminal: bool,
    #[serde(default)]
    pub categories: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Icon {
    pub name: String,
    pub source: String,
    pub context: String,
    pub size: String,
}

impl Recipe {
    pub(crate) fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        let mut recipe: Self = toml::from_str(&text)?;
        let parent = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .canonicalize()?;
        recipe.directory = parent;
        Ok(recipe)
    }

    pub(crate) fn recipe_dir(&self) -> &Path {
        &self.directory
    }

    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        if self.schema != 1 {
            anyhow::bail!("unsupported recipe schema {}", self.schema);
        }

        validate_package_name(&self.package.name)?;
        if self.package.version.trim().is_empty() {
            anyhow::bail!("package version must not be empty");
        }
        if self.package.release == 0 {
            anyhow::bail!("release must be positive");
        }

        let mut targets = BTreeSet::new();
        for target in &self.targets {
            validate_target(target)?;
            if !targets.insert(target.platform.as_str()) {
                anyhow::bail!("duplicate target {}", target.platform);
            }
        }

        validate_integrations(&self.integrations)?;
        Ok(())
    }
}

fn validate_target(target: &Target) -> anyhow::Result<()> {
    if !matches!(target.platform.as_str(), "linux/x86_64" | "linux/aarch64") {
        anyhow::bail!("unsupported target {}", target.platform);
    }

    if let Some(shell) = target.build.shell.as_deref() {
        if shell != "bash" {
            anyhow::bail!("unsupported build shell {shell}; schema 1 supports bash only");
        }
    }

    if !target.build.scripts.is_empty() {
        let environment = target
            .build
            .environment
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("build scripts require an environment"))?;
        if !environment.contains("@sha256:") {
            anyhow::bail!("build environment must be pinned by OCI digest");
        }
    }

    let mut source_ids = BTreeSet::new();
    for source in &target.sources {
        validate_source(source)?;
        if !source_ids.insert(source.id.as_str()) {
            anyhow::bail!("duplicate source id {}", source.id);
        }
    }

    Ok(())
}

fn validate_source(source: &Source) -> anyhow::Result<()> {
    validate_simple_identifier(&source.id, "source id")?;
    source.hash.parse::<Sha256Digest>()?;

    if source.path.is_some() != source.urls.is_empty() {
        anyhow::bail!(
            "source {} must define exactly one of path or urls",
            source.id
        );
    }

    if source.format.is_none() {
        if let Some(destination) = &source.destination {
            PackagePath::new(destination.clone())?;
        }
    }

    Ok(())
}

fn validate_integrations(integrations: &Integrations) -> anyhow::Result<()> {
    let mut launcher_names = BTreeSet::new();
    for launcher in &integrations.launchers {
        validate_simple_identifier(&launcher.name, "launcher name")?;
        PackagePath::new(launcher.target.clone())?;
        if !launcher_names.insert(launcher.name.as_str()) {
            anyhow::bail!("duplicate launcher {}", launcher.name);
        }
    }

    for icon in &integrations.icons {
        validate_simple_identifier(&icon.name, "icon name")?;
        PackagePath::new(icon.source.clone())?;
    }

    Ok(())
}

fn validate_simple_identifier(value: &str, field: &str) -> anyhow::Result<()> {
    let valid = !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'));

    if valid {
        Ok(())
    } else {
        anyhow::bail!("invalid {field}: {value}")
    }
}
