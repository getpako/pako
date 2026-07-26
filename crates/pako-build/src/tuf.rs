use std::{
    collections::{BTreeMap, HashMap},
    num::NonZeroU64,
    path::{Path, PathBuf},
};

use jiff::{SignedDuration, Timestamp};
use pako_core::manifest::{Artifact, PackageManifest};
use ring::{
    rand::SystemRandom,
    signature::{Ed25519KeyPair, KeyPair},
};
use serde::{Deserialize, Serialize};
use tough::{
    editor::{signed::SignedRole, RepositoryEditor},
    key_source::LocalKeySource,
    schema::{key::Key, KeyHolder, RoleKeys, RoleType, Root, Target},
    TargetName,
};

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Catalog {
    schema: u32,
    packages: Vec<Package>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Package {
    name: String,
    channels: BTreeMap<String, String>,
    releases: Vec<Release>,
}

#[derive(Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Release {
    upstream_version: String,
    #[serde(rename = "release")]
    number: u32,
    channel: String,
    target: String,
    manifest_target: String,
}

pub(crate) async fn init(directory: &Path) -> anyhow::Result<()> {
    if directory.exists() {
        anyhow::bail!("TUF directory already exists: {}", directory.display());
    }
    let metadata = directory.join("metadata");
    let targets = directory.join("targets");
    let keys = directory.join("keys");
    std::fs::create_dir_all(&metadata)?;
    std::fs::create_dir_all(&targets)?;
    std::fs::create_dir_all(&keys)?;

    let key_path = keys.join("targets-and-metadata.ed25519.pk8");
    let key = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
        .map_err(|_| anyhow::anyhow!("could not generate an Ed25519 key"))?;
    std::fs::write(&key_path, key.as_ref())?;
    #[cfg(unix)]
    std::fs::set_permissions(
        &key_path,
        std::os::unix::fs::PermissionsExt::from_mode(0o600),
    )?;

    let pair = Ed25519KeyPair::from_pkcs8(key.as_ref())
        .map_err(|_| anyhow::anyhow!("could not read generated Ed25519 key"))?;
    let public = hex::encode(pair.public_key().as_ref());
    let key: Key = public.parse()?;
    let key_id = key.key_id()?;
    let roles = [
        RoleType::Root,
        RoleType::Targets,
        RoleType::Snapshot,
        RoleType::Timestamp,
    ]
    .into_iter()
    .map(|role| {
        (
            role,
            RoleKeys {
                keyids: vec![key_id.clone()],
                threshold: NonZeroU64::MIN,
                _extra: HashMap::new(),
            },
        )
    })
    .collect();
    let root = Root {
        spec_version: "1.0.0".into(),
        consistent_snapshot: false,
        version: NonZeroU64::MIN,
        expires: Timestamp::now() + SignedDuration::from_hours(3650 * 24),
        keys: HashMap::from([(key_id, key)]),
        roles,
        _extra: HashMap::new(),
    };
    let holder = KeyHolder::Root(root.clone());
    let root = SignedRole::new(
        root,
        &holder,
        &[Box::new(LocalKeySource { path: key_path })],
        &aws_lc_rs::rand::SystemRandom::new(),
    )
    .await?;
    std::fs::write(metadata.join("root.json"), root.buffer())?;
    std::fs::write(
        targets.join("catalog.json"),
        serde_json::to_vec_pretty(&Catalog {
            schema: 1,
            packages: Vec::new(),
        })?,
    )?;
    sign(directory, 1).await
}

pub(crate) async fn refresh(directory: &Path) -> anyhow::Result<()> {
    let targets_metadata = directory.join("metadata/targets.json");
    let version = next_version(&targets_metadata)?;
    sign(directory, version).await
}

pub(crate) async fn add_release(
    directory: &Path,
    package_name: String,
    artifact_directory: &Path,
    release: Release,
) -> anyhow::Result<()> {
    let manifest_path = artifact_directory.join("package-manifest.json");
    let manifest: PackageManifest = serde_json::from_slice(&std::fs::read(&manifest_path)?)?;
    manifest.validate()?;
    let manifest_target = release.manifest_target.clone();
    copy_target(directory, &manifest_target, &manifest_path)?;
    if let Artifact::TufArchive { target, .. } = &manifest.artifact {
        let artifact_path = artifact_directory.join("package.tar.zst");
        let (digest, size) =
            pako_core::Sha256Digest::calculate_reader(std::fs::File::open(&artifact_path)?)?;
        if digest != manifest.artifact.digest() || size != manifest.artifact.size() {
            anyhow::bail!("package archive does not match manifest");
        }
        copy_target(directory, target, &artifact_path)?;
    }

    let catalog_path = directory.join("targets/catalog.json");
    let mut catalog: Catalog = serde_json::from_slice(&std::fs::read(&catalog_path)?)?;
    let position = catalog
        .packages
        .iter()
        .position(|package| package.name == package_name);
    let package = if let Some(position) = position {
        &mut catalog.packages[position]
    } else {
        catalog.packages.push(Package {
            name: package_name,
            channels: BTreeMap::new(),
            releases: Vec::new(),
        });
        catalog
            .packages
            .last_mut()
            .expect("package was just inserted")
    };
    package.releases.retain(|item| {
        !(item.target == release.target
            && item.channel == release.channel
            && item.upstream_version == release.upstream_version
            && item.number == release.number)
    });
    let release_id = format!("{}-{}", release.upstream_version, release.number);
    let channel = release.channel.clone();
    package.releases.push(release);
    package.channels.insert(channel, release_id);
    catalog
        .packages
        .sort_by(|left, right| left.name.cmp(&right.name));
    let catalog_bytes = serde_json::to_vec_pretty(&catalog)?;
    let catalog_temporary = directory.join("targets/catalog.json.partial");
    std::fs::write(&catalog_temporary, catalog_bytes)?;
    std::fs::rename(catalog_temporary, catalog_path)?;
    sign(
        directory,
        next_version(&directory.join("metadata/targets.json"))?,
    )
    .await
}

