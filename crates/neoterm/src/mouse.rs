use std::ops::Range;

use alacritty_terminal::index::Side;
use winit::event::Modifiers;

/// A mouse position resolved to a terminal cell.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CellHit {
    /// Grid line (viewport row minus display offset; negative = history).
    pub grid_line: i32,
    /// Viewport column.
    pub col: usize,
    /// Left/right half of the cell (selection anchor side).
    pub side: Side,
}

/// Map a physical-pixel cursor position to a terminal cell.
///
/// Geometry must match `Renderer::body_geometry` ((cell_w, line_h, left, top)
/// in physical pixels). Returns `None` above/left of the body or past the
/// last row; clamps the column so trailing-edge clicks still select.
/// Column mapping is approximate on rows containing wide chars.
#[allow(clippy::too_many_arguments)]
pub fn hit_test(
    x_px: f64,
    y_px: f64,
    cell_w: f32,
    line_h: f32,
    left: f32,
    top: f32,
    rows: usize,
    cols: usize,
    display_offset: usize,
) -> Option<CellHit> {
    if cell_w <= 0.0 || line_h <= 0.0 || rows == 0 || cols == 0 {
        return None;
    }
    let col_f = (x_px as f32 - left) / cell_w;
    let row_f = (y_px as f32 - top) / line_h;
    if col_f < 0.0 || row_f < 0.0 {
        return None;
    }
    let row = row_f as usize;
    if row >= rows {
        return None;
    }
    let col = (col_f as usize).min(cols - 1);
    let side = if col_f - col as f32 >= 0.5 {
        Side::Right
    } else {
        Side::Left
    };
    Some(CellHit {
        grid_line: row as i32 - display_offset as i32,
        col,
        side,
    })
}

pub const BTN_LEFT: u8 = 0;
/// Added to the button code for drag/motion reports.
pub const BTN_MOTION: u8 = 32;
pub const BTN_RELEASE: u8 = 3;
pub const BTN_WHEEL_UP: u8 = 64;
pub const BTN_WHEEL_DOWN: u8 = 65;
pub const BTN_MIDDLE: u8 = 1;

/// One tab-bar segment label. Single source of truth shared by the renderer
/// (drawing) and hit-testing, so clicks always land on what is drawn.
/// ASCII `x` (not ×): hit-test math counts 1 cell per char, and a
/// fallback-font × can render wider and drift every target right of it.
pub fn tab_label(index: usize, title: &str, is_active: bool) -> String {
    if is_active {
        format!(" [{}:{} x] ", index + 1, title)
    } else {
        format!("  {}:{} x  ", index + 1, title)
    }
}

/// Full tab-bar text (joined segment labels plus a `+` new-tab mark).
pub fn tab_bar_text(titles: &[String], active: usize) -> String {
    if titles.is_empty() {
        return " NeoTerm ".to_string();
    }
    let mut bar: String = titles
        .iter()
        .enumerate()
        .map(|(i, t)| tab_label(i, t, i == active))
        .collect();
    bar.push_str("  +");
    bar
}

/// Strip visibility (Ghostty `window-show-tab-bar` language): `auto`
/// shows the strip only when 2+ tabs exist; anything else honors
/// `always`/`never` literally.
pub fn strip_visible(tab_count: usize, mode: &str) -> bool {
    match mode {
        "never" => false,
        "always" => true,
        _ => tab_count > 1,
    }
}

/// X-ranges (physical px) of each tab segment for a monospace advance.
/// `advance` is layout units (see `Renderer::body_geometry`: physical cell
/// width is `advance * scale`); `left` is the bar's physical left edge.
pub fn tab_regions(
    titles: &[String],
    active: usize,
    advance: f32,
    left: f32,
    scale: f32,
) -> Vec<(f32, f32)> {
    let mut out = Vec::with_capacity(titles.len());
    let mut x = left;
    for (i, t) in titles.iter().enumerate() {
        let w = tab_label(i, t, i == active).chars().count() as f32 * advance * scale;
        out.push((x, x + w));
        x += w;
    }
    out
}

/// Index of the tab segment containing physical x, if any.
pub fn tab_hit(regions: &[(f32, f32)], x: f32) -> Option<usize> {
    regions
        .iter()
        .position(|(x0, x1)| x >= *x0 && x < *x1)
}

