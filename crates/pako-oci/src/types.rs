use std::collections::BTreeMap;

use pako_core::Sha256Digest;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Descriptor {
    pub media_type: String,
    pub digest: Sha256Digest,
    pub size: u64,
    #[serde(default)]
    pub annotations: BTreeMap<String, String>,
    #[serde(default)]
    pub platform: Option<Platform>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Platform {
    pub architecture: String,
    pub os: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageManifest {
    pub schema_version: u32,
    pub media_type: String,
    pub config: Descriptor,
    pub layers: Vec<Descriptor>,
    #[serde(default)]
    pub annotations: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageIndex {
    pub schema_version: u32,
    pub media_type: String,
    pub manifests: Vec<Descriptor>,
    #[serde(default)]
    pub annotations: BTreeMap<String, String>,
}
