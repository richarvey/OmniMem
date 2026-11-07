//! The starting page, pages not ported yet, and redirects.

use std::fmt::Write as _;

use axum::extract::State;
use axum::http::{Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use minijinja::context;

use crate::PanelState;
use crate::render::{failure, page};

/// Run file or store work that has no engine error of its own off the async
/// workers: the panel shares its runtime with the webview, so a directory
/// scan or a YAML parse on a worker would stall every other page. A worker
/// that stops becomes the error page.
// The error is the page to send, which every caller returns straight away,
// so boxing it would buy nothing. The same goes for `blocking`.
#[allow(clippy::result_large_err)]
pub(crate) async fn off_runtime<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, Response> {
    tokio::task::spawn_blocking(move || {
        // Every change the panel makes is the panel's, in the audit log.
        let _audit = omnimem_store::enter(omnimem_store::Actor::Panel, "");
        work()
    })
    .await
    .map_err(|e| failure(&format!("the request's worker stopped: {e}")))
}

/// Run store and engine work off the async workers. An error becomes the
/// error page.
#[allow(clippy::result_large_err)]
pub(crate) async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> omnimem_engine::Result<T> + Send + 'static,
) -> Result<T, Response> {
    match off_runtime(work).await? {
        Ok(value) => Ok(value),
        Err(e) => Err(failure(&format!("{e:#}"))),
    }
}

/// Seconds since the epoch, as `time.time()` reads.
pub(crate) fn now_seconds() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

/// Percent-encode for a query value, as `urllib.parse.quote` does.
pub(crate) fn quote(text: &str) -> String {
    percent_encode(text, |byte| byte == b'/')
}

/// Percent-encode one path segment: a memory key or project name that goes
/// between slashes. Unlike [`quote`] a `/` is encoded, so a value can't add
/// segments, while `:` stays as it is so memory keys read as they are.
pub(crate) fn quote_segment(text: &str) -> String {
    percent_encode(text, |byte| byte == b':')
}

fn percent_encode(text: &str, keep: impl Fn(u8) -> bool) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'~' => {
                out.push(byte as char);
            }
            b if keep(b) => out.push(b as char),
            // Writing into a String cannot fail.
            _ => {
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
    out
}

pub(crate) use omnimem_engine::feeds_file::is_web_url;

/// Shown in place of any page that needs the engine before the services
/// have opened it, or with the reason they stopped.
pub(crate) fn starting(state: &PanelState) -> Response {
    page(
        state.templates(),
        "starting.html",
        context! { current_page => "", error => state.failure() },
    )
}

pub(crate) async fn pending(State(state): State<PanelState>, method: Method, uri: Uri) -> Response {
    if method != Method::GET {
        return (StatusCode::NOT_FOUND, "not in the panel yet").into_response();
    }
    page(
        state.templates(),
        "pending.html",
        context! { current_page => "", path => uri.path() },
    )
}

/// The header carrying where [`see_other`] sends the browser, as a panel
/// path without [`crate::ADMIN_ROOT`], for tests.
pub const REDIRECT_HEADER: &str = "x-omnimem-redirect";

/// Send the browser to a panel page after a form acts: a 303, so the POST
/// stays out of the history. `location` is a panel path ("/memories"); the
/// `Location` header puts it under [`crate::ADMIN_ROOT`], where the panel is
/// served. Every caller builds `location` from a fixed path or a checked
/// `next`, so it can't name another site.
pub(crate) fn see_other(location: &str) -> Response {
    let target = format!("{}{location}", crate::ADMIN_ROOT);
    let mut response =
        (StatusCode::SEE_OTHER, [(header::CACHE_CONTROL, "no-store")]).into_response();
    let headers = response.headers_mut();
    for (name, value) in [
        (header::LOCATION, target.as_str()),
        (header::HeaderName::from_static(REDIRECT_HEADER), location),
    ] {
        match header::HeaderValue::from_str(value) {
            Ok(value) => {
                headers.insert(name, value);
            }
            // A control character can't reach a header; the dashboard can.
            Err(_) => {
                headers.insert(name, header::HeaderValue::from_static("/admin/"));
            }
        }
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_redirect_lands_under_the_admin_root() {
        let response = see_other("/memory/mem:episodic:01ABC?message=Saved");
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            response.headers()[header::LOCATION],
            "/admin/memory/mem:episodic:01ABC?message=Saved"
        );
        assert_eq!(
            response.headers()[REDIRECT_HEADER],
            "/memory/mem:episodic:01ABC?message=Saved"
        );
    }

    #[test]
    fn a_target_no_header_can_carry_goes_to_the_dashboard() {
        let response = see_other("/memory/a\r\nSet-Cookie: x=1");
        assert_eq!(response.headers()[header::LOCATION], "/admin/");
    }

    #[test]
    fn path_segments_are_encoded_but_keys_stay_readable() {
        assert_eq!(quote_segment("mem:episodic:01ABC"), "mem:episodic:01ABC");
        assert_eq!(
            quote_segment("a/b?c#d e</script>"),
            "a%2Fb%3Fc%23d%20e%3C%2Fscript%3E"
        );
        assert_eq!(quote("a/b c&d"), "a/b%20c%26d");
    }

    #[test]
    fn only_web_urls_are_linked() {
        assert!(is_web_url("https://example.com/feed.xml"));
        assert!(is_web_url("HTTP://example.com"));
        assert!(!is_web_url("javascript:alert(1)"));
        assert!(!is_web_url("file:///etc/passwd"));
        assert!(!is_web_url("https://"));
        assert!(!is_web_url("http://example.com/\n"));
        assert!(!is_web_url("example.com"));
    }
}

