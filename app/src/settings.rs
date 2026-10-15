//! Settings screen (Ctrl+S), text prompts, the keymap editor and the
//! per-user install. Every change is saved to config.toml immediately and
//! applied live where possible.

use rj_core::config::SegmentAction;
use rj_core::lang::Lang;
use rj_input::{Action, Context, KeyChord, Keymap};
use slint::{ModelRc, SharedString, VecModel};

use crate::layout::STYLE_SETTING;
use crate::nav::Placement;
use crate::screens::{CardContent, Content, HeaderContent, RowContent, Source, Target};
use crate::subtitles::{Panel, PanelAction, PanelKind};
use crate::{App, PanelItem, SCREEN_PROMPT};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Setting {
    Languages,
    PreferSdh,
    Intro,
    Recap,
    Outro,
    Preview,
    Commercial,
    Countdown,
    Hwdec,
    ToneMapping,
    SubAutoSearch,
    SubAutoSync,
    SubUpload,
    SubSize,
    SubFont,
    ProvJellyfin,
    ProvSubdl,
    SubdlKey,
    ProvOpenSubtitles,
    OsKey,
    OsUser,
    OsPassword,
    AiRuntime,
    AiModel,
    AiGpu,
    AiVad,
    EditKeys,
    ResetKeys,
    UiScale,
    Install,
    SwitchUser,
    SignOut,
    ChangeServer,
}

const COUNTDOWNS: &[u32] = &[0, 5, 10, 15, 30];
const SUB_SIZES: &[u32] = &[36, 40, 46, 52, 58, 64];
const HWDEC: &[&str] = &["auto-safe", "auto", "no"];
const RUNTIMES: &[&str] = &["cuda12", "cuda11", "cpu"];
const MODELS: &[&str] = &["large-v3-turbo-q5_0", "large-v3-turbo", "medium-q5_0", "small"];
/// 0.0 = follow Windows display scaling.
const UI_SCALES: &[f32] = &[0.0, 1.0, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0];

fn next_in<T: PartialEq + Copy>(list: &[T], current: T) -> T {
    let i = list.iter().position(|v| *v == current).map_or(0, |i| (i + 1) % list.len());
    list[i]
}

fn on_off(b: bool) -> String {
    if b { "On".into() } else { "Off".into() }
}

fn segment_label(a: SegmentAction) -> String {
    match a {
        SegmentAction::Ask => "Show skip button".into(),
        SegmentAction::Auto => "Skip automatically".into(),
        SegmentAction::Off => "Off".into(),
    }
}

fn next_segment(a: SegmentAction) -> SegmentAction {
    match a {
        SegmentAction::Ask => SegmentAction::Auto,
        SegmentAction::Auto => SegmentAction::Off,
        SegmentAction::Off => SegmentAction::Ask,
    }
}

fn secret_label(v: &Option<String>) -> String {
    if v.as_deref().is_some_and(|k| !k.trim().is_empty()) { "Set".into() } else { "Not set".into() }
}

/// Where a text prompt's answer goes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PromptKind {
    Setting(Setting),
}

