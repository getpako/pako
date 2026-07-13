use serde::Serialize;
use serde_json::{Map, Value};

use crate::{Result, Sha256Digest};

/// Serialize JSON with recursively sorted object keys and no insignificant
/// whitespace.
pub fn to_vec<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    let value = serde_json::to_value(value)?;
    Ok(serde_json::to_vec(&canonicalize(value))?)
}

pub fn digest<T: Serialize>(value: &T) -> Result<Sha256Digest> {
    Ok(Sha256Digest::calculate(&to_vec(value)?))
}

fn canonicalize(value: Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut entries: Vec<_> = object.into_iter().collect();
            entries.sort_by(|left, right| left.0.as_bytes().cmp(right.0.as_bytes()));

            let canonical = entries
                .into_iter()
                .map(|(key, value)| (key, canonicalize(value)))
                .collect::<Map<_, _>>();
            Value::Object(canonical)
        }
        Value::Array(values) => Value::Array(values.into_iter().map(canonicalize).collect()),
        other => other,
    }
}
