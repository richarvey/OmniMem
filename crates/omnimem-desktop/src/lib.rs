//! The desktop app: OmniMem's services behind a tray or menu bar icon.
//!
//! The platform event loop owns the main thread, as macOS requires and GTK
//! expects, and the services run on a background thread reporting their
//! state back into it. The settings panel is served by the services at
//! `/admin`, like a server's; the tray's Settings item opens it in the
//! browser with a sign-in link that works once, within a minute.

mod clients;
pub mod icon;
mod model;
mod smoke;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use auto_launch::{AutoLaunch, AutoLaunchBuilder};
use clients::{Client, Presence};
use omnimem_app::{
    Instance, ServiceState, acquire, feeds_path, request_show, run_services, take_show_request,
};
use omnimem_core::env::{install_overlay, parse_settings};
use omnimem_engine::Engine;
use omnimem_settings::{
    Panel, SecretStore, StaticOverhead, TRAY_CODE_LIFETIME, mint_sign_in_code, secret_settings,
    sign_in_url,
};
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy, EventLoopWindowTarget};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{TrayIcon, TrayIconBuilder};

pub struct DesktopOptions {
    /// The database the services open.
    pub db: PathBuf,
    /// Where the single-instance lock and show-window requests live.
    pub data_dir: PathBuf,
    /// Build the tray, serve the settings panel on a spare port, sign in to
    /// it over HTTP, and exit, without starting the services.
    pub smoke_test: bool,
}

#[derive(Debug)]
enum AppEvent {
    State(ServiceState),
    Menu(MenuEvent),
    /// What the smoke test's check of the panel found.
    Smoke(Result<String, String>),
    ServicesExited,
    /// A change to a client's settings finished.
    ClientDone(Client, Result<(), String>),
    /// The newest release, when it is newer than this one (or no longer is).
    Update(Option<String>),
}

#[cfg(not(target_os = "macos"))]
const ICON_SIZE: u32 = 64;
const POLL: Duration = Duration::from_millis(500);
const SMOKE_DEADLINE: Duration = Duration::from_secs(60);
const QUIT_DEADLINE: Duration = Duration::from_secs(15);
/// How often the clients' settings are read again, for edits made elsewhere.
const CLIENT_RECHECK: Duration = Duration::from_secs(30);
/// How often the store is asked whether a newer release has turned up.
const UPDATE_RECHECK: Duration = Duration::from_secs(30);

struct TrayMenu {
    menu: Menu,
    /// "Update available", at the top of the menu only while one is.
    update: MenuItem,
    status: MenuItem,
    settings: MenuItem,
    copy_url: MenuItem,
    desktop: MenuItem,
    code: MenuItem,
    opencode: MenuItem,
    opencode_hooks: MenuItem,
    vibe: MenuItem,
    vibe_hooks: MenuItem,
    codex: MenuItem,
    codex_hooks: MenuItem,
    kiro: MenuItem,
    kiro_hooks: MenuItem,
    start_at_login: CheckMenuItem,
    quit: MenuItem,
}

impl TrayMenu {
    fn new(login: Option<&AutoLaunch>) -> Result<Self> {
        let enabled = login.and_then(|l| l.is_enabled().ok()).unwrap_or(false);
        let this = Self {
            menu: Menu::new(),
            update: MenuItem::new(model::update_item_text(""), true, None),
            status: MenuItem::new(model::status_line(&ServiceState::Starting), false, None),
            settings: MenuItem::new("Settings…", true, None),
            copy_url: MenuItem::new("Copy MCP URL", false, None),
            desktop: client_menu_item(Client::Desktop),
            code: client_menu_item(Client::Code),
            opencode: client_menu_item(Client::OpenCode),
            opencode_hooks: client_menu_item(Client::OpenCodeHooks),
            vibe: client_menu_item(Client::Vibe),
            vibe_hooks: client_menu_item(Client::VibeHooks),
            codex: client_menu_item(Client::Codex),
            codex_hooks: client_menu_item(Client::CodexHooks),
            kiro: client_menu_item(Client::Kiro),
            kiro_hooks: client_menu_item(Client::KiroHooks),
            start_at_login: CheckMenuItem::new("Start at login", login.is_some(), enabled, None),
            quit: MenuItem::new("Quit OmniMem", true, None),
        };
        this.menu
            .append_items(&[
                &this.status,
                &PredefinedMenuItem::separator(),
                &this.settings,
                &this.copy_url,
                &this.desktop,
                &this.code,
                &this.opencode,
                &this.opencode_hooks,
                &this.vibe,
                &this.vibe_hooks,
                &this.codex,
                &this.codex_hooks,
                &this.kiro,
                &this.kiro_hooks,
                &this.start_at_login,
                &PredefinedMenuItem::separator(),
                &this.quit,
            ])
            .context("building the tray menu")?;
        Ok(this)
    }
}

/// A client's item as it starts, before the services have a URL.
fn client_menu_item(client: Client) -> MenuItem {
    let (text, enabled) = model::client_item(client, None, false, false, None);
    MenuItem::new(text, enabled, None)
}

