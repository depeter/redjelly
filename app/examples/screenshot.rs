//! Renders the UI offscreen with fake data and writes PNGs, so layouts can be
//! checked without a display:  cargo run -p redjelly --example screenshot -- <out-dir>

use std::rc::Rc;

use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType, Rgb565Pixel};
use slint::platform::{Platform, WindowAdapter};
use slint::{ComponentHandle, ModelRc, PhysicalSize, Rgb8Pixel, SharedPixelBuffer, VecModel};

slint::include_modules!();

struct Offscreen(Rc<MinimalSoftwareWindow>);

impl Platform for Offscreen {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}

const W: u32 = 1280;
const H: u32 = 720;

fn fake_image(w: u32, h: u32, seed: u8) -> slint::Image {
    let mut buf = SharedPixelBuffer::<Rgb8Pixel>::new(w, h);
    for (i, px) in buf.make_mut_slice().iter_mut().enumerate() {
        let (x, y) = (i as u32 % w, i as u32 / w);
        *px = Rgb8Pixel { r: (x * 255 / w) as u8 ^ seed, g: (y * 255 / h) as u8, b: seed.wrapping_mul(3) };
    }
    slint::Image::from_rgb8(buf)
}

fn card(title: &str, sub: &str, img: Option<slint::Image>, progress: f32, played: bool) -> Card {
    Card {
        title: title.into(),
        subtitle: sub.into(),
        has_image: img.is_some(),
        image: img.unwrap_or_default(),
        progress,
        played,
    }
}

fn row(title: &str, style: i32, cards: Vec<Card>) -> Row {
    Row { title: title.into(), style, cards: ModelRc::new(VecModel::from(cards)), scroll: 0 }
}

