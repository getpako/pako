use pako_core::{
    installer::{InstallRequest, Installer},
    manifest::{PackageManifest, PACKAGE_MANIFEST_MEDIA_TYPE, PAYLOAD_MEDIA_TYPE},
    receipt::{PackageState, Receipt},
    Sha256Digest,
};
use pako_oci::{ImageIndex, ImageManifest, OciClient, OciReference, Registry};
use pako_trust::TrustedRepository;
use serde::Deserialize;
use std::{fs::File, path::PathBuf, str::FromStr};
use url::Url;

use crate::{cli::Concurrency, output::Ui};

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RepositoryConfig {
    name: String,
    root: PathBuf,
    metadata_url: Url,
    targets_url: Url,
    #[serde(default)]
    allow_insecure_http: bool,
}
impl RepositoryConfig {
    pub(crate) fn load(layout: &pako_core::layout::Layout) -> anyhow::Result<Self> {
        let path = layout.config.join("repository.json");
        if !path.exists() {
            anyhow::bail!("repository is not configured; create {}", path.display());
        }
        Ok(serde_json::from_reader(File::open(path)?)?)
    }
}
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum PackageOperation {
    Install,
    Upgrade,
}
#[derive(Debug)]
pub(crate) struct RemoteInstallPlan {
    pub(crate) operation: PackageOperation,
    pub(crate) repository: String,
    pub(crate) channel: String,
    pub(crate) target: String,
    pub(crate) manifest: PackageManifest,
    pub(crate) download_bytes: u64,
    pub(crate) installed_bytes: u64,
    pub(crate) current_version: Option<String>,
    pub(crate) up_to_date: bool,
    pub(crate) launcher_count: usize,
    pub(crate) desktop_entry_count: usize,
    pub(crate) icon_count: usize,
    client: OciClient,
    reference: OciReference,
    platform_digest: Sha256Digest,
    manifest_digest: Sha256Digest,
    payload_digest: Sha256Digest,
}
impl RemoteInstallPlan {
    pub(crate) fn version(&self) -> String {
        format!(
            "{}-{}",
            self.manifest.upstream_version, self.manifest.release
        )
    }
    pub(crate) fn data_growth(&self) -> u64 {
        self.installed_bytes
    }
}
#[derive(Debug)]
pub(crate) enum InstallOutcome {
    Installed(Box<Receipt>),
    AlreadyCurrent,
}

