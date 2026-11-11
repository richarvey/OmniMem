//! The OmniMem binary.
//!
//! `serve` runs the MCP server; the other commands bring a 6.x backup across
//! and inspect the store. `serve` also runs the enrichment
//! worker and the RSS scheduler; `rss` runs one ingestion cycle by hand.

// With the desktop app built in, Windows starts it as a windowed program, so
// launching it from the Start menu doesn't open a console behind it. Commands
// run from a terminal borrow that terminal's console instead (see main).
#![cfg_attr(all(windows, feature = "desktop"), windows_subsystem = "windows")]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result, anyhow, bail};
use chrono::TimeZone;
use clap::{Parser, Subcommand};
mod hook;
mod in_person;
mod install_hooks;
mod trust_cli;

use omnimem_app::{data_dir, feeds_path, load_embedder, open_engine, run_services};
use omnimem_core::env::{install_overlay, parse_settings};
use omnimem_core::{Namespace, TextEmbedder};
use omnimem_rss::{Ingester, RssConfig};
use omnimem_settings::{
    PRINTED_CODE_LIFETIME, Panel, SettingsFileSecrets, StaticOverhead, mint_sign_in_code,
    sign_in_url,
};
use omnimem_store::{SearchFilter, Store, read_backup, write_backup};
use tokio_util::sync::CancellationToken;

