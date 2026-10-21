//! Skill transfer bundles (`memory/skill_transfer.py`): a compiled skill and
//! its source memories as a checksummed zip, and a strictly additive import.
//!
//! A manifest carries the format version and a sha256 for every payload
//! file; the skill travels as machine fields (`skill.json`) and as the
//! readable `SKILL.md`; each source memory is one JSON file. Import never
//! overwrites an existing key, so replaying a bundle only adds what's
//! missing. Vectors never travel: the importing instance re-embeds. Format 2
//! adds `feeds.json`, the feeds influencing the skill's domain.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::{Cursor, Read, Write};
use std::sync::LazyLock;

use chrono::{Local, Utc};
use omnimem_core::classification::{
    LICENCE_OPEN, LICENCE_RESTRICTED, LICENCE_UNKNOWN, PROVENANCE_CLASSES,
};
use omnimem_store::{Fields, discovery_text};
use regex::Regex;
use serde_json::{Map, Value, json};
use tracing::{debug, info};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::classification::{MAX_LICENCE_NOTE, default_provenance, resolve_licence};
use crate::domains::is_valid_domain;
use crate::feeds::validate_feed_skills;
use crate::pyfmt::now_str;
use crate::skills::{
    SKILL_KEY_PREFIX, generated_skill_key, parse_string_list, py_repr, py_str, sha256_hex,
};
use crate::{Engine, Result};

pub const EXPORT_FORMAT: &str = "omnimem-skill-export";
pub const EXPORT_FORMAT_VERSION: i64 = 2;
const SUPPORTED_FORMAT_VERSIONS: [i64; 2] = [1, 2];

const MAX_ZIP_BYTES: usize = 20 * 1024 * 1024;
const MAX_TOTAL_UNCOMPRESSED: u64 = 50 * 1024 * 1024;
const MAX_ENTRY_UNCOMPRESSED: u64 = 1024 * 1024;
const MAX_MEMORIES: usize = 500;
const MAX_FIELDS_PER_MEMORY: usize = 64;
const MAX_FIELD_NAME: usize = 64;
const MAX_FIELD_VALUE: usize = 100_000;
const MAX_CONTENT: usize = 50_000;
const MAX_SKILL_BODY: usize = 100_000;
const MAX_BUNDLE_FEEDS: usize = 50;
const MAX_FEED_NAME: usize = 200;
const MAX_FEED_URL: usize = 2000;
const MAX_FEED_TOPICS: usize = 20;
const MAX_FEED_TOPIC_CHARS: usize = 100;
const MAX_FEED_PROJECT: usize = 100;

static MEMORY_KEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^mem:(episodic|knowledge):[0-9A-Za-z][0-9A-Za-z._\-]{0,80}$").expect("valid")
});
static MEMORY_ENTRY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^memories/[0-9]{4}\.json$").expect("valid"));
static UNSAFE_FILENAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[^0-9A-Za-z._\-]").expect("valid"));

/// Per-instance telemetry and binary data never travel.
const INSTANCE_LOCAL_FIELDS: [&str; 3] = ["vector", "recall_count", "last_recalled"];

/// What a bundled memory may bring in: what it says about itself in words.
/// What the exporting instance decided about it (blessing, ranking scores,
/// lifecycle state, lineage to its own records, identity stamps) stays
/// behind, because the importing instance has no way to check any of it
/// and would otherwise rank and trust a stranger's memory on its say-so.
/// `licence` and `provenance` are on the list but re-checked on apply.
const IMPORTABLE_MEMORY_FIELDS: [&str; 20] = [
    "content",
    "tags",
    "project",
    "project_name",
    "title",
    "source_url",
    "published_at",
    "event_date",
    "topics",
    "outcome",
    "iterations",
    "breakthrough",
    "gotchas",
    "lesson",
    "abandoned_approaches",
    "created_at",
    "updated_at",
    "licence",
    "licence_note",
    "provenance",
];

/// What a compiled skill legitimately carries: its identity, its text and
/// the manifests saying what it was compiled from. Blessing, surface score
/// and domain routing are the importing instance's to decide.
const IMPORTABLE_SKILL_FIELDS: [&str; 13] = [
    "name",
    "description",
    "domain",
    "user",
    "body",
    "generated",
    "contract_version",
    "compiled_at",
    "created_at",
    "updated_at",
    "tags",
    "source_manifest",
    "rule_manifest",
];

