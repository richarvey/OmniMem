//! The starting page, pages not ported yet, and the window's POST check.

use axum::extract::State;
use axum::http::{Method, StatusCode, Uri, header};
use axum::response::{Html, IntoResponse, Response};
use minijinja::context;

use crate::PanelState;
use crate::render::{failure, page};

/// Run store and engine work off the async workers. An error becomes the
/// error page.
pub(crate) async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> omnimem_engine::Result<T> + Send + 'static,
) -> Result<T, Response> {
    match tokio::task::spawn_blocking(work).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(e)) => Err(failure(&format!("{e:#}"))),
        Err(e) => Err(failure(&format!("the request's worker stopped: {e}"))),
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
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'~' | b'/' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

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

/// Echoes a POST body: the desktop smoke test uses it to prove request
/// bodies cross the window's custom scheme, which every htmx form relies on.
pub(crate) async fn echo(body: String) -> String {
    body
}

/// The header carrying where [`see_other`] sends the window.
pub const REDIRECT_HEADER: &str = "x-omnimem-redirect";

/// Send the window to a panel page after a form acts.
///
/// A plain 303 would be right on the web, but WebKitGTK doesn't follow
/// redirects from a custom scheme: the form's POST would complete and the
/// window would stay where it was. So the answer is a page that replaces
/// itself with the target, which also keeps the POST out of the history.
/// The target travels in [`REDIRECT_HEADER`] too, for tests.
pub(crate) fn see_other(location: &str) -> Response {
    let script_target = serde_json::to_string(location).unwrap_or_else(|_| "\"/\"".to_owned());
    let attribute_target = location
        .replace('&', "&amp;")
        .replace('"', "&#34;")
        .replace('<', "&lt;");
    let body = format!(
        "<!doctype html><meta http-equiv=\"refresh\" content=\"0;url={attribute_target}\">\
         <script>location.replace({script_target})</script>"
    );
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        [(REDIRECT_HEADER, location)],
        body,
    )
        .into_response()
}

/// The smoke test's form target: redirects to `/_panel/landed` carrying the
/// submitted fields, proving a form body arrived and the 303 was followed.
pub(crate) async fn redirect(body: String) -> Response {
    see_other(&format!("/_panel/landed?{body}"))
}

pub(crate) async fn landed() -> Html<&'static str> {
    Html(
        "<!doctype html><title>Landed</title><script>\
         const report = Object.fromEntries(new URLSearchParams(location.search));\
         window.ipc.postMessage(JSON.stringify({ ...report, cmd: 'smoke', landed: location.pathname }));\
         </script>",
    )
}