impl App {
    pub fn settings_content(&self) -> Content {
        let cfg = self.config.borrow();
        let card = |label: &str, value: String, s: Setting| CardContent {
            title: label.into(),
            subtitle: value,
            image: None,
            progress: -1.0,
            played: false,
            target: Target::Setting(s),
        };
        let row = |title: &str, cards: Vec<CardContent>| RowContent { title: title.into(), style: STYLE_SETTING, cards, start: 0 };
        let p = &cfg.playback;
        let sub = &cfg.subtitles;
        let prov = &cfg.providers;
        let ai = &cfg.ai;
        let scale = cfg.appearance.ui_scale;
        let rows = vec![
            row(
                "Languages",
                vec![
                    card("Preferred (audio & subtitles)", cfg.languages.preferred.join(", "), Setting::Languages),
                    card("Prefer SDH subtitles", on_off(cfg.languages.prefer_sdh), Setting::PreferSdh),
                ],
            ),
            row(
                "Playback",
                vec![
                    card("Intro", segment_label(p.intro), Setting::Intro),
                    card("Recap", segment_label(p.recap), Setting::Recap),
                    card("Credits", segment_label(p.outro), Setting::Outro),
                    card("Previews", segment_label(p.preview), Setting::Preview),
                    card("Commercials", segment_label(p.commercial), Setting::Commercial),
                    card(
                        "Next episode autoplay",
                        if p.next_episode_countdown == 0 { "Off".into() } else { format!("After {} s countdown", p.next_episode_countdown) },
                        Setting::Countdown,
                    ),
                    card("Hardware decoding", p.hwdec.clone(), Setting::Hwdec),
                ],
            ),
            row(
                "Picture",
                vec![
                    card("HDR tone mapping (Ctrl+T)", self.tone_mapping_name(), Setting::ToneMapping),
                    card(
                        "UI size",
                        if scale <= 0.0 { "Windows scaling".into() } else { format!("{:.0} % (after restart)", scale * 100.0) },
                        Setting::UiScale,
                    ),
                ],
            ),
            row(
                "Subtitles",
                vec![
                    card("Search when missing", on_off(sub.auto_search), Setting::SubAutoSearch),
                    card("Auto-sync downloads", on_off(sub.auto_sync_non_hash), Setting::SubAutoSync),
                    card("Save downloads to Jellyfin", on_off(sub.upload_to_server), Setting::SubUpload),
                    card("Size", sub.font_size.to_string(), Setting::SubSize),
                    card("Font", sub.font.clone(), Setting::SubFont),
                ],
            ),
            row(
                "Subtitle sources",
                vec![
                    card("Jellyfin server plugins", on_off(prov.jellyfin_remote), Setting::ProvJellyfin),
                    card("Subdl (free)", on_off(prov.subdl), Setting::ProvSubdl),
                    card("Subdl API key", secret_label(&prov.subdl_api_key), Setting::SubdlKey),
                    card("OpenSubtitles (quota)", on_off(prov.opensubtitles), Setting::ProvOpenSubtitles),
                    card("OpenSubtitles API key", secret_label(&prov.opensubtitles_api_key), Setting::OsKey),
                    card(
                        "OpenSubtitles user",
                        prov.opensubtitles_username.clone().unwrap_or_else(|| "Not set".into()),
                        Setting::OsUser,
                    ),
                    card("OpenSubtitles password", "Change…".into(), Setting::OsPassword),
                ],
            ),
            row(
                "AI subtitles",
                vec![
                    card(
                        "Whisper engine",
                        match ai.whisper_runtime.as_str() {
                            "cuda11" => "NVIDIA CUDA 11".into(),
                            "cpu" => "CPU only".into(),
                            _ => "NVIDIA CUDA 12".into(),
                        },
                        Setting::AiRuntime,
                    ),
                    card("Whisper model", ai.whisper_model.clone(), Setting::AiModel),
                    card("Use GPU", on_off(ai.use_gpu), Setting::AiGpu),
                    card("Voice detection", on_off(ai.vad), Setting::AiVad),
                ],
            ),
            row(
                "Keyboard & controller",
                vec![
                    card("Key bindings", "Edit…".into(), Setting::EditKeys),
                    card("Reset all keys", "Restore defaults".into(), Setting::ResetKeys),
                ],
            ),
            row(
                "This PC & account",
                vec![
                    card("Install", "Add to Start menu".into(), Setting::Install),
                    card("Switch user", "Ctrl+U".into(), Setting::SwitchUser),
                    card("Sign out", "Forget saved login".into(), Setting::SignOut),
                    card("Server", cfg.server.url.clone().unwrap_or_default(), Setting::ChangeServer),
                ],
            ),
        ];
        Content {
            source: Source::Settings,
            header: Some(HeaderContent {
                title: "Settings".into(),
                meta: format!("redjelly {}", env!("CARGO_PKG_VERSION")),
                info: "Enter changes a setting · saved immediately".into(),
                ..Default::default()
            }),
            rows,
            focus: (0, 0),
            paging: None,
        }
    }

    fn save_config(&self) {
        if let Err(e) = self.config.borrow().save(&self.config_path) {
            self.toast(format!("Couldn't save settings: {e}"));
        }
    }

    fn refresh_settings(&self) {
        if self.top_source() == Some(Source::Settings) {
            self.present(self.settings_content(), Placement::ReplaceTop);
        }
    }