/// Index of the tab whose close mark (`x`) contains physical x, if any.
/// The close zone is the last 3 cells of the segment (the `x` plus a
/// 1-cell margin on each side), derived from the same labels/advance as
/// `tab_regions` so it cannot drift from what is drawn.
pub fn tab_close_hit(
    titles: &[String],
    active: usize,
    advance: f32,
    left: f32,
    scale: f32,
    x: f32,
) -> Option<usize> {
    let cell = advance * scale;
    let mut seg_x = left;
    for (i, t) in titles.iter().enumerate() {
        let n = tab_label(i, t, i == active).chars().count() as f32;
        let close_x0 = seg_x + (n - 3.0) * cell;
        let seg_x1 = seg_x + n * cell;
        if x >= close_x0 && x < seg_x1 {
            return Some(i);
        }
        seg_x = seg_x1;
    }
    None
}

/// Byte ranges over the joined `tab_bar_text` for styling: the active
/// segment's full range, every segment's close-mark (`x`) range, and the
/// trailing `+` new-tab mark range (empty when there are no tabs).
/// The `x` is always the third-to-last byte of its label (ASCII suffix),
/// so ranges stay on char boundaries for any title.
pub fn tab_span_ranges(
    titles: &[String],
    active: usize,
) -> (Range<usize>, Vec<Range<usize>>, Range<usize>) {
    let mut active_range = 0..0;
    let mut xs = Vec::with_capacity(titles.len());
    let mut off = 0;
    for (i, t) in titles.iter().enumerate() {
        let label = tab_label(i, t, i == active);
        let end = off + label.len();
        if i == active {
            active_range = off..end;
        }
        let x = end - 3;
        xs.push(x..x + 1);
        off = end;
    }
    // Trailing "  +" mark (3 bytes) when tabs exist.
    let plus = if titles.is_empty() {
        0..0
    } else {
        off..off + 3
    };
    (active_range, xs, plus)
}

/// Whether physical x hits the `+` new-tab mark: the 3 cells directly
/// past the last tab segment.
pub fn tab_new_hit(
    titles: &[String],
    active: usize,
    advance: f32,
    left: f32,
    scale: f32,
    x: f32,
) -> bool {
    if titles.is_empty() {
        return false;
    }
    let cell = advance * scale;
    let mut end = left;
    for (i, t) in titles.iter().enumerate() {
        end += tab_label(i, t, i == active).chars().count() as f32 * cell;
    }
    x >= end && x < end + 3.0 * cell
}

/// SGR (1006) mouse encoding: `ESC [ < Cb ; Cx ; Cy M|m` with 1-based
/// viewport coordinates. `pressed=false` renders the `m` (release) suffix.
pub fn encode_sgr(
    button: u8,
    col: usize,
    row: usize,
    mods: &Modifiers,
    pressed: bool,
) -> Vec<u8> {
    let st = mods.state();
    let mut cb = button as u16;
    if st.shift_key() {
        cb += 4;
    }
    if st.alt_key() {
        cb += 8;
    }
    if st.control_key() {
        cb += 16;
    }
    let m = if pressed { 'M' } else { 'm' };
    format!("\x1b[<{cb};{};{}{m}", col + 1, row + 1).into_bytes()
}

/// Scrollbar geometry in physical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScrollBar {
    /// Track rect: (x0, top, width, height).
    pub track_x: f32,
    pub track_top: f32,
    pub track_w: f32,
    pub track_h: f32,
    /// Thumb rect within the track: y offset from track top + height.
    /// `None` when there is no scrollback history (bar hidden).
    pub thumb: Option<(f32, f32)>,
}

/// Minimum thumb height so it stays grabbable with huge histories.
pub const MIN_THUMB_H: f32 = 24.0;
/// Scrollbar width + right margin, physical pixels at scale 1.
pub const BAR_W: f32 = 10.0;
pub const BAR_MARGIN: f32 = 4.0;

