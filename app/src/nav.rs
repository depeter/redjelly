//! Screen stack, focus and scrolling for every browse-type screen.
//!
//! Each screen keeps its own models, focus and per-row column, so Back
//! restores exactly where you were (SPEC §4.1).

use std::rc::Rc;

use rj_jellyfin::Client;
use slint::{ComponentHandle, Model, ModelRc, Rgb8Pixel, SharedPixelBuffer, VecModel};

use crate::layout;
use crate::screens::{self, CardContent, Content, Paging, Source, Target};
use crate::{App, Card, Header, Row, SCREEN_BROWSE};

pub struct Screen {
    pub source: Source,
    header: Header,
    header_visible: bool,
    rows: Rc<VecModel<Row>>,
    cards: Vec<Rc<VecModel<Card>>>,
    targets: Vec<Vec<Target>>,
    /// (style, has title) per row, for scroll maths.
    shapes: Vec<(i32, bool)>,
    /// Remembered focused column per row.
    cols: Vec<usize>,
    focus_row: usize,
    generation: u64,
    paging: Option<Paging>,
    loading_more: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Placement {
    Push,
    /// Reload of the current screen: keeps focus.
    ReplaceTop,
    /// New root (after login, switching user).
    Reset,
}

#[derive(Debug, Clone, Copy)]
enum Slot {
    Card(usize, usize),
    Backdrop,
}

fn to_card(c: &CardContent) -> Card {
    Card {
        title: c.title.as_str().into(),
        subtitle: c.subtitle.as_str().into(),
        image: Default::default(),
        has_image: false,
        progress: c.progress,
        played: c.played,
    }
}

impl App {
    fn window_width(&self) -> f32 {
        let ui = self.ui();
        let w = ui.window();
        w.size().width as f32 / w.scale_factor()
    }

    /// A client for loading screens: the signed-in one, or an anonymous one
    /// for the user picker.
    pub fn browse_client(&self) -> Option<Client> {
        if let Some(c) = self.client.borrow().clone() {
            return Some(c);
        }
        let url = self.config.borrow().server.url.clone()?;
        Client::new(&url, self.client_info()).ok()
    }

    pub fn top_source(&self) -> Option<Source> {
        self.nav.borrow().last().map(|s| s.source.clone())
    }

    /// Loads a screen and shows it once its content arrives.
    pub fn open(&self, source: Source, placement: Placement) {
        match source {
            Source::Jobs => return self.present(self.jobs_content(), placement),
            Source::Settings => return self.present(self.settings_content(), placement),
            _ => {}
        }
        let Some(client) = self.browse_client() else {
            self.show_login_form(0);
            return;
        };
        if placement == Placement::Push {
            if self.busy.get() {
                return;
            }
            self.busy.set(true);
            self.toast("Loading…");
        }
        let columns = layout::grid_columns(self.window_width());
        let prefs = self.config.borrow().track_preferences().languages;
        let url = self.config.borrow().server.url.clone().unwrap_or_default();
        let src = source.clone();
        self.spawn_ui(
            async move {
                match &src {
                    Source::Users => screens::users(&client, &url).await,
                    Source::Home => screens::home(&client).await,
                    Source::Library { parent_id, title, collection_type } => {
                        screens::library(&client, parent_id, title, collection_type.as_deref(), columns).await
                    }
                    Source::Series { id } => screens::series(&client, id).await,
                    Source::Detail { id } => screens::detail(&client, id, &prefs).await,
                    Source::Search { term } => screens::search(&client, term).await,
                    Source::Jobs | Source::Settings => unreachable!("built locally"),
                }
            },
            move |app, result| {
                if placement == Placement::Push {
                    app.busy.set(false);
                    app.clear_toast();
                }
                match result {
                    Ok(content) => app.present(content, placement),
                    Err(e) => {
                        tracing::warn!("loading {source:?}: {e:#}");
                        if source == Source::Users && app.nav.borrow().is_empty() {
                            app.show_login_form(0);
                            app.ui().set_status(format!("Can't reach the server: {e:#}").into());
                        } else {
                            app.toast(format!("Couldn't load: {e:#}"));
                        }
                    }
                }
            },
        );
    }

