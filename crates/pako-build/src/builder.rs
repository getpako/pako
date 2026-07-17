use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::Mutex,
    time::Duration,
};

use futures_util::StreamExt;
use indicatif::{ProgressBar, ProgressStyle};
use log::{debug, info, trace, warn};
use pako_core::{
    canonical,
    chunking::{Chunker, PakoFastCdcV1},
    manifest::{
        ChunkLocation, ChunkRef, ChunkingProfile, DesktopEntry, Entry, Icon, Integrations,
        Launcher, PackDescriptor, PackIndex, PackageManifest, PackageMetadata, Policies,
        PACKAGE_MANIFEST_MEDIA_TYPE,
    },
    pack::{PackWriter, SOFT_PACK_LIMIT},
    path::{validate_symlink_target, PackagePath},
    verify::compute_tree_digest,
    Sha256Digest,
};
use sha2::{Digest as _, Sha256};
use tempfile::TempDir;
use tokio::io::AsyncWriteExt;
use walkdir::WalkDir;

use crate::{
    archive,
    recipe::{Assertion, Recipe, Source, Target, Transform},
    sandbox::Sandbox,
};

#[derive(Debug, Clone)]
pub(crate) struct BuildReport {
    pub package: String,
    pub version: String,
    pub target: String,
    pub package_manifest: PathBuf,
    pub pack_index: PathBuf,
    pub output: PathBuf,
}

#[derive(Debug)]
pub(crate) struct Builder {
    output: PathBuf,
    http: reqwest::Client,
    jobs: usize,
}

impl Builder {
    pub(crate) fn new(output: PathBuf, jobs: usize) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .timeout(Duration::from_hours(1))
            .user_agent(concat!("pako-build/", env!("CARGO_PKG_VERSION")))
            .build()?;

