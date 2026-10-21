//! `/version-check`: a marker in the sidebar when a newer stable release is
//! out (`web_ui/routes/version_check.py`). `/releases/latest` skips drafts and
//! pre-releases, and the answer is cached for an hour. A development build,
//! whose version isn't plain numbers, never shows the marker.

use std::time::{Duration, Instant};

use axum::extract::State;
use axum::response::{Html, IntoResponse, Response};
use minijinja::context;
use tracing::debug;

use crate::PanelState;
use crate::render::page;

const RELEASES_API_URL: &str =
    "https://code.squarecows.com/api/v1/repos/ric/omnimem/releases/latest";
const CACHE_TTL: Duration = Duration::from_hours(1);

fn parse(version: &str) -> Option<Vec<u64>> {
    version.split('.').map(|part| part.parse().ok()).collect()
}

fn fetch_latest() -> Option<String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .user_agent(concat!("omnimem/", env!("CARGO_PKG_VERSION")))
        .build()
        .ok()?;
    let body: serde_json::Value = client
        .get(RELEASES_API_URL)
        .header("Accept", "application/json")
        .send()
        .ok()?
        .error_for_status()
        .ok()?
        .text()
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())?;
    body["tag_name"]
        .as_str()
        .map(|t| t.trim_start_matches('v').to_owned())
}

pub(crate) async fn check(State(state): State<PanelState>) -> Response {
    let running = env!("CARGO_PKG_VERSION");
    let Some(current) = parse(running) else {
        return Html("").into_response();
    };
    let cached = state
        .caches()
        .latest_release
        .clone()
        .filter(|(at, _)| at.elapsed() < CACHE_TTL);
    let latest = if let Some((_, latest)) = cached {
        latest
    } else {
        let latest = tokio::task::spawn_blocking(fetch_latest)
            .await
            .ok()
            .flatten();
        debug!(?latest, "checked for a newer release");
        state.caches().latest_release = Some((Instant::now(), latest.clone()));
        latest
    };
    match latest.as_deref().and_then(|l| parse(l).map(|v| (l, v))) {
        Some((latest, version)) if version > current => page(
            state.templates(),
            "partials/version_update.html",
            context! { latest_version => latest },
        ),
        _ => Html("").into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically_and_dev_builds_opt_out() {
        assert!(parse("7.0.10").unwrap() > parse("7.0.9").unwrap());
        assert_eq!(parse("7.0.0-dev"), None);
    }
}
