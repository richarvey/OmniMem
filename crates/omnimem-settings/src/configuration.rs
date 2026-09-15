//! `/configuration`: the settings a headless install sets as environment
//! variables, for the desktop app, which has no shell to set them in.
//!
//! Ordinary settings are written to `omnimem.env` in the data folder;
//! secrets (the Anthropic key, the MCP token, a Hugging Face token) go to the
//! OS keychain through a [`SecretStore`] the app provides, and are never
//! shown again once saved. The app reads both at start into the overlay
//! `omnimem_core::env` consults, so a change applies at the next start, and a
//! real environment variable of the same name still wins.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;

use axum::Form;
use axum::extract::{Query, State};
use axum::response::Response;
use minijinja::context;
use omnimem_core::env::{parse_settings, render_settings};
use serde_json::{Value, json};
use tracing::{info, warn};

use crate::PanelState;
use crate::pages::{quote, see_other};
use crate::render::page;

/// Where secrets are kept. The desktop app backs this with the OS keychain.
pub trait SecretStore: Send + Sync {
    /// The stored value, or `None` when there isn't one.
    fn get(&self, name: &str) -> Result<Option<String>, String>;
    fn set(&self, name: &str, value: &str) -> Result<(), String>;
    /// Remove the value. Removing one that isn't there is not an error.
    fn delete(&self, name: &str) -> Result<(), String>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Text,
    Integer,
    Decimal,
    Flag,
    Choice(&'static [&'static str]),
    Secret,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::Text => "text",
            Kind::Integer => "integer",
            Kind::Decimal => "decimal",
            Kind::Flag => "flag",
            Kind::Choice(_) => "choice",
            Kind::Secret => "secret",
        }
    }
}

struct Setting {
    name: &'static str,
    label: &'static str,
    kind: Kind,
    /// What applies when nothing is set, as the page shows it.
    default: &'static str,
    help: &'static str,
}

const fn setting(
    name: &'static str,
    label: &'static str,
    kind: Kind,
    default: &'static str,
    help: &'static str,
) -> Setting {
    Setting {
        name,
        label,
        kind,
        default,
        help,
    }
}

const HAIKU: &str = "claude-haiku-4-5-20251001";

