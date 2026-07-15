use std::{collections::BTreeSet, fs::File, path::PathBuf, str::FromStr, time::Duration};

use futures_util::{stream, StreamExt};
use indicatif::{ProgressBar, ProgressStyle};

use pako_core::{
    installer::{InstallRequest, Installer},
    manifest::{PackIndex, PackageManifest},
    pack::PackReader,
    planner,
};
use pako_oci::{ImageIndex, ImageManifest, OciClient, OciReference, Registry};
use pako_trust::TrustedRepository;
use serde::Deserialize;
use url::Url;

#[derive(Debug, Deserialize)]
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

pub(crate) async fn install_remote(
    installer: &Installer,
    package: &str,
    channel: &str,
    dry_run: bool,
) -> anyhow::Result<()> {
    let repository = RepositoryConfig::load(installer.layout())?;
    let catalog = refresh_catalog(installer, &repository).await?;
    let target = host_target()?;
    let release = catalog.resolve(package, &target, channel)?;

    let index_reference =
        OciReference::from_str(&release.oci)?.with_digest(release.manifest_digest);
    let mut client = OciClient::new()?;
    if repository.allow_insecure_http {
        ensure_loopback_registry(&index_reference.registry)?;
        client = client.insecure_http();
    }
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

    let progress = download_progress(package_descriptor.size + index_descriptor.size);
    let (package_manifest, pack_index) = fetch_package_metadata(
        installer,
        &client,
        &platform_reference,
        package_descriptor,
        index_descriptor,
        &progress,
    )
    .await?;

    package_manifest.validate()?;
    pack_index.validate_against(&package_manifest)?;

    let available = collect_available_chunks(installer, &pack_index)?;
    let plan = planner::plan(&pack_index, &available)?;
    print_plan(&package_manifest, &plan, available.len());

    if dry_run {
        return Ok(());
    }

    progress.set_length(progress.position() + plan.network_bytes);
    download_missing_chunks(installer, &client, &platform_reference, &plan, &progress).await?;
    progress.finish_with_message("downloaded package blobs");

    let request = InstallRequest {
        repository: repository.name,
        oci_manifest_digest: platform.digest,
        package_manifest_digest: package_descriptor.digest,
        pack_index_digest: index_descriptor.digest,
        channel: channel.to_owned(),
    };
    let receipt = installer.install(&package_manifest, &pack_index, &request)?;

    println!(
        "installed {} {}-{}",
        receipt.package, receipt.upstream_version, receipt.release
    );
    Ok(())
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
    progress: &ProgressBar,
) -> anyhow::Result<(PackageManifest, PackIndex)> {
    let directory = installer.layout().cache.join("metadata");
    std::fs::create_dir_all(&directory)?;

    let package_path = directory.join(package_descriptor.digest.hex());
    let index_path = directory.join(index_descriptor.digest.hex());
    tokio::try_join!(
        client.fetch_blob_with_progress(
            reference,
            package_descriptor.digest,
            &package_path,
            progress,
        ),
        client.fetch_blob_with_progress(reference, index_descriptor.digest, &index_path, progress),
    )?;

    let package_manifest = serde_json::from_reader(File::open(package_path)?)?;
    let pack_index = serde_json::from_reader(File::open(index_path)?)?;
    Ok((package_manifest, pack_index))
}

fn collect_available_chunks(
    installer: &Installer,
    index: &PackIndex,
) -> anyhow::Result<BTreeSet<pako_core::Sha256Digest>> {
    let mut available = BTreeSet::new();

    for digest in index.chunks.keys() {
        if installer.store().contains(*digest)? {
            available.insert(*digest);
        }
    }

    Ok(available)
}

async fn download_missing_chunks(
    installer: &Installer,
    client: &OciClient,
    reference: &OciReference,
    plan: &pako_core::planner::DownloadPlan,
    progress: &ProgressBar,
) -> anyhow::Result<()> {
    let jobs = std::thread::available_parallelism().map_or(1, usize::from);
    let downloads = stream::iter(plan.packs.iter().cloned().map(|planned_pack| {
        let pack_path = installer
            .layout()
            .packs()
            .join(format!("{}.pakopack", planned_pack.digest.hex()));
        async move {
            client
                .fetch_blob_with_progress(reference, planned_pack.digest, &pack_path, progress)
                .await
        }
    }))
    .buffer_unordered(jobs);

    futures_util::pin_mut!(downloads);
    while let Some(result) = downloads.next().await {
        result?;
    }

    for planned_pack in &plan.packs {
        let pack_path = installer
            .layout()
            .packs()
            .join(format!("{}.pakopack", planned_pack.digest.hex()));
        let mut reader = PackReader::open(&pack_path)?;
        for digest in &planned_pack.needed_chunks {
            let mut temporary = installer
                .store()
                .create_temp(installer.layout().cache.as_path())?;
            reader.extract(*digest, &mut temporary)?;
            installer.store().import(temporary.reopen()?, *digest)?;
        }
    }

    Ok(())
}

fn download_progress(total: u64) -> ProgressBar {
    let progress = ProgressBar::new(total);
    let style = ProgressStyle::with_template(
        "{spinner:.green} downloading package [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}, {eta})",
    )
    .expect("package download progress template is valid")
    .progress_chars("#>-");
    progress.set_style(style);
    progress.enable_steady_tick(Duration::from_millis(100));
    progress
}

fn print_plan(
    manifest: &PackageManifest,
    plan: &pako_core::planner::DownloadPlan,
    available_chunks: usize,
) {
    println!(
        "{} {}-{}: download {}, reuse {} chunks",
        manifest.package,
        manifest.upstream_version,
        manifest.release,
        format_size(plan.network_bytes),
        available_chunks,
    );
}

fn format_size(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;

    if bytes >= GIB {
        format_scaled_size(bytes, GIB, "GiB")
    } else if bytes >= MIB {
        format_scaled_size(bytes, MIB, "MiB")
    } else if bytes >= KIB {
        format_scaled_size(bytes, KIB, "KiB")
    } else {
        format!("{bytes} B")
    }
}

fn format_scaled_size(bytes: u64, unit: u64, suffix: &str) -> String {
    let whole = bytes / unit;
    let tenths = (bytes % unit) * 10 / unit;
    format!("{whole}.{tenths} {suffix}")
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
    use super::{ensure_loopback_registry, format_size};

    #[test]
    fn insecure_http_is_limited_to_loopback_registries() {
        for registry in ["localhost:5000", "127.0.0.1:5000", "[::1]:5000"] {
            assert!(ensure_loopback_registry(registry).is_ok());
        }
        assert!(ensure_loopback_registry("registry.example.com").is_err());
    }

    #[test]
    fn download_sizes_use_appropriate_units() {
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(2 * 1024), "2.0 KiB");
        assert_eq!(format_size(3 * 1024 * 1024), "3.0 MiB");
        assert_eq!(format_size(4 * 1024 * 1024 * 1024), "4.0 GiB");
    }
}