/// Compute scrollbar geometry from scroll state.
///
/// `track_top`/`track_h` describe the full-height gutter; `history` is total
/// scrollback lines, `offset` lines scrolled up (0 = live view at bottom),
/// `rows` the viewport height. The thumb sits at the *bottom* when live —
/// inverted vs text editors, like all terminals.
pub fn scrollbar(
    track_top: f32,
    track_h: f32,
    scale: f32,
    history: usize,
    offset: usize,
    rows: usize,
) -> ScrollBar {
    let bar = ScrollBar {
        track_x: 0.0, // filled in by the caller (needs surface width)
        track_top,
        track_w: BAR_W * scale,
        track_h,
        thumb: None,
    };
    if history == 0 || track_h <= 0.0 || rows == 0 {
        return bar;
    }
    let total = (history + rows) as f32;
    let thumb_h = (track_h * rows as f32 / total).max(MIN_THUMB_H * scale);
    let thumb_h = thumb_h.min(track_h);
    // offset 0 (live) -> bottom; offset == history -> top.
    let travel = (track_h - thumb_h).max(0.0);
    let frac = (offset.min(history) as f32) / history as f32;
    let thumb_y = track_top + travel * (1.0 - frac);
    ScrollBar {
        thumb: Some((thumb_y, thumb_h)),
        ..bar
    }
}