const GROUPS: &[(&str, &[Setting])] = &[
    (
        "MCP server",
        &[
            setting(
                "MCP_HOST",
                "Listen address",
                Kind::Text,
                "127.0.0.1",
                "Anything other than this machine needs an access token.",
            ),
            setting(
                "MCP_PORT",
                "Port",
                Kind::Integer,
                "8765",
                "Clients connect to http://address:port/mcp.",
            ),
            setting(
                "MCP_AUTH_TOKEN",
                "Access token",
                Kind::Secret,
                "",
                "A shared bearer token clients send. Required when listening beyond this machine.",
            ),
            setting(
                "MCP_PUBLIC_URL",
                "Public URL",
                Kind::Text,
                "",
                "Where clients reach the server through a proxy or tunnel. Its host and origin are trusted.",
            ),
            setting(
                "OAUTH_BASE_URL",
                "OAuth base URL",
                Kind::Text,
                "",
                "The address the OAuth flow uses. Its host and origin are trusted too.",
            ),
            setting(
                "MCP_ALLOWED_HOSTS",
                "Extra allowed hosts",
                Kind::Text,
                "",
                "More Host headers to accept, comma separated.",
            ),
            setting(
                "MCP_ALLOWED_ORIGINS",
                "Extra allowed origins",
                Kind::Text,
                "",
                "More browser origins to accept, comma separated.",
            ),
        ],
    ),
    (
        "Claude",
        &[
            setting(
                "ANTHROPIC_API_KEY",
                "Anthropic API key",
                Kind::Secret,
                "",
                "Turns on fact extraction, query expansion, contradiction checks and RSS summaries.",
            ),
            setting(
                "ANTHROPIC_BASE_URL",
                "API endpoint",
                Kind::Text,
                "https://api.anthropic.com",
                "A different Messages API endpoint, such as a proxy.",
            ),
            setting(
                "FACT_EXTRACTION_MODEL",
                "Fact extraction model",
                Kind::Text,
                HAIKU,
                "",
            ),
            setting(
                "QUERY_EXPANSION_MODEL",
                "Query expansion model",
                Kind::Text,
                HAIKU,
                "",
            ),
            setting(
                "INGEST_MODE",
                "Ingest mode",
                Kind::Choice(&["full", "raw"]),
                "full",
                "Full extracts facts from new memories in the background; raw stores them as written.",
            ),
            setting(
                "ENRICHMENT_BATCH_MODE",
                "Batch enrichment",
                Kind::Flag,
                "off",
                "Extract facts from queued memories in batches.",
            ),
            setting(
                "RECALL_EXPAND_QUERIES",
                "Expand recall queries",
                Kind::Flag,
                "off",
                "Rephrase every recall query with Claude unless the call says otherwise.",
            ),
            setting(
                "RECALL_EXPAND_COUNT",
                "Variants per expanded query",
                Kind::Integer,
                "3",
                "1 to 10.",
            ),
        ],
    ),
    (
        "Recall",
        &[
            setting(
                "MEMORY_RECALL_TOP_K",
                "Results per recall",
                Kind::Integer,
                "5",
                "",
            ),
            setting(
                "RECALL_MIN_SCORE",
                "Relevance floor",
                Kind::Decimal,
                "0.15",
                "Similarity below this is left out of recall. 0 keeps everything.",
            ),
            setting(
                "RECALL_WEAK_SCORE",
                "Weak match below",
                Kind::Decimal,
                "0.35",
                "Results under this are marked as weak matches. 0 turns the marker off.",
            ),
            setting(
                "RECENCY_DECAY_DAYS",
                "Recency decay after (days)",
                Kind::Decimal,
                "90",
                "",
            ),
            setting(
                "DEPRIORITISED_WEIGHT",
                "Deprioritised weight",
                Kind::Decimal,
                "0.2",
                "How much a deprioritised memory still counts in recall.",
            ),
            setting(
                "DEDUP_SIMILARITY_THRESHOLD",
                "Duplicate similarity",
                Kind::Decimal,
                "0.92",
                "",
            ),
            setting(
                "CONTRADICTION_SIMILARITY_THRESHOLD",
                "Contradiction similarity",
                Kind::Decimal,
                "0.7",
                "",
            ),
            setting(
                "STALE_MEMORY_DAYS",
                "Stale after (days)",
                Kind::Integer,
                "30",
                "The briefing lists active memories untouched this long.",
            ),
            setting(
                "AUTO_MAINTENANCE_INTERVAL",
                "Maintenance every n briefings",
                Kind::Integer,
                "10",
                "0 turns auto-maintenance off.",
            ),
            setting(
                "BACKUP_DIR",
                "Backup folder",
                Kind::Text,
                "",
                "Defaults to a backups folder beside the database.",
            ),
        ],
    ),
    (
        "RSS",
        &[
            setting(
                "RSS_SCHEDULE_HOURS",
                "Check feeds every (hours)",
                Kind::Integer,
                "6",
                "0 checks only at start and when feeds.yml changes.",
            ),
            setting(
                "RSS_REQUIRE_LICENCE",
                "Require a declared licence",
                Kind::Flag,
                "off",
                "Skip feeds that don't say what their articles may be used for.",
            ),
            setting(
                "MAX_KNOWLEDGE_AGE_DAYS",
                "Articles expire after (days)",
                Kind::Integer,
                "30",
                "",
            ),
            setting(
                "RSS_MAX_ARTICLES_PER_FEED",
                "Articles per feed per check",
                Kind::Integer,
                "20",
                "",
            ),
            setting(
                "RSS_MAX_DIGEST_ENTRIES",
                "Digest entries per check",
                Kind::Integer,
                "2",
                "",
            ),
            setting(
                "FEEDS_CONFIG_PATH",
                "Reading list file",
                Kind::Text,
                "",
                "Defaults to feeds.yml beside the database.",
            ),
        ],
    ),
    (
        "Skills",
        &[
            setting(
                "OMNIMEM_USER",
                "Skill owner name",
                Kind::Text,
                "local",
                "The user part of generated skill names.",
            ),
            setting(
                "SKILL_SCAN_INTERVAL_HOURS",
                "Scan for skills every (hours)",
                Kind::Decimal,
                "24",
                "0 turns the automatic scan off.",
            ),
            setting(
                "SKILL_SCAN_CROSS_PROJECT",
                "Scan across projects",
                Kind::Flag,
                "on",
                "",
            ),
            setting(
                "SKILL_FEED_MAX_ARTICLES",
                "Feed watch articles",
                Kind::Integer,
                "25",
                "0 leaves the Feed watch section out.",
            ),
            setting(
                "SKILL_KNOWLEDGE_WATCH_DAYS",
                "Knowledge watch window (days)",
                Kind::Integer,
                "14",
                "0 turns the watch off.",
            ),
            setting(
                "SKILL_PROPOSAL_TTL_SECONDS",
                "Proposals expire after (seconds)",
                Kind::Integer,
                "86400",
                "",
            ),
        ],
    ),
    (
        "Embeddings",
        &[
            setting(
                "EMBEDDING_THREADS",
                "Embedding threads",
                Kind::Integer,
                "",
                "Defaults to ONNX Runtime's own choice.",
            ),
            setting(
                "HF_HUB_OFFLINE",
                "Stay offline",
                Kind::Flag,
                "off",
                "Never contact Hugging Face. The model must already be downloaded.",
            ),
            setting(
                "HF_TOKEN",
                "Hugging Face token",
                Kind::Secret,
                "",
                "Only needed for a private or gated model.",
            ),
        ],
    ),
    (
        "Settings panel",
        &[
            setting(
                "DASHBOARD_STATS_TTL",
                "Dashboard counts cached for (seconds)",
                Kind::Integer,
                "60",
                "0 recounts on every visit.",
            ),
            setting(
                "TELEMETRY_COLD_DAYS",
                "Gone cold after (days)",
                Kind::Integer,
                "60",
                "",
            ),
        ],
    ),
];

