//! Telling people a newer release is out.
//!
//! One background thread asks the forge for the latest stable release a
//! little after start and then twice a day, and files the answer in the
//! store's kv table. Everything that shows it (`briefing()`, the tray, the
//! settings panel) reads that value and never the network. `UPDATE_CHECK=false`
//! stops the thread before it asks anything.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use omnimem_engine::release::{is_newer, parse_version, record_latest_release};
use omnimem_store::Store;
use tracing::{debug, info, warn};

const RELEASES_API_URL: &str =
    "https://code.squarecows.com/api/v1/repos/ric/omnimem/releases/latest";
/// Long enough that start-up (the model load, the first recall) isn't
/// competing with a request for nothing urgent.
const FIRST_CHECK_AFTER: Duration = Duration::from_secs(20);
const CHECK_EVERY: Duration = Duration::from_hours(12);

/// The version out of a `/releases/latest` body. That endpoint already skips
/// drafts and pre-releases.
fn latest_from_body(body: &str) -> Option<String> {
    let body: serde_json::Value = serde_json::from_str(body).ok()?;
    let version = body["tag_name"].as_str()?.trim_start_matches('v');
    // Only a plain numeric version is worth filing: anything else couldn't
    // be compared, so the readers would ignore it anyway.
    parse_version(version).map(|_| version.to_owned())
}

/// The only request OmniMem makes for this: a GET with a user agent, and
/// nothing else about the install.
fn fetch_latest() -> Option<String> {
    fetch_latest_from(RELEASES_API_URL)
}

/// [`fetch_latest`] against `url`, so a test can point it at a local server.
fn fetch_latest_from(url: &str) -> Option<String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .user_agent(concat!("omnimem/", env!("CARGO_PKG_VERSION")))
        .build()
        .ok()?;
    let body = client
        .get(url)
        .header("Accept", "application/json")
        .send()
        .ok()?
        .error_for_status()
        .ok()?
        .text()
        .ok()?;
    latest_from_body(&body)
}

/// One check: ask `fetch` for the latest version and file it. Returns it, or
/// `None` when the forge couldn't be reached, in which case the last answer
/// stays where it is.
pub fn check_once(store: &Store, fetch: &dyn Fn() -> Option<String>) -> Option<String> {
    let latest = fetch()?;
    if let Err(e) = record_latest_release(store, &latest) {
        warn!(error = %e, "could not record the latest release");
        return None;
    }
    Some(latest)
}

/// Sleep for `total`, waking each second to see whether `stop` is set.
/// Returns false if it was.
fn wait(stop: &AtomicBool, total: Duration) -> bool {
    let mut left = total;
    while !left.is_zero() {
        if stop.load(Ordering::Relaxed) {
            return false;
        }
        let step = left.min(Duration::from_secs(1));
        std::thread::sleep(step);
        left -= step;
    }
    !stop.load(Ordering::Relaxed)
}

/// Check for a newer release until `stop`. Does nothing when `UPDATE_CHECK`
/// is off, or in a development build, which is never told about updates.
pub fn run_update_checks(store: &Store, stop: &AtomicBool) {
    if !omnimem_engine::release::update_check_enabled() {
        info!("UPDATE_CHECK is off: not looking for a newer release");
        return;
    }
    check_until_stopped(
        store,
        stop,
        env!("CARGO_PKG_VERSION"),
        (FIRST_CHECK_AFTER, CHECK_EVERY),
        &fetch_latest,
    );
}

