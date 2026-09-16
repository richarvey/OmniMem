//! The skill compiler's engine (`memory/skills.py`).
//!
//! The raw memories (experience, graveyard, promoted knowledge) are the source
//! of truth and a compiled skill is build output. Compilation is
//! deterministic: the same source memories render the same body apart from
//! the `compiled_at` line, so a proposed diff shows real change and the
//! accept-to-write gate stays reviewable. No LLM is involved; rule text is
//! lifted from the memories with their keys cited.
//!
//! A memory error is noise, a skill error is policy. Reinforcement gating,
//! the bless override and the propose-and-accept write path all exist to
//! stop a bad lesson becoming policy silently.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::LazyLock;

use chrono::DateTime;
use omnimem_store::Fields;
use regex::Regex;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use similar::TextDiff;
use tracing::warn;

use crate::pyfmt::{py_float, round_to};
use crate::{Engine, Result};

/// Bump when the operating contract text changes.
pub const CONTRACT_VERSION: i64 = 1;

pub const GENERATED_SKILL_PREFIX: &str = "mem:skill:gen:";
pub const SKILL_KEY_PREFIX: &str = "mem:skill:";

pub const INVALID_DOMAIN: &str = "Invalid domain. Use 1-64 characters: lowercase letters, digits, hyphens, underscores, or dots (e.g. 'python', 'technical-blogging').";

/// Fixed boilerplate that travels with every skill, versioned by
/// `CONTRACT_VERSION`. Never per-skill content.
const OPERATING_CONTRACT: &str = "## Operating contract  (fixed, applies to every OmniMem skill)

While working under this skill, keep the data pool alive:

- Check OmniMem preferences first and honour them.
- Read relevant experience before acting (recall / get_experience), and
  warn_if_abandoned before retrying anything that looks like a known dead end.
- Record experience as you go (record_experience): effort, outcome, dead ends
  (log_abandoned into the graveyard), and breakthroughs.
- Remember durable new facts (remember).

This block is fixed boilerplate, identical across all skills, inserted from one
template and versioned by contract_version. It is not per-skill compiled content.";

/// The banner still names Valkey: every skill compiled by 6.x carries this
/// line, and changing it would turn every recompile into a diff. It changes
/// deliberately at cut-over, not as a side effect of the port.
fn generated_banner(domain: &str) -> String {
    format!(
        "> GENERATED. Stored in and served from OmniMem (Valkey), domain tag: {domain}.\n\
         > Do not edit by hand. To change the domain guidance, update the underlying\n\
         > memories and recompile with omnimem:compile_skill. Hand edits are overwritten."
    )
}

const POOL_FIELDS: [&str; 13] = [
    "content",
    "state",
    "project",
    "tags",
    "effort_score",
    "outcome",
    "breakthrough",
    "lesson",
    "gotchas",
    "abandoned_approaches",
    "blessed",
    "created_at",
    "updated_at",
];

const KNOWLEDGE_POOL_FIELDS: [&str; 10] = [
    "content",
    "title",
    "state",
    "skill_domains",
    "skill_rules",
    "feed_name",
    "source_url",
    "created_at",
    "updated_at",
    "promoted_at",
];

pub const REFERENCE_RULE_KINDS: [&str; 4] = ["do", "watch", "dont", "note"];
const MAX_REFERENCE_RULES: usize = 20;
const MAX_REFERENCE_RULE_CHARS: usize = 400;
pub(crate) const MAX_POOL_KEYS: usize = 5000;

static WHITESPACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").expect("valid"));

pub fn generated_skill_key(domain: &str, user: &str) -> String {
    format!("{GENERATED_SKILL_PREFIX}{domain}-{user}")
}

// -- Python value helpers ------------------------------------------------------

/// Python truthiness of a JSON value.
pub fn py_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Python's `str()` of a JSON value, for the shapes stored data holds.
pub fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "None".to_owned(),
        Value::Bool(true) => "True".to_owned(),
        Value::Bool(false) => "False".to_owned(),
        Value::Number(n) => n
            .as_i64()
            .map(|i| i.to_string())
            .or_else(|| n.as_u64().map(|u| u.to_string()))
            .unwrap_or_else(|| py_float(n.as_f64().unwrap_or(0.0))),
        other => other.to_string(),
    }
}

/// Python's `repr()` of a JSON value, for error messages.
pub(crate) fn py_repr(v: &Value) -> String {
    match v {
        Value::String(s) => format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'")),
        other => py_str(other),
    }
}

pub(crate) fn safe_float(raw: Option<&String>) -> f64 {
    raw.and_then(|r| r.trim().parse::<f64>().ok())
        .filter(|f| f.is_finite())
        .unwrap_or(0.0)
}

fn nonempty(fields: &Fields, name: &str) -> Option<String> {
    fields.get(name).filter(|v| !v.is_empty()).cloned()
}

fn active_or_unset(row: &Fields) -> bool {
    matches!(row.get("state").map(String::as_str), None | Some("active"))
}

/// Tags stored as a JSON array (or, from old data, comma-separated text).
pub(crate) fn parse_tags(raw: Option<&str>) -> Vec<String> {
    let Some(raw) = raw.filter(|r| !r.is_empty()) else {
        return Vec::new();
    };
    match serde_json::from_str::<Value>(raw) {
        Ok(Value::Array(items)) => items.iter().filter(|v| py_truthy(v)).map(py_str).collect(),
        Ok(_) => Vec::new(),
        Err(_) => raw
            .split(',')
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_owned)
            .collect(),
    }
}

/// A JSON list of objects, or nothing.
pub(crate) fn parse_objects(raw: Option<&String>) -> Vec<Map<String, Value>> {
    raw.and_then(|r| serde_json::from_str::<Value>(r).ok())
        .and_then(|v| match v {
            Value::Array(items) => Some(items),
            _ => None,
        })
        .unwrap_or_default()
        .into_iter()
        .filter_map(|v| match v {
            Value::Object(o) => Some(o),
            _ => None,
        })
        .collect()
}

