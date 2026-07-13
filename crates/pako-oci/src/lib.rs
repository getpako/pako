//! Minimal native OCI Distribution API implementation.
mod client;
mod reference;
mod types;

pub use client::{OciClient, Registry};
pub use reference::{OciReference, Reference};
pub use types::{Descriptor, ImageIndex, ImageManifest, Platform};
