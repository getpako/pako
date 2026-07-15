//! Expansion of the concise recipe format into the complete builder model.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    path::{Path, PathBuf},
};

use pako_core::Sha256Digest;

use super::{OneOrMany, RawBuild, RawCommand, RawDesktop, RawRecipe, RawSource};
use crate::recipe::{
    Assertion, Build, DesktopEntry, Icon, Integrations, Launcher, Metadata, Package, Recipe,
    Scripts, Source, Target, Transform,
};

pub(super) fn recipe(raw: RawRecipe, directory: PathBuf) -> anyhow::Result<Recipe> {
    let RawRecipe {
        schema,
        name,
        version,
        release,
        display_name,
        summary,
        description,
        vendor,
        homepage,
        license,
        source,
        build,
        remove,
        executables,
        moves,
        copy,
        permissions,
        symlinks,
        commands,
        desktop,
        assert_absent,
        transforms: explicit_transforms,
        assertions: explicit_assertions,
    } = raw;

    let display_name = display_name.unwrap_or_else(|| name.clone());
    let mut transforms =
        normalize_transforms(remove, &executables, moves, copy, permissions, symlinks);
    transforms.extend(explicit_transforms);

    let (integrations, integration_assertions) =
        normalize_integrations(&name, &display_name, commands, desktop)?;

    let mut assertions = executables
        .into_iter()
        .map(executable_assertion)
        .collect::<Vec<_>>();
    assertions.extend(
        assert_absent
            .into_iter()
            .map(|path| Assertion::Absent { path }),
    );
    assertions.extend(integration_assertions);
    assertions.extend(explicit_assertions);

    Ok(Recipe {
        schema,
        package: Package {
            name,
            version,
            release,
        },
        metadata: Metadata {
            display_name,
            summary: summary.clone(),
            description: description.unwrap_or(summary),
            vendor: vendor.unwrap_or_default(),
            homepage: homepage.unwrap_or_default(),
            license,
        },
        targets: normalize_targets(source, build, &directory)?,
        transforms,
        assertions,
        integrations,
        directory,
    })
}

fn normalize_targets(
    source: BTreeMap<String, OneOrMany<RawSource>>,
    build: BTreeMap<String, RawBuild>,
    directory: &Path,
) -> anyhow::Result<Vec<Target>> {
    let mut source = normalize_target_map(source, "source")?;
    let mut build = normalize_target_map(build, "build")?;
    let target_names = source
        .keys()
        .chain(build.keys())
        .cloned()
        .collect::<BTreeSet<_>>();

    let mut targets = Vec::with_capacity(target_names.len());
    for platform in target_names {
        let raw_sources = source
            .remove(&platform)
            .map_or_else(Vec::new, OneOrMany::into_vec);
        let sources = raw_sources
            .into_iter()
            .map(|raw| normalize_source(raw, directory))
            .collect::<anyhow::Result<Vec<_>>>()?;
        let build = build
            .remove(&platform)
            .map(|raw| normalize_build(raw, directory))
            .transpose()?
            .unwrap_or_default();

        targets.push(Target {
            platform,
            build,
            sources,
            transforms: Vec::new(),
            assertions: Vec::new(),
        });
    }

    Ok(targets)
}

fn normalize_target_map<T>(
    input: BTreeMap<String, T>,
    field: &str,
) -> anyhow::Result<BTreeMap<String, T>> {
    let mut output = BTreeMap::new();
    for (target, value) in input {
        let platform = normalize_target_name(&target)?;
        if output.insert(platform.clone(), value).is_some() {
            anyhow::bail!("duplicate {field} definition for target {platform}");
        }
    }
    Ok(output)
}

fn normalize_target_name(value: &str) -> anyhow::Result<String> {
    match value {
        "x86_64" | "linux/x86_64" => Ok("linux/x86_64".into()),
        "aarch64" | "linux/aarch64" => Ok("linux/aarch64".into()),
        other => anyhow::bail!("unsupported target {other}"),
    }
}

