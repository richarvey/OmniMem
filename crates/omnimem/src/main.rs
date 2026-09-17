//! The OmniMem binary.
//!
//! `serve` runs the MCP server; the other commands bring a 6.x backup across
//! and inspect the store. `serve` also runs the enrichment
//! worker and the RSS scheduler; `rss` runs one ingestion cycle by hand.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result, anyhow};
use clap::{Parser, Subcommand};
mod hook;

use omnimem_app::{data_dir, load_embedder, open_engine, run_services};
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
    /// SQLite database file. Defaults to data/omnimem.db, or for the desktop
    /// app to the per-user data folder.
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
    Hook,
    /// Run one RSS ingestion cycle now and print the result. With
    /// --dry-run, fetch and parse the feeds and print what would be
    /// ingested, without summarising or storing anything.
    Rss {
        #[arg(long)]
        dry_run: bool,
    },
}

fn main() -> Result<()> {
    let Cli { db, command } = Cli::parse();
    let default_level = if matches!(
        command,
        Some(
            Command::Import { .. }
                | Command::Export { .. }
                | Command::Stats
                | Command::Search { .. }
                | Command::Embed { .. }
                | Command::Hook
        )
    ) {
        "warn"
    } else {
        "info"
    };
    let filter = tracing_subscriber::EnvFilter::try_from_env("OMNIMEM_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default_level));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();

    let Some(command) = command else {
        return run_default(db);
    };
    let db_path = db.unwrap_or_else(|| PathBuf::from("data/omnimem.db"));
    match command {
        Command::Serve => serve(&db_path),
        Command::Import { file, no_embed } => import(&db_path, &file, no_embed),
        Command::Export { file } => export(&db_path, &file),
        Command::Hook => hook::run(&db_path),
        Command::Stats => stats(&db_path),
        Command::Search {
            query,
            namespace,
            top_k,
            project,
            all_states,
            json,
        } => search(
            &db_path, &query, &namespace, top_k, project, all_states, json,
        ),
        Command::Embed { text } => embed(&text),
        Command::Rss { dry_run } => rss(&db_path, dry_run),
        #[cfg(feature = "desktop")]
        Command::Desktop { smoke_test } => desktop(db, smoke_test),
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

fn import(db: &Path, file: &Path, no_embed: bool) -> Result<()> {
    let started = Instant::now();
    let backup = read_backup(file).with_context(|| format!("reading {}", file.display()))?;
    let store = Store::open(db).with_context(|| format!("opening {}", db.display()))?;
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
        db.display(),
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

fn export(db: &Path, file: &Path) -> Result<()> {
    let store = Store::open(db)?;
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