        Ok(Self { output, http, jobs })
    }

    pub(crate) async fn build(
        &self,
        recipe: &Recipe,
        target_name: &str,
    ) -> anyhow::Result<BuildReport> {
        recipe.validate()?;
        let target = recipe
            .targets
            .iter()
            .find(|candidate| candidate.platform == target_name)
            .ok_or_else(|| anyhow::anyhow!("target not found: {target_name}"))?;

        let workspace = BuildWorkspace::create()?;
        debug!("created temporary build workspace");
        trace!(
            "temporary build workspace: {}",
            workspace.temporary.path().display()
        );
        self.prepare_sources(recipe, target, &workspace).await?;

        if !target.build.scripts.is_empty() {
            info!("running source build stages");
            self.run_source_build(recipe, target, &workspace).await?;
        }

        info!("applying payload transforms and assertions");
        apply_transforms(&workspace.payload, &recipe.transforms)?;
        apply_transforms(&workspace.payload, &target.transforms)?;
        check_assertions(&workspace.payload, &recipe.assertions)?;
        check_assertions(&workspace.payload, &target.assertions)?;

        self.package_payload(recipe, target, &workspace.payload)
    }

    async fn prepare_sources(
        &self,
        recipe: &Recipe,
        target: &Target,
        workspace: &BuildWorkspace,
    ) -> anyhow::Result<()> {
        for (index, source) in target.sources.iter().enumerate() {
            let downloaded = workspace.sources.join(format!("source-{}", index + 1));
            info!("preparing source {}", source_filename(source));
            trace!("temporary source path: {}", downloaded.display());
            self.download_source(source, recipe.recipe_dir(), &downloaded)
                .await?;

            if let Some(format) = source.format.as_deref() {
                info!("extracting {format} archive");
                archive::extract(
                    &downloaded,
                    format,
                    &workspace.payload,
                    source.strip_components,
                )?;
            } else {
                let source_name = source_filename(source);
                let destination = source.destination.as_deref().unwrap_or(&source_name);
                info!("placing source at {destination}");
                let destination =
                    PackagePath::new(destination.to_owned())?.join_to(&workspace.payload);
                if let Some(parent) = destination.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::copy(&downloaded, destination)?;
            }
        }

        Ok(())
    }

    async fn run_source_build(
        &self,
        recipe: &Recipe,
        target: &Target,
        workspace: &BuildWorkspace,
    ) -> anyhow::Result<()> {
        let image = target
            .build
            .environment
            .clone()
            .ok_or_else(|| anyhow::anyhow!("source build requires an environment"))?;
        info!("using build environment {image}");
        let sandbox = Sandbox {
            image,
            network: target.build.network,
            timeout: Duration::from_secs(target.build.timeout_seconds.unwrap_or(3600)),
        };
        let environment = build_environment(target, self.jobs);

        for (phase, script) in target.build.scripts.phases() {
            let Some(script) = script else {
                continue;
            };

            sandbox
                .run(
                    phase,
                    script,
                    recipe.recipe_dir(),
                    &workspace.payload,
                    &workspace.build,
                    &workspace.destination,
                    &environment,
                )
                .await?;
        }

        if workspace.destination.read_dir()?.next().is_none() {
            anyhow::bail!("source build produced an empty PAKO_DESTDIR");
        }

        std::fs::remove_dir_all(&workspace.payload)?;
        std::fs::rename(&workspace.destination, &workspace.payload)?;
        Ok(())
    }

    fn package_payload(
        &self,
        recipe: &Recipe,
        target: &Target,
        payload: &Path,
    ) -> anyhow::Result<BuildReport> {
        let version = format!("{}-{}", recipe.package.version, recipe.package.release);
        let output = self
            .output
            .join(&recipe.package.name)
            .join(&version)
            .join(target.platform.replace('/', "_"));
        if output.exists() {
            anyhow::bail!(
                "build output already exists; remove it before rebuilding: {}",
                output.display()
            );
        }
        info!("packaging payload into {}", output.display());
        std::fs::create_dir_all(&output)?;

        let chunks_directory = output.join("chunks");
        std::fs::create_dir_all(&chunks_directory)?;

        let mut entries = scan_tree(payload, &chunks_directory, self.jobs)?;
        info!("scanned {} payload entries", entries.len());
        entries.sort_by(|left, right| left.path().cmp(right.path()));

        let tree_digest = compute_tree_digest(&entries);
        let manifest = PackageManifest {
            schema_version: 1,
            media_type: PACKAGE_MANIFEST_MEDIA_TYPE.into(),
            package: recipe.package.name.clone(),
            upstream_version: recipe.package.version.clone(),
            release: recipe.package.release,
            target: target.platform.clone(),
            metadata: PackageMetadata {
                display_name: recipe.metadata.display_name.clone(),
                summary: recipe.metadata.summary.clone(),
                description: recipe.metadata.description.clone(),
                vendor: recipe.metadata.vendor.clone(),
                homepage: recipe.metadata.homepage.clone(),
                license: recipe.metadata.license.clone(),
            },
            chunking: ChunkingProfile::default(),
            tree_digest,
            entries,
            integrations: convert_integrations(recipe)?,
            policies: Policies {
                payload_mutation: "deny".into(),
                self_update: "external".into(),
                user_data: "external".into(),
            },
        };
        manifest.validate()?;

        let manifest_bytes = canonical::to_vec(&manifest)?;
        let manifest_digest = Sha256Digest::calculate(&manifest_bytes);
        let manifest_path = output.join("package-manifest.json");
        std::fs::write(&manifest_path, &manifest_bytes)?;

        let index = build_packs(
            &manifest,
            &chunks_directory,
            &output.join("packs"),
            manifest_digest,
            self.jobs,
        )?;
        index.validate_against(&manifest)?;

        let index_path = output.join("pack-index.json");
        std::fs::write(&index_path, canonical::to_vec(&index)?)?;
        std::fs::remove_dir_all(chunks_directory)?;

        Ok(BuildReport {
            package: recipe.package.name.clone(),
            version,
            target: target.platform.clone(),
            package_manifest: manifest_path,
            pack_index: index_path,
            output,
        })
    }

    async fn download_source(
        &self,
        source: &Source,
        recipe_directory: &Path,
        destination: &Path,
    ) -> anyhow::Result<()> {
        let expected: Sha256Digest = source.hash.parse()?;
        let partial = destination.with_extension("partial");

        if let Some(path) = &source.path {
            info!("copying local source {path}");
            self.copy_local_source(path, recipe_directory, expected, &partial)
                .await?;
            tokio::fs::rename(&partial, destination).await?;
            return Ok(());
        }

        for url in &source.urls {
            let source_name = source_filename_from_url(url);
            let result = self
                .download_mirror(&source_name, url, expected, &partial)
                .await;
            match result {
                Ok(()) => {
                    tokio::fs::rename(&partial, destination).await?;
                    return Ok(());
                }
                Err(error) => {
                    let _ = tokio::fs::remove_file(&partial).await;
                    warn!("source mirror failed for {source_name}: {error:#}");
                }
            }
        }

        anyhow::bail!("all source mirrors failed")
    }

    async fn copy_local_source(
        &self,
        relative_path: &str,
        recipe_directory: &Path,
        expected_digest: Sha256Digest,
        destination: &Path,
    ) -> anyhow::Result<()> {
        let recipe_directory = std::fs::canonicalize(recipe_directory)?;
        let source = std::fs::canonicalize(recipe_directory.join(relative_path))?;
        if !source.starts_with(&recipe_directory) {
            anyhow::bail!("local source is outside the recipe directory");
        }

        let (digest, _) = Sha256Digest::calculate_reader(std::fs::File::open(&source)?)?;
        if digest != expected_digest {
            anyhow::bail!("source digest mismatch: expected {expected_digest}, got {digest}");
        }

        tokio::fs::copy(source, destination).await?;
        Ok(())
    }

    async fn download_mirror(
        &self,
        source_name: &str,
        url: &str,
        expected_digest: Sha256Digest,
        destination: &Path,
    ) -> anyhow::Result<()> {
        info!("downloading {source_name} from {url}");
        let response = self.http.get(url).send().await?.error_for_status()?;
        trace!(
            "response content length for {source_name}: {:?}",
            response.content_length()
        );
        let progress = download_progress(source_name, response.content_length());
        let mut stream = response.bytes_stream();
        let mut output = tokio::fs::File::create(destination).await?;
        let mut hash = Sha256::new();

        let result = async {
            while let Some(chunk) = stream.next().await {
                let chunk = chunk?;
                output.write_all(&chunk).await?;
                hash.update(&chunk);
                progress.inc(chunk.len() as u64);
            }

            output.sync_all().await?;

            let actual = Sha256Digest::from_bytes(hash.finalize().into());
            if actual != expected_digest {
                anyhow::bail!("source digest mismatch: expected {expected_digest}, got {actual}");
            }

            Ok(())
        }
        .await;

        match result {
            Ok(()) => pako_log::finish_progress(&progress, format!("Downloaded {source_name}")),
            Err(_) => {
                pako_log::abandon_progress(&progress, format!("Download failed for {source_name}"));
            }
        }
        result
    }
}

