//! `/configuration`: the settings file, edited in the browser.
//!
//! Ordinary settings are written to the settings file (`omnimem.env` in the
//! desktop app's data folder, or the server's own); secrets (the Anthropic
//! key, the MCP token, the admin password, a Hugging Face token) go through a
//! [`SecretStore`] the app provides: the OS keychain on a desktop, the
//! settings file itself on a server ([`SettingsFileSecrets`]). Either way a
//! secret is never shown again once saved. OmniMem reads them at start, so a
//! change applies at the next start, and a real environment variable of the
//! same name still wins.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use axum::Form;
use axum::extract::{Query, State};
use axum::response::Response;
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
    /// Where the secrets go, as the page says it: "the system keychain".
    fn place(&self) -> &'static str {
        "the system keychain"
    }
}

/// Secrets kept in the settings file beside everything else, for a server,
/// which has no keychain. The file is rewritten with the permissions it had,
/// or readable by its owner alone when this creates it.
pub struct SettingsFileSecrets {
    path: PathBuf,
}

impl SettingsFileSecrets {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl SecretStore for SettingsFileSecrets {
    fn get(&self, name: &str) -> Result<Option<String>, String> {
        Ok(read_file(&self.path)?
            .remove(name)
            .filter(|value| !value.is_empty()))
    }

    fn set(&self, name: &str, value: &str) -> Result<(), String> {
        let mut values = read_file(&self.path)?;
        values.insert(name.to_owned(), value.to_owned());
        write_file(&self.path, &values)
    }

    fn delete(&self, name: &str) -> Result<(), String> {
        let mut values = read_file(&self.path)?;
        if values.remove(name).is_some() {
            write_file(&self.path, &values)?;
        }
        Ok(())
    }

