use std::{
    collections::HashMap,
    num::NonZeroU64,
    path::{Path, PathBuf},
};

use jiff::{SignedDuration, Timestamp};
use pako_core::Sha256Digest;
use ring::{
    rand::SystemRandom,
    signature::{Ed25519KeyPair, KeyPair},
};
use serde::{Deserialize, Serialize};
use tough::{
    editor::{signed::SignedRole, RepositoryEditor},
    key_source::LocalKeySource,
    schema::{key::Key, KeyHolder, RoleKeys, RoleType, Root},
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
    oci: String,
    manifest_digest: Sha256Digest,
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

pub(crate) async fn add_release(
    directory: &Path,
    package_name: String,
    release: Release,
) -> anyhow::Result<()> {
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
    package.releases.push(release);
    catalog
        .packages
        .sort_by(|left, right| left.name.cmp(&right.name));
    std::fs::write(catalog_path, serde_json::to_vec_pretty(&catalog)?)?;
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
    oci: String,
    manifest_digest: Sha256Digest,
) -> Release {
    Release {
        upstream_version,
        number: release,
        channel: "stable".into(),
        target,
        oci,
        manifest_digest,
    }
}

async fn sign(directory: &Path, version: u64) -> anyhow::Result<()> {
    let metadata = directory.join("metadata");
    let root = metadata.join("root.json");
    let key = directory.join("keys/targets-and-metadata.ed25519.pk8");
    let mut editor = RepositoryEditor::new(&root).await?;
    let expires = Timestamp::now() + SignedDuration::from_hours(30 * 24);
    editor
        .targets_expires(expires)?
        .targets_version(NonZeroU64::new(version).unwrap())?
        .snapshot_expires(expires)
        .snapshot_version(NonZeroU64::new(version).unwrap())
        .timestamp_expires(expires)
        .timestamp_version(NonZeroU64::new(version).unwrap())
        .add_target_path(directory.join("targets/catalog.json"))
        .await?;
    let repository = editor
        .sign(&[Box::new(LocalKeySource { path: key })])
        .await?;
    repository.write(metadata).await?;
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