    pub fn present(&self, content: Content, placement: Placement) {
        if placement == Placement::ReplaceTop {
            // Drop stale results (e.g. an older search that finished late).
            if self.top_source().as_ref() != Some(&content.source) {
                return;
            }
        }
        let mut screen = self.build_screen(content);
        {
            let mut nav = self.nav.borrow_mut();
            match placement {
                Placement::Push => nav.push(screen),
                Placement::Reset => {
                    nav.clear();
                    nav.push(screen);
                }
                Placement::ReplaceTop => {
                    if let Some(old) = nav.pop() {
                        screen.focus_row = old.focus_row.min(screen.cards.len().saturating_sub(1));
                        for (i, c) in old.cols.iter().enumerate() {
                            if let Some(slot) = screen.cols.get_mut(i) {
                                *slot = *c;
                            }
                        }
                        // Keep already-loaded library pages' worth of position.
                        if screen.paging.is_some() && old.focus_row >= screen.cards.len() {
                            screen.focus_row = screen.cards.len().saturating_sub(1);
                        }
                    }
                    nav.push(screen);
                }
            }
        }
        if self.ui().get_screen() == SCREEN_BROWSE || placement != Placement::ReplaceTop {
            self.show_top();
        }
    }

    fn build_screen(&self, content: Content) -> Screen {
        let generation = self.next_gen.get() + 1;
        self.next_gen.set(generation);

        let mut cards = Vec::new();
        let mut targets = Vec::new();
        let mut shapes = Vec::new();
        let mut cols = Vec::new();
        let rows_model = Rc::new(VecModel::default());
        for (r, row) in content.rows.into_iter().enumerate() {
            let model = Rc::new(VecModel::from(row.cards.iter().map(to_card).collect::<Vec<_>>()));
            for (c, card) in row.cards.iter().enumerate() {
                if let Some(src) = &card.image {
                    self.load_image(generation, Slot::Card(r, c), src.clone());
                }
            }
            rows_model.push(Row {
                title: row.title.as_str().into(),
                style: row.style,
                cards: ModelRc::from(model.clone()),
                scroll: 0,
            });
            shapes.push((row.style, !row.title.is_empty()));
            cols.push(row.start.min(row.cards.len().saturating_sub(1)));
            targets.push(row.cards.into_iter().map(|c| c.target).collect());
            cards.push(model);
        }

        let (header, header_visible) = match content.header {
            Some(h) => {
                if let Some(src) = h.backdrop.clone() {
                    self.load_image(generation, Slot::Backdrop, src);
                }
                (
                    Header {
                        title: h.title.into(),
                        meta: h.meta.into(),
                        info: h.info.into(),
                        overview: h.overview.into(),
                        backdrop: Default::default(),
                        has_backdrop: false,
                    },
                    true,
                )
            }
            None => (Header::default(), false),
        };
        let (fr, fc) = content.focus;
        if fc > 0
            && let Some(c) = cols.get_mut(fr) {
                *c = fc;
            }
        Screen {
            source: content.source,
            header,
            header_visible,
            rows: rows_model,
            cards,
            targets,
            shapes,
            cols,
            focus_row: fr,
            generation,
            paging: content.paging,
            loading_more: false,
        }
    }

    fn load_image(&self, generation: u64, slot: Slot, src: crate::images::ImageSrc) {
        let loader = self.images.clone();
        self.spawn_ui(async move { loader.load(&src).await }, move |app, result| match result {
            Ok(buf) => app.set_image(generation, slot, buf),
            Err(e) => tracing::debug!("image: {e:#}"),
        });
    }

    fn set_image(&self, generation: u64, slot: Slot, buf: SharedPixelBuffer<Rgb8Pixel>) {
        let mut nav = self.nav.borrow_mut();
        let top_gen = nav.last().map(|s| s.generation);
        let Some(screen) = nav.iter_mut().find(|s| s.generation == generation) else { return };
        let image = slint::Image::from_rgb8(buf);
        match slot {
            Slot::Card(r, c) => {
                let Some(model) = screen.cards.get(r) else { return };
                if let Some(mut card) = model.row_data(c) {
                    card.image = image;
                    card.has_image = true;
                    model.set_row_data(c, card);
                }
            }
            Slot::Backdrop => {
                screen.header.backdrop = image;
                screen.header.has_backdrop = true;
                if top_gen == Some(generation) {
                    self.ui().set_header(screen.header.clone());
                }
            }
        }
    }