/// A JSON list of strings, or nothing.
pub(crate) fn parse_string_list(raw: Option<&String>) -> Vec<String> {
    raw.and_then(|r| serde_json::from_str::<Value>(r).ok())
        .and_then(|v| match v {
            Value::Array(items) => Some(items),
            _ => None,
        })
        .unwrap_or_default()
        .into_iter()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect()
}

/// Collapse whitespace so a lesson renders as one markdown bullet.
pub(crate) fn one_line(text: &str, limit: usize) -> String {
    let collapsed = WHITESPACE.replace_all(text, " ").trim().to_owned();
    if collapsed.chars().count() > limit {
        let cut: String = collapsed.chars().take(limit - 1).collect();
        format!("{}…", cut.trim_end())
    } else {
        collapsed
    }
}

fn gist(text: &str, limit: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= limit {
        text.to_owned()
    } else {
        let cut: String = text.chars().take(limit - 1).collect();
        format!("{}…", cut.trim_end())
    }
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn ends_sentence(text: &str) -> bool {
    text.ends_with(['.', '!', '?', '…'])
}

fn plural<'a>(n: usize, one: &'a str, many: &'a str) -> &'a str {
    if n == 1 { one } else { many }
}

// -- pools ------------------------------------------------------------------

/// One active episodic memory tagged with a domain: the compiler's input.
#[derive(Debug, Clone)]
pub(crate) struct PoolMemory {
    pub key: String,
    pub content: String,
    pub project: Option<String>,
    pub outcome: Option<String>,
    pub breakthrough: Option<String>,
    pub lesson: Option<String>,
    pub gotchas: Option<String>,
    pub abandoned: Vec<Map<String, Value>>,
    pub blessed: bool,
    pub created_at: f64,
    pub updated_at: f64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceRule {
    pub kind: String,
    pub text: String,
}

impl ReferenceRule {
    pub fn to_value(&self) -> Value {
        json!({"kind": self.kind, "text": self.text})
    }
}

/// A knowledge article promoted to a domain.
#[derive(Debug, Clone)]
pub(crate) struct PromotedItem {
    pub key: String,
    pub content: String,
    pub title: String,
    pub source_url: String,
    pub skill_rules: Vec<ReferenceRule>,
    pub promoted_at: f64,
}

/// A feed influencing a domain, strongest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedLink {
    pub feed_name: String,
    pub influence: i64,
    pub url: String,
}

#[derive(Debug, Clone)]
pub(crate) struct FeedArticle {
    pub key: String,
    pub content: String,
    pub title: String,
    pub feed_name: String,
    pub influence: i64,
    pub source_url: String,
    pub created_at: f64,
}

/// Domains a knowledge item has been promoted to.
pub(crate) fn parse_skill_domains(raw: Option<&String>) -> Vec<String> {
    parse_tags(raw.map(String::as_str))
        .into_iter()
        .map(|d| d.to_lowercase())
        .collect()
}

/// Validate extracted reference rules supplied at promotion time.
pub fn validate_reference_rules(raw: &Value) -> std::result::Result<Vec<ReferenceRule>, String> {
    let Value::Array(items) = raw else {
        return Err("rules must be a list of {kind, text} objects".to_owned());
    };
    if items.len() > MAX_REFERENCE_RULES {
        return Err(format!("rules: max {MAX_REFERENCE_RULES} per article"));
    }
    let mut validated = Vec::new();
    for item in items {
        let Value::Object(item) = item else {
            return Err("rules entries must be {kind, text} objects".to_owned());
        };
        let text_of = |name: &str| item.get(name).map(py_str).unwrap_or_default();
        let kind = text_of("kind").trim().to_lowercase();
        let text = text_of("text").trim().to_owned();
        if !REFERENCE_RULE_KINDS.contains(&kind.as_str()) {
            return Err(format!(
                "rules kind must be one of {}, got '{kind}'",
                REFERENCE_RULE_KINDS.join("/")
            ));
        }
        if text.is_empty() {
            return Err("rules entries need non-empty text".to_owned());
        }
        if text.chars().count() > MAX_REFERENCE_RULE_CHARS {
            return Err(format!("rules text max {MAX_REFERENCE_RULE_CHARS} chars"));
        }
        validated.push(ReferenceRule { kind, text });
    }
    Ok(validated)
}

/// The stored `skill_rules` field back to a validated list; empty on damage.
pub(crate) fn parse_reference_rules(raw: Option<&String>) -> Vec<ReferenceRule> {
    raw.filter(|r| !r.is_empty())
        .and_then(|r| serde_json::from_str::<Value>(r).ok())
        .and_then(|v| validate_reference_rules(&v).ok())
        .unwrap_or_default()
}

impl Engine {
    fn scan_capped(&self, prefix: &str, what: &str) -> Result<Vec<String>> {
        let mut keys = self.store.scan_prefix(prefix)?;
        if keys.len() > MAX_POOL_KEYS {
            warn!(
                cap = MAX_POOL_KEYS,
                total = keys.len(),
                "{what} scan capped"
            );
            keys.truncate(MAX_POOL_KEYS);
        }
        Ok(keys)
    }

    /// Active episodic memories tagged with each domain, in one scan.
    pub(crate) fn gather_domain_pools(
        &self,
        domains: &[String],
    ) -> Result<BTreeMap<String, Vec<PoolMemory>>> {
        let mut pools: BTreeMap<String, Vec<PoolMemory>> =
            domains.iter().map(|d| (d.clone(), Vec::new())).collect();
        if pools.is_empty() {
            return Ok(pools);
        }
        let keys = self.scan_capped("mem:episodic:", "skill pool")?;
        let rows = self.store.get_fields_multi(&keys, &POOL_FIELDS)?;
        for (key, row) in keys.into_iter().zip(rows) {
            let Some(row) = row else { continue };
            if !active_or_unset(&row) {
                continue;
            }
            let tags: Vec<String> = parse_tags(row.get("tags").map(String::as_str))
                .into_iter()
                .map(|t| t.to_lowercase())
                .collect();
            let matched: Vec<String> = pools.keys().filter(|d| tags.contains(d)).cloned().collect();
            if matched.is_empty() {
                continue;
            }
            let entry = PoolMemory {
                content: row.get("content").cloned().unwrap_or_default(),
                project: nonempty(&row, "project"),
                outcome: nonempty(&row, "outcome"),
                breakthrough: nonempty(&row, "breakthrough"),
                lesson: nonempty(&row, "lesson"),
                gotchas: nonempty(&row, "gotchas"),
                abandoned: parse_objects(row.get("abandoned_approaches")),
                blessed: row.get("blessed").map(String::as_str) == Some("1"),
                created_at: safe_float(row.get("created_at")),
                updated_at: safe_float(row.get("updated_at")),
                key,
            };
            for domain in matched {
                if let Some(pool) = pools.get_mut(&domain) {
                    pool.push(entry.clone());
                }
            }
        }
        for pool in pools.values_mut() {
            pool.sort_by(|a, b| a.key.cmp(&b.key));
        }
        Ok(pools)
    }

    /// Active knowledge promoted to each domain: the Reference input.
    pub(crate) fn gather_promoted_knowledge(
        &self,
        domains: &[String],
    ) -> Result<BTreeMap<String, Vec<PromotedItem>>> {
        let mut pools: BTreeMap<String, Vec<PromotedItem>> =
            domains.iter().map(|d| (d.clone(), Vec::new())).collect();
        if pools.is_empty() {
            return Ok(pools);
        }
        let keys = self.scan_capped("mem:knowledge:", "promoted-knowledge")?;
        let rows = self.store.get_fields_multi(&keys, &KNOWLEDGE_POOL_FIELDS)?;
        for (key, row) in keys.into_iter().zip(rows) {
            let Some(row) = row else { continue };
            if !active_or_unset(&row) {
                continue;
            }
            let promoted_to = parse_skill_domains(row.get("skill_domains"));
            let matched: Vec<String> = pools
                .keys()
                .filter(|d| promoted_to.contains(d))
                .cloned()
                .collect();
            if matched.is_empty() {
                continue;
            }
            let entry = PromotedItem {
                content: row.get("content").cloned().unwrap_or_default(),
                title: row.get("title").cloned().unwrap_or_default(),
                source_url: row.get("source_url").cloned().unwrap_or_default(),
                skill_rules: parse_reference_rules(row.get("skill_rules")),
                promoted_at: safe_float(row.get("promoted_at")),
                key,
            };
            for domain in matched {
                if let Some(pool) = pools.get_mut(&domain) {
                    pool.push(entry.clone());
                }
            }
        }
        for pool in pools.values_mut() {
            pool.sort_by(|a, b| a.key.cmp(&b.key));
        }
        Ok(pools)
    }

    /// The latest articles from feeds influencing a domain, each feed
    /// contributing up to its influence score, capped overall.
    pub(crate) fn gather_feed_knowledge(
        &self,
        domain: &str,
        domain_feeds: &[FeedLink],
    ) -> Result<Vec<FeedArticle>> {
        let max_total = self.config.skill_feed_max_articles;
        if domain_feeds.is_empty() || max_total <= 0 {
            return Ok(Vec::new());
        }
        let keys = self.scan_capped("mem:knowledge:", "feed-knowledge")?;
        let rows = self.store.get_fields_multi(&keys, &KNOWLEDGE_POOL_FIELDS)?;
        let mut by_feed: HashMap<String, Vec<FeedArticle>> = HashMap::new();
        for (key, row) in keys.into_iter().zip(rows) {
            let Some(row) = row else { continue };
            if !active_or_unset(&row) {
                continue;
            }
            let feed_name = row.get("feed_name").cloned().unwrap_or_default();
            let Some(link) = domain_feeds.iter().find(|f| f.feed_name == feed_name) else {
                continue;
            };
            if parse_skill_domains(row.get("skill_domains"))
                .iter()
                .any(|d| d == domain)
            {
                continue;
            }
            by_feed
                .entry(feed_name.clone())
                .or_default()
                .push(FeedArticle {
                    content: row.get("content").cloned().unwrap_or_default(),
                    title: row.get("title").cloned().unwrap_or_default(),
                    influence: link.influence,
                    source_url: row.get("source_url").cloned().unwrap_or_default(),
                    created_at: safe_float(row.get("created_at")),
                    feed_name,
                    key,
                });
        }
        let mut selected = Vec::new();
        for feed in domain_feeds {
            let Some(mut articles) = by_feed.remove(&feed.feed_name) else {
                continue;
            };
            articles.sort_by(|a, b| {
                b.created_at
                    .partial_cmp(&a.created_at)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.key.cmp(&b.key))
            });
            selected.extend(articles.into_iter().take(feed.influence.max(0) as usize));
        }
        selected.truncate(max_total as usize);
        Ok(selected)
    }
}