pub(crate) fn release(
    upstream_version: String,
    release: u32,
    target: String,
    manifest_target: String,
) -> Release {
    Release {
        upstream_version,
        number: release,
        channel: "stable".into(),
        target,
        manifest_target,
    }
}

async fn sign(directory: &Path, version: u64) -> anyhow::Result<()> {
    let metadata = directory.join("metadata");
    let targets = directory.join("targets");
    let root = metadata.join("root.json");
    let key = directory.join("keys/targets-and-metadata.ed25519.pk8");
    let mut editor = RepositoryEditor::new(&root).await?;
    let now = Timestamp::now();
    editor
        .targets_expires(now + SignedDuration::from_hours(90 * 24))?
        .targets_version(NonZeroU64::new(version).unwrap())?
        .snapshot_expires(now + SignedDuration::from_hours(30 * 24))
        .snapshot_version(NonZeroU64::new(version).unwrap())
        .timestamp_expires(now + SignedDuration::from_hours(7 * 24))
        .timestamp_version(NonZeroU64::new(version).unwrap());
    for path in target_paths(&targets)? {
        let name = target_name(&targets, &path)?;
        editor.add_target(TargetName::new(name)?, Target::from_path(path).await?)?;
    }
    let repository = editor
        .sign(&[Box::new(LocalKeySource { path: key })])
        .await?;
    repository.write(metadata).await?;
    Ok(())
}

fn target_paths(targets: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for entry in walkdir::WalkDir::new(targets).follow_links(false) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let file_name = entry.file_name().to_string_lossy();
        if file_name.ends_with(".partial") || file_name.contains(".partial-") {
            continue;
        }
        paths.push(entry.into_path());
    }
    paths.sort();
    Ok(paths)
}

fn target_name(targets: &Path, path: &Path) -> anyhow::Result<String> {
    path.strip_prefix(targets)?
        .components()
        .map(|component| {
            component
                .as_os_str()
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("target path is not valid UTF-8"))
        })
        .collect::<anyhow::Result<Vec<_>>>()
        .map(|components| components.join("/"))
}

fn copy_target(directory: &Path, target: &str, source: &Path) -> anyhow::Result<()> {
    let destination = directory.join("targets").join(target);
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(source, destination)?;
    Ok(())
}

fn next_version(path: &PathBuf) -> anyhow::Result<u64> {
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(path)?)?;
    value["signed"]["version"]
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("invalid targets version"))?
        .checked_add(1)
        .ok_or_else(|| anyhow::anyhow!("TUF version overflow"))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use serde_json::Value;
    use tempfile::tempdir;

    use super::*;

    #[tokio::test]
    async fn signing_preserves_all_targets_across_publications_and_refresh() {
        let parent = tempdir().expect("temporary directory");
        let directory = parent.path().join("repository");
        init(&directory).await.expect("initialize repository");

        let targets = directory.join("targets");
        for target in [
            "manifests/package-a/1.0-1/linux-x86_64.json",
            "manifests/package-b/1.0-1/linux-x86_64.json",
            "manifests/package-a/2.0-1/linux-x86_64.json",
            "artifacts/package-a/1.0-1/linux-x86_64.tar.zst",
        ] {
            let path = targets.join(target);
            std::fs::create_dir_all(path.parent().expect("target parent"))
                .expect("target directory");
            std::fs::write(path, target).expect("target contents");
        }

        sign(&directory, 2).await.expect("sign publications");
        let expected = BTreeSet::from([
            "artifacts/package-a/1.0-1/linux-x86_64.tar.zst".to_owned(),
            "manifests/package-a/1.0-1/linux-x86_64.json".to_owned(),
            "manifests/package-a/2.0-1/linux-x86_64.json".to_owned(),
            "manifests/package-b/1.0-1/linux-x86_64.json".to_owned(),
            "catalog.json".to_owned(),
        ]);
        assert_eq!(signed_target_names(&directory), expected);

        refresh(&directory).await.expect("refresh repository");
        assert_eq!(signed_target_names(&directory), expected);
    }

    fn signed_target_names(directory: &Path) -> BTreeSet<String> {
        let metadata = directory.join("metadata/targets.json");
        let value: Value = serde_json::from_slice(&std::fs::read(metadata).expect("metadata"))
            .expect("valid metadata");
        value["signed"]["targets"]
            .as_object()
            .expect("targets map")
            .keys()
            .cloned()
            .collect()
    }
}