fn source_filename(source: &Source) -> String {
    source.path.as_deref().map_or_else(
        || source_filename_from_url(&source.urls[0]),
        |path| {
            Path::new(path)
                .file_name()
                .and_then(|name| name.to_str())
                .map_or_else(|| "download".into(), ToOwned::to_owned)
        },
    )
}

fn source_filename_from_url(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|url| {
            url.path_segments()?
                .rfind(|segment| !segment.is_empty())
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| "download".into())
}

fn download_progress(source_name: &str, length: Option<u64>) -> ProgressBar {
    let progress = pako_log::add_progress(match length {
        Some(length) => ProgressBar::new(length),
        None => ProgressBar::new_spinner(),
    });
    let style = match length {
        Some(_) => ProgressStyle::with_template(
            "{spinner:.green} {msg} [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}, {eta})",
        ),
        None => ProgressStyle::with_template("{spinner:.green} {msg} {bytes} ({bytes_per_sec})"),
    }
    .expect("download progress templates are valid")
    .progress_chars("#>-");
    progress.set_style(style);
    progress.set_message(format!("downloading {source_name}"));
    progress.enable_steady_tick(Duration::from_millis(100));
    progress
}

#[cfg(test)]
mod tests {
    use indicatif::ProgressBar;
    use tempfile::TempDir;