/// Every client with an item in the menu, in the menu's order.
const CLIENTS: [Client; 10] = [
    Client::Desktop,
    Client::Code,
    Client::OpenCode,
    Client::OpenCodeHooks,
    Client::Vibe,
    Client::VibeHooks,
    Client::Codex,
    Client::CodexHooks,
    Client::Kiro,
    Client::KiroHooks,
];

/// Where OmniMem stands in one client, for its menu item.
#[derive(Default)]
struct ClientSlot {
    presence: Option<Presence>,
    busy: bool,
    changed: bool,
    error: Option<String>,
}

struct App {
    options: DesktopOptions,
    proxy: EventLoopProxy<AppEvent>,
    panel: Panel,
    /// The engine once the services have opened it, for minting sign-in codes.
    engine: Arc<Mutex<Option<Arc<Engine>>>>,
    /// Open the panel as soon as the services are running: Settings was
    /// chosen while they started, or nothing shows the tray icon.
    open_when_running: bool,
    menu: TrayMenu,
    tray: Option<TrayIcon>,
    state: ServiceState,
    shutdown: CancellationToken,
    services_running: bool,
    quit_requested: Option<Instant>,
    clipboard: Option<arboard::Clipboard>,
    login: Option<AutoLaunch>,
    desktop: ClientSlot,
    code: ClientSlot,
    opencode: ClientSlot,
    opencode_hooks: ClientSlot,
    /// OmniMem's OpenCode plugin as this app would write it: running this
    /// program on this database. `None` if this program can't be found.
    opencode_plugin: Option<String>,
    vibe: ClientSlot,
    vibe_hooks: ClientSlot,
    /// How Vibe's and Codex's hooks start this program on this database, as
    /// this app would write them. `None` if this program can't be found.
    hook_base: Option<String>,
    codex: ClientSlot,
    codex_hooks: ClientSlot,
    kiro: ClientSlot,
    kiro_hooks: ClientSlot,
    /// The client items in the menu now, in order.
    clients_shown: RefCell<Vec<Client>>,
    clients_checked: Option<Instant>,
    /// The newer release the tray is announcing.
    update: Option<String>,
    started: Instant,
    in_flatpak: bool,
}

/// Whether anything will show the tray icon. On Linux that takes a
/// StatusNotifierItem host on the session bus, which the panel or an
/// extension runs; the icon is created without one, just never seen.
#[cfg(target_os = "linux")]
fn tray_host_running() -> bool {
    use gtk::gio;
    use gtk::glib::ToVariant;
    let Ok(bus) = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) else {
        return false;
    };
    bus.call_sync(
        Some("org.freedesktop.DBus"),
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        "NameHasOwner",
        Some(&("org.kde.StatusNotifierWatcher",).to_variant()),
        None,
        gio::DBusCallFlags::NONE,
        2000,
        gio::Cancellable::NONE,
    )
    .ok()
    .and_then(|reply| reply.get::<(bool,)>())
    .is_some_and(|(owned,)| owned)
}

/// macOS and Windows always have somewhere to put the icon.
#[cfg(not(target_os = "linux"))]
fn tray_host_running() -> bool {
    true
}

fn start_at_login() -> Option<AutoLaunch> {
    let exe = std::env::current_exe().ok()?;
    let flatpak_id = std::env::var("FLATPAK_ID").ok();
    let (program, args) = model::login_command(flatpak_id.as_deref(), exe.to_str()?);
    AutoLaunchBuilder::new()
        .set_app_name("OmniMem")
        .set_app_path(&program)
        .set_args(&args)
        .build()
        .map_err(|e| warn!(error = %e, "start at login is unavailable"))
        .ok()
}

/// Open a web link in the system browser. Only web links (and mail links)
/// are handed to the OS, so nothing can launch whatever handles another
/// scheme.
fn open_externally(url: String) {
    if !is_openable(&url) {
        warn!(
            url,
            "refusing to open a link that isn't http, https or mailto"
        );
        return;
    }
    std::thread::spawn(move || {
        if let Err(e) = open::that(&url) {
            warn!(url, error = %e, "could not open the link");
        }
    });
}

/// Read the background check's answer from the store every half minute and
/// tell the event loop when it changes. A thread of its own, so the loop never
/// waits on SQLite; it ends with the services or when the loop is gone.
/// `UPDATE_CHECK=false` makes the answer always "none".
fn watch_for_updates(
    engine: Arc<omnimem_engine::Engine>,
    proxy: EventLoopProxy<AppEvent>,
    stop: CancellationToken,
) {
    let spawned = std::thread::Builder::new()
        .name("update-watch".into())
        .spawn(move || {
            let mut told: Option<String> = None;
            while !stop.is_cancelled() {
                let latest = omnimem_engine::release::newer_release(
                    engine.store(),
                    env!("CARGO_PKG_VERSION"),
                );
                if latest != told {
                    if proxy.send_event(AppEvent::Update(latest.clone())).is_err() {
                        return;
                    }
                    told = latest;
                }
                std::thread::sleep(UPDATE_RECHECK);
            }
        });
    if let Err(e) = spawned {
        warn!(error = %e, "could not watch for a newer release");
    }
}

