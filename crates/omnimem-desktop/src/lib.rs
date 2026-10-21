//! The desktop app: OmniMem's services behind a tray or menu bar icon, with
//! the settings panel in a window.
//!
//! The platform event loop owns the main thread, as macOS requires and GTK
//! expects, and the services run on a background thread reporting their
//! state back into it. The window is a webview whose pages come from
//! `omnimem-settings` through the `omnimem://` scheme, answered on a small
//! tokio runtime, so nothing about the panel is reachable over a network.

mod clients;
pub mod icon;
mod model;

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
use omnimem_settings::{Panel, SecretStore, StaticOverhead, secret_settings};
use tao::event::{Event, StartCause, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy, EventLoopWindowTarget};
use tao::window::{Window, WindowBuilder};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{TrayIcon, TrayIconBuilder};
use wry::{NewWindowResponse, PageLoadEvent, WebView, WebViewBuilder};

pub struct DesktopOptions {
    /// The database the services open.
    pub db: PathBuf,
    /// Where the single-instance lock and show-window requests live.
    pub data_dir: PathBuf,
    /// Build the tray and window, load the panel, check a POST and the
    /// stylesheet through the scheme, and exit, without starting the services.
    pub smoke_test: bool,
}

#[derive(Debug)]
enum AppEvent {
    State(ServiceState),
    Menu(MenuEvent),
    Page(String),
    PageLoaded(String),
    ServicesExited,
    /// A change to a client's settings finished.
    ClientDone(Client, Result<(), String>),
}

const ICON_SIZE: u32 = 64;
const POLL: Duration = Duration::from_millis(500);
const SMOKE_DEADLINE: Duration = Duration::from_secs(60);
const QUIT_DEADLINE: Duration = Duration::from_secs(15);
/// How often the clients' settings are read again, for edits made elsewhere.
const CLIENT_RECHECK: Duration = Duration::from_secs(30);

struct TrayMenu {
    menu: Menu,
    status: MenuItem,
    settings: MenuItem,
    copy_url: MenuItem,
    desktop: MenuItem,
    code: MenuItem,
    start_at_login: CheckMenuItem,
    quit: MenuItem,
}

