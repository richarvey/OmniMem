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
pub use projects::migrate_project_domains;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use omnimem_core::{LanguageModel, TextEmbedder};
use omnimem_store::Store;

pub use config::EngineConfig;
pub use error::EngineError;
pub use lifecycle::MemoryState;
pub use recall::{RecallResult, compute_experience_weight};
pub use tools::DomainFilter;

pub type Result<T, E = EngineError> = std::result::Result<T, E>;

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
    pub fn with_llm(mut self, llm: Arc<dyn LanguageModel>) -> Self {
        self.llm = Some(llm);
        self
    }

    pub(crate) fn embed(&self, text: &str) -> Result<Vec<f32>> {
        self.embed_many(&[text])?
            .pop()
            .ok_or_else(|| EngineError::Embedding("the embedder returned no vector".into()))
    }

    pub(crate) fn embed_many(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        self.embedder
            .embed_texts(texts)
            .map_err(|e| EngineError::Embedding(e.to_string()))
    }

    /// Drop the cached abandoned-approach list after a write that may change it.
    pub fn invalidate_abandoned_cache(&self) {
        *self.abandoned.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }

    /// Drop the cached domain-to-project map after a project write.
    pub fn invalidate_domain_cache(&self) {
        *self.domain_map.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }
}