/// Would `extract_lessons` get anything out of this memory?
pub(crate) fn lesson_bearing(mem: &PoolMemory) -> bool {
    if mem.blessed {
        return mem.lesson.is_some()
            || mem.breakthrough.is_some()
            || mem.gotchas.is_some()
            || !mem.abandoned.is_empty()
            || !mem.content.is_empty();
    }
    if (mem.lesson.is_some() || mem.breakthrough.is_some())
        && mem.outcome.as_deref() == Some("succeeded")
    {
        return true;
    }
    mem.gotchas.is_some() || !mem.abandoned.is_empty()
}

// -- lessons and rules --------------------------------------------------------

/// One extractable unit of procedure from a single memory.
#[derive(Debug, Clone)]
pub(crate) struct Lesson {
    pub kind: &'static str,
    pub text: String,
    pub source_key: String,
    pub source_updated_at: f64,
    pub blessed: bool,
    pub project: Option<String>,
    pub name: Option<String>,
    pub approach_type: String,
}

/// A lesson pattern that cleared the promotion gate.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Rule {
    pub kind: &'static str,
    pub text: String,
    pub sources: Vec<String>,
    pub reinforcement: usize,
    pub blessed: bool,
    pub name: Option<String>,
    pub approach_type: String,
    pub projects: Vec<String>,
    pub url: String,
    pub feed: String,
    pub influence: i64,
}

