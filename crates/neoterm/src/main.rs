mod config;
mod input;
mod mouse;
mod renderer;
mod tab;
mod tabs;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::SelectionType;
use alacritty_terminal::term::TermMode;
use anyhow::{Context, Result};
use arboard::{Clipboard, GetExtLinux, LinuxClipboardKind, SetExtLinux};
use clap::Parser;
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, NamedKey};
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

    /// Directory to start the shell in (default: $HOME)
    #[arg(short = 'w', long, value_name = "DIR")]
    working_directory: Option<PathBuf>,
}

struct App {
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    tabs: Option<Tabs>,
    cfg: NeotermConfig,
    /// Directory every shell starts in (from `--working-directory`, else
    /// `$HOME`). Launch-time only: a config reload never moves the shell.
    cwd: PathBuf,
    /// Session-only font zoom override; `None` follows `cfg.font_size`.
    /// Config reloads keep an active zoom instead of snapping back.
    font_override: Option<f32>,
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
    /// Visual bell: the body flashes until this instant.
    bell_flash_until: Option<Instant>,
    /// In-progress tab rename (Ctrl+Shift+R): the text being edited.
    rename: Option<RenameState>,
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

/// Live tab rename: which tab is being edited and the text so far.
struct RenameState {
    tab: usize,
    text: String,
    /// The prefilled title is still untouched: the first keystroke
    /// replaces it (like selected text in a rename field).
    fresh: bool,
}

impl App {
    fn new(cfg: NeotermConfig, proxy: EventLoopProxy<()>, cwd: PathBuf) -> Self {
        Self {
            window: None,
            renderer: None,
            tabs: None,
            cfg,
            cwd,
            font_override: None,
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
            bell_flash_until: None,
            rename: None,
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
        // Watch the parent directory, not the file: editors and `sed -i`
        // replace the inode on save, which silently detaches a file watch
        // after the first reload.
        let watched = path
            .parent()
            .map(|dir| watcher.watch(dir, notify::RecursiveMode::NonRecursive).is_ok())
            .unwrap_or(false)
            || watcher
                .watch(&path, notify::RecursiveMode::NonRecursive)
                .is_ok();
        if watched {
            self.config_rx = Some(rx);
            self._watcher = Some(watcher);
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
                // Keep a live zoom across reloads; only 0 / restart resets it.
                if let Some(size) = self.font_override {
                    r.set_font_size(size);
                }
            }
            if let Some(t) = self.tabs.as_mut() {
                t.apply_config(&self.cfg);
            }
            // Config affects colors/metrics: reflow (font size may have
            // changed) and force the next redraw to present.
            self.reflow_grid();
        }
    }

    /// Cols/rows fitting the window. Delegates to the renderer so the grid
    /// uses the same advance/padding as drawing and hit-testing (font zoom
    /// and custom families included). Heuristic only before the renderer
    /// exists.
    fn estimate_grid(&self, width_px: u32, height_px: u32) -> (u16, u16) {
        let scale = self.window.as_ref().map(|w| w.scale_factor()).unwrap_or(1.0) as f32;
        if let Some(r) = self.renderer.as_ref() {
            return r.grid_size(width_px, height_px, scale, self.strip_on());
        }
        let cell_w = self.cfg.font_size * 0.62 * scale;
        let cell_h = self.cfg.font_size * 1.35 * scale;
        let top = if self.strip_on() { 32.0 } else { 10.0 };
        let cols = ((width_px as f32 - 24.0 * scale) / cell_w.max(1.0)) as u16;
        let rows = ((height_px as f32 - (top + 10.0) * scale) / cell_h.max(1.0)) as u16;
        (cols.clamp(20, 400), rows.clamp(5, 200))
    }

    /// Re-flow every pane after a metrics change (font zoom, config reload).
    /// `resize_body` only resizes panes whose grid changed, so this never
    /// SIGWINCH-storms unchanged panes.
    fn reflow_grid(&mut self) {
        let grid = self.window.as_ref().map(|w| {
            let s = w.inner_size();
            self.estimate_grid(s.width, s.height)
        });
        if let (Some((cols, rows)), Some(tabs)) = (grid, self.tabs.as_mut()) {
            tabs.resize_all(cols, rows);
        }
        self.force_draw = true;
        self.request_redraw();
    }

    /// Apply a session font size to the renderer and reflow the grid.
    fn apply_font_size(&mut self, size: f32) {
        if let Some(r) = self.renderer.as_mut() {
            r.set_font_size(size);
        }
        self.reflow_grid();
    }

    fn current_font_size(&self) -> f32 {
        self.renderer
            .as_ref()
            .map(|r| r.font_size())
            .unwrap_or(self.cfg.font_size)
    }