    pub fn activate_setting(&self, s: Setting) {
        use Setting::*;
        match s {
            Languages => {
                let cur = self.config.borrow().languages.preferred.join(", ");
                return self.open_prompt(
                    s,
                    "Preferred languages",
                    "Most preferred first, comma separated (e.g. nl, en). Used for audio, subtitles and AI subtitles.",
                    &cur,
                    false,
                );
            }
            SubFont => {
                let cur = self.config.borrow().subtitles.font.clone();
                return self.open_prompt(s, "Subtitle font", "Any installed font name, e.g. Segoe UI Semibold.", &cur, false);
            }
            SubdlKey => {
                return self.open_prompt(s, "Subdl API key", "From your account page on subdl.com (free).", "", false);
            }
            OsKey => {
                return self.open_prompt(
                    s,
                    "OpenSubtitles API key",
                    "From opensubtitles.com → your profile → API consumers.",
                    "",
                    false,
                );
            }
            OsUser => {
                let cur = self.config.borrow().providers.opensubtitles_username.clone().unwrap_or_default();
                return self.open_prompt(s, "OpenSubtitles username", "", &cur, false);
            }
            OsPassword => {
                if self.config.borrow().providers.opensubtitles_username.is_none() {
                    return self.toast("Set the OpenSubtitles username first");
                }
                return self.open_prompt(s, "OpenSubtitles password", "Stored in Windows Credential Manager.", "", true);
            }
            EditKeys => return self.open_keymap_editor(),
            ResetKeys => {
                *self.keymap.borrow_mut() = Keymap::defaults();
                self.save_keymap();
                return self.toast("All keys reset to defaults");
            }
            Install => return self.install_for_user(),
            SwitchUser => return self.switch_user(),
            SignOut => {
                let (url, user) = {
                    let cfg = self.config.borrow();
                    (cfg.server.url.clone(), cfg.server.last_user_id.clone())
                };
                if let (Some(url), Some(user)) = (url, user) {
                    self.tokens.delete(&url, &user);
                }
                self.config.borrow_mut().server.last_user_id = None;
                self.save_config();
                return self.switch_user();
            }
            ChangeServer => return self.show_login_form(0),
            _ => {}
        }

        {
            let mut cfg = self.config.borrow_mut();
            match s {
                PreferSdh => cfg.languages.prefer_sdh ^= true,
                Intro => cfg.playback.intro = next_segment(cfg.playback.intro),
                Recap => cfg.playback.recap = next_segment(cfg.playback.recap),
                Outro => cfg.playback.outro = next_segment(cfg.playback.outro),
                Preview => cfg.playback.preview = next_segment(cfg.playback.preview),
                Commercial => cfg.playback.commercial = next_segment(cfg.playback.commercial),
                Countdown => cfg.playback.next_episode_countdown = next_in(COUNTDOWNS, cfg.playback.next_episode_countdown),
                Hwdec => cfg.playback.hwdec = next_in(HWDEC, cfg.playback.hwdec.as_str()).into(),
                ToneMapping => {
                    let n = cfg.video.tone_mapping_presets.len().max(1);
                    cfg.video.tone_mapping = (cfg.video.tone_mapping + 1) % n;
                }
                SubAutoSearch => cfg.subtitles.auto_search ^= true,
                SubAutoSync => cfg.subtitles.auto_sync_non_hash ^= true,
                SubUpload => cfg.subtitles.upload_to_server ^= true,
                SubSize => cfg.subtitles.font_size = next_in(SUB_SIZES, cfg.subtitles.font_size),
                ProvJellyfin => cfg.providers.jellyfin_remote ^= true,
                ProvSubdl => cfg.providers.subdl ^= true,
                ProvOpenSubtitles => cfg.providers.opensubtitles ^= true,
                AiRuntime => cfg.ai.whisper_runtime = next_in(RUNTIMES, cfg.ai.whisper_runtime.as_str()).into(),
                AiModel => cfg.ai.whisper_model = next_in(MODELS, cfg.ai.whisper_model.as_str()).into(),
                AiGpu => cfg.ai.use_gpu ^= true,
                AiVad => cfg.ai.vad ^= true,
                UiScale => cfg.appearance.ui_scale = next_in(UI_SCALES, cfg.appearance.ui_scale),
                _ => {}
            }
        }
        self.save_config();
        self.apply_live(s);
        self.refresh_settings();
    }

    /// Applies a changed setting to the running player where possible.
    fn apply_live(&self, s: Setting) {
        match s {
            Setting::Hwdec => {
                let v = self.config.borrow().playback.hwdec.clone();
                let _ = self.player.mpv().set_property("hwdec", v.as_str());
            }
            Setting::ToneMapping => self.apply_tone_mapping(),
            Setting::SubSize | Setting::SubFont => {
                let _ = self.player.apply_subtitle_style(&self.config.borrow());
            }
            Setting::ProvOpenSubtitles | Setting::OsKey | Setting::OsUser | Setting::OsPassword => {
                *self.opensubs.borrow_mut() = None;
            }
            Setting::UiScale => self.toast("UI size changes after restarting redjelly"),
            _ => {}
        }
    }

