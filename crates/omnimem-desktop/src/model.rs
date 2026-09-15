//! The parts of the desktop app with no GUI in them: what the tray says,
//! which addresses belong to the panel, and what the window reports back.

use omnimem_app::ServiceState;
use serde_json::Value;

/// The scheme the settings panel is served on. Nothing is served over a
/// network: the webview asks the process for each page.
pub const SCHEME: &str = "omnimem";

/// Where the webview starts. Windows' WebView2 reaches custom schemes
/// through `http://<scheme>.localhost`; everywhere else the scheme is used
/// directly.
pub fn start_url() -> String {
    if cfg!(windows) {
        format!("http://{SCHEME}.localhost/")
    } else {
        format!("{SCHEME}://localhost/")
    }
}

/// True for the panel's own pages. Anything else (a footer link, a source
/// URL on an article) opens in the system browser instead.
pub fn is_panel_url(url: &str) -> bool {
    let origins = [
        format!("{SCHEME}://localhost"),
        format!("http://{SCHEME}.localhost"),
    ];
    url == "about:blank"
        || origins.iter().any(|origin| {
            url.strip_prefix(origin.as_str())
                .is_some_and(|rest| rest.is_empty() || rest.starts_with(['/', '?', '#']))
        })
}

/// The tray's status line.
pub fn status_line(state: &ServiceState) -> String {
    match state {
        ServiceState::Starting => "OmniMem: starting…".to_owned(),
        ServiceState::Running { memories, .. } => format!(
            "OmniMem: running, {memories} {}",
            if *memories == 1 { "memory" } else { "memories" }
        ),
        ServiceState::Failed(_) => "OmniMem: stopped with an error".to_owned(),
        ServiceState::Stopped => "OmniMem: stopped".to_owned(),
    }
}

/// The MCP URL, once there is one to copy.
pub fn mcp_url(state: &ServiceState) -> Option<&str> {
    match state {
        ServiceState::Running { mcp_url, .. } => Some(mcp_url),
        _ => None,
    }
}

/// The smoke test's check, run in the loaded panel: a fetch POST through the
/// scheme (what htmx does) and the stylesheet, then a real form submission
/// (what the lifecycle and edit forms do). The panel's redirect after the
/// form lands on a page that reports everything over IPC, so a redirect the
/// webview won't follow fails the test by never reporting. (A plain 303 is
/// one: WebKitGTK ignores redirects from a custom scheme.)
pub const SMOKE_SCRIPT: &str = r#"
Promise.all([
  fetch('/_panel/echo', { method: 'POST', body: 'panel-post-ok' }).then(r => r.text()),
  fetch('/static/style.css').then(r => r.ok ? r.text().then(t => t.length) : 0),
]).then(([echo, css]) => {
  const form = document.createElement('form');
  form.method = 'POST';
  form.action = '/_panel/redirect';
  for (const [name, value] of Object.entries({ echo, css, title: document.title })) {
    const input = document.createElement('input');
    input.type = 'hidden';
    input.name = name;
    input.value = value;
    form.appendChild(input);
  }
  document.body.appendChild(form);
  form.submit();
}).catch(e => window.ipc.postMessage(JSON.stringify({ cmd: 'smoke', echo: 'error: ' + e, css: 0, title: document.title })));
"#;

/// What the smoke script found, or why it failed.
pub fn smoke_verdict(body: &str) -> Result<String, String> {
    let report: Value =
        serde_json::from_str(body).map_err(|e| format!("unreadable report: {e}"))?;
    if report["cmd"] != "smoke" {
        return Err(format!("unexpected message: {body}"));
    }
    let echo = report["echo"].as_str().unwrap_or("");
    // A number from the fetch, or a string once it has been through the form.
    let css = report["css"]
        .as_u64()
        .or_else(|| report["css"].as_str().and_then(|c| c.parse().ok()))
        .unwrap_or(0);
    let title = report["title"].as_str().unwrap_or("");
    if echo != "panel-post-ok" {
        return Err(format!("POST through the scheme came back as {echo:?}"));
    }
    if report["landed"] != "/_panel/landed" {
        return Err("the form POST's redirect was not followed".to_owned());
    }
    if css < 1000 {
        return Err(format!("the stylesheet came back with {css} bytes"));
    }
    if !title.contains("OmniMem") {
        return Err(format!("the panel page's title was {title:?}"));
    }
    Ok(format!(
        "page {title:?}, POST echoed, stylesheet {css} bytes, form redirect followed"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tray_describes_each_state() {
        let running = ServiceState::Running {
            mcp_url: "http://127.0.0.1:8765/mcp".into(),
            memories: 1,
        };
        assert_eq!(status_line(&ServiceState::Starting), "OmniMem: starting…");
        assert_eq!(status_line(&running), "OmniMem: running, 1 memory");
        assert_eq!(mcp_url(&running), Some("http://127.0.0.1:8765/mcp"));
        assert_eq!(mcp_url(&ServiceState::Starting), None);
    }

    #[test]
    fn only_panel_addresses_stay_in_the_window() {
        assert!(is_panel_url("omnimem://localhost/memories"));
        assert!(is_panel_url("http://omnimem.localhost/skills"));
        assert!(!is_panel_url("https://omnimem.org"));
        assert!(
            !is_panel_url("http://omnimem.localhost.evil.example"),
            "a lookalike host"
        );
        assert!(!is_panel_url("omnimem://localhost.evil.example/"));
        assert!(!is_panel_url("about:srcdoc"));
    }

    #[test]
    fn the_smoke_report_is_checked() {
        let good = r#"{"echo":"panel-post-ok","css":"42265","title":"Starting — OmniMem","cmd":"smoke","landed":"/_panel/landed"}"#;
        assert!(smoke_verdict(good).is_ok());
        let bad = r#"{"cmd":"smoke","echo":"","css":42265,"title":"Starting — OmniMem"}"#;
        assert!(smoke_verdict(bad).unwrap_err().contains("POST"));
        let stuck =
            r#"{"cmd":"smoke","echo":"panel-post-ok","css":42265,"title":"Starting — OmniMem"}"#;
        assert!(smoke_verdict(stuck).unwrap_err().contains("redirect"));
    }
}
