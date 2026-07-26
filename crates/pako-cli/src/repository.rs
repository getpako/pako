use crate::{cli::Concurrency, output::Ui};
use fs2::FileExt;
use futures_util::StreamExt;
use pako_core::{
    installer::{InstallRequest, Installer},
    manifest::{Artifact, PackageManifest},
    receipt::{PackageState, Receipt},
    Sha256Digest,
};
use pako_trust::TrustedRepository;
use serde::Deserialize;
use std::{
    fs::File,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
use tokio::{fs::File as AsyncFile, io::AsyncWriteExt};
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
    let lock_path = path.with_extension("lock");
    let lock = File::create(&lock_path)?;
    lock.lock_exclusive()?;
    if valid_cached_artifact(&path, &plan.artifact)? {
        ui.note("Artifact cache hit");
        return Ok(path);
    }
    let temporary = unique_partial_path(&path);
    match &plan.artifact {
        Artifact::TufArchive { target, .. } => {
            if let Err(error) = plan.trusted.read_target_to_file(target, &temporary).await {
                let _ = std::fs::remove_file(&temporary);
                return Err(error);
            }
        }
        Artifact::ExternalArchive { urls, .. } => {
            let client = reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_mins(5))
                .redirect(reqwest::redirect::Policy::custom(|attempt| {
                    if attempt.previous().last().is_some_and(|previous| {
                        previous.scheme() == "https"
                            && attempt.url().scheme() == "http"
                            && !is_loopback(attempt.url())
                    }) {
                        return attempt.stop();
                    }
                    if attempt.previous().len() >= 5 {
                        return attempt.stop();
                    }
                    attempt.follow()
                }))
                .build()?;
            let mut errors = Vec::new();
            for url in urls {
                if !is_allowed_url(url) {
                    errors.push(format!("{url}: HTTPS required"));
                    continue;
                }
                let mirror_path = unique_partial_path(&path);
                match download_mirror(&client, url, &mirror_path, plan.artifact.size(), ui).await {
                    Ok(()) => {
                        let (digest, size) =
                            Sha256Digest::calculate_reader(File::open(&mirror_path)?)?;
                        if digest == plan.artifact.digest() && size == plan.artifact.size() {
                            std::fs::rename(&mirror_path, &temporary)?;
                            break;
                        }
                        errors.push(format!("{url}: digest or size mismatch"));
                        let _ = std::fs::remove_file(&mirror_path);
                    }
                    Err(error) => {
                        errors.push(format!("{url}: {error}"));
                        let _ = std::fs::remove_file(&mirror_path);
                    }
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
    lock.unlock()?;
    ui.note("Artifact verified and cached");
    Ok(path)
}

static PARTIAL_COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_partial_path(path: &Path) -> PathBuf {
    let number = PARTIAL_COUNTER.fetch_add(1, Ordering::Relaxed);
    path.with_extension(format!("partial-{}-{number}", std::process::id()))
}

fn valid_cached_artifact(path: &Path, artifact: &Artifact) -> anyhow::Result<bool> {
    if !path.is_file() {
        return Ok(false);
    }
    let (digest, size) = Sha256Digest::calculate_reader(File::open(path)?)?;
    if digest == artifact.digest() && size == artifact.size() {
        Ok(true)
    } else {
        let _ = std::fs::remove_file(path);
        Ok(false)
    }
}

fn is_loopback(url: &url::Url) -> bool {
    url.host_str()
        .is_some_and(|host| matches!(host, "localhost" | "127.0.0.1" | "::1" | "[::1]"))
}

fn is_allowed_url(url: &url::Url) -> bool {
    url.scheme() == "https" || (url.scheme() == "http" && is_loopback(url))
}

async fn download_mirror(
    client: &reqwest::Client,
    url: &url::Url,
    path: &Path,
    expected_size: u64,
    ui: &Ui,
) -> anyhow::Result<()> {
    let response = client.get(url.clone()).send().await?;
    let response = response.error_for_status()?;
    if response
        .content_length()
        .is_some_and(|size| size != expected_size)
    {
        anyhow::bail!("Content-Length does not match manifest");
    }
    let progress = ui.download_progress(expected_size);
    let mut stream = response.bytes_stream();
    let mut file = AsyncFile::create(path).await?;
    let mut downloaded = 0_u64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        downloaded = downloaded
            .checked_add(u64::try_from(chunk.len())?)
            .ok_or_else(|| anyhow::anyhow!("download size overflow"))?;
        if downloaded > expected_size {
            progress.finish_and_clear();
            anyhow::bail!("download exceeded manifest size");
        }
        file.write_all(&chunk).await?;
        progress.set_position(downloaded);
    }
    file.flush().await?;
    progress.finish_and_clear();
    if downloaded != expected_size {
        anyhow::bail!("download ended before manifest size");
    }
    Ok(())
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
