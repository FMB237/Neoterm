mod config;
mod input;
mod mouse;
mod renderer;
mod tab;
mod tabs;

use std::sync::Arc;
use std::time::{Duration, Instant};

use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::SelectionType;
use alacritty_terminal::term::TermMode;
use anyhow::Result;
use arboard::{Clipboard, GetExtLinux, LinuxClipboardKind, SetExtLinux};
use clap::Parser;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{ElementState, Modifiers, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::window::{CursorIcon, Window, WindowId};

use config::NeotermConfig;
use input::InputAction;
use renderer::{PaneDraw, Renderer, ScrollState};
use tab::{CellRect, PaneId, SplitDir};
use tabs::Tabs;

#[derive(Parser, Debug)]
#[command(name = "neoterm", version, about = "NeoTerm v0.2 - GPU terminal")]
struct Args {
    /// Command / shell to run (overrides config)
    #[arg(short = 'e', long)]
    command: Option<String>,
}

struct App {
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    tabs: Option<Tabs>,
    cfg: NeotermConfig,
    mods: Modifiers,
    config_rx: Option<std::sync::mpsc::Receiver<()>>,
    _watcher: Option<notify::RecommendedWatcher>,
    blink_on: bool,
    last_blink: Instant,
    proxy: EventLoopProxy<()>,
    // Redraw decision cache: skip present when nothing changed.
    last_titles: Vec<String>,
    last_active: usize,
    last_blink_on: bool,
    force_draw: bool,
    /// Last seen strip visibility (drives grid reflow on show/hide).
    last_strip: bool,
    /// Long-lived clipboard handle. On X11 the clipboard content is hosted
    /// by our process, so this is kept alive for the whole session instead
    /// of opening/closing per operation.
    clipboard: Option<Clipboard>,
    // ---- mouse state (Stage 4) ----
    /// Last known cursor position in physical pixels.
    cursor_pos: Option<PhysicalPosition<f64>>,
    /// Local drag-selection in progress.
    selecting: bool,
    /// Left button currently held.
    left_held: bool,
    /// Current press was forwarded to the app (mouse-reporting mode).
    press_app_mouse: bool,
    /// Selection anchor as (pane, grid_line, col) for click-vs-drag detection.
    anchor: Option<(PaneId, i32, usize)>,
    /// Consecutive clicks on the same cell (1/2/3 = char/word/line).
    click_count: u32,
    last_click_at: Instant,
    /// Thumb drag in progress: distance from thumb top where grabbed.
    /// (Verified against live geometry each motion, so no stale rects.)
    scroll_drag: Option<f32>,
    /// A middle-button press was forwarded to the app (mouse-reporting mode).
    press_app_middle: bool,
    /// Pane + pane-local cell of the current mouse press (selection and
    /// app-report release paths stay in the press pane across panes).
    press_pane: Option<PaneId>,
    press_local: Option<(usize, usize)>,
    /// Divider drag in progress: tab index, tree path, direction, and the
    /// parent span (ratio-invariant, captured at grab time).
    split_drag: Option<SplitDrag>,
    /// Resize cursor currently shown for divider hover/drag.
    split_cursor: Option<CursorIcon>,
}

/// Divider drag state. The parent rect never changes under ratio changes,
/// so drag math needs no re-layout.
struct SplitDrag {
    tab: usize,
    path: Vec<bool>,
    dir: SplitDir,
    parent: CellRect,
}

impl App {
    fn new(cfg: NeotermConfig, proxy: EventLoopProxy<()>) -> Self {
        Self {
            window: None,
            renderer: None,
            tabs: None,
            cfg,
            mods: Modifiers::default(),
            config_rx: None,
            _watcher: None,
            blink_on: true,
            last_blink: Instant::now(),
            proxy,
            last_titles: Vec::new(),
            last_active: usize::MAX,
            last_blink_on: false,
            force_draw: true, // ensure first paint
            last_strip: true,
            clipboard: None,
            cursor_pos: None,
            selecting: false,
            left_held: false,
            press_app_mouse: false,
            anchor: None,
            click_count: 0,
            last_click_at: Instant::now(),
            scroll_drag: None,
            press_app_middle: false,
            press_pane: None,
            press_local: None,
            split_drag: None,
            split_cursor: None,
        }
    }

    /// Lazily opened clipboard; failures (headless, no X11/Wayland
    /// connection) are logged once and never fatal.
    fn clipboard(&mut self) -> Option<&mut Clipboard> {
        if self.clipboard.is_none() {
            match Clipboard::new() {
                Ok(cb) => self.clipboard = Some(cb),
                Err(e) => {
                    log::error!("clipboard unavailable: {e}");
                    return None;
                }
            }
        }
        self.clipboard.as_mut()
    }

    fn do_copy(&mut self) {
        let text = match self.tabs.as_ref().and_then(|t| t.active()) {
            Some(tab) => match tab.copy_text() {
                Some(s) => s,
                None => return,
            },
            None => return,
        };
        match self.clipboard() {
            Some(cb) => {
                if let Err(e) = cb.set_text(text) {
                    log::error!("copy failed: {e}");
                }
            }
            None => log::error!("copy failed: no clipboard"),
        }
    }

    fn do_paste(&mut self) {
        let text = match self.clipboard() {
            Some(cb) => match cb.get_text() {
                Ok(s) => s,
                Err(e) => {
                    log::error!("paste failed: {e}");
                    return;
                }
            },
            None => return,
        };
        self.write_paste(text);
    }

    /// X11 middle-click: paste the PRIMARY selection buffer.
    fn do_paste_primary(&mut self) {
        let text = match self.clipboard() {
            Some(cb) => match cb.get().clipboard(LinuxClipboardKind::Primary).text() {
                Ok(s) => s,
                Err(e) => {
                    log::error!("primary paste failed: {e}");
                    return;
                }
            },
            None => return,
        };
        self.write_paste(text);
    }

    /// X11 select-to-copy: mirror the active selection into PRIMARY so
    /// middle-click pastes it anywhere on the desktop.
    fn copy_primary(&mut self) {
        let has_sel = self
            .tabs
            .as_ref()
            .and_then(|t| t.active())
            .map(|tab| tab.has_selection())
            .unwrap_or(false);
        if !has_sel {
            return;
        }
        let text = match self
            .tabs
            .as_ref()
            .and_then(|t| t.active())
            .and_then(|tab| tab.copy_text())
        {
            Some(s) => s,
            None => return,
        };
        match self.clipboard() {
            Some(cb) => {
                if let Err(e) = cb
                    .set()
                    .clipboard(LinuxClipboardKind::Primary)
                    .text(text)
                {
                    log::error!("primary copy failed: {e}");
                }
            }
            None => log::error!("primary copy failed: no clipboard"),
        }
    }

    /// Shared paste core: normalize newlines, honor bracketed paste.
    fn write_paste(&mut self, text: String) {
        if text.is_empty() {
            return;
        }
        let Some(tab) = self.tabs.as_ref().and_then(|t| t.active()) else {
            return;
        };
        // Normalize newlines for the PTY; honor bracketed paste when the
        // running app requested it (e.g. vim, readline).
        let body = text.replace("\r\n", "\r").replace('\n', "\r");
        if tab.bracketed_paste() {
            tab.write(b"\x1b[200~");
            tab.write(body.as_bytes());
            tab.write(b"\x1b[201~");
        } else {
            tab.write(body.as_bytes());
        }
    }

    fn setup_config_watch(&mut self) {
        let Some(path) = NeotermConfig::path() else {
            return;
        };
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let mut watcher =
            match notify::recommended_watcher(move |res: Result<notify::Event, notify::Error>| {
                if res.is_ok() {
                    let _ = tx.send(());
                }
            }) {
                Ok(w) => w,
                Err(_) => return,
            };
        use notify::Watcher;
        if watcher
            .watch(&path, notify::RecursiveMode::NonRecursive)
            .is_ok()
        {
            self.config_rx = Some(rx);
            self._watcher = Some(watcher);
        } else if let Some(parent) = path.parent() {
            // Watch parent dir if file doesn't exist yet.
            if watcher
                .watch(parent, notify::RecursiveMode::NonRecursive)
                .is_ok()
            {
                self.config_rx = Some(rx);
                self._watcher = Some(watcher);
            }
        }
    }

    fn poll_config_reload(&mut self) {
        let dirty = match &self.config_rx {
            Some(rx) => rx.try_iter().next().is_some(),
            None => false,
        };
        if dirty {
            // Debounce: wait a moment for editors finishing writes.
            std::thread::sleep(Duration::from_millis(50));
            let new_cfg = NeotermConfig::load();
            log::info!("config reloaded");
            self.cfg = new_cfg;
            if let Some(r) = self.renderer.as_mut() {
                r.apply_config(&self.cfg);
            }
            if let Some(t) = self.tabs.as_mut() {
                t.apply_config(&self.cfg);
            }
            // Config affects colors/metrics: force next redraw to present.
            self.force_draw = true;
            if let Some(w) = self.window.as_ref() {
                w.request_redraw();
            }
        }
    }

    /// Rough cols/rows estimate from pixels (v0.2 heuristic).
    fn estimate_grid(&self, width_px: u32, height_px: u32) -> (u16, u16) {
        let scale = self.window.as_ref().map(|w| w.scale_factor()).unwrap_or(1.0) as f32;
        let cell_w = self.cfg.font_size * 0.62 * scale;
        let cell_h = self.cfg.font_size * 1.35 * scale;
        let top = if self.strip_on() { 30.0 } else { 8.0 };
        let cols = ((width_px as f32 - 16.0 * scale) / cell_w.max(1.0)) as u16;
        let rows = ((height_px as f32 - (top + 10.0) * scale) / cell_h.max(1.0)) as u16;
        (cols.clamp(20, 400), rows.clamp(5, 200))
    }

    fn request_redraw(&self) {
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }

    /// Whether the tab strip is currently visible (Ghostty auto-hide).
    fn strip_on(&self) -> bool {
        let n = self.tabs.as_ref().map(|t| t.len()).unwrap_or(0);
        mouse::strip_visible(n, &self.cfg.tab_bar)
    }

    /// Resolve physical-pixel coordinates to the pane under the cursor:
    /// (pane id, pane-local col, pane-local viewport row, half). None on
    /// dividers, the tab strip, or outside all panes. Coordinates are
    /// pane-local so selection Points and SGR reports match the app's own
    /// grid size.
    fn pane_cell_at(&self, x: f64, y: f64) -> Option<(PaneId, usize, usize, Side)> {
        let window = self.window.as_ref()?;
        let renderer = self.renderer.as_ref()?;
        let tabs = self.tabs.as_ref()?;
        let tab = tabs.active()?;
        let scale = window.scale_factor() as f32;
        let (cell_w, line_h, left, top) = renderer.body_geometry(scale, self.strip_on());
        // Unbounded clamp: pane_at decides membership (dividers → None).
        let hit = mouse::hit_test(x, y, cell_w, line_h, left, top, 100_000, 10_000, 0)?;
        let layout = tab.layout();
        let (id, rect) = layout
            .panes
            .iter()
            .find(|(_, r)| r.contains(hit.col, hit.grid_line as usize))
            .copied()?;
        Some((
            id,
            hit.col - rect.col,
            hit.grid_line as usize - rect.row,
            hit.side,
        ))
    }

    /// Show a resize cursor for divider hover/drag (deduped: windowing
    /// round-trip only on change).
    fn set_split_cursor(&mut self, icon: Option<CursorIcon>) {
        if self.split_cursor == icon {
            return;
        }
        self.split_cursor = icon;
        if let Some(w) = self.window.as_ref() {
            w.set_cursor(icon.unwrap_or(CursorIcon::Default));
        }
    }
    /// Divider under the cursor, if any: (tab index, tree path, direction,
    /// parent span) for starting a drag-resize.
    fn divider_grab(&self, x: f64, y: f64) -> Option<(usize, Vec<bool>, SplitDir, CellRect)> {
        let window = self.window.as_ref()?;
        let renderer = self.renderer.as_ref()?;
        let tabs = self.tabs.as_ref()?;
        let tab = tabs.active()?;
        let scale = window.scale_factor() as f32;
        let (cell_w, line_h, left, top) = renderer.body_geometry(scale, self.strip_on());
        let hit = mouse::hit_test(x, y, cell_w, line_h, left, top, 100_000, 10_000, 0)?;
        let (path, _) = tab.divider_at(hit.col, hit.grid_line as usize)?;
        let (dir, parent) = tab.divider_info(&path)?;
        Some((tabs.active_index(), path, dir, parent))
    }

    /// Scrollbar live geometry + history. None when hidden (no history)
    /// or without window/tab. Mirrors the rect math in `Renderer::draw`.
    fn scrollbar_geom(&self) -> Option<(mouse::ScrollBar, f32, usize)> {
        let window = self.window.as_ref()?;
        let renderer = self.renderer.as_ref()?;
        let tab = self.tabs.as_ref()?.active()?;
        let scale = window.scale_factor() as f32;
        let (cell_w, line_h, left, top) = renderer.body_geometry(scale, self.strip_on());
        // Track lives inside the ACTIVE pane (splits: each pane owns its
        // scrollback; the bar never overlays a neighboring pane).
        let layout = tab.layout();
        let (_, rect) = layout
            .panes
            .iter()
            .find(|(id, _)| *id == tab.active_id())
            .copied()?;
        let track_top = top + rect.row as f32 * line_h;
        let track_h = (rect.rows as f32 * line_h - 4.0 * scale).max(0.0);
        let history = tab.history_size();
        let bar = mouse::scrollbar(
            track_top,
            track_h,
            scale,
            history,
            tab.display_offset(),
            tab.grid_size().1,
        );
        if bar.thumb.is_none() {
            return None;
        }
        let bar_w = mouse::BAR_W * scale;
        let track_x = left + (rect.col + rect.cols) as f32 * cell_w - bar_w - mouse::BAR_MARGIN * scale;
        Some((bar, track_x, history))
    }

    fn on_left_press(&mut self) {
        let Some(pos) = self.cursor_pos else {
            return;
        };
        self.left_held = true;
        // Tab bar first: `+` opens, close mark closes, elsewhere switches.
        // Hidden strip: clicks fall through to the body. Chrome never
        // reaches selection/apps.
        let tab_click: Option<usize> = (|| {
            if !self.strip_on() {
                return None;
            }
            let window = self.window.as_ref()?;
            let renderer = self.renderer.as_ref()?;
            let tabs = self.tabs.as_ref()?;
            let scale = window.scale_factor() as f32;
            let g = renderer.body_geometry(scale, self.strip_on());
            if pos.y as f32 >= g.3 {
                return None;
            }
            let titles = tabs.titles();
            let adv = g.0 / scale.max(f32::EPSILON);
            let regions = mouse::tab_regions(&titles, tabs.active_index(), adv, 8.0 * scale, scale);
            mouse::tab_hit(&regions, pos.x as f32)
        })();
        if self.strip_on() {
            // `+` new-tab mark (outside all tab segments).
            let new_tab = (|| {
                let window = self.window.as_ref()?;
                let renderer = self.renderer.as_ref()?;
                let tabs = self.tabs.as_ref()?;
                let scale = window.scale_factor() as f32;
                let g = renderer.body_geometry(scale, self.strip_on());
                if pos.y as f32 >= g.3 {
                    return None;
                }
                let titles = tabs.titles();
                let adv = g.0 / scale.max(f32::EPSILON);
                mouse::tab_new_hit(&titles, tabs.active_index(), adv, 8.0 * scale, scale, pos.x as f32)
                    .then_some(())
            })();
            if new_tab.is_some() {
                if let Some(tabs) = self.tabs.as_mut() {
                    let _ = tabs.new_tab(&self.cfg);
                }
                self.force_draw = true;
                self.request_redraw();
                return;
            }
        }
        if let Some(i) = tab_click {
            // Close mark (`x` zone) closes instead of switching.
            let close = (|| {
                let window = self.window.as_ref()?;
                let renderer = self.renderer.as_ref()?;
                let tabs = self.tabs.as_ref()?;
                let scale = window.scale_factor() as f32;
                let g = renderer.body_geometry(scale, self.strip_on());
                let titles = tabs.titles();
                let adv = g.0 / scale.max(f32::EPSILON);
                mouse::tab_close_hit(
                    &titles,
                    tabs.active_index(),
                    adv,
                    8.0 * scale,
                    scale,
                    pos.x as f32,
                )
            })();
            if let Some(tabs) = self.tabs.as_mut() {
                if close == Some(i) {
                    tabs.close_at(i, &self.cfg);
                } else {
                    tabs.select(i);
                }
            }
            self.force_draw = true;
            self.request_redraw();
            return;
        }
        // Divider chrome: grab for drag-resize. Above scrollbar/body.
        if self.split_drag.is_none() {
            if let Some((tab_idx, path, dir, parent)) = self.divider_grab(pos.x, pos.y) {
                self.split_drag = Some(SplitDrag {
                    tab: tab_idx,
                    path,
                    dir,
                    parent,
                });
                self.left_held = true;
                self.set_split_cursor(Some(match dir {
                    SplitDir::V => CursorIcon::ColResize,
                    SplitDir::H => CursorIcon::RowResize,
                }));
                return;
            }
        }
        // Scrollbar chrome first: never selection, never app mouse reports.
        if let Some((bar, track_x, _)) = self.scrollbar_geom() {
            let x = pos.x as f32;
            let y = pos.y as f32;
            let in_bar = x >= track_x
                && x <= track_x + bar.track_w
                && y >= bar.track_top
                && y <= bar.track_top + bar.track_h;
            if in_bar {
                if let Some((thumb_y, thumb_h)) = bar.thumb {
                    if y >= thumb_y && y <= thumb_y + thumb_h {
                        // Grab the thumb (dragged in on_cursor_moved).
                        self.scroll_drag = Some(y - thumb_y);
                    } else if let Some(tab) =
                        self.tabs.as_ref().and_then(|t| t.active())
                    {
                        // Track click: page toward the click.
                        tab.scroll_page(y < thumb_y);
                        self.force_draw = true;
                    }
                }
                self.request_redraw();
                return;
            }
        }
        let Some((pid, col, vrow, side)) = self.pane_cell_at(pos.x, pos.y) else {
            // Divider, tab bar, or outside: no selection, no app report.
            return;
        };
        // Click focuses another pane without starting selection.
        let focused = match self.tabs.as_mut() {
            Some(tabs) => match tabs.active_mut() {
                Some(tab) if tab.active_id() != pid => {
                    tab.focus_id(pid);
                    true
                }
                _ => false,
            },
            None => return,
        };
        if focused {
            self.force_draw = true;
            self.request_redraw();
            return;
        }
        let Some(tab) = self.tabs.as_ref().and_then(|t| t.active()) else {
            return;
        };
        let Some(pane) = tab.pane(pid) else {
            return;
        };
        let point = Point::new(
            Line(vrow as i32 - pane.display_offset() as i32),
            Column(col),
        );
        let mode = pane.term_mode();
        if mode.intersects(TermMode::MOUSE_MODE) {
            // App owns the mouse: SGR press report, no local selection.
            pane.clear_selection();
            pane.write(&mouse::encode_sgr(
                mouse::BTN_LEFT,
                col,
                vrow,
                &self.mods,
                true,
            ));
            self.press_app_mouse = true;
            self.press_pane = Some(pid);
            self.press_local = Some((col, vrow));
            self.selecting = false;
            return;
        }
        self.press_app_mouse = false;
        self.press_pane = Some(pid);
        // Click counting for single/double/triple select (400ms window).
        let now = Instant::now();
        let same_cell = self.anchor == Some((pid, point.line.0, col));
        if same_cell && now.duration_since(self.last_click_at) < Duration::from_millis(400) {
            self.click_count = (self.click_count % 3) + 1;
        } else {
            self.click_count = 1;
        }
        self.last_click_at = now;
        self.anchor = Some((pid, point.line.0, col));
        self.press_local = Some((col, vrow));
        let ty = match self.click_count {
            2 => SelectionType::Semantic,
            3 => SelectionType::Lines,
            _ => SelectionType::Simple,
        };
        pane.begin_selection(ty, point, side);
        self.selecting = true;
        // Selection is excluded from damage tracking: force the present.
        self.force_draw = true;
        self.request_redraw();
    }

    fn on_left_release(&mut self) {
        self.left_held = false;
        // End of a divider drag (no selection/app-report state was touched).
        if self.split_drag.take().is_some() {
            self.set_split_cursor(None);
            return;
        }
        // End of a thumb drag (no selection/app-report state was touched).
        if self.scroll_drag.take().is_some() {
            return;
        }
        if self.press_app_mouse {
            self.press_app_mouse = false;
            // Release goes to the press pane (capture semantics), at the
            // current cursor mapped into it, else the press cell.
            if let Some(pid) = self.press_pane {
                let tabs = self.tabs.as_ref();
                let tab = tabs.and_then(|t| t.active());
                let pane = tab.and_then(|t| t.pane(pid));
                if let (Some(pane), Some(pos)) = (pane, self.cursor_pos) {
                    if pane.term_mode().contains(TermMode::MOUSE_REPORT_CLICK) {
                        let local = self
                            .pane_cell_at(pos.x, pos.y)
                            .filter(|(id, _, _, _)| *id == pid)
                            .map(|(_, c, r, _)| (c, r))
                            .or(self.press_local);
                        if let Some((col, vrow)) = local {
                            pane.write(&mouse::encode_sgr(
                                mouse::BTN_RELEASE,
                                col,
                                vrow,
                                &self.mods,
                                false,
                            ));
                        }
                    }
                }
            }
            self.press_pane = None;
            self.press_local = None;
            return;
        }
        if self.selecting {
            self.selecting = false;
            // Plain click (no drag) clears the selection it just opened.
            if self.click_count == 1 {
                if let (Some(pos), Some(tab), Some(anchor)) = (
                    self.cursor_pos,
                    self.tabs.as_ref().and_then(|t| t.active()),
                    self.anchor,
                ) {
                    if let Some((pid, col, vrow, _)) = self.pane_cell_at(pos.x, pos.y) {
                        if let Some(pane) = tab.pane(pid) {
                            let gl = vrow as i32 - pane.display_offset() as i32;
                            if (pid, gl, col) == anchor {
                                pane.clear_selection();
                            }
                        }
                    }
                }
            }
            // X11 tradition: whatever selection remains owns PRIMARY.
            self.copy_primary();
            self.force_draw = true;
            self.request_redraw();
            self.press_pane = None;
            self.press_local = None;
        }
    }

    fn on_middle_press(&mut self) {
        let Some(pos) = self.cursor_pos else {
            return;
        };
        // Tab bar: middle-click closes the tab (hidden strip: paste).
        let tab_click: Option<usize> = (|| {
            if !self.strip_on() {
                return None;
            }
            let window = self.window.as_ref()?;
            let renderer = self.renderer.as_ref()?;
            let tabs = self.tabs.as_ref()?;
            let scale = window.scale_factor() as f32;
            let g = renderer.body_geometry(scale, self.strip_on());
            if pos.y as f32 >= g.3 {
                return None;
            }
            let titles = tabs.titles();
            let adv = g.0 / scale.max(f32::EPSILON);
            let regions =
                mouse::tab_regions(&titles, tabs.active_index(), adv, 8.0 * scale, scale);
            mouse::tab_hit(&regions, pos.x as f32)
        })();
        if let Some(i) = tab_click {
            if let Some(tabs) = self.tabs.as_mut() {
                tabs.close_at(i, &self.cfg);
            }
            self.force_draw = true;
            self.request_redraw();
            return;
        }
        let Some((pid, col, vrow, _)) = self.pane_cell_at(pos.x, pos.y) else {
            return;
        };
        // Middle-click focuses the pane first (same as left-click).
        if let Some(tabs) = self.tabs.as_mut() {
            if let Some(tab) = tabs.active_mut() {
                if tab.active_id() != pid {
                    tab.focus_id(pid);
                    self.force_draw = true;
                    self.request_redraw();
                }
            }
        }
        let Some(tab) = self.tabs.as_ref().and_then(|t| t.active()) else {
            return;
        };
        let Some(pane) = tab.pane(pid) else {
            return;
        };
        if pane.term_mode().intersects(TermMode::MOUSE_MODE) {
            pane.write(&mouse::encode_sgr(
                mouse::BTN_MIDDLE,
                col,
                vrow,
                &self.mods,
                true,
            ));
            self.press_app_middle = true;
            self.press_pane = Some(pid);
            self.press_local = Some((col, vrow));
            return;
        }
        self.do_paste_primary();
    }

    fn on_middle_release(&mut self) {
        if !self.press_app_middle {
            return;
        }
        self.press_app_middle = false;
        if let Some(pid) = self.press_pane {
            let tabs = self.tabs.as_ref();
            let tab = tabs.and_then(|t| t.active());
            if let Some(pane) = tab.and_then(|t| t.pane(pid)) {
                if pane.term_mode().contains(TermMode::MOUSE_REPORT_CLICK) {
                    let local = self
                        .pane_cell_at(
                            self.cursor_pos.map(|p| p.x).unwrap_or(0.0),
                            self.cursor_pos.map(|p| p.y).unwrap_or(0.0),
                        )
                        .filter(|(id, _, _, _)| *id == pid)
                        .map(|(_, c, r, _)| (c, r))
                        .or(self.press_local);
                    if let Some((col, vrow)) = local {
                        pane.write(&mouse::encode_sgr(
                            mouse::BTN_RELEASE,
                            col,
                            vrow,
                            &self.mods,
                            false,
                        ));
                    }
                }
            }
        }
        self.press_pane = None;
        self.press_local = None;
    }

    fn on_cursor_moved(&mut self, pos: PhysicalPosition<f64>) {
        self.cursor_pos = Some(pos);
        // Divider drag: ratio from the pointer, clamped to livable panes.
        // resize_body only touches panes whose grid size changed.
        if let Some(drag) = self.split_drag.as_ref() {
            let (tab_idx, path, dir, parent) =
                (drag.tab, drag.path.clone(), drag.dir, drag.parent);
            let mut drop_drag = false;
            let geom = (|| {
                let window = self.window.as_ref()?;
                let renderer = self.renderer.as_ref()?;
                let scale = window.scale_factor() as f32;
                Some(renderer.body_geometry(scale, self.strip_on()))
            })();
            if let (Some((cell_w, line_h, left, top)), Some(tabs)) =
                (geom, self.tabs.as_mut())
            {
                if let Some(hit) =
                    mouse::hit_test(pos.x, pos.y, cell_w, line_h, left, top, 100_000, 10_000, 0)
                {
                    match tabs.get_mut(tab_idx) {
                        Some(tab) => {
                            // Abort if the tree changed under the drag (pane
                            // closed elsewhere): tab indices shift on close.
                            if tab.divider_info(&path).is_none() {
                                drop_drag = true;
                            } else {
                                let ratio = tab::ratio_from_pointer(
                                    dir,
                                    parent,
                                    hit.col,
                                    hit.grid_line as usize,
                                );
                                tab.set_ratio(&path, dir, parent, ratio);
                                self.force_draw = true;
                                self.request_redraw();
                            }
                        }
                        // Dragged tab is gone: drop the drag.
                        None => drop_drag = true,
                    }
                }
            }
            if drop_drag {
                self.split_drag = None;
                self.set_split_cursor(None);
            }
            return;
        }
        // Thumb drag captures the mouse until release.
        if let Some(grab) = self.scroll_drag {
            if let (Some(tab), Some((bar, _, history))) = (
                self.tabs.as_ref().and_then(|t| t.active()),
                self.scrollbar_geom(),
            ) {
                if let Some((_, thumb_h)) = bar.thumb {
                    let off = mouse::drag_offset(
                        pos.y as f32,
                        grab,
                        bar.track_top,
                        bar.track_h,
                        thumb_h,
                        history,
                    );
                    tab.scroll_to(off);
                    self.force_draw = true;
                    self.request_redraw();
                }
            }
            return;
        }
        if self.press_app_mouse {
            // Motion reports stay in the press pane (capture semantics),
            // at pane-local coordinates.
            let press = self.press_pane;
            let tabs = self.tabs.as_ref();
            let tab = tabs.and_then(|t| t.active());
            if let (Some(tab), Some(pid), Some((_, col, vrow, _))) =
                (tab, press, self.pane_cell_at(pos.x, pos.y))
            {
                if pid == press.unwrap() {
                    if let Some(pane) = tab.pane(pid) {
                        let mode = pane.term_mode();
                        // Drag reports need DRAG or MOTION; hover reports need MOTION.
                        let motion_code = if self.left_held && mode.intersects(TermMode::MOUSE_DRAG) {
                            Some(mouse::BTN_LEFT + mouse::BTN_MOTION)
                        } else if !self.left_held && mode.intersects(TermMode::MOUSE_MOTION) {
                            Some(mouse::BTN_RELEASE + mouse::BTN_MOTION)
                        } else {
                            None
                        };
                        if let Some(code) = motion_code {
                            pane.write(&mouse::encode_sgr(code, col, vrow, &self.mods, true));
                            self.press_local = Some((col, vrow));
                        }
                    }
                }
            }
            return;
        }
        if self.selecting {
            // Selection extends only inside the press pane; crossing a
            // divider holds the edge (no cross-pane selections in v1).
            if let (Some(pid), Some((_, col, vrow, side))) =
                (self.press_pane, self.pane_cell_at(pos.x, pos.y))
            {
                if pid == self.press_pane.unwrap() {
                    if let Some(pane) = self
                        .tabs
                        .as_ref()
                        .and_then(|t| t.active())
                        .and_then(|t| t.pane(pid))
                    {
                        let point = Point::new(
                            Line(vrow as i32 - pane.display_offset() as i32),
                            Column(col),
                        );
                        pane.update_selection(point, side);
                        self.press_local = Some((col, vrow));
                        self.force_draw = true;
                        self.request_redraw();
                    }
                }
            }
            return;
        }
        // Hover: resize cursor over dividers (cleared when leaving).
        if self.split_drag.is_none() && !self.left_held {
            let over = self.divider_grab(pos.x, pos.y).map(|(_, _, dir, _)| match dir {
                SplitDir::V => CursorIcon::ColResize,
                SplitDir::H => CursorIcon::RowResize,
            });
            self.set_split_cursor(over);
        }
    }

    fn on_wheel(&mut self, delta: MouseScrollDelta) {
        // Positive = up into history.
        let lines: i32 = match delta {
            MouseScrollDelta::LineDelta(_, y) => (y * 3.0).round() as i32,
            MouseScrollDelta::PixelDelta(p) => {
                let scale = self
                    .window
                    .as_ref()
                    .map(|w| w.scale_factor())
                    .unwrap_or(1.0) as f32;
                let line_h = self
                    .renderer
                    .as_ref()
                    .map(|r| r.body_geometry(scale, self.strip_on()).1)
                    .unwrap_or(20.0);
                (p.y as f32 / line_h.max(1.0)).round() as i32
            }
        };
        if lines == 0 {
            return;
        }
        let Some(tab) = self.tabs.as_ref().and_then(|t| t.active()) else {
            return;
        };
        let mode = tab.term_mode();
        if mode.intersects(TermMode::MOUSE_MODE) {
            // Forward wheel as buttons 64/65 (no release events for wheel).
            let Some(pos) = self.cursor_pos else {
                return;
            };
            let Some((pid, col, vrow, _)) = self.pane_cell_at(pos.x, pos.y) else {
                return;
            };
            let btn = if lines > 0 {
                mouse::BTN_WHEEL_UP
            } else {
                mouse::BTN_WHEEL_DOWN
            };
            if let Some(pane) = tab.pane(pid) {
                for _ in 0..lines.abs().min(10) {
                    pane.write(&mouse::encode_sgr(btn, col, vrow, &self.mods, true));
                }
            }
            return;
        }
        if tab.in_alt_screen() {
            // No scrollback on the alternate screen: arrows, like Alacritty.
            let seq: &[u8] = if lines > 0 { b"\x1b[A" } else { b"\x1b[B" };
            for _ in 0..lines.abs().min(10) {
                tab.write(seq);
            }
            return;
        }
        tab.scroll_lines(lines);
        // scroll_display marks full damage on offset change; force covers no-ops.
        self.force_draw = true;
        self.request_redraw();
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("NeoTerm 0.2")
            .with_inner_size(LogicalSize::new(960.0, 600.0));
        let window = Arc::new(event_loop.create_window(attrs).expect("create window"));

        let renderer = pollster::block_on(Renderer::new(window.clone(), event_loop, &self.cfg));

        let tabs = Tabs::new(&self.cfg, self.proxy.clone()).expect("spawn initial tab");
        // Sync initial grid size with window.
        let size = window.inner_size();

        self.window = Some(window);
        self.renderer = Some(renderer);
        self.tabs = Some(tabs);
        self.setup_config_watch();

        {
            let (cols, rows) = self.estimate_grid(size.width, size.height);
            if let Some(t) = self.tabs.as_mut() {
                t.resize_all(cols, rows);
            }
        }
        log::info!("NeoTerm v0.2 ready: Ctrl+Shift+T new tab, Ctrl+Shift+W close, Ctrl+Tab switch");
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::ModifiersChanged(m) => {
                self.mods = m;
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let alt = self
                    .tabs
                    .as_ref()
                    .and_then(|t| t.active())
                    .map(|t| t.in_alt_screen())
                    .unwrap_or(false);
                match input::map_key(&event, &self.mods, alt) {
                    InputAction::PtyInput(bytes) => {
                        if let Some(tabs) = self.tabs.as_ref() {
                            if let Some(tab) = tabs.active() {
                                tab.write(&bytes);
                            }
                        }
                        if let Some(w) = self.window.as_ref() {
                            w.request_redraw();
                        }
                    }
                    InputAction::NewTab => {
                        if let Some(tabs) = self.tabs.as_mut() {
                            if let Err(e) = tabs.new_tab(&self.cfg) {
                                log::error!("new tab failed: {e:#}");
                            }
                        }
                        if let Some(w) = self.window.as_ref() {
                            w.request_redraw();
                        }
                    }
                    InputAction::SplitV => {
                        if let Some(tabs) = self.tabs.as_mut() {
                            match tabs.split_active_tab(SplitDir::V, &self.cfg) {
                                Ok(false) => log::info!("split refused: pane too small"),
                                Err(e) => log::error!("split failed: {e:#}"),
                                _ => {}
                            }
                        }
                        self.force_draw = true;
                        self.request_redraw();
                    }
                    InputAction::SplitH => {
                        if let Some(tabs) = self.tabs.as_mut() {
                            match tabs.split_active_tab(SplitDir::H, &self.cfg) {
                                Ok(false) => log::info!("split refused: pane too small"),
                                Err(e) => log::error!("split failed: {e:#}"),
                                _ => {}
                            }
                        }
                        self.force_draw = true;
                        self.request_redraw();
                    }
                    InputAction::ClosePane => {
                        if let Some(tabs) = self.tabs.as_mut() {
                            tabs.close_active_pane(&self.cfg);
                        }
                        self.force_draw = true;
                        self.request_redraw();
                    }
                    InputAction::FocusPane(dir) => {
                        let moved = self
                            .tabs
                            .as_mut()
                            .map(|t| t.focus_active_tab(dir))
                            .unwrap_or(false);
                        if moved {
                            self.force_draw = true;
                            self.request_redraw();
                        }
                    }
                    InputAction::ZoomPane => {
                        if let Some(tabs) = self.tabs.as_mut() {
                            tabs.zoom_active_tab();
                        }
                        self.force_draw = true;
                        self.request_redraw();
                    }
                    InputAction::NextTab => {
                        if let Some(tabs) = self.tabs.as_mut() {
                            tabs.next();
                        }
                        if let Some(w) = self.window.as_ref() {
                            w.request_redraw();
                        }
                    }
                    InputAction::PrevTab => {
                        if let Some(tabs) = self.tabs.as_mut() {
                            tabs.prev();
                        }
                        if let Some(w) = self.window.as_ref() {
                            w.request_redraw();
                        }
                    }
                    InputAction::SelectTab(i) => {
                        if let Some(tabs) = self.tabs.as_mut() {
                            tabs.select(i);
                        }
                        if let Some(w) = self.window.as_ref() {
                            w.request_redraw();
                        }
                    }
                    InputAction::Copy => self.do_copy(),
                    InputAction::Paste => self.do_paste(),
                    InputAction::ScrollPageUp => {
                        if let Some(tab) = self.tabs.as_ref().and_then(|t| t.active()) {
                            tab.scroll_page(true);
                        }
                        self.force_draw = true;
                        self.request_redraw();
                    }
                    InputAction::ScrollPageDown => {
                        if let Some(tab) = self.tabs.as_ref().and_then(|t| t.active()) {
                            tab.scroll_page(false);
                        }
                        self.force_draw = true;
                        self.request_redraw();
                    }
                    InputAction::Ignored => {}
                }
            }
            WindowEvent::CursorMoved { position, .. } => self.on_cursor_moved(position),
            WindowEvent::MouseInput { state, button, .. } => match (button, state) {
                (MouseButton::Left, ElementState::Pressed) => self.on_left_press(),
                (MouseButton::Left, ElementState::Released) => self.on_left_release(),
                (MouseButton::Middle, ElementState::Pressed) => self.on_middle_press(),
                (MouseButton::Middle, ElementState::Released) => self.on_middle_release(),
                _ => {}
            },
            WindowEvent::MouseWheel { delta, .. } => self.on_wheel(delta),
            WindowEvent::Resized(size) => {
                if let Some(r) = self.renderer.as_mut() {
                    r.resize(size.width, size.height);
                }
                let (cols, rows) = self.estimate_grid(size.width, size.height);
                if let Some(t) = self.tabs.as_mut() {
                    t.resize_all(cols, rows);
                }
                // Resize clears renderer caches: force the next present.
                self.force_draw = true;
                if let Some(w) = self.window.as_ref() {
                    w.request_redraw();
                }
            }
            WindowEvent::RedrawRequested => {
                let Some(tabs) = self.tabs.as_ref() else {
                    return;
                };
                let titles = tabs.titles();
                let active = tabs.active_index();
                let blink = self.blink_on;
                let strip = self.strip_on();
                // Drain reader wakeups + damage; skip the whole present when
                // nothing changed since the last frame.
                let pty_dirty = tabs.take_dirty();
                let term_damaged = tabs
                    .active()
                    .map(|tab| tab.take_damage())
                    .unwrap_or(true);
                let ui_changed = titles != self.last_titles
                    || active != self.last_active
                    || blink != self.last_blink_on
                    || self.force_draw;
                if !(pty_dirty || term_damaged || ui_changed) {
                    return;
                }
                if let (Some(r), Some(t)) =
                    (self.renderer.as_mut(), self.tabs.as_ref())
                {
                    // All panes of the ACTIVE tab (inactive tabs don't draw).
                    // Cursor blinks only in the focused pane.
                    let fg = self.cfg.fg_rgb();
                    let bg = self.cfg.bg_rgb();
                    let blink = self.blink_on;
                    let mut panes = Vec::new();
                    let mut divs = Vec::new();
                    if let Some(tab) = t.active() {
                        let layout = tab.layout();
                        let focus = tab.active_id();
                        for (id, rect) in layout.panes {
                            if let Some(pane) = tab.pane(id) {
                                let (rows, _) =
                                    pane.rendered_rows(fg, bg, blink && id == focus);
                                panes.push(PaneDraw { id, rect, rows });
                            }
                        }
                        divs = layout
                            .dividers
                            .into_iter()
                            .map(|(_, r)| r)
                            .collect();
                    }
                    let scroll = t
                        .active()
                        .map(|tab| {
                            let (_, rows) = tab.grid_size();
                            let pane = tab.layout().panes.iter().find(|(id, _)| *id == tab.active_id()).map(|(_, r)| *r);
                            ScrollState {
                                history: tab.history_size(),
                                offset: tab.display_offset(),
                                rows,
                                pane,
                            }
                        })
                        .unwrap_or(ScrollState {
                            history: 0,
                            offset: 0,
                            rows: 24,
                            pane: None,
                        });
                    r.draw(&titles, t.active_index(), &panes, &divs, scroll, strip);
                }
                self.last_titles = titles;
                self.last_active = active;
                self.last_blink_on = blink;
                self.force_draw = false;
            }
            _ => {}
        }
    }

    /// PTY-reader wakeups land here (coalesced per output burst).
    fn user_event(&mut self, _event_loop: &ActiveEventLoop, _event: ()) {
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.poll_config_reload();
        // Strip show/hide reflows the grid (body reclaims the strip
        // height), same as a window resize. Runs here (no live borrows)
        // rather than in the redraw arm.
        if self.strip_on() != self.last_strip {
            self.last_strip = self.strip_on();
            let grid = self.window.as_ref().map(|w| {
                let s = w.inner_size();
                self.estimate_grid(s.width, s.height)
            });
            if let (Some((cols, rows)), Some(t)) = (grid, self.tabs.as_mut()) {
                t.resize_all(cols, rows);
            }
            self.force_draw = true;
            self.request_redraw();
        }
        // Reap tabs whose shells exited (`exit` closes the tab). Runs on
        // every wake, so no dead tab lingers even if its wakeup coalesced.
        // Last tab out quits the app.
        let (reaped, empty) = match self.tabs.as_mut() {
            Some(tabs) => {
                let reaped = tabs.reap_dead();
                (reaped, tabs.len() == 0)
            }
            None => (false, false),
        };
        if reaped {
            self.force_draw = true;
            self.request_redraw();
        }
        if empty {
            event_loop.exit();
            return;
        }
        // Cursor blink at ~2Hz. No unconditional redraw/sleep anymore:
        // the loop parks in the OS until input, PTY output (user_event),
        // resize, or the next blink deadline wakes it.
        if self.last_blink.elapsed() >= Duration::from_millis(500) {
            self.blink_on = !self.blink_on;
            self.last_blink = Instant::now();
            if let Some(w) = self.window.as_ref() {
                w.request_redraw();
            }
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(
            self.last_blink + Duration::from_millis(500),
        ));
    }
}

fn main() -> Result<()> {
    env_logger::init();
    let args = Args::parse();

    let mut cfg = NeotermConfig::load();
    if let Some(cmd) = args.command {
        cfg.shell = cmd;
    }
    let event_loop = EventLoop::new().expect("create event loop");
    event_loop.set_control_flow(ControlFlow::Wait);

    let proxy = event_loop.create_proxy();
    let mut app = App::new(cfg, proxy);
    event_loop.run_app(&mut app).expect("run app");
    Ok(())
}