fn strip_instance_local(fields: &Fields) -> Fields {
    fields
        .iter()
        .filter(|(k, _)| !INSTANCE_LOCAL_FIELDS.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// Keep the fields on `allowed`; the rest are logged and dropped.
fn keep_importable(fields: Fields, allowed: &[&str], entry: &str) -> Fields {
    fields
        .into_iter()
        .filter(|(name, _)| {
            let keep = allowed.contains(&name.as_str());
            if !keep {
                debug!(entry, field = %name, "dropping a field an import may not carry");
            }
            keep
        })
        .collect()
}

fn chars(s: &str) -> usize {
    s.chars().count()
}

/// A finished export.
#[derive(Debug, Clone)]
pub struct SkillExport {
    pub data: Vec<u8>,
    pub filename: String,
    pub memory_count: usize,
    pub missing_sources: Vec<String>,
}

/// A bundle that passed every check, ready to plan or apply.
#[derive(Debug, Clone)]
pub struct ValidatedBundle {
    pub skill_key: String,
    pub skill_fields: Fields,
    pub memories: Vec<(String, Fields)>,
    /// Feed entries as a reading list holds them.
    pub feeds: Vec<Map<String, Value>>,
    pub manifest: Value,
    pub warnings: Vec<String>,
}

fn zip_bytes(files: &[(String, Vec<u8>)]) -> std::result::Result<Vec<u8>, String> {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for (name, data) in files {
        writer
            .start_file(name.as_str(), options)
            .and_then(|()| writer.write_all(data).map_err(Into::into))
            .map_err(|e| format!("Could not build the bundle: {e}"))?;
    }
    writer
        .finish()
        .map(Cursor::into_inner)
        .map_err(|e| format!("Could not build the bundle: {e}"))
}

impl Engine {
    /// Bundle a skill and its source memories into zip bytes.
    pub fn build_skill_export(
        &self,
        key: &str,
    ) -> Result<std::result::Result<SkillExport, String>> {
        if !key.starts_with(SKILL_KEY_PREFIX) {
            return Ok(Err("Not a skill key".to_owned()));
        }
        let Some(skill) = self.store.get(key)? else {
            return Ok(Err("Skill not found".to_owned()));
        };
        if skill.get("generated").map(String::as_str) != Some("true") {
            return Ok(Err("Only generated skills can be exported".to_owned()));
        }
        let skill_fields = strip_instance_local(&skill);
        let source_keys: Vec<String> = parse_string_list(skill.get("source_manifest"))
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();

        let mut memories: Vec<(String, Fields)> = Vec::new();
        let mut missing = Vec::new();
        for (source_key, row) in source_keys.iter().zip(self.store.get_multi(&source_keys)?) {
            match row {
                Some(row) if MEMORY_KEY.is_match(source_key) => {
                    memories.push((source_key.clone(), strip_instance_local(&row)));
                }
                _ => missing.push(source_key.clone()),
            }
        }

        let pretty = |v: &Value| serde_json::to_vec_pretty(v).unwrap_or_default();
        let skill_json: BTreeMap<&String, &String> = skill_fields.iter().collect();
        let mut files: Vec<(String, Vec<u8>)> = vec![
            ("skill.json".into(), pretty(&json!(skill_json))),
            (
                "SKILL.md".into(),
                skill_fields
                    .get("body")
                    .cloned()
                    .unwrap_or_default()
                    .into_bytes(),
            ),
        ];
        for (i, (key, fields)) in memories.iter().enumerate() {
            let sorted: BTreeMap<&String, &String> = fields.iter().collect();
            let mut entry = Map::new();
            entry.insert("fields".into(), json!(sorted));
            entry.insert("key".into(), key.as_str().into());
            files.push((
                format!("memories/{i:04}.json"),
                pretty(&Value::Object(entry)),
            ));
        }
        let domain = skill_fields.get("domain").cloned().unwrap_or_default();
        let feeds = self.influencing_feeds(&domain);
        if !feeds.is_empty() {
            files.push(("feeds.json".into(), pretty(&Value::Array(feeds.clone()))));
        }

        let field = |name: &str| skill_fields.get(name).cloned().unwrap_or_default();
        let checksums: Map<String, Value> = files
            .iter()
            .map(|(name, data)| (name.clone(), sha256_hex(data).into()))
            .collect();
        let mut manifest = Map::new();
        manifest.insert("format".into(), EXPORT_FORMAT.into());
        manifest.insert("format_version".into(), EXPORT_FORMAT_VERSION.into());
        manifest.insert(
            "exported_at".into(),
            Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string().into(),
        );
        manifest.insert("omnimem_version".into(), env!("CARGO_PKG_VERSION").into());
        manifest.insert("skill_key".into(), key.into());
        manifest.insert("name".into(), field("name").into());
        manifest.insert("domain".into(), domain.into());
        manifest.insert("user".into(), field("user").into());
        manifest.insert("memory_count".into(), memories.len().into());
        manifest.insert("missing_sources".into(), json!(missing));
        manifest.insert("feed_count".into(), feeds.len().into());
        manifest.insert("checksums".into(), Value::Object(checksums));
        files.insert(
            0,
            ("manifest.json".into(), pretty(&Value::Object(manifest))),
        );

        let data = match zip_bytes(&files) {
            Ok(data) => data,
            Err(e) => return Ok(Err(e)),
        };
        let name = Some(field("name"))
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| "skill".into());
        let filename = format!(
            "omnimem_skill_{}_{}.zip",
            UNSAFE_FILENAME.replace_all(&name, "_"),
            Local::now().format("%Y%m%d_%H%M%S")
        );
        info!(
            key,
            memories = memories.len(),
            missing = missing.len(),
            "exported skill"
        );
        Ok(Ok(SkillExport {
            data,
            filename,
            memory_count: memories.len(),
            missing_sources: missing,
        }))
    }

    /// Feeds whose mapping influences this domain; only this domain's score travels.
    fn influencing_feeds(&self, domain: &str) -> Vec<Value> {
        if domain.is_empty() {
            return Vec::new();
        }
        self.load_feed_influences()
            .into_iter()
            .filter_map(|(name, entry)| {
                let score = entry.skills.iter().find(|(d, _)| d == domain)?.1;
                // Keys in sorted order, as 6.x wrote them.
                let mut feed = Map::new();
                if let Some(licence) = &entry.licence {
                    feed.insert("licence".into(), licence.as_str().into());
                    if let Some(note) = &entry.licence_note {
                        feed.insert("licence_note".into(), note.as_str().into());
                    }
                }
                if let Some(mode) = &entry.mode {
                    feed.insert("mode".into(), mode.as_str().into());
                }
                feed.insert("name".into(), name.into());
                if let Some(project) = &entry.project {
                    feed.insert("project".into(), project.as_str().into());
                }
                feed.insert("skills".into(), json!({domain: score}));
                if !entry.topics.is_empty() {
                    feed.insert("topics".into(), json!(entry.topics));
                }
                feed.insert("url".into(), entry.url.into());
                Some(Value::Object(feed))
            })
            .collect()
    }

    /// Preview what applying a bundle would do. Read-only.
    pub fn plan_skill_import(
        &self,
        bundle: &ValidatedBundle,
        current_feeds: Option<&[Map<String, Value>]>,
    ) -> Result<Value> {
        let skill_exists = self.store.get(&bundle.skill_key)?.is_some();
        let (mut new_memories, mut existing_memories) = (Vec::new(), Vec::new());
        for (key, _) in &bundle.memories {
            if self.store.get(key)?.is_some() {
                existing_memories.push(key.clone());
            } else {
                new_memories.push(key.clone());
            }
        }
        let (mut added, mut updated, mut skipped) = (Vec::new(), Vec::new(), Vec::new());
        if !bundle.feeds.is_empty()
            && let Some(current) = current_feeds
        {
            (_, added, updated, skipped) = merge_feed_influences(current, &bundle.feeds);
        }
        Ok(json!({
            "skill_key": bundle.skill_key,
            "skill_exists": skill_exists,
            "new_memories": new_memories,
            "existing_memories": existing_memories,
            "new_feeds": added,
            "updated_feeds": updated,
            "skipped_feeds": skipped,
        }))
    }

    /// Write a validated bundle. Strictly additive, re-checked per key.
    pub fn apply_skill_import(&self, bundle: &ValidatedBundle) -> Result<Value> {
        let now = now_str();
        let (mut written, mut skipped) = (Vec::new(), Vec::new());
        for (key, bundled) in &bundle.memories {
            if self.store.get(key)?.is_some() {
                skipped.push(key.clone());
                continue;
            }
            let namespace = key.split(':').nth(1).unwrap_or("");
            // Only the descriptive fields survived validation; the store
            // stamps origin, epoch, classification and content hash afresh
            // on the write, as for any new memory.
            let mut fields = bundled.clone();
            fields.insert("state".into(), "active".into());
            // An imported memory is someone else's work: a bundled "own" is
            // the exporter's. Open and restricted travel; the rest is unknown.
            let licence = fields.get("licence").map(String::as_str);
            if licence != Some(LICENCE_OPEN) && licence != Some(LICENCE_RESTRICTED) {
                fields.insert("licence".into(), LICENCE_UNKNOWN.into());
                fields.remove("licence_note");
            }
            if !fields
                .get("provenance")
                .is_some_and(|p| PROVENANCE_CLASSES.contains(&p.as_str()))
            {
                fields.insert("provenance".into(), default_provenance(namespace).into());
            }
            fields.insert("imported_at".into(), now.clone());
            let vector = self.embed(fields.get("content").map_or("", String::as_str))?;
            self.store.upsert(key, &fields, Some(&vector))?;
            written.push(key.clone());
        }

        let mut skill_written = false;
        if self.store.get(&bundle.skill_key)?.is_none() {
            // Unblessed and at the default surface score, as a skill compiled
            // here would start.
            let mut fields = bundle.skill_fields.clone();
            fields.insert("state".into(), "active".into());
            fields.insert("imported_at".into(), now);
            let field = |name: &str| fields.get(name).cloned().unwrap_or_default();
            let vector = self.embed(&discovery_text(
                &field("name"),
                &field("description"),
                &field("domain"),
            ))?;
            self.store
                .upsert(&bundle.skill_key, &fields, Some(&vector))?;
            skill_written = true;
        }
        info!(
            skill = %bundle.skill_key,
            skill_written,
            written = written.len(),
            skipped = skipped.len(),
            "imported skill bundle"
        );
        Ok(json!({
            "skill_key": bundle.skill_key,
            "skill_written": skill_written,
            "memories_written": written,
            "memories_skipped": skipped,
        }))
    }
}

/// Read one entry without trusting its header. The declared size was
/// checked against the cap already, but a deflate stream can inflate to
/// far more than its header claims, so the stream itself is cut at the cap
/// and must produce exactly the declared number of bytes.
fn read_entry(
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    name: &str,
) -> std::result::Result<Vec<u8>, String> {
    let unreadable = || format!("Could not read {name}");
    let mut file = archive.by_name(name).map_err(|_| unreadable())?;
    let declared = file.size();
    let mut data = Vec::new();
    (&mut file)
        .take(MAX_ENTRY_UNCOMPRESSED + 1)
        .read_to_end(&mut data)
        .map_err(|_| unreadable())?;
    if data.len() as u64 > MAX_ENTRY_UNCOMPRESSED {
        return Err(format!("Bundle entry {name} is too large"));
    }
    if data.len() as u64 != declared {
        return Err(format!(
            "Bundle entry {name} does not match its declared size"
        ));
    }
    Ok(data)
}

fn validate_memory(raw: &[u8], entry: &str) -> std::result::Result<(String, Fields), String> {
    let parsed: Value = std::str::from_utf8(raw)
        .ok()
        .and_then(|s| serde_json::from_str(s).ok())
        .ok_or_else(|| format!("{entry} is not valid JSON"))?;
    let Value::Object(parsed) = parsed else {
        return Err(format!("{entry} must be a JSON object"));
    };
    let key = parsed
        .get("key")
        .and_then(Value::as_str)
        .filter(|k| MEMORY_KEY.is_match(k))
        .ok_or_else(|| format!("{entry} has an invalid memory key"))?;
    let Some(Value::Object(raw_fields)) = parsed.get("fields") else {
        return Err(format!("{entry} has no fields object"));
    };
    if raw_fields.len() > MAX_FIELDS_PER_MEMORY {
        return Err(format!(
            "{entry} has too many fields (max {MAX_FIELDS_PER_MEMORY})"
        ));
    }
    let mut fields = Fields::new();
    for (name, value) in raw_fields {
        if name.is_empty() || chars(name) > MAX_FIELD_NAME {
            return Err(format!("{entry} has an invalid field name"));
        }
        let Some(value) = value.as_str() else {
            return Err(format!("{entry} field '{name}' is not a string"));
        };
        if chars(value) > MAX_FIELD_VALUE {
            return Err(format!("{entry} field '{name}' is too large"));
        }
        fields.insert(name.clone(), value.to_owned());
    }
    let content = fields.get("content").map_or("", String::as_str);
    if content.trim().is_empty() {
        return Err(format!(
            "{entry} has no content — nothing to embed on import"
        ));
    }
    if chars(content) > MAX_CONTENT {
        return Err(format!("{entry} content exceeds {MAX_CONTENT} chars"));
    }
    Ok((
        key.to_owned(),
        keep_importable(fields, &IMPORTABLE_MEMORY_FIELDS, entry),
    ))
}

fn validate_feeds(
    raw: Option<Vec<u8>>,
    domain: &str,
) -> std::result::Result<Vec<Map<String, Value>>, String> {
    let raw = raw.ok_or("Could not read feeds.json")?;
    let parsed: Value = std::str::from_utf8(&raw)
        .ok()
        .and_then(|s| serde_json::from_str(s).ok())
        .ok_or("feeds.json is not valid JSON")?;
    let Value::Array(entries) = parsed else {
        return Err("feeds.json must be a list of feed entries".to_owned());
    };
    if entries.len() > MAX_BUNDLE_FEEDS {
        return Err(format!(
            "feeds.json carries too many feeds (max {MAX_BUNDLE_FEEDS})"
        ));
    }
    let mut feeds = Vec::new();
    let mut seen_urls = HashSet::new();
    for (i, entry) in entries.iter().enumerate() {
        let label = format!("feeds.json entry {i}");
        let Value::Object(entry) = entry else {
            return Err(format!("{label} must be an object"));
        };
        let name = entry
            .get("name")
            .and_then(Value::as_str)
            .filter(|n| !n.trim().is_empty() && chars(n) <= MAX_FEED_NAME)
            .ok_or_else(|| format!("{label} has an invalid name"))?;
        let url = entry
            .get("url")
            .and_then(Value::as_str)
            .filter(|u| {
                chars(u) <= MAX_FEED_URL && (u.starts_with("http://") || u.starts_with("https://"))
            })
            .ok_or_else(|| format!("{label} has an invalid url (must be http or https)"))?;
        if !seen_urls.insert(url.to_owned()) {
            return Err(format!("feeds.json lists the url {url} twice"));
        }
        let skills =
            validate_feed_skills(entry.get("skills")).map_err(|e| format!("{label}: {e}"))?;
        if skills.len() != 1 || skills[0].0 != domain {
            return Err(format!(
                "{label} must carry exactly one influence entry, for the bundle's domain '{domain}'"
            ));
        }

        let mut feed = Map::new();
        feed.insert("name".into(), name.trim().into());
        feed.insert("url".into(), url.into());
        feed.insert("skills".into(), json!({ &skills[0].0: skills[0].1 }));

        if let Some(topics) = entry.get("topics").filter(|t| crate::skills::py_truthy(t)) {
            let valid = topics.as_array().filter(|items| {
                items.len() <= MAX_FEED_TOPICS
                    && items.iter().all(|t| {
                        t.as_str()
                            .is_some_and(|s| !s.is_empty() && chars(s) <= MAX_FEED_TOPIC_CHARS)
                    })
            });
            let Some(topics) = valid else {
                return Err(format!("{label} has invalid topics"));
            };
            feed.insert("topics".into(), Value::Array(topics.clone()));
        }
        if let Some(mode) = entry.get("mode").filter(|m| !m.is_null()) {
            if !matches!(mode.as_str(), Some("summary" | "digest")) {
                return Err(format!("{label} has an invalid mode (summary or digest)"));
            }
            feed.insert("mode".into(), mode.clone());
        }
        if let Some(project) = entry.get("project").filter(|p| !p.is_null()) {
            let project = project
                .as_str()
                .filter(|p| chars(p) <= MAX_FEED_PROJECT)
                .ok_or_else(|| format!("{label} has an invalid project label"))?;
            if !project.is_empty() {
                feed.insert("project".into(), project.into());
            }
        }
        if let Some(licence) = entry.get("licence").filter(|l| !l.is_null()) {
            let licence = licence
                .as_str()
                .filter(|l| resolve_licence(l).is_ok())
                .ok_or_else(|| format!("{label} has an unrecognised licence"))?;
            if !licence.is_empty() {
                feed.insert("licence".into(), licence.into());
                if let Some(note) = entry.get("licence_note").filter(|n| !n.is_null()) {
                    let note = note
                        .as_str()
                        .filter(|n| chars(n) <= MAX_LICENCE_NOTE)
                        .ok_or_else(|| format!("{label} has an invalid licence note"))?;
                    if !note.is_empty() {
                        feed.insert("licence_note".into(), note.into());
                    }
                }
            }
        }
        feeds.push(feed);
    }
    Ok(feeds)
}

/// Validate uploaded bundle bytes without touching the store. Every check
/// runs before anything is trusted.
pub fn validate_skill_import(data: &[u8]) -> std::result::Result<ValidatedBundle, String> {
    if data.is_empty() {
        return Err("The uploaded file is empty".to_owned());
    }
    if data.len() > MAX_ZIP_BYTES {
        return Err(format!(
            "Bundle too large (max {} MB)",
            MAX_ZIP_BYTES / (1024 * 1024)
        ));
    }
    let mut archive =
        ZipArchive::new(Cursor::new(data)).map_err(|_| "Not a valid zip file".to_owned())?;

    let mut names = Vec::new();
    let mut total = 0u64;
    for i in 0..archive.len() {
        let file = archive
            .by_index_raw(i)
            .map_err(|_| "Not a valid zip file".to_owned())?;
        if file.is_dir() {
            continue;
        }
        let name = file.name().to_owned();
        if file.encrypted() {
            return Err("Encrypted zip entries are not supported".to_owned());
        }
        if file.size() > MAX_ENTRY_UNCOMPRESSED {
            return Err(format!("Bundle entry {name} is too large"));
        }
        total += file.size();
        names.push(name);
    }
    if names.len() != names.iter().collect::<HashSet<_>>().len() {
        return Err("Bundle contains duplicate entries".to_owned());
    }
    if total > MAX_TOTAL_UNCOMPRESSED {
        return Err("Bundle expands too large".to_owned());
    }

    let mut payload = Vec::new();
    for name in &names {
        if name == "manifest.json" {
            continue;
        }
        if matches!(name.as_str(), "skill.json" | "SKILL.md" | "feeds.json")
            || MEMORY_ENTRY.is_match(name)
        {
            payload.push(name.clone());
        } else {
            return Err(format!("Unexpected entry in bundle: {name}"));
        }
    }
    let has = |n: &str| names.iter().any(|x| x == n);
    if !has("manifest.json") {
        return Err("Bundle has no manifest.json — not an OmniMem skill export".to_owned());
    }
    if !has("skill.json") || !has("SKILL.md") {
        return Err("Bundle is missing skill.json or SKILL.md".to_owned());
    }

    // The declared sizes summed under the cap; the bytes actually produced
    // are counted too, in case the headers lied.
    let mut inflated = 0u64;
    let raw_manifest = read_entry(&mut archive, "manifest.json")?;
    inflated += raw_manifest.len() as u64;
    let manifest: Value = std::str::from_utf8(&raw_manifest)
        .ok()
        .and_then(|s| serde_json::from_str(s).ok())
        .ok_or("manifest.json is not valid JSON")?;
    let Value::Object(manifest) = manifest else {
        return Err("manifest.json must be a JSON object".to_owned());
    };
    if manifest.get("format").and_then(Value::as_str) != Some(EXPORT_FORMAT) {
        return Err("Not an OmniMem skill export (wrong format marker)".to_owned());
    }
    let version = manifest
        .get("format_version")
        .cloned()
        .unwrap_or(Value::Null);
    let supported = version
        .as_f64()
        .filter(|v| v.fract() == 0.0)
        .is_some_and(|v| SUPPORTED_FORMAT_VERSIONS.contains(&(v as i64)));
    if !supported {
        return Err(format!(
            "Unsupported export format version {} (this instance reads versions {})",
            py_repr(&version),
            SUPPORTED_FORMAT_VERSIONS.map(|v| v.to_string()).join(", ")
        ));
    }
    let Some(Value::Object(checksums)) = manifest.get("checksums") else {
        return Err("manifest.json has no checksums".to_owned());
    };
    let mut contents: HashMap<String, Vec<u8>> = HashMap::new();
    for name in &payload {
        let raw = read_entry(&mut archive, name)?;
        inflated += raw.len() as u64;
        if inflated > MAX_TOTAL_UNCOMPRESSED {
            return Err("Bundle expands too large".to_owned());
        }
        if checksums.get(name).and_then(Value::as_str) != Some(sha256_hex(&raw).as_str()) {
            return Err(format!(
                "Checksum mismatch on {name} — the bundle is corrupt or was modified after export"
            ));
        }
        contents.insert(name.clone(), raw);
    }

    let skill_value: Value = std::str::from_utf8(&contents["skill.json"])
        .ok()
        .and_then(|s| serde_json::from_str(s).ok())
        .ok_or("skill.json is not valid JSON")?;
    let Value::Object(skill_map) = skill_value else {
        return Err("skill.json must be a JSON object".to_owned());
    };
    let mut skill_fields = Fields::new();
    for (name, value) in &skill_map {
        let Some(value) = value.as_str() else {
            return Err("skill.json fields must all be strings".to_owned());
        };
        if name != "body" && chars(value) > MAX_FIELD_VALUE {
            return Err(format!("skill.json field '{name}' is too large"));
        }
        skill_fields.insert(name.clone(), value.to_owned());
    }
    let skill_fields = keep_importable(skill_fields, &IMPORTABLE_SKILL_FIELDS, "skill.json");
    let field = |name: &str| skill_fields.get(name).cloned().unwrap_or_default();
    let (domain, user) = (field("domain"), field("user"));
    if !is_valid_domain(&domain) || !is_valid_domain(&user) {
        return Err("skill.json has an invalid domain or user".to_owned());
    }
    if field("name").is_empty() {
        return Err("skill.json has no name".to_owned());
    }
    if field("generated") != "true" {
        return Err(
            "Only generated skills can be imported — the bundle's skill is not flagged generated:true"
                .to_owned(),
        );
    }
    let body = field("body");
    if body.trim().is_empty() {
        return Err("skill.json has an empty body".to_owned());
    }
    if chars(&body) > MAX_SKILL_BODY {
        return Err(format!("Skill body exceeds {MAX_SKILL_BODY} chars"));
    }
    if String::from_utf8_lossy(&contents["SKILL.md"]) != body {
        return Err("SKILL.md does not match the skill body in skill.json".to_owned());
    }
    let skill_key = generated_skill_key(&domain, &user);
    if manifest.get("skill_key").and_then(Value::as_str) != Some(skill_key.as_str()) {
        return Err("manifest skill_key does not match the skill's domain and user".to_owned());
    }
    for name in ["rule_manifest", "source_manifest"] {
        let raw = field(name);
        if raw.is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(&raw) {
            Err(_) => return Err(format!("skill.json {name} is not valid JSON")),
            Ok(v) if !v.is_array() => return Err(format!("skill.json {name} is not a list")),
            Ok(_) => {}
        }
    }

    let feeds = if has("feeds.json") {
        validate_feeds(contents.remove("feeds.json"), &domain)?
    } else {
        Vec::new()
    };

    let mut memory_names: Vec<&String> = payload
        .iter()
        .filter(|n| n.starts_with("memories/"))
        .collect();
    memory_names.sort();
    if memory_names.len() > MAX_MEMORIES {
        return Err(format!(
            "Bundle carries too many memories (max {MAX_MEMORIES})"
        ));
    }
    let mut memories = Vec::new();
    let mut seen_keys: BTreeSet<String> = BTreeSet::new();
    for name in memory_names {
        let (key, fields) = validate_memory(&contents[name], name)?;
        if !seen_keys.insert(key.clone()) {
            return Err(format!("Bundle contains memory key {key} twice"));
        }
        memories.push((key, fields));
    }

    let declared_count = manifest.get("memory_count").and_then(Value::as_f64);
    if declared_count != Some(memories.len() as f64) {
        return Err("manifest memory_count does not match the bundled memories".to_owned());
    }
    let declared: BTreeSet<String> = parse_string_list(skill_fields.get("source_manifest"))
        .into_iter()
        .collect();
    if !seen_keys.is_subset(&declared) {
        return Err("Bundle carries memories the skill's source_manifest does not cite".to_owned());
    }
    let mut warnings = Vec::new();
    let absent = declared.difference(&seen_keys).count();
    if absent > 0 {
        warnings.push(format!(
            "{absent} cited source {} not in the bundle (missing on the exporting instance); the \
             skill will cite keys that may not resolve here",
            if absent == 1 {
                "memory is"
            } else {
                "memories are"
            }
        ));
    }
    if let Some(Value::Array(missing)) = manifest.get("missing_sources")
        && !missing.is_empty()
    {
        warnings.push(format!(
            "The exporting instance already reported {} source memories missing at export time",
            missing.len()
        ));
    }

    let text = |name: &str| {
        manifest
            .get(name)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned()
    };
    Ok(ValidatedBundle {
        manifest: json!({
            "exported_at": text("exported_at"),
            "omnimem_version": text("omnimem_version"),
            "name": field("name"),
            "domain": domain,
            "user": user,
        }),
        skill_key,
        skill_fields,
        memories,
        feeds,
        warnings,
    })
}

/// (merged reading list, added, updated, skipped feed names)
pub type FeedMerge = (
    Vec<Map<String, Value>>,
    Vec<String>,
    Vec<String>,
    Vec<String>,
);

/// Fold bundled feeds into a reading list, additively, matching by URL.
/// Returns (merged, added, updated, skipped) with feed names.
pub fn merge_feed_influences(
    current: &[Map<String, Value>],
    bundle: &[Map<String, Value>],
) -> FeedMerge {
    let mut merged: Vec<Map<String, Value>> = current.to_vec();
    let mut by_url: HashMap<String, usize> = merged
        .iter()
        .enumerate()
        .filter_map(|(i, f)| {
            f.get("url")
                .and_then(Value::as_str)
                .filter(|u| !u.is_empty())
                .map(|u| (u.to_owned(), i))
        })
        .collect();
    let mut names: HashSet<String> = merged
        .iter()
        .map(|f| {
            f.get("name")
                .map(py_str)
                .unwrap_or_default()
                .trim()
                .to_owned()
        })
        .collect();
    let (mut added, mut updated, mut skipped) = (Vec::new(), Vec::new(), Vec::new());
    for feed in bundle {
        let url = feed
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let bundle_name = feed.get("name").map(py_str).unwrap_or_default();
        let Some(&index) = by_url.get(&url) else {
            let mut name = bundle_name.clone();
            if names.contains(&name) {
                name = format!("{name} (imported)");
            }
            if names.contains(&name) {
                skipped.push(bundle_name);
                continue;
            }
            let mut entry = feed.clone();
            entry.insert("name".into(), name.as_str().into());
            by_url.insert(url, merged.len());
            merged.push(entry);
            names.insert(name.clone());
            added.push(name);
            continue;
        };
        let existing = &mut merged[index];
        let existing_name = existing.get("name").map(py_str).unwrap_or(bundle_name);
        let mut skills = existing
            .get("skills")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let mut gained = false;
        if let Some(Value::Object(bundled)) = feed.get("skills") {
            for (domain, score) in bundled {
                if !skills.contains_key(domain) {
                    skills.insert(domain.clone(), score.clone());
                    gained = true;
                }
            }
        }
        if gained {
            existing.insert("skills".into(), Value::Object(skills));
            updated.push(existing_name);
        } else {
            skipped.push(existing_name);
        }
    }
    (merged, added, updated, skipped)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A zip of `files`, deflated, as the exporter writes one.
    fn zipped(files: &[(&str, &[u8])]) -> Vec<u8> {
        let owned: Vec<(String, Vec<u8>)> = files
            .iter()
            .map(|(n, d)| ((*n).to_owned(), d.to_vec()))
            .collect();
        zip_bytes(&owned).unwrap()
    }

    /// Rewrite the uncompressed size an entry declares, in both its local
    /// header and its central directory record, leaving the compressed
    /// stream and CRC as they are: a bundle whose header lies.
    fn declare_size(zip: &mut [u8], name: &str, size: u32) {
        let mut patched = 0;
        let mut i = 0;
        while i + 4 <= zip.len() {
            let (name_len_at, name_at, size_at) = match &zip[i..i + 4] {
                [0x50, 0x4b, 0x03, 0x04] => (26, 30, 22),
                [0x50, 0x4b, 0x01, 0x02] => (28, 46, 24),
                _ => {
                    i += 1;
                    continue;
                }
            };
            let name_len =
                u16::from_le_bytes([zip[i + name_len_at], zip[i + name_len_at + 1]]) as usize;
            if &zip[i + name_at..i + name_at + name_len] == name.as_bytes() {
                zip[i + size_at..i + size_at + 4].copy_from_slice(&size.to_le_bytes());
                patched += 1;
            }
            i += name_at;
        }
        assert_eq!(patched, 2, "local header and central directory record");
    }

    #[test]
    fn a_large_entry_is_refused_even_though_it_compresses_to_almost_nothing() {
        let big = vec![b' '; 3 * 1024 * 1024];
        let data = zipped(&[
            ("manifest.json", &big),
            ("skill.json", b"{}"),
            ("SKILL.md", b"x"),
        ]);
        assert!(data.len() < 64 * 1024, "deflate makes it tiny");
        assert_eq!(
            validate_skill_import(&data).unwrap_err(),
            "Bundle entry manifest.json is too large"
        );
    }

    #[test]
    fn an_entry_that_lies_about_its_size_is_refused() {
        // Declares a kilobyte, inflates to three megabytes.
        let big = vec![b' '; 3 * 1024 * 1024];
        let mut data = zipped(&[
            ("manifest.json", &big),
            ("skill.json", b"{}"),
            ("SKILL.md", b"x"),
        ]);
        declare_size(&mut data, "manifest.json", 1024);
        assert_eq!(
            validate_skill_import(&data).unwrap_err(),
            "Bundle entry manifest.json is too large"
        );

        // Declares a kilobyte, inflates to four: under the cap, still a lie.
        let small = vec![b' '; 4 * 1024];
        let mut data = zipped(&[
            ("manifest.json", &small),
            ("skill.json", b"{}"),
            ("SKILL.md", b"x"),
        ]);
        declare_size(&mut data, "manifest.json", 1024);
        assert_eq!(
            validate_skill_import(&data).unwrap_err(),
            "Bundle entry manifest.json does not match its declared size"
        );
    }

    /// A complete, checksummed bundle around one memory and one skill, with
    /// whatever extra fields the test wants smuggled in.
    fn bundle(memory_extra: &[(&str, &str)], skill_extra: &[(&str, &str)]) -> Vec<u8> {
        let key = "mem:episodic:01A";
        let body = "# python-local\n\nUse queues.\n";
        let mut skill = json!({
            "name": "python-local",
            "description": "Lessons from python work",
            "domain": "python",
            "user": "local",
            "body": body,
            "generated": "true",
            "contract_version": "1",
            "created_at": "1700000000.0",
            "updated_at": "1700000000.0",
            "tags": "[\"python\"]",
            "source_manifest": format!("[\"{key}\"]"),
            "rule_manifest": "[]",
        });
        for (name, value) in skill_extra {
            skill[*name] = json!(value);
        }
        let mut fields = json!({
            "content": "work on alpha",
            "tags": "[\"python\"]",
            "project": "alpha",
            "outcome": "succeeded",
            "lesson": "Use queues.",
            "created_at": "1700000000.0",
            "updated_at": "1700000000.0",
        });
        for (name, value) in memory_extra {
            fields[*name] = json!(value);
        }
        let memory = serde_json::to_vec(&json!({"key": key, "fields": fields})).unwrap();
        let skill_json = serde_json::to_vec(&skill).unwrap();
        let files: Vec<(&str, Vec<u8>)> = vec![
            ("skill.json", skill_json),
            ("SKILL.md", body.as_bytes().to_vec()),
            ("memories/0000.json", memory),
        ];
        let checksums: Map<String, Value> = files
            .iter()
            .map(|(n, d)| ((*n).to_owned(), sha256_hex(d).into()))
            .collect();
        let manifest = serde_json::to_vec(&json!({
            "format": EXPORT_FORMAT,
            "format_version": EXPORT_FORMAT_VERSION,
            "skill_key": "mem:skill:gen:python-local",
            "memory_count": 1,
            "checksums": checksums,
        }))
        .unwrap();
        let mut all: Vec<(&str, &[u8])> = vec![("manifest.json", &manifest)];
        all.extend(files.iter().map(|(n, d)| (*n, d.as_slice())));
        zipped(&all)
    }

    #[test]
    fn imports_keep_only_the_fields_a_stranger_may_set() {
        let smuggled = [
            ("blessed", "1"),
            ("blessed_at", "1700000001.0"),
            ("surface_score", "9.0"),
            ("effort_score", "10"),
            ("experience_weight", "5.0"),
            ("skill_domains", "[\"python\"]"),
            ("origin_id", "someone-else"),
            ("epoch", "42"),
            ("classification", "restricted"),
            ("content_hash", "deadbeef"),
            ("expires_at", "0"),
            ("enriched_from", "mem:knowledge:x"),
            ("source_doc_id", "doc"),
            ("feed_name", "news"),
            ("recall_count", "99"),
            ("last_recalled", "1700000002.0"),
            ("state", "deleted"),
            ("deprioritised_reason", "x"),
        ];
        let validated = validate_skill_import(&bundle(&smuggled, &smuggled)).unwrap();

        let (key, fields) = &validated.memories[0];
        assert_eq!(key, "mem:episodic:01A");
        for (name, _) in &smuggled {
            assert!(!fields.contains_key(*name), "memory field {name} travelled");
            assert!(
                !validated.skill_fields.contains_key(*name),
                "skill field {name} travelled"
            );
        }
        for name in [
            "content",
            "tags",
            "project",
            "outcome",
            "lesson",
            "created_at",
        ] {
            assert!(fields.contains_key(name), "memory field {name} was dropped");
        }
        // Every importable field the bundle carries survives (it has no
        // compiled_at, which is optional).
        for name in IMPORTABLE_SKILL_FIELDS
            .iter()
            .filter(|n| **n != "compiled_at")
        {
            assert!(
                validated.skill_fields.contains_key(*name),
                "skill field {name} was dropped"
            );
        }
    }
}
