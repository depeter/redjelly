//! redjelly entry point: owns the Slint window, the tokio runtime and app
//! state, and routes every input through the keymap (SPEC §2).
//!
//! `redjelly --play <file-or-url>` (or dropping a file on the exe) skips the
//! server and plays directly — handy for testing video output.

// No console window on Windows; logs go to %LOCALAPPDATA%\redjelly\data\logs.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod ai;
mod audio;
mod controls;
mod gl_state;
mod images;
mod layout;
mod login;
mod models;
mod nav;
mod playback;
mod player_ui;
mod screens;
mod secrets;
mod settings;
mod subtitles;
mod video;
#[cfg(windows)]
mod video_window;

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use rj_core::config::{Config, VideoOutput};
use rj_core::state::AppState;
use rj_input::media_controls::OsMediaControls;
use rj_input::{Action, Context, KeyChord, Keymap};
use rj_jellyfin::Client;
use rj_player::Player;
use rj_player::render::VideoRenderer;
use slint::winit_030::WinitWindowAccessor;
use slint::{ComponentHandle, GraphicsAPI, ModelRc, RenderingState, SharedString, VecModel};

use crate::nav::Placement;
use crate::playback::{PlayRequest, Session};
use crate::screens::Source;

slint::include_modules!();

pub const SCREEN_BROWSE: i32 = 1;
pub const SCREEN_PLAYER: i32 = 2;
pub const SCREEN_LOGIN_FORM: i32 = 3;
pub const SCREEN_QUICK_CONNECT: i32 = 4;
pub const SCREEN_PROMPT: i32 = 5;
const TOAST_DURATION: Duration = Duration::from_secs(4);

thread_local! {
    /// The app lives on the UI thread; async tasks reach it through
    /// `slint::invoke_from_event_loop` + this slot.
    static APP: RefCell<Option<Rc<App>>> = const { RefCell::new(None) };
}

pub fn with_app(f: impl FnOnce(&Rc<App>)) {
    let app = APP.with(|a| a.borrow().clone());
    if let Some(app) = app {
        f(&app)
    }
}

pub struct App {
    ui: slint::Weak<AppWindow>,
    rt: tokio::runtime::Handle,
    client: RefCell<Option<Client>>,
    config: RefCell<Config>,
    config_path: PathBuf,
    state: RefCell<AppState>,
    state_path: PathBuf,
    tokens: secrets::TokenStore,
    images: images::ImageLoader,
    keymap: RefCell<Keymap>,
    keymap_path: PathBuf,
    player: Player,
    nav: RefCell<Vec<nav::Screen>>,
    next_gen: Cell<u64>,
    /// A screen or playback is loading; ignore repeated Enter.
    busy: Cell<bool>,
    search_timer: slint::Timer,
    qc_gen: login::QcGeneration,
    session: RefCell<Option<Session>>,
    /// `--play` target waiting for the GL context to exist.
    pending: RefCell<Option<PlayRequest>>,
    video_ready: Cell<bool>,
    standalone: bool,
    osd_until: Cell<Instant>,
    toast_until: Cell<Instant>,
    stats: Cell<bool>,
    subs_raised: Cell<bool>,
    media: RefCell<Option<OsMediaControls>>,
    last_media_update: Cell<Instant>,
    /// Shared client for subtitle providers and server downloads.
    http: reqwest::Client,
    sub_search: RefCell<Option<subtitles::SubSearch>>,
    panel: RefCell<Option<subtitles::Panel>>,
    opensubs: RefCell<Option<std::sync::Arc<rj_subs::providers::opensubtitles::OpenSubtitles>>>,
    sub_cache: PathBuf,
    /// Last seen mpv `sid`, to re-apply remembered delays on track change.
    last_sid: RefCell<String>,
    jobs: RefCell<Vec<ai::Job>>,
    ai_running: Cell<bool>,
    models_dir: PathBuf,
    prompt: RefCell<Option<settings::PromptKind>>,
    gamepads: RefCell<Option<rj_input::gamepad::Gamepads>>,
    /// Player button with key/controller focus; None = keys control playback.
    controls_focus: Cell<Option<i32>>,
}

