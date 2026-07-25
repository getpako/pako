//! Safe, in-process extraction of package archives.
use crate::{
    manifest::ArchiveFormat,
    path::{validate_symlink_target, PackagePath},
    Error,
};
use flate2::read::GzDecoder;
use std::{
    fs::File,
    io::{Read, Seek},
    path::{Component, Path},
};
use tar::Archive;
use xz2::read::XzDecoder;

pub fn extract(
    path: &Path,
    format: ArchiveFormat,
    destination: &Path,
    strip: u32,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(destination)?;
    match format {
        ArchiveFormat::Tar => extract_tar(File::open(path)?, destination, strip),
        ArchiveFormat::TarGz => extract_tar(GzDecoder::new(File::open(path)?), destination, strip),
        ArchiveFormat::TarXz => extract_tar(XzDecoder::new(File::open(path)?), destination, strip),
        ArchiveFormat::TarZst => extract_tar(
            zstd::stream::read::Decoder::new(File::open(path)?)?,
            destination,
            strip,
        ),
        ArchiveFormat::Zip => extract_zip(File::open(path)?, destination, strip),
    }
}
fn extract_tar(reader: impl Read, destination: &Path, strip: u32) -> anyhow::Result<()> {
    let mut archive = Archive::new(reader);
    for item in archive.entries()? {
        let mut entry = item?;
        let kind = entry.header().entry_type();
        if !(kind.is_file() || kind.is_dir() || kind.is_symlink()) {
            return Err(Error::InvalidManifest("unsupported archive entry type".into()).into());
        }
        let Some(relative) = safe_path(&entry.path()?, strip)? else {
            continue;
        };
        let output = destination.join(relative.as_str());
        ensure_safe(destination, &output)?;
        if kind.is_symlink() {
            let target = entry
                .link_name()?
                .ok_or_else(|| Error::InvalidManifest("symlink has no target".into()))?;
            validate_symlink_target(
                &relative,
                target
                    .to_str()
                    .ok_or_else(|| Error::InvalidManifest("non UTF-8 symlink target".into()))?,
            )?;
        }
        entry.unpack(&output)?;
    }
    Ok(())
}
fn extract_zip(mut file: impl Read + Seek, destination: &Path, strip: u32) -> anyhow::Result<()> {
    let mut archive = zip::ZipArchive::new(&mut file)?;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        let Some(relative) = safe_path(Path::new(entry.name()), strip)? else {
            continue;
        };
        let output = destination.join(relative.as_str());
        ensure_safe(destination, &output)?;
        if entry.is_dir() {
            std::fs::create_dir_all(output)?;
            continue;
        }
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::io::copy(&mut entry, &mut File::create(output)?)?;
    }
    Ok(())
}
fn safe_path(path: &Path, strip: u32) -> anyhow::Result<Option<PackagePath>> {
    let components: Vec<_> = path.components().collect();
    if components
        .iter()
        .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(
            Error::InvalidManifest(format!("unsafe archive path: {}", path.display())).into(),
        );
    }
    if components.len() <= strip as usize {
        return Ok(None);
    }
    let value = components
        .into_iter()
        .skip(strip as usize)
        .map(|c| c.as_os_str().to_str().unwrap_or_default())
        .collect::<Vec<_>>()
        .join("/");
    Ok(Some(PackagePath::new(value)?))
}
fn ensure_safe(root: &Path, output: &Path) -> anyhow::Result<()> {
    let relative = output
        .strip_prefix(root)
        .map_err(|_| Error::InvalidManifest("archive escaped staging".into()))?;
    let mut current = root.to_owned();
    for component in relative
        .components()
        .take(relative.components().count().saturating_sub(1))
    {
        current.push(component);
        if current
            .symlink_metadata()
            .is_ok_and(|m| m.file_type().is_symlink())
        {
            return Err(Error::InvalidManifest("archive traverses symlink".into()).into());
        }
    }
    Ok(())
}
