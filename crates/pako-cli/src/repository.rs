use std::{
    collections::{BTreeSet, VecDeque},
    fs::File,
    path::{Path, PathBuf},
    str::FromStr,
    sync::Mutex,
};

use futures_util::{stream, StreamExt};
use indicatif::ProgressBar;

use pako_core::{
    installer::{InstallRequest, Installer},
    lock::DigestLock,
    manifest::{Entry, PackIndex, PackageManifest},
    pack::{validate_cached_pack, PackReader},
    planner,
    receipt::{PackageState, Receipt},
    Sha256Digest,
};
use pako_oci::{ImageIndex, ImageManifest, OciClient, OciReference, Registry};
use pako_trust::TrustedRepository;
use serde::Deserialize;
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

        let config: Self = serde_json::from_reader(File::open(path)?)?;
        pako_core::path::validate_managed_name(&config.name, "repository name")?;
        Ok(config)
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
    pub(crate) download: planner::DownloadPlan,
    pub(crate) available_chunks: usize,
    pub(crate) total_chunks: usize,
    pub(crate) reusable_bytes: u64,
    pub(crate) installed_bytes: u64,
    pub(crate) current_version: Option<String>,
    pub(crate) up_to_date: bool,
    pub(crate) launcher_count: usize,
    pub(crate) desktop_entry_count: usize,
    pub(crate) icon_count: usize,
    cpu_jobs: usize,
    download_jobs: usize,
    client: OciClient,
    platform_reference: OciReference,
    pack_index: PackIndex,
    platform_digest: Sha256Digest,
    package_manifest_digest: Sha256Digest,
    pack_index_digest: Sha256Digest,
}

impl RemoteInstallPlan {
    pub(crate) fn version(&self) -> String {
        format!("{}-{}", self.manifest.upstream_version, self.manifest.release)
    }

    pub(crate) fn cache_growth(&self) -> u64 {
        self.download
            .network_bytes
            .saturating_add(self.download.required_raw_bytes)
    }

    pub(crate) fn data_growth(&self) -> u64 {
        self.installed_bytes
    }
}

#[derive(Debug)]
pub(crate) enum InstallOutcome {
    Installed(Receipt),
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
    pako_core::path::validate_channel(channel)?;
    let repository = RepositoryConfig::load(installer.layout())?;
    log::info!(
        "resolving {package} from repository {} channel {channel}",
        repository.name
    );

    let catalog_step = ui.spinner("Refreshing trusted repository metadata");
    let catalog = refresh_catalog(installer, &repository).await?;
    catalog_step.finish("Repository metadata verified");

    let target = host_target()?;
    let release = catalog.resolve(package, &target, channel)?;
    let index_reference =
        OciReference::from_str(&release.oci)?.with_digest(release.manifest_digest);
    let mut client = OciClient::new()?;
    if repository.allow_insecure_http {
        ensure_loopback_registry(&index_reference.registry)?;
        client = client.insecure_http();
    }

    let package_step = ui.spinner("Resolving package metadata");
    let platform = resolve_platform(&client, &index_reference, &target).await?;
    let platform_reference = index_reference.with_digest(platform.digest);
    let oci_manifest = fetch_image_manifest(&client, &platform_reference).await?;
    let package_descriptor = oci_manifest
        .layers
        .iter()
        .find(|descriptor| {
            descriptor.media_type == pako_core::manifest::PACKAGE_MANIFEST_MEDIA_TYPE
        })
        .ok_or_else(|| anyhow::anyhow!("package manifest layer is missing"))?;
    let index_descriptor = oci_manifest
        .layers
        .iter()
        .find(|descriptor| descriptor.media_type == pako_core::manifest::PACK_INDEX_MEDIA_TYPE)
        .ok_or_else(|| anyhow::anyhow!("pack index layer is missing"))?;
    let (manifest, pack_index) = fetch_package_metadata(
        installer,
        &client,
        &platform_reference,
        package_descriptor,
        index_descriptor,
    )
    .await?;
    manifest.validate()?;
    pack_index.validate_against(&manifest)?;
    package_step.finish("Package metadata verified");