    pub fn show_top(&self) {
        let ui = self.ui();
        {
            let nav = self.nav.borrow();
            let Some(s) = nav.last() else { return };
            ui.set_header(s.header.clone());
            ui.set_header_visible(s.header_visible);
            ui.set_rows(ModelRc::from(s.rows.clone()));
        }
        ui.set_screen(SCREEN_BROWSE);
        self.apply_focus();
        ui.invoke_focus_keys();
    }

    /// Pushes the top screen's focus to the UI, scrolling as needed.
    fn apply_focus(&self) {
        let width = self.window_width();
        let ui = self.ui();
        let mut nav = self.nav.borrow_mut();
        let Some(s) = nav.last_mut() else { return };
        if s.cards.is_empty() {
            ui.set_focus_row(-1);
            ui.set_scroll_y(0.0);
            return;
        }
        s.focus_row = s.focus_row.min(s.cards.len() - 1);
        let r = s.focus_row;
        let len = s.cards[r].row_count();
        s.cols[r] = s.cols[r].min(len.saturating_sub(1));
        let c = s.cols[r];

        let style = s.shapes[r].0;
        let scroll = layout::row_scroll(c, layout::visible_cards(width, style), len) as i32;
        if let Some(mut row) = s.rows.row_data(r)
            && row.scroll != scroll {
                row.scroll = scroll;
                s.rows.set_row_data(r, row);
            }
        ui.set_focus_row(r as i32);
        ui.set_focus_col(c as i32);
        ui.set_scroll_y(layout::scroll_y(&s.shapes, r));
        drop(nav);
        self.maybe_load_more();
    }

    pub fn move_focus(&self, dr: i32, dc: i32) {
        {
            let mut nav = self.nav.borrow_mut();
            let Some(s) = nav.last_mut() else { return };
            if s.cards.is_empty() {
                return;
            }
            let r = s.focus_row;
            if dr != 0 {
                s.focus_row = (r as i32 + dr).clamp(0, s.cards.len() as i32 - 1) as usize;
                // Grids keep the column; other rows return to where you were.
                if s.paging.is_some() || matches!(s.source, Source::Library { .. }) {
                    let col = s.cols[r];
                    s.cols[s.focus_row] = col;
                }
            } else {
                let len = s.cards[r].row_count() as i32;
                s.cols[r] = (s.cols[r] as i32 + dc).clamp(0, (len - 1).max(0)) as usize;
            }
        }
        self.apply_focus();
    }

    pub fn focus_card(&self, r: usize, c: usize) {
        if let Some(s) = self.nav.borrow_mut().last_mut()
            && r < s.cards.len() {
                s.focus_row = r;
                s.cols[r] = c;
            }
        self.apply_focus();
    }

    pub fn activate_focused(&self) {
        let target = {
            let nav = self.nav.borrow();
            let Some(s) = nav.last() else { return };
            let r = s.focus_row;
            s.targets.get(r).and_then(|t| t.get(s.cols[r])).cloned()
        };
        let Some(target) = target else { return };
        match target {
            Target::None => {}
            Target::Open(src) => self.open(src, Placement::Push),
            Target::Play { id, from_start } => self.play_item(id, from_start),
            Target::SetPlayed { id, played } => self.set_played(id, played),
            Target::User { id, name, has_password } => self.select_user(id, name, has_password),
            Target::QuickConnect => self.start_quick_connect(),
            Target::ManualLogin => self.show_login_form(1),
            Target::ChangeServer => self.show_login_form(0),
            Target::AiItem { id } => self.queue_ai_for_item(id),
            Target::AiSeries { id } => self.queue_ai_for_series(id),
            Target::Job(id) => self.activate_job(id),
            Target::Setting(s) => self.activate_setting(s),
            Target::SwitchUser => self.switch_user(),
        }
    }