    use super::{compress_packs, scan_tree, source_filename_from_url, PlannedPack};
    use pako_core::{manifest::Entry, pack::PackReader, Sha256Digest};

    #[test]
    fn derives_filename_from_source_url() {
        assert_eq!(
            source_filename_from_url(
                "https://downloads.example.org/releases/tool-1.2.3.tar.gz?mirror=1"
            ),
            "tool-1.2.3.tar.gz"
        );
    }

    #[test]
    fn uses_generic_name_when_url_has_no_filename() {
        assert_eq!(source_filename_from_url("not a URL"), "download");
    }

    #[test]
    fn compresses_separate_packs_in_parallel() {
        let temporary = TempDir::new().unwrap();
        let chunks = temporary.path().join("chunks");
        let packs = temporary.path().join("packs");
        std::fs::create_dir_all(&chunks).unwrap();
        std::fs::create_dir_all(&packs).unwrap();

        let first = Sha256Digest::calculate(b"first chunk");
        let second = Sha256Digest::calculate(b"second chunk");
        std::fs::write(chunks.join(first.hex()), b"first chunk").unwrap();
        std::fs::write(chunks.join(second.hex()), b"second chunk").unwrap();

        let progress = ProgressBar::hidden();
        let completed = compress_packs(
            vec![
                PlannedPack {
                    ordinal: 0,
                    chunks: vec![first],
                },
                PlannedPack {
                    ordinal: 1,
                    chunks: vec![second],
                },
            ],
            &chunks,
            &packs,
            2,
            &progress,
        )
        .unwrap();

        assert_eq!(completed.len(), 2);
        for pack in completed {
            PackReader::open(&packs.join(format!("{}.pakopack", pack.digest.hex()))).unwrap();
        }
    }

    #[test]
    fn scans_multiple_files_with_parallel_workers() {
        let temporary = TempDir::new().unwrap();
        let payload = temporary.path().join("payload");
        let chunks = temporary.path().join("chunks");
        std::fs::create_dir_all(&payload).unwrap();
        std::fs::create_dir_all(&chunks).unwrap();
        std::fs::write(payload.join("first"), b"first file").unwrap();
        std::fs::write(payload.join("second"), b"second file").unwrap();

        let entries = scan_tree(&payload, &chunks, 2).unwrap();

        assert_eq!(entries.len(), 2);
        assert!(entries
            .iter()
            .all(|entry| matches!(entry, Entry::File { .. })));
        assert_eq!(std::fs::read_dir(&chunks).unwrap().count(), 2);
    }
}

#[derive(Debug)]
struct BuildWorkspace {
    temporary: TempDir,
    sources: PathBuf,
    payload: PathBuf,
    build: PathBuf,
    destination: PathBuf,
}

impl BuildWorkspace {
    fn create() -> anyhow::Result<Self> {
        let temporary = TempDir::new()?;
        let sources = temporary.path().join("sources");
        let payload = temporary.path().join("payload");
        let build = temporary.path().join("build");
        let destination = temporary.path().join("dest");

        for path in [&sources, &payload, &build, &destination] {
            std::fs::create_dir_all(path)?;
        }

        Ok(Self {
            temporary,
            sources,
            payload,
            build,
            destination,
        })
    }
}