fn settings() -> impl Iterator<Item = &'static Setting> {
    GROUPS.iter().flat_map(|(_, group)| group.iter())
}

/// The settings kept in the keychain rather than the settings file.
pub fn secret_settings() -> Vec<&'static str> {
    settings()
        .filter(|s| s.kind == Kind::Secret)
        .map(|s| s.name)
        .collect()
}

fn read_file(path: &Path) -> Result<BTreeMap<String, String>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(parse_settings(&text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(e) => Err(format!("Could not read {}: {e}", path.display())),
    }
}

fn write_file(path: &Path, values: &BTreeMap<String, String>) -> Result<(), String> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Could not create {}: {e}", parent.display()))?;
    }
    let partial = path.with_extension("env.partial");
    std::fs::write(&partial, render_settings(values))
        .and_then(|()| std::fs::rename(&partial, path))
        .map_err(|e| format!("Could not write {}: {e}", path.display()))
}

/// The check a value must pass before it's written. Empty means "use the
/// default" and is always allowed.
fn validate(setting: &Setting, value: &str) -> Result<(), String> {
    let ok = match setting.kind {
        Kind::Text | Kind::Secret => !value.contains(['\n', '\r']),
        Kind::Integer => value.parse::<i64>().is_ok(),
        Kind::Decimal => value.parse::<f64>().is_ok_and(f64::is_finite),
        Kind::Flag => matches!(value, "true" | "false"),
        Kind::Choice(choices) => choices.contains(&value),
    };
    if ok {
        Ok(())
    } else {
        let expected = match setting.kind {
            Kind::Integer => "a whole number",
            Kind::Decimal => "a number",
            Kind::Choice(choices) => {
                return Err(format!(
                    "{} must be one of {}",
                    setting.label,
                    choices.join(", ")
                ));
            }
            _ => "a single line",
        };
        Err(format!(
            "{} ({}) must be {expected}",
            setting.label, setting.name
        ))
    }
}

fn unavailable(state: &PanelState) -> Response {
    page(
        state.templates(),
        "configuration.html",
        context! { current_page => "configuration", available => false },
    )
}

pub(crate) async fn form(
    State(state): State<PanelState>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let Some(path) = state.settings_path() else {
        return unavailable(&state);
    };
    let store = state.secret_store();
    let loaded = {
        let path = path.clone();
        let store = store.clone();
        tokio::task::spawn_blocking(move || {
            let file = read_file(&path);
            let secrets: HashMap<&str, Result<bool, String>> = secret_settings()
                .into_iter()
                .filter_map(|name| {
                    let store = store.as_ref()?;
                    Some((name, store.get(name).map(|v| v.is_some())))
                })
                .collect();
            (file, secrets)
        })
        .await
    };
    let (file, secrets) = match loaded {
        Ok(loaded) => loaded,
        Err(e) => return see_other(&format!("/?error={}", quote(&e.to_string()))),
    };
    let (values, file_error) = match file {
        Ok(values) => (values, None),
        Err(problem) => (BTreeMap::new(), Some(problem)),
    };
    let mut keychain_error = None;
    let groups: Vec<Value> = GROUPS
        .iter()
        .map(|(title, group)| {
            let settings: Vec<Value> = group
                .iter()
                .map(|s| {
                    let stored = match secrets.get(s.name) {
                        Some(Ok(stored)) => *stored,
                        Some(Err(problem)) => {
                            keychain_error.get_or_insert_with(|| problem.clone());
                            false
                        }
                        None => false,
                    };
                    json!({
                        "name": s.name,
                        "label": s.label,
                        "kind": s.kind.as_str(),
                        "choices": match s.kind { Kind::Choice(choices) => choices.to_vec(), _ => Vec::new() },
                        "default": s.default,
                        "help": s.help,
                        "value": if s.kind == Kind::Secret { "" } else { values.get(s.name).map_or("", String::as_str) },
                        "stored": stored,
                        "overridden": std::env::var_os(s.name).is_some(),
                    })
                })
                .collect();
            json!({"title": title, "settings": settings})
        })
        .collect();
    page(
        state.templates(),
        "configuration.html",
        context! {
            current_page => "configuration",
            available => true,
            groups,
            settings_path => path.display().to_string(),
            secrets_available => store.is_some() && keychain_error.is_none(),
            keychain_error,
            message => query.get("message"),
            error => file_error.or_else(|| query.get("error").cloned()),
        },
    )
}

