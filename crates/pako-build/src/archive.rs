use std::{
    fs::File,
    io::{Read, Seek},
    path::{Component, Path, PathBuf},
};

use flate2::read::GzDecoder;
use indicatif::{ProgressBar, ProgressStyle};
use tar::Archive;

/// Extract a supported archive into `destination` while rejecting path
/// traversal, special files and symlink-based escapes.
pub(crate) fn extract(
    path: &Path,
    format: &str,
    destination: &Path,
    strip_components: u32,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(destination)?;

    match format {
        "tar.gz" => extract_tar(
            GzDecoder::new(File::open(path)?),
            destination,
            strip_components,
        ),
        "tar" => extract_tar(File::open(path)?, destination, strip_components),
        "zip" => extract_zip(File::open(path)?, destination, strip_components),
        other => anyhow::bail!("unsupported archive format {other}"),
    }
}

fn extract_tar(reader: impl Read, destination: &Path, strip_components: u32) -> anyhow::Result<()> {
    let mut archive = Archive::new(reader);
    let progress = extraction_progress(None, "extracting archive");

    let result = (|| {
        for entry in archive.entries()? {
            let mut entry = entry?;
            let entry_type = entry.header().entry_type();

            if !(entry_type.is_file() || entry_type.is_dir() || entry_type.is_symlink()) {
                anyhow::bail!("unsupported archive entry type");
            }

            let archive_path = entry.path()?.into_owned();
            let relative = strip_path(&archive_path, strip_components)?;
            if relative.as_os_str().is_empty() {
                continue;
            }

            let output = destination.join(&relative);
            ensure_inside(destination, &output)?;
            ensure_no_symlink_ancestor(destination, &output)?;

            if entry_type.is_symlink() {
                let target = entry
                    .link_name()?
                    .ok_or_else(|| anyhow::anyhow!("symlink entry has no target"))?;
                validate_symlink_target(&relative, &target)?;
            }

            entry.unpack(&output)?;
            progress.inc(1);
        }
        Ok::<_, anyhow::Error>(())
    })();

    match result {
        Ok(()) => {
            progress.finish_with_message("extracted archive");
            Ok(())
        }
        Err(error) => {
            progress.abandon_with_message("archive extraction failed");
            Err(error)
        }
    }
}

fn extract_zip(
    mut file: impl Read + Seek,
    destination: &Path,
    strip_components: u32,
) -> anyhow::Result<()> {
    let mut archive = zip::ZipArchive::new(&mut file)?;
    let progress = extraction_progress(Some(archive.len()), "extracting archive");

    let result = (|| {
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index)?;
            let archive_path = entry
                .enclosed_name()
                .ok_or_else(|| anyhow::anyhow!("unsafe ZIP path"))?
                .clone();
            let relative = strip_path(&archive_path, strip_components)?;
            progress.inc(1);
            if relative.as_os_str().is_empty() {
                continue;
            }

            let output = destination.join(&relative);
            ensure_inside(destination, &output)?;
            ensure_no_symlink_ancestor(destination, &output)?;

            if entry.is_dir() {
                std::fs::create_dir_all(&output)?;
                continue;
            }

            if let Some(parent) = output.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::io::copy(&mut entry, &mut File::create(&output)?)?;
        }
        Ok::<_, anyhow::Error>(())
    })();

    match result {
        Ok(()) => {
            progress.finish_with_message("extracted archive");
            Ok(())
        }
        Err(error) => {
            progress.abandon_with_message("archive extraction failed");
            Err(error)
        }
    }
}

fn extraction_progress(total: Option<usize>, message: &str) -> ProgressBar {
    let progress = total.map_or_else(ProgressBar::new_spinner, |total| {
        ProgressBar::new(total as u64)
    });
    let style = match total {
        Some(_) => ProgressStyle::with_template(
            "{spinner:.green} {msg} [{bar:40.cyan/blue}] {pos}/{len} entries ({per_sec})",
        ),
        None => ProgressStyle::with_template("{spinner:.green} {msg} {pos} entries ({per_sec})"),
    }
    .expect("archive extraction progress template is valid")
    .progress_chars("#>-");
    progress.set_style(style);
    progress.set_message(message.to_owned());
    progress.enable_steady_tick(std::time::Duration::from_millis(100));
    progress
}

fn strip_path(path: &Path, count: u32) -> anyhow::Result<PathBuf> {
    let components: Vec<_> = path.components().collect();
    if components.iter().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        anyhow::bail!("unsafe archive path: {}", path.display());
    }

    Ok(components.into_iter().skip(count as usize).collect())
}

fn ensure_inside(root: &Path, path: &Path) -> anyhow::Result<()> {
    if !path.starts_with(root) {
        anyhow::bail!("archive entry escapes destination");
    }
    Ok(())
}

fn ensure_no_symlink_ancestor(root: &Path, path: &Path) -> anyhow::Result<()> {
    let relative = path.strip_prefix(root)?;
    let component_count = relative.components().count();
    let mut current = root.to_owned();

    for component in relative
        .components()
        .take(component_count.saturating_sub(1))
    {
        current.push(component);
        if let Ok(metadata) = std::fs::symlink_metadata(&current) {
            if metadata.file_type().is_symlink() {
                anyhow::bail!(
                    "archive entry traverses symlink ancestor: {}",
                    current.display()
                );
            }
        }
    }

    Ok(())
}

fn validate_symlink_target(link: &Path, target: &Path) -> anyhow::Result<()> {
    if target.is_absolute() {
        anyhow::bail!("absolute symlink target is not allowed");
    }

    let mut depth = link
        .parent()
        .map_or(0, |parent| parent.components().count());
    for component in target.components() {
        match component {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir if depth > 0 => depth -= 1,
            _ => anyhow::bail!("symlink target escapes archive root"),
        }
    }

    Ok(())
}
