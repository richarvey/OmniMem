//! The desktop app: OmniMem's services behind a tray or menu bar icon, with a
//! settings window.
//!
//! The platform event loop owns the main thread, as macOS requires and GTK
//! expects, and the services run on a background thread reporting their
//! state back into it. The settings window is a webview whose page comes from
//! the process through the `omnimem://` scheme and talks back over IPC, so
//! nothing about it is reachable over a network.

mod icon;
mod model;
mod page;

use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use auto_launch::{AutoLaunch, AutoLaunchBuilder};
use omnimem_app::{Instance, ServiceState, acquire, request_show, run_services, take_show_request};
use tao::event::{Event, StartCause, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy, EventLoopWindowTarget};
use tao::window::{Window, WindowBuilder};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{TrayIcon, TrayIconBuilder};
use wry::http::Response;
use wry::{WebView, WebViewBuilder};

use model::PageCommand;

pub struct DesktopOptions {
    /// The database the services open.
    pub db: PathBuf,
    /// Where the single-instance lock and show-window requests live.
    pub data_dir: PathBuf,
    /// Build the tray and window, load the page, round-trip a status over
    /// IPC and exit 0, without starting the services.
    pub smoke_test: bool,
}

#[derive(Debug)]
enum AppEvent {
    State(ServiceState),
    Menu(MenuEvent),
    Page(String),
    ServicesExited,
}

const ICON_SIZE: u32 = 64;
const POLL: Duration = Duration::from_millis(500);
const SMOKE_DEADLINE: Duration = Duration::from_secs(60);
const QUIT_DEADLINE: Duration = Duration::from_secs(15);

struct TrayMenu {
    menu: Menu,
    status: MenuItem,
    settings: MenuItem,
    copy_url: MenuItem,
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
            start_at_login: CheckMenuItem::new("Start at login", login.is_some(), enabled, None),
            quit: MenuItem::new("Quit OmniMem", true, None),
        };
        this.menu
            .append_items(&[
                &this.status,
                &PredefinedMenuItem::separator(),
                &this.settings,
                &this.copy_url,
                &this.start_at_login,
                &PredefinedMenuItem::separator(),
                &this.quit,
            ])
            .context("building the tray menu")?;
        Ok(this)
    }
}

struct App {
    options: DesktopOptions,
    proxy: EventLoopProxy<AppEvent>,
    menu: TrayMenu,
    tray: Option<TrayIcon>,
    window: Option<(Window, WebView)>,
    state: ServiceState,
    shutdown: CancellationToken,
    services_running: bool,
    quit_requested: Option<Instant>,
    clipboard: Option<arboard::Clipboard>,
    login: Option<AutoLaunch>,
    started: Instant,
}

fn start_at_login() -> Option<AutoLaunch> {
    let exe = std::env::current_exe().ok()?;
    AutoLaunchBuilder::new()
        .set_app_name("OmniMem")
        .set_app_path(exe.to_str()?)
        .set_args(&["desktop"])
        .build()
        .map_err(|e| warn!(error = %e, "start at login is unavailable"))
        .ok()
}

impl App {
    fn create_tray(&mut self) -> Result<()> {
        let icon = tray_icon::Icon::from_rgba(icon::rgba(ICON_SIZE), ICON_SIZE, ICON_SIZE)
            .map_err(|e| anyhow!("tray icon: {e}"))?;
        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(self.menu.menu.clone()))
            .with_tooltip("OmniMem")
            .with_icon(icon)
            .build()
            .context("creating the tray icon")?;
        self.tray = Some(tray);
        Ok(())
    }

    fn start_services(&mut self) {
        let (db, token, proxy) = (
            self.options.db.clone(),
            self.shutdown.clone(),
            self.proxy.clone(),
        );
        let spawned = std::thread::Builder::new()
            .name("omnimem-services".into())
            .spawn(move || {
                let report = |state| {
                    let _ = proxy.send_event(AppEvent::State(state));
                };
                if let Err(e) = run_services(&db, token, false, &report) {
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
        let icon = tao::window::Icon::from_rgba(icon::rgba(ICON_SIZE), ICON_SIZE, ICON_SIZE)
            .map_err(|e| anyhow!("window icon: {e}"))?;
        let window = WindowBuilder::new()
            .with_title("OmniMem")
            .with_inner_size(tao::dpi::LogicalSize::new(640.0, 520.0))
            .with_window_icon(Some(icon))
            .build(target)
            .context("creating the settings window")?;

        let proxy = self.proxy.clone();
        let builder = WebViewBuilder::new()
            .with_custom_protocol(model::SCHEME.to_owned(), |_id, request| {
                let (status, content_type, body) = model::respond(request.uri().path());
                Response::builder()
                    .status(status)
                    .header("Content-Type", content_type)
                    .header(
                        "Content-Security-Policy",
                        "default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'",
                    )
                    .body(Cow::Borrowed(body))
                    .unwrap_or_else(|_| Response::new(Cow::Borrowed(&b""[..])))
            })
            .with_ipc_handler(move |request| {
                let _ = proxy.send_event(AppEvent::Page(request.body().clone()));
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
        self.state = state;
        self.push_status();
    }

    fn push_status(&self) {
        if let Some((_, webview)) = &self.window
            && let Err(e) = webview.evaluate_script(&model::deliver_status_script(&self.state))
        {
            warn!(error = %e, "could not update the settings page");
        }
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
                if let Err(e) = self.create_tray() {
                    if self.options.smoke_test {
                        error!(error = %format!("{e:#}"), "smoke test: no tray icon");
                        *control_flow = ControlFlow::ExitWithCode(1);
                        return;
                    }
                    // A desktop without a tray (GNOME without the AppIndicator
                    // extension): show the window so the app is visibly running.
                    warn!(error = %format!("{e:#}"), "no tray icon, opening the settings window instead");
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
                    error!("smoke test: the settings page never answered over IPC");
                    *control_flow = ControlFlow::ExitWithCode(1);
                }
                if self
                    .quit_requested
                    .is_some_and(|at| at.elapsed() > QUIT_DEADLINE)
                {
                    warn!("the services did not stop in time, exiting anyway");
                    *control_flow = ControlFlow::Exit;
                }
            }
            Event::UserEvent(AppEvent::State(state)) => self.set_state(state),
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
                } else if id == self.menu.start_at_login.id() {
                    self.toggle_start_at_login();
                } else if id == self.menu.quit.id() {
                    self.quit(control_flow);
                }
            }
            Event::UserEvent(AppEvent::Page(body)) => match model::parse_command(&body) {
                Some(PageCommand::Ready) => self.push_status(),
                Some(PageCommand::Ack) if self.options.smoke_test => {
                    println!(
                        "smoke test passed: tray icon, settings window, omnimem:// page and IPC round trip"
                    );
                    *control_flow = ControlFlow::ExitWithCode(0);
                }
                Some(PageCommand::Ack) => {}
                Some(PageCommand::CopyMcpUrl) => self.copy_mcp_url(),
                None => warn!(body, "ignoring an unknown message from the settings page"),
            },
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
    let mut app = App {
        menu: TrayMenu::new(login.as_ref())?,
        options,
        proxy,
        tray: None,
        window: None,
        state: ServiceState::Starting,
        shutdown: CancellationToken::new(),
        services_running: false,
        quit_requested: None,
        clipboard: None,
        login,
        started: Instant::now(),
    };
    event_loop.run(move |event, target, control_flow| {
        // Held until the process exits.
        let _lock = &lock;
        app.handle(event, target, control_flow);
    })
}
