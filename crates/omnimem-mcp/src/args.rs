//! Tool arguments: names, types and defaults as the 6.x signatures had them.

use omnimem_engine::DomainFilter;
use rmcp::schemars::JsonSchema;
use serde::Deserialize;

fn episodic() -> String {
    "episodic".to_owned()
}

fn paragraphs() -> String {
    "paragraphs".to_owned()
}

fn five() -> i64 {
    5
}

fn ten() -> i64 {
    10
}

fn snippet() -> i64 {
    150
}

fn yes() -> bool {
    true
}

#[derive(Debug, Deserialize, JsonSchema, Default)]
#[schemars(crate = "rmcp::schemars")]
pub struct NoArgs {}

/// One domain (a comma-separated string works too) or a list of them.
#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(untagged)]
pub enum DomainArg {
    List(Vec<String>),
    One(String),
}

impl From<DomainArg> for DomainFilter {
    fn from(arg: DomainArg) -> Self {
        match arg {
            DomainArg::List(v) => DomainFilter::Many(v),
            DomainArg::One(s) => DomainFilter::One(s),
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct Remember {
    pub content: String,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    #[serde(default = "episodic")]
    pub namespace: String,
    #[serde(default)]
    pub force: bool,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub licence: Option<String>,
    #[serde(default)]
    pub provenance: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct RememberDocument {
    pub content: String,
    #[serde(default = "paragraphs")]
    pub chunk_strategy: String,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    #[serde(default = "episodic")]
    pub namespace: String,
    #[serde(default)]
    pub chunk_size: Option<i64>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub licence: Option<String>,
    #[serde(default)]
    pub provenance: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct Recall {
    pub query: String,
    #[serde(default = "five")]
    pub top_k: i64,
    #[serde(default)]
    pub namespaces: Option<Vec<String>>,
    #[serde(default)]
    pub project_filter: Option<String>,
    #[serde(default)]
    pub expand_queries: Option<bool>,
    #[serde(default)]
    pub domain_filter: Option<DomainArg>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct RecallIndex {
    pub query: String,
    #[serde(default = "ten")]
    pub top_k: i64,
    #[serde(default)]
    pub namespaces: Option<Vec<String>>,
    #[serde(default)]
    pub project_filter: Option<String>,
    #[serde(default = "snippet")]
    pub snippet_length: i64,
    #[serde(default)]
    pub expand_queries: Option<bool>,
    #[serde(default)]
    pub domain_filter: Option<DomainArg>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct RecallDetail {
    pub keys: Vec<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct Deprioritise {
    pub key_or_query: String,
    pub reason: String,
    #[serde(default)]
    pub reinstate_hints: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct Archive {
    pub key_or_query: String,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct KeyOrQuery {
    pub key_or_query: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct Retag {
    pub key: String,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    #[serde(default)]
    pub add: Option<Vec<String>>,
    #[serde(default)]
    pub remove: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct Forget {
    pub key_or_query: String,
    #[serde(default)]
    pub confirm: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct SuppressTopic {
    pub topic: String,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct Topic {
    pub topic: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct FindDuplicates {
    #[serde(default = "episodic")]
    pub namespace: String,
    #[serde(default)]
    pub threshold: Option<f64>,
    #[serde(default)]
    pub project_filter: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct DumpToFile {
    #[serde(default)]
    pub filename: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct RestoreFromFile {
    pub filename: String,
    #[serde(default = "yes")]
    pub dry_run: bool,
}
