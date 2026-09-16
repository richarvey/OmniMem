//! The Claude Haiku features: fact extraction (`memory/extraction.py`), the
//! enrichment queue (`memory/enrichment.py`), query expansion
//! (`memory/query_expansion.py`) and the second contradiction tier
//! (`memory/contradiction.py`).
//!
//! Every one fails open, as in 6.x: no model, an API error or an unreadable
//! reply means no facts, no variants or no confirmation, never a failed call.

use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, TimeZone};
use omnimem_core::{MemoryKey, Namespace};
use omnimem_store::Fields;
use regex::Regex;
use serde_json::{Map, Value, json};
use sha1::{Digest, Sha1};
use tracing::{debug, error, info, warn};

use crate::pyfmt::{now_str, py_float, py_json, take_chars};
use crate::skills::{py_str, py_truthy};
use crate::{Engine, Result};

const EXTRACTION_PROMPT: &str = "Extract discrete, atomic facts from the following text. Each fact should \
be a single declarative statement that stands on its own when read in \
isolation. Aim for facts that would survive being indexed and recalled \
later by a question-answering system.\n\n\
For each fact, identify:\n\
- text: the standalone declarative sentence\n\
- kind: one of 'fact' (general factual statement) or 'preference' \
(prescriptive rule about how someone wants to work, e.g. 'I prefer X', \
'always do Y', 'never do Z')\n\
- event_date: if the fact references a specific date or relative time \
('last March', '2026-03-15', 'yesterday'), provide it as ISO 8601 \
(YYYY-MM-DD). Omit this field if no date is mentioned.\n\n\
Return ONLY a JSON array of objects. No markdown fences, no explanation. \
If the text contains no extractable facts, return an empty array.\n\
Skip pleasantries, hedging, and meta-commentary about the conversation.\n\n\
Text:\n{content}\n";

/// 6.x hard-coded the model for the contradiction check.
const CONTRADICTION_MODEL: &str = "claude-haiku-4-5-20251001";

const EXPANSION_CACHE_TTL: Duration = Duration::from_secs(86_400);

/// How often the worker looks at an empty queue.
const WORKER_POLL: Duration = Duration::from_secs(1);

static JSON_OBJECT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\{[^}]+\}").expect("valid"));

#[derive(Debug, Clone, PartialEq)]
pub struct ExtractedFact {
    pub text: String,
    /// `fact` or `preference`.
    pub kind: &'static str,
    /// Unix seconds.
    pub event_date: Option<f64>,
}

/// Strip a markdown fence as 6.x did: drop the first line, then everything
/// from the last fence on. `None` when there is no first line to drop.
fn strip_fences(reply: &str) -> Option<String> {
    let text = reply.trim();
    if !text.starts_with("```") {
        return Some(text.to_owned());
    }
    let (_, rest) = text.split_once('\n')?;
    let body = rest.rfind("```").map_or(rest, |i| &rest[..i]);
    Some(body.trim().to_owned())
}

fn local_timestamp(naive: NaiveDateTime) -> Option<f64> {
    Local
        .from_local_datetime(&naive)
        .earliest()
        .map(|t| t.timestamp() as f64 + f64::from(t.timestamp_subsec_micros()) / 1e6)
}

/// `_parse_event_date`: numbers as given; ISO 8601 text, naive values read
/// in local time as Python's `datetime.timestamp()` does; else the leading
/// `YYYY-MM-DD`.
pub(crate) fn parse_event_date(raw: &Value) -> Option<f64> {
    if !py_truthy(raw) {
        return None;
    }
    match raw {
        Value::Number(n) => return n.as_f64(),
        Value::Bool(true) => return Some(1.0),
        _ => {}
    }
    let text = py_str(raw);
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let iso = text.replacen(' ', "T", 1);
    let aware = if iso.ends_with('Z') || iso.ends_with('z') {
        DateTime::parse_from_rfc3339(&format!("{}+00:00", &iso[..iso.len() - 1])).ok()
    } else {
        DateTime::parse_from_rfc3339(&iso)
            .ok()
            .or_else(|| DateTime::parse_from_str(&iso, "%Y-%m-%dT%H:%M%:z").ok())
    };
    if let Some(t) = aware {
        return Some(t.timestamp() as f64 + f64::from(t.timestamp_subsec_micros()) / 1e6);
    }
    for format in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%dT%H:%M"] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(&iso, format) {
            return local_timestamp(naive);
        }
    }
    let date: String = text.chars().take(10).collect();
    NaiveDate::parse_from_str(&date, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .and_then(local_timestamp)
}