pub(crate) async fn resolve_remote(
    installer: &Installer,
    package: &str,
    channel: &str,
    operation: PackageOperation,
    concurrency: Concurrency,
    ui: Ui,
) -> anyhow::Result<RemoteInstallPlan> {
    let repository = RepositoryConfig::load(installer.layout())?;
    let trusted = TrustedRepository::new(
        repository.root.clone(),
        repository.metadata_url.clone(),
        repository.targets_url.clone(),
        installer.layout().state.join("tuf").join(&repository.name),
    );
    let catalog = trusted.refresh_catalog().await?;
    let target = host_target();
    let release = catalog.resolve(package, &target, channel)?;
    let reference = OciReference::from_str(&release.oci)?.with_digest(release.manifest_digest);
    let mut client = OciClient::new()?.with_download_limit(concurrency.download_jobs);
    if repository.allow_insecure_http {
        ensure_loopback_registry(&reference.registry)?;
        client = client.insecure_http();
    }
    let step = ui.spinner("Resolving package metadata");
    let platform = resolve_platform(&client, &reference, &target).await?;
    let reference = reference.with_digest(platform.digest);
    let image = fetch_image_manifest(&client, &reference).await?;
    let manifest_descriptor = image
        .layers
        .iter()
        .find(|d| d.media_type == PACKAGE_MANIFEST_MEDIA_TYPE)
        .ok_or_else(|| anyhow::anyhow!("package manifest layer is missing"))?;
    let payload_descriptor = image
        .layers
        .iter()
        .find(|d| d.media_type == PAYLOAD_MEDIA_TYPE)
        .ok_or_else(|| anyhow::anyhow!("payload layer is missing"))?;
    let manifest_path = installer
        .layout()
        .staging()
        .join(format!("manifest-{}", manifest_descriptor.digest.hex()));
    client
        .fetch_blob(&reference, manifest_descriptor.digest, &manifest_path)
        .await?;
    let manifest: PackageManifest = serde_json::from_reader(File::open(&manifest_path)?)?;
    let _ = std::fs::remove_file(&manifest_path);
    manifest.validate()?;
    if manifest.payload.digest != payload_descriptor.digest
        || manifest.payload.size != payload_descriptor.size
    {
        anyhow::bail!("payload descriptor does not match package manifest");
    }
    step.finish("Package metadata verified");
    let installed_bytes = manifest
        .entries
        .iter()
        .filter_map(|entry| {
            if let pako_core::manifest::Entry::File { size, .. } = entry {
                Some(*size)
            } else {
                None
            }
        })
        .sum();
    let current = installer
        .layout()
        .package_state(package)?
        .exists()
        .then(|| PackageState::load(&installer.layout().package_state(package)?))
        .transpose()?;
    let current_version = current.as_ref().map(|state| state.active.clone());
    let up_to_date = current
        .as_ref()
        .map(|state| Receipt::load(&installer.layout().version_record(package, &state.active)?))
        .transpose()?
        .is_some_and(|receipt| receipt.oci_manifest_digest == platform.digest);
    Ok(RemoteInstallPlan {
        operation,
        repository: repository.name,
        channel: channel.into(),
        target,
        download_bytes: manifest.payload.size,
        installed_bytes,
        current_version,
        up_to_date,
        launcher_count: manifest.integrations.launchers.len(),
        desktop_entry_count: manifest.integrations.desktop_entries.len(),
        icon_count: manifest.integrations.icons.len(),
        manifest,
        client,
        reference,
        platform_digest: platform.digest,
        manifest_digest: manifest_descriptor.digest,
        payload_digest: payload_descriptor.digest,
    })
}
pub(crate) async fn execute_remote(
    installer: &Installer,
    plan: RemoteInstallPlan,
    ui: Ui,
) -> anyhow::Result<InstallOutcome> {
    if plan.up_to_date {
        return Ok(InstallOutcome::AlreadyCurrent);
    }
    let path = installer
        .layout()
        .staging()
        .join(format!("payload-{}.tar.zst", plan.payload_digest.hex()));
    let progress = ui.byte_progress("Downloading package payload", plan.download_bytes);
    plan.client
        .fetch_blob_with_progress(&plan.reference, plan.payload_digest, &path, &progress)
        .await?;
    pako_log::finish_progress(&progress, "Downloaded package payload");
    let request = InstallRequest {
        repository: plan.repository,
        oci_manifest_digest: plan.platform_digest,
        package_manifest_digest: plan.manifest_digest,
        payload_digest: plan.payload_digest,
        channel: plan.channel,
    };
    let local = installer.clone();
    let manifest = plan.manifest;
    let install_path = path.clone();
    let result =
        tokio::task::spawn_blocking(move || local.install(&manifest, &install_path, &request))
            .await?;
    let _ = std::fs::remove_file(&path);
    Ok(InstallOutcome::Installed(Box::new(result?)))
}
fn ensure_loopback_registry(registry: &str) -> anyhow::Result<()> {
    if registry.starts_with("localhost")
        || registry.starts_with("127.0.0.1")
        || registry.starts_with("[::1]")
    {
        Ok(())
    } else {
        anyhow::bail!("allowInsecureHttp is permitted only for a loopback registry")
    }
}
async fn resolve_platform(
    client: &OciClient,
    reference: &OciReference,
    target: &str,
) -> anyhow::Result<pako_oci::Descriptor> {
    let (_, bytes) = client.fetch_manifest(reference).await?;
    let index: ImageIndex = serde_json::from_slice(&bytes)?;
    index
        .manifests
        .into_iter()
        .find(|descriptor| {
            descriptor.platform.as_ref().is_some_and(|platform| {
                format!(
                    "{}/{}",
                    platform.os,
                    normalize_architecture(&platform.architecture)
                ) == target
            })
        })
        .ok_or_else(|| anyhow::anyhow!("OCI index has no platform for {target}"))
}
async fn fetch_image_manifest(
    client: &OciClient,
    reference: &OciReference,
) -> anyhow::Result<ImageManifest> {
    let (_, bytes) = client.fetch_manifest(reference).await?;
    Ok(serde_json::from_slice(&bytes)?)
}
fn host_target() -> String {
    format!("linux/{}", normalize_architecture(std::env::consts::ARCH))
}
fn normalize_architecture(value: &str) -> &str {
    match value {
        "x86_64" | "amd64" => "x86_64",
        "aarch64" | "arm64" => "aarch64",
        _ => value,
    }
}
