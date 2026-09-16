//! The starting page, pages not ported yet, and the window's POST check.

use std::fmt::Write as _;

use axum::extract::State;
use axum::http::{Method, StatusCode, Uri, header};
use axum::response::{Html, IntoResponse, Response};
use minijinja::context;

use crate::PanelState;
use crate::render::{failure, page};

/// Run file or store work that has no engine error of its own off the async
/// workers: the panel shares its runtime with the webview, so a directory
/// scan or a YAML parse on a worker would stall every other page. A worker
/// that stops becomes the error page.
pub(crate) async fn off_runtime<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, Response> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| failure(&format!("the request's worker stopped: {e}")))
}

/// Run store and engine work off the async workers. An error becomes the
/// error page.
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

/// True for an `http:` or `https:` URL. Anything else a feed or an article
/// names is shown as text rather than linked, and the desktop app only hands
/// these schemes to the system browser.
pub(crate) fn is_web_url(url: &str) -> bool {
    let Some((scheme, rest)) = url.split_once("://") else {
        return false;
    };
    let web = scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https");
    web && !rest.is_empty() && !url.chars().any(char::is_control)
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
///
/// The target can carry form data (`next`, a key, a name), so it is written
/// as a JavaScript string literal with every character that could end the
/// script or the attribute escaped: `<` alone would let `</script>` in a
/// target start writing HTML.
pub(crate) fn see_other(location: &str) -> Response {
    let script_target = js_string_literal(location);
    let attribute_target = location
        .replace('&', "&amp;")
        .replace('"', "&#34;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
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

/// A JavaScript string literal for `text`, safe inside an HTML `<script>`:
/// JSON escaping, then `<`, `>` and `&` as `\uXXXX` so no HTML tag or entity
/// can form, and U+2028 and U+2029, which JSON allows raw but which end a
/// JavaScript line.
fn js_string_literal(text: &str) -> String {
    let json = serde_json::to_string(text).unwrap_or_else(|_| "\"/\"".to_owned());
    let mut out = String::with_capacity(json.len());
    for c in json.chars() {
        match c {
            '<' => out.push_str("\\u003c"),
            '>' => out.push_str("\\u003e"),
            '&' => out.push_str("\\u0026"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c => out.push(c),
        }
    }
    out
}

/// The smoke test's form target: redirects to `/_panel/landed` carrying the
/// submitted fields, proving a form body arrived and the 303 was followed.
/// Routed only by [`crate::Panel::with_smoke_routes`].
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_redirect_target_cannot_break_out_of_the_script() {
        let response = see_other("/memory/</script><img src=x onerror=alert(1)>&x=\u{2028}");
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(!body.contains("</script><img"), "{body}");
        assert!(
            body.contains(
                r#"location.replace("/memory/\u003c/script\u003e\u003cimg src=x onerror=alert(1)\u003e\u0026x=\u2028")"#
            ),
            "{body}"
        );
        assert!(
            body.contains("url=/memory/&lt;/script&gt;&lt;img src=x onerror=alert(1)&gt;&amp;x="),
            "{body}"
        );
        assert_eq!(js_string_literal("plain/path?a=1"), r#""plain/path?a=1""#);
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