fn is_openable(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    ["http://", "https://", "mailto:"]
        .iter()
        .any(|scheme| lower.starts_with(scheme))
        && !url.chars().any(char::is_control)
}

impl App {
    fn create_tray(&mut self) -> Result<()> {
        // The macOS menu bar gets the wide `<o>`, 18 points tall at 2x;
        // trays elsewhere fit icons into a square, so they get the compact one.
        #[cfg(target_os = "macos")]
        let icon = {
            let height = 36;
            let (pixels, width) = icon::menu_bar(height);
            tray_icon::Icon::from_rgba(pixels, width, height)
        };
        #[cfg(not(target_os = "macos"))]
        let icon = tray_icon::Icon::from_rgba(icon::tray(ICON_SIZE), ICON_SIZE, ICON_SIZE);
        let icon = icon.map_err(|e| anyhow!("tray icon: {e}"))?;
        #[allow(unused_mut)]
        let mut builder = TrayIconBuilder::new()
            .with_menu(Box::new(self.menu.menu.clone()))
            .with_tooltip(model::tooltip(self.update.as_deref()))
            .with_icon(icon);
        // On Linux the icon goes to the tray as a file. Inside a Flatpak the
        // tray, outside the sandbox, can only read $XDG_RUNTIME_DIR/app/<id>.
        #[cfg(target_os = "linux")]
        if let Some(dir) = model::flatpak_icon_dir(
            std::env::var_os("FLATPAK_ID"),
            std::env::var_os("XDG_RUNTIME_DIR"),
        ) {
            builder = builder.with_temp_dir_path(dir);
        }
        let tray = builder.build().context("creating the tray icon")?;
        self.tray = Some(tray);
        Ok(())
    }

    fn start_services(&mut self) {
        let (db, token, proxy, panel, slot) = (
            self.options.db.clone(),
            self.shutdown.clone(),
            self.proxy.clone(),
            self.panel.clone(),
            self.engine.clone(),
        );
        let spawned = std::thread::Builder::new()
            .name("omnimem-services".into())
            .spawn(move || {
                let report = |state: ServiceState| {
                    if let ServiceState::Failed(message) = &state {
                        panel.set_failure(message.clone());
                    }
                    let _ = proxy.send_event(AppEvent::State(state));
                };
                let ready = |engine: &Arc<Engine>| {
                    if let Ok(mut slot) = slot.lock() {
                        *slot = Some(engine.clone());
                    }
                    watch_for_updates(engine.clone(), proxy.clone(), token.clone());
                };
                if let Err(e) = run_services(
                    &db,
                    token.clone(),
                    false,
                    Some(panel.clone()),
                    &report,
                    &ready,
                ) {
                    error!(error = %format!("{e:#}"), "services stopped");
                }
                let _ = proxy.send_event(AppEvent::ServicesExited);
            });
        match spawned {
            Ok(_) => self.services_running = true,
            Err(e) => self.set_state(ServiceState::Failed(format!(
                "could not start the services: {e}"
            ))),
        }
    }

    /// Open the settings panel in the browser, signed in by a code that
    /// works once, within a minute. Before the services are running there is
    /// nothing to open yet, so it opens when they are.
    fn open_settings(&mut self) {
        let Some(base) = model::mcp_url(&self.state).map(|url| model::server_base(url).to_owned())
        else {
            self.open_when_running = true;
            return;
        };
        self.open_when_running = false;
        let engine = self.engine.lock().ok().and_then(|e| e.clone());
        let Some(engine) = engine else {
            self.open_when_running = true;
            return;
        };
        match mint_sign_in_code(engine.store(), TRAY_CODE_LIFETIME) {
            Ok(code) => open_externally(sign_in_url(&base, &code)),
            Err(e) => error!(error = e, "could not open the settings panel"),
        }
    }

    /// Show or hide the update item and say so in the tooltip.
    fn set_update(&mut self, latest: Option<String>) {
        if latest == self.update {
            return;
        }
        if let Some(latest) = &latest {
            self.menu.update.set_text(model::update_item_text(latest));
            if self.update.is_none() {
                // The top of the menu, above the status line.
                if let Err(e) = self.menu.menu.insert(&self.menu.update, 0) {
                    warn!(error = %e, "could not add the update item to the menu");
                }
            }
            info!(latest, "a newer release is out");
        } else {
            let _ = self.menu.menu.remove(&self.menu.update);
        }
        if let Some(tray) = &self.tray
            && let Err(e) = tray.set_tooltip(Some(model::tooltip(latest.as_deref())))
        {
            warn!(error = %e, "could not update the tray tooltip");
        }
        self.update = latest;
    }