#[derive(Parser)]
#[command(
    name = "omnimem",
    version,
    about = "Self-hosted semantic memory for AI agents"
)]
struct Cli {
    /// SQLite database file. Defaults to OMNIMEM_DB from the settings file,
    /// then on Linux to /var/lib/omnimem/omnimem.db where the server package
    /// is installed, then to omnimem.db in the desktop app's data folder.
    #[arg(long, env = "OMNIMEM_DB", global = true)]
    db: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the MCP server (streamable HTTP at /mcp). Configured by the 6.x
    /// environment variables: MCP_HOST, MCP_PORT, MCP_AUTH_TOKEN and friends.
    Serve,
    /// Import a backup (dump_to_file JSON, 6.x's or 7's), then re-embed every
    /// memory. RSS feeds in the file are added to the reading list, leaving
    /// any feed it already has as it is.
    Import {
        file: PathBuf,
        /// Skip embedding. Memories aren't searchable until embedded.
        #[arg(long)]
        no_embed: bool,
        /// Start a new database if there isn't one yet. Without it, import
        /// only adds to a database that already exists.
        #[arg(long)]
        create: bool,
    },
    /// Write every memory and log to a backup file in the 6.x format.
    Export {
        file: PathBuf,
        /// Put the RSS reading list (feeds.yml) in the backup too.
        #[arg(long)]
        feeds: bool,
        /// Put the audit log in the backup too. It can be large, and at
        /// AUDIT_LOG=full it holds memory text, so it is only included when
        /// asked for.
        #[arg(long)]
        audit: bool,
    },
    /// Show the audit log: who changed what, and when. It stays empty until
    /// AUDIT_LOG is set to lifecycle, writes or full; see docs/audit-log.md.
    /// Newest first.
    Audit {
        /// Only changes to this memory, project or skill key (or this
        /// suppressed topic).
        #[arg(long)]
        key: Option<String>,
        /// Only changes since this long ago (30m, 12h, 7d, 2w), or since this
        /// date or time (2026-11-03, 2026-11-03T14:30, UTC).
        #[arg(long)]
        since: Option<String>,
        /// Only changes by this actor: mcp, panel, hook, maintenance,
        /// enrichment, rss, cli or import.
        #[arg(long)]
        actor: Option<String>,
        /// Only this action, such as transition, forget, retag, remember or
        /// suppress.
        #[arg(long)]
        action: Option<String>,
        /// Most rows to show.
        #[arg(long, default_value_t = 50)]
        limit: usize,
        /// One JSON object per line, with the full from and to.
        #[arg(long)]
        json: bool,
    },
    /// Memories linked as contradicting each other: the pairs still in
    /// dispute and the resolutions waiting for a person. With `resolve`,
    /// settle one, from a terminal.
    Contradictions {
        #[command(subcommand)]
        action: Option<ContradictionAction>,
        /// One JSON object instead of the lists.
        #[arg(long, global = true)]
        json: bool,
    },
    /// Facts and preferences OmniMem concluded that nobody has confirmed
    /// yet: list them, confirm them, or reject a memory that is wrong. Only a
    /// person can do this, here or in the settings panel, never an agent.
    Trust {
        #[command(subcommand)]
        action: trust_cli::TrustCommand,
    },
    /// Record and vector counts per namespace.
    Stats,
    /// Nearest memories to a query by raw similarity, without recall's
    /// scoring (recency, experience, lifecycle) or its relevance floor.
    Search {
        query: String,
        #[arg(long, default_value = "episodic")]
        namespace: String,
        #[arg(long, default_value_t = 10)]
        top_k: usize,
        /// Restrict to projects (repeatable).
        #[arg(long)]
        project: Vec<String>,
        /// Include archived memories.
        #[arg(long)]
        all_states: bool,
        /// Only memories true at this time: an ISO date, a timestamp or a
        /// phrase ("last March").
        #[arg(long)]
        valid_at: Option<String>,
        /// Only memories OmniMem believed at this time.
        #[arg(long)]
        known_at: Option<String>,
        /// One JSON object per line.
        #[arg(long)]
        json: bool,
    },
    /// Embed text and print the vector's first components.
    Embed { text: String },
    /// Print a link that signs a browser in to the settings panel at /admin.
    /// It works once, within five minutes, so a server with no admin
    /// password can still be configured: in Docker, run it with
    /// `docker exec <container> omnimem admin-link`.
    AdminLink,
    /// Run the desktop app: OmniMem behind a tray or menu bar icon that opens
    /// the settings panel in the browser. What `omnimem` does when given no
    /// command.
    #[cfg(feature = "desktop")]
    Desktop {
        /// Build the tray, sign in to the settings panel over HTTP on a spare
        /// port, and exit, without starting the services.
        #[arg(long)]
        smoke_test: bool,
    },
    /// Answer a Claude Code PreToolUse hook (or OmniMem's OpenCode plugin,
    /// which sends the same JSON, or with --agent vibe, Mistral Vibe's
    /// pre_tool hook): read the tool call on stdin and,
    /// if it proposes an approach this project already abandoned, deny it and
    /// explain what worked instead. Prints nothing when the call is fine, and
    /// stays quiet on any error, so work is never blocked by a sick store.
    /// See docs/claude-code-hook.md for the settings.json entry.
    ///
    /// With `session-start`, instead prints what this project already knows as
    /// the session's opening context: where the work stands, and what has been
    /// tried and abandoned.
    Hook {
        /// Which hook is calling. Defaults to the PreToolUse guard.
        #[arg(value_enum, default_value = "pre-tool-use")]
        event: hook::Event,
        /// Project to brief on. Defaults to OMNIMEM_PROJECT, then the
        /// directory name.
        #[arg(long)]
        project: Option<String>,
        /// The agent calling, which decides how the answer is written.
        #[arg(long, value_enum, default_value = "claude-code")]
        agent: hook::Agent,
    },
    /// Add OmniMem's Claude Code hooks (session-start, the pre-tool-use guard
    /// and session-end) to ~/.claude/settings.json, leaving alone any event
    /// that already has one and everything else in the file. With --db, the
    /// hooks use that database. See docs/claude-code-hook.md.
    InstallHooks {
        /// Write .claude/settings.json in the current folder instead, for
        /// this project only.
        #[arg(long)]
        project: bool,
        /// Show what would be added without writing anything.
        #[arg(long)]
        dry_run: bool,
        /// Write OmniMem's OpenCode plugin instead, which runs the same
        /// dead-end guard and session-start briefing from inside OpenCode:
        /// to ~/.config/opencode/plugins, or with --project to
        /// .opencode/plugins here. See guides/opencode.md.
        #[arg(long, conflicts_with = "vibe")]
        opencode: bool,
        /// Put OmniMem's hooks in Mistral Vibe's hooks.toml instead: the
        /// dead-end guard and, from Vibe 3.1.1, the session-start briefing,
        /// in ~/.vibe (or $VIBE_HOME), or with --project in .vibe here. See
        /// guides/vibe.md.
        #[arg(long)]
        vibe: bool,
    },
    /// Run one RSS ingestion cycle now and print the result. With
    /// --dry-run, fetch and parse the feeds and print what would be
    /// ingested, without summarising or storing anything.
    Rss {
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
enum ContradictionAction {
    /// Record how a pair was settled. The decision is yours: this is the
    /// command line's equivalent of a click in the settings panel.
    Resolve {
        key_a: String,
        key_b: String,
        /// supersedes, both_valid, both_wrong, false_positive or reopened.
        #[arg(long)]
        outcome: String,
        /// For supersedes: the key of the memory that is current.
        #[arg(long)]
        winner: Option<String>,
        #[arg(long)]
        reason: Option<String>,
        /// Deprioritise the out-of-date memory (both, for both_wrong). Give
        /// it `archive` to archive instead.
        #[arg(
            long,
            value_name = "deprioritise|archive",
            num_args = 0..=1,
            default_missing_value = "deprioritise"
        )]
        retire_loser: Option<String>,
    },
    /// Write a resolution an agent proposed, as it was proposed.
    Accept { key_a: String, key_b: String },
    /// Throw a proposed resolution away.
    Discard { key_a: String, key_b: String },
}