    // ---- Prompt ----------------------------------------------------------------

    fn open_prompt(&self, s: Setting, title: &str, description: &str, value: &str, secret: bool) {
        *self.prompt.borrow_mut() = Some(PromptKind::Setting(s));
        let ui = self.ui();
        ui.set_prompt_title(title.into());
        ui.set_prompt_description(description.into());
        ui.set_prompt_secret(secret);
        ui.set_prompt_value(value.into());
        // Re-create the view so its init focuses the field.
        ui.set_screen(0);
        ui.set_screen(SCREEN_PROMPT);
    }

    pub fn prompt_cancel(&self) {
        *self.prompt.borrow_mut() = None;
        self.show_top();
    }

    pub fn prompt_accepted(&self) {
        let Some(PromptKind::Setting(s)) = self.prompt.borrow_mut().take() else { return };
        let value = self.ui().get_prompt_value().trim().to_owned();
        let opt = |v: &str| (!v.is_empty()).then(|| v.to_owned());
        {
            let mut cfg = self.config.borrow_mut();
            match s {
                Setting::Languages => {
                    let langs: Vec<String> = value.split([',', ' ']).filter_map(Lang::parse).map(|l| l.to_string()).collect();
                    if langs.is_empty() {
                        drop(cfg);
                        self.toast("No valid language codes (use e.g. nl, en)");
                        return self.show_top();
                    }
                    cfg.languages.preferred = langs;
                }
                Setting::SubFont if !value.is_empty() => cfg.subtitles.font = value.clone(),
                Setting::SubdlKey => cfg.providers.subdl_api_key = opt(&value),
                Setting::OsKey => cfg.providers.opensubtitles_api_key = opt(&value),
                Setting::OsUser => cfg.providers.opensubtitles_username = opt(&value),
                Setting::OsPassword => {
                    if let Some(user) = cfg.providers.opensubtitles_username.clone() {
                        self.tokens.set_secret(&crate::secrets::provider_account("opensubtitles", &user), &value);
                    }
                }
                _ => {}
            }
        }
        self.save_config();
        self.apply_live(s);
        self.show_top();
        self.refresh_settings();
        self.toast("Saved");
    }

    // ---- Keymap editor ------------------------------------------------------------

    fn open_keymap_editor(&self) {
        *self.panel.borrow_mut() = Some(Panel { kind: PanelKind::Keymap, ..Default::default() });
        let ui = self.ui();
        ui.set_panel_title("Key bindings".into());
        ui.set_panel_visible(true);
        self.refresh_keymap_panel();
        self.panel_move(0);
    }

    pub fn refresh_keymap_panel(&self) {
        let defaults = Keymap::defaults();
        let km = self.keymap.borrow();
        let capturing = self.panel.borrow().as_ref().and_then(|p| p.capturing);
        let mut entries: Vec<(PanelItem, PanelAction)> = Vec::new();
        for (ctx, title) in [(Context::Player, "PLAYER"), (Context::Browse, "BROWSING"), (Context::Global, "EVERYWHERE")] {
            entries.push((PanelItem { title: title.into(), header: true, ..Default::default() }, PanelAction::None));
            // Every action bound by default or by the user in this context.
            let mut actions: Vec<Action> = Vec::new();
            for (_, a) in defaults.bindings(ctx).into_iter().chain(km.bindings(ctx)) {
                if !actions.contains(&a) {
                    actions.push(a);
                }
            }
            actions.sort_by_key(|a| a.to_string());
            for a in actions {
                let keys: Vec<String> = km.chords_for(ctx, a).iter().map(KeyChord::to_string).collect();
                let is_capturing = capturing == Some((ctx, a));
                entries.push((
                    PanelItem {
                        title: a.to_string().into(),
                        detail: if keys.is_empty() { "(no key)".into() } else { keys.join("   ").into() },
                        badge: if is_capturing { "PRESS A KEY".into() } else { SharedString::new() },
                        ..Default::default()
                    },
                    PanelAction::KeyBinding(ctx, a),
                ));
            }
        }
        drop(km);
        let ui = self.ui();
        ui.set_panel_status(
            match capturing {
                Some((_, a)) => format!("Press the new key or controller button for \"{a}\"\nEsc cancels · Backspace removes all its keys"),
                None => "Enter on an action to add a key (keyboard or controller)".to_owned(),
            }
            .into(),
        );
        ui.set_panel_items(ModelRc::new(VecModel::from(entries.iter().map(|(i, _)| i.clone()).collect::<Vec<_>>())));
        if let Some(p) = self.panel.borrow_mut().as_mut() {
            p.entries = entries;
        }
    }

