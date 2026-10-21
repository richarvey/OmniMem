//! The memory engine.
//!
//! Everything the 6.x `memory/` package and `tools/core.py` did above the
//! store: the recall pipeline and its scoring, lifecycle transitions, topic
//! suppression, dedup, the tier-1 contradiction check, chunking, temporal
//! boosts, domain routing, and the core tool behaviours. Ported to keep the
//! Python's results, including the order of fields in what a tool returns,
//! because agents already read those shapes.
//!
//! Tool methods return `serde_json::Value` shaped exactly as 6.x returned
//! its dicts. Bad input is an [`EngineError::Invalid`] whose message is what
//! the Python raised as a `ValueError`.

mod audit;
mod briefing;
mod chunking;
pub mod classification;
mod compiler;
mod config;
mod contradiction;
mod dedup;
pub mod domains;
mod error;
mod experience;
pub mod feeds;
mod knowledge;
mod lifecycle;
mod lineage;
mod llm;
mod maintenance;
mod projects;
pub mod pyfmt;
mod recall;
mod skill_scan;
mod skill_tools;
pub mod skills;
mod tags;
mod temporal;
mod tools;
pub mod transfer;

pub use llm::ExtractedFact;
pub use projects::{Suggestion, migrate_project_domains};

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use omnimem_core::{LanguageModel, Namespace, TextEmbedder};
use omnimem_store::Store;

pub use config::EngineConfig;
pub use dedup::DuplicateMatch;
pub use error::EngineError;
pub use lifecycle::MemoryState;
pub use recall::{RecallResult, compute_experience_weight};
pub use tools::DomainFilter;

pub type Result<T, E = EngineError> = std::result::Result<T, E>;

/// Texts per embedding call. The ONNX embedder pads each batch to its
/// longest member, so a batch of a few hundred document chunks would cost
/// memory in proportion; this bounds every caller without each having to
/// chunk for itself.
const EMBED_BATCH: usize = 32;

/// A value cached with the time it was computed.
type Cached<T> = Mutex<Option<(Instant, Arc<T>)>>;

pub struct Engine {
    store: Arc<Store>,
    embedder: Arc<dyn TextEmbedder>,
    llm: Option<Arc<dyn LanguageModel>>,
    config: EngineConfig,
    abandoned: Cached<Vec<recall::AbandonedEntry>>,
    domain_map: Cached<BTreeMap<String, Vec<String>>>,
    started: Instant,
}

impl Engine {
    pub fn new(store: Arc<Store>, embedder: Arc<dyn TextEmbedder>, config: EngineConfig) -> Self {
        Self {
            store,
            embedder,
            config,
            llm: None,
            abandoned: Mutex::new(None),
            domain_map: Mutex::new(None),
            started: Instant::now(),
        }
    }

    pub fn store(&self) -> &Arc<Store> {
        &self.store
    }

    pub fn config(&self) -> &EngineConfig {
        &self.config
    }

    /// Turn on the Claude Haiku features: fact extraction, query expansion
    /// and contradiction tier 2. Without a model they degrade as 6.x did
    /// with no API key.
    #[must_use]
    pub fn with_llm(mut self, llm: Arc<dyn LanguageModel>) -> Self {
        self.llm = Some(llm);
        self
    }

    /// The language model, when one is configured.
    pub fn llm(&self) -> Option<&Arc<dyn LanguageModel>> {
        self.llm.as_ref()
    }

    /// Unit vectors for `texts`, in order, from the engine's embedder.
    pub fn embed_texts(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        self.embed_many(texts)
    }

    pub(crate) fn embed(&self, text: &str) -> Result<Vec<f32>> {
        self.embed_many(&[text])?
            .pop()
            .ok_or_else(|| EngineError::Embedding("the embedder returned no vector".into()))
    }

    /// Vectors for `texts`, embedded [`EMBED_BATCH`] at a time.
    pub(crate) fn embed_many(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let mut vectors = Vec::with_capacity(texts.len());
        for batch in texts.chunks(EMBED_BATCH) {
            let embedded = self
                .embedder
                .embed_texts(batch)
                .map_err(|e| EngineError::Embedding(e.to_string()))?;
            if embedded.len() != batch.len() {
                return Err(EngineError::Embedding(format!(
                    "asked for {} vectors, got {}",
                    batch.len(),
                    embedded.len()
                )));
            }
            vectors.extend(embedded);
        }
        Ok(vectors)
    }

    /// Record count of a namespace, for the scans that cap how many keys
    /// they consider and say so when the cap bites.
    pub(crate) fn warn_if_capped(&self, namespace: Namespace, cap: usize, what: &str) {
        match self.store.count_records(namespace) {
            Ok(total) if total > cap => {
                tracing::warn!(cap, total, "{what} scan capped");
            }
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "could not count {namespace} records"),
        }
    }

    /// Drop the cached abandoned-approach list after a write that may change it.
    pub fn invalidate_abandoned_cache(&self) {
        *self
            .abandoned
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }

    /// Drop the cached domain-to-project map after a project write.
    pub fn invalidate_domain_cache(&self) {
        *self
            .domain_map
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }
}