fn main() -> Result<()> {
    #[cfg(all(windows, feature = "desktop"))]
    attach_parent_console();
    let Cli { db, command } = Cli::parse();
    // The desktop app reads its own settings, keychain and all, and
    // install-hooks needs none; every other command reads the settings file
    // of the install it works on, before anything reads a setting.
    #[cfg(feature = "desktop")]
    let runs_desktop = matches!(command, None | Some(Command::Desktop { .. }));
    #[cfg(not(feature = "desktop"))]
    let runs_desktop = command.is_none();
    let settings = if runs_desktop || matches!(command, Some(Command::InstallHooks { .. })) {
        SettingsFile::None
    } else {
        load_settings_file()?
    };
    let default_level = if matches!(
        command,
        Some(
            Command::Import { .. }
                | Command::Export { .. }
                | Command::Audit { .. }
                | Command::Contradictions { .. }
                | Command::Trust { .. }
                | Command::Stats
                | Command::Search { .. }
                | Command::Embed { .. }
                | Command::AdminLink
                | Command::Hook { .. }
                | Command::InstallHooks { .. }
        )
    ) {
        "warn"
    } else {
        "info"
    };
    let filter = omnimem_core::env::var("OMNIMEM_LOG")
        .and_then(|level| tracing_subscriber::EnvFilter::try_new(level).ok())
        .unwrap_or_else(|| tracing_subscriber::EnvFilter::new(default_level));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();

    if let SettingsFile::Unreadable(path, error) = &settings {
        tracing::warn!(
            "can't read {} ({error}), so its settings don't apply to this command. \
             Run it as a user that can, such as the service's: sudo -u omnimem omnimem ...",
            path.display()
        );
    }

    let Some(command) = command else {
        return run_default(db);
    };
    #[cfg(feature = "desktop")]
    if let Command::Desktop { smoke_test } = command {
        return desktop(db, smoke_test);
    }
    if let Command::InstallHooks {
        project,
        dry_run,
        opencode,
        vibe,
    } = command
    {
        let agent = if opencode {
            install_hooks::Target::OpenCode
        } else if vibe {
            install_hooks::Target::Vibe
        } else {
            install_hooks::Target::ClaudeCode
        };
        return install_hooks::run(db.as_deref(), project, dry_run, agent);
    }
    let db = resolve_db(db, &settings)?;
    // What a command writes is the command line's, unless it says otherwise.
    let _audit = omnimem_store::enter(omnimem_store::Actor::Cli, "");
    match command {
        Command::Serve => serve(&db.path, &settings),
        Command::AdminLink => {
            db.must_exist(Some("start the server once first"))?;
            admin_link(&db.path)
        }
        Command::Import {
            file,
            no_embed,
            create,
        } => {
            if !create {
                db.must_exist(Some("pass --create to start a new one there"))?;
            }
            import(&db, &file, no_embed)
        }
        Command::Export { file, feeds, audit } => {
            db.must_exist(None)?;
            export(&db, &file, feeds, audit)
        }
        Command::Audit {
            key,
            since,
            actor,
            action,
            limit,
            json,
        } => {
            db.must_exist(None)?;
            audit_log(&db.path, key, since, actor, action, limit, json)
        }
        Command::Contradictions { action, json } => {
            db.must_exist(None)?;
            contradictions(&db.path, action, json)
        }
        Command::Trust { action } => {
            db.must_exist(None)?;
            trust_cli::run(&db.path, action)
        }
        Command::Hook {
            event,
            project,
            agent,
        } => {
            // A hook never fails the tool call or session it answers: say why
            // in the hook log and let the work carry on.
            if let Err(error) = db.must_exist(None) {
                eprintln!("omnimem hook: {error:#}");
                return Ok(());
            }
            hook::run(&db.path, event, project.as_deref(), agent)
        }
        Command::Stats => {
            db.must_exist(None)?;
            stats(&db.path)
        }
        Command::Search {
            query,
            namespace,
            top_k,
            project,
            all_states,
            valid_at,
            known_at,
            json,
        } => {
            db.must_exist(None)?;
            let as_of = omnimem_engine::AsOf::parse(valid_at.as_deref(), known_at.as_deref())
                .map_err(|e| anyhow!("{e}"))?;
            search(
                &db.path, &query, &namespace, top_k, project, all_states, as_of, json,
            )
        }
        Command::Embed { text } => embed(&text),
        Command::InstallHooks { .. } => unreachable!("handled above"),
        Command::Rss { dry_run } => {
            db.must_exist(None)?;
            rss(&db.path, dry_run)
        }
        #[cfg(feature = "desktop")]
        Command::Desktop { .. } => unreachable!("handled above"),
    }
}