impl TrayMenu {
    fn new(login: Option<&AutoLaunch>) -> Result<Self> {
        let enabled = login.and_then(|l| l.is_enabled().ok()).unwrap_or(false);
        let this = Self {
            menu: Menu::new(),
            status: MenuItem::new(model::status_line(&ServiceState::Starting), false, None),
            settings: MenuItem::new("Settings…", true, None),
            copy_url: MenuItem::new("Copy MCP URL", false, None),
            desktop: client_menu_item(Client::Desktop),
            code: client_menu_item(Client::Code),
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
    runtime: Arc<tokio::runtime::Runtime>,
    menu: TrayMenu,
    tray: Option<TrayIcon>,
    window: Option<(Window, WebView)>,
    state: ServiceState,
    shutdown: CancellationToken,
    services_running: bool,
    quit_requested: Option<Instant>,
    clipboard: Option<arboard::Clipboard>,
    login: Option<AutoLaunch>,
    desktop: ClientSlot,
    code: ClientSlot,
    clients_checked: Option<Instant>,
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

/// Open a link from the panel in the system browser. Only web links (and
/// mail links) are handed to the OS: a `file:`, `smb:` or custom-scheme URL
/// rendered from a memory's `source_url` would otherwise launch whatever
/// handles that scheme.
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
            .with_tooltip("OmniMem")
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
        let (db, token, proxy, panel) = (
            self.options.db.clone(),
            self.shutdown.clone(),
            self.proxy.clone(),
            self.panel.clone(),
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
                let ready = |engine: &Arc<omnimem_engine::Engine>| panel.set_engine(engine.clone());
                if let Err(e) = run_services(&db, token, false, &report, &ready) {
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

    fn open_window(&mut self, target: &EventLoopWindowTarget<AppEvent>) -> Result<()> {
        if let Some((window, _)) = &self.window {
            window.set_visible(true);
            window.set_focus();
            return Ok(());
        }
        let icon = tao::window::Icon::from_rgba(icon::app_icon(ICON_SIZE), ICON_SIZE, ICON_SIZE)
            .map_err(|e| anyhow!("window icon: {e}"))?;
        let window = WindowBuilder::new()
            .with_title("OmniMem")
            .with_inner_size(tao::dpi::LogicalSize::new(1180.0, 800.0))
            .with_window_icon(Some(icon))
            .build(target)
            .context("creating the settings window")?;

        let (panel, runtime) = (self.panel.clone(), self.runtime.clone());
        let (ipc_proxy, load_proxy) = (self.proxy.clone(), self.proxy.clone());
        let smoke = self.options.smoke_test;
        let builder = WebViewBuilder::new()
            .with_asynchronous_custom_protocol(
                model::SCHEME.to_owned(),
                move |_id, request, responder| {
                    let panel = panel.clone();
                    runtime.spawn(async move {
                        let (method, uri) = (request.method().clone(), request.uri().clone());
                        let body_bytes = request.body().len();
                        let response = panel.handle(request).await;
                        if smoke {
                            info!(%method, %uri, body_bytes, status = response.status().as_u16(), "panel request");
                        }
                        responder.respond(response);
                    });
                },
            )
            .with_ipc_handler(move |request| {
                let _ = ipc_proxy.send_event(AppEvent::Page(request.body().clone()));
            })
            .with_on_page_load_handler(move |event, url| {
                if matches!(event, PageLoadEvent::Finished) {
                    let _ = load_proxy.send_event(AppEvent::PageLoaded(url));
                }
            })
            .with_navigation_handler(|url| {
                if model::is_panel_url(&url) {
                    true
                } else {
                    open_externally(url);
                    false
                }
            })
            .with_new_window_req_handler(|url, _features| {
                if !model::is_panel_url(&url) {
                    open_externally(url);
                }
                NewWindowResponse::Deny
            })
            .with_url(model::start_url());

        #[cfg(any(
            target_os = "linux",
            target_os = "dragonfly",
            target_os = "freebsd",
            target_os = "netbsd",
            target_os = "openbsd"
        ))]
        let webview = {
            use tao::platform::unix::WindowExtUnix;
            use wry::WebViewBuilderExtUnix;
            let vbox = window
                .default_vbox()
                .ok_or_else(|| anyhow!("the window has no GTK container"))?;
            builder.build_gtk(vbox)
        };
        #[cfg(not(any(
            target_os = "linux",
            target_os = "dragonfly",
            target_os = "freebsd",
            target_os = "netbsd",
            target_os = "openbsd"
        )))]
        let webview = builder.build(&window);

        let webview = webview.context("creating the settings webview")?;
        self.window = Some((window, webview));
        Ok(())
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
    }

    /// Read both clients' settings again, against the current MCP URL, and
    /// relabel their menu items. Without a URL there's nothing to add.
    fn check_clients(&mut self) {
        self.clients_checked = Some(Instant::now());
        let url = model::mcp_url(&self.state).map(str::to_owned);
        self.desktop.presence = url
            .as_deref()
            .map(|url| clients::desktop_presence(clients::desktop_config_path().as_deref(), url));
        self.code.presence = url.as_deref().map(|url| {
            clients::code_presence(
                clients::find_claude().as_deref(),
                clients::code_config_path().as_deref(),
                url,
            )
        });
        self.relabel(Client::Desktop);
        self.relabel(Client::Code);
    }

    fn slot(&mut self, client: Client) -> &mut ClientSlot {
        match client {
            Client::Desktop => &mut self.desktop,
            Client::Code => &mut self.code,
        }
    }

    fn relabel(&self, client: Client) {
        let (slot, item) = match client {
            Client::Desktop => (&self.desktop, &self.menu.desktop),
            Client::Code => (&self.code, &self.menu.code),
        };
        let (text, enabled) = if self.in_flatpak && client == Client::Code {
            (model::CODE_IN_FLATPAK.to_owned(), false)
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
            (Some(Presence::Elsewhere { name, .. }), false) => Some(name.clone()),
            _ => return,
        };
        slot.busy = true;
        slot.error = None;
        self.relabel(client);
        // The token the services check, if one is set, so the client can get in.
        let token = omnimem_core::env::var("MCP_AUTH_TOKEN").filter(|t| !t.is_empty());
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
        target: &EventLoopWindowTarget<AppEvent>,
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
                    warn!(error = %format!("{e:#}"), "no tray icon, opening the settings window instead");
                }
                // On Linux the icon is "created" even when nothing will show
                // it: stock GNOME has no tray without the AppIndicator
                // extension. Then the window is the app's only face, and it
                // says what to install.
                let tray_shows = created.is_ok() && tray_host_running();
                if !self.options.smoke_test && !tray_shows {
                    if created.is_ok() {
                        warn!("nothing here shows tray icons, opening the settings window instead");
                    }
                    self.panel.set_notice(model::NO_TRAY.to_owned());
                    let _ = self.open_window(target);
                }
                if self.options.smoke_test {
                    if let Err(e) = self.open_window(target) {
                        error!(error = %format!("{e:#}"), "smoke test: no settings window");
                        *control_flow = ControlFlow::ExitWithCode(1);
                    }
                } else {
                    self.start_services();
                }
            }
            Event::NewEvents(_) => {
                if !self.options.smoke_test
                    && take_show_request(&self.options.data_dir)
                    && let Err(e) = self.open_window(target)
                {
                    error!(error = %format!("{e:#}"), "could not open the settings window");
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
                    if let Err(e) = self.open_window(target) {
                        error!(error = %format!("{e:#}"), "could not open the settings window");
                    }
                } else if id == self.menu.copy_url.id() {
                    self.copy_mcp_url();
                } else if id == self.menu.desktop.id() {
                    self.change_client(Client::Desktop);
                } else if id == self.menu.code.id() {
                    self.change_client(Client::Code);
                } else if id == self.menu.start_at_login.id() {
                    self.toggle_start_at_login();
                } else if id == self.menu.quit.id() {
                    self.quit(control_flow);
                }
            }
            Event::UserEvent(AppEvent::PageLoaded(url)) => {
                if self.options.smoke_test {
                    info!(url, "smoke test: page loaded");
                }
                // Every ordinary page load runs the check, not just the first:
                // the starting page reloads itself every two seconds, which
                // can cut a check short on a slow machine (the check holds
                // its failure back long enough for that reload to discard
                // it). The /_panel/ pages are the check's own redirect and
                // landing.
                if self.options.smoke_test
                    && !model::is_smoke_page(&url)
                    && let Some((_, webview)) = &self.window
                    && let Err(e) = webview.evaluate_script(model::SMOKE_SCRIPT)
                {
                    error!(error = %e, "smoke test: could not run the check");
                    *control_flow = ControlFlow::ExitWithCode(1);
                }
            }
            Event::UserEvent(AppEvent::Page(body)) => {
                if self.options.smoke_test {
                    match model::smoke_verdict(&body) {
                        Ok(found) => {
                            println!(
                                "smoke test passed: tray icon, settings window and panel over omnimem:// ({found})"
                            );
                            *control_flow = ControlFlow::ExitWithCode(0);
                        }
                        Err(problem) => {
                            error!(problem, "smoke test failed");
                            *control_flow = ControlFlow::ExitWithCode(1);
                        }
                    }
                } else {
                    warn!(body, "ignoring a message from the panel");
                }
            }
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                // Closing the window leaves OmniMem running in the tray.
                self.window = None;
            }
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

