//! The parts of the desktop app with no GUI in them: what the tray says, what
//! the settings page is told, and what it may ask for.

use omnimem_app::ServiceState;
use serde_json::{Value, json};

/// The scheme the settings page is served on. Nothing is served over a
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

/// The tray's status line.
pub fn status_line(state: &ServiceState) -> String {
    match state {
        ServiceState::Starting => "OmniMem: starting…".to_owned(),
        ServiceState::Running { memories, .. } => {
            format!(
                "OmniMem: running, {memories} {}",
                if *memories == 1 { "memory" } else { "memories" }
            )
        }
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

/// What the settings page is sent.
pub fn status_json(state: &ServiceState) -> Value {
    let (name, url, memories, error) = match state {
        ServiceState::Starting => ("starting", None, None, None),
        ServiceState::Running { mcp_url, memories } => {
            ("running", Some(mcp_url.as_str()), Some(*memories), None)
        }
        ServiceState::Failed(message) => ("failed", None, None, Some(message.as_str())),
        ServiceState::Stopped => ("stopped", None, None, None),
    };
    json!({
        "state": name,
        "mcp_url": url,
        "memories": memories,
        "error": error,
        "version": env!("CARGO_PKG_VERSION"),
    })
}

/// A request from the settings page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PageCommand {
    /// The page has loaded and wants the current status.
    Ready,
    /// The page has applied a status it was sent.
    Ack,
    CopyMcpUrl,
}

pub fn parse_command(body: &str) -> Option<PageCommand> {
    let message: Value = serde_json::from_str(body).ok()?;
    match message.get("cmd")?.as_str()? {
        "ready" => Some(PageCommand::Ready),
        "ack" => Some(PageCommand::Ack),
        "copy_mcp_url" => Some(PageCommand::CopyMcpUrl),
        _ => None,
    }
}

/// The script that hands a status to the page.
pub fn deliver_status_script(state: &ServiceState) -> String {
    format!(
        "window.omnimem && window.omnimem.receive({});",
        status_json(state)
    )
}

/// (status, content type, body) for a request path under the scheme.
pub fn respond(path: &str) -> (u16, &'static str, &'static [u8]) {
    match path {
        "" | "/" | "/index.html" => (
            200,
            "text/html; charset=utf-8",
            crate::page::INDEX.as_bytes(),
        ),
        _ => (404, "text/plain; charset=utf-8", b"not found"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tray_and_page_describe_each_state() {
        let running = ServiceState::Running {
            mcp_url: "http://127.0.0.1:8765/mcp".into(),
            memories: 1,
        };
        assert_eq!(status_line(&ServiceState::Starting), "OmniMem: starting…");
        assert_eq!(status_line(&running), "OmniMem: running, 1 memory");
        assert_eq!(mcp_url(&running), Some("http://127.0.0.1:8765/mcp"));
        assert_eq!(mcp_url(&ServiceState::Starting), None);
        let json = status_json(&ServiceState::Failed(
            "could not bind 127.0.0.1:8765".into(),
        ));
        assert_eq!(json["state"], "failed");
        assert_eq!(json["error"], "could not bind 127.0.0.1:8765");
        assert!(deliver_status_script(&running).contains("\"memories\":1"));
    }

    #[test]
    fn only_known_commands_are_accepted() {
        assert_eq!(
            parse_command(r#"{"cmd": "ready"}"#),
            Some(PageCommand::Ready)
        );
        assert_eq!(
            parse_command(r#"{"cmd": "copy_mcp_url"}"#),
            Some(PageCommand::CopyMcpUrl)
        );
        assert_eq!(parse_command(r#"{"cmd": "rm -rf"}"#), None);
        assert_eq!(parse_command("not json"), None);
    }

    #[test]
    fn only_the_page_is_served() {
        assert_eq!(respond("/").0, 200);
        assert_eq!(respond("/../../etc/passwd").0, 404);
    }
}