fn save(window: &MinimalSoftwareWindow, path: &str) {
    slint::platform::update_timers_and_animations();
    let mut buf = vec![Rgb565Pixel::default(); (W * H) as usize];
    window.request_redraw();
    window.draw_if_needed(|r| {
        r.render(&mut buf, W as usize);
    });
    let rgb: Vec<u8> = buf
        .iter()
        .flat_map(|p| {
            let v = p.0;
            let (r, g, b) = ((v >> 11) & 0x1f, (v >> 5) & 0x3f, v & 0x1f);
            [(r << 3 | r >> 2) as u8, (g << 2 | g >> 4) as u8, (b << 3 | b >> 2) as u8]
        })
        .collect();
    image::save_buffer(path, &rgb, W, H, image::ExtendedColorType::Rgb8).unwrap();
    println!("wrote {path}");
}

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| ".".into());
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Offscreen(window.clone()))).unwrap();
    let ui = AppWindow::new().unwrap();
    window.set_size(PhysicalSize::new(W, H));
    ui.show().unwrap();

    // Home
    let wide = |s| Some(fake_image(288, 162, s));
    let poster = |s| Some(fake_image(160, 240, s));
    let rows = vec![
        row("", 2, vec![
            card("Search · or just type", "", None, -1.0, false),
            card("Settings", "", None, -1.0, false),
            card("AI subtitle jobs", "", None, -1.0, false),
            card("Switch user", "", None, -1.0, false),
        ]),
        row("Continue watching", 1, vec![
            card("Fate", "Billy the Kid · S01E06", wide(10), 0.23, false),
            card("A very long episode title that should elide", "Some Show · S03E11", wide(40), 0.8, false),
            card("No image", "Show · S01E01", None, -1.0, false),
        ]),
        row("Latest in Movies", 0, (0..9).map(|i| card(&format!("Movie {i}"), "2024", poster(i * 20), -1.0, i % 3 == 0)).collect()),
        row("Latest in Shows", 0, (0..9).map(|i| card(&format!("Show {i}"), "2023", poster(i * 30), -1.0, false)).collect()),
    ];
    ui.set_rows(ModelRc::new(VecModel::from(rows)));
    ui.set_header_visible(false);
    ui.set_focus_row(1);
    ui.set_focus_col(0);
    ui.set_toast("Arrow keys to move · Enter to open · ? for all keys".into());
    save(&window, &format!("{out}/home.png"));

    // Detail
    ui.set_toast("".into());
    ui.set_header(Header {
        title: "The Long Night".into(),
        meta: "2024 · 2h 14m · 16 · ★ 7.4 · Drama, Thriller".into(),
        info: "Audio: English 5.1, Dutch 2.0 · Subtitles: English, Dutch".into(),
        overview: "A sprawling story about a family that has to survive a winter without end. When the power fails, old secrets surface and alliances shift in ways nobody could have predicted, and the night keeps getting longer.".into(),
        backdrop: fake_image(640, 360, 90),
        has_backdrop: true,
    });
    ui.set_header_visible(true);
    ui.set_rows(ModelRc::new(VecModel::from(vec![
        row("", 2, vec![card("Resume 1:02:13", "", None, -1.0, false), card("Play from start", "", None, -1.0, false), card("Mark watched", "", None, -1.0, false)]),
        row("More in Season 1", 1, (1..6).map(|i| card(&format!("Episode {i}"), &format!("S01E0{i}"), wide(i * 25), if i == 2 { 0.5 } else { -1.0 }, i == 1)).collect()),
    ])));
    save(&window, &format!("{out}/detail.png"));

    // Key help
    ui.set_keyhelp_visible(true);
    ui.set_keyhelp_title("Player keys".into());
    let keys: Vec<KeyHelp> = (0..40).map(|i| KeyHelp { key: format!("Ctrl+Key{i}").into(), action: format!("Action number {i}").into() }).collect();
    ui.set_keyhelp(ModelRc::new(VecModel::from(keys)));
    save(&window, &format!("{out}/keyhelp.png"));
    ui.set_keyhelp_visible(false);

    // Player overlay (video area is just the window background here)
    ui.set_screen(2);
    ui.set_player_title("Billy the Kid · S01E06 · Fate".into());
    ui.set_player_time("10:22 / 45:24".into());
    ui.set_player_progress(0.23);
    ui.set_player_paused(true);
    ui.set_player_skip("Skip intro".into());
    ui.set_player_stats("video   H.264 1920x1080\nhwdec   nvdec".into());
    save(&window, &format!("{out}/player.png"));

    // Subtitle panel
    ui.set_player_stats("".into());
    ui.set_player_skip("".into());
    let item = |t: &str, d: &str, b: &str, header: bool, dimmed: bool| PanelItem {
        title: t.into(), detail: d.into(), badge: b.into(), header, dimmed,
    };
    let items = vec![
        item("CURRENT", "", "", true, false),
        item("English · subrip", "Embedded · delay 0.0 s", "", false, false),
        item("Auto-sync current subtitle", "Align to another subtitle track", "", false, false),
        item("Generate Dutch subtitles with AI", "Coming in M3", "", false, true),
        item("DUTCH — 3 FOUND", "", "", true, false),
        item("Billy.the.Kid.S01E06.1080p.WEB.H264-SuccessfulCrab", "Subdl", "", false, false),
        item("Billy.the.Kid.S01E06.720p.HDTV.x264-SYNCOPY", "Jellyfin · 1.2k downloads", "", false, false),
        item("Billy The Kid S01E06 Fate", "OpenSubtitles · uses daily quota", "HASH", false, false),
        item("ENGLISH — 12 FOUND", "", "", true, false),
        item("Billy.the.Kid.S01E06.1080p.WEB.H264-SuccessfulCrab", "Subdl · hearing impaired", "SDH", false, false),
    ];
    ui.set_panel_items(ModelRc::new(VecModel::from(items)));
    ui.set_panel_title("Subtitles".into());
    ui.set_panel_status("Searched Jellyfin, Subdl, OpenSubtitles · 15 results".into());
    ui.set_panel_focus(5);
    ui.set_panel_visible(true);
    save(&window, &format!("{out}/panel.png"));
    ui.set_panel_visible(false);

    // User picker
    ui.set_screen(1);
    ui.set_header(Header { title: "Who's watching?".into(), meta: "https://jellyfin.example.com".into(), ..Default::default() });
    ui.set_header_visible(true);
    ui.set_rows(ModelRc::new(VecModel::from(vec![
        row("", 0, vec![card("aiden", "", None, -1.0, false), card("peter", "", None, -1.0, false)]),
        row("", 2, vec![card("Quick Connect", "", None, -1.0, false), card("Other user", "", None, -1.0, false), card("Change server", "", None, -1.0, false)]),
    ])));
    ui.set_focus_row(1);
    ui.set_focus_col(0);
    ui.set_scroll_y(0.0);
    save(&window, &format!("{out}/users.png"));

    // Library grid, scrolled to the third row
    ui.set_header(Header { title: "Movies".into(), meta: "412 items".into(), ..Default::default() });
    let grid: Vec<Row> = (0..6).map(|r| row("", 0, (0..6).map(|c| card(&format!("Movie {}", r * 6 + c), "2021", poster((r * 6 + c) as u8 * 7), -1.0, false)).collect())).collect();
    ui.set_rows(ModelRc::new(VecModel::from(grid)));
    ui.set_focus_row(2);
    ui.set_focus_col(3);
    ui.set_scroll_y(2.0 * (292.0 + 24.0) - 48.0);
    save(&window, &format!("{out}/library.png"));

    // Settings
    ui.set_header(Header { title: "Settings".into(), meta: "redjelly 0.1.0".into(), info: "Enter changes a setting · saved immediately".into(), ..Default::default() });
    let set = |l: &str, v: &str| card(l, v, None, -1.0, false);
    ui.set_rows(ModelRc::new(VecModel::from(vec![
        row("Languages", 3, vec![set("Preferred (audio & subtitles)", "nl, en"), set("Prefer SDH subtitles", "Off")]),
        row("Playback", 3, vec![set("Intro", "Show skip button"), set("Recap", "Skip automatically"), set("Credits", "Show skip button"), set("Next episode autoplay", "After 10 s countdown")]),
        row("Picture", 3, vec![set("HDR tone mapping (Ctrl+T)", "Projector · bright (200 nits)"), set("UI size", "150 % (after restart)")]),
    ])));
    ui.set_focus_row(1);
    ui.set_focus_col(1);
    ui.set_scroll_y(0.0);
    save(&window, &format!("{out}/settings.png"));

    // Keymap editor panel over settings
    let keys = vec![
        item("PLAYER", "", "", true, false),
        item("Play / pause", "K   Space", "", false, false),
        item("Seek -5 s", "Left", "PRESS A KEY", false, false),
        item("Next episode", "N   MediaNext", "", false, false),
    ];
    ui.set_panel_items(ModelRc::new(VecModel::from(keys)));
    ui.set_panel_title("Key bindings".into());
    ui.set_panel_status("Press the new key or controller button for \"Seek -5 s\"\nEsc cancels · Backspace removes all its keys".into());
    ui.set_panel_focus(2);
    ui.set_panel_visible(true);
    save(&window, &format!("{out}/keymap.png"));
    ui.set_panel_visible(false);

    // Next-episode countdown
    ui.set_screen(2);
    ui.set_player_skip("".into());
    ui.set_player_stats("".into());
    ui.set_player_paused(false);
    ui.set_player_osd(false);
    ui.set_player_next("Next: Billy the Kid · S01E07 · Reckoning · in 8 s\nEnter: play now · Esc: keep watching".into());
    save(&window, &format!("{out}/nextup.png"));
    ui.set_player_next("".into());

    // Prompt
    ui.set_screen(5);
    ui.set_prompt_title("Subdl API key".into());
    ui.set_prompt_description("From your account page on subdl.com (free).".into());
    ui.set_prompt_value("".into());
    save(&window, &format!("{out}/prompt.png"));

    // Quick connect
    ui.set_screen(4);
    ui.set_quick_connect_code("482913".into());
    save(&window, &format!("{out}/quickconnect.png"));
}