fn normalize_source(raw: RawSource, directory: &Path) -> anyhow::Result<Source> {
    let RawSource {
        path,
        url,
        mirrors,
        sha256,
        format,
        strip,
        destination,
    } = raw;

    if path.is_some() && (url.is_some() || !mirrors.is_empty()) {
        anyhow::bail!("a source must use either path or url, not both");
    }
    if path.is_none() && url.is_none() {
        anyhow::bail!("a source must define path or url");
    }

    let mut urls = Vec::new();
    if let Some(url) = url {
        urls.push(url);
        urls.extend(mirrors);
    }

    let format = format.or_else(|| {
        path.as_deref()
            .or_else(|| urls.first().map(String::as_str))
            .and_then(infer_archive_format)
    });

    if format.is_some() && destination.is_some() {
        anyhow::bail!("archive source cannot define `to`");
    }
    if format.is_none() && strip != 0 {
        anyhow::bail!("non-archive source cannot define `strip`");
    }

    let hash = if let Some(path) = &path {
        match sha256 {
            Some(value) => normalize_sha256(&value)?,
            None => hash_local_source(directory, path)?,
        }
    } else {
        let value = sha256
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("remote source must define sha256"))?;
        normalize_sha256(value)?
    };

    Ok(Source {
        path,
        urls,
        hash,
        format,
        strip_components: strip,
        destination,
    })
}

fn normalize_sha256(value: &str) -> anyhow::Result<String> {
    let normalized = if value.starts_with("sha256:") {
        value.to_owned()
    } else {
        format!("sha256:{value}")
    };
    normalized.parse::<Sha256Digest>()?;
    Ok(normalized)
}

fn hash_local_source(directory: &Path, value: &str) -> anyhow::Result<String> {
    let source = std::fs::canonicalize(directory.join(value))?;
    if !source.starts_with(directory) {
        anyhow::bail!("local source is outside the recipe directory");
    }
    if !source.is_file() {
        anyhow::bail!("local source is not a regular file: {}", source.display());
    }

    let (digest, _) = Sha256Digest::calculate_reader(File::open(source)?)?;
    Ok(digest.to_string())
}

fn infer_archive_format(value: &str) -> Option<String> {
    let path = value.split(['?', '#']).next().unwrap_or(value);
    let path = Path::new(path);
    let extension = path.extension()?.to_str()?;

    if extension.eq_ignore_ascii_case("tgz") {
        return Some("tar.gz".into());
    }

    if extension.eq_ignore_ascii_case("gz")
        && path
            .file_stem()
            .and_then(|stem| Path::new(stem).extension())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("tar"))
    {
        return Some("tar.gz".into());
    }

    if extension.eq_ignore_ascii_case("tar") {
        Some("tar".into())
    } else if extension.eq_ignore_ascii_case("zip") {
        Some("zip".into())
    } else {
        None
    }
}

fn normalize_build(raw: RawBuild, directory: &Path) -> anyhow::Result<Build> {
    if raw.script.is_some() && !raw.steps.is_empty() {
        anyhow::bail!("build must define either script or steps, not both");
    }
    if raw.script.is_none() && raw.steps.is_empty() {
        anyhow::bail!("build must define script or at least one step");
    }

    let scripts = if let Some(script) = raw.script {
        Scripts {
            install: Some(read_recipe_script(directory, &script)?),
            ..Scripts::default()
        }
    } else {
        Scripts {
            prepare: read_optional_script(directory, raw.steps.prepare)?,
            configure: read_optional_script(directory, raw.steps.configure)?,
            build: read_optional_script(directory, raw.steps.build)?,
            check: read_optional_script(directory, raw.steps.check)?,
            install: read_optional_script(directory, raw.steps.install)?,
        }
    };

    Ok(Build {
        environment: Some(raw.image),
        shell: None,
        network: raw.network,
        timeout_seconds: raw.timeout,
        scripts,
    })
}

fn read_optional_script(directory: &Path, path: Option<String>) -> anyhow::Result<Option<String>> {
    path.map(|path| read_recipe_script(directory, &path))
        .transpose()
}

