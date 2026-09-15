//! Sentence embeddings on ONNX Runtime.
//!
//! A port of OmniMem 6.7's Python engine (`mcp_server/memory/onnx_embedding.py`)
//! that must stay vector-equivalent to it: the same maintainer-exported ONNX
//! graph, the same `tokenizers` library (the Python engine wraps this crate),
//! the same truncation, padding, pooling and L2 normalisation. A store built by
//! 6.7 imports without re-embedding because of that, and the tests in
//! `tests/reference_vectors.rs` hold the engine to vectors the Python engine
//! wrote.
//!
//! Model files come from a local directory or the Hugging Face cache, at a
//! pinned revision for the default model so an upstream re-export can never
//! move vectors under a live store.

mod engine;
mod error;
mod model;

pub use engine::{Embedder, Pooling};
pub use error::EmbedError;
pub use model::{
    DEFAULT_MAX_SEQ_LENGTH, DEFAULT_MODEL, DEFAULT_MODEL_REVISION, DEFAULT_ONNX_FILE, EmbedConfig,
};
