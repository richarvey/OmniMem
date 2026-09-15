//! The OmniMem binary.
//!
//! For now: the store and embedding commands used to bring a 6.x backup
//! across and check it. `serve` (MCP, web UI, RSS) arrives with phase 2.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, anyhow};
use clap::{Parser, Subcommand};
use omnimem_core::{Namespace, TextEmbedder};
use omnimem_embed::{EmbedConfig, Embedder};
use omnimem_store::{SearchFilter, Store, read_backup, write_backup};

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
    let filter = tracing_subscriber::EnvFilter::try_from_env("OMNIMEM_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();

    let Cli { db, command } = Cli::parse();
    match command {
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
