//! The parts of the desktop app with no GUI in them: what the tray says,
//! which addresses belong to the panel, and what the window reports back.

use omnimem_app::ServiceState;
use serde_json::Value;

use crate::clients::{Client, Presence, host_of};

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

/// Where the tray icon's file goes inside a Flatpak: the one part of the
/// runtime folder the sandbox shares with the desktop. `None` outside one.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn flatpak_icon_dir(
    flatpak_id: Option<std::ffi::OsString>,
    runtime_dir: Option<std::ffi::OsString>,
) -> Option<std::path::PathBuf> {
    let (id, runtime) = (flatpak_id?, runtime_dir?);
    (!id.is_empty() && !runtime.is_empty())
        .then(|| std::path::PathBuf::from(runtime).join("app").join(id))
}

/// What starts the app at login, as a program and its arguments. Inside a
/// Flatpak the autostart entry is run from outside the sandbox, where
/// `/app/bin` doesn't exist, so it goes through `flatpak run`.
pub fn login_command(flatpak_id: Option<&str>, exe: &str) -> (String, Vec<String>) {
    match flatpak_id.filter(|id| !id.is_empty()) {
        Some(id) => (
            "flatpak".to_owned(),
            ["run", "--command=omnimem", id, "desktop"]
                .map(str::to_owned)
                .to_vec(),
        ),
        None => (exe.to_owned(), vec!["desktop".to_owned()]),
    }
}

/// The Claude Code item inside a Flatpak, which can neither see nor run a
/// `claude` installed outside the sandbox.
pub const CODE_IN_FLATPAK: &str = "Add to Claude Code (unavailable in the Flatpak)";

/// What the window says when nothing on the desktop shows tray icons.
pub const NO_TRAY: &str = "Nothing on this desktop shows tray icons, so OmniMem's \
    menu has nowhere to go. On GNOME, install the \"AppIndicator and \
    KStatusNotifierItem Support\" extension (gnome-shell-extension-appindicator) and \
    restart OmniMem. Until then this window is OmniMem: closing it leaves OmniMem \
    running, and opening OmniMem again brings it back.";

/// A client's menu item, its text and whether it can be clicked, from what
/// the client's settings hold (`None` until the services are running),
/// whether a change is under way, whether this run made one (so the client
/// needs a restart) and the last change's error.
pub fn client_item(
    client: Client,
    presence: Option<&Presence>,
    busy: bool,
    changed: bool,
    error: Option<&str>,
) -> (String, bool) {
    let name = client.name();
    if busy {
        return (format!("Adding OmniMem to {name}…"), false);
    }
    let (text, enabled) = match presence {
        None => (format!("Add to {name}"), false),
        Some(Presence::NotInstalled) => (format!("{name} not found"), false),
        Some(Presence::Unreadable(_)) => (format!("{name}: settings unreadable"), false),
        Some(Presence::Absent) => (format!("Add to {name}"), true),
        Some(Presence::Here) if changed => (format!("✓ In {name} (restart it to connect)"), false),
        Some(Presence::Here) => (format!("✓ In {name}"), false),
        Some(Presence::Elsewhere { target, .. }) => (
            format!("Switch {name} here (now {})", host_of(target)),
            true,
        ),
    };
    match error {
        Some(error) if enabled => (format!("{text} (failed: {})", clip(error, 60)), true),
        _ => (text, enabled),
    }
}

/// At most `max` characters of `text`, with an ellipsis if any were cut.
fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let cut: String = text.chars().take(max).collect();
    format!("{}…", cut.trim_end())
}

/// True for the smoke test's own pages (`/_panel/…`), where its check
/// mustn't start again.
pub fn is_smoke_page(url: &str) -> bool {
    url.contains("/_panel/")
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
    fn client_items_say_where_omnimem_stands() {
        let item = |presence: Option<Presence>| {
            client_item(Client::Desktop, presence.as_ref(), false, false, None)
        };
        assert_eq!(item(None), ("Add to Claude Desktop".into(), false));
        assert_eq!(
            item(Some(Presence::NotInstalled)),
            ("Claude Desktop not found".into(), false)
        );
        assert_eq!(
            item(Some(Presence::Absent)),
            ("Add to Claude Desktop".into(), true)
        );
        assert_eq!(
            item(Some(Presence::Here)),
            ("✓ In Claude Desktop".into(), false)
        );
        assert_eq!(
            item(Some(Presence::Elsewhere {
                name: "omnimem".into(),
                target: "https://cloudai.example.net/mcp".into()
            })),
            (
                "Switch Claude Desktop here (now cloudai.example.net)".into(),
                true
            )
        );
        assert_eq!(
            client_item(Client::Code, Some(&Presence::Here), false, true, None),
            ("✓ In Claude Code (restart it to connect)".into(), false)
        );
        assert_eq!(
            client_item(Client::Code, Some(&Presence::Absent), true, false, None),
            ("Adding OmniMem to Claude Code…".into(), false)
        );
        let long = "x".repeat(80);
        let (failed, enabled) = client_item(
            Client::Code,
            Some(&Presence::Absent),
            false,
            false,
            Some(&long),
        );
        assert!(enabled, "a failed add can be tried again");
        assert!(failed.starts_with("Add to Claude Code (failed: xxx") && failed.ends_with("…)"));
    }

    #[test]
    fn a_flatpak_puts_the_tray_icon_where_the_desktop_can_read_it() {
        let os = |s: &str| Some(std::ffi::OsString::from(s));
        assert_eq!(
            flatpak_icon_dir(os("com.squarecows.OmniMem"), os("/run/user/1000")),
            Some(std::path::PathBuf::from(
                "/run/user/1000/app/com.squarecows.OmniMem"
            ))
        );
        assert_eq!(flatpak_icon_dir(None, os("/run/user/1000")), None);
        assert_eq!(flatpak_icon_dir(os(""), os("/run/user/1000")), None);
        assert_eq!(flatpak_icon_dir(os("com.squarecows.OmniMem"), None), None);
    }

    #[test]
    fn a_flatpak_starts_at_login_through_flatpak_run() {
        assert_eq!(
            login_command(Some("com.squarecows.OmniMem"), "/app/bin/omnimem"),
            (
                "flatpak".to_owned(),
                [
                    "run",
                    "--command=omnimem",
                    "com.squarecows.OmniMem",
                    "desktop"
                ]
                .map(str::to_owned)
                .to_vec()
            )
        );
        for outside in [None, Some("")] {
            assert_eq!(
                login_command(outside, "/usr/bin/omnimem"),
                ("/usr/bin/omnimem".to_owned(), vec!["desktop".to_owned()])
            );
        }
    }

    #[test]
    fn the_smoke_check_restarts_on_every_page_but_its_own() {
        assert!(!is_smoke_page("omnimem://localhost/"));
        assert!(!is_smoke_page("http://omnimem.localhost/memories"));
        assert!(is_smoke_page("omnimem://localhost/_panel/redirect"));
        assert!(is_smoke_page(
            "omnimem://localhost/_panel/landed?echo=panel-post-ok"
        ));
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
