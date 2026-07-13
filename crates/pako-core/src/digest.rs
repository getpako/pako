use std::{fmt, io::Read, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest as _, Sha256};

use crate::{Error, Result};

/// Canonical lowercase `sha256:<64 hex>` digest.
#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Sha256Digest([u8; 32]);

impl Sha256Digest {
    pub const EMPTY: Self = Self([
        0xe3, 0xb0, 0xc4, 0x42, 0x98, 0xfc, 0x1c, 0x14, 0x9a, 0xfb, 0xf4, 0xc8, 0x99, 0x6f, 0xb9,
        0x24, 0x27, 0xae, 0x41, 0xe4, 0x64, 0x9b, 0x93, 0x4c, 0xa4, 0x95, 0x99, 0x1b, 0x78, 0x52,
        0xb8, 0x55,
    ]);

    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn hex(&self) -> String {
        hex::encode(self.0)
    }

    pub fn calculate(bytes: &[u8]) -> Self {
        Self(Sha256::digest(bytes).into())
    }

    pub fn calculate_reader(mut reader: impl Read) -> Result<(Self, u64)> {
        let mut hash = Sha256::new();
        let mut total = 0_u64;
        let mut buffer = vec![0_u8; 128 * 1024];

        loop {
            let count = reader.read(&mut buffer).map_err(anyhow::Error::from)?;
            if count == 0 {
                break;
            }

            hash.update(&buffer[..count]);
            total = total
                .checked_add(count as u64)
                .ok_or_else(|| anyhow::anyhow!("input length overflow"))?;
        }

        Ok((Self(hash.finalize().into()), total))
    }
}

impl fmt::Debug for Sha256Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl fmt::Display for Sha256Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "sha256:{}", self.hex())
    }
}

impl FromStr for Sha256Digest {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self> {
        let encoded = value
            .strip_prefix("sha256:")
            .ok_or_else(|| Error::InvalidDigest(value.to_owned()))?;

        if encoded.len() != 64 || encoded.bytes().any(|byte| byte.is_ascii_uppercase()) {
            return Err(Error::InvalidDigest(value.to_owned()));
        }

        let decoded = hex::decode(encoded).map_err(|_| Error::InvalidDigest(value.to_owned()))?;
        let bytes: [u8; 32] = decoded
            .try_into()
            .map_err(|_| Error::InvalidDigest(value.to_owned()))?;

        Ok(Self(bytes))
    }
}

impl Serialize for Sha256Digest {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Sha256Digest {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::Sha256Digest;

    #[test]
    fn canonical_roundtrip() {
        let digest = Sha256Digest::calculate(b"pako");
        assert_eq!(digest.to_string().parse::<Sha256Digest>().unwrap(), digest);
    }
}