/// The server package's settings file and database. The service is pointed
/// at the settings file explicitly; other commands look for it, so that run
/// on a server they work on the server's store with the server's settings.
#[cfg(target_os = "linux")]
const SERVER_SETTINGS: &str = "/etc/omnimem/omnimem.env";
#[cfg(target_os = "linux")]
const SERVER_DATA: &str = "/var/lib/omnimem";

/// What `load_settings_file` found.
enum SettingsFile {
    None,
    Loaded(PathBuf),
    /// There, but not readable by whoever ran the command.
    Unreadable(PathBuf, std::io::Error),
}

/// Install the settings file as the overlay `omnimem_core::env` reads: the
/// file `OMNIMEM_ENV_FILE` names, else the server package's, else the desktop
/// app's `omnimem.env`. An environment variable still wins over the file.
///
/// The server reads its file this way rather than through systemd's
/// `EnvironmentFile`, which would hand it `8765 # the default` for a line
/// with a trailing comment.
fn load_settings_file() -> Result<SettingsFile> {
    if let Some(path) = std::env::var_os("OMNIMEM_ENV_FILE").filter(|p| !p.is_empty()) {
        let path = PathBuf::from(path);
        // Named explicitly, as the service does: a file that can't be read
        // is an error, not something to start without.
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading OMNIMEM_ENV_FILE ({})", path.display()))?;
        install_overlay(parse_settings(&text));
        return Ok(SettingsFile::Loaded(path));
    }
    let mut candidates = Vec::new();
    #[cfg(target_os = "linux")]
    candidates.push(PathBuf::from(SERVER_SETTINGS));
    if let Ok(dir) = omnimem_app::default_data_dir() {
        candidates.push(dir.join("omnimem.env"));
    }
    for path in candidates {
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                install_overlay(parse_settings(&text));
                return Ok(SettingsFile::Loaded(path));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Ok(SettingsFile::Unreadable(path, e)),
        }
    }
    Ok(SettingsFile::None)
}

/// The database a command works on, and how it was chosen, for messages.
struct Database {
    path: PathBuf,
    chosen_by: String,
}

/// `--db` or `OMNIMEM_DB`, then `OMNIMEM_DB` from the settings file, then the
/// server package's database where that is installed, then the desktop app's.
/// Never a path relative to wherever the command happens to run.
fn resolve_db(flag: Option<PathBuf>, settings: &SettingsFile) -> Result<Database> {
    if let Some(path) = flag.filter(|p| !p.as_os_str().is_empty()) {
        return Ok(Database {
            path,
            chosen_by: "--db or OMNIMEM_DB".into(),
        });
    }
    if let Some(path) = omnimem_core::env::var("OMNIMEM_DB").filter(|p| !p.trim().is_empty()) {
        let file = match settings {
            SettingsFile::Loaded(file) => file.display().to_string(),
            _ => "the settings file".into(),
        };
        return Ok(Database {
            path: PathBuf::from(path.trim()),
            chosen_by: format!("OMNIMEM_DB in {file}"),
        });
    }
    #[cfg(target_os = "linux")]
    if Path::new(SERVER_DATA).is_dir() {
        return Ok(Database {
            path: Path::new(SERVER_DATA).join("omnimem.db"),
            chosen_by: "the server package's default".into(),
        });
    }
    Ok(Database {
        path: omnimem_app::default_data_dir()?.join("omnimem.db"),
        chosen_by: "the desktop app's data folder".into(),
    })
}