/// Run the desktop app until Quit. A second launch asks the first to show
/// its window and returns.
pub fn run(options: DesktopOptions) -> Result<()> {
    let lock = if options.smoke_test {
        None
    } else {
        match acquire(&options.data_dir).context("checking for a running OmniMem")? {
            Instance::Primary(lock) => Some(lock),
            Instance::AlreadyRunning => {
                request_show(&options.data_dir)
                    .context("asking the running OmniMem to show itself")?;
                info!("OmniMem is already running; asked it to show its window");
                return Ok(());
            }
        }
    };
    take_show_request(&options.data_dir);
    // The smoke test runs where there is no keychain to ask.
    let keychain = (!options.smoke_test).then_some(Keychain);
    install_settings(&options.data_dir, keychain.as_ref());

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("omnimem-panel")
        .enable_all()
        .build()
        .context("starting the panel runtime")?;
    let runtime = Arc::new(runtime);
    // Never shut down. On Linux each panel request holds a WebKit request,
    // which may only be released on the main thread; a shutdown as the app
    // quits cancels requests still waiting (an update check on a slow
    // network, say) and releases theirs on a worker thread after the window
    // has gone, and WebKitGTK aborts. The process ending stops these threads,
    // and the services have stopped by then (`quit`).
    std::mem::forget(Arc::clone(&runtime));

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
    let panel = if options.smoke_test {
        Panel::with_smoke_routes()
    } else {
        Panel::new()
    };
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
    let mut app = App {
        menu: TrayMenu::new(login.as_ref())?,
        options,
        proxy,
        panel,
        runtime,
        tray: None,
        window: None,
        state: ServiceState::Starting,
        shutdown: CancellationToken::new(),
        services_running: false,
        quit_requested: None,
        clipboard: None,
        login,
        desktop: ClientSlot::default(),
        code: ClientSlot::default(),
        clients_checked: None,
        started: Instant::now(),
        in_flatpak: std::env::var_os("FLATPAK_ID").is_some_and(|id| !id.is_empty()),
    };
    event_loop.run(move |event, target, control_flow| {
        // Held until the process exits.
        let _lock = &lock;
        app.handle(event, target, control_flow);
    })
}