    fn set_state(&mut self, state: ServiceState) {
        info!(?state, "services state");
        self.menu.status.set_text(model::status_line(&state));
        self.menu
            .copy_url
            .set_enabled(model::mcp_url(&state).is_some());
        let url_changed = model::mcp_url(&state) != model::mcp_url(&self.state);
        self.state = state;
        if url_changed {
            self.check_clients();
        }
        if self.open_when_running && model::mcp_url(&self.state).is_some() {
            self.open_settings();
        }
    }

    /// Read every client's settings again, against the current MCP URL, and
    /// relabel their menu items. Without a URL there's nothing to add.
    fn check_clients(&mut self) {
        self.clients_checked = Some(Instant::now());
        let url = model::mcp_url(&self.state).map(str::to_owned);
        // What an entry has to send to get in, as the services check it.
        let token = omnimem_core::env::var("MCP_AUTH_TOKEN").filter(|t| !t.is_empty());
        let oauth = omnimem_core::env::var("OAUTH_ENABLED")
            .is_some_and(|v| matches!(v.to_ascii_lowercase().as_str(), "true" | "1" | "yes"));
        let server = url.as_deref().map(|url| clients::Server {
            url,
            token: token.as_deref(),
            oauth,
        });
        let url = server.as_ref();
        self.desktop.presence = url
            .map(|url| clients::desktop_presence(clients::desktop_config_path().as_deref(), url));
        self.code.presence = url.map(|url| {
            clients::code_presence(
                clients::find_claude().as_deref(),
                clients::code_config_path().as_deref(),
                url,
            )
        });
        let opencode_here = url.is_some() && omnimem_app::opencode::installed();
        self.opencode.presence = url.map(|url| {
            clients::opencode_presence(opencode_here, &clients::opencode_config_paths(), url)
        });
        self.opencode_hooks.presence = url.as_ref().map(|_| match &self.opencode_plugin {
            Some(source) => clients::opencode_hooks_presence(
                opencode_here,
                omnimem_app::opencode::plugin_path(None).as_deref(),
                source,
            ),
            None => Presence::Unreadable("this program wasn't found".to_owned()),
        });
        let vibe_here = url.is_some() && omnimem_app::vibe::installed();
        self.vibe.presence = url.map(|url| {
            clients::vibe_presence(vibe_here, omnimem_app::vibe::config_path().as_deref(), url)
        });
        self.vibe_hooks.presence = url.as_ref().map(|_| match &self.hook_base {
            Some(base) => clients::vibe_hooks_presence(
                vibe_here,
                omnimem_app::vibe::hooks_path(None).as_deref(),
                base,
            ),
            None => Presence::Unreadable("this program wasn't found".to_owned()),
        });
        let codex_here = url.is_some() && omnimem_app::codex::installed();
        self.codex.presence = url.map(|url| {
            clients::codex_presence(
                codex_here,
                omnimem_app::codex::config_path().as_deref(),
                url,
            )
        });
        self.codex_hooks.presence = url.as_ref().map(|_| match &self.hook_base {
            Some(base) => clients::codex_hooks_presence(
                codex_here,
                omnimem_app::codex::hooks_path(None).as_deref(),
                base,
            ),
            None => Presence::Unreadable("this program wasn't found".to_owned()),
        });
        let kiro_here = url.is_some() && omnimem_app::kiro::installed();
        self.kiro.presence = url.map(|url| {
            clients::kiro_presence(kiro_here, omnimem_app::kiro::config_path().as_deref(), url)
        });
        self.kiro_hooks.presence = url.as_ref().map(|_| match &self.hook_base {
            Some(base) => clients::kiro_hooks_presence(
                kiro_here,
                omnimem_app::kiro::hooks_path(None).as_deref(),
                base,
            ),
            None => Presence::Unreadable("this program wasn't found".to_owned()),
        });
        for client in CLIENTS {
            self.relabel(client);
        }
    }

    fn slot(&mut self, client: Client) -> &mut ClientSlot {
        match client {
            Client::Desktop => &mut self.desktop,
            Client::Code => &mut self.code,
            Client::OpenCode => &mut self.opencode,
            Client::OpenCodeHooks => &mut self.opencode_hooks,
            Client::Vibe => &mut self.vibe,
            Client::VibeHooks => &mut self.vibe_hooks,
            Client::Codex => &mut self.codex,
            Client::CodexHooks => &mut self.codex_hooks,
            Client::Kiro => &mut self.kiro,
            Client::KiroHooks => &mut self.kiro_hooks,
        }
    }

    fn slot_and_item(&self, client: Client) -> (&ClientSlot, &MenuItem) {
        match client {
            Client::Desktop => (&self.desktop, &self.menu.desktop),
            Client::Code => (&self.code, &self.menu.code),
            Client::OpenCode => (&self.opencode, &self.menu.opencode),
            Client::OpenCodeHooks => (&self.opencode_hooks, &self.menu.opencode_hooks),
            Client::Vibe => (&self.vibe, &self.menu.vibe),
            Client::VibeHooks => (&self.vibe_hooks, &self.menu.vibe_hooks),
            Client::Codex => (&self.codex, &self.menu.codex),
            Client::CodexHooks => (&self.codex_hooks, &self.menu.codex_hooks),
            Client::Kiro => (&self.kiro, &self.menu.kiro),
            Client::KiroHooks => (&self.kiro_hooks, &self.menu.kiro_hooks),
        }
    }