/// Map a thumb-drag position to a scroll offset.
///
/// `grab_delta` is the distance from the thumb top where it was grabbed
/// (recorded at press so the thumb doesn't jump). `y` is the current cursor
/// y in physical pixels. Returns lines scrolled up, clamped to history.
pub fn drag_offset(
    y: f32,
    grab_delta: f32,
    track_top: f32,
    track_h: f32,
    thumb_h: f32,
    history: usize,
) -> usize {
    if history == 0 {
        return 0;
    }
    let travel = (track_h - thumb_h).max(0.0);
    if travel <= 0.0 {
        return 0;
    }
    // Thumb top measured from track top, live (offset 0) at the bottom.
    let thumb_top = (y - grab_delta - track_top).clamp(0.0, travel);
    let frac = 1.0 - thumb_top / travel;
    ((frac * history as f32).round() as usize).min(history)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_without_history() {
        let bar = scrollbar(0.0, 500.0, 1.0, 0, 0, 24);
        assert_eq!(bar.thumb, None);
    }

    #[test]
    fn live_view_thumb_at_bottom() {
        let track_h = 500.0;
        let bar = scrollbar(0.0, track_h, 1.0, 100, 0, 24);
        let (y, h) = bar.thumb.expect("thumb");
        assert!((y + h - track_h).abs() < 0.01, "y={y} h={h}");
    }

    #[test]
    fn max_offset_thumb_at_top() {
        let bar = scrollbar(10.0, 500.0, 1.0, 100, 100, 24);
        let (y, _) = bar.thumb.expect("thumb");
        assert!((y - 10.0).abs() < 0.01, "y={y}");
    }

    #[test]
    fn thumb_proportional_and_clamped() {
        let track_h = 500.0;
        let rows = 24;
        let history = 100;
        let bar = scrollbar(0.0, track_h, 1.0, history, 50, rows);
        let (_, h) = bar.thumb.expect("thumb");
        let expect = (track_h * rows as f32 / (history + rows) as f32).max(MIN_THUMB_H);
        assert!((h - expect).abs() < 0.01);
        // Round-trip: drag math inverts thumb math.
        let (thumb_y, thumb_h) = bar.thumb.unwrap();
        let grab = thumb_h / 2.0;
        let off = drag_offset(thumb_y + grab, grab, 0.0, track_h, thumb_h, history);
        assert_eq!(off, 50);
    }

    #[test]
    fn drag_extremes() {
        let track_h = 400.0;
        let bar = scrollbar(0.0, track_h, 1.0, 80, 0, 20);
        let (_, thumb_h) = bar.thumb.unwrap();
        // Drag to very top -> max history; to bottom -> live.
        assert_eq!(drag_offset(0.0, 0.0, 0.0, track_h, thumb_h, 80), 80);
        assert_eq!(
            drag_offset(track_h, thumb_h, 0.0, track_h, thumb_h, 80),
            0
        );
    }

    #[test]
    fn tab_regions_contiguous_and_hittable() {
        let titles = vec!["tab-1".to_string(), "tab-2".to_string()];
        let regions = tab_regions(&titles, 0, 9.0, 8.0, 1.0);
        assert_eq!(regions.len(), 2);
        // Contiguous: each segment starts where the previous ended.
        assert!((regions[1].0 - regions[0].1).abs() < 0.01);
        assert_eq!(tab_hit(&regions, 8.0 + 1.0), Some(0));
        assert_eq!(tab_hit(&regions, regions[1].0 + 1.0), Some(1));
        assert_eq!(tab_hit(&regions, 0.0), None);
        assert_eq!(tab_hit(&regions, regions[1].1 + 100.0), None);
        // Labels match what the renderer draws.
        assert_eq!(tab_label(0, "tab-1", true), " [1:tab-1 x] ");
        assert_eq!(tab_label(1, "tab-2", false), "  2:tab-2 x  ");
    }

    #[test]
    fn tab_close_zone_tracks_label_end() {
        let titles = vec!["tab-1".to_string(), "tab-2".to_string()];
        // " [1:tab-1 x] " is 13 cells at advance 9: segment [8, 125).
        // Close zone = last 3 cells: [8 + 10*9, 125) = [98, 125).
        assert_eq!(tab_close_hit(&titles, 0, 9.0, 8.0, 1.0, 98.0), Some(0));
        assert_eq!(tab_close_hit(&titles, 0, 9.0, 8.0, 1.0, 124.9), Some(0));
        // Just left of the zone is tab 0 but NOT its close mark.
        assert_eq!(tab_close_hit(&titles, 0, 9.0, 8.0, 1.0, 97.9), None);
        assert_eq!(tab_hit(&tab_regions(&titles, 0, 9.0, 8.0, 1.0), 97.9), Some(0));
        // Second tab's zone starts where its segment ends minus 3 cells.
        let regions = tab_regions(&titles, 0, 9.0, 8.0, 1.0);
        assert_eq!(
            tab_close_hit(&titles, 0, 9.0, 8.0, 1.0, regions[1].1 - 1.0),
            Some(1)
        );
        assert_eq!(tab_close_hit(&titles, 0, 9.0, 8.0, 1.0, 0.0), None);
    }

    #[test]
    fn tab_span_ranges_cover_active_and_close_marks() {
        let titles = vec!["tab-1".to_string(), "tab-2".to_string()];
        // Joined: " [1:tab-1 x] " (0..13) + "  2:tab-2 x  " (13..26) + "  +" (26..29).
        let (active, xs, plus) = tab_span_ranges(&titles, 0);
        assert_eq!(active, 0..13);
        assert_eq!(xs, vec![10..11, 23..24]);
        assert_eq!(plus, 26..29);
        let joined = tab_bar_text(&titles, 0);
        assert_eq!(&joined[active.clone()], " [1:tab-1 x] ");
        assert_eq!(&joined[xs[0].clone()], "x");
        assert_eq!(&joined[xs[1].clone()], "x");
        assert_eq!(&joined[plus], "  +");
        // Active tab 1: ranges follow the brackets.
        let (active, _, _) = tab_span_ranges(&titles, 1);
        let joined1 = tab_bar_text(&titles, 1);
        assert_eq!(&joined1[active], " [2:tab-2 x] ");
    }

    #[test]
    fn tab_new_hit_covers_plus_mark_only() {
        let titles = vec!["tab-1".to_string(), "tab-2".to_string()];
        // Two 13-cell segments at advance 9 from x=8: bar ends at 8+234=242.
        assert!(!tab_new_hit(&titles, 0, 9.0, 8.0, 1.0, 241.9));
        assert!(tab_new_hit(&titles, 0, 9.0, 8.0, 1.0, 242.0));
        assert!(tab_new_hit(&titles, 0, 9.0, 8.0, 1.0, 268.9));
        assert!(!tab_new_hit(&titles, 0, 9.0, 8.0, 1.0, 269.0));
        assert!(!tab_new_hit(&[], 0, 9.0, 8.0, 1.0, 8.0));
    }

    #[test]
    fn strip_visibility_modes() {
        assert!(!strip_visible(1, "auto"));
        assert!(strip_visible(2, "auto"));
        assert!(strip_visible(1, "always"));
        assert!(!strip_visible(5, "never"));
        // Unknown modes behave like auto (show when useful).
        assert!(strip_visible(2, "bogus"));
    }
}
