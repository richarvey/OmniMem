use thiserror::Error;

#[derive(Debug, Error)]
pub enum EmbedError {
    /// A file the engine can't run without isn't in the local directory or
    /// the Hugging Face cache.
    #[error(
        "the embedding engine needs {file} from {repo} and could not find it ({detail}). \
         Either the model has no ONNX export, or this host has not fetched it: pre-fill \
         the Hugging Face cache, or point EMBEDDING_MODEL at a directory holding the \
         repo's files"
    )]
    ModelUnavailable {
        repo: String,
        file: String,
        detail: String,
    },

    /// The model pools in a way this engine doesn't implement. Embedding it
    /// with the wrong pooling would land every vector in a different space
    /// from the ones already stored, silently, so it is refused.
    #[error(
        "{repo} pools with {described}, which this engine does not implement \
         (supported: mean, cls, max, one at a time)"
    )]
    UnsupportedPooling { repo: String, described: String },

    #[error(
        "output {name:?} has shape {shape:?}; the engine needs a [batch, tokens, dim] token-embedding output"
    )]
    BadOutput { name: String, shape: Vec<i64> },

    #[error("tokeniser error: {0}")]
    Tokenizer(String),

    #[error("ONNX Runtime error: {0}")]
    Runtime(String),

    #[error("could not read {path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
}

impl EmbedError {
    /// True when the model simply isn't present, as opposed to present and
    /// broken. Tests skip on this; startup reports it with the fix.
    pub fn is_model_unavailable(&self) -> bool {
        matches!(self, EmbedError::ModelUnavailable { .. })
    }
}

impl From<ort::Error> for EmbedError {
    fn from(err: ort::Error) -> Self {
        EmbedError::Runtime(err.to_string())
    }
}