impl Database {
    /// Commands that work on an existing store refuse to start a new, empty
    /// one somewhere unexpected; `create_hint` says how to ask for one.
    fn must_exist(&self, create_hint: Option<&str>) -> Result<()> {
        let path = self.path.display();
        let chosen_by = &self.chosen_by;
        match self.path.try_exists() {
            Ok(true) => Ok(()),
            Ok(false) => {
                let create = create_hint
                    .map(|hint| format!(", or {hint}"))
                    .unwrap_or_default();
                bail!(
                    "there's no OmniMem database at {path} (from {chosen_by}). \
                     Point at yours with --db <file> or OMNIMEM_DB{create}"
                )
            }
            Err(e) => bail!(
                "can't reach the database at {path} (from {chosen_by}): {e}. \
                 Run this as a user that can, such as the service's: sudo -u omnimem omnimem ..."
            ),
        }
    }
}

/// A windowed build has no console of its own: when it was started from a
/// terminal, print to the terminal's. Started from the Start menu there is
/// no parent console, and this quietly does nothing.
#[cfg(all(windows, feature = "desktop"))]
fn attach_parent_console() {
    use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
    // SAFETY: no pointers; it only fails when there is no console to attach to.
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

/// No command: the desktop app when it is built in, otherwise the help.
#[cfg(feature = "desktop")]
fn run_default(db: Option<PathBuf>) -> Result<()> {
    desktop(db, false)
}

#[cfg(not(feature = "desktop"))]
fn run_default(_db: Option<PathBuf>) -> Result<()> {
    use clap::CommandFactory;
    Cli::command().print_help()?;
    std::process::exit(2);
}

#[cfg(feature = "desktop")]
fn desktop(db: Option<PathBuf>, smoke_test: bool) -> Result<()> {
    let db = match db {
        Some(db) => db,
        None => omnimem_app::default_data_dir()?.join("omnimem.db"),
    };
    omnimem_desktop::run(omnimem_desktop::DesktopOptions {
        data_dir: data_dir(&db),
        db,
        smoke_test,
    })
}

/// The MCP server, with the settings panel at `/admin`. Its Configuration
/// page edits the settings file this run loaded, secrets and all, since a
/// server has no keychain.
fn serve(db: &Path, settings: &SettingsFile) -> Result<()> {
    let panel = Panel::new();
    panel.set_feeds_path(feeds_path(db));
    if let SettingsFile::Loaded(path) = settings {
        panel.set_settings_path(path.clone());
        panel.set_secret_store(Arc::new(SettingsFileSecrets::new(path.clone())));
    }
    let overhead = omnimem_app::context_overhead();
    panel.set_static_overhead(StaticOverhead {
        instructions_chars: overhead.instructions_chars,
        tool_count: overhead.tool_count,
        tool_schemas_chars: overhead.tool_schemas_chars,
        deferred_names_chars: overhead.deferred_names_chars,
    });
    run_services(
        db,
        CancellationToken::new(),
        true,
        Some(panel),
        &|_| {},
        &|_| {},
    )
}

/// Mint a sign-in code in the store the server reads and print the link.
fn admin_link(db: &Path) -> Result<()> {
    if !omnimem_settings::admin_panel_enabled() {
        bail!("the settings panel is off here (ADMIN_PANEL=false)");
    }
    let store = Store::open(db).with_context(|| format!("opening {}", db.display()))?;
    let code = mint_sign_in_code(&store, PRINTED_CODE_LIFETIME).map_err(|e| anyhow!(e))?;
    println!("{}", sign_in_url(&server_address(), &code));
    eprintln!(
        "Open it within {} minutes; it works once.",
        PRINTED_CODE_LIFETIME.as_secs() / 60
    );
    Ok(())
}

/// Where a browser reaches this server: the first address `MCP_PUBLIC_URL`
/// or `OAUTH_BASE_URL` lists, or this machine's port.
fn server_address() -> String {
    ["MCP_PUBLIC_URL", "OAUTH_BASE_URL"]
        .iter()
        .filter_map(|name| omnimem_core::env::var(name))
        .flat_map(|urls| {
            urls.split(',')
                .map(|u| u.trim().to_owned())
                .collect::<Vec<_>>()
        })
        .find(|url| url.starts_with("http://") || url.starts_with("https://"))
        .unwrap_or_else(|| {
            let port = omnimem_core::env::var("MCP_PORT")
                .and_then(|p| p.trim().parse::<u16>().ok())
                .unwrap_or(8765);
            format!("http://127.0.0.1:{port}")
        })
}

/// One RSS cycle by hand, or a dry run of what one would ingest.
fn rss(db: &Path, dry_run: bool) -> Result<()> {
    let engine = Arc::new(open_engine(db)?);
    let ingester = Ingester::new(engine, RssConfig::from_env(data_dir(db).join("feeds.yml")))
        .map_err(|e| anyhow!("starting the RSS ingester: {e}"))?;
    let result = if dry_run {
        ingester.preview()
    } else {
        ingester.ingest_all()
    };
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

fn import(db: &Database, file: &Path, no_embed: bool) -> Result<()> {
    let started = Instant::now();
    eprintln!(
        "importing into {} (from {})",
        db.path.display(),
        db.chosen_by
    );
    let backup = read_backup(file).with_context(|| format!("reading {}", file.display()))?;
    let store = Store::open(&db.path).with_context(|| format!("opening {}", db.path.display()))?;
    // This store isn't opened through an engine, so it reads AUDIT_LOG itself.
    store.set_audit_level(omnimem_engine::audit_level_from_env());
    let _audit = omnimem_store::enter(omnimem_store::Actor::Import, "");
    let embedder = if no_embed {
        None
    } else {
        Some(load_embedder()?)
    };
    let mut last = Instant::now();
    let mut progress = |done: usize, total: usize| {
        if done == total || last.elapsed().as_millis() >= 250 {
            eprint!("\rembedding {done}/{total}");
            let _ = std::io::stderr().flush();
            last = Instant::now();
        }
    };
    let report = store.restore_backup(
        &backup,
        embedder.as_ref().map(|e| e as &dyn TextEmbedder),
        &mut progress,
    )?;
    if report.embedded > 0 {
        eprintln!();
    }
    omnimem_engine::migrate_project_domains(&store).context("seeding project domains")?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    if let Some(feeds) = &backup.feeds {
        import_feeds(&feeds_path(&db.path), feeds)?;
    }
    if let Some(rows) = &backup.audit {
        let (added, skipped) = store.merge_audit(rows)?;
        eprintln!("audit log: {added} rows added, {skipped} already there");
    }
    eprintln!(
        "imported {} into {} in {:.1}s",
        file.display(),
        db.path.display(),
        started.elapsed().as_secs_f64()
    );
    if no_embed {
        eprintln!(
            "nothing was embedded, so recall can't find these until it's imported again without --no-embed"
        );
    } else {
        eprintln!(
            "a server already running on this database finds them on its next recall, no restart needed"
        );
    }
    Ok(())
}

/// Add a backup's feeds to the reading list. The running server notices the
/// changed file and its next RSS cycle mirrors the list for the skill
/// compiler, so nothing else needs telling.
fn import_feeds(path: &Path, feeds: &[serde_json::Value]) -> Result<()> {
    use omnimem_engine::feeds_file;
    let existing = feeds_file::load(path).map_err(|e| anyhow!(e))?;
    let (merged, report) = feeds_file::merge(&existing, feeds);
    if report.added > 0 {
        feeds_file::save(path, &merged).map_err(|e| anyhow!(e))?;
    }
    eprintln!(
        "RSS feeds: {} in the backup, {} added to {}, {} already listed, {} invalid",
        report.in_backup,
        report.added,
        path.display(),
        report.already_present,
        report.invalid
    );
    Ok(())
}

fn export(db: &Database, file: &Path, include_feeds: bool, include_audit: bool) -> Result<()> {
    eprintln!(
        "exporting from {} (from {})",
        db.path.display(),
        db.chosen_by
    );
    let store = Store::open(&db.path)?;
    let mut dump = store.dump()?;
    if include_feeds {
        let path = feeds_path(&db.path);
        let feeds = omnimem_engine::feeds_file::load(&path).map_err(|e| anyhow!(e))?;
        let plural = if feeds.len() == 1 { "" } else { "s" };
        eprintln!(
            "including {} RSS feed{plural} from {}",
            feeds.len(),
            path.display()
        );
        dump.feeds = Some(feeds.into_iter().map(serde_json::Value::Object).collect());
    }
    if include_audit {
        let rows = store.export_audit()?;
        let plural = if rows.len() == 1 { "" } else { "s" };
        eprintln!("including {} audit log row{plural}", rows.len());
        dump.audit = Some(rows);
    }
    write_backup(file, &dump)?;
    eprintln!("wrote {} keys to {}", dump.data.len(), file.display());
    Ok(())
}

/// The audit log, newest first, as a table or one JSON object a line.
fn audit_log(
    db: &Path,
    key: Option<String>,
    since: Option<String>,
    actor: Option<String>,
    action: Option<String>,
    limit: usize,
    json: bool,
) -> Result<()> {
    let store = Store::open(db)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64());
    let since = since
        .map(|raw| {
            omnimem_engine::parse_when(&raw, now).ok_or_else(|| {
                anyhow!("can't read --since {raw:?}: try 7d, 12h, 2026-11-03 or 2026-11-03T14:30")
            })
        })
        .transpose()?;
    if let Some(actor) = &actor
        && omnimem_store::Actor::parse(actor).is_none()
    {
        bail!(
            "--actor {actor:?} isn't one of mcp, panel, hook, maintenance, enrichment, rss, cli or import"
        );
    }
    let rows = store.audit_entries(&omnimem_store::AuditQuery {
        key,
        actor: actor.map(|a| a.to_ascii_lowercase()),
        action,
        since,
        limit: limit.max(1),
        ..Default::default()
    })?;
    if rows.is_empty() && !json {
        let level = omnimem_engine::audit_level_from_env();
        if level == omnimem_engine::AuditLevel::Off && store.audit_count()? == 0 {
            eprintln!(
                "the audit log is empty, and AUDIT_LOG is off: set it to lifecycle (the default, by leaving it unset), writes or full to start keeping one"
            );
        } else {
            eprintln!("no audit rows match");
        }
        return Ok(());
    }
    for row in rows {
        if json {
            println!("{}", row.to_json());
            continue;
        }
        let when = chrono::Local
            .timestamp_opt(row.at as i64, 0)
            .single()
            .map_or_else(String::new, |t| t.format("%Y-%m-%d %H:%M:%S").to_string());
        let who = if row.client.is_empty() {
            row.actor.clone()
        } else {
            format!("{} ({})", row.actor, row.client)
        };
        let change = omnimem_engine::change_summary(&row.from, &row.to, 60);
        let reason = if row.reason.is_empty() {
            String::new()
        } else {
            format!("  [{}]", row.reason)
        };
        println!(
            "{when}  {who}  {}  {}  {change}{reason}",
            row.action, row.key
        );
    }
    Ok(())
}

/// List the open and pending contradictions, or act on one.
fn contradictions(db: &Path, action: Option<ContradictionAction>, json: bool) -> Result<()> {
    let engine = hook::open_engine(db)?;
    let say = |result: serde_json::Value| {
        if json {
            println!("{result}");
        } else {
            println!("{}", result["status"].as_str().unwrap_or("done"));
        }
    };
    match action {
        None => {
            let open = engine.open_contradictions().map_err(|e| anyhow!("{e}"))?;
            let pending = engine.pending_resolutions().map_err(|e| anyhow!("{e}"))?;
            if json {
                println!("{}", serde_json::json!({"open": open, "pending": pending}));
                return Ok(());
            }
            let snippet = |v: &serde_json::Value| {
                v.as_str()
                    .unwrap_or("")
                    .chars()
                    .take(70)
                    .collect::<String>()
                    .replace('\n', " ")
            };
            if open.is_empty() && pending.is_empty() {
                eprintln!("no contradictions in dispute");
            }
            for pair in &open {
                println!(
                    "open     {}  {}",
                    pair["key_a"].as_str().unwrap_or(""),
                    snippet(&pair["content_a"])
                );
                println!(
                    "         {}  {}",
                    pair["key_b"].as_str().unwrap_or(""),
                    snippet(&pair["content_b"])
                );
            }
            for p in &pending {
                let winner = p["winner"]
                    .as_str()
                    .filter(|w| !w.is_empty())
                    .map_or(String::new(), |w| format!(", {w} is current"));
                println!(
                    "pending  {}  {}  {}{winner}",
                    p["key_a"].as_str().unwrap_or(""),
                    p["key_b"].as_str().unwrap_or(""),
                    p["outcome"].as_str().unwrap_or(""),
                );
            }
            Ok(())
        }
        Some(ContradictionAction::Resolve {
            key_a,
            key_b,
            outcome,
            winner,
            reason,
            retire_loser,
        }) => {
            let retire = omnimem_engine::Retire::parse(retire_loser.as_deref().unwrap_or("no"))
                .map_err(|e| anyhow!("{e}"))?;
            let request = omnimem_engine::ResolveRequest::new(
                &key_a,
                &key_b,
                &outcome,
                winner.as_deref(),
                reason.as_deref(),
                retire,
            )
            .map_err(|e| anyhow!("{e}"))?;
            in_person::confirm(&format!("resolve this pair as {outcome}"), &key_a)?;
            say(engine
                .resolve_contradiction(&request, "cli")
                .map_err(|e| anyhow!("{e}"))?);
            Ok(())
        }
        Some(ContradictionAction::Accept { key_a, key_b }) => {
            in_person::confirm("accept this resolution", &key_a)?;
            match engine
                .accept_pending_resolution(&key_a, &key_b, "cli")
                .map_err(|e| anyhow!("{e}"))?
            {
                Some(result) => say(result),
                None => bail!("no pending resolution for that pair (it may have expired)"),
            }
            Ok(())
        }
        Some(ContradictionAction::Discard { key_a, key_b }) => {
            if !engine
                .discard_pending_resolution(&key_a, &key_b)
                .map_err(|e| anyhow!("{e}"))?
            {
                bail!("no pending resolution for that pair");
            }
            say(serde_json::json!({"status": "discarded"}));
            Ok(())
        }
    }
}

fn stats(db: &Path) -> Result<()> {
    let store = Store::open(db)?;
    println!("{:<12} {:>8} {:>8}", "namespace", "records", "vectors");
    for (namespace, records) in store.count_all_records()? {
        println!(
            "{:<12} {:>8} {:>8}",
            namespace.as_str(),
            records,
            store.vector_count(namespace)
        );
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn search(
    db: &Path,
    query: &str,
    namespace: &str,
    top_k: usize,
    projects: Vec<String>,
    all_states: bool,
    as_of: omnimem_engine::AsOf,
    json: bool,
) -> Result<()> {
    let namespace: Namespace = namespace.parse().map_err(|e| anyhow!("{e}"))?;
    let store = Store::open(db)?;
    let embedder = load_embedder()?;
    let vector = embedder.embed_queries(&[query])?.pop().unwrap_or_default();
    let filter = SearchFilter {
        states: if all_states {
            Vec::new()
        } else {
            vec!["active".into(), "deprioritised".into()]
        },
        projects,
        // `omnimem search` is a raw look at the store, so it shows every
        // trust; each hit says which.
        trusts: Vec::new(),
        valid_at: as_of.valid_at,
        known_at: as_of.known_at,
    };
    for hit in store.search(namespace, &vector, top_k, &filter, None)? {
        // The store's rows are a superset when both times are given.
        if let Some(valid) = as_of.valid_at
            && !omnimem_store::valid_at_as_known(&hit.fields, valid, as_of.known_at)
        {
            continue;
        }
        let text = hit
            .fields
            .get("content")
            .or_else(|| hit.fields.get("name"))
            .map_or("", String::as_str);
        let snippet: String = text
            .chars()
            .take(100)
            .collect::<String>()
            .replace('\n', " ");
        if json {
            println!(
                "{}",
                serde_json::json!({
                    "key": hit.key,
                    "similarity": hit.similarity(),
                    "snippet": snippet,
                    "valid_to": omnimem_store::valid_to_of(&hit.fields).and_then(omnimem_engine::validity::iso),
                    "retired_at": omnimem_store::retired_at_of(&hit.fields).and_then(omnimem_engine::validity::iso),
                })
            );
        } else {
            println!("{:.4}  {}  {}", hit.similarity(), hit.key, snippet);
        }
    }
    Ok(())
}

fn embed(text: &str) -> Result<()> {
    let embedder = load_embedder()?;
    let vector = embedder.embed(text)?;
    let head: Vec<String> = vector.iter().take(8).map(|x| format!("{x:.6}")).collect();
    println!("dimension {}: [{}, ...]", vector.len(), head.join(", "));
    Ok(())
}