    let cache_store = installer.store().clone();
    let cache_packs_root = installer.layout().packs();
    let cache_index = pack_index.clone();
    let cache_jobs = concurrency.cpu_jobs;
    let (available, cached_packs) = tokio::task::spawn_blocking(move || {
        let available = collect_available_chunks(&cache_store, &cache_index, cache_jobs, ui)?;
        let cached_packs = collect_cached_packs(
            &cache_packs_root,
            &cache_index,
            &available,
            cache_jobs,
            ui,
        )?;
        Ok::<_, anyhow::Error>((available, cached_packs))
    })
    .await??;
    let download = planner::plan(&pack_index, &available, &cached_packs)?;
    let total_raw_bytes = pack_index
        .chunks
        .values()
        .try_fold(0_u64, |total, location| total.checked_add(location.raw_size))
        .ok_or_else(|| anyhow::anyhow!("package chunk size overflow"))?;
    let installed_bytes = manifest
        .entries
        .iter()
        .filter_map(|entry| match entry {
            Entry::File { size, .. } => Some(*size),
            Entry::Directory { .. } | Entry::Symlink { .. } => None,
        })
        .try_fold(0_u64, |total, size| total.checked_add(size))
        .ok_or_else(|| anyhow::anyhow!("installed size overflow"))?;

    let state_path = installer.layout().package_state(package)?;
    let current = if state_path.exists() {
        let state = PackageState::load(&state_path)?;
        let receipt = Receipt::load(
            &installer
                .layout()
                .version_record(package, &state.active)?,
        )?;
        Some((state, receipt))
    } else {
        None
    };
    let current_version = current.as_ref().map(|(state, _)| state.active.clone());
    let up_to_date = current
        .as_ref()
        .is_some_and(|(_, receipt)| receipt.oci_manifest_digest == platform.digest);