    /// Whether a client's item belongs in the menu: not once the client
    /// already points here. In the Flatpak every item stays, to say why it
    /// can't be used.
    fn client_shown(&self, client: Client) -> bool {
        let (slot, _) = self.slot_and_item(client);
        (self.in_flatpak && client != Client::Desktop)
            || model::client_item_shown(slot.presence.as_ref(), slot.busy, slot.changed)
    }

    /// Put the client items the menu should have under Copy MCP URL, in
    /// their usual order, and take out the rest. Menus have no hidden items,
    /// so they're removed and inserted, and only when the set changes.
    fn place_client_items(&self) {
        let want: Vec<Client> = CLIENTS
            .into_iter()
            .filter(|&client| self.client_shown(client))
            .collect();
        if *self.clients_shown.borrow() == want {
            return;
        }
        for client in CLIENTS {
            // An item that isn't there is an error, and nothing to undo.
            let _ = self.menu.menu.remove(self.slot_and_item(client).1);
        }
        let copy_url = self.menu.copy_url.id();
        let Some(mut at) = self
            .menu
            .menu
            .items()
            .iter()
            .position(|item| item.id() == copy_url)
        else {
            warn!("Copy MCP URL is missing from the tray menu");
            return;
        };
        for &client in &want {
            at += 1;
            if let Err(e) = self.menu.menu.insert(self.slot_and_item(client).1, at) {
                warn!(client = client.name(), error = %e, "could not put a client back in the menu");
            }
        }
        *self.clients_shown.borrow_mut() = want;
    }

    fn relabel(&self, client: Client) {
        let (slot, item) = self.slot_and_item(client);
        let flatpak_text = match client {
            Client::Desktop => None,
            Client::Code => Some(model::CODE_IN_FLATPAK),
            Client::OpenCode => Some(model::OPENCODE_IN_FLATPAK),
            Client::OpenCodeHooks => Some(model::OPENCODE_HOOKS_IN_FLATPAK),
            Client::Vibe => Some(model::VIBE_IN_FLATPAK),
            Client::VibeHooks => Some(model::VIBE_HOOKS_IN_FLATPAK),
            Client::Codex => Some(model::CODEX_IN_FLATPAK),
            Client::CodexHooks => Some(model::CODEX_HOOKS_IN_FLATPAK),
            Client::Kiro => Some(model::KIRO_IN_FLATPAK),
            Client::KiroHooks => Some(model::KIRO_HOOKS_IN_FLATPAK),
        };
        let (text, enabled) = if let Some(text) = flatpak_text.filter(|_| self.in_flatpak) {
            (text.to_owned(), false)
        } else {
            model::client_item(
                client,
                slot.presence.as_ref(),
                slot.busy,
                slot.changed,
                slot.error.as_deref(),
            )
        };
        item.set_text(text);
        item.set_enabled(enabled);
        self.place_client_items();
    }