fn read_recipe_script(directory: &Path, value: &str) -> anyhow::Result<String> {
    let path = std::fs::canonicalize(directory.join(value))?;
    if !path.starts_with(directory) {
        anyhow::bail!("build script is outside the recipe directory");
    }
    if !path.is_file() {
        anyhow::bail!("build script is not a regular file: {}", path.display());
    }
    std::fs::read_to_string(path).map_err(anyhow::Error::from)
}

fn normalize_transforms(
    remove: Vec<String>,
    executables: &[String],
    moves: BTreeMap<String, String>,
    copy: BTreeMap<String, String>,
    permissions: BTreeMap<String, String>,
    symlinks: BTreeMap<String, String>,
) -> Vec<Transform> {
    let mut transforms = Vec::new();
    if !remove.is_empty() {
        transforms.push(Transform::Remove {
            paths: remove,
            required: false,
        });
    }
    transforms.extend(
        moves
            .into_iter()
            .map(|(from, to)| Transform::Move { from, to }),
    );
    transforms.extend(
        copy.into_iter()
            .map(|(from, to)| Transform::Copy { from, to }),
    );
    transforms.extend(
        symlinks
            .into_iter()
            .map(|(path, target)| Transform::Symlink { path, target }),
    );
    transforms.extend(
        permissions
            .into_iter()
            .map(|(path, mode)| Transform::Chmod { path, mode }),
    );
    transforms.extend(executables.iter().cloned().map(|path| Transform::Chmod {
        path,
        mode: "0755".into(),
    }));
    transforms
}

fn executable_assertion(path: String) -> Assertion {
    Assertion::Path {
        path,
        kind: "file-or-symlink".into(),
        executable: true,
    }
}

fn normalize_integrations(
    package_name: &str,
    display_name: &str,
    commands: BTreeMap<String, RawCommand>,
    desktop: Option<OneOrMany<RawDesktop>>,
) -> anyhow::Result<(Integrations, Vec<Assertion>)> {
    let mut launchers = Vec::with_capacity(commands.len());
    let mut assertions = Vec::with_capacity(commands.len());
    let command_names = commands.keys().cloned().collect::<BTreeSet<_>>();

    for (name, command) in commands {
        let (target, arguments) = match command {
            RawCommand::Target(target) => (target, Vec::new()),
            RawCommand::Detailed(details) => (details.target, details.arguments),
        };
        assertions.push(executable_assertion(target.clone()));
        launchers.push(Launcher {
            name,
            target,
            arguments,
        });
    }

    let raw_desktop_entries = desktop.map_or_else(Vec::new, OneOrMany::into_vec);
    let desktop_count = raw_desktop_entries.len();
    let mut desktop_entries = Vec::with_capacity(desktop_count);
    let mut icons = Vec::new();

    for (index, raw) in raw_desktop_entries.into_iter().enumerate() {
        if !command_names.contains(&raw.command) {
            anyhow::bail!("desktop entry references unknown command {}", raw.command);
        }
        if raw.icon.is_some() && raw.icon_name.is_some() {
            anyhow::bail!("desktop entry must not define both icon and icon_name");
        }

        let id = raw.id.unwrap_or_else(|| {
            if desktop_count == 1 {
                package_name.to_owned()
            } else {
                let number = index + 1;
                format!("{package_name}-{number}")
            }
        });
        let icon = if let Some(source) = raw.icon {
            let name = format!("pako-{id}");
            icons.push(Icon {
                name: name.clone(),
                source,
                context: "apps".into(),
                size: "scalable".into(),
            });
            name
        } else {
            raw.icon_name.unwrap_or_default()
        };
        let exec = match raw.arguments {
            Some(arguments) if !arguments.trim().is_empty() => {
                let arguments = arguments.trim();
                let command = raw.command;
                format!("{command} {arguments}")
            }
            Some(_) | None => raw.command,
        };

        desktop_entries.push(DesktopEntry {
            id,
            name: raw.name.unwrap_or_else(|| display_name.to_owned()),
            exec,
            icon,
            terminal: raw.terminal,
            categories: raw.categories,
        });
    }

    Ok((
        Integrations {
            launchers,
            desktop_entries,
            icons,
        },
        assertions,
    ))
}
