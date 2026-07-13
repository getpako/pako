use std::{
    collections::BTreeMap,
    fs::File,
    path::{Path, PathBuf},
};

use pako_core::{
    canonical,
    manifest::{PackIndex, PackageManifest, PACKAGE_MANIFEST_MEDIA_TYPE, PACK_INDEX_MEDIA_TYPE},
    Sha256Digest,
};
use pako_oci::{
    Descriptor, ImageIndex, ImageManifest, OciClient, OciReference, Platform, Reference, Registry,
};
use tempfile::NamedTempFile;

const OCI_IMAGE_INDEX_MEDIA_TYPE: &str = "application/vnd.oci.image.index.v1+json";
const OCI_IMAGE_MANIFEST_MEDIA_TYPE: &str = "application/vnd.oci.image.manifest.v1+json";
const OCI_EMPTY_CONFIG_MEDIA_TYPE: &str = "application/vnd.oci.empty.v1+json";
const PACK_MEDIA_TYPE: &str = "application/vnd.pako.chunk-pack.v1";

#[expect(
    clippy::too_many_lines,
    reason = "publication order is kept together to make its atomicity guarantees auditable"
)]
pub(crate) async fn publish(
    artifact: &Path,
    reference: OciReference,
    insecure_http: bool,
    credentials: Option<(String, String)>,
) -> anyhow::Result<Sha256Digest> {
    if matches!(reference.reference, Reference::Digest(_)) {
        anyhow::bail!("publish reference must use a tag, not a digest");
    }

    let artifacts = Artifacts::load(artifact)?;
    let mut client = OciClient::new()?;
    if insecure_http {
        client = client.insecure_http();
    }
    if let Some((username, password)) = credentials {
        client = client.with_basic_auth(username, password);
    }

    let config = NamedTempFile::new()?;
    std::fs::write(config.path(), b"{}")?;
    let config_digest = client.push_blob(&reference, config.path()).await?;

    let package_digest =
        push_checked_blob(&client, &reference, &artifacts.package_manifest).await?;
    let index_digest = push_checked_blob(&client, &reference, &artifacts.pack_index).await?;
    if package_digest != artifacts.package_manifest_digest
        || index_digest != artifacts.pack_index_digest
    {
        anyhow::bail!("artifact metadata changed while publishing");
    }

    let mut layers = vec![
        descriptor(
            PACKAGE_MANIFEST_MEDIA_TYPE,
            package_digest,
            &artifacts.package_manifest,
        )?,
        descriptor(PACK_INDEX_MEDIA_TYPE, index_digest, &artifacts.pack_index)?,
    ];
    for (digest, path) in &artifacts.packs {
        let uploaded = push_checked_blob(&client, &reference, path).await?;
        if uploaded != *digest {
            anyhow::bail!(
                "pack filename digest does not match its contents: {}",
                path.display()
            );
        }
        layers.push(descriptor(PACK_MEDIA_TYPE, uploaded, path)?);
    }

    let manifest = ImageManifest {
        schema_version: 2,
        media_type: OCI_IMAGE_MANIFEST_MEDIA_TYPE.into(),
        config: Descriptor {
            media_type: OCI_EMPTY_CONFIG_MEDIA_TYPE.into(),
            digest: config_digest,
            size: 2,
            annotations: BTreeMap::default(),
            platform: None,
        },
        layers,
        annotations: BTreeMap::from([
            (
                "org.opencontainers.image.title".into(),
                artifacts.manifest.package.clone(),
            ),
            (
                "org.opencontainers.image.version".into(),
                artifacts.manifest.upstream_version.clone(),
            ),
            (
                "dev.pako.release".into(),
                artifacts.manifest.release.to_string(),
            ),
            ("dev.pako.target".into(), artifacts.manifest.target.clone()),
        ]),
    };
    let manifest_bytes = canonical::to_vec(&manifest)?;
    let manifest_digest = Sha256Digest::calculate(&manifest_bytes);
    client
        .push_manifest(
            &reference.with_digest(manifest_digest),
            OCI_IMAGE_MANIFEST_MEDIA_TYPE,
            &manifest_bytes,
        )
        .await?;

    let (os, architecture) = artifacts
        .manifest
        .target
        .split_once('/')
        .ok_or_else(|| anyhow::anyhow!("invalid package target"))?;
    let index = ImageIndex {
        schema_version: 2,
        media_type: OCI_IMAGE_INDEX_MEDIA_TYPE.into(),
        manifests: vec![Descriptor {
            media_type: OCI_IMAGE_MANIFEST_MEDIA_TYPE.into(),
            digest: manifest_digest,
            size: u64::try_from(manifest_bytes.len())?,
            annotations: BTreeMap::default(),
            platform: Some(Platform {
                architecture: architecture.into(),
                os: os.into(),
            }),
        }],
        annotations: BTreeMap::from([
            (
                "org.opencontainers.image.title".into(),
                artifacts.manifest.package,
            ),
            (
                "org.opencontainers.image.version".into(),
                artifacts.manifest.upstream_version,
            ),
        ]),
    };
    let index_bytes = canonical::to_vec(&index)?;
    client
        .push_manifest(&reference, OCI_IMAGE_INDEX_MEDIA_TYPE, &index_bytes)
        .await
}