fn key_name(text: &str) -> Option<String> {
    use slint::platform::Key;
    let c = text.chars().next()?;
    let named = [
        (Key::LeftArrow, "Left"),
        (Key::RightArrow, "Right"),
        (Key::UpArrow, "Up"),
        (Key::DownArrow, "Down"),
        (Key::Return, "Enter"),
        (Key::Escape, "Esc"),
        (Key::Backspace, "Backspace"),
        (Key::Tab, "Tab"),
        (Key::Backtab, "Tab"),
        (Key::PageUp, "PageUp"),
        (Key::PageDown, "PageDown"),
        (Key::Space, "Space"),
        (Key::Menu, "Menu"),
        (Key::Home, "Home"),
        (Key::End, "End"),
    ];
    if let Some((_, name)) = named.iter().find(|(k, _)| c == char::from(*k)) {
        return Some((*name).into());
    }
    if c.is_control() || ('\u{F700}'..='\u{F8FF}').contains(&c) {
        return None;
    }
    Some(text.into())
}

/// The character a key types, for search input.
fn typed_char(text: &str) -> Option<char> {
    let mut chars = text.chars();
    let c = chars.next()?;
    (chars.next().is_none() && !c.is_control() && !('\u{F700}'..='\u{F8FF}').contains(&c)).then_some(c)
}

impl App {
    fn ui(&self) -> AppWindow {
        self.ui.upgrade().expect("window alive while app runs")
    }