/// The loop behind [`run_update_checks`], with the running version, the
/// delays (first, then between checks) and the fetch passed in.
fn check_until_stopped(
    store: &Store,
    stop: &AtomicBool,
    running: &str,
    (first, every): (Duration, Duration),
    fetch: &dyn Fn() -> Option<String>,
) {
    if parse_version(running).is_none() {
        debug!(
            running,
            "a development build: not looking for a newer release"
        );
        return;
    }
    let mut delay = first;
    while wait(stop, delay) {
        match check_once(store, fetch) {
            Some(latest) if is_newer(&latest, running) => {
                info!(running, latest, "a newer OmniMem release is out");
            }
            Some(latest) => debug!(running, latest, "OmniMem is up to date"),
            None => debug!("could not check for a newer release"),
        }
        delay = every;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    /// `newer_release` reads `UPDATE_CHECK`, which one test here sets for the
    /// whole process, and tests run in parallel threads: each that reads or
    /// sets it holds this, or a read can land while it says `false`.
    static ENV: Mutex<()> = Mutex::new(());

    fn env_lock() -> MutexGuard<'static, ()> {
        ENV.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn newer_release(store: &Store, running: &str) -> Option<String> {
        let _env = env_lock();
        omnimem_engine::release::newer_release(store, running)
    }

    fn store() -> Store {
        let store = Store::open_in_memory().unwrap();
        store.run_migrations().unwrap();
        store
    }

    #[test]
    fn the_tag_becomes_a_version() {
        assert_eq!(
            latest_from_body(r#"{"tag_name": "v7.0.7", "name": "x"}"#).as_deref(),
            Some("7.0.7")
        );
        assert_eq!(
            latest_from_body(r#"{"tag_name": "7.1.0"}"#).as_deref(),
            Some("7.1.0")
        );
        assert_eq!(latest_from_body(r#"{"tag_name": "v7.0.7-rc1"}"#), None);
        assert_eq!(latest_from_body(r#"{"message": "not found"}"#), None);
        assert_eq!(latest_from_body("<html>"), None);
    }

    #[test]
    fn a_check_files_the_answer_and_a_failure_leaves_the_last_one() {
        let store = store();
        let found = check_once(&store, &|| Some("999.0.0".to_owned()));
        assert_eq!(found.as_deref(), Some("999.0.0"));
        assert_eq!(newer_release(&store, "7.0.5").as_deref(), Some("999.0.0"));
        assert_eq!(check_once(&store, &|| None), None);
        assert_eq!(newer_release(&store, "7.0.5").as_deref(), Some("999.0.0"));
    }

    #[test]
    fn waiting_stops_when_asked() {
        let stop = AtomicBool::new(true);
        let started = std::time::Instant::now();
        assert!(!wait(&stop, Duration::from_secs(30)));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::AtomicUsize;

    /// Serve one canned HTTP response on 127.0.0.1; returns the URL.
    fn serve_once(status: &str, body: &str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        std::thread::spawn(move || {
            if let Ok((mut conn, _)) = listener.accept() {
                let mut buf = [0u8; 2048];
                let _ = conn.read(&mut buf);
                let _ = conn.write_all(response.as_bytes());
            }
        });
        url
    }

    #[test]
    fn the_fetch_reads_the_tag_from_a_server() {
        let url = serve_once("200 OK", r#"{"tag_name": "v8.1.2"}"#);
        assert_eq!(fetch_latest_from(&url).as_deref(), Some("8.1.2"));
    }

    #[test]
    fn the_fetch_fails_quietly() {
        assert_eq!(fetch_latest_from(&serve_once("404 Not Found", "{}")), None);
        assert_eq!(fetch_latest_from(&serve_once("200 OK", "<html>")), None);
        // Nothing listening: bind, note the port, drop.
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        assert_eq!(
            fetch_latest_from(&format!("http://127.0.0.1:{port}/")),
            None
        );
        assert_eq!(fetch_latest_from("not a url"), None);
    }

    #[test]
    fn the_loop_checks_until_stopped() {
        let store = store();
        let stop = AtomicBool::new(false);
        let calls = AtomicUsize::new(0);
        let fetch = || {
            // Newer, then equal, then a failure; stop after the third.
            match calls.fetch_add(1, Ordering::SeqCst) {
                0 => Some("999.0.0".to_owned()),
                1 => Some("0.0.1".to_owned()),
                _ => {
                    stop.store(true, Ordering::SeqCst);
                    None
                }
            }
        };
        let short = Duration::from_millis(5);
        check_until_stopped(&store, &stop, "7.0.5", (short, short), &fetch);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        // The last answer filed was 0.0.1, which is not newer than 7.0.5.
        assert_eq!(newer_release(&store, "7.0.5"), None);
        assert_eq!(newer_release(&store, "0.0.0").as_deref(), Some("0.0.1"));
    }

    #[test]
    fn a_development_build_never_asks() {
        let store = store();
        let stop = AtomicBool::new(false);
        let fetch = || panic!("a development build must not fetch");
        let short = Duration::from_millis(1);
        check_until_stopped(&store, &stop, "7.0.5-dev", (short, short), &fetch);
    }

    #[test]
    fn update_check_off_returns_before_asking() {
        // The only test here that touches the environment, so it holds the
        // lock throughout and reads the store without taking it again.
        let env = env_lock();
        let store = store();
        // Already stopped, so the loop (if reached) would also return at
        // once; the point is that it returns without waiting 20 seconds.
        let stop = AtomicBool::new(true);
        unsafe { std::env::set_var("UPDATE_CHECK", "false") };
        let started = std::time::Instant::now();
        run_update_checks(&store, &stop);
        assert!(started.elapsed() < Duration::from_secs(2));
        unsafe { std::env::set_var("UPDATE_CHECK", "true") };
        run_update_checks(&store, &stop);
        unsafe { std::env::remove_var("UPDATE_CHECK") };
        assert_eq!(
            omnimem_engine::release::newer_release(&store, "7.0.5"),
            None
        );
        drop(env);
    }
}