    Ok(RemoteInstallPlan {
        operation,
        repository: repository.name,
        channel: channel.to_owned(),
        target,
        launcher_count: manifest.integrations.launchers.len(),
        desktop_entry_count: manifest.integrations.desktop_entries.len(),
        icon_count: manifest.integrations.icons.len(),
        cpu_jobs: concurrency.cpu_jobs,
        download_jobs: concurrency.download_jobs,
        total_chunks: pack_index.chunks.len(),
        available_chunks: available.len(),
        reusable_bytes: total_raw_bytes.saturating_sub(download.required_raw_bytes),
        installed_bytes,
        current_version,
        up_to_date,
        manifest,
        download,
        client,
        platform_reference,
        pack_index,
        platform_digest: platform.digest,
        package_manifest_digest: package_descriptor.digest,
        pack_index_digest: index_descriptor.digest,
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

    if !plan.download.missing_chunks.is_empty() {
        let progress = if plan.download.network_bytes > 0 {
            ui.byte_progress("Downloading package data", plan.download.network_bytes)
        } else {
            ProgressBar::hidden()
        };
        download_missing_chunks(
            installer,
            &plan.client,
            &plan.platform_reference,
            &plan.download,
            plan.cpu_jobs,
            plan.download_jobs,
            ui,
            &progress,
        )
        .await?;
        if plan.download.network_bytes > 0 {
            progress.finish_with_message("Downloaded package data");
        }
    } else {
        log::info!("all required chunks are already available locally");
    }

    let step = ui.spinner("Materializing and verifying package");
    let request = InstallRequest {
        repository: plan.repository,
        oci_manifest_digest: plan.platform_digest,
        package_manifest_digest: plan.package_manifest_digest,
        pack_index_digest: plan.pack_index_digest,
        channel: plan.channel,
    };
    let local_installer = installer.clone();
    let manifest = plan.manifest;
    let pack_index = plan.pack_index;
    let receipt = tokio::task::spawn_blocking(move || {
        local_installer.install(&manifest, &pack_index, &request)
    })
    .await??;
    step.finish("Package materialized, verified, and activated");
    Ok(InstallOutcome::Installed(receipt))
}

fn ensure_loopback_registry(registry: &str) -> anyhow::Result<()> {
    let host = registry
        .strip_prefix('[')
        .and_then(|value| value.split_once(']').map(|(host, _)| host))
        .unwrap_or_else(|| registry.split_once(':').map_or(registry, |(host, _)| host));

    if matches!(host, "localhost" | "127.0.0.1" | "::1") {
        Ok(())
    } else {
        anyhow::bail!("allowInsecureHttp is permitted only for a loopback registry, got {registry}")
    }
}

async fn refresh_catalog(
    installer: &Installer,
    repository: &RepositoryConfig,
) -> anyhow::Result<pako_trust::ReleaseCatalog> {
    let trusted = TrustedRepository::new(
        repository.root.clone(),
        repository.metadata_url.clone(),
        repository.targets_url.clone(),
        installer.layout().state.join("tuf").join(&repository.name),
    );
    trusted.refresh_catalog().await
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

async fn fetch_package_metadata(
    installer: &Installer,
    client: &OciClient,
    reference: &OciReference,
    package_descriptor: &pako_oci::Descriptor,
    index_descriptor: &pako_oci::Descriptor,
) -> anyhow::Result<(PackageManifest, PackIndex)> {
    let directory = installer.layout().cache.join("metadata");
    let lock_directory = installer.layout().locks().join("metadata");
    std::fs::create_dir_all(&directory)?;

    let package_path = directory.join(package_descriptor.digest.hex());
    let index_path = directory.join(index_descriptor.digest.hex());
    tokio::try_join!(
        fetch_cached_blob(
            client,
            reference,
            package_descriptor.digest,
            package_descriptor.size,
            &package_path,
            &lock_directory,
        ),
        fetch_cached_blob(
            client,
            reference,
            index_descriptor.digest,
            index_descriptor.size,
            &index_path,
            &lock_directory,
        ),
    )?;

    let package_manifest = serde_json::from_reader(File::open(package_path)?)?;
    let pack_index = serde_json::from_reader(File::open(index_path)?)?;
    Ok((package_manifest, pack_index))
}

async fn fetch_cached_blob(
    client: &OciClient,
    reference: &OciReference,
    digest: Sha256Digest,
    size: u64,
    path: &Path,
    lock_directory: &Path,
) -> anyhow::Result<()> {
    let lock_directory = lock_directory.to_owned();
    let lock = tokio::task::spawn_blocking(move || DigestLock::acquire(&lock_directory, digest))
        .await??;

    if validate_cached_blob(path, digest, size)? {
        log::debug!("using verified cached metadata blob {digest}");
        drop(lock);
        return Ok(());
    }

    client.fetch_blob(reference, digest, path).await?;
    drop(lock);
    Ok(())
}

fn validate_cached_blob(
    path: &Path,
    expected_digest: Sha256Digest,
    expected_size: u64,
) -> anyhow::Result<bool> {
    if !path.exists() {
        return Ok(false);
    }

    let metadata = std::fs::metadata(path)?;
    let (actual_digest, actual_size) = Sha256Digest::calculate_reader(File::open(path)?)?;
    if metadata.len() == expected_size
        && actual_size == expected_size
        && actual_digest == expected_digest
    {
        return Ok(true);
    }

    log::warn!("removing corrupted metadata cache entry {}", path.display());
    std::fs::remove_file(path)?;
    Ok(false)
}

fn collect_available_chunks(
    store: &pako_core::object_store::ObjectStore,
    index: &PackIndex,
    worker_limit: usize,
    ui: Ui,
) -> anyhow::Result<BTreeSet<Sha256Digest>> {
    let digests = index.chunks.keys().copied().collect::<Vec<_>>();
    let worker_count = worker_limit.max(1).min(digests.len().max(1));
    let progress = ui.item_progress("Checking local chunk cache", digests.len(), "chunks");
    let queue = Mutex::new(VecDeque::from(digests));
    let results = Mutex::new(Vec::new());

    std::thread::scope(|scope| {
        for _ in 0..worker_count {
            let store = store.clone();
            let queue = &queue;
            let results = &results;
            let progress = progress.clone();
            scope.spawn(move || loop {
                let Some(digest) = queue
                    .lock()
                    .expect("chunk check queue lock poisoned")
                    .pop_front()
                else {
                    return;
                };
                let result = store.contains(digest).map(|present| (digest, present));
                progress.inc(1);
                results
                    .lock()
                    .expect("chunk check result lock poisoned")
                    .push(result);
            });
        }
    });

    let checked = results
        .into_inner()
        .expect("chunk check result lock poisoned")
        .into_iter()
        .collect::<std::result::Result<Vec<_>, _>>()?;
    progress.finish_with_message("Checked local chunk cache");
    Ok(checked
        .into_iter()
        .filter_map(|(digest, present)| present.then_some(digest))
        .collect())
}

fn collect_cached_packs(
    packs_root: &Path,
    index: &PackIndex,
    available_chunks: &BTreeSet<Sha256Digest>,
    worker_limit: usize,
    ui: Ui,
) -> anyhow::Result<BTreeSet<Sha256Digest>> {
    let required = index
        .chunks
        .iter()
        .filter(|(digest, _)| !available_chunks.contains(digest))
        .map(|(_, location)| location.pack)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|digest| (digest, index.packs[&digest].size))
        .collect::<Vec<_>>();

    if required.is_empty() {
        return Ok(BTreeSet::new());
    }

    let progress = ui.item_progress("Checking local pack cache", required.len(), "packs");
    let worker_count = worker_limit.max(1).min(required.len());
    let queue = Mutex::new(VecDeque::from(required));
    let results = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..worker_count {
            let queue = &queue;
            let results = &results;
            let progress = progress.clone();
            let packs_root = packs_root.to_owned();
            scope.spawn(move || loop {
                let Some((digest, size)) = queue
                    .lock()
                    .expect("pack check queue lock poisoned")
                    .pop_front()
                else {
                    return;
                };
                let path = packs_root.join(format!("{}.pakopack", digest.hex()));
                let result = validate_cached_pack(&path, digest, size)
                    .map(|present| (digest, present));
                progress.inc(1);
                results
                    .lock()
                    .expect("pack check result lock poisoned")
                    .push(result);
            });
        }
    });