// ---------------------------------------------------------------- return paths

/// The longest return path a link or form may carry.
const MAX_BACK_LEN: usize = 512;

/// `path` when it is somewhere inside the panel a link or form may send the
/// browser back to, written as a panel path (`/memories?page=2`), else
/// `None`. A single leading `/` and nothing that could read as another
/// site: `//` is scheme-relative, and a browser reads `/\` the same way.
/// Control characters have no place in a path. A path that already starts
/// with the panel's root would be prefixed twice by [`see_other`], and the
/// sign-in, sign-out and lifecycle routes are actions, not places to return
/// to, so those are refused too.
pub(crate) fn local_path(path: &str) -> Option<String> {
    if path.len() > MAX_BACK_LEN {
        return None;
    }
    let mut chars = path.chars();
    if chars.next() != Some('/') {
        return None;
    }
    if matches!(chars.next(), Some('/' | '\\')) || path.chars().any(|c| c == '\\' || c.is_control())
    {
        return None;
    }
    let route = path.split(['?', '#']).next().unwrap_or(path);
    let under = |prefix: &str| route == prefix || route.starts_with(&format!("{prefix}/"));
    if under(crate::ADMIN_ROOT) || under("/lifecycle") || under("/login") || under("/logout") {
        return None;
    }
    Some(path.to_owned())
}

/// The `back` a link or form carried, when it is a usable return path.
pub(crate) fn back_of(values: &std::collections::HashMap<String, String>) -> Option<String> {
    values.get("back").and_then(|b| local_path(b.trim()))
}

/// A memory's page, remembering where the person came from.
pub(crate) fn memory_url(key: &str, back: Option<&str>) -> String {
    match back {
        Some(back) => format!("/memory/{}?back={}", quote_segment(key), quote(back)),
        None => format!("/memory/{}", quote_segment(key)),
    }
}

tokio::task_local! {
    /// The panel path (with its query) of the page being rendered, so every
    /// template can link onwards with `?back={{ here|urlquote }}`.
    static HERE: String;
}

/// Middleware: remember where this request is, for [`here`]. An htmx
/// request names the page it came from in `HX-Current-URL`, which is the
/// place a person would come back to, not the fragment's own route.
pub(crate) async fn remember_here(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let own = request.uri().path_and_query().map_or_else(
        || request.uri().path().to_owned(),
        |pq| pq.as_str().to_owned(),
    );
    let from_htmx = request
        .headers()
        .get("hx-current-url")
        .and_then(|v| v.to_str().ok())
        .and_then(|url| url.parse::<Uri>().ok())
        .and_then(|uri| {
            let pq = uri.path_and_query()?.as_str().to_owned();
            pq.strip_prefix(crate::ADMIN_ROOT).map(|rest| {
                if rest.is_empty() || rest.starts_with('?') {
                    format!("/{rest}")
                } else {
                    rest.to_owned()
                }
            })
        });
    let here = from_htmx
        .or(Some(own))
        .and_then(|p| local_path(&p))
        .unwrap_or_else(|| "/".to_owned());
    HERE.scope(here, next.run(request)).await
}

/// The page being rendered, as a panel path: `/` outside a request.
pub(crate) fn here() -> String {
    HERE.try_with(Clone::clone)
        .unwrap_or_else(|_| "/".to_owned())
}

#[cfg(test)]
mod return_path_tests {
    use super::*;

    #[test]
    fn only_panel_paths_are_return_paths() {
        assert_eq!(
            local_path("/memories?page=2").as_deref(),
            Some("/memories?page=2")
        );
        assert_eq!(local_path("/").as_deref(), Some("/"));
        assert!(local_path("//evil.example").is_none());
        assert!(local_path("/\\evil.example").is_none());
        assert!(local_path("https://evil.example").is_none());
        assert!(local_path("/memories\r\nX: y").is_none());
        assert!(local_path("").is_none());
        assert!(local_path("/admin/memories").is_none());
        assert!(local_path("/admin").is_none());
        assert!(local_path("/lifecycle/archive").is_none());
        assert!(local_path("/logout").is_none());
        assert!(local_path(&format!("/{}", "a".repeat(600))).is_none());
        assert_eq!(
            local_path("/administrators").as_deref(),
            Some("/administrators")
        );
    }

    #[test]
    fn memory_links_carry_their_return_path() {
        assert_eq!(memory_url("mem:episodic:1", None), "/memory/mem:episodic:1");
        assert_eq!(
            memory_url(
                "mem:episodic:1",
                Some("/duplicates?namespace=knowledge&scan=1")
            ),
            "/memory/mem:episodic:1?back=/duplicates%3Fnamespace%3Dknowledge%26scan%3D1"
        );
    }
}