    fn set_played(&self, id: String, played: bool) {
        let Some(client) = self.client.borrow().clone() else { return };
        self.spawn_ui(async move { client.set_played(&id, played).await }, move |app, r| match r {
            Ok(()) => {
                app.toast(if played { "Marked as watched" } else { "Marked as unwatched" });
                app.reload_top();
            }
            Err(e) => app.toast(format!("Couldn't update: {e}")),
        });
    }

    pub fn back(&self) {
        let popped = {
            let mut nav = self.nav.borrow_mut();
            (nav.len() > 1).then(|| nav.pop())
        };
        if popped.is_some() {
            self.show_top();
        }
    }

    pub fn go_home(&self) {
        let is_home_root = self.nav.borrow().first().is_some_and(|s| s.source == Source::Home);
        if is_home_root {
            self.nav.borrow_mut().truncate(1);
            self.show_top();
            self.reload_top();
        }
    }

    pub fn reload_top(&self) {
        if let Some(src) = self.top_source() {
            self.open(src, Placement::ReplaceTop);
        }
    }

    /// Search screen typing: `Some(c)` appends, `None` deletes (or leaves
    /// search when already empty).
    pub fn search_edit(&self, ch: Option<char>) {
        let term = {
            let mut nav = self.nav.borrow_mut();
            let Some(s) = nav.last_mut() else { return };
            let Source::Search { term } = &mut s.source else { return };
            match ch {
                Some(c) => term.push(c),
                None if term.is_empty() => {
                    drop(nav);
                    self.back();
                    return;
                }
                None => {
                    term.pop();
                }
            }
            s.header.meta = format!("{term}_").into();
            term.clone()
        };
        let ui = self.ui();
        let mut h = ui.get_header();
        h.meta = format!("{term}_").into();
        ui.set_header(h);
        // Debounce: search 300 ms after the last keystroke.
        self.search_timer.start(slint::TimerMode::SingleShot, std::time::Duration::from_millis(300), || {
            crate::with_app(|app| app.reload_top())
        });
    }

    fn maybe_load_more(&self) {
        let (paging, generation) = {
            let mut nav = self.nav.borrow_mut();
            let Some(s) = nav.last_mut() else { return };
            let Some(p) = &s.paging else { return };
            if s.loading_more || s.focus_row + 3 < s.cards.len() {
                return;
            }
            s.loading_more = true;
            (p.clone(), s.generation)
        };
        let Some(client) = self.client.borrow().clone() else { return };
        let p = paging.clone();
        self.spawn_ui(async move { screens::library_page(&client, &p).await }, move |app, result| match result {
            Ok((cards, total)) => app.append_grid(generation, cards, total),
            Err(e) => {
                app.toast(format!("Couldn't load more: {e:#}"));
                if let Some(s) = app.nav.borrow_mut().iter_mut().find(|s| s.generation == generation) {
                    s.loading_more = false;
                }
            }
        });
    }

    fn append_grid(&self, generation: u64, cards: Vec<CardContent>, total: u32) {
        let mut loads = Vec::new();
        {
            let mut nav = self.nav.borrow_mut();
            let Some(s) = nav.iter_mut().find(|s| s.generation == generation) else { return };
            let Some(paging) = s.paging.as_mut() else { return };
            paging.loaded += cards.len() as u32;
            paging.total = total;
            let columns = paging.columns.max(1);
            let done = paging.loaded >= total || cards.is_empty();
            for card in cards {
                let full = s.cards.last().is_none_or(|m| m.row_count() >= columns);
                if full {
                    let model = Rc::new(VecModel::default());
                    s.rows.push(Row {
                        title: Default::default(),
                        style: layout::STYLE_POSTER,
                        cards: ModelRc::from(model.clone()),
                        scroll: 0,
                    });
                    s.cards.push(model);
                    s.targets.push(Vec::new());
                    s.shapes.push((layout::STYLE_POSTER, false));
                    s.cols.push(0);
                }
                let r = s.cards.len() - 1;
                let c = s.cards[r].row_count();
                s.cards[r].push(to_card(&card));
                if let Some(src) = card.image.clone() {
                    loads.push((r, c, src));
                }
                s.targets[r].push(card.target);
            }
            if done {
                s.paging = None;
            }
            s.loading_more = false;
        }
        for (r, c, src) in loads {
            self.load_image(generation, Slot::Card(r, c), src);
        }
    }
}