    let checked = results
        .into_inner()
        .expect("pack check result lock poisoned")
        .into_iter()
        .collect::<std::result::Result<Vec<_>, _>>()?;
    progress.finish_with_message("Checked local pack cache");
    Ok(checked
        .into_iter()
        .filter_map(|(digest, present)| present.then_some(digest))
        .collect())
}

async fn download_missing_chunks(
    installer: &Installer,
    client: &OciClient,
    reference: &OciReference,
    plan: &planner::DownloadPlan,
    cpu_jobs: usize,
    download_jobs: usize,
    ui: Ui,
    progress: &ProgressBar,
) -> anyhow::Result<()> {
    let packs_root = installer.layout().packs();
    let lock_root = installer.layout().locks().join("packs");
    let downloads = stream::iter(
        plan.packs
            .iter()
            .filter(|pack| !pack.cached)
            .cloned()
            .map(|planned_pack| {
                let pack_path =
                    packs_root.join(format!("{}.pakopack", planned_pack.digest.hex()));
                let lock_root = lock_root.clone();
                let progress = progress.clone();
                async move {
                    let digest = planned_pack.digest;
                    let size = planned_pack.size;
                    let lock = tokio::task::spawn_blocking(move || {
                        DigestLock::acquire(&lock_root, digest)
                    })
                    .await??;

                    if validate_cached_pack(&pack_path, digest, size)? {
                        log::info!("pack {digest} was completed by another Pako process");
                        progress.inc(size);
                        drop(lock);
                        return Ok::<(), anyhow::Error>(());
                    }

                    client
                        .fetch_blob_with_progress(reference, digest, &pack_path, &progress)
                        .await?;
                    drop(lock);
                    Ok::<(), anyhow::Error>(())
                }
            }),
    )
    .buffer_unordered(download_jobs.max(1));

    futures_util::pin_mut!(downloads);
    while let Some(result) = downloads.next().await {
        result?;
    }

    let import_progress = ui.item_progress(
        "Importing missing chunks",
        plan.missing_chunks.len(),
        "chunks",
    );
    let packs = plan.packs.clone();
    let store = installer.store().clone();
    let packs_root_for_extract = packs_root.clone();
    tokio::task::spawn_blocking(move || {
        extract_packs_parallel(packs, packs_root_for_extract, store, cpu_jobs, import_progress)
    })
    .await??;

    Ok(())
}