#[derive(Debug)]
struct Artifacts {
    manifest: PackageManifest,
    package_manifest: PathBuf,
    package_manifest_digest: Sha256Digest,
    pack_index: PathBuf,
    pack_index_digest: Sha256Digest,
    packs: BTreeMap<Sha256Digest, PathBuf>,
}

impl Artifacts {
    fn load(directory: &Path) -> anyhow::Result<Self> {
        let package_manifest = directory.join("package-manifest.json");
        let package_bytes = std::fs::read(&package_manifest)?;
        let manifest: PackageManifest = serde_json::from_slice(&package_bytes)?;
        manifest.validate()?;
        let package_manifest_digest = Sha256Digest::calculate(&package_bytes);
        let pack_index = directory.join("pack-index.json");
        let index_bytes = std::fs::read(&pack_index)?;
        let index: PackIndex = serde_json::from_slice(&index_bytes)?;
        index.validate_against(&manifest)?;
        if index.package_manifest_digest != package_manifest_digest {
            anyhow::bail!("pack index references a different package manifest");
        }
        let mut packs = BTreeMap::new();
        for (digest, descriptor) in &index.packs {
            let path = directory
                .join("packs")
                .join(format!("{}.pakopack", digest.hex()));
            if std::fs::metadata(&path)?.len() != descriptor.size {
                anyhow::bail!("pack size does not match index: {}", path.display());
            }
            let (actual, _) = Sha256Digest::calculate_reader(File::open(&path)?)?;
            if actual != *digest {
                anyhow::bail!("pack digest does not match index: {}", path.display());
            }
            packs.insert(*digest, path);
        }
        Ok(Self {
            manifest,
            package_manifest,
            package_manifest_digest,
            pack_index,
            pack_index_digest: Sha256Digest::calculate(&index_bytes),
            packs,
        })
    }
}

async fn push_checked_blob(
    client: &OciClient,
    reference: &OciReference,
    path: &Path,
) -> anyhow::Result<Sha256Digest> {
    let expected = Sha256Digest::calculate_reader(File::open(path)?)?.0;
    let actual = client.push_blob(reference, path).await?;
    if actual != expected {
        anyhow::bail!(
            "registry returned an unexpected blob digest for {}",
            path.display()
        );
    }
    Ok(actual)
}

fn descriptor(media_type: &str, digest: Sha256Digest, path: &Path) -> anyhow::Result<Descriptor> {
    Ok(Descriptor {
        media_type: media_type.into(),
        digest,
        size: std::fs::metadata(path)?.len(),
        annotations: BTreeMap::default(),
        platform: None,
    })
}
