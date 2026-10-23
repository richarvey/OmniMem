//! Opening the engine and running the services until told to stop.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use omnimem_embed::{EmbedConfig, Embedder};
use omnimem_engine::{Engine, EngineConfig};
use omnimem_llm::{AnthropicClient, AnthropicConfig};
use omnimem_mcp::ServerConfig;
use omnimem_rss::{Ingester, RssConfig};
use omnimem_store::Store;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

/// Where the services are, for a status line or a tray icon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceState {
    /// Opening the store and loading the embedding model, which the first
    /// run downloads.
    Starting,
    Running {
        mcp_url: String,
        memories: usize,
    },
    Failed(String),
    Stopped,
}

pub fn load_embedder() -> Result<Embedder> {
    Embedder::load(&EmbedConfig::from_env()).context("loading the embedding model")
}

/// The folder holding the database: backups and `feeds.yml` live beside it.
pub fn data_dir(db: &Path) -> PathBuf {
    db.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

/// The reading list: `FEEDS_CONFIG_PATH`, or `feeds.yml` beside the database.
/// The RSS scheduler reads it and the settings panel edits it.
pub fn feeds_path(db: &Path) -> PathBuf {
    RssConfig::from_env(data_dir(db).join("feeds.yml")).feeds_path
}

/// The desktop app's data folder.
///
/// `%APPDATA%\squarecows\OmniMem\data` on Windows,
/// `~/Library/Application Support/com.squarecows.OmniMem` on macOS,
/// `$XDG_DATA_HOME/omnimem` on Linux (inside a Flatpak, the app's own).
pub fn default_data_dir() -> Result<PathBuf> {
    directories::ProjectDirs::from("com", "squarecows", "OmniMem")
        .map(|dirs| dirs.data_dir().to_path_buf())
        .ok_or_else(|| anyhow!("no home folder to keep OmniMem's data in"))
}

/// The store, migrated, with the embedder and (when a key is set) Claude.
pub fn open_engine(db: &Path) -> Result<Engine> {
    if let Some(parent) = db.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let store = Arc::new(Store::open(db).with_context(|| format!("opening {}", db.display()))?);
    store.run_migrations().context("running migrations")?;
    omnimem_engine::migrate_project_domains(&store).context("seeding project domains")?;
    let embedder = Arc::new(load_embedder()?);
    let mut config = EngineConfig::from_env(data_dir(db).join("backups"));
    config.feeds_path = Some(feeds_path(db));
    let mut engine = Engine::new(store, embedder, config);
    if let Some(llm) = AnthropicConfig::from_env() {
        let client = AnthropicClient::new(llm).context("starting the Anthropic client")?;
        engine = engine.with_llm(Arc::new(client));
        info!("ANTHROPIC_API_KEY set: Claude Haiku features and RSS summaries are on");
    } else {
        info!(
            "ANTHROPIC_API_KEY not set: Claude Haiku features are off and RSS summaries fall back to truncation"
        );
    }
    Ok(engine)
}

/// The MCP address a client on this machine should use: a wildcard bind is
/// reached through loopback.
pub fn local_mcp_url(bound: SocketAddr) -> String {
    let ip = bound.ip();
    let host = if ip.is_unspecified() {
        "127.0.0.1".to_owned()
    } else if ip.is_ipv6() {
        format!("[{ip}]")
    } else {
        ip.to_string()
    };
    format!("http://{host}:{}/mcp", bound.port())
}

/// Run every service until `shutdown` is cancelled (or, with
/// `watch_signals`, until Ctrl-C or SIGTERM), reporting each state change and
/// handing the engine to `ready` as soon as it is open.
pub fn run_services(
    db: &Path,
    shutdown: CancellationToken,
    watch_signals: bool,
    report: &dyn Fn(ServiceState),
    ready: &dyn Fn(&Arc<Engine>),
) -> Result<()> {
    report(ServiceState::Starting);
    let result = serve_until_shutdown(db, shutdown, watch_signals, report, ready);
    match &result {
        Ok(()) => report(ServiceState::Stopped),
        Err(e) => {
            error!(error = %format!("{e:#}"), "services failed");
            report(ServiceState::Failed(format!("{e:#}")));
        }
    }
    result
}

fn serve_until_shutdown(
    db: &Path,
    shutdown: CancellationToken,
    watch_signals: bool,
    report: &dyn Fn(ServiceState),
    ready: &dyn Fn(&Arc<Engine>),
) -> Result<()> {
    let config = ServerConfig::from_env();
    config.validate()?;
    let engine = Arc::new(open_engine(db)?);
    ready(&engine);

    let stop = Arc::new(AtomicBool::new(false));
    let worker = {
        let (engine, stop) = (engine.clone(), stop.clone());
        std::thread::Builder::new()
            .name("enrichment-worker".into())
            .spawn(move || engine.run_enrichment_worker(&stop))
            .context("starting the enrichment worker")?
    };
    // The RSS scheduler isn't joined on shutdown: a cycle can be waiting on a
    // feed or the API, and every write it makes is its own transaction.
    let rss = Ingester::new(
        engine.clone(),
        RssConfig::from_env(data_dir(db).join("feeds.yml")),
    )
    .map_err(|e| anyhow!("starting the RSS ingester: {e}"))?;
    std::thread::Builder::new()
        .name("rss-scheduler".into())
        .spawn({
            let stop = stop.clone();
            move || rss.run(&stop)
        })
        .context("starting the RSS scheduler")?;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting the async runtime")?;
    let served = runtime.block_on(async {
        if watch_signals {
            let shutdown = shutdown.clone();
            tokio::spawn(async move {
                wait_for_shutdown_signal().await;
                shutdown.cancel();
            });
        }
        let addr = omnimem_mcp::bind_address(&config.host, config.port);
        let listener = TcpListener::bind(&addr)
            .await
            .with_context(|| format!("could not bind {addr}"))?;
        let mcp_url = local_mcp_url(listener.local_addr()?);
        let count: Counter = {
            let engine = engine.clone();
            Arc::new(move || count_memories(&engine))
        };
        let serving = async {
            omnimem_mcp::serve(engine, config, Some(listener), shutdown)
                .await
                .map_err(anyhow::Error::from)
        };
        serve_and_recount(serving, count, RECOUNT_EVERY, &mcp_url, report).await
    });
    stop.store(true, Ordering::Relaxed);
    if worker.join().is_err() {
        error!("the enrichment worker panicked");
    }
    served
}

/// How often the running services count the memories again.
const RECOUNT_EVERY: Duration = Duration::from_secs(5);

/// Counts the memories; blocking, so it runs off the async workers.
type Counter = Arc<dyn Fn() -> usize + Send + Sync>;

/// Reports `Running` with the memory count, then drives `serving` to the end,
/// counting again every `every` and reporting whenever the number moves:
/// agents, RSS, imports and restores all change it, and the tray shows it.
async fn serve_and_recount(
    serving: impl Future<Output = Result<()>>,
    count: Counter,
    every: Duration,
    mcp_url: &str,
    report: &dyn Fn(ServiceState),
) -> Result<()> {
    let running = |memories| ServiceState::Running {
        mcp_url: mcp_url.to_owned(),
        memories,
    };
    let recount =
        |count: Counter| async move { tokio::task::spawn_blocking(move || count()).await.ok() };
    let mut memories = recount(count.clone()).await.unwrap_or(0);
    report(running(memories));
    tokio::pin!(serving);
    let mut ticks = tokio::time::interval(every);
    ticks.tick().await;
    loop {
        tokio::select! {
            served = &mut serving => return served,
            _ = ticks.tick() => {
                if let Some(now) = recount(count.clone()).await
                    && now != memories
                {
                    memories = now;
                    report(running(memories));
                }
            }
        }
    }
}

/// Every memory in the store, across namespaces; 0 if the store can't say.
fn count_memories(engine: &Engine) -> usize {
    engine
        .store()
        .count_all_records()
        .map_or(0, |counts| counts.values().sum())
}

/// Ctrl-C, or SIGTERM from a container runtime or systemd.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_urls_reach_wildcard_binds_through_loopback() {
        assert_eq!(
            local_mcp_url("0.0.0.0:8765".parse().unwrap()),
            "http://127.0.0.1:8765/mcp"
        );
        assert_eq!(
            local_mcp_url("127.0.0.1:9000".parse().unwrap()),
            "http://127.0.0.1:9000/mcp"
        );
        assert_eq!(
            local_mcp_url("[::1]:8765".parse().unwrap()),
            "http://[::1]:8765/mcp"
        );
        assert_eq!(
            local_mcp_url("[::]:8765".parse().unwrap()),
            "http://127.0.0.1:8765/mcp"
        );
    }

    #[tokio::test]
    async fn the_memory_count_is_reported_again_when_it_moves_and_only_then() {
        use std::sync::atomic::AtomicUsize;

        let url = "http://127.0.0.1:8765/mcp";
        let running = |memories| ServiceState::Running {
            mcp_url: url.to_owned(),
            memories,
        };
        let memories = Arc::new(AtomicUsize::new(3));
        let count: Counter = {
            let memories = memories.clone();
            Arc::new(move || memories.load(Ordering::SeqCst))
        };
        let reports = std::sync::Mutex::new(Vec::new());
        let report = |state: ServiceState| reports.lock().unwrap().push(state);
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let serving = async {
            let _ = stopped.await;
            Ok(())
        };
        // A few recounts with nothing new, then an import lands.
        let import = async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            memories.store(6360, Ordering::SeqCst);
            for _ in 0..500 {
                if reports.lock().unwrap().len() > 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            let _ = stop.send(());
        };
        let (served, ()) = tokio::join!(
            serve_and_recount(serving, count, Duration::from_millis(5), url, &report),
            import
        );
        served.unwrap();
        assert_eq!(*reports.lock().unwrap(), [running(3), running(6360)]);
    }

    #[test]
    fn data_dir_is_the_database_folder() {
        assert_eq!(
            data_dir(Path::new("/var/lib/omnimem/omnimem.db")),
            PathBuf::from("/var/lib/omnimem")
        );
        assert_eq!(data_dir(Path::new("omnimem.db")), PathBuf::from("."));
    }

    #[test]
    fn a_public_bind_without_a_token_fails_before_anything_opens() {
        // SAFETY: this test is the only reader of these variables in the crate.
        unsafe {
            std::env::set_var("MCP_HOST", "0.0.0.0");
            std::env::remove_var("MCP_AUTH_TOKEN");
        }
        let states = std::sync::Mutex::new(Vec::new());
        let result = run_services(
            Path::new("/nonexistent/omnimem.db"),
            CancellationToken::new(),
            false,
            &|s| states.lock().unwrap().push(s),
            &|_| {},
        );
        unsafe { std::env::remove_var("MCP_HOST") };
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("no authentication is configured")
        );
        let states = states.into_inner().unwrap();
        assert_eq!(states[0], ServiceState::Starting);
        assert!(matches!(&states[1], ServiceState::Failed(m) if m.contains("MCP_HOST=0.0.0.0")));
    }
}