impl Rule {
    fn single(kind: &'static str, text: String, source: &str) -> Self {
        Self {
            kind,
            text,
            sources: vec![source.to_owned()],
            reinforcement: 1,
            blessed: false,
            name: None,
            approach_type: String::new(),
            projects: Vec::new(),
            url: String::new(),
            feed: String::new(),
            influence: 0,
        }
    }

    pub fn primary_source(&self) -> &str {
        self.sources.last().map_or("", String::as_str)
    }

    pub fn to_value(&self) -> Value {
        let mut m = Map::new();
        m.insert("kind".into(), self.kind.into());
        m.insert("text".into(), self.text.clone().into());
        m.insert("sources".into(), json!(self.sources));
        m.insert("reinforcement".into(), self.reinforcement.into());
        if self.blessed {
            m.insert("blessed".into(), true.into());
        }
        if let Some(name) = self.name.as_ref().filter(|n| !n.is_empty()) {
            m.insert("name".into(), name.clone().into());
        }
        if !self.url.is_empty() {
            m.insert("url".into(), self.url.clone().into());
        }
        if !self.feed.is_empty() {
            m.insert("feed".into(), self.feed.clone().into());
        }
        if self.influence != 0 {
            m.insert("influence".into(), self.influence.into());
        }
        Value::Object(m)
    }
}

/// Succeeded work becomes a do-lesson from its lesson (or breakthrough),
/// gotchas become watch-lessons and graveyard entries dont-lessons. A blessed
/// memory always contributes something.
pub(crate) fn extract_lessons(pool: &[PoolMemory], include_graveyard: bool) -> Vec<Lesson> {
    let mut lessons = Vec::new();
    for mem in pool {
        let mut contributed = false;
        let lesson = |kind: &'static str, text: String| Lesson {
            kind,
            text,
            source_key: mem.key.clone(),
            source_updated_at: mem.updated_at,
            blessed: mem.blessed,
            project: mem.project.clone(),
            name: None,
            approach_type: String::new(),
        };

        // The lesson is the generalisable claim, the breakthrough what
        // happened this time (#35): prefer the claim, fall back to the story.
        let do_text = mem.lesson.as_ref().or(mem.breakthrough.as_ref());
        if let Some(text) = do_text
            && (mem.outcome.as_deref() == Some("succeeded") || mem.blessed)
        {
            lessons.push(lesson("do", one_line(text, 400)));
            contributed = true;
        }

        if let Some(gotchas) = &mem.gotchas {
            lessons.push(lesson("watch", one_line(gotchas, 400)));
            contributed = true;
        }

        if include_graveyard {
            for approach in &mem.abandoned {
                let Some(name) = approach
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|n| !n.is_empty())
                else {
                    continue;
                };
                let reason = approach.get("reason").and_then(Value::as_str).unwrap_or("");
                let mut dont = lesson("dont", one_line(reason, 400));
                dont.name = Some(name.to_owned());
                dont.approach_type = approach
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                lessons.push(dont);
                contributed = true;
            }
        }

        if mem.blessed && !contributed && !mem.content.is_empty() {
            lessons.push(lesson("do", one_line(&mem.content, 400)));
        }
    }
    lessons
}

/// Connected components over pairs at or above the threshold, ordered by
/// their smallest member.
fn cluster_indices(vectors: &[Vec<f32>], threshold: f64) -> Vec<Vec<usize>> {
    let n = vectors.len();
    if n == 0 {
        return Vec::new();
    }
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(parent: &mut [usize], mut x: usize) -> usize {
        while parent[x] != x {
            parent[x] = parent[parent[x]];
            x = parent[x];
        }
        x
    }
    for i in 0..n {
        for j in (i + 1)..n {
            let dot: f32 = vectors[i].iter().zip(&vectors[j]).map(|(a, b)| a * b).sum();
            if f64::from(dot) >= threshold {
                let (ri, rj) = (find(&mut parent, i), find(&mut parent, j));
                if ri != rj {
                    parent[ri] = rj;
                }
            }
        }
    }
    let mut clusters: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for i in 0..n {
        let root = find(&mut parent, i);
        clusters.entry(root).or_default().push(i);
    }
    let mut clusters: Vec<Vec<usize>> = clusters.into_values().collect();
    clusters.sort_by_key(|c| c[0]);
    clusters
}

/// Fold a lesson cluster into one rule: the newest source supplies the
/// wording, and reinforcement counts distinct source memories.
fn rule_from_cluster(kind: &'static str, members: &[&Lesson]) -> Rule {
    let mut representative = members[0];
    for lesson in &members[1..] {
        let newer = lesson.source_updated_at > representative.source_updated_at
            || (lesson.source_updated_at == representative.source_updated_at
                && lesson.source_key > representative.source_key);
        if newer {
            representative = lesson;
        }
    }
    let sources: BTreeSet<String> = members.iter().map(|l| l.source_key.clone()).collect();
    let projects: BTreeSet<String> = members.iter().filter_map(|l| l.project.clone()).collect();
    Rule {
        kind,
        text: representative.text.clone(),
        reinforcement: sources.len(),
        sources: sources.into_iter().collect(),
        blessed: members.iter().any(|l| l.blessed),
        name: representative.name.clone(),
        approach_type: representative.approach_type.clone(),
        projects: projects.into_iter().collect(),
        url: String::new(),
        feed: String::new(),
        influence: 0,
    }
}

fn kind_rank(kind: &str) -> u8 {
    match kind {
        "do" => 0,
        "watch" => 1,
        _ => 2,
    }
}