    /// Ctrl+Shift+Plus/Minus: step the font size, session-only.
    fn zoom_font(&mut self, delta: f32) {
        let size = (self.current_font_size() + delta).clamp(6.0, 48.0);
        self.font_override = Some(size);
        self.apply_font_size(size);
    }

    /// Ctrl+Shift+0: back to the configured font size.
    fn reset_font(&mut self) {
        self.font_override = None;
        self.apply_font_size(self.cfg.font_size);
    }

    /// Ctrl+Shift+R: start editing the active tab's title. The strip
    /// shows the text being typed; Enter commits (and pins it), Esc
    /// cancels.
    fn start_rename(&mut self) {
        let (tab, text) = match self.tabs.as_ref() {
            Some(tabs) => (
                tabs.active_index(),
                tabs.active().map(|t| t.title()).unwrap_or_default(),
            ),
            None => return,
        };
        self.rename = Some(RenameState {
            tab,
            text,
            fresh: true,
        });
        self.force_draw = true;
        self.request_redraw();
    }

    /// Keyboard input while a rename is live: every key edits the title.
    fn on_rename_key(&mut self, event: &KeyEvent) {
        /// What the key did.
        enum Next {
            Commit(String),
            Cancel,
            Edit,
        }

        // Only key *presses* edit the title (winit also delivers releases).
        if event.state != ElementState::Pressed {
            return;
        }
        let ctrl = self.mods.state().control_key();
        let next = {
            let Some(state) = self.rename.as_mut() else {
                return;
            };
            match &event.logical_key {
                Key::Named(NamedKey::Enter) => Next::Commit(state.text.trim().to_string()),
                Key::Named(NamedKey::Escape) => Next::Cancel,
                Key::Named(NamedKey::Backspace) => {
                    state.text.pop();
                    state.fresh = false;
                    Next::Edit
                }
                _ => {
                    if ctrl {
                        // Ctrl+U clears the field; other chords are ignored.
                        if let Key::Character(c) = &event.logical_key {
                            if c.eq_ignore_ascii_case("u") {
                                state.text.clear();
                                state.fresh = false;
                            }
                        }
                    } else if let Some(text) = &event.text {
                        // Ignore control characters (they are commands,
                        // not title text).
                        if !text.chars().any(|c| c.is_control()) {
                            // The prefilled title behaves like selected
                            // text: the first keystroke replaces it.
                            if state.fresh {
                                state.text.clear();
                                state.fresh = false;
                            }
                            state.text.push_str(text);
                        }
                    }
                    Next::Edit
                }
            }
        };

        match next {
            Next::Commit(name) => {
                let tab = self.rename.as_ref().map(|s| s.tab).unwrap_or(0);
                self.rename = None;
                if let Some(tabs) = self.tabs.as_mut() {
                    // An empty name clears the pin (live OSC titles return).
                    tabs.set_pin(tab, if name.is_empty() { None } else { Some(name) });
                }
            }
            Next::Cancel => self.rename = None,
            Next::Edit => {}
        }
        self.force_draw = true;
        self.request_redraw();
    }

    fn request_redraw(&self) {
        if let Some(w) = self.window.as_ref() {
            w.request_redraw();
        }
    }

    /// Whether the tab strip is currently visible (Ghostty auto-hide).
    fn strip_on(&self) -> bool {
        // While renaming, the strip must be visible: that is where the
        // text is being edited.
        if self.rename.is_some() {
            return true;
        }
        let n = self.tabs.as_ref().map(|t| t.len()).unwrap_or(0);
        mouse::strip_visible(n, &self.cfg.tab_bar)
    }

