//! The parts of the desktop app with no GUI in them: what the tray says and
//! where the settings panel is.

use omnimem_app::ServiceState;

use crate::clients::{Client, Presence, host_of};

/// The server's address, which the settings panel sits under at `/admin`,
/// from the MCP URL the services report.
pub fn server_base(mcp_url: &str) -> &str {
    mcp_url.strip_suffix("/mcp").unwrap_or(mcp_url)
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

/// What the panel says when nothing on the desktop shows tray icons.
pub const NO_TRAY: &str = "Nothing on this desktop shows tray icons, so OmniMem's \
    menu has nowhere to go. On GNOME, install the \"AppIndicator and \
    KStatusNotifierItem Support\" extension (gnome-shell-extension-appindicator) and \
    restart OmniMem. Until then OmniMem keeps running without an icon, and opening \
    OmniMem again brings this panel back.";

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

/// The menu item that opens the release page.
pub fn update_item_text(latest: &str) -> String {
    format!("Update available: {latest}…")
}

/// The tray icon's tooltip, saying so when a newer release is out.
pub fn tooltip(update: Option<&str>) -> String {
    match update {
        Some(latest) => format!("OmniMem: {latest} is out"),
        None => "OmniMem".to_owned(),
    }
}

/// The MCP URL, once there is one to copy.
pub fn mcp_url(state: &ServiceState) -> Option<&str> {
    match state {
        ServiceState::Running { mcp_url, .. } => Some(mcp_url),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tray_announces_a_newer_release() {
        assert_eq!(update_item_text("7.0.7"), "Update available: 7.0.7…");
        assert_eq!(tooltip(Some("7.0.7")), "OmniMem: 7.0.7 is out");
        assert_eq!(tooltip(None), "OmniMem");
    }

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
    fn the_panel_sits_beside_the_mcp_endpoint() {
        assert_eq!(
            server_base("http://127.0.0.1:8765/mcp"),
            "http://127.0.0.1:8765"
        );
        assert_eq!(server_base("http://[::1]:9000/mcp"), "http://[::1]:9000");
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
}
