use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};

use sha2::{Digest as _, Sha256};

use crate::{error::IoContext, manifest::Compression, Error, Result, Sha256Digest};

const MAGIC: &[u8; 8] = b"PAKPACK1";
const VERSION: u16 = 1;
const HEADER_SIZE: u64 = 64;
const INDEX_ENTRY_SIZE: u64 = 64;

pub const SOFT_PACK_LIMIT: u64 = 16 * 1024 * 1024;
pub const HARD_PACK_LIMIT: u64 = 32 * 1024 * 1024;

#[derive(Debug, Clone, Copy)]
pub struct PackEntry {
    pub digest: Sha256Digest,
    pub data_offset: u64,
    pub stored_size: u64,
    pub raw_size: u64,
    pub compression: Compression,
}

#[derive(Debug)]
pub struct PackWriter {
    entries: Vec<PendingEntry>,
}

#[derive(Debug)]
struct PendingEntry {
    digest: Sha256Digest,
    stored: Vec<u8>,
    raw_size: u64,
    compression: Compression,
}

impl PackWriter {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Add a raw chunk to the pack.
    ///
    /// Duplicate digests are ignored because chunk identity is content based.
    pub fn add(&mut self, raw: &[u8]) -> Result<Sha256Digest> {
        let digest = Sha256Digest::calculate(raw);
        if self.entries.iter().any(|entry| entry.digest == digest) {
            return Ok(digest);
        }

        let compressed = zstd::stream::encode_all(raw, 10).map_err(anyhow::Error::from)?;
        let (stored, compression) = if compressed.len() < raw.len() {
            (compressed, Compression::Zstd)
        } else {
            (raw.to_vec(), Compression::Raw)
        };

        self.entries.push(PendingEntry {
            digest,
            stored,
            raw_size: raw.len() as u64,
            compression,
        });

        Ok(digest)
    }

    pub fn estimated_stored_size(&self) -> u64 {
        self.entries
            .iter()
            .map(|entry| entry.stored.len() as u64)
            .sum()
    }

    /// Write a deterministic pack and return its digest and index entries.
    pub fn finish(mut self, path: &Path) -> Result<(Sha256Digest, Vec<PackEntry>)> {
        self.entries.sort_by_key(|entry| entry.digest);

        let parent = path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("pack output has no parent"))?;
        std::fs::create_dir_all(parent).at(parent)?;

        let mut file = File::create(path).at(path)?;
        file.write_all(&[0_u8; 64]).at(path)?;

        let mut index = Vec::with_capacity(self.entries.len());
        for entry in self.entries {
            let data_offset = file.stream_position().at(path)?;
            file.write_all(&entry.stored).at(path)?;

            index.push(PackEntry {
                digest: entry.digest,
                data_offset,
                stored_size: entry.stored.len() as u64,
                raw_size: entry.raw_size,
                compression: entry.compression,
            });
        }

        let index_offset = file.stream_position().at(path)?;
        for entry in &index {
            write_index_entry(&mut file, entry).at(path)?;
        }

        let index_length = (index.len() as u64)
            .checked_mul(INDEX_ENTRY_SIZE)
            .ok_or_else(|| Error::InvalidPack("index overflow".into()))?;

        file.seek(SeekFrom::Start(0)).at(path)?;
        write_header(&mut file, index.len(), index_offset, index_length).at(path)?;
        file.sync_all().at(path)?;

        if file.metadata().at(path)?.len() > HARD_PACK_LIMIT {
            return Err(Error::InvalidPack("hard pack size limit exceeded".into()));
        }

        drop(file);
        let (digest, _) = Sha256Digest::calculate_reader(File::open(path).at(path)?)?;
        Ok((digest, index))
    }
}

impl Default for PackWriter {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug)]
pub struct PackReader {
    file: File,
    entries: BTreeMap<Sha256Digest, PackEntry>,
    file_len: u64,
}