fn sort_rules(rules: &mut [Rule]) {
    let label = |r: &Rule| {
        r.name
            .as_deref()
            .filter(|n| !n.is_empty())
            .unwrap_or(&r.text)
            .to_lowercase()
    };
    rules.sort_by(|a, b| {
        kind_rank(a.kind)
            .cmp(&kind_rank(b.kind))
            .then(b.reinforcement.cmp(&a.reinforcement))
            .then_with(|| label(a).cmp(&label(b)))
    });
}

impl Engine {
    /// Cluster lessons into rules and apply the promotion gate. Returns
    /// (eligible, held back).
    pub(crate) fn build_rules(
        &self,
        lessons: &[Lesson],
        min_reinforcement: usize,
    ) -> Result<(Vec<Rule>, Vec<Rule>)> {
        let threshold = self.config.skill_cluster_threshold;
        let mut rules = Vec::new();
        for kind in ["do", "watch"] {
            let kind_lessons: Vec<&Lesson> = lessons.iter().filter(|l| l.kind == kind).collect();
            if kind_lessons.is_empty() {
                continue;
            }
            let mut vectors = Vec::with_capacity(kind_lessons.len());
            for chunk in kind_lessons.chunks(32) {
                let texts: Vec<&str> = chunk.iter().map(|l| l.text.as_str()).collect();
                vectors.extend(self.embed_many(&texts)?);
            }
            for cluster in cluster_indices(&vectors, threshold) {
                let members: Vec<&Lesson> = cluster.iter().map(|i| kind_lessons[*i]).collect();
                rules.push(rule_from_cluster(kind, &members));
            }
        }

        let mut dont_groups: BTreeMap<String, Vec<&Lesson>> = BTreeMap::new();
        for lesson in lessons {
            if lesson.kind == "dont"
                && let Some(name) = lesson.name.as_ref().filter(|n| !n.is_empty())
            {
                dont_groups
                    .entry(name.to_lowercase())
                    .or_default()
                    .push(lesson);
            }
        }
        for members in dont_groups.values() {
            rules.push(rule_from_cluster("dont", members));
        }

        let (mut eligible, mut held_back): (Vec<Rule>, Vec<Rule>) = rules
            .into_iter()
            .partition(|r| r.reinforcement >= min_reinforcement || r.blessed);
        sort_rules(&mut eligible);
        sort_rules(&mut held_back);
        Ok((eligible, held_back))
    }
}

fn summary_text(title: &str, content: &str) -> String {
    let title = title.trim();
    let content = content.trim();
    if !title.is_empty()
        && !content.is_empty()
        && !content.to_lowercase().starts_with(&title.to_lowercase())
    {
        one_line(&format!("{title}: {content}"), 400)
    } else if content.is_empty() {
        one_line(title, 400)
    } else {
        one_line(content, 400)
    }
}

/// Ref rules for promoted articles: one per extracted rule, or one summary.
pub(crate) fn build_reference_rules(promoted: &[PromotedItem]) -> Vec<Rule> {
    let mut items: Vec<&PromotedItem> = promoted.iter().collect();
    items.sort_by(|a, b| a.key.cmp(&b.key));
    let mut rules = Vec::new();
    for item in items {
        let title = item.title.trim();
        let name = Some(title.to_owned()).filter(|t| !t.is_empty());
        if !item.skill_rules.is_empty() {
            for entry in &item.skill_rules {
                let prefix = match entry.kind.as_str() {
                    "do" => "Do: ",
                    "watch" => "Watch out: ",
                    "dont" => "Avoid: ",
                    _ => "",
                };
                let mut rule = Rule::single(
                    "ref",
                    one_line(&format!("{prefix}{}", entry.text), 400),
                    &item.key,
                );
                rule.name = name.clone();
                rule.url = item.source_url.clone();
                rules.push(rule);
            }
            continue;
        }
        let text = summary_text(&item.title, &item.content);
        if text.is_empty() {
            continue;
        }
        let mut rule = Rule::single("ref", text, &item.key);
        rule.name = name;
        rule.url = item.source_url.clone();
        rules.push(rule);
    }
    rules
}

/// Feed rules for influence-selected articles, in selection order.
pub(crate) fn build_feed_rules(articles: &[FeedArticle]) -> Vec<Rule> {
    articles
        .iter()
        .filter_map(|item| {
            let text = summary_text(&item.title, &item.content);
            if text.is_empty() {
                return None;
            }
            let mut rule = Rule::single("feed", text, &item.key);
            rule.name = Some(item.title.trim().to_owned()).filter(|t| !t.is_empty());
            rule.url = item.source_url.clone();
            rule.feed = item.feed_name.clone();
            rule.influence = item.influence;
            Some(rule)
        })
        .collect()
}

/// The compiler's draft of a new skill's load trigger.
pub(crate) fn draft_description(domain: &str, user: &str) -> String {
    format!(
        "How {user} works in {domain}: distilled do/don't procedure compiled from experience and \
         graveyard memories. Load when: starting {domain} work, reviewing or writing {domain} code \
         or content, or beginning a greenfield {domain} project."
    )
}

fn manifest_annotations(rules: &[Rule]) -> Vec<(String, String)> {
    let mut seen: Vec<(String, String)> = Vec::new();
    for rule in rules {
        for source in &rule.sources {
            if seen.iter().any(|(k, _)| k == source) {
                continue;
            }
            let annotation = match rule.kind {
                "dont" => format!("graveyard: {}", rule.name.as_deref().unwrap_or("None")),
                "ref" => "promoted reference".to_owned(),
                "feed" => format!("feed: {} (influence {}/10)", rule.feed, rule.influence),
                _ if rule.blessed && rule.reinforcement < 2 => "blessed".to_owned(),
                _ => format!("reinforced x{}", rule.reinforcement),
            };
            seen.push((source.clone(), annotation));
        }
    }
    seen
}

fn sentence(text: &str) -> String {
    if ends_sentence(text) {
        text.to_owned()
    } else {
        format!("{text}.")
    }
}

