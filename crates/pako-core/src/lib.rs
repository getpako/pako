//! Registry-independent package and installation primitives.

pub mod canonical;
pub mod digest;
pub mod error;
pub mod installer;
pub mod integrations;
pub mod layout;
pub mod manifest;
pub mod path;
pub mod payload;
pub mod receipt;
pub mod transaction;
pub mod verify;

pub use digest::Sha256Digest;
pub use error::{Error, Result};
pub use manifest::PackageManifest;