/// Validate one cached immutable pack against its descriptor and pack format.
///
/// Invalid cache entries are removed and reported as missing so the caller can
/// download a clean copy under the same content digest.
pub fn validate_cached_pack(
    path: &Path,
    expected_digest: Sha256Digest,
    expected_size: u64,
) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }

    let metadata = std::fs::metadata(path).at(path)?;
    if metadata.len() != expected_size {
        log::warn!(
            "removing cached pack {} with unexpected size {} (expected {})",
            path.display(),
            metadata.len(),
            expected_size
        );
        std::fs::remove_file(path).at(path)?;
        return Ok(false);
    }

    let (actual_digest, actual_size) = Sha256Digest::calculate_reader(File::open(path).at(path)?)?;
    if actual_size != expected_size || actual_digest != expected_digest {
        log::warn!("removing corrupted cached pack {}", path.display());
        std::fs::remove_file(path).at(path)?;
        return Ok(false);
    }

    if let Err(error) = PackReader::open(path) {
        log::warn!("removing invalid cached pack {}: {error}", path.display());
        std::fs::remove_file(path).at(path)?;
        return Ok(false);
    }

    Ok(true)
}

impl PackReader {
    /// Open and validate the complete pack index before exposing any chunk.
    pub fn open(path: &Path) -> Result<Self> {
        let mut file = File::open(path).at(path)?;
        let file_len = file.metadata().at(path)?.len();

        if file_len < HEADER_SIZE {
            return Err(Error::InvalidPack("file is shorter than header".into()));
        }

        let (count, index_offset, index_length) = read_header(&mut file).at(path)?;
        let expected_index_length = u64::from(count)
            .checked_mul(INDEX_ENTRY_SIZE)
            .ok_or_else(|| Error::InvalidPack("index overflow".into()))?;

        if index_length != expected_index_length {
            return Err(Error::InvalidPack("index length mismatch".into()));
        }

        if index_offset.checked_add(index_length) != Some(file_len) {
            return Err(Error::InvalidPack("index does not end at EOF".into()));
        }

        file.seek(SeekFrom::Start(index_offset)).at(path)?;

        let mut entries = BTreeMap::new();
        let mut previous_digest = None;
        let mut ranges = Vec::new();

        for _ in 0..count {
            let entry = read_index_entry(&mut file).at(path)?;

            if previous_digest.is_some_and(|digest| digest >= entry.digest) {
                return Err(Error::InvalidPack("index is not strictly sorted".into()));
            }
            previous_digest = Some(entry.digest);

            let end = entry
                .data_offset
                .checked_add(entry.stored_size)
                .ok_or_else(|| Error::InvalidPack("payload range overflow".into()))?;
            if entry.data_offset < HEADER_SIZE || end > index_offset {
                return Err(Error::InvalidPack("payload outside data region".into()));
            }

            ranges.push((entry.data_offset, end));
            if entries.insert(entry.digest, entry).is_some() {
                return Err(Error::InvalidPack("duplicate digest".into()));
            }
        }

        ranges.sort_unstable();
        if ranges.windows(2).any(|window| window[0].1 > window[1].0) {
            return Err(Error::InvalidPack("overlapping payload ranges".into()));
        }

        Ok(Self {
            file,
            entries,
            file_len,
        })
    }

    pub fn entries(&self) -> impl Iterator<Item = &PackEntry> {
        self.entries.values()
    }

    pub fn file_len(&self) -> u64 {
        self.file_len
    }

    /// Extract one chunk and verify both its raw size and SHA-256 digest.
    pub fn extract(&mut self, digest: Sha256Digest, mut output: impl Write) -> Result<()> {
        let entry = *self
            .entries
            .get(&digest)
            .ok_or_else(|| Error::MissingChunk(digest.to_string()))?;

        self.file
            .seek(SeekFrom::Start(entry.data_offset))
            .map_err(anyhow::Error::from)?;

        let stored = std::io::Read::by_ref(&mut self.file).take(entry.stored_size);
        let mut hashing_output = HashingWriter::new(&mut output);

        match entry.compression {
            Compression::Raw => {
                let mut raw = stored.take(entry.raw_size.saturating_add(1));
                std::io::copy(&mut raw, &mut hashing_output).map_err(anyhow::Error::from)?;
            }
            Compression::Zstd => {
                let decoder =
                    zstd::stream::read::Decoder::new(stored).map_err(anyhow::Error::from)?;
                let mut raw = decoder.take(entry.raw_size.saturating_add(1));
                std::io::copy(&mut raw, &mut hashing_output).map_err(anyhow::Error::from)?;
            }
        }

        if hashing_output.written != entry.raw_size {
            return Err(Error::InvalidPack("decompressed size mismatch".into()));
        }

        let actual = Sha256Digest::from_bytes(hashing_output.hash.finalize().into());
        if actual != digest {
            return Err(Error::InvalidPack("chunk digest mismatch".into()));
        }

        Ok(())
    }
}