fn build_environment(target: &Target, jobs: usize) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("PAKO_RECIPE_DIR".into(), "/pako/recipe".into()),
        ("PAKO_SOURCE_DIR".into(), "/pako/source".into()),
        ("PAKO_BUILD_DIR".into(), "/pako/build".into()),
        ("PAKO_DESTDIR".into(), "/pako/dest".into()),
        ("PAKO_TARGET".into(), target.platform.clone()),
        ("PAKO_JOBS".into(), jobs.to_string()),
        ("HOME".into(), "/tmp/home".into()),
        ("SOURCE_DATE_EPOCH".into(), "0".into()),
    ])
}

#[derive(Debug)]
struct FileScanTask {
    path: PathBuf,
    package_path: PackagePath,
    mode: u16,
    size: u64,
}

fn scan_tree(root: &Path, chunks_directory: &Path, jobs: usize) -> anyhow::Result<Vec<Entry>> {
    let mut entries = Vec::new();
    let mut files = Vec::new();

    for item in WalkDir::new(root)
        .follow_links(false)
        .min_depth(1)
        .sort_by_file_name()
    {
        let item = item?;
        let path = item.path();
        let relative = path
            .strip_prefix(root)?
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("non UTF-8 path"))?;
        let relative = PackagePath::new(relative.to_owned())?;
        let metadata = std::fs::symlink_metadata(path)?;
        let mode = (metadata.permissions().mode() & 0o777) as u16;

        if metadata.is_dir() {
            entries.push(Entry::Directory {
                path: relative,
                mode,
            });
        } else if metadata.file_type().is_symlink() {
            let target = std::fs::read_link(path)?
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("non UTF-8 symlink target"))?
                .to_owned();
            validate_symlink_target(&relative, &target)?;
            entries.push(Entry::Symlink {
                path: relative,
                target,
            });
        } else if metadata.is_file() {
            files.push(FileScanTask {
                path: path.to_owned(),
                package_path: relative,
                mode,
                size: metadata.len(),
            });
        } else {
            anyhow::bail!("unsupported filesystem entry: {}", path.display());
        }
    }

    let worker_count = jobs.max(1).min(files.len().max(1));
    info!(
        "scanning {} files with {worker_count} worker(s)",
        files.len()
    );
    let file_count = files.len();
    let progress = scan_progress(file_count);
    let scanned = scan_files(files, chunks_directory, worker_count, &progress);
    match scanned {
        Ok(scanned) => {
            pako_log::finish_progress(&progress, format!("Scanned {file_count} files"));
            entries.extend(scanned);
        }
        Err(error) => {
            pako_log::abandon_progress(&progress, "File scan failed");
            return Err(error);
        }
    }

    Ok(entries)
}

fn scan_files(
    files: Vec<FileScanTask>,
    chunks_directory: &Path,
    worker_count: usize,
    progress: &ProgressBar,
) -> anyhow::Result<Vec<Entry>> {
    let queue = Mutex::new(VecDeque::from(files));
    let results = Mutex::new(Vec::new());

    std::thread::scope(|scope| {
        for _ in 0..worker_count {
            scope.spawn(|| loop {
                let Some(file) = queue
                    .lock()
                    .expect("file scan queue lock poisoned")
                    .pop_front()
                else {
                    return;
                };
                let result = scan_file(file, chunks_directory);
                progress.inc(1);
                results
                    .lock()
                    .expect("file scan result lock poisoned")
                    .push(result);
            });
        }
    });

    results
        .into_inner()
        .expect("file scan result lock poisoned")
        .into_iter()
        .collect()
}