    pub fn start_capture(&self, ctx: Context, action: Action) {
        if let Some(p) = self.panel.borrow_mut().as_mut() {
            p.capturing = Some((ctx, action));
        }
        self.refresh_keymap_panel();
    }

    /// Handles the key pressed while capturing. Returns false when not capturing.
    pub fn capture_key(&self, chord: &KeyChord) -> bool {
        let Some((ctx, action)) = self.panel.borrow().as_ref().and_then(|p| p.capturing) else { return false };
        if let Some(p) = self.panel.borrow_mut().as_mut() {
            p.capturing = None;
        }
        match chord.key.as_str() {
            "Esc" if !chord.ctrl && !chord.alt && !chord.shift => {}
            "Backspace" if !chord.ctrl && !chord.alt && !chord.shift => {
                let mut km = self.keymap.borrow_mut();
                for c in km.chords_for(ctx, action) {
                    km.unbind(ctx, &c);
                }
                drop(km);
                self.save_keymap();
                self.toast(format!("Removed all keys for \"{action}\""));
            }
            _ => {
                let previous = self.keymap.borrow().lookup(ctx, chord).filter(|a| *a != action);
                self.keymap.borrow_mut().set(ctx, chord.clone(), action);
                self.save_keymap();
                self.toast(match previous {
                    Some(old) => format!("{chord} → {action} (was {old})"),
                    None => format!("{chord} → {action}"),
                });
            }
        }
        self.refresh_keymap_panel();
        true
    }

    fn save_keymap(&self) {
        let text = self.keymap.borrow().to_user_toml();
        if let Some(dir) = self.keymap_path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = std::fs::write(&self.keymap_path, text) {
            self.toast(format!("Couldn't save keys: {e}"));
        }
    }

    // ---- Install -------------------------------------------------------------------

    /// Copies the app to %LOCALAPPDATA%\Programs\redjelly and adds a Start
    /// menu shortcut (no admin rights needed).
    fn install_for_user(&self) {
        #[cfg(not(windows))]
        self.toast("Installing is only for Windows");
        #[cfg(windows)]
        {
            self.toast("Installing…");
            self.spawn_ui(async { tokio::task::spawn_blocking(install_windows).await? }, |app, r| match r {
                Ok(dir) => app.toast(format!("Installed to {} · start it from the Start menu", dir.display())),
                Err(e) => app.toast(format!("Install failed: {e:#}")),
            });
        }
    }
}

#[cfg(windows)]
fn install_windows() -> anyhow::Result<std::path::PathBuf> {
    use std::os::windows::process::CommandExt;
    let exe = std::env::current_exe()?;
    let src = exe.parent().ok_or_else(|| anyhow::anyhow!("no exe dir"))?.to_path_buf();
    let local = std::env::var("LOCALAPPDATA")?;
    let dest = std::path::PathBuf::from(local).join("Programs").join("redjelly");
    if src != dest {
        std::fs::create_dir_all(&dest)?;
        for entry in std::fs::read_dir(&src)? {
            let p = entry?.path();
            if p.is_file() {
                std::fs::copy(&p, dest.join(p.file_name().expect("file has a name")))?;
            }
        }
    }
    let target = dest.join("redjelly.exe");
    let quote = |p: &std::path::Path| p.to_string_lossy().replace('\'', "''");
    let script = format!(
        "$s=(New-Object -ComObject WScript.Shell).CreateShortcut([Environment]::GetFolderPath('Programs')+'\\redjelly.lnk');\
         $s.TargetPath='{}';$s.WorkingDirectory='{}';$s.Description='redjelly Jellyfin client';$s.Save()",
        quote(&target),
        quote(&dest)
    );
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let status = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", &script])
        .creation_flags(CREATE_NO_WINDOW)
        .status()?;
    anyhow::ensure!(status.success(), "creating the Start menu shortcut failed");
    Ok(dest)
}
