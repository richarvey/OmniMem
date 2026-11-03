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
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .user_agent(concat!("omnimem/", env!("CARGO_PKG_VERSION")))
        .build()
        .ok()?;
    let body = client
        .get(RELEASES_API_URL)
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
    let running = env!("CARGO_PKG_VERSION");
    if parse_version(running).is_none() {
        debug!(
            running,
            "a development build: not looking for a newer release"
        );
        return;
    }
    let mut delay = FIRST_CHECK_AFTER;
    while wait(stop, delay) {
        match check_once(store, &fetch_latest) {
            Some(latest) if is_newer(&latest, running) => {
                info!(running, latest, "a newer OmniMem release is out");
            }
            Some(latest) => debug!(running, latest, "OmniMem is up to date"),
            None => debug!("could not check for a newer release"),
        }
        delay = CHECK_EVERY;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omnimem_engine::release::newer_release;

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
}