fn scan_progress(file_count: usize) -> ProgressBar {
    let progress = pako_log::add_progress(ProgressBar::new(file_count as u64));
    let style = ProgressStyle::with_template(
        "{spinner:.green} {msg} [{bar:40.cyan/blue}] {pos}/{len} files ({per_sec})",
    )
    .expect("file scan progress template is valid")
    .progress_chars("#>-");
    progress.set_style(style);
    progress.set_message("scanning payload");
    progress.enable_steady_tick(Duration::from_millis(100));
    progress
}

fn scan_file(task: FileScanTask, chunks_directory: &Path) -> anyhow::Result<Entry> {
    let mut file = File::open(&task.path)?;
    let boundaries = PakoFastCdcV1.boundaries(&mut file)?;
    let mut chunks = Vec::with_capacity(boundaries.len());
    let mut file_hash = Sha256::new();

    for boundary in boundaries {
        file.seek(SeekFrom::Start(boundary.offset))?;
        let mut bytes = vec![0_u8; boundary.length as usize];
        file.read_exact(&mut bytes)?;
        file_hash.update(&bytes);

        let digest = Sha256Digest::calculate(&bytes);
        store_chunk(chunks_directory, digest, &bytes)?;

        chunks.push(ChunkRef {
            digest,
            size: boundary.length,
        });
    }

    let digest = if task.size == 0 {
        Sha256Digest::EMPTY
    } else {
        Sha256Digest::from_bytes(file_hash.finalize().into())
    };

    Ok(Entry::File {
        path: task.package_path,
        mode: task.mode,
        size: task.size,
        digest,
        chunks,
    })
}

