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

    /// Unit-length vectors, in input order, for text being stored (or
    /// compared with other stored text).
    fn embed_texts(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbeddingError>;

    /// Unit-length vectors for search queries, matched against stored text.
    /// The same as [`Self::embed_texts`] unless the model wants queries
    /// marked differently (bge's instruction, e5's `query:`).
    fn embed_queries(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        self.embed_texts(texts)
    }
}
