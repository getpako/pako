use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use log::info;
use pako_core::{
    canonical,
    manifest::{PackageManifest, PACKAGE_MANIFEST_MEDIA_TYPE, PAYLOAD_MEDIA_TYPE},
    Sha256Digest,
};
use pako_oci::{
    Descriptor, ImageIndex, ImageManifest, OciClient, OciReference, Platform, Reference, Registry,
};
use tempfile::NamedTempFile;

const OCI_IMAGE_INDEX_MEDIA_TYPE: &str = "application/vnd.oci.image.index.v1+json";
const OCI_IMAGE_MANIFEST_MEDIA_TYPE: &str = "application/vnd.oci.image.manifest.v1+json";
const OCI_EMPTY_CONFIG_MEDIA_TYPE: &str = "application/vnd.oci.empty.v1+json";

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
    info!("uploading package manifest and payload");
    let manifest_digest = client
        .push_blob(&reference, &artifacts.manifest_path)
        .await?;
    let payload_digest = client.push_blob(&reference, &artifacts.payload).await?;
    if manifest_digest != artifacts.manifest_digest
        || payload_digest != artifacts.manifest.payload.digest
    {
        anyhow::bail!("artifact changed while publishing");
    }
    let layers = vec![
        descriptor(
            PACKAGE_MANIFEST_MEDIA_TYPE,
            manifest_digest,
            &artifacts.manifest_path,
        )?,
        descriptor(PAYLOAD_MEDIA_TYPE, payload_digest, &artifacts.payload)?,
    ];
    let image = ImageManifest {
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
        annotations: BTreeMap::default(),
    };
    let bytes = canonical::to_vec(&image)?;
    let digest = Sha256Digest::calculate(&bytes);
    client
        .push_manifest(
            &reference.with_digest(digest),
            OCI_IMAGE_MANIFEST_MEDIA_TYPE,
            &bytes,
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
            digest,
            size: u64::try_from(bytes.len())?,
            annotations: BTreeMap::default(),
            platform: Some(Platform {
                os: os.into(),
                architecture: match architecture {
                    "x86_64" => "amd64",
                    "aarch64" => "arm64",
                    _ => anyhow::bail!("unsupported target"),
                }
                .into(),
            }),
        }],
        annotations: BTreeMap::default(),
    };
    client
        .push_manifest(
            &reference,
            OCI_IMAGE_INDEX_MEDIA_TYPE,
            &canonical::to_vec(&index)?,
        )
        .await
}

#[derive(Debug)]
struct Artifacts {
    manifest: PackageManifest,
    manifest_path: PathBuf,
    manifest_digest: Sha256Digest,
    payload: PathBuf,
}
impl Artifacts {
    fn load(directory: &Path) -> anyhow::Result<Self> {
        let manifest_path = directory.join("package-manifest.json");
        let bytes = std::fs::read(&manifest_path)?;
        let manifest: PackageManifest = serde_json::from_slice(&bytes)?;
        manifest.validate()?;
        let payload = directory.join("payload.tar.zst");
        let (digest, size) = Sha256Digest::calculate_reader(std::fs::File::open(&payload)?)?;
        if digest != manifest.payload.digest || size != manifest.payload.size {
            anyhow::bail!("payload does not match manifest");
        }
        Ok(Self {
            manifest,
            manifest_path,
            manifest_digest: Sha256Digest::calculate(&bytes),
            payload,
        })
    }
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