/// POST `/configuration`: validate everything, then write the file and the
/// keychain. Nothing is written if any value is refused.
pub(crate) async fn save(
    State(state): State<PanelState>,
    Form(form): Form<Vec<(String, String)>>,
) -> Response {
    let Some(path) = state.settings_path() else {
        return unavailable(&state);
    };
    let store = state.secret_store();
    let outcome = tokio::task::spawn_blocking(move || save_blocking(&path, store, &form)).await;
    match outcome {
        Ok(Ok(())) => see_other(&format!(
            "/configuration?message={}",
            quote("Saved. Restart OmniMem for the changes to take effect.")
        )),
        Ok(Err(problem)) => see_other(&format!("/configuration?error={}", quote(&problem))),
        Err(e) => see_other(&format!("/configuration?error={}", quote(&e.to_string()))),
    }
}

fn save_blocking(
    path: &Path,
    store: Option<Arc<dyn SecretStore>>,
    form: &[(String, String)],
) -> Result<(), String> {
    let field = |name: &str| {
        form.iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.trim().to_owned())
    };
    // Hand-written lines the page doesn't know about are kept.
    let mut values = read_file(path)?;
    let mut refused = Vec::new();
    for s in settings().filter(|s| s.kind != Kind::Secret) {
        let value = field(s.name).unwrap_or_default();
        if value.is_empty() {
            values.remove(s.name);
            continue;
        }
        match validate(s, &value) {
            Ok(()) => {
                values.insert(s.name.to_owned(), value);
            }
            Err(problem) => refused.push(problem),
        }
    }
    for s in settings().filter(|s| s.kind == Kind::Secret) {
        if let Some(value) = field(s.name).filter(|v| !v.is_empty())
            && let Err(problem) = validate(s, &value)
        {
            refused.push(problem);
        }
    }
    if !refused.is_empty() {
        return Err(format!("Nothing was saved. {}.", refused.join("; ")));
    }
    write_file(path, &values)?;
    info!(path = %path.display(), "saved settings from the settings panel");

    let mut problems = Vec::new();
    for name in secret_settings() {
        let clear = field(&format!("clear:{name}")).is_some_and(|v| v == "on");
        let value = field(name).filter(|v| !v.is_empty());
        if !clear && value.is_none() {
            continue;
        }
        let Some(store) = &store else {
            problems.push(format!("{name} wasn't saved: the keychain isn't available"));
            continue;
        };
        let result = if clear {
            store.delete(name)
        } else {
            store.set(name, value.as_deref().unwrap_or(""))
        };
        if let Err(problem) = result {
            warn!(setting = name, problem, "could not update the keychain");
            problems.push(format!("{name}: {problem}"));
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "The settings file was saved, but the keychain refused: {}.",
            problems.join("; ")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_catalogue_is_consistent() {
        let names: Vec<&str> = settings().map(|s| s.name).collect();
        let mut unique = names.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(names.len(), unique.len(), "no setting is listed twice");
        assert!(names.iter().all(|n| omnimem_core::env::is_setting_name(n)));
        assert_eq!(
            secret_settings(),
            ["MCP_AUTH_TOKEN", "ANTHROPIC_API_KEY", "HF_TOKEN"]
        );
        for s in settings().filter(|s| !s.default.is_empty()) {
            if matches!(s.kind, Kind::Integer | Kind::Decimal | Kind::Choice(_)) {
                assert!(validate(s, s.default).is_ok(), "{} default", s.name);
            }
        }
    }

    #[test]
    fn values_are_checked_by_kind() {
        let find = |name: &str| settings().find(|s| s.name == name).unwrap();
        assert!(validate(find("MCP_PORT"), "9000").is_ok());
        assert!(
            validate(find("MCP_PORT"), "lots")
                .unwrap_err()
                .contains("whole number")
        );
        assert!(validate(find("RECALL_MIN_SCORE"), "0.2").is_ok());
        assert!(validate(find("RECALL_MIN_SCORE"), "NaN").is_err());
        assert!(
            validate(find("INGEST_MODE"), "sometimes")
                .unwrap_err()
                .contains("full, raw")
        );
        assert!(validate(find("RSS_REQUIRE_LICENCE"), "true").is_ok());
        assert!(validate(find("RSS_REQUIRE_LICENCE"), "yes").is_err());
    }
}