    fn place(&self) -> &'static str {
        "the settings file"
    }
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
            Self::Text => "text",
            Self::Integer => "integer",
            Self::Decimal => "decimal",
            Self::Flag => "flag",
            Self::Choice(_) => "choice",
            Self::Secret => "secret",
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

const HAIKU: &str = "claude-haiku-5-5";

const GROUPS: &[(&str, &[Setting])] = &[
    (
        "MCP server",
        &[
            setting(
                "MCP_HOST",
                "Listen address",
                Kind::Text,
                "127.0.0.1",
                "Anything other than this machine needs an access token or OAuth.",
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
                "A shared bearer token clients send. Required when listening beyond this machine, unless OAuth is on.",
            ),
            setting(
                "MCP_PUBLIC_URL",
                "Public URL",
                Kind::Text,
                "",
                "Where clients reach the server through a proxy or tunnel, comma separated for more than one. Their hosts and origins are trusted.",
            ),
            setting(
                "MCP_ALLOWED_HOSTS",
                "Extra allowed hosts",
                Kind::Text,
                "",
                "More Host headers to accept, comma separated, such as mcp.example.com,10.0.0.5. *.example.com accepts any name under it; * accepts any host, so only use it when something in front of OmniMem checks the host.",
            ),
            setting(
                "MCP_ALLOWED_ORIGINS",
                "Extra allowed origins",
                Kind::Text,
                "",
                "More browser origins to accept, comma separated, such as https://chat.mistral.ai for Le Chat. * accepts any.",
            ),
        ],
    ),
    (
        "OAuth",
        &[
            setting(
                "OAUTH_ENABLED",
                "Sign in with OAuth",
                Kind::Flag,
                "false",
                "Lets claude.ai and other OAuth clients connect by signing in on a login page instead of sending a token.",
            ),
            setting(
                "OAUTH_BASE_URL",
                "OAuth base URL",
                Kind::Text,
                "",
                "The https address clients reach OmniMem at, such as https://mcp.example.com. Required for OAuth. List several, comma separated, to sign clients in on each (a Tailscale name and a Cloudflare one, say): each is answered with the address it used. * answers any host the allowed hosts let in, with its own address. Listed hosts and origins are trusted.",
            ),
            setting(
                "OAUTH_ADMIN_USER",
                "Username",
                Kind::Text,
                "",
                "The username the OAuth login page and the settings panel ask for.",
            ),
            setting(
                "OAUTH_ADMIN_PASSWORD",
                "Password",
                Kind::Secret,
                "",
                "The password the OAuth login page and the settings panel ask for. Without one, the settings panel opens through sign-in links only.",
            ),
            setting(
                "OAUTH_REFRESH_MAX_DAYS",
                "Stay signed in for (days)",
                Kind::Integer,
                "30",
                "How long a client keeps refreshing before it has to sign in again. At most 90.",
            ),
            setting(
                "OAUTH_REFRESH_GRACE_SECONDS",
                "Refresh grace window (seconds)",
                Kind::Integer,
                "120",
                "How long a replaced refresh token keeps working, so clients refreshing at once aren't signed out. 0 turns it off.",
            ),
            setting(
                "OAUTH_LOGIN_MAX_ATTEMPTS",
                "Failed logins allowed",
                Kind::Integer,
                "10",
                "Failed logins from one address before the login page refuses it for a while. 0 turns the limit off.",
            ),
            setting(
                "OAUTH_LOGIN_WINDOW_SECONDS",
                "Failed login window (seconds)",
                Kind::Integer,
                "900",
                "How long a failed login counts towards that limit.",
            ),
        ],
    ),
    (
        "Settings panel",
        &[
            setting(
                "ADMIN_PANEL",
                "Serve the settings panel",
                Kind::Flag,
                "true",
                "This panel, at /admin on the MCP address. Turning it off closes it until the setting is changed back in the settings file.",
            ),
            setting(
                "ADMIN_SESSION_HOURS",
                "Stay signed in for (hours)",
                Kind::Integer,
                "12",
                "How long a browser stays signed in to the panel. At most 2160 (90 days).",
            ),
            setting(
                "ADMIN_ALLOW_HTTP",
                "Allow plain HTTP from other machines",
                Kind::Flag,
                "false",
                "The panel only opens over HTTPS or from this machine. Turn this on for a network that is already private, such as WireGuard or Tailscale, or a container whose port is published to localhost only.",
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
                "CONTRADICTION_MODEL",
                "Contradiction check model",
                Kind::Text,
                HAIKU,
                "Asked whether two similar memories really contradict each other.",
            ),
            setting(
                "RSS_SUMMARY_MODEL",
                "RSS summary model",
                Kind::Text,
                HAIKU,
                "Summarises feed articles and splits digests into their items.",
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
                "Rephrase every recall query with Claude unless the call says otherwise. Needs \
                 the Anthropic API key: without one, recall searches the original query alone.",
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
                "0.40",
                "Results under this are marked as weak matches. 0 turns the marker off.",
            ),
            setting(
                "RECALL_EXPERIENCE_SPAN",
                "Experience boost span",
                Kind::Decimal,
                "0",
                "Above 0, experience nudges a score by at most this much instead of multiplying it, so a loose match can't outrank a close one. Try 0.05. 0 keeps the multiplier.",
            ),
            setting(
                "GRAVEYARD_SCOPE",
                "Dead-end warnings",
                Kind::Choice(&["project", "all"]),
                "project",
                "Project keeps a recall's warnings (and the Claude Code hook's refusals) to dead ends from the project you're working in, plus any recorded with no project. A recall with no project still sees them all. All warns about any project's dead ends and says which project each came from.",
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
                "TRUST_ENRICHMENT",
                "Extracted facts start as",
                Kind::Choice(&["candidate", "accepted"]),
                "candidate",
                "What a fact or preference OmniMem extracts from your notes starts as. Candidate waits for you to confirm it on the Unconfirmed page and ranks a little lower until then. Accepted means you trust the extraction: new ones are believed from the start. Only new extractions change; ones already stored keep their trust.",
            ),
            setting(
                "TRUST_CANDIDATE_WEIGHT",
                "Unconfirmed weight",
                Kind::Decimal,
                "0.9",
                "How much a fact or preference OmniMem extracted, which no one has confirmed yet, counts in recall. 0.9 is about what 90 days of age costs a memory. Confirm them on the Unconfirmed page. 1.0 ranks them like everything else.",
            ),
            setting(
                "TRUST_DISPUTED_WEIGHT",
                "Disputed weight",
                Kind::Decimal,
                "0.8",
                "How much a memory in an open contradiction the model confirmed counts in recall. A contradiction only the quick check found doesn't lower anything. Resolve it on the Contradictions page and the weight goes.",
            ),
            setting(
                "VALIDITY_EXPIRED_WEIGHT",
                "No longer true weight",
                Kind::Decimal,
                "0.8",
                "How much a memory that has stopped being true (its valid to date has passed) counts in recall. It still comes back, with the date it ended, so an agent can tell. 1.0 ranks it like everything else.",
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
        "Audit log",
        &[
            setting(
                "AUDIT_LOG",
                "Audit log",
                Kind::Choice(&["off", "lifecycle", "writes", "full"]),
                "lifecycle",
                "Keeps a record of who changed what, and when. Off records nothing. Lifecycle, the default, records state changes and deletes (deprioritise, archive, reinstate, forget, suppression, skill writes). Writes adds every other write, such as new memories, tags, licences and restores, without their text. Full also keeps the text of a memory before and after each change, so it is the one level that stores what your memories say: anyone who can read the database or a backup made with the audit log can read it too.",
            ),
            setting(
                "AUDIT_RETENTION_DAYS",
                "Keep audit rows for (days)",
                Kind::Integer,
                "0",
                "0, the default, keeps every row, so the log can answer why the store believes something however long ago it changed. Set a number of days and older rows are removed by the maintenance pass and when OmniMem starts.",
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
                "RSS_ALLOW_PRIVATE_HOSTS",
                "Allow private addresses",
                Kind::Flag,
                "false",
                "Fetch feeds and article pages on this machine or the local network. Off, a feed can't point OmniMem at either.",
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
                "HUMAN_APPROVAL",
                "Who approves changes",
                Kind::Choice(&["human", "agent"]),
                "human",
                "Human means an agent can propose a skill, or the resolution of a contradiction, but a person has to accept it: your client asks you, or the proposal waits under Pending proposals on the Skills page or Pending on the Contradictions page. Agent lets the agent's own call commit it, as 6.x did. The two settings below set each gate on its own.",
            ),
            setting(
                "HUMAN_APPROVAL_SKILLS",
                "Who approves skill writes",
                Kind::Choice(&["human", "agent"]),
                "",
                "Only for skill writes. Not set follows Who approves changes.",
            ),
            setting(
                "HUMAN_APPROVAL_CONTRADICTIONS",
                "Who approves contradiction resolutions",
                Kind::Choice(&["human", "agent"]),
                "",
                "Only for settling contradictions. Not set follows Who approves changes.",
            ),
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
                "EMBEDDING_QUERY_PREFIX",
                "Query prefix",
                Kind::Text,
                "",
                "Put before every search query, for models that want one: bge's \"Represent this sentence for searching relevant passages:\", e5's \"query:\".",
            ),
            setting(
                "EMBEDDING_PASSAGE_PREFIX",
                "Passage prefix",
                Kind::Text,
                "",
                "Put before every memory as it's embedded (e5's \"passage:\"). Re-embed (export, then import) after changing it.",
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
        "Updates",
        &[setting(
            "UPDATE_CHECK",
            "Tell me about new releases",
            Kind::Flag,
            "on",
            "Looks at code.squarecows.com for a newer release a little after start and every 12 hours, and says so in the tray, the settings panel and the session briefing. It sends nothing but the request itself. false turns every update check off.",
        )],
    ),
    (
        "Dashboard and telemetry",
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

/// The settings a [`SecretStore`] keeps, never shown once saved.
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
    // The file can hold secrets: keep whatever permissions it has, or give
    // a new one to its owner alone.
    let permissions = std::fs::metadata(path).ok().map(|m| m.permissions());
    std::fs::write(&partial, render_settings(values))
        .and_then(|()| match permissions {
            Some(permissions) => std::fs::set_permissions(&partial, permissions),
            None => owner_only(&partial),
        })
        .and_then(|()| std::fs::rename(&partial, path))
        .map_err(|e| {
            let _ = std::fs::remove_file(&partial);
            if e.kind() == std::io::ErrorKind::ReadOnlyFilesystem {
                format!(
                    "OmniMem can't change {}: its folder is read-only to OmniMem ({e}). \
                     A systemd service sees it read-only unless the unit's ReadWritePaths \
                     includes it, as the omnimem package's does from 7.1. Edit it as an \
                     administrator, such as with sudo, then restart OmniMem",
                    path.display()
                )
            } else if e.kind() == std::io::ErrorKind::PermissionDenied {
                format!(
                    "OmniMem isn't allowed to change {} ({e}). Edit it as an administrator, \
                     such as with sudo, then restart OmniMem",
                    path.display()
                )
            } else {
                format!("Could not write {}: {e}", path.display())
            }
        })
}

#[cfg(unix)]
fn owner_only(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

/// Windows keeps a new file to its owner by the folder's ACL.
#[cfg(not(unix))]
fn owner_only(_path: &Path) -> std::io::Result<()> {
    Ok(())
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
            secret_place => store.as_ref().map_or("the system keychain", |s| s.place()),
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
    let outcome =
        tokio::task::spawn_blocking(move || save_blocking(&path, store.as_deref(), &form)).await;
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
    store: Option<&dyn SecretStore>,
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
        let Some(store) = store else {
            problems.push(format!(
                "{name} wasn't saved: there's nowhere to keep secrets"
            ));
            continue;
        };
        let result = if clear {
            store.delete(name)
        } else {
            store.set(name, value.as_deref().unwrap_or(""))
        };
        if let Err(problem) = result {
            warn!(
                setting = name,
                problem,
                place = store.place(),
                "could not save a secret"
            );
            problems.push(format!("{name}: {problem}"));
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "The settings were saved, but not every secret: {}.",
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
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(names.len(), unique.len(), "no setting is listed twice");
        assert!(names.iter().all(|n| omnimem_core::env::is_setting_name(n)));
        assert_eq!(
            secret_settings(),
            [
                "MCP_AUTH_TOKEN",
                "OAUTH_ADMIN_PASSWORD",
                "ANTHROPIC_API_KEY",
                "HF_TOKEN"
            ]
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
        assert!(validate(find("HUMAN_APPROVAL"), "agent").is_ok());
        assert!(validate(find("HUMAN_APPROVAL"), "human").is_ok());
        assert!(
            validate(find("HUMAN_APPROVAL"), "nobody")
                .unwrap_err()
                .contains("human, agent")
        );
        assert!(validate(find("RSS_REQUIRE_LICENCE"), "true").is_ok());
        assert!(validate(find("RSS_REQUIRE_LICENCE"), "yes").is_err());
        assert!(validate(find("UPDATE_CHECK"), "false").is_ok());
        assert!(validate(find("UPDATE_CHECK"), "off").is_err());
    }
}
