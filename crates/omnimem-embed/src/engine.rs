//! The ONNX Runtime session, tokenisation, pooling and normalisation.

use std::sync::Mutex;

use ort::session::builder::GraphOptimizationLevel;
use ort::session::{Session, SessionInputValue};
use ort::value::Tensor;
use tokenizers::{PaddingParams, Tokenizer, TruncationParams};
use tracing::info;

use crate::error::EmbedError;
use crate::model::{EmbedConfig, ModelFiles, TOKENIZER_FILE};

/// Texts per ONNX Runtime call, as in the Python engine.
const BATCH_SIZE: usize = 32;
/// Characters kept per text before tokenising, as a multiple of the token
/// cap. The tokeniser's normalisation is linear in the input, so a text of
/// megabytes would be paid for in full only to keep its first few hundred
/// tokens. A token is rarely more than a few characters, so 32 per token
/// leaves the truncated output identical for any text that matters.
const CHARS_PER_TOKEN: usize = 32;
/// Output names that carry per-token embeddings, preferred over position.
const TOKEN_OUTPUT_NAMES: [&str; 2] = ["last_hidden_state", "token_embeddings"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pooling {
    Mean,
    Cls,
    Max,
}

/// A loaded model. Cheap to share: embedding takes `&self`.
pub struct Embedder {
    // ort's `Session::run` takes `&mut self`; one lock per call is far
    // cheaper than the inference it guards.
    session: Mutex<Session>,
    tokenizer: Tokenizer,
    wants_token_type_ids: bool,
    output_index: usize,
    pooling: Pooling,
    dimension: usize,
    max_seq_length: usize,
}

impl Embedder {
    /// Find the model files, open the graph and tokeniser, and check the
    /// graph produces per-token embeddings.
    pub fn load(config: &EmbedConfig) -> Result<Self, EmbedError> {
        let files = ModelFiles::locate(config)?;
        let model_path = files.required(&config.onnx_file)?;
        let tokenizer_path = files.required(TOKENIZER_FILE)?;
        let pooling = files.pooling()?;
        let max_seq_length = files.max_seq_length(config.max_seq_length);

        info!(repo = files.repo(), file = %config.onnx_file, "loading ONNX embedding model");

        let runtime = |e: ort::Error<ort::session::builder::SessionBuilder>| {
            EmbedError::Runtime(e.to_string())
        };
        let mut builder = Session::builder()?
            // Level2 is ONNX Runtime's "extended", the level the Python
            // engine uses.
            .with_optimization_level(GraphOptimizationLevel::Level2)
            .map_err(runtime)?;
        if let Some(threads) = config.threads.filter(|n| *n > 0) {
            builder = builder.with_intra_threads(threads).map_err(runtime)?;
        }
        let session = builder.commit_from_file(&model_path)?;

        let wants_token_type_ids = session
            .inputs()
            .iter()
            .any(|i| i.name() == "token_type_ids");
        let outputs = session.outputs();
        let output_index = outputs
            .iter()
            .position(|o| TOKEN_OUTPUT_NAMES.contains(&o.name()))
            .unwrap_or(0);
        let outlet = outputs
            .get(output_index)
            .ok_or_else(|| EmbedError::BadOutput {
                name: String::new(),
                shape: Vec::new(),
            })?;
        let shape: Vec<i64> = outlet
            .dtype()
            .tensor_shape()
            .map(|s| s.to_vec())
            .unwrap_or_default();
        if shape.len() != 3 {
            return Err(EmbedError::BadOutput {
                name: outlet.name().to_owned(),
                shape,
            });
        }
        let static_dimension = usize::try_from(shape[2]).ok().filter(|d| *d > 0);

        let mut tokenizer = Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| EmbedError::Tokenizer(e.to_string()))?;
        tokenizer
            .with_truncation(Some(TruncationParams {
                max_length: max_seq_length,
                ..Default::default()
            }))
            .map_err(|e| EmbedError::Tokenizer(e.to_string()))?;
        // Pad each batch to its longest member, with the model's own [PAD]
        // id, exactly as the Python engine configures the same library.
        let padding = match tokenizer.token_to_id("[PAD]") {
            Some(pad_id) => PaddingParams {
                pad_id,
                pad_token: "[PAD]".to_owned(),
                ..Default::default()
            },
            None => PaddingParams::default(),
        };
        tokenizer.with_padding(Some(padding));

        let mut engine = Self {
            session: Mutex::new(session),
            tokenizer,
            wants_token_type_ids,
            output_index,
            pooling,
            dimension: static_dimension.unwrap_or(0),
            max_seq_length,
        };
        if static_dimension.is_none() {
            // A graph with a dynamic hidden size: learn it from one pass.
            engine.dimension = engine.embed("")?.len();
        }
        info!(
            dimension = engine.dimension,
            pooling = ?engine.pooling,
            max_seq_length = engine.max_seq_length,
            revision = %config.effective_revision(),
            "ONNX embedding model loaded"
        );
        Ok(engine)
    }

    pub fn dimension(&self) -> usize {
        self.dimension
    }

    pub fn pooling(&self) -> Pooling {
        self.pooling
    }

    pub fn max_seq_length(&self) -> usize {
        self.max_seq_length
    }

    /// One unit-length vector.
    pub fn embed(&self, text: &str) -> Result<Vec<f32>, EmbedError> {
        Ok(self.embed_batch(&[text])?.pop().unwrap_or_default())
    }

    /// Unit-length vectors, in input order.
    pub fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
        let mut out = Vec::with_capacity(texts.len());
        for chunk in texts.chunks(BATCH_SIZE) {
            out.extend(self.embed_chunk(chunk)?);
        }
        Ok(out)
    }

    fn embed_chunk(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
        let limit = self.max_seq_length.saturating_mul(CHARS_PER_TOKEN);
        let texts: Vec<&str> = texts.iter().map(|t| take_chars(t, limit)).collect();
        let encodings = self
            .tokenizer
            .encode_batch(texts, true)
            .map_err(|e| EmbedError::Tokenizer(e.to_string()))?;
        let batch = encodings.len();
        let tokens = encodings.first().map_or(0, |e| e.get_ids().len());

        let flatten = |field: fn(&tokenizers::Encoding) -> &[u32]| -> Vec<i64> {
            encodings
                .iter()
                .flat_map(|e| field(e).iter().map(|&v| i64::from(v)))
                .collect()
        };
        let input_ids = flatten(tokenizers::Encoding::get_ids);
        let attention_mask = flatten(tokenizers::Encoding::get_attention_mask);
        let shape = vec![batch as i64, tokens as i64];

        let mut inputs: Vec<(&str, SessionInputValue<'_>)> = vec![
            (
                "input_ids",
                Tensor::from_array((shape.clone(), input_ids))?.into(),
            ),
            (
                "attention_mask",
                Tensor::from_array((shape.clone(), attention_mask.clone()))?.into(),
            ),
        ];
        if self.wants_token_type_ids {
            let type_ids = flatten(tokenizers::Encoding::get_type_ids);
            inputs.push((
                "token_type_ids",
                Tensor::from_array((shape, type_ids))?.into(),
            ));
        }

        let mut session = self
            .session
            .lock()
            .map_err(|_| EmbedError::Runtime("session lock poisoned".into()))?;
        let outputs = session.run(inputs)?;
        let (out_shape, data) = outputs[self.output_index].try_extract_tensor::<f32>()?;
        let bad_output = || EmbedError::BadOutput {
            name: String::new(),
            shape: out_shape.to_vec(),
        };
        // The shape is checked against the batch and the data length before
        // any slicing: a graph that answers with the wrong shape must be an
        // error, not a panic that poisons the session lock for every later
        // call.
        if out_shape.len() != 3 || out_shape[0] != batch as i64 {
            return Err(bad_output());
        }
        let dim = usize::try_from(out_shape[2]).map_err(|_| bad_output())?;
        let out_tokens = usize::try_from(out_shape[1]).map_err(|_| bad_output())?;
        if batch
            .checked_mul(out_tokens)
            .and_then(|n| n.checked_mul(dim))
            .is_none_or(|n| n > data.len())
        {
            return Err(bad_output());
        }

        let mut vectors = Vec::with_capacity(batch);
        for row in 0..batch {
            let token =
                |t: usize| &data[(row * out_tokens + t) * dim..(row * out_tokens + t + 1) * dim];
            let mask = &attention_mask[row * tokens..(row + 1) * tokens];
            let mut pooled = pool(self.pooling, dim, out_tokens.min(tokens), &token, mask);
            normalise(&mut pooled);
            vectors.push(pooled);
        }
        Ok(vectors)
    }
}

