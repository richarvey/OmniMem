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

fn one() -> i64 {
    1
}

fn seven() -> i64 {
    7
}

fn twenty() -> i64 {
    20
}

fn hundred() -> i64 {
    100
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct RecordExperience {
    pub key: String,
    pub effort_score: i64,
    pub outcome: String,
    #[serde(default = "one")]
    pub iterations: i64,
    #[serde(default)]
    pub abandoned_approaches: Option<Vec<serde_json::Map<String, serde_json::Value>>>,
    #[serde(default)]
    pub breakthrough: Option<String>,
    #[serde(default)]
    pub gotchas: Option<String>,
    #[serde(default)]
    pub lesson: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct LogAbandoned {
    pub key: String,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub reason: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct Key {
    pub key: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct OptionalProject {
    #[serde(default)]
    pub project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct Query {
    pub query: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct SetProjectContext {
    pub project_name: String,
    pub description: String,
    pub stack: String,
    pub goals: String,
    pub current_state: String,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub domains: Option<DomainArg>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct ProjectName {
    pub project_name: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct ListProjects {
    #[serde(default)]
    pub domain: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct CompileProject {
    pub project_name: String,
    #[serde(default)]
    pub auto_save: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct UpdateProjectState {
    pub project_name: String,
    pub current_state: String,
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct BulkProject {
    pub project_name: String,
    #[serde(default)]
    pub confirm: bool,
    #[serde(default)]
    pub include_context: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct DeprioritiseProject {
    pub project_name: String,
    #[serde(default)]
    pub confirm: bool,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub include_context: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct MemoryAudit {
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub namespace: Option<String>,
    #[serde(default)]
    pub include_archived: bool,
    #[serde(default = "hundred")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct Reindex {
    #[serde(default)]
    pub namespace: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct SetLicence {
    pub licence: String,
    #[serde(default)]
    pub keys: Option<Vec<String>>,
    #[serde(default)]
    pub feed_name: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct SetProvenance {
    pub provenance: String,
    pub keys: Vec<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct CheckContradictions {
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default = "episodic")]
    pub namespace: String,
    #[serde(default)]
    pub project_filter: Option<String>,
    #[serde(default)]
    pub use_api: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct Briefing {
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default = "yes")]
    pub include_knowledge: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct RecentKnowledge {
    #[serde(default = "seven")]
    pub days: i64,
    #[serde(default)]
    pub feed_name: Option<String>,
    #[serde(default)]
    pub topics: Option<Vec<String>>,
    #[serde(default = "twenty")]
    pub limit: i64,
    #[serde(default)]
    pub licence: Option<String>,
}