    /// Add OmniMem to a client, or switch its `omnimem` entry here, on a
    /// thread of its own: Claude Code's command takes a second or two.
    fn change_client(&mut self, client: Client) {
        let Some(url) = model::mcp_url(&self.state).map(str::to_owned) else {
            return;
        };
        let slot = self.slot(client);
        let replacing = match (&slot.presence, slot.busy) {
            (Some(Presence::Absent), false) => None,
            (Some(Presence::Elsewhere { name, .. } | Presence::OldToken { name }), false) => {
                Some(name.clone())
            }
            _ => return,
        };
        slot.busy = true;
        slot.error = None;
        self.relabel(client);
        // The token the services check, if one is set, so the client can get in.
        let token = omnimem_core::env::var("MCP_AUTH_TOKEN").filter(|t| !t.is_empty());
        let plugin = self.opencode_plugin.clone();
        let hook_base = self.hook_base.clone();
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            let result = match client {
                Client::Desktop => clients::desktop_config_path()
                    .ok_or_else(|| "Claude Desktop doesn't run here".to_owned())
                    .and_then(|path| clients::write_desktop(&path, &url, token.as_deref())),
                Client::Code => clients::find_claude()
                    .ok_or_else(|| "the claude command wasn't found".to_owned())
                    .and_then(|cli| {
                        clients::write_code(&cli, replacing.as_deref(), &url, token.as_deref())
                    }),
                Client::OpenCode => {
                    clients::opencode_config_target(&clients::opencode_config_paths())
                        .ok_or_else(|| "no home folder to find OpenCode's config in".to_owned())
                        .and_then(|path| clients::write_opencode(&path, &url, token.as_deref()))
                }
                Client::OpenCodeHooks => plugin
                    .ok_or_else(|| "this program wasn't found".to_owned())
                    .and_then(|source| {
                        let path = omnimem_app::opencode::plugin_path(None)
                            .ok_or("no home folder to find OpenCode's config in")?;
                        omnimem_app::opencode::write_plugin(&path, &source)
                            .map(|_| ())
                            .map_err(|e| format!("{e:#}"))
                    }),
                Client::Vibe => omnimem_app::vibe::config_path()
                    .ok_or_else(|| "no home folder to find Vibe's in".to_owned())
                    .and_then(|path| clients::write_vibe(&path, &url, token.as_deref())),
                Client::VibeHooks => hook_base
                    .ok_or_else(|| "this program wasn't found".to_owned())
                    .and_then(|base| {
                        let path = omnimem_app::vibe::hooks_path(None)
                            .ok_or("no home folder to find Vibe's in")?;
                        omnimem_app::vibe::write_hooks(&path, &base)
                            .map(|_| ())
                            .map_err(|e| format!("{e:#}"))
                    }),
                Client::Codex => omnimem_app::codex::config_path()
                    .ok_or_else(|| "no home folder to find Codex's in".to_owned())
                    .and_then(|path| clients::write_codex(&path, &url, token.as_deref())),
                Client::CodexHooks => hook_base
                    .ok_or_else(|| "this program wasn't found".to_owned())
                    .and_then(|base| {
                        let path = omnimem_app::codex::hooks_path(None)
                            .ok_or("no home folder to find Codex's in")?;
                        omnimem_app::codex::write_hooks(&path, &base)
                            .map(|_| ())
                            .map_err(|e| format!("{e:#}"))
                    }),
                Client::Kiro => omnimem_app::kiro::config_path()
                    .ok_or_else(|| "no home folder to find Kiro's in".to_owned())
                    .and_then(|path| clients::write_kiro(&path, &url, token.as_deref())),
                Client::KiroHooks => hook_base
                    .ok_or_else(|| "this program wasn't found".to_owned())
                    .and_then(|base| {
                        let path = omnimem_app::kiro::hooks_path(None)
                            .ok_or("no home folder to find Kiro's in")?;
                        omnimem_app::kiro::write_hooks(&path, &base)
                            .map(|_| ())
                            .map_err(|e| format!("{e:#}"))
                    }),
            };
            let _ = proxy.send_event(AppEvent::ClientDone(client, result));
        });
    }

    fn client_done(&mut self, client: Client, result: Result<(), String>) {
        let slot = self.slot(client);
        slot.busy = false;
        match result {
            Ok(()) => {
                info!(client = client.name(), "OmniMem added");
                slot.changed = true;
            }
            Err(e) => {
                warn!(client = client.name(), error = %e, "could not add OmniMem");
                slot.error = Some(e);
            }
        }
        self.check_clients();
    }

    fn copy_mcp_url(&mut self) {
        let Some(url) = model::mcp_url(&self.state).map(str::to_owned) else {
            return;
        };
        if self.clipboard.is_none() {
            // Kept for the app's lifetime: on X11 the text goes when it drops.
            self.clipboard = arboard::Clipboard::new()
                .map_err(|e| warn!(error = %e, "no clipboard"))
                .ok();
        }
        if let Some(clipboard) = &mut self.clipboard
            && let Err(e) = clipboard.set_text(url)
        {
            warn!(error = %e, "could not copy the MCP URL");
        }
    }

    fn toggle_start_at_login(&mut self) {
        let Some(login) = &self.login else { return };
        let wanted = self.menu.start_at_login.is_checked();
        let result = if wanted {
            login.enable()
        } else {
            login.disable()
        };
        if let Err(e) = result {
            warn!(error = %e, "could not change start at login");
            self.menu.start_at_login.set_checked(!wanted);
        }
    }

    fn quit(&mut self, control_flow: &mut ControlFlow) {
        if !self.services_running {
            *control_flow = ControlFlow::Exit;
            return;
        }
        if self.quit_requested.is_none() {
            info!("quitting: stopping the services");
            self.menu.status.set_text("OmniMem: stopping…");
            self.menu.quit.set_enabled(false);
            self.shutdown.cancel();
            self.quit_requested = Some(Instant::now());
        }
    }

    fn handle(
        &mut self,
        event: Event<'_, AppEvent>,
        _target: &EventLoopWindowTarget<AppEvent>,
        control_flow: &mut ControlFlow,
    ) {
        *control_flow = ControlFlow::WaitUntil(Instant::now() + POLL);
        match event {
            Event::NewEvents(StartCause::Init) => {
                let created = self.create_tray();
                if let Err(e) = &created {
                    if self.options.smoke_test {
                        error!(error = %format!("{e:#}"), "smoke test: no tray icon");
                        *control_flow = ControlFlow::ExitWithCode(1);
                        return;
                    }
                    warn!(error = %format!("{e:#}"), "no tray icon, opening the settings panel instead");
                }
                // On Linux the icon is "created" even when nothing will show
                // it: stock GNOME has no tray without the AppIndicator
                // extension. Then the panel is the app's only face, and it
                // says what to install.
                let tray_shows = created.is_ok() && tray_host_running();
                if !self.options.smoke_test && !tray_shows {
                    if created.is_ok() {
                        warn!("nothing here shows tray icons, opening the settings panel instead");
                    }
                    self.panel.set_notice(model::NO_TRAY.to_owned());
                    self.open_when_running = true;
                }
                if self.options.smoke_test {
                    let proxy = self.proxy.clone();
                    std::thread::spawn(move || {
                        let result = tokio::runtime::Builder::new_multi_thread()
                            .worker_threads(1)
                            .enable_all()
                            .build()
                            .map_err(|e| format!("starting a runtime: {e}"))
                            .and_then(|runtime| smoke::check(&runtime));
                        let _ = proxy.send_event(AppEvent::Smoke(result));
                    });
                } else {
                    self.start_services();
                }
            }
            Event::NewEvents(_) => {
                if !self.options.smoke_test && take_show_request(&self.options.data_dir) {
                    self.open_settings();
                }
                if self.options.smoke_test && self.started.elapsed() > SMOKE_DEADLINE {
                    error!("smoke test: the panel never reported back");
                    *control_flow = ControlFlow::ExitWithCode(1);
                }
                if self
                    .quit_requested
                    .is_some_and(|at| at.elapsed() > QUIT_DEADLINE)
                {
                    warn!("the services did not stop in time, exiting anyway");
                    *control_flow = ControlFlow::Exit;
                }
                // Picks up an entry added or removed by hand, or by the client.
                if self
                    .clients_checked
                    .is_some_and(|at| at.elapsed() >= CLIENT_RECHECK)
                {
                    self.check_clients();
                }
            }
            Event::UserEvent(AppEvent::State(state)) => self.set_state(state),
            Event::UserEvent(AppEvent::Update(latest)) => self.set_update(latest),
            Event::UserEvent(AppEvent::ClientDone(client, result)) => {
                self.client_done(client, result);
            }
            Event::UserEvent(AppEvent::ServicesExited) => {
                self.services_running = false;
                if self.quit_requested.is_some() {
                    *control_flow = ControlFlow::Exit;
                }
            }
            Event::UserEvent(AppEvent::Menu(menu_event)) => {
                let id = &menu_event.id;
                if id == self.menu.settings.id() {
                    self.open_settings();
                } else if id == self.menu.update.id() {
                    if let Some(latest) = &self.update {
                        open_externally(omnimem_engine::release::release_url(latest));
                    }
                } else if id == self.menu.copy_url.id() {
                    self.copy_mcp_url();
                } else if id == self.menu.desktop.id() {
                    self.change_client(Client::Desktop);
                } else if id == self.menu.code.id() {
                    self.change_client(Client::Code);
                } else if id == self.menu.opencode.id() {
                    self.change_client(Client::OpenCode);
                } else if id == self.menu.opencode_hooks.id() {
                    self.change_client(Client::OpenCodeHooks);
                } else if id == self.menu.vibe.id() {
                    self.change_client(Client::Vibe);
                } else if id == self.menu.vibe_hooks.id() {
                    self.change_client(Client::VibeHooks);
                } else if id == self.menu.codex.id() {
                    self.change_client(Client::Codex);
                } else if id == self.menu.codex_hooks.id() {
                    self.change_client(Client::CodexHooks);
                } else if id == self.menu.kiro.id() {
                    self.change_client(Client::Kiro);
                } else if id == self.menu.kiro_hooks.id() {
                    self.change_client(Client::KiroHooks);
                } else if id == self.menu.start_at_login.id() {
                    self.toggle_start_at_login();
                } else if id == self.menu.quit.id() {
                    self.quit(control_flow);
                }
            }
            Event::UserEvent(AppEvent::Smoke(result)) => match result {
                Ok(found) => {
                    println!("smoke test passed: tray icon and settings panel over HTTP ({found})");
                    *control_flow = ControlFlow::ExitWithCode(0);
                }
                Err(problem) => {
                    error!(problem, "smoke test failed");
                    *control_flow = ControlFlow::ExitWithCode(1);
                }
            },
            _ => {}
        }
    }
}