/// sentence-transformers' Pooling module for the modes it is safe to
/// reproduce: mean over the attention mask, the first (CLS) token, or the
/// element-wise max over unmasked tokens.
fn pool<'a>(
    mode: Pooling,
    dim: usize,
    tokens: usize,
    token: &impl Fn(usize) -> &'a [f32],
    mask: &[i64],
) -> Vec<f32> {
    match mode {
        Pooling::Cls => token(0).to_vec(),
        Pooling::Max => {
            let mut out = vec![f32::NEG_INFINITY; dim];
            for t in (0..tokens).filter(|&t| mask[t] > 0) {
                for (o, v) in out.iter_mut().zip(token(t)) {
                    *o = o.max(*v);
                }
            }
            out
        }
        Pooling::Mean => {
            let mut sum = vec![0.0f32; dim];
            let mut count = 0.0f32;
            for t in (0..tokens).filter(|&t| mask[t] > 0) {
                for (s, v) in sum.iter_mut().zip(token(t)) {
                    *s += *v;
                }
                count += 1.0;
            }
            let count = count.max(1e-9);
            sum.iter_mut().for_each(|s| *s /= count);
            sum
        }
    }
}

/// L2 normalisation with the Python engine's 1e-12 floor.
fn normalise(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-12);
    v.iter_mut().for_each(|x| *x /= norm);
}

