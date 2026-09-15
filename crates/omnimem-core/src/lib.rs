//! OmniMem record types, validation and the v7 identity fields.
//!
//! This crate has no I/O. It holds what every other part of the binary has
//! to agree on: the namespaces and their key shapes, and the content hash the
//! v7 conformance spec (`docs/v7-change-spec.md`) makes normative.

pub mod classification;
pub mod embedding;
pub mod hash;
pub mod key;

pub use embedding::{EmbeddingError, TextEmbedder, VECTOR_DIM};
pub use hash::{content_hash, normalise_content};
pub use key::{KeyError, MemoryKey, Namespace};