    /// Runs `fut` on tokio, then `then` on the UI thread with the result.
    fn spawn_ui<T: Send + 'static>(
        &self,
        fut: impl Future<Output = T> + Send + 'static,
        then: impl FnOnce(&Rc<App>, T) + Send + 'static,
    ) {
        self.rt.spawn(async move {
            let value = fut.await;
            let _ = slint::invoke_from_event_loop(move || with_app(|app| then(app, value)));
        });
    }

    fn toast(&self, msg: impl Into<SharedString>) {
        self.ui().set_toast(msg.into());
        self.toast_until.set(Instant::now() + TOAST_DURATION);
    }

    fn clear_toast(&self) {
        self.ui().set_toast(SharedString::new());
    }

    fn tick(&self) {
        let now = Instant::now();
        let ui = self.ui();
        if now > self.toast_until.get() && !ui.get_toast().is_empty() {
            ui.set_toast(SharedString::new());
        }
        self.player_tick(now);
    }

    fn on_key(&self, text: &str, ctrl: bool, alt: bool, shift: bool) -> bool {
        let Some(name) = key_name(text) else { return false };
        self.dispatch(&name, typed_char(text), ctrl, alt, shift)
    }

    /// Gamepad buttons arrive as `Pad.*` key names.
    fn poll_gamepads(&self) {
        let chords = match self.gamepads.borrow_mut().as_mut() {
            Some(g) => g.poll(),
            None => return,
        };
        for chord in chords {
            // B on text screens means Esc.
            let screen = self.ui().get_screen();
            let name = if chord.key == "Pad.East" && [SCREEN_LOGIN_FORM, SCREEN_QUICK_CONNECT, SCREEN_PROMPT].contains(&screen) {
                "Esc".to_owned()
            } else {
                chord.key.clone()
            };
            self.dispatch(&name, None, false, false, false);
        }
    }

    /// Routes one key press (keyboard or controller) to whatever owns input.
    fn dispatch(&self, name: &str, typed: Option<char>, ctrl: bool, alt: bool, shift: bool) -> bool {
        let ui = self.ui();
        let chord = KeyChord::new(name, ctrl, alt, shift);
        if self.capture_key(&chord) {
            return true;
        }
        if ui.get_keyhelp_visible() {
            ui.set_keyhelp_visible(false);
            return true;
        }
        let screen = ui.get_screen();
        if screen == SCREEN_LOGIN_FORM || screen == SCREEN_QUICK_CONNECT || screen == SCREEN_PROMPT {
            if name == "Esc" {
                if screen == SCREEN_PROMPT {
                    self.prompt_cancel();
                } else {
                    self.login_escape();
                }
                return true;
            }
            return false;
        }

        if self.panel_open() {
            let in_player = screen == SCREEN_PLAYER;
            let dialog = self.keymap.borrow().lookup(Context::Dialog, &chord);
            match dialog {
                Some(Action::NavUp) => self.panel_move(-1),
                Some(Action::NavDown) => self.panel_move(1),
                Some(Action::Activate) => self.panel_activate(),
                Some(Action::Back) => self.close_panel(),
                _ if in_player => {
                    // S toggles the panel closed; other player keys still work.
                    let player = self.keymap.borrow().lookup(Context::Player, &chord);
                    match player {
                        Some(Action::SubtitlePanel) => self.close_panel(),
                        Some(action) => self.handle(Context::Player, action),
                        None => return false,
                    }
                }
                _ => return false,
            }
            return true;
        }

        let in_search = matches!(self.top_source(), Some(Source::Search { .. }));
        if screen == SCREEN_BROWSE && in_search && !ctrl && !alt {
            if name == "Backspace" {
                self.search_edit(None);
                return true;
            }
            if let Some(c) = typed {
                self.search_edit(Some(c));
                return true;
            }
        }

        // Player bar has focus: the arrows move between its buttons.
        if screen == SCREEN_PLAYER && self.controls_engaged() {
            let nav = self.keymap.borrow().lookup(Context::Dialog, &chord);
            if let Some(
                a @ (Action::NavUp | Action::NavDown | Action::NavLeft | Action::NavRight | Action::Activate | Action::Back),
            ) = nav
            {
                self.controls_nav(a);
                return true;
            }
        }

        let ctx = if screen == SCREEN_PLAYER { Context::Player } else { Context::Browse };
        let action = self.keymap.borrow().lookup(ctx, &chord);
        if let Some(action) = action {
            tracing::debug!(%chord, ?action, "key");
            self.handle(ctx, action);
            return true;
        }
        // Type-to-search from any browse screen once signed in.
        let signed_in = self.client.borrow().is_some();
        if ctx == Context::Browse && signed_in && !ctrl && !alt
            && let Some(c) = typed.filter(|c| c.is_alphanumeric()) {
                self.open(Source::Search { term: c.to_string() }, Placement::Push);
                return true;
            }
        false
    }

    fn handle(&self, ctx: Context, action: Action) {
        let ui = self.ui();
        match action {
            Action::ToggleFullscreen => {
                let w = ui.window();
                w.set_fullscreen(!w.is_fullscreen());
            }
            Action::Quit => {
                let _ = slint::quit_event_loop();
            }
            Action::ShowKeymap => self.show_keymap(ctx),
            _ if ctx == Context::Player => self.handle_player(action),
            Action::NavUp | Action::PrevRegion => self.move_focus(-1, 0),
            Action::NavDown | Action::NextRegion => self.move_focus(1, 0),
            Action::NavLeft => self.move_focus(0, -1),
            Action::NavRight => self.move_focus(0, 1),
            Action::Activate => self.activate_focused(),
            Action::Back => self.back(),
            Action::Search if self.client.borrow().is_some() => {
                self.open(Source::Search { term: String::new() }, Placement::Push)
            }
            Action::GoHome => self.go_home(),
            Action::Jobs if self.client.borrow().is_some() => self.open(Source::Jobs, Placement::Push),
            Action::Settings if self.top_source() != Some(Source::Settings) => {
                self.open(Source::Settings, Placement::Push)
            }
            Action::SwitchUser => self.switch_user(),
            _ => {}
        }
    }

    /// The `?` overlay: every binding for the current context, grouped by action.
    fn show_keymap(&self, ctx: Context) {
        let mut by_action: Vec<(String, Vec<String>)> = Vec::new();
        let keymap = self.keymap.borrow();
        let mut bindings = keymap.bindings(ctx);
        let local: Vec<_> = bindings.iter().map(|(k, _)| k.clone()).collect();
        bindings.extend(keymap.bindings(Context::Global).into_iter().filter(|(k, _)| !local.contains(k)));
        drop(keymap);
        for (key, action) in bindings {
            if key.key.starts_with("Pad.") || key.key.starts_with("Media") {
                continue;
            }
            let label = action.to_string();
            match by_action.iter_mut().find(|(l, _)| *l == label) {
                Some((_, keys)) => keys.push(key.to_string()),
                None => by_action.push((label, vec![key.to_string()])),
            }
        }
        by_action.sort_by(|a, b| a.0.cmp(&b.0));
        let items: Vec<KeyHelp> = by_action
            .into_iter()
            .map(|(action, keys)| KeyHelp { key: keys.join("  ").into(), action: action.into() })
            .collect();
        let ui = self.ui();
        ui.set_keyhelp_title(if ctx == Context::Player { "Player keys" } else { "Keys" }.into());
        ui.set_keyhelp(ModelRc::new(VecModel::from(items)));
        ui.set_keyhelp_visible(true);
    }

    /// The UI window's native handle (HWND) on Windows.
    fn hwnd(&self) -> Option<*mut std::ffi::c_void> {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let ui = self.ui();
        let handle = ui.window().window_handle();
        handle.window_handle().ok().and_then(|h| match h.as_raw() {
            RawWindowHandle::Win32(w) => Some(w.hwnd.get() as *mut std::ffi::c_void),
            _ => None,
        })
    }

    /// Playback can start: mpv has somewhere to draw.
    fn on_video_ready(&self) {
        self.video_ready.set(true);
        if let Some(req) = self.pending.borrow_mut().take() {
            self.start(req);
        }
    }

    /// Window output: creates mpv's native window behind the UI window and
    /// makes the player screen see-through.
    #[cfg(windows)]
    fn setup_video_window(&self) {
        let Some(ui_hwnd) = self.hwnd() else {
            return tracing::error!("no native handle for the UI window; no video");
        };
        let result = video_window::VideoWindow::new().and_then(|w| {
            w.follow(ui_hwnd)?;
            self.player.set_window(w.handle())?;
            // Lives as long as the process; mpv draws into it until exit.
            std::mem::forget(w);
            Ok(())
        });
        match result {
            Ok(()) => {
                self.ui().set_video_behind(true);
                self.on_video_ready();
            }
            Err(e) => {
                tracing::error!("video window: {e:#}");
                self.toast(format!("Couldn't create the video window ({e}). Try Settings → Video output → Embedded."));
            }
        }
    }

    #[cfg(not(windows))]
    fn setup_video_window(&self) {}

    fn setup_media_controls(&self) {
        let hwnd = self.hwnd();
        let on_action = |action: Action| {
            let _ = slint::invoke_from_event_loop(move || with_app(|app| app.media_key(action)));
        };
        match OsMediaControls::new(hwnd, on_action) {
            Ok(m) => *self.media.borrow_mut() = Some(m),
            Err(e) => tracing::warn!("media keys unavailable: {e:?}"),
        }
    }
}

