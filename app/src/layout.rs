//! Layout numbers mirrored from ui/app.slint, used to compute scrolling so
//! the focused card is always on screen.

pub const SIDE_PAD: f32 = 48.0;
pub const GAP: f32 = 16.0;
const ROW_SPACING: f32 = 24.0;
const ROW_TITLE: f32 = 26.0 + 10.0;

pub const STYLE_POSTER: i32 = 0;
pub const STYLE_WIDE: i32 = 1;
pub const STYLE_BUTTON: i32 = 2;
/// Settings: label + value.
pub const STYLE_SETTING: i32 = 3;

pub fn card_width(style: i32) -> f32 {
    match style {
        STYLE_WIDE => 288.0,
        STYLE_BUTTON => 220.0,
        STYLE_SETTING => 300.0,
        _ => 160.0,
    }
}

pub fn row_height(style: i32, has_title: bool) -> f32 {
    let body = match style {
        STYLE_BUTTON => 56.0,
        STYLE_SETTING => 72.0,
        STYLE_WIDE => 162.0 + 52.0,
        _ => 240.0 + 52.0,
    };
    body + if has_title { ROW_TITLE } else { 0.0 }
}

/// Vertical offset that puts row `focus` at the top of the rows area.
pub fn scroll_y(rows: &[(i32, bool)], focus: usize) -> f32 {
    let top: f32 = rows.iter().take(focus).map(|&(s, t)| row_height(s, t) + ROW_SPACING).sum();
    top
}

/// Whole cards that fit in a row at this window width.
pub fn visible_cards(window_width: f32, style: i32) -> usize {
    (((window_width - SIDE_PAD + GAP) / (card_width(style) + GAP)).floor() as usize).max(1)
}

/// Columns for library grids (also keeps a right margin).
pub fn grid_columns(window_width: f32) -> usize {
    visible_cards(window_width - SIDE_PAD, STYLE_POSTER)
}

/// Horizontal scroll (in cards) keeping the focused card one card away
/// from the right edge when possible.
pub fn row_scroll(focus: usize, visible: usize, len: usize) -> usize {
    if len <= visible || focus + 2 <= visible {
        return 0;
    }
    (focus + 2 - visible).min(len - visible)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrolls_row_only_near_the_edge() {
        assert_eq!(row_scroll(0, 7, 20), 0);
        assert_eq!(row_scroll(5, 7, 20), 0);
        assert_eq!(row_scroll(6, 7, 20), 1);
        assert_eq!(row_scroll(19, 7, 20), 13);
        assert_eq!(row_scroll(4, 7, 5), 0);
    }

    #[test]
    fn grid_fits_window() {
        // 1280 wide: 48 left pad, 48 right margin → 7 posters of 160+16.
        assert_eq!(grid_columns(1280.0), 6);
        assert_eq!(visible_cards(1280.0, STYLE_POSTER), 7);
    }

    #[test]
    fn first_row_never_scrolls() {
        let rows = [(STYLE_WIDE, true), (STYLE_POSTER, true)];
        assert_eq!(scroll_y(&rows, 0), 0.0);
        assert!(scroll_y(&rows, 1) > 0.0);
    }
}