/// The first `n` characters of `s`, on a character boundary; `s` itself
/// when it is short enough.
fn take_chars(s: &str, n: usize) -> &str {
    s.char_indices().nth(n).map_or(s, |(i, _)| &s[..i])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn take_chars_cuts_on_character_boundaries() {
        assert_eq!(take_chars("héllo", 2), "hé");
        assert_eq!(take_chars("héllo", 5), "héllo");
        assert_eq!(take_chars("héllo", 50), "héllo");
        assert_eq!(take_chars("", 3), "");
        assert_eq!(take_chars("abc", 0), "");
    }

    fn rows() -> Vec<Vec<f32>> {
        vec![vec![1.0, 4.0], vec![3.0, 2.0], vec![100.0, 100.0]]
    }

    #[test]
    fn mean_pooling_ignores_padding() {
        let rows = rows();
        let token = |t: usize| rows[t].as_slice();
        // Third token is padding: its huge values must not move the mean.
        assert_eq!(
            pool(Pooling::Mean, 2, 3, &token, &[1, 1, 0]),
            vec![2.0, 3.0]
        );
    }

    #[test]
    fn max_and_cls_pooling() {
        let rows = rows();
        let token = |t: usize| rows[t].as_slice();
        assert_eq!(pool(Pooling::Max, 2, 3, &token, &[1, 1, 0]), vec![3.0, 4.0]);
        assert_eq!(pool(Pooling::Cls, 2, 3, &token, &[1, 1, 0]), vec![1.0, 4.0]);
    }

    #[test]
    fn normalise_gives_unit_length_and_survives_zero() {
        let mut v = vec![3.0, 4.0];
        normalise(&mut v);
        assert_eq!(v, vec![0.6, 0.8]);
        let mut zero = vec![0.0, 0.0];
        normalise(&mut zero);
        assert_eq!(zero, vec![0.0, 0.0]);
    }
}