fn rule_bullet(rule: &Rule) -> String {
    let mut line = match rule.kind {
        "dont" => {
            let type_part = if rule.approach_type.is_empty() {
                String::new()
            } else {
                format!(" ({})", rule.approach_type)
            };
            let reason = sentence(if rule.text.is_empty() {
                "abandoned"
            } else {
                &rule.text
            });
            let tried = if rule.projects.is_empty() {
                String::new()
            } else {
                format!(" Tried on {}, abandoned.", rule.projects.join(", "))
            };
            format!(
                "- Avoid {}{type_part}. {reason}{tried}",
                rule.name.as_deref().unwrap_or("None")
            )
        }
        "ref" => {
            let mut line = format!("- {}", sentence(&rule.text));
            if !rule.url.is_empty() {
                line.push_str(&format!(" ({})", rule.url));
            }
            line
        }
        "feed" => {
            let mut line = format!(
                "- {} (via {}, influence {}/10)",
                sentence(&rule.text),
                rule.feed,
                rule.influence
            );
            if !rule.url.is_empty() {
                line.push_str(&format!(" ({})", rule.url));
            }
            line
        }
        _ => format!("- {}", sentence(&rule.text)),
    };
    if rule.blessed && rule.reinforcement < 2 {
        line.push_str(" (blessed)");
    } else if rule.reinforcement > 1 {
        line.push_str(&format!(" (reinforced x{})", rule.reinforcement));
    }
    format!("{line} [{}]", rule.primary_source())
}

/// Render the full SKILL.md body. Deterministic for fixed inputs.
pub(crate) fn render_skill_md(
    domain: &str,
    user: &str,
    description: &str,
    rules: &[Rule],
    compiled_at: f64,
    min_reinforcement: usize,
) -> String {
    let compiled_iso = DateTime::from_timestamp(compiled_at.floor() as i64, 0)
        .map(|t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_default();
    let of = |kind: &str| -> Vec<&Rule> { rules.iter().filter(|r| r.kind == kind).collect() };
    let (do_rules, watch_rules, dont_rules, ref_rules, feed_rules) =
        (of("do"), of("watch"), of("dont"), of("ref"), of("feed"));
    let exp_sources: BTreeSet<&String> = do_rules
        .iter()
        .chain(&watch_rules)
        .flat_map(|r| r.sources.iter())
        .collect();
    let (exp, dont) = (exp_sources.len(), dont_rules.len());

    let mut lines: Vec<String> = vec!["---".into()];
    lines.push(format!("name: {domain}-{user}"));
    lines.push(format!(
        "description: {}",
        crate::pyfmt::py_json(&Value::from(description))
    ));
    lines.push("generated: true".into());
    lines.push("source: omnimem".into());
    lines.push(format!("domain: {domain}"));
    lines.push(format!("compiled_at: {compiled_iso}"));
    lines.push(format!("contract_version: {CONTRACT_VERSION}"));
    let manifest = manifest_annotations(rules);
    if !manifest.is_empty() {
        lines.push("source_manifest:".into());
        for (key, annotation) in manifest {
            lines.push(format!("  - {key}   # {annotation}"));
        }
    }
    lines.push("---".into());
    lines.push(String::new());
    lines.push(generated_banner(domain));
    lines.push(String::new());
    lines.push(OPERATING_CONTRACT.into());
    lines.push(String::new());
    lines.push("## How I work  (compiled, domain-specific)".into());
    lines.push(String::new());
    lines.push(format!(
        "Distilled procedure from {exp} experience {} and {dont} graveyard {} in domain `{domain}`. \
         Each rule cites its source memory; treat Don't entries as known dead ends and \
         warn_if_abandoned before retrying one.",
        plural(exp, "memory", "memories"),
        plural(dont, "entry", "entries"),
    ));

    let section = |lines: &mut Vec<String>, title: &str, intro: Option<&str>, items: &[&Rule]| {
        if items.is_empty() {
            return;
        }
        lines.push(String::new());
        lines.push(title.to_owned());
        lines.push(String::new());
        if let Some(intro) = intro {
            lines.push(intro.to_owned());
            lines.push(String::new());
        }
        lines.extend(items.iter().map(|r| rule_bullet(r)));
    };
    section(&mut lines, "## Do", None, &do_rules);
    section(&mut lines, "## Watch out", None, &watch_rules);
    section(&mut lines, "## Don't (and why)", None, &dont_rules);
    section(
        &mut lines,
        "## Reference  (promoted knowledge)",
        Some(
            "Curated reference material promoted from the knowledge namespace \
             (promote_knowledge). These are vetted pointers, not lived experience — check the \
             cited article for the full text before treating one as procedure.",
        ),
        &ref_rules,
    );
    section(
        &mut lines,
        "## Feed watch  (influenced feeds)",
        Some(
            "The latest articles from RSS feeds tied to this domain, pulled in automatically at \
             compile time and weighted by each feed's influence score (1-10 — the score is how \
             many of its most recent articles a feed contributes). Unlike the Reference section \
             these are unvetted: treat them as current signal, not procedure, and \
             promote_knowledge() anything worth keeping before it ages out of the knowledge \
             namespace.",
        ),
        &feed_rules,
    );

    lines.push(String::new());
    lines.push("## Provenance".into());
    lines.push(String::new());
    let mut provenance = format!(
        "Compiled from {exp} experience {} (min {min_reinforcement} {}) and {dont} graveyard {}. ",
        plural(exp, "memory", "memories"),
        plural(min_reinforcement, "reinforcement", "reinforcements"),
        plural(dont, "entry", "entries"),
    );
    if !ref_rules.is_empty() {
        provenance.push_str(&format!(
            "Plus {} promoted reference {} from the knowledge namespace. ",
            ref_rules.len(),
            plural(ref_rules.len(), "article", "articles"),
        ));
    }
    if !feed_rules.is_empty() {
        let feed_names: BTreeSet<&str> = feed_rules
            .iter()
            .map(|r| r.feed.as_str())
            .filter(|f| !f.is_empty())
            .collect();
        provenance.push_str(&format!(
            "Plus {} feed-watch {} from {} influencing {} ({}). ",
            feed_rules.len(),
            plural(feed_rules.len(), "article", "articles"),
            feed_names.len(),
            plural(feed_names.len(), "feed", "feeds"),
            feed_names.into_iter().collect::<Vec<_>>().join(", "),
        ));
    }
    provenance.push_str(&format!(
        "Full source manifest in frontmatter. Operating contract at contract_version {CONTRACT_VERSION}."
    ));
    lines.push(provenance);
    lines.push(String::new());
    lines.join("\n")
}

/// `str.splitlines()`: every Unicode line boundary, `\r\n` as one.
fn python_lines(s: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = s.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if matches!(
            c,
            '\n' | '\r'
                | '\u{0b}'
                | '\u{0c}'
                | '\u{1c}'
                | '\u{1d}'
                | '\u{1e}'
                | '\u{85}'
                | '\u{2028}'
                | '\u{2029}'
        ) {
            lines.push(&s[start..i]);
            let mut end = i + c.len_utf8();
            if c == '\r' && chars.peek().map(|p| p.1) == Some('\n') {
                chars.next();
                end += 1;
            }
            start = end;
        }
    }
    if start < s.len() {
        lines.push(&s[start..]);
    }
    lines
}