/// The settings the configuration page writes, beside the database.
const SETTINGS_FILE: &str = "omnimem.env";
/// The keychain service every OmniMem secret is filed under.
const KEYCHAIN_SERVICE: &str = "OmniMem";

/// Secrets in the OS keychain: Keychain Services on macOS, Credential
/// Manager on Windows, the Secret Service elsewhere.
struct Keychain;

fn keychain_entry(name: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(KEYCHAIN_SERVICE, name).map_err(|e| e.to_string())
}

impl SecretStore for Keychain {
    fn get(&self, name: &str) -> Result<Option<String>, String> {
        match keychain_entry(name)?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    fn set(&self, name: &str, value: &str) -> Result<(), String> {
        keychain_entry(name)?
            .set_password(value)
            .map_err(|e| e.to_string())
    }

    fn delete(&self, name: &str) -> Result<(), String> {
        match keychain_entry(name)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
}

/// Give the services the desktop app's settings: `omnimem.env` from the data
/// folder, then the secrets in the keychain. Runs once, before anything reads
/// a setting; an environment variable of the same name still wins.
fn install_settings(data_dir: &Path, keychain: Option<&Keychain>) {
    let path = data_dir.join(SETTINGS_FILE);
    let mut values = match std::fs::read_to_string(&path) {
        Ok(text) => parse_settings(&text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
        Err(e) => {
            warn!(path = %path.display(), error = %e, "could not read the settings file");
            BTreeMap::new()
        }
    };
    if let Some(keychain) = keychain {
        for name in secret_settings() {
            if std::env::var_os(name).is_some() {
                continue;
            }
            match keychain.get(name) {
                Ok(Some(value)) => {
                    values.insert(name.to_owned(), value);
                }
                Ok(None) => {}
                Err(problem) => warn!(setting = name, problem, "could not read the keychain"),
            }
        }
    }
    info!(settings = values.len(), "loaded the desktop app's settings");
    install_overlay(values);
}

/// Run the desktop app until Quit. A second launch asks the first to open
/// the settings panel and returns.
pub fn run(options: DesktopOptions) -> Result<()> {
    let lock = if options.smoke_test {
        None
    } else {
        match acquire(&options.data_dir).context("checking for a running OmniMem")? {
            Instance::Primary(lock) => Some(lock),
            Instance::AlreadyRunning => {
                request_show(&options.data_dir)
                    .context("asking the running OmniMem to show itself")?;
                info!("OmniMem is already running; asked it to open the settings panel");
                return Ok(());
            }
        }
    };
    take_show_request(&options.data_dir);
    // The smoke test runs where there is no keychain to ask.
    let keychain = (!options.smoke_test).then_some(Keychain);
    install_settings(&options.data_dir, keychain.as_ref());

    #[allow(unused_mut)]
    let mut event_loop = EventLoopBuilder::<AppEvent>::with_user_event().build();
    #[cfg(target_os = "macos")]
    {
        use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
        // A menu bar app: no Dock icon, no app menu.
        event_loop.set_activation_policy(ActivationPolicy::Accessory);
    }
    let proxy = event_loop.create_proxy();
    let menu_proxy = Mutex::new(proxy.clone());
    MenuEvent::set_event_handler(Some(move |event| {
        if let Ok(proxy) = menu_proxy.lock() {
            let _ = proxy.send_event(AppEvent::Menu(event));
        }
    }));

    let login = start_at_login();
    let panel = Panel::new();
    panel.set_feeds_path(feeds_path(&options.db));
    panel.set_settings_path(options.data_dir.join(SETTINGS_FILE));
    if let Some(keychain) = keychain {
        panel.set_secret_store(Arc::new(keychain));
    }
    let overhead = omnimem_app::context_overhead();
    panel.set_static_overhead(StaticOverhead {
        instructions_chars: overhead.instructions_chars,
        tool_count: overhead.tool_count,
        tool_schemas_chars: overhead.tool_schemas_chars,
        deferred_names_chars: overhead.deferred_names_chars,
    });
    // The hooks run this program on this database, whatever the default is.
    let opencode_plugin = omnimem_app::opencode::this_program()
        .map(|program| {
            omnimem_app::opencode::plugin_source(&omnimem_app::opencode::hook_command(
                &program,
                Some(&options.db),
            ))
        })
        .map_err(|e| warn!(error = %format!("{e:#}"), "the OpenCode hooks are unavailable"))
        .ok();
    let hook_base = omnimem_app::opencode::this_program()
        .map(|program| omnimem_app::shell::hook_base(&program, Some(&options.db)))
        .ok();
    let mut app = App {
        menu: TrayMenu::new(login.as_ref())?,
        options,
        proxy,
        panel,
        engine: Arc::default(),
        open_when_running: false,
        tray: None,
        state: ServiceState::Starting,
        shutdown: CancellationToken::new(),
        services_running: false,
        quit_requested: None,
        clipboard: None,
        login,
        desktop: ClientSlot::default(),
        code: ClientSlot::default(),
        opencode: ClientSlot::default(),
        opencode_hooks: ClientSlot::default(),
        opencode_plugin,
        vibe: ClientSlot::default(),
        vibe_hooks: ClientSlot::default(),
        hook_base,
        codex: ClientSlot::default(),
        codex_hooks: ClientSlot::default(),
        kiro: ClientSlot::default(),
        kiro_hooks: ClientSlot::default(),
        clients_shown: RefCell::new(CLIENTS.to_vec()),
        clients_checked: None,
        update: None,
        started: Instant::now(),
        in_flatpak: std::env::var_os("FLATPAK_ID").is_some_and(|id| !id.is_empty()),
    };
    event_loop.run(move |event, target, control_flow| {
        // Held until the process exits.
        let _lock = &lock;
        app.handle(event, target, control_flow);
    })
}
