use std::collections::{BTreeMap, BTreeSet};

use crate::{manifest::PackIndex, Result, Sha256Digest};

#[derive(Debug, Clone)]
pub struct DownloadPlan {
    pub missing_chunks: BTreeSet<Sha256Digest>,
    pub packs: Vec<PlannedPack>,
    pub required_raw_bytes: u64,
    pub network_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct PlannedPack {
    pub digest: Sha256Digest,
    pub size: u64,
    pub needed_chunks: Vec<Sha256Digest>,
    pub useful_stored_bytes: u64,
}

impl DownloadPlan {
    pub fn overfetch_bytes(&self) -> u64 {
        let useful_bytes = self.packs.iter().map(|pack| pack.useful_stored_bytes).sum();
        self.network_bytes.saturating_sub(useful_bytes)
    }
}

/// Select the minimum set of immutable packs needed for missing chunks.
pub fn plan(index: &PackIndex, locally_available: &BTreeSet<Sha256Digest>) -> Result<DownloadPlan> {
    let missing_chunks: BTreeSet<_> = index
        .chunks
        .keys()
        .filter(|digest| !locally_available.contains(digest))
        .copied()
        .collect();

    let mut by_pack = BTreeMap::<Sha256Digest, PlannedPack>::new();
    let mut required_raw_bytes = 0_u64;

    for digest in &missing_chunks {
        let location = &index.chunks[digest];
        required_raw_bytes = required_raw_bytes
            .checked_add(location.raw_size)
            .ok_or_else(|| anyhow::anyhow!("plan size overflow"))?;

        let pack_size = index.packs[&location.pack].size;
        let planned = by_pack.entry(location.pack).or_insert_with(|| PlannedPack {
            digest: location.pack,
            size: pack_size,
            needed_chunks: Vec::new(),
            useful_stored_bytes: 0,
        });

        planned.needed_chunks.push(*digest);
        planned.useful_stored_bytes = planned
            .useful_stored_bytes
            .checked_add(location.stored_size)
            .ok_or_else(|| anyhow::anyhow!("plan stored size overflow"))?;
    }

    let packs: Vec<_> = by_pack.into_values().collect();
    let network_bytes = packs.iter().map(|pack| pack.size).sum();

    Ok(DownloadPlan {
        missing_chunks,
        packs,
        required_raw_bytes,
        network_bytes,
    })
}