/// Drop the `compiled_at` stamp so bodies compare on substance.
pub(crate) fn strip_volatile(body: &str) -> String {
    python_lines(body)
        .into_iter()
        .filter(|l| !l.starts_with("compiled_at: "))
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn body_sha(body: &str) -> String {
    sha256_hex(body.as_bytes())
}

pub(crate) fn bodies_equivalent(old: &str, new: &str) -> bool {
    strip_volatile(old) == strip_volatile(new)
}

pub(crate) fn render_unified_diff(old: &str, new: &str, skill_id: &str) -> String {
    TextDiff::from_lines(old, new)
        .unified_diff()
        .context_radius(3)
        .header(
            &format!("{skill_id} (stored)"),
            &format!("{skill_id} (proposed)"),
        )
        .to_string()
}

fn text_field<'a>(rule: &'a Value, name: &str) -> &'a str {
    rule.get(name).and_then(Value::as_str).unwrap_or("")
}

fn source_set(rule: &Value) -> BTreeSet<String> {
    rule.get("sources")
        .and_then(Value::as_array)
        .map(|s| {
            s.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Risk-classified change list between two rule manifests. Additions and
/// reinforcement growth are low risk; rewrites and removals are high, so
/// they can't slip through a batch accept. Feed churn is always low.
pub(crate) fn summarise_rule_changes(old_rules: &[Value], new_rules: &[Value]) -> Vec<Value> {
    let mut changes: Vec<Map<String, Value>> = Vec::new();
    let mut add = |change: &str, risk: &str, rule: &Value, was: Option<&str>| {
        let kind = text_field(rule, "kind");
        let risk = if kind == "feed" { "low" } else { risk };
        let name = text_field(rule, "name");
        let label = if kind == "dont" && !name.is_empty() {
            format!("Avoid {name}")
        } else {
            text_field(rule, "text").to_owned()
        };
        let mut entry = Map::new();
        entry.insert("change".into(), change.into());
        entry.insert("risk".into(), risk.into());
        entry.insert("rule_kind".into(), kind.into());
        entry.insert("rule".into(), gist(&label, 72).into());
        if let Some(was) = was.filter(|w| !w.is_empty()) {
            entry.insert("was".into(), gist(was, 72).into());
        }
        changes.push(entry);
    };

    for kind in ["do", "watch", "dont", "ref", "feed"] {
        let olds: Vec<&Value> = old_rules
            .iter()
            .filter(|r| text_field(r, "kind") == kind)
            .collect();
        let news: Vec<&Value> = new_rules
            .iter()
            .filter(|r| text_field(r, "kind") == kind)
            .collect();
        let mut matched_old: BTreeSet<usize> = BTreeSet::new();
        for new in &news {
            let mut match_idx = None;
            if kind == "dont" {
                let new_name = text_field(new, "name").to_lowercase();
                match_idx = olds.iter().enumerate().find_map(|(i, old)| {
                    (!matched_old.contains(&i)
                        && text_field(old, "name").to_lowercase() == new_name)
                        .then_some(i)
                });
            } else {
                let new_sources = source_set(new);
                let mut best_overlap = 0;
                for (i, old) in olds.iter().enumerate() {
                    if matched_old.contains(&i) {
                        continue;
                    }
                    let overlap = new_sources.intersection(&source_set(old)).count();
                    if overlap > best_overlap {
                        best_overlap = overlap;
                        match_idx = Some(i);
                    }
                }
                if match_idx.is_none() {
                    match_idx = olds.iter().enumerate().find_map(|(i, old)| {
                        (!matched_old.contains(&i) && old.get("text") == new.get("text"))
                            .then_some(i)
                    });
                }
            }
            let Some(i) = match_idx else {
                add("added", "low", new, None);
                continue;
            };
            matched_old.insert(i);
            let old = olds[i];
            if old.get("text") != new.get("text") {
                add("rewritten", "high", new, Some(text_field(old, "text")));
            } else if source_set(old) != source_set(new) {
                add("reinforced", "low", new, None);
            }
        }
        for (i, old) in olds.iter().enumerate() {
            if !matched_old.contains(&i) {
                add("removed", "high", old, None);
            }
        }
    }

    let risk_rank = |c: &Map<String, Value>| u8::from(c["risk"] != "high");
    let text = |c: &Map<String, Value>, f: &str| c[f].as_str().unwrap_or("").to_owned();
    changes.sort_by(|a, b| {
        risk_rank(a)
            .cmp(&risk_rank(b))
            .then_with(|| text(a, "rule_kind").cmp(&text(b, "rule_kind")))
            .then_with(|| text(a, "rule").cmp(&text(b, "rule")))
    });
    changes.into_iter().map(Value::Object).collect()
}

impl Engine {
    /// Candidate domain vocabulary: episodic tags plus existing skill
    /// domains, with occurrence counts, in first-seen order.
    pub(crate) fn known_domains(&self) -> Result<Vec<(String, usize)>> {
        let mut counts: Vec<(String, usize)> = Vec::new();
        let mut index: HashMap<String, usize> = HashMap::new();
        let mut bump = |name: String| {
            if let Some(i) = index.get(&name) {
                counts[*i].1 += 1
            } else {
                index.insert(name.clone(), counts.len());
                counts.push((name, 1));
            }
        };
        let mut keys = self.store.scan_prefix("mem:episodic:")?;
        keys.truncate(MAX_POOL_KEYS);
        for row in self
            .store
            .get_fields_multi(&keys, &["tags", "state"])?
            .into_iter()
            .flatten()
        {
            if !active_or_unset(&row) {
                continue;
            }
            for tag in parse_tags(row.get("tags").map(String::as_str)) {
                bump(tag.to_lowercase());
            }
        }
        let skill_keys = self.store.scan_prefix(SKILL_KEY_PREFIX)?;
        for row in self
            .store
            .get_fields_multi(&skill_keys, &["domain"])?
            .into_iter()
            .flatten()
        {
            if let Some(domain) = row.get("domain").filter(|d| !d.is_empty()) {
                bump(domain.clone());
            }
        }
        Ok(counts)
    }

    /// Did-you-mean guard: the closest existing domain to a new one.
    pub(crate) fn suggest_similar_domain(
        &self,
        domain: &str,
        candidates: &[String],
    ) -> Result<Option<(String, f64)>> {
        let pool: BTreeSet<String> = candidates
            .iter()
            .filter(|c| !c.is_empty())
            .map(|c| c.to_lowercase())
            .filter(|c| c != domain)
            .collect();
        if pool.is_empty() {
            return Ok(None);
        }
        if let Some(candidate) = pool
            .iter()
            .find(|c| domain.contains(c.as_str()) || c.contains(domain))
        {
            return Ok(Some((candidate.clone(), 0.9)));
        }
        let pool: Vec<&str> = pool.iter().take(200).map(String::as_str).collect();
        let target = self.embed(domain)?;
        let mut best: Option<(usize, f32)> = None;
        for (offset, chunk) in pool.chunks(32).enumerate() {
            for (i, vector) in self.embed_many(chunk)?.iter().enumerate() {
                let sim: f32 = target.iter().zip(vector).map(|(a, b)| a * b).sum();
                if best.is_none_or(|(_, b)| sim > b) {
                    best = Some((offset * 32 + i, sim));
                }
            }
        }
        Ok(best
            .filter(|(_, sim)| f64::from(*sim) >= self.config.skill_domain_suggest_threshold)
            .map(|(i, sim)| (pool[i].to_owned(), round_to(f64::from(sim), 4))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_line_collapses_and_trims_with_an_ellipsis() {
        assert_eq!(one_line("  a\n\n b\t c ", 400), "a b c");
        assert_eq!(one_line("abcdef ghij", 8), "abcdef…");
    }

    #[test]
    fn tags_parse_like_python() {
        assert_eq!(
            parse_tags(Some(r#"["Python", "", null, 3]"#)),
            ["Python", "3"]
        );
        assert_eq!(parse_tags(Some("rust, go")), ["rust", "go"]);
        assert!(parse_tags(Some("12")).is_empty());
    }

    #[test]
    fn clusters_are_components_ordered_by_first_member() {
        let v = |x: f32, y: f32| vec![x, y];
        let clusters = cluster_indices(&[v(1.0, 0.0), v(0.0, 1.0), v(1.0, 0.0), v(0.0, 1.0)], 0.8);
        assert_eq!(clusters, vec![vec![0, 2], vec![1, 3]]);
    }

    #[test]
    fn strip_volatile_drops_only_the_stamp() {
        let a = "---\ncompiled_at: 2026-01-01T00:00:00Z\nname: x\n";
        let b = "---\ncompiled_at: 2027-01-01T00:00:00Z\nname: x\n";
        assert!(bodies_equivalent(a, b));
        assert!(!bodies_equivalent(a, "---\nname: y\n"));
    }

    #[test]
    fn reference_rules_validate_with_6x_messages() {
        let bad = json!([{"kind": "maybe", "text": "x"}]);
        assert_eq!(
            validate_reference_rules(&bad).unwrap_err(),
            "rules kind must be one of do/watch/dont/note, got 'maybe'"
        );
        let ok = json!([{"kind": " DO ", "text": " pin versions "}]);
        assert_eq!(
            validate_reference_rules(&ok).unwrap(),
            [ReferenceRule {
                kind: "do".into(),
                text: "pin versions".into()
            }]
        );
    }

    #[test]
    fn rewrites_and_removals_are_high_risk() {
        let old = [
            json!({"kind": "do", "text": "a", "sources": ["k1"]}),
            json!({"kind": "dont", "text": "slow", "name": "Celery", "sources": ["k2"]}),
        ];
        let new = [
            json!({"kind": "do", "text": "b", "sources": ["k1"]}),
            json!({"kind": "watch", "text": "w", "sources": ["k3"]}),
        ];
        let changes = summarise_rule_changes(&old, &new);
        let summary: Vec<(String, String, String)> = changes
            .iter()
            .map(|c| {
                (
                    c["change"].as_str().unwrap().into(),
                    c["risk"].as_str().unwrap().into(),
                    c["rule"].as_str().unwrap().into(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                ("rewritten".into(), "high".into(), "b".into()),
                ("removed".into(), "high".into(), "Avoid Celery".into()),
                ("added".into(), "low".into(), "w".into()),
            ]
        );
        assert_eq!(changes[0]["was"], "a");
    }
}
