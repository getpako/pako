use std::io::{Read, Seek, SeekFrom};

use crate::{manifest::ChunkingProfile, Result};

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct ChunkBoundary {
    pub offset: u64,
    pub length: u32,
}

pub trait Chunker: Send + Sync {
    fn profile(&self) -> ChunkingProfile;

    fn boundaries(&self, reader: &mut (impl Read + Seek)) -> Result<Vec<ChunkBoundary>>;
}

/// Frozen content-defined chunking implementation used by
/// `pako-fastcdc-v1`.
///
/// The rolling Gear hash selects boundaries only. SHA-256 is calculated
/// separately and remains the chunk identity and integrity primitive.
#[derive(Debug, Default, Clone, Copy)]
pub struct PakoFastCdcV1;

impl Chunker for PakoFastCdcV1 {
    fn profile(&self) -> ChunkingProfile {
        ChunkingProfile::default()
    }

    fn boundaries(&self, reader: &mut (impl Read + Seek)) -> Result<Vec<ChunkBoundary>> {
        let profile = self.profile();
        let length = reader.seek(SeekFrom::End(0)).map_err(anyhow::Error::from)?;
        reader
            .seek(SeekFrom::Start(0))
            .map_err(anyhow::Error::from)?;

        if length == 0 {
            return Ok(Vec::new());
        }

        if length < u64::from(profile.small_file_threshold) {
            return Ok(vec![ChunkBoundary {
                offset: 0,
                length: u32::try_from(length).map_err(anyhow::Error::from)?,
            }]);
        }

        let maximum = usize::try_from(profile.maximum).map_err(anyhow::Error::from)?;
        let mut buffer = vec![0_u8; maximum];
        let mut boundaries = Vec::new();
        let mut offset = 0_u64;

        while offset < length {
            reader
                .seek(SeekFrom::Start(offset))
                .map_err(anyhow::Error::from)?;

            let remaining = length - offset;
            let wanted = usize::try_from(remaining.min(u64::from(profile.maximum)))
                .map_err(anyhow::Error::from)?;
            reader
                .read_exact(&mut buffer[..wanted])
                .map_err(anyhow::Error::from)?;

            let cut = find_cut(&buffer[..wanted], &profile);
            boundaries.push(ChunkBoundary {
                offset,
                length: u32::try_from(cut).map_err(anyhow::Error::from)?,
            });

            offset = offset
                .checked_add(cut as u64)
                .ok_or_else(|| anyhow::anyhow!("chunk offset overflow"))?;
        }

        Ok(boundaries)
    }
}

fn find_cut(data: &[u8], profile: &ChunkingProfile) -> usize {
    let minimum = usize::try_from(profile.minimum)
        .expect("chunking profile fits usize")
        .min(data.len());
    let average = usize::try_from(profile.average)
        .expect("chunking profile fits usize")
        .min(data.len());

    if data.len() <= minimum {
        return data.len();
    }

    let normal_mask = (1_u64 << 20) - 1;
    let strict_mask = (1_u64 << 21) - 1;
    let mut hash = 0_u64;

    for (index, byte) in data.iter().enumerate().skip(minimum) {
        hash = hash.wrapping_shl(1).wrapping_add(GEAR[usize::from(*byte)]);

        let mask = if index < average {
            strict_mask
        } else {
            normal_mask
        };

        if hash & mask == 0 {
            return index + 1;
        }
    }

    data.len()
}

const fn gear_table() -> [u64; 256] {
    let mut table = [0_u64; 256];
    let mut state = 0x243f_6a88_85a3_08d3_u64;
    let mut index = 0;

    while index < table.len() {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        table[index] = state.wrapping_mul(0x2545_f491_4f6c_dd1d);
        index += 1;
    }

    table
}

const GEAR: [u64; 256] = gear_table();

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::{Chunker, PakoFastCdcV1};

    #[test]
    fn boundaries_are_deterministic() {
        let data: Vec<_> = (0_i32..8 * 1024 * 1024)
            .map(|index| u8::try_from((index * 31).rem_euclid(256)).expect("remainder fits in u8"))
            .collect();
        let mut first = Cursor::new(&data);
        let mut second = Cursor::new(&data);

        assert_eq!(
            PakoFastCdcV1.boundaries(&mut first).unwrap(),
            PakoFastCdcV1.boundaries(&mut second).unwrap(),
        );
    }
}