fn main() -> anyhow::Result<()> {
    let dirs = rj_core::config::dirs()?;
    let log_dir = dirs.data_local_dir().join("logs");
    std::fs::create_dir_all(&log_dir)?;
    let log_file = std::fs::File::create(log_dir.join("redjelly.log"))?;
    {
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::util::SubscriberInitExt;
        let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
        tracing_subscriber::registry()
            .with(filter)
            .with(tracing_subscriber::fmt::layer())
            .with(tracing_subscriber::fmt::layer().with_ansi(false).with_writer(std::sync::Mutex::new(log_file)))
            .init();
    }

    let mut args = std::env::args().skip(1);
    let mut play_target = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--play" => play_target = Some(args.next().context("--play needs a file or URL")?),
            // A file dropped onto the exe arrives as a bare path.
            path if !path.starts_with("--") => play_target = Some(path.to_owned()),
            other => anyhow::bail!("unknown argument `{other}` (usage: redjelly [--play <file-or-url>])"),
        }
    }

    let config_path = Config::default_path()?;
    let mut config = Config::load(&config_path)?;
    if config.server.device_id.is_none() {
        config.server.device_id = Some(uuid::Uuid::new_v4().to_string());
        config.save(&config_path)?;
    }
    let state_path = dirs.data_local_dir().join("state.json");
    let tokens = secrets::TokenStore::new(dirs.data_local_dir().to_path_buf());
    // A provider password typed into config.toml moves to the credential store.
    if let Some(pw) = config.providers.opensubtitles_password.take() {
        match &config.providers.opensubtitles_username {
            Some(user) => tokens.set_secret(&secrets::provider_account("opensubtitles", user), &pw),
            None => tracing::warn!("opensubtitles_password set without opensubtitles_username; ignored"),
        }
        config.save(&config_path)?;
    }

    // Absolute UI zoom for 10-foot viewing; must be set before the window
    // system starts.
    if config.appearance.ui_scale > 0.0 {
        // SAFETY: still single-threaded here (no runtime or window yet).
        unsafe { std::env::set_var("SLINT_SCALE_FACTOR", config.appearance.ui_scale.to_string()) };
    }

    let keymap_path = dirs.config_dir().join("keymap.toml");
    let keymap = {
        let user_path = &keymap_path;
        let user = std::fs::read_to_string(user_path).unwrap_or_default();
        let (km, warnings) = Keymap::defaults().overlay(&user);
        for w in warnings {
            tracing::warn!("{}: {w}", user_path.display());
        }
        km
    };

    let mpv_log = log_dir.join("mpv.log");
    tracing::info!("mpv log: {}", mpv_log.display());
    let player = Player::new(&config, Some(&mpv_log)).context("starting mpv")?;

    let video_output = config.video_output();
    // Embedded video is drawn with OpenGL under the UI, so the renderer
    // must be GL; window output needs a transparent window to see through.
    let backend = slint::BackendSelector::new().require_opengl();
    let backend = if video_output == VideoOutput::Window {
        backend.with_winit_window_attributes_hook(|a| a.with_transparent(true))
    } else {
        backend
    };
    backend.select().context("no OpenGL-capable renderer available")?;

    let runtime = tokio::runtime::Runtime::new().context("starting tokio")?;
    let ui = AppWindow::new()?;

    let app = Rc::new(App {
        ui: ui.as_weak(),
        rt: runtime.handle().clone(),
        client: RefCell::new(None),
        config: RefCell::new(config),
        config_path,
        state: RefCell::new(AppState::load(&state_path)),
        state_path,
        tokens,
        images: images::ImageLoader::new(dirs.cache_dir().join("images")),
        keymap: RefCell::new(keymap),
        keymap_path,
        player,
        nav: RefCell::new(Vec::new()),
        next_gen: Cell::new(0),
        busy: Cell::new(false),
        search_timer: slint::Timer::default(),
        qc_gen: Default::default(),
        session: RefCell::new(None),
        pending: RefCell::new(None),
        video_ready: Cell::new(false),
        standalone: play_target.is_some(),
        osd_until: Cell::new(Instant::now()),
        toast_until: Cell::new(Instant::now()),
        stats: Cell::new(false),
        subs_raised: Cell::new(false),
        media: RefCell::new(None),
        last_media_update: Cell::new(Instant::now()),
        http: reqwest::Client::new(),
        sub_search: RefCell::new(None),
        panel: RefCell::new(None),
        opensubs: RefCell::new(None),
        sub_cache: dirs.cache_dir().join("subtitles"),
        last_sid: RefCell::new(String::new()),
        jobs: RefCell::new(Vec::new()),
        ai_running: Cell::new(false),
        models_dir: dirs.data_local_dir().join("models"),
        prompt: RefCell::new(None),
        gamepads: RefCell::new(match rj_input::gamepad::Gamepads::new() {
            Ok(g) => Some(g),
            Err(e) => {
                tracing::warn!("gamepads unavailable: {e}");
                None
            }
        }),
        controls_focus: Cell::new(None),
    });
    app.apply_tone_mapping();
    APP.with(|a| *a.borrow_mut() = Some(app.clone()));

    ui.on_login(|| with_app(|app| app.submit_login_form()));
    ui.on_key(|text, ctrl, alt, shift| {
        let mut handled = false;
        with_app(|app| handled = app.on_key(&text, ctrl, alt, shift));
        handled
    });
    ui.on_card_clicked(|r, c| {
        with_app(|app| {
            app.focus_card(r as usize, c as usize);
            app.activate_focused();
        })
    });
    ui.on_player_clicked(|| with_app(|app| app.video_clicked()));
    ui.on_player_double_clicked(|| {
        with_app(|app| {
            let ui = app.ui();
            ui.window().set_fullscreen(!ui.window().is_fullscreen());
        })
    });
    ui.on_player_pointer_moved(|| with_app(|app| app.show_osd()));
    ui.on_player_button(|b| with_app(|app| app.press_control(b)));
    ui.on_player_seek(|f| with_app(|app| app.seek_to_fraction(f)));
    ui.on_player_set_volume(|f| with_app(|app| app.set_volume_fraction(f)));
    ui.on_player_skip_clicked(|| {
        with_app(|app| {
            app.skip_segment_or_play_next();
        })
    });
    ui.on_panel_item_clicked(|i| with_app(|app| app.panel_click(i as usize)));
    ui.on_panel_close(|| with_app(|app| app.close_panel()));
    ui.on_prompt_accepted(|| with_app(|app| app.prompt_accepted()));

    // ---- mpv video underlay ------------------------------------------------
    let renderer: RefCell<Option<(VideoRenderer, glow::Context)>> = RefCell::new(None);
    let ui_weak = ui.as_weak();
    let notifier = ui.window().set_rendering_notifier(move |state, api| match state {
        RenderingState::RenderingSetup if video_output == VideoOutput::Window => {
            // Needs the live native window: after this frame.
            slint::Timer::single_shot(Duration::ZERO, || with_app(|app| app.setup_video_window()));
        }
        RenderingState::RenderingSetup => {
            let GraphicsAPI::NativeOpenGL { get_proc_address } = api else { return };
            // SAFETY: Slint guarantees the GL context is current in this callback.
            let gl = unsafe { glow::Context::from_loader_function_cstr(|s| get_proc_address(s)) };
            let redraw = ui_weak.clone();
            let mpv = APP.with(|a| a.borrow().as_ref().map(|app| app.player.mpv()));
            let Some(mpv) = mpv else { return };
            let created = VideoRenderer::new(mpv, *get_proc_address, move || {
                let _ = redraw.upgrade_in_event_loop(|ui| {
                    // Straight to winit: off macOS/Wayland, Slint's own
                    // request_redraw waits for a free-running ~16 ms timer
                    // that drifts against vsync, so video frames landed on
                    // 1, 2 or 3 refreshes at random — visible judder.
                    if ui.window().with_winit_window(|w| w.request_redraw()).is_none() {
                        ui.window().request_redraw();
                    }
                });
            });
            match created {
                Ok(r) => {
                    *renderer.borrow_mut() = Some((r, gl));
                    // Loading may touch UI properties: do it after this frame.
                    slint::Timer::single_shot(Duration::ZERO, || with_app(|app| app.on_video_ready()));
                }
                Err(e) => tracing::error!("creating mpv render context: {e}"),
            }
        }
        RenderingState::BeforeRendering => {
            let Some(ui) = ui_weak.upgrade() else { return };
            if ui.get_screen() != SCREEN_PLAYER {
                return;
            }
            if let Some((r, gl)) = renderer.borrow().as_ref() {
                let size = ui.window().size();
                // SAFETY: called with Slint's GL context current.
                unsafe {
                    let saved = gl_state::GlState::save(gl);
                    if let Err(e) = r.render(size.width as i32, size.height as i32) {
                        tracing::warn!("mpv render: {e}");
                    }
                    saved.restore(gl);
                }
            }
        }
        RenderingState::RenderingTeardown => {
            renderer.borrow_mut().take();
        }
        _ => {}
    });
    if let Err(e) = notifier {
        anyhow::bail!("can't hook into the renderer for video output: {e:?}");
    }

    match &play_target {
        Some(target) => {
            ui.set_screen(SCREEN_PLAYER);
            app.start(PlayRequest::standalone(target));
        }
        None => app.startup(),
    }

    // Needs the native window, which exists once the event loop runs.
    slint::Timer::single_shot(Duration::from_millis(200), || with_app(|app| app.setup_media_controls()));
    let tick = slint::Timer::default();
    tick.start(slint::TimerMode::Repeated, Duration::from_millis(100), || with_app(|app| app.tick()));
    // Controllers need snappier polling than the 100 ms UI tick.
    let pad_tick = slint::Timer::default();
    pad_tick.start(slint::TimerMode::Repeated, Duration::from_millis(16), || with_app(|app| app.poll_gamepads()));

    // Open fullscreen; only F / Alt+Enter / double-click leave it.
    ui.window().set_fullscreen(true);
    ui.run()?;
    // Stop playback cleanly so the server gets a final position.
    with_app(|app| {
        if app.session.borrow().is_some() {
            app.report(player_ui::REPORT_STOPPED);
        }
    });
    APP.with(|a| a.borrow_mut().take());
    runtime.shutdown_timeout(Duration::from_secs(2));
    Ok(())
}
