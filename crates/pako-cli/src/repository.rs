use crate::{cli::Concurrency, output::Ui};
use pako_core::{
    installer::{InstallRequest, Installer},
    manifest::{Artifact, PackageManifest},
    receipt::{PackageState, Receipt},
    Sha256Digest,
};
use pako_trust::TrustedRepository;
use serde::Deserialize;
use std::{fs::File, path::PathBuf};
use url::Url;

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
    pub(crate) manifest_target: String,
    pub(crate) manifest_digest: Sha256Digest,
    pub(crate) artifact: Artifact,
    pub(crate) download_bytes: u64,
    pub(crate) installed_bytes: u64,
    pub(crate) current_version: Option<String>,
    pub(crate) up_to_date: bool,
    pub(crate) launcher_count: usize,
    pub(crate) desktop_entry_count: usize,
    pub(crate) icon_count: usize,
    trusted: TrustedRepository,
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
    let config = RepositoryConfig::load(installer.layout())?;
    if config.allow_insecure_http {
        log::warn!("allowInsecureHttp is ignored for external artifacts; manifests require HTTPS");
    }
    let trusted = TrustedRepository::new(
        config.root.clone(),
        config.metadata_url.clone(),
        config.targets_url.clone(),
        installer.layout().state.join("tuf").join(&config.name),
    );
    let catalog = trusted.refresh_catalog().await?;
    let target = host_target();
    let release = catalog.resolve(package, &target, channel)?;
    let manifest_bytes = trusted.read_target(&release.manifest_target).await?;
    let manifest_digest = Sha256Digest::calculate(&manifest_bytes);
    let manifest: PackageManifest = serde_json::from_slice(&manifest_bytes)?;
    manifest.validate()?;
    if manifest.package != package || manifest.target != target {
        anyhow::bail!("signed package manifest does not match catalog resolution");
    }
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
        .is_some_and(|receipt| {
            receipt.manifest_target == release.manifest_target
                && receipt.manifest_digest == manifest_digest
        });
    let step = ui.spinner("Package metadata verified");
    step.finish("Package metadata verified");
    log::debug!(
        "configured download concurrency: {}",
        concurrency.download_jobs
    );
    Ok(RemoteInstallPlan {
        operation,
        repository: config.name,
        channel: channel.into(),
        target,
        manifest_target: release.manifest_target.clone(),
        manifest_digest,
        download_bytes: manifest.artifact.size(),
        installed_bytes,
        current_version,
        up_to_date,
        launcher_count: manifest.integrations.launchers.len(),
        desktop_entry_count: manifest.integrations.desktop_entries.len(),
        icon_count: manifest.integrations.icons.len(),
        artifact: manifest.artifact.clone(),
        manifest,
        trusted,
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
    let path = fetch_artifact(installer, &plan, &ui).await?;
    let request = InstallRequest {
        repository: plan.repository,
        manifest_target: plan.manifest_target,
        manifest_digest: plan.manifest_digest,
        channel: plan.channel,
    };
    let local = installer.clone();
    let manifest = plan.manifest;
    let result =
        tokio::task::spawn_blocking(move || local.install(&manifest, &path, &request)).await??;
    Ok(InstallOutcome::Installed(Box::new(result)))
}

async fn fetch_artifact(
    installer: &Installer,
    plan: &RemoteInstallPlan,
    ui: &Ui,
) -> anyhow::Result<PathBuf> {
    let cache = installer.layout().cache.join("artifacts/sha256");
    tokio::fs::create_dir_all(&cache).await?;
    let path = cache.join(plan.artifact.digest().hex());
    if path.exists() {
        let (digest, size) = Sha256Digest::calculate_reader(File::open(&path)?)?;
        if digest == plan.artifact.digest() && size == plan.artifact.size() {
            return Ok(path);
        }
        let _ = std::fs::remove_file(&path);
    }
    let temporary = path.with_extension("partial");
    match &plan.artifact {
        Artifact::TufArchive { target, .. } => {
            tokio::fs::write(&temporary, plan.trusted.read_target(target).await?).await?;
        }
        Artifact::ExternalArchive { urls, .. } => {
            let client = reqwest::Client::new();
            let mut errors = Vec::new();
            for url in urls {
                if url.scheme() != "https"
                    && !(url.scheme() == "http"
                        && url
                            .host_str()
                            .is_some_and(|host| matches!(host, "localhost" | "127.0.0.1" | "::1")))
                {
                    errors.push(format!("{url}: HTTPS required"));
                    continue;
                }
                match client
                    .get(url.clone())
                    .send()
                    .await
                    .and_then(reqwest::Response::error_for_status)
                {
                    Ok(response) => match response.bytes().await {
                        Ok(bytes) => {
                            tokio::fs::write(&temporary, &bytes).await?;
                            break;
                        }
                        Err(error) => errors.push(format!("{url}: {error}")),
                    },
                    Err(error) => errors.push(format!("{url}: {error}")),
                }
            }
            if !temporary.exists() {
                anyhow::bail!("all artifact mirrors failed: {}", errors.join("; "));
            }
        }
    }
    let (digest, size) = Sha256Digest::calculate_reader(File::open(&temporary)?)?;
    if digest != plan.artifact.digest() || size != plan.artifact.size() {
        let _ = std::fs::remove_file(&temporary);
        anyhow::bail!("downloaded artifact integrity mismatch");
    }
    std::fs::rename(&temporary, &path)?;
    ui.note("Artifact verified and cached");
    Ok(path)
}
fn host_target() -> String {
    format!(
        "linux/{}",
        match std::env::consts::ARCH {
            "x86_64" | "amd64" => "x86_64",
            "aarch64" | "arm64" => "aarch64",
            value => value,
        }
    )
}