    /// Tab titles as shown in the strip, with an in-progress rename
    /// substituted in (the caret is the trailing underscore).
    fn tab_titles(&self) -> Vec<String> {
        let mut titles = self.tabs.as_ref().map(|t| t.titles()).unwrap_or_default();
        if let Some(r) = &self.rename {
            if let Some(slot) = titles.get_mut(r.tab) {
                *slot = format!("{}_", r.text);
            }
        }
        titles
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
            let titles = self.tab_titles();
            let adv = g.0 / scale.max(f32::EPSILON);
            let regions = mouse::tab_regions(&titles, tabs.active_index(), adv, renderer::TAB_LEFT * scale, scale);
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
                let titles = self.tab_titles();
                let adv = g.0 / scale.max(f32::EPSILON);
                mouse::tab_new_hit(&titles, tabs.active_index(), adv, renderer::TAB_LEFT * scale, scale, pos.x as f32)
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
                let titles = self.tab_titles();
                let adv = g.0 / scale.max(f32::EPSILON);
                mouse::tab_close_hit(
                    &titles,
                    tabs.active_index(),
                    adv,
                    renderer::TAB_LEFT * scale,
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
            let titles = self.tab_titles();
            let adv = g.0 / scale.max(f32::EPSILON);
            let regions =
                mouse::tab_regions(&titles, tabs.active_index(), adv, renderer::TAB_LEFT * scale, scale);
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
            .with_inner_size(Renderer::window_logical_size());
        let window = Arc::new(event_loop.create_window(attrs).expect("create window"));

        let renderer = pollster::block_on(Renderer::new(window.clone(), event_loop, &self.cfg));

        let tabs = Tabs::new(&self.cfg, self.proxy.clone(), self.cwd.clone())
            .expect("spawn initial tab");
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
        log::info!(
            "NeoTerm v0.2 ready: Ctrl+Shift+T new tab, Ctrl+Shift+W close, \
             Ctrl+Tab switch, Ctrl+Shift+± font size"
        );
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
                // A live rename owns the keyboard: every key edits the
                // title instead of reaching the shell.
                if self.rename.is_some() {
                    self.on_rename_key(&event);
                    return;
                }
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
                    InputAction::FontZoomIn => self.zoom_font(1.0),
                    InputAction::FontZoomOut => self.zoom_font(-1.0),
                    InputAction::FontZoomReset => self.reset_font(),
                    InputAction::RenameTab => self.start_rename(),
                    InputAction::UnpinTab => {
                        let was_pinned = self
                            .tabs
                            .as_ref()
                            .and_then(|t| t.active())
                            .map(|t| t.pinned())
                            .unwrap_or(false);
                        if let Some(tabs) = self.tabs.as_mut() {
                            tabs.unpin_active();
                        }
                        if !was_pinned {
                            log::info!("tab title was not pinned");
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
                let titles = self.tab_titles();
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
                    // The focused pane blinks its cursor; unfocused panes
                    // show a steady one so the prompt stays locatable.
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
                                let cursor_on = if id == focus { blink } else { true };
                                let (rows, cursor) = pane.rendered_rows(fg, bg, cursor_on);
                                panes.push(PaneDraw {
                                    id,
                                    rect,
                                    rows,
                                    cursor,
                                });
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
                    // Visual bell: tint the body while the flash runs.
                    let flash = self
                        .bell_flash_until
                        .map(|until| Instant::now() < until)
                        .unwrap_or(false);
                    r.set_flash(flash);
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
        // Visual bell: a BEL from any pane flashes the body briefly. The
        // flag is drained even when the bell is disabled, so it never
        // lingers.
        let rang = self.tabs.as_ref().map(|t| t.take_bell()).unwrap_or(false);
        if rang {
            log::debug!("bell: detected (enabled={})", self.cfg.bell_enabled());
        }
        if rang && self.cfg.bell_enabled() {
            self.bell_flash_until = Some(Instant::now() + Duration::from_millis(180));
            self.force_draw = true;
            self.request_redraw();
        }
        // Flash over: paint the normal background again.
        if let Some(until) = self.bell_flash_until {
            if Instant::now() >= until {
                self.bell_flash_until = None;
                self.force_draw = true;
                self.request_redraw();
            }
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
        // Wake for the blink deadline, or earlier when a bell flash is
        // still running (so the flash can be cleared on time).
        let blink_deadline = self.last_blink + Duration::from_millis(500);
        let deadline = match self.bell_flash_until {
            Some(until) => until.min(blink_deadline),
            None => blink_deadline,
        };
        event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
    }
}

fn main() -> Result<()> {
    env_logger::init();
    let args = Args::parse();

    let mut cfg = NeotermConfig::load();
    if let Some(cmd) = args.command {
        cfg.shell = cmd;
    }
    let cwd = resolve_cwd(args.working_directory)?;

    let event_loop = EventLoop::new().expect("create event loop");
    event_loop.set_control_flow(ControlFlow::Wait);

    let proxy = event_loop.create_proxy();
    let mut app = App::new(cfg, proxy, cwd);
    event_loop.run_app(&mut app).expect("run app");
    Ok(())
}

/// Resolve the directory shells start in: `--working-directory` when given
/// (must exist and be a directory), otherwise the user's home.
fn resolve_cwd(dir: Option<PathBuf>) -> Result<PathBuf> {
    let Some(dir) = dir else {
        return Ok(dirs::home_dir().unwrap_or_else(|| PathBuf::from("/")));
    };
    let path = dir
        .canonicalize()
        .with_context(|| format!("--working-directory: {}", dir.display()))?;
    if !path.is_dir() {
        anyhow::bail!("--working-directory is not a directory: {}", path.display());
    }
    Ok(path)
}
