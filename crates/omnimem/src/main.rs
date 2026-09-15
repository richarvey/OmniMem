//! The OmniMem binary.
//!
//! `serve` runs the MCP server; the other commands bring a 6.x backup across
//! and inspect the store. The web UI and RSS scheduler join `serve` in later
//! phases.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result, anyhow};
use clap::{Parser, Subcommand};
use omnimem_core::{Namespace, TextEmbedder};
use omnimem_embed::{EmbedConfig, Embedder};
use omnimem_engine::{Engine, EngineConfig};
use omnimem_mcp::ServerConfig;
use omnimem_store::{SearchFilter, Store, read_backup, write_backup};
use tokio_util::sync::CancellationToken;

#[derive(Parser)]
#[command(
    name = "omnimem",
    version,
    about = "Self-hosted semantic memory for AI agents"
)]
struct Cli {
    /// SQLite database file.
    #[arg(
        long,
        env = "OMNIMEM_DB",
        default_value = "data/omnimem.db",
        global = true
    )]
    db: PathBuf,

    #[command(subcommand)]
    command: Command,
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
}

fn main() -> Result<()> {
    let Cli { db, command } = Cli::parse();
    let default_level = if matches!(command, Command::Serve) {
        "info"
    } else {
        "warn"
    };
    let filter = tracing_subscriber::EnvFilter::try_from_env("OMNIMEM_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default_level));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();

    match command {
        Command::Serve => serve(&db),
        Command::Import { file, no_embed } => import(&db, &file, no_embed),
        Command::Export { file } => export(&db, &file),
        Command::Stats => stats(&db),
        Command::Search {
            query,
            namespace,
            top_k,
            project,
            all_states,
            json,
        } => search(&db, &query, &namespace, top_k, project, all_states, json),
        Command::Embed { text } => embed(&text),
    }
}

fn load_embedder() -> Result<Embedder> {
    Embedder::load(&EmbedConfig::from_env()).context("loading the embedding model")
}

fn serve(db: &Path) -> Result<()> {
    let store = Arc::new(Store::open(db).with_context(|| format!("opening {}", db.display()))?);
    store.run_migrations().context("running migrations")?;
    omnimem_engine::migrate_project_domains(&store).context("seeding project domains")?;
    let embedder = Arc::new(load_embedder()?);
    let backup_dir = db
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map_or_else(|| PathBuf::from("backups"), |p| p.join("backups"));
    let engine = Arc::new(Engine::new(
        store,
        embedder,
        EngineConfig::from_env(backup_dir),
    ));
    let config = ServerConfig::from_env();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting the async runtime")?;
    runtime.block_on(async move {
        let shutdown = CancellationToken::new();
        let signal = shutdown.clone();
        tokio::spawn(async move {
            wait_for_shutdown_signal().await;
            signal.cancel();
        });
        omnimem_mcp::serve(engine, config, None, shutdown).await
    })?;
    Ok(())
}

/// Ctrl-C, or SIGTERM from a container runtime.
async fn wait_for_shutdown_signal() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("installing the SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
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
            .map(String::as_str)
            .unwrap_or("");
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