fn expansion_cache_key(query: &str, n: i64) -> String {
    let digest = Sha1::digest(format!("{n}|{query}").as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("qexp:{hex}")
}

/// The classification a fact inherits: the source's own, the live record
/// winning over what the queued job declared. Nothing when neither exists.
fn inherited_classification(declared: Option<&Value>, source: Option<&Fields>) -> Fields {
    if declared.is_none_or(|d| !d.is_object()) && source.is_none() {
        return Fields::new();
    }
    let mut merged: Map<String, Value> = declared
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    for (name, value) in source.into_iter().flatten() {
        merged.insert(name.clone(), value.clone().into());
    }
    let pick = |name: &str, default: &str| {
        merged
            .get(name)
            .filter(|v| py_truthy(v))
            .map(py_str)
            .unwrap_or_else(|| default.to_owned())
    };
    let mut fields = Fields::from([
        ("licence".to_owned(), pick("licence", "own")),
        ("provenance".to_owned(), pick("provenance", "concluded")),
    ]);
    let note = pick("licence_note", "");
    if !note.is_empty() {
        fields.insert("licence_note".to_owned(), note);
    }
    fields
}

impl Engine {
    /// Discrete facts from `content`, or nothing on any failure.
    pub fn extract_facts(&self, content: &str) -> Vec<ExtractedFact> {
        if content.trim().is_empty() {
            return Vec::new();
        }
        let Some(llm) = &self.llm else {
            debug!("fact extraction disabled: no ANTHROPIC_API_KEY");
            return Vec::new();
        };
        let prompt = EXTRACTION_PROMPT.replace("{content}", &take_chars(content, 12_000));
        let parsed = llm
            .complete(&self.config.fact_extraction_model, &prompt, 2048)
            .map_err(|e| e.to_string())
            .and_then(|reply| {
                strip_fences(&reply).ok_or_else(|| "unterminated code fence".to_owned())
            })
            .and_then(|text| serde_json::from_str::<Value>(&text).map_err(|e| e.to_string()));
        let items = match parsed {
            Ok(Value::Array(items)) => items,
            Ok(_) => return Vec::new(),
            Err(e) => {
                warn!(error = %e, "fact extraction failed");
                return Vec::new();
            }
        };
        items
            .iter()
            .filter_map(Value::as_object)
            .filter_map(|item| {
                let text = item
                    .get("text")
                    .map(py_str)
                    .unwrap_or_default()
                    .trim()
                    .to_owned();
                if text.is_empty() {
                    return None;
                }
                let kind = item.get("kind").map_or_else(|| "fact".to_owned(), py_str);
                let kind = if kind.trim().to_lowercase() == "preference" {
                    "preference"
                } else {
                    "fact"
                };
                let event_date = item.get("event_date").and_then(parse_event_date);
                Some(ExtractedFact {
                    text,
                    kind,
                    event_date,
                })
            })
            .collect()
    }

    /// Alternative phrasings of `query` (not including it), cached for a
    /// day. Empty on any failure, which leaves recall on the original query.
    pub fn expand_query(&self, query: &str) -> Vec<String> {
        if query.trim().is_empty() {
            return Vec::new();
        }
        let n = self.config.recall_expand_count.clamp(1, 10);
        let cache_key = expansion_cache_key(query, n);
        let cached = self
            .store
            .hash_get_all(&cache_key)
            .ok()
            .flatten()
            .and_then(|f| f.get("variants").cloned())
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            .and_then(|v| v.as_array().cloned());
        if let Some(cached) = cached {
            debug!(variants = cached.len(), "query expansion cache hit");
            return cached.iter().filter(|v| py_truthy(v)).map(py_str).collect();
        }
        let Some(llm) = &self.llm else {
            return Vec::new();
        };
        let prompt = format!(
            "Generate {n} alternative phrasings of the following search query to improve semantic \
             search recall. Each variant should preserve the original meaning but use different \
             vocabulary, synonyms, or related concepts that might appear in stored content.\n\n\
             Return ONLY a JSON array of {n} strings. No markdown fences, no explanation.\n\n\
             Original query: {query}\n"
        );
        let parsed = llm
            .complete(&self.config.query_expansion_model, &prompt, 512)
            .map_err(|e| e.to_string())
            .and_then(|reply| {
                strip_fences(&reply).ok_or_else(|| "unterminated code fence".to_owned())
            })
            .and_then(|text| serde_json::from_str::<Value>(&text).map_err(|e| e.to_string()));
        let variants = match parsed {
            Ok(Value::Array(items)) => items,
            Ok(_) => return Vec::new(),
            Err(e) => {
                warn!(query = %take_chars(query, 80), error = %e, "query expansion failed");
                return Vec::new();
            }
        };
        let cleaned: Vec<String> = variants
            .iter()
            .filter(|v| matches!(v, Value::String(_) | Value::Number(_) | Value::Bool(_)))
            .map(|v| py_str(v).trim().to_owned())
            .filter(|v| !v.is_empty())
            .take(n as usize)
            .collect();
        if !cleaned.is_empty() {
            let fields = Fields::from([
                ("variants".to_owned(), py_json(&json!(cleaned))),
                ("ts".to_owned(), now_str()),
            ]);
            let cached = self
                .store
                .hash_set(&cache_key, &fields)
                .and_then(|()| self.store.expire(&cache_key, EXPANSION_CACHE_TTL));
            if let Err(e) = cached {
                debug!(error = %e, "failed to cache query expansion");
            }
        }
        cleaned
    }

    /// Tier 2: ask Claude whether two memories contradict each other.
    pub(crate) fn check_contradiction_api(&self, content_a: &str, content_b: &str) -> Value {
        let unconfirmed = |explanation: String| json!({"is_contradiction": false, "confidence": 0.0, "explanation": explanation});
        let Some(llm) = &self.llm else {
            return unconfirmed(
                "ANTHROPIC_API_KEY not configured for API-based contradiction check.".to_owned(),
            );
        };
        let prompt = format!(
            "You are analysing two memories from a knowledge base for contradictions.\n\n\
             Memory A:\n{}\n\n\
             Memory B:\n{}\n\n\
             Do these two memories contradict each other? Consider:\n\
             - Direct factual contradictions\n\
             - Opposing recommendations or advice\n\
             - Conflicting decisions or approaches\n\
             - One says to use something the other says to avoid\n\n\
             Respond in JSON format:\n\
             {{\"is_contradiction\": true/false, \"confidence\": 0.0-1.0, \"explanation\": \"brief explanation\"}}",
            take_chars(content_a, 1000),
            take_chars(content_b, 1000),
        );
        match llm.complete(CONTRADICTION_MODEL, &prompt, 256) {
            Err(e) => {
                error!(error = %e, "contradiction API check failed");
                unconfirmed(format!("API check failed: {e}"))
            }
            Ok(reply) => {
                let text = reply.trim();
                match JSON_OBJECT.find(text) {
                    None => unconfirmed(take_chars(text, 200)),
                    Some(found) => match serde_json::from_str::<Value>(found.as_str()) {
                        // The model's answer is untrusted: only a JSON true
                        // confirms, the confidence is clamped, and nothing
                        // else in its object reaches the caller.
                        Ok(verdict) => json!({
                            "is_contradiction": verdict["is_contradiction"].as_bool() == Some(true),
                            "confidence": verdict["confidence"]
                                .as_f64()
                                .filter(|c| c.is_finite())
                                .map_or(0.0, |c| c.clamp(0.0, 1.0)),
                            "explanation": take_chars(
                                verdict["explanation"].as_str().unwrap_or(""),
                                1000
                            ),
                        }),
                        Err(e) => {
                            error!(error = %e, "contradiction API check failed");
                            unconfirmed(format!("API check failed: {e}"))
                        }
                    },
                }
            }
        }
    }

    /// Process the oldest enrichment job. `false` when the queue is empty. A
    /// job is removed once handled, whether or not it produced facts, as
    /// 6.x consumed it; only a crash mid-job leaves it for the next start.
    pub fn process_next_enrichment(&self) -> Result<bool> {
        let Some((id, payload)) = self.store.next_enrichment()? else {
            return Ok(false);
        };
        if !payload.is_object() {
            warn!(id, "enrichment queue: invalid payload");
        } else if let Err(e) = self.enrich(&payload) {
            error!(
                key = payload["key"].as_str().unwrap_or("?"),
                error = %e,
                "enrichment failed"
            );
        }
        self.store.complete_enrichment(id)?;
        Ok(true)
    }

    /// Drain the queue until `stop` is set, looking again every second when
    /// it is empty.
    pub fn run_enrichment_worker(&self, stop: &AtomicBool) {
        info!("enrichment worker started");
        while !stop.load(Ordering::Relaxed) {
            match self.process_next_enrichment() {
                Ok(true) => continue,
                Ok(false) => {}
                Err(e) => warn!(error = %e, "enrichment queue read failed"),
            }
            let mut waited = Duration::ZERO;
            while waited < WORKER_POLL && !stop.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(100));
                waited += Duration::from_millis(100);
            }
        }
        info!("enrichment worker stopped");
    }

    /// Extract facts from a stored memory (or a batch of chunks) and write
    /// them as linked memories in `knowledge` or `preference`.
    fn enrich(&self, payload: &Value) -> Result<()> {
        let truthy_text = |name: &str| payload.get(name).filter(|v| py_truthy(v)).map(py_str);
        let key = payload
            .get("key")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let project = truthy_text("project");
        let tags = payload
            .get("tags")
            .filter(|t| py_truthy(t))
            .cloned()
            .unwrap_or(json!([]));
        let declared = payload.get("classification").filter(|c| !c.is_null());
        let mut source_event_date = truthy_text("event_date");
        let mut source_created_at = truthy_text("created_at");

        let batch_content = payload
            .get("batch_mode")
            .is_some_and(py_truthy)
            .then(|| truthy_text("batch_content"))
            .flatten();
        let (facts, classification) = if let Some(content) = batch_content {
            let mut classification = Fields::new();
            if !key.is_empty() {
                let source = self
                    .store
                    .get_fields_multi(
                        std::slice::from_ref(&key),
                        &[
                            "event_date",
                            "created_at",
                            "licence",
                            "licence_note",
                            "provenance",
                        ],
                    )?
                    .pop()
                    .flatten()
                    .unwrap_or_default();
                classification = inherited_classification(declared, Some(&source));
                if source_created_at.is_none() {
                    let own = |name: &str| source.get(name).filter(|v| !v.is_empty()).cloned();
                    source_event_date = source_event_date.or_else(|| own("event_date"));
                    source_created_at = own("created_at");
                }
            }
            (self.extract_facts(&content), classification)
        } else {
            let Some(data) = self.store.get(&key).ok().flatten() else {
                debug!(key, "enrichment: key no longer exists, skipping");
                return Ok(());
            };
            let Some(content) = data.get("content").filter(|c| !c.is_empty()) else {
                return Ok(());
            };
            let own = |name: &str| data.get(name).filter(|v| !v.is_empty()).cloned();
            source_event_date = own("event_date").or(source_event_date);
            source_created_at = own("created_at").or(source_created_at);
            let classification = inherited_classification(declared, Some(&data));
            (self.extract_facts(content), classification)
        };
        if facts.is_empty() {
            return Ok(());
        }

        let now = now_str();
        let source_doc_id = truthy_text("doc_id").unwrap_or_else(|| key.clone());
        let (mut stored, mut preferences, mut duplicates) = (0, 0, 0);
        for fact in &facts {
            // Facts supplement the verbatim memory and live outside its
            // namespace, so they can't crowd it out of recall (#20).
            let namespace = if fact.kind == "preference" {
                Namespace::Preference
            } else {
                Namespace::Knowledge
            };
            let vector = self.embed(&fact.text)?;
            if self
                .check_duplicate(namespace, &vector, project.as_deref())?
                .is_some()
            {
                duplicates += 1;
                continue;
            }
            let mut fields = Fields::from([
                ("content".to_owned(), fact.text.clone()),
                ("state".to_owned(), "active".to_owned()),
                ("surface_score".to_owned(), "0.5".to_owned()),
                ("experience_weight".to_owned(), "1.0".to_owned()),
                ("created_at".to_owned(), now.clone()),
                ("updated_at".to_owned(), now.clone()),
                ("tags".to_owned(), py_json(&tags)),
                ("source_doc_id".to_owned(), source_doc_id.clone()),
                ("enriched_from".to_owned(), key.clone()),
            ]);
            fields.extend(classification.clone());
            if let Some(project) = &project {
                fields.insert("project".to_owned(), project.clone());
            }
            // The fact's own date, else the source's, else when the source
            // was stored, so date-shaped queries still find it (#20).
            let event_date = fact
                .event_date
                .map(py_float)
                .or_else(|| source_event_date.clone())
                .or_else(|| source_created_at.clone());
            if let Some(event_date) = event_date {
                fields.insert("event_date".to_owned(), event_date);
            }
            if namespace == Namespace::Preference {
                let scope = if project.is_some() {
                    "project"
                } else {
                    "global"
                };
                fields.insert("scope".to_owned(), scope.to_owned());
                preferences += 1;
            }
            let fact_key = MemoryKey::generate(namespace).to_string();
            self.store.upsert(&fact_key, &fields, Some(&vector))?;
            stored += 1;
        }
        info!(
            key,
            stored,
            preferences,
            duplicates,
            extracted = facts.len(),
            "enriched memory"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fences_strip_like_python() {
        assert_eq!(strip_fences("```json\n[1]\n```").as_deref(), Some("[1]"));
        assert_eq!(strip_fences("  [2]  ").as_deref(), Some("[2]"));
        assert_eq!(strip_fences("```[3]"), None);
    }

    #[test]
    fn event_dates_parse_like_python() {
        assert_eq!(
            parse_event_date(&json!("2026-03-15T10:00:00Z")),
            Some(1_773_568_800.0)
        );
        assert_eq!(
            parse_event_date(&json!("2026-03-15T10:00:00+01:00")),
            Some(1_773_565_200.0)
        );
        let midnight = Local
            .from_local_datetime(
                &NaiveDate::from_ymd_opt(2026, 3, 15)
                    .unwrap()
                    .and_hms_opt(0, 0, 0)
                    .unwrap(),
            )
            .earliest()
            .unwrap()
            .timestamp() as f64;
        assert_eq!(parse_event_date(&json!("2026-03-15")), Some(midnight));
        assert_eq!(
            parse_event_date(&json!("2026-03-15 and later")),
            Some(midnight)
        );
        assert_eq!(parse_event_date(&json!(1234.5)), Some(1234.5));
        assert_eq!(parse_event_date(&json!("last March")), None);
        assert_eq!(parse_event_date(&json!("")), None);
    }

    #[test]
    fn cache_keys_match_6x() {
        // hashlib.sha1(b"3|what degree did I graduate with").hexdigest()
        assert_eq!(
            expansion_cache_key("what degree did I graduate with", 3),
            format!("qexp:{}", {
                let d = Sha1::digest(b"3|what degree did I graduate with");
                d.iter().map(|b| format!("{b:02x}")).collect::<String>()
            })
        );
    }

    #[test]
    fn facts_inherit_the_live_record_over_the_declared_class() {
        let declared = json!({"licence": "restricted", "licence_note": "Paywalled", "provenance": "retrieved"});
        let live = Fields::from([("licence".to_owned(), "open".to_owned())]);
        let class = inherited_classification(Some(&declared), Some(&live));
        assert_eq!(class["licence"], "open");
        assert_eq!(class["licence_note"], "Paywalled");
        assert_eq!(class["provenance"], "retrieved");
        assert!(inherited_classification(None, None).is_empty());
        assert_eq!(
            inherited_classification(None, Some(&Fields::new()))["licence"],
            "own"
        );
    }
}
