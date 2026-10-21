//! The seam between the store and whatever produces vectors.
//!
//! The store re-embeds on import and the engine embeds queries, but neither
//! should need ONNX Runtime to be tested. `omnimem-embed` implements this
//! trait for the real model; tests use a deterministic fake.

/// Every index is built for this many dimensions (all-MiniLM-L6-v2).
pub const VECTOR_DIM: usize = 384;

pub type EmbeddingError = Box<dyn std::error::Error + Send + Sync>;

pub trait TextEmbedder: Send + Sync {
    /// Length of every vector this embedder returns.
    fn dimension(&self) -> usize;

    /// Unit-length vectors, in input order.
    fn embed_texts(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbeddingError>;
}