fn store_chunk(chunks_directory: &Path, digest: Sha256Digest, bytes: &[u8]) -> anyhow::Result<()> {
    let path = chunks_directory.join(digest.hex());
    match File::options().write(true).create_new(true).open(&path) {
        Ok(mut file) => {
            file.write_all(bytes)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

fn build_packs(
    manifest: &PackageManifest,
    chunks_directory: &Path,
    packs_directory: &Path,
    manifest_digest: Sha256Digest,
    jobs: usize,
) -> anyhow::Result<PackIndex> {
    std::fs::create_dir_all(packs_directory)?;

    let required: BTreeSet<_> = manifest
        .entries
        .iter()
        .filter_map(|entry| match entry {
            Entry::File { chunks, .. } => Some(chunks),
            Entry::Directory { .. } | Entry::Symlink { .. } => None,
        })
        .flatten()
        .map(|chunk| chunk.digest)
        .collect();

    let planned = plan_packs(required, chunks_directory)?;
    let worker_count = jobs.max(1).min(planned.len().max(1));
    info!(
        "compressing {} packs with {worker_count} worker(s)",
        planned.len()
    );
    let pack_count = planned.len();
    let progress = pack_progress(pack_count);
    let compressed = compress_packs(
        planned,
        chunks_directory,
        packs_directory,
        worker_count,
        &progress,
    );
    let completed = match compressed {
        Ok(completed) => {
            pako_log::finish_progress(&progress, format!("Compressed {pack_count} packs"));
            completed
        }
        Err(error) => {
            pako_log::abandon_progress(&progress, "Pack compression failed");
            return Err(error);
        }
    };

    let mut packs = BTreeMap::new();
    let mut locations = BTreeMap::new();
    for pack in completed {
        packs.insert(pack.digest, PackDescriptor { size: pack.size });
        for entry in pack.entries {
            locations.insert(
                entry.digest,
                ChunkLocation {
                    pack: pack.digest,
                    offset: entry.data_offset,
                    stored_size: entry.stored_size,
                    raw_size: entry.raw_size,
                    compression: entry.compression,
                },
            );
        }
    }

    Ok(PackIndex {
        schema: "pako.pack-index.v1".into(),
        package_manifest_digest: manifest_digest,
        packs,
        chunks: locations,
    })
}

#[derive(Debug)]
struct PlannedPack {
    ordinal: usize,
    chunks: Vec<Sha256Digest>,
}

#[derive(Debug)]
struct CompletedPack {
    digest: Sha256Digest,
    size: u64,
    entries: Vec<pako_core::pack::PackEntry>,
}

fn plan_packs(
    required: BTreeSet<Sha256Digest>,
    chunks_directory: &Path,
) -> anyhow::Result<Vec<PlannedPack>> {
    let mut planned = Vec::new();
    let mut chunks = Vec::new();
    let mut size = 0_u64;

    for digest in required {
        let chunk_size = std::fs::metadata(chunks_directory.join(digest.hex()))?.len();
        if !chunks.is_empty() && size + chunk_size > SOFT_PACK_LIMIT {
            planned.push(PlannedPack {
                ordinal: planned.len(),
                chunks,
            });
            chunks = Vec::new();
            size = 0;
        }
        chunks.push(digest);
        size += chunk_size;
    }

    if !chunks.is_empty() {
        planned.push(PlannedPack {
            ordinal: planned.len(),
            chunks,
        });
    }

    Ok(planned)
}

fn compress_packs(
    planned: Vec<PlannedPack>,
    chunks_directory: &Path,
    packs_directory: &Path,
    worker_count: usize,
    progress: &ProgressBar,
) -> anyhow::Result<Vec<CompletedPack>> {
    let queue = Mutex::new(VecDeque::from(planned));
    let results = Mutex::new(Vec::new());

    std::thread::scope(|scope| {
        for _ in 0..worker_count {
            scope.spawn(|| loop {
                let Some(pack) = queue.lock().expect("pack queue lock poisoned").pop_front() else {
                    return;
                };
                let result = compress_pack(pack, chunks_directory, packs_directory);
                progress.inc(1);
                results
                    .lock()
                    .expect("pack result lock poisoned")
                    .push(result);
            });
        }
    });

    results
        .into_inner()
        .expect("pack result lock poisoned")
        .into_iter()
        .collect()
}

fn pack_progress(pack_count: usize) -> ProgressBar {
    let progress = pako_log::add_progress(ProgressBar::new(pack_count as u64));
    let style = ProgressStyle::with_template(
        "{spinner:.green} {msg} [{bar:40.cyan/blue}] {pos}/{len} packs ({per_sec})",
    )
    .expect("pack compression progress template is valid")
    .progress_chars("#>-");
    progress.set_style(style);
    progress.set_message("compressing packs");
    progress.enable_steady_tick(Duration::from_millis(100));
    progress
}

fn compress_pack(
    planned: PlannedPack,
    chunks_directory: &Path,
    directory: &Path,
) -> anyhow::Result<CompletedPack> {
    let mut writer = PackWriter::new();
    for digest in planned.chunks {
        writer.add(&std::fs::read(chunks_directory.join(digest.hex()))?)?;
    }

    let temporary = directory.join(format!("building-{}.pakopack", planned.ordinal));
    let (pack_digest, entries) = writer.finish(&temporary)?;
    let final_path = directory.join(format!("{}.pakopack", pack_digest.hex()));

    if final_path.exists() {
        std::fs::remove_file(&temporary)?;
    } else {
        std::fs::rename(&temporary, &final_path)?;
    }

    let size = std::fs::metadata(&final_path)?.len();
    Ok(CompletedPack {
        digest: pack_digest,
        size,
        entries,
    })
}

fn apply_transforms(root: &Path, transforms: &[Transform]) -> anyhow::Result<()> {
    for transform in transforms {
        apply_transform(root, transform)?;
    }
    Ok(())
}

fn apply_transform(root: &Path, transform: &Transform) -> anyhow::Result<()> {
    match transform {
        Transform::Remove { paths, required } => {
            for path in paths {
                let path = payload_path(root, path)?;
                if path.symlink_metadata().is_err() {
                    if *required {
                        anyhow::bail!("required path is missing: {}", path.display());
                    }
                    continue;
                }

                let metadata = std::fs::symlink_metadata(&path)?;
                if metadata.is_dir() {
                    std::fs::remove_dir_all(&path)?;
                } else {
                    std::fs::remove_file(&path)?;
                }
            }
        }
        Transform::Chmod { path, mode } => {
            let path = payload_path(root, path)?;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(parse_mode(mode)?))?;
        }
        Transform::Move { from, to } => {
            let from = payload_path(root, from)?;
            let to = payload_path(root, to)?;
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::rename(from, to)?;
        }
        Transform::Copy { from, to } => {
            let from = payload_path(root, from)?;
            let to = payload_path(root, to)?;
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(from, to)?;
        }
        Transform::Write {
            path,
            mode,
            content,
        } => {
            let path = payload_path(root, path)?;
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, content)?;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(parse_mode(mode)?))?;
        }
        Transform::Symlink { path, target } => {
            let package_path = PackagePath::new(path.clone())?;
            validate_symlink_target(&package_path, target)?;
            let path = package_path.join_to(root);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::os::unix::fs::symlink(target, path)?;
        }
    }

    Ok(())
}