fn extract_packs_parallel(
    packs: Vec<planner::PlannedPack>,
    packs_root: PathBuf,
    store: pako_core::object_store::ObjectStore,
    jobs: usize,
    progress: ProgressBar,
) -> anyhow::Result<()> {
    if packs.is_empty() {
        progress.finish_with_message("No chunks need importing");
        return Ok(());
    }

    let worker_count = jobs.max(1).min(packs.len());
    log::info!(
        "extracting {} pack(s) with {worker_count} worker(s)",
        packs.len()
    );
    let queue = Mutex::new(VecDeque::from(
        packs.into_iter().enumerate().collect::<Vec<_>>(),
    ));
    let results = Mutex::new(Vec::new());

    std::thread::scope(|scope| {
        for _ in 0..worker_count {
            let queue = &queue;
            let results = &results;
            let packs_root = &packs_root;
            let store = &store;
            let progress = progress.clone();
            scope.spawn(move || loop {
                let Some((index, planned_pack)) = queue
                    .lock()
                    .expect("pack extraction queue lock poisoned")
                    .pop_front()
                else {
                    return;
                };

                let result = extract_pack(packs_root, store, &planned_pack, &progress);
                results
                    .lock()
                    .expect("pack extraction result lock poisoned")
                    .push((index, result));
            });
        }
    });

    let mut completed = results
        .into_inner()
        .expect("pack extraction result lock poisoned");
    completed.sort_by_key(|(index, _)| *index);
    for (_, result) in completed {
        result?;
    }
    progress.finish_with_message("Imported missing chunks");
    Ok(())
}

fn extract_pack(
    packs_root: &Path,
    store: &pako_core::object_store::ObjectStore,
    planned_pack: &planner::PlannedPack,
    progress: &ProgressBar,
) -> anyhow::Result<()> {
    let pack_path = packs_root.join(format!("{}.pakopack", planned_pack.digest.hex()));
    let mut reader = PackReader::open(&pack_path)?;
    for digest in &planned_pack.needed_chunks {
        let mut temporary = store.create_temp_for(*digest)?;
        reader.extract(*digest, &mut temporary)?;
        store.publish_verified(temporary, *digest)?;
        progress.inc(1);
    }
    Ok(())
}

fn host_target() -> anyhow::Result<String> {
    let architecture = match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        other => anyhow::bail!("unsupported architecture {other}"),
    };

    Ok(format!("linux/{architecture}"))
}

fn normalize_architecture(value: &str) -> &str {
    match value {
        "amd64" => "x86_64",
        "arm64" => "aarch64",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::ensure_loopback_registry;

    #[test]
    fn insecure_http_is_limited_to_loopback_registries() {
        for registry in ["localhost:5000", "127.0.0.1:5000", "[::1]:5000"] {
            assert!(ensure_loopback_registry(registry).is_ok());
        }
        assert!(ensure_loopback_registry("registry.example.com").is_err());
    }
}
