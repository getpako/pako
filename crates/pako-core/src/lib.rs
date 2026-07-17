//! Registry-independent package and installation primitives.

pub mod canonical;
pub mod chunking;
pub mod digest;
pub mod error;
pub mod installer;
pub mod integrations;
pub mod layout;
pub mod lock;
pub mod manifest;
pub mod materialize;
pub mod object_store;
pub mod pack;
pub mod path;
pub mod planner;
pub mod receipt;
pub mod transaction;
pub mod verify;

pub use digest::Sha256Digest;
pub use error::{Error, Result};
pub use manifest::{PackIndex, PackageManifest};