struct HashingWriter<W> {
    inner: W,
    hash: Sha256,
    written: u64,
}

impl<W> HashingWriter<W> {
    fn new(inner: W) -> Self {
        Self {
            inner,
            hash: Sha256::new(),
            written: 0,
        }
    }
}

impl<W: Write> Write for HashingWriter<W> {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let count = self.inner.write(buffer)?;
        self.hash.update(&buffer[..count]);
        self.written = self.written.saturating_add(count as u64);
        Ok(count)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

fn write_header(
    mut writer: impl Write,
    entry_count: usize,
    index_offset: u64,
    index_length: u64,
) -> std::io::Result<()> {
    let entry_count =
        u32::try_from(entry_count).map_err(|_| invalid_data("too many pack index entries"))?;

    writer.write_all(MAGIC)?;
    writer.write_all(&VERSION.to_le_bytes())?;
    writer.write_all(&0_u16.to_le_bytes())?;
    writer.write_all(&entry_count.to_le_bytes())?;
    writer.write_all(&index_offset.to_le_bytes())?;
    writer.write_all(&index_length.to_le_bytes())?;
    writer.write_all(&[0_u8; 32])
}

fn read_header(mut reader: impl Read) -> std::io::Result<(u32, u64, u64)> {
    let mut magic = [0_u8; 8];
    reader.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(invalid_data("invalid magic"));
    }

    let version = read_u16(&mut reader)?;
    let flags = read_u16(&mut reader)?;
    if version != VERSION || flags != 0 {
        return Err(invalid_data("unsupported pack header"));
    }

    let entry_count = read_u32(&mut reader)?;
    let index_offset = read_u64(&mut reader)?;
    let index_length = read_u64(&mut reader)?;

    let mut reserved = [0_u8; 32];
    reader.read_exact(&mut reserved)?;
    if reserved.iter().any(|byte| *byte != 0) {
        return Err(invalid_data("reserved bytes are non-zero"));
    }

    Ok((entry_count, index_offset, index_length))
}

fn write_index_entry(mut writer: impl Write, entry: &PackEntry) -> std::io::Result<()> {
    writer.write_all(entry.digest.as_bytes())?;
    writer.write_all(&entry.data_offset.to_le_bytes())?;
    writer.write_all(&entry.stored_size.to_le_bytes())?;
    writer.write_all(&entry.raw_size.to_le_bytes())?;
    writer.write_all(&[compression_code(entry.compression)])?;
    writer.write_all(&[0_u8; 7])
}

fn read_index_entry(mut reader: impl Read) -> std::io::Result<PackEntry> {
    let mut digest = [0_u8; 32];
    reader.read_exact(&mut digest)?;

    let data_offset = read_u64(&mut reader)?;
    let stored_size = read_u64(&mut reader)?;
    let raw_size = read_u64(&mut reader)?;

    let mut compression = [0_u8; 1];
    reader.read_exact(&mut compression)?;
    let compression = match compression[0] {
        0 => Compression::Raw,
        1 => Compression::Zstd,
        _ => return Err(invalid_data("unknown compression")),
    };

    let mut reserved = [0_u8; 7];
    reader.read_exact(&mut reserved)?;
    if reserved.iter().any(|byte| *byte != 0) {
        return Err(invalid_data("reserved bytes are non-zero"));
    }

    Ok(PackEntry {
        digest: Sha256Digest::from_bytes(digest),
        data_offset,
        stored_size,
        raw_size,
        compression,
    })
}

fn compression_code(compression: Compression) -> u8 {
    match compression {
        Compression::Raw => 0,
        Compression::Zstd => 1,
    }
}

fn read_u16(mut reader: impl Read) -> std::io::Result<u16> {
    let mut buffer = [0_u8; 2];
    reader.read_exact(&mut buffer)?;
    Ok(u16::from_le_bytes(buffer))
}

fn read_u32(mut reader: impl Read) -> std::io::Result<u32> {
    let mut buffer = [0_u8; 4];
    reader.read_exact(&mut buffer)?;
    Ok(u32::from_le_bytes(buffer))
}

fn read_u64(mut reader: impl Read) -> std::io::Result<u64> {
    let mut buffer = [0_u8; 8];
    reader.read_exact(&mut buffer)?;
    Ok(u64::from_le_bytes(buffer))
}

fn invalid_data(message: &'static str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message)
}
