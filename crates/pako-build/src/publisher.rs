use std::{
    collections::{BTreeMap, VecDeque},
    fs::File,
    path::{Path, PathBuf},
    sync::Mutex,
};

use futures_util::{stream, StreamExt};
use indicatif::{ProgressBar, ProgressStyle};
use log::info;
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

    info!("loading and verifying publish artifact");
    let artifact = artifact.to_owned();
    let artifacts = tokio::task::spawn_blocking(move || Artifacts::load(&artifact)).await??;
    let mut client = OciClient::new()?;
    if insecure_http {
        client = client.insecure_http();
    }
    if let Some((username, password)) = credentials {
        client = client.with_basic_auth(username, password);
    }

    let config = NamedTempFile::new()?;
    std::fs::write(config.path(), b"{}")?;
    info!("uploading OCI configuration");
    let config_digest = client.push_blob(&reference, config.path()).await?;

    info!("uploading package manifest");
    let package_digest = push_checked_blob(
        &client,
        &reference,
        &artifacts.package_manifest,
        artifacts.package_manifest_digest,
    )
    .await?;
    info!("uploading pack index");
    let index_digest = push_checked_blob(
        &client,
        &reference,
        &artifacts.pack_index,
        artifacts.pack_index_digest,
    )
    .await?;
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
    let pack_count = artifacts.packs.len();
    let progress = pack_progress("uploading packs", pack_count);
    let upload_jobs = std::thread::available_parallelism()
        .map_or(1, usize::from)
        .min(6);
    let upload_client = client.clone();
    let upload_reference = reference.clone();
    let upload_progress = progress.clone();
    let uploads = stream::iter(artifacts.packs.iter().map(move |(digest, path)| {
        let digest = *digest;
        let path = path.clone();
        let progress = upload_progress.clone();
        let client = upload_client.clone();
        let reference = upload_reference.clone();
        async move {
            let uploaded = push_checked_blob(&client, &reference, &path, digest).await?;
            if uploaded != digest {
                anyhow::bail!(
                    "pack filename digest does not match its contents: {}",
                    path.display()
                );
            }
            let layer = descriptor(PACK_MEDIA_TYPE, uploaded, &path)?;
            progress.inc(1);
            Ok::<_, anyhow::Error>((digest, layer))
        }
    }))
    .buffer_unordered(upload_jobs.max(1));

    futures_util::pin_mut!(uploads);
    let mut pack_layers = BTreeMap::new();
    while let Some(result) = uploads.next().await {
        let (digest, layer) = result?;
        pack_layers.insert(digest, layer);
    }
    layers.extend(pack_layers.into_values());
    progress.finish_with_message(format!("uploaded {pack_count} packs"));

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
    info!("publishing platform manifest");
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
                architecture: oci_architecture(architecture)?.into(),
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
    info!("publishing OCI image index");
    client
        .push_manifest(&reference, OCI_IMAGE_INDEX_MEDIA_TYPE, &index_bytes)
        .await
}

fn oci_architecture(architecture: &str) -> anyhow::Result<&'static str> {
    match architecture {
        "x86_64" => Ok("amd64"),
        "aarch64" => Ok("arm64"),
        _ => anyhow::bail!("unsupported Pako architecture for OCI: {architecture}"),
    }
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
        let pack_count = index.packs.len();
        let progress = pack_progress("verifying packs", pack_count);
        let jobs = std::thread::available_parallelism()
            .map_or(1, usize::from)
            .min(pack_count.max(1));
        let pending = index
            .packs
            .iter()
            .map(|(digest, descriptor)| (*digest, descriptor.size))
            .collect::<Vec<_>>();
        let queue = Mutex::new(VecDeque::from(pending));
        let results = Mutex::new(Vec::new());

        std::thread::scope(|scope| {
            for _ in 0..jobs {
                let queue = &queue;
                let results = &results;
                let progress = progress.clone();
                scope.spawn(move || loop {
                    let Some((digest, size)) = queue
                        .lock()
                        .expect("pack verification queue lock poisoned")
                        .pop_front()
                    else {
                        return;
                    };
                    let path = directory
                        .join("packs")
                        .join(format!("{}.pakopack", digest.hex()));
                    let result = verify_pack_artifact(&path, digest, size)
                        .map(|()| (digest, path));
                    progress.inc(1);
                    results
                        .lock()
                        .expect("pack verification result lock poisoned")
                        .push(result);
                });
            }
        });

        let verified = results
            .into_inner()
            .expect("pack verification result lock poisoned")
            .into_iter()
            .collect::<anyhow::Result<Vec<_>>>()?;
        let packs = verified.into_iter().collect::<BTreeMap<_, _>>();
        progress.finish_with_message(format!("verified {pack_count} packs"));
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

fn verify_pack_artifact(
    path: &Path,
    expected_digest: Sha256Digest,
    expected_size: u64,
) -> anyhow::Result<()> {
    if std::fs::metadata(path)?.len() != expected_size {
        anyhow::bail!("pack size does not match index: {}", path.display());
    }
    let (actual, _) = Sha256Digest::calculate_reader(File::open(path)?)?;
    if actual != expected_digest {
        anyhow::bail!("pack digest does not match index: {}", path.display());
    }
    Ok(())
}

fn pack_progress(message: &str, pack_count: usize) -> ProgressBar {
    let progress = ProgressBar::new(pack_count as u64);
    let style = ProgressStyle::with_template(
        "{spinner:.green} {msg} [{bar:40.cyan/blue}] {pos}/{len} packs ({per_sec})",
    )
    .expect("pack publish progress template is valid")
    .progress_chars("#>-");
    progress.set_style(style);
    progress.set_message(message.to_owned());
    progress.enable_steady_tick(std::time::Duration::from_millis(100));
    progress
}

async fn push_checked_blob(
    client: &OciClient,
    reference: &OciReference,
    path: &Path,
    expected: Sha256Digest,
) -> anyhow::Result<Sha256Digest> {
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