fn check_assertions(root: &Path, assertions: &[Assertion]) -> anyhow::Result<()> {
    for assertion in assertions {
        match assertion {
            Assertion::Path {
                path,
                kind,
                executable,
            } => {
                let path = payload_path(root, path)?;
                let metadata = std::fs::symlink_metadata(&path)?;
                let kind_matches = match kind.as_str() {
                    "file" => metadata.is_file(),
                    "directory" => metadata.is_dir(),
                    "symlink" => metadata.file_type().is_symlink(),
                    "file-or-symlink" => metadata.is_file() || metadata.file_type().is_symlink(),
                    other => anyhow::bail!("unsupported assertion kind {other}"),
                };

                if !kind_matches {
                    anyhow::bail!("path assertion failed for {}", path.display());
                }
                if *executable && metadata.permissions().mode() & 0o111 == 0 {
                    anyhow::bail!("path is not executable: {}", path.display());
                }
            }
            Assertion::Absent { path } => {
                let path = payload_path(root, path)?;
                if path.symlink_metadata().is_ok() {
                    anyhow::bail!("path must be absent: {}", path.display());
                }
            }
        }
    }

    Ok(())
}

fn payload_path(root: &Path, value: &str) -> anyhow::Result<PathBuf> {
    Ok(PackagePath::new(value.to_owned())?.join_to(root))
}

fn parse_mode(value: &str) -> anyhow::Result<u32> {
    let value = value
        .strip_prefix("0o")
        .or_else(|| value.strip_prefix('0'))
        .unwrap_or(value);
    let mode = u32::from_str_radix(value, 8)?;

    if mode & !0o777 != 0 {
        anyhow::bail!("forbidden permission bits in mode {value}");
    }

    Ok(mode)
}

fn convert_integrations(recipe: &Recipe) -> anyhow::Result<Integrations> {
    let launchers = recipe
        .integrations
        .launchers
        .iter()
        .map(|launcher| {
            Ok(Launcher {
                name: launcher.name.clone(),
                target: PackagePath::new(launcher.target.clone())?,
                arguments: launcher.arguments.clone(),
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    let desktop_entries = recipe
        .integrations
        .desktop_entries
        .iter()
        .map(|entry| DesktopEntry {
            id: entry.id.clone(),
            name: entry.name.clone(),
            exec: entry.exec.clone(),
            icon: entry.icon.clone(),
            terminal: entry.terminal,
            categories: entry.categories.clone(),
        })
        .collect();

    let icons = recipe
        .integrations
        .icons
        .iter()
        .map(|icon| {
            Ok(Icon {
                name: icon.name.clone(),
                source: PackagePath::new(icon.source.clone())?,
                context: icon.context.clone(),
                size: icon.size.clone(),
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    Ok(Integrations {
        launchers,
        desktop_entries,
        icons,
    })
}
