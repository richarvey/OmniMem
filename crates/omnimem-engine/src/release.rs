//! What the store knows about the newest release.
//!
//! Nothing here touches the network. The app's background check
//! (`omnimem_app::run_update_checks`) writes the latest stable version to the
//! kv table, and `briefing()`, the tray and the settings panel read it back,
//! so a briefing stays fast and works offline.

use omnimem_store::Store;
use serde_json::{Value, json};
use tracing::warn;

use crate::Result;
use crate::pyfmt::now_secs;

/// The kv key holding `{"version": "7.0.7", "checked_at": <unix seconds>}`.
pub const LATEST_RELEASE_KEY: &str = "meta:latest_release";

/// `UPDATE_CHECK`: on unless set to `false`, `0` or `no`. Off stops every
/// update check, the panel's marker and the briefing's line included.
pub fn update_check_enabled() -> bool {
    omnimem_core::env::var("UPDATE_CHECK")
        .is_none_or(|v| !matches!(v.trim().to_ascii_lowercase().as_str(), "false" | "0" | "no"))
}

/// `7.0.10` as `[7, 0, 10]`. A version that isn't plain numbers (`7.0.0-dev`)
/// is a development build and gets `None`, which opts it out of notices.
pub fn parse_version(version: &str) -> Option<Vec<u64>> {
    version.split('.').map(|part| part.parse().ok()).collect()
}

/// True when `latest` is a newer release than `running`. A development build
/// or an unreadable tag is never told about an update.
pub fn is_newer(latest: &str, running: &str) -> bool {
    match (parse_version(latest), parse_version(running)) {
        (Some(latest), Some(running)) => latest > running,
        _ => false,
    }
}

/// The release's page on the forge.
pub fn release_url(version: &str) -> String {
    format!("https://code.squarecows.com/ric/omnimem/releases/tag/v{version}")
}

/// Remember what the check found.
pub fn record_latest_release(store: &Store, version: &str) -> Result<()> {
    let value = json!({"version": version, "checked_at": now_secs() as i64});
    store.string_set(LATEST_RELEASE_KEY, &value.to_string(), None)?;
    Ok(())
}

/// The newest release the last check found, if it is newer than `running`
/// and `UPDATE_CHECK` hasn't been switched off.
pub fn newer_release(store: &Store, running: &str) -> Option<String> {
    if !update_check_enabled() {
        return None;
    }
    newer_release_in(store, running)
}

/// [`newer_release`] without the setting, for the engine, which carries the
/// setting in its config.
pub(crate) fn newer_release_in(store: &Store, running: &str) -> Option<String> {
    let raw = store
        .string_get(LATEST_RELEASE_KEY)
        .map_err(|e| warn!(error = %e, "could not read the latest release"))
        .ok()??;
    let latest = serde_json::from_str::<Value>(&raw)
        .ok()?
        .get("version")?
        .as_str()?
        .to_owned();
    is_newer(&latest, running).then_some(latest)
}

/// The briefing's `update_available` object.
pub(crate) fn update_available(latest: &str, running: &str) -> Value {
    json!({"current": running, "latest": latest, "url": release_url(latest)})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        let store = Store::open_in_memory().unwrap();
        store.run_migrations().unwrap();
        store
    }

    #[test]
    fn versions_compare_numerically_and_dev_builds_opt_out() {
        assert!(is_newer("7.0.10", "7.0.9"));
        assert!(!is_newer("7.0.5", "7.0.5"));
        assert!(!is_newer("7.0.4", "7.0.5"));
        assert_eq!(parse_version("7.0.0-dev"), None);
        assert!(!is_newer("7.0.7", "7.0.6-dev"));
        assert!(!is_newer("nightly", "7.0.6"));
    }

    #[test]
    fn the_latest_release_round_trips_through_the_kv_table() {
        let store = store();
        assert_eq!(newer_release_in(&store, "7.0.6"), None);
        record_latest_release(&store, "7.0.7").unwrap();
        assert_eq!(newer_release_in(&store, "7.0.6").as_deref(), Some("7.0.7"));
        assert_eq!(newer_release_in(&store, "7.0.7"), None);
        let raw: Value =
            serde_json::from_str(&store.string_get(LATEST_RELEASE_KEY).unwrap().unwrap()).unwrap();
        assert_eq!(raw["version"], "7.0.7");
        assert!(raw["checked_at"].as_i64().unwrap() > 1_700_000_000);
    }

    #[test]
    fn a_garbled_value_reads_as_no_update() {
        let store = store();
        store
            .string_set(LATEST_RELEASE_KEY, "not json", None)
            .unwrap();
        assert_eq!(newer_release_in(&store, "7.0.6"), None);
    }

    #[test]
    fn the_update_object_names_the_release_page() {
        assert_eq!(
            update_available("7.0.7", "7.0.6"),
            json!({
                "current": "7.0.6",
                "latest": "7.0.7",
                "url": "https://code.squarecows.com/ric/omnimem/releases/tag/v7.0.7"
            })
        );
    }
}
