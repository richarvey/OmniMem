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
use clap::{Parser, Subcommand};
mod hook;

use omnimem_app::{data_dir, load_embedder, open_engine, run_services};
use omnimem_core::env::{install_overlay, parse_settings};
use omnimem_core::{Namespace, TextEmbedder};
use omnimem_rss::{Ingester, RssConfig};
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
    /// Import a 6.x backup (dump_to_file JSON), then re-embed every memory.
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
    Export { file: PathBuf },
    /// Record and vector counts per namespace.
    Stats,
    /// Nearest memories to a query by raw similarity. Recall scoring
    /// (recency, experience, lifecycle) arrives with the engine.
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
        /// One JSON object per line.
        #[arg(long)]
        json: bool,
    },
    /// Embed text and print the vector's first components.
    Embed { text: String },
    /// Run the desktop app: OmniMem behind a tray or menu bar icon with a
    /// settings window. What `omnimem` does when given no command.
    #[cfg(feature = "desktop")]
    Desktop {
        /// Build the tray and window, check the settings page answers over
        /// IPC, and exit, without starting the services.
        #[arg(long)]
        smoke_test: bool,
    },
    /// Answer a Claude Code PreToolUse hook: read the tool call on stdin and,
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
    },
    /// Run one RSS ingestion cycle now and print the result. With
    /// --dry-run, fetch and parse the feeds and print what would be
    /// ingested, without summarising or storing anything.
    Rss {
        #[arg(long)]
        dry_run: bool,
    },
}

fn main() -> Result<()> {
    #[cfg(all(windows, feature = "desktop"))]
    attach_parent_console();
    let Cli { db, command } = Cli::parse();
    // The desktop app reads its own settings, keychain and all; every other
    // command reads the settings file of the install it works on, before
    // anything reads a setting.
    #[cfg(feature = "desktop")]
    let desktop = matches!(command, None | Some(Command::Desktop { .. }));
    #[cfg(not(feature = "desktop"))]
    let desktop = command.is_none();
    let settings = if desktop {
        SettingsFile::None
    } else {
        load_settings_file()?
    };
    let default_level = if matches!(
        command,
        Some(
            Command::Import { .. }
                | Command::Export { .. }
                | Command::Stats
                | Command::Search { .. }
                | Command::Embed { .. }
                | Command::Hook { .. }
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
    let db = resolve_db(db, &settings)?;
    match command {
        Command::Serve => serve(&db.path),
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
        Command::Export { file } => {
            db.must_exist(None)?;
            export(&db, &file)
        }
        Command::Hook { event, project } => {
            // A hook never fails the tool call or session it answers: say why
            // in the hook log and let the work carry on.
            if let Err(error) = db.must_exist(None) {
                eprintln!("omnimem hook: {error:#}");
                return Ok(());
            }
            hook::run(&db.path, event, project.as_deref())
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
            json,
        } => {
            db.must_exist(None)?;
            search(
                &db.path, &query, &namespace, top_k, project, all_states, json,
            )
        }
        Command::Embed { text } => embed(&text),
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

fn serve(db: &Path) -> Result<()> {
    run_services(db, CancellationToken::new(), true, &|_| {}, &|_| {})
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
    eprintln!(
        "imported {} into {} in {:.1}s",
        file.display(),
        db.path.display(),
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

fn export(db: &Database, file: &Path) -> Result<()> {
    eprintln!(
        "exporting from {} (from {})",
        db.path.display(),
        db.chosen_by
    );
    let store = Store::open(&db.path)?;
    let dump = store.dump()?;
    write_backup(file, &dump)?;
    eprintln!("wrote {} keys to {}", dump.data.len(), file.display());
    Ok(())
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
    json: bool,
) -> Result<()> {
    let namespace: Namespace = namespace.parse().map_err(|e| anyhow!("{e}"))?;
    let store = Store::open(db)?;
    let embedder = load_embedder()?;
    let vector = embedder.embed(query)?;
    let filter = SearchFilter {
        states: if all_states {
            Vec::new()
        } else {
            vec!["active".into(), "deprioritised".into()]
        },
        projects,
    };
    for hit in store.search(namespace, &vector, top_k, &filter, None)? {
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
                serde_json::json!({"key": hit.key, "similarity": hit.similarity(), "snippet": snippet})
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
