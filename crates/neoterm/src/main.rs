mod config;
mod graphics;
mod input;
mod mouse;
mod palette;
mod renderer;
mod search;
mod session;
mod shell_integration;
mod tab;
mod tabs;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::SelectionType;
use alacritty_terminal::term::TermMode;
use anyhow::{Context, Result};
use arboard::{Clipboard, GetExtLinux, LinuxClipboardKind, SetExtLinux};
use clap::{Parser, Subcommand};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, NamedKey};
use winit::window::{CursorIcon, Window, WindowId};

// Platform-specific window attributes for transparency/blur
#[cfg(target_os = "windows")]
use winit::platform::windows::WindowAttributesExtWindows;
#[cfg(target_os = "macos")]
use winit::platform::macos::WindowAttributesExtMacOS;

use config::NeotermConfig;
use input::InputAction;
use renderer::{PaneDraw, Renderer, ScrollState};
use tab::Tab;
use search::{SearchMatch, SearchState};
use session::{build_session_state, restore_session_state, session_file_path, SessionState};
use palette::{PaletteAction, PaletteState};
use tab::{CellRect, PaneId, SplitDir};
use tabs::Tabs;

/// Apply transparency and blur settings to window attributes.
fn apply_window_effects(attrs: winit::window::WindowAttributes, cfg: &NeotermConfig) -> winit::window::WindowAttributes {
    let mut attrs = attrs;
    
    // Apply transparency (0.0 = opaque, >0 = transparent)
    if cfg.transparency > 0.0 {
        attrs = attrs.with_transparent(true);
    }
    
    // Apply blur effect based on platform and config
    if cfg.blur != "none" {
        #[cfg(target_os = "windows")]
        {
            if cfg.blur == "acrylic" || cfg.blur == "auto" {
                // Windows 11 acrylic blur
                attrs = attrs.with_blur_behind(true);
            }
        }
        #[cfg(target_os = "macos")]
        {
            if cfg.blur == "acrylic" || cfg.blur == "auto" {
                // macOS vibrancy/blur
                attrs = attrs.with_titlebar_transparent(true);
                // Note: Full window vibrancy requires additional NSView setup
            }
        }
    }
    
    attrs
}

#[derive(Parser, Debug)]
#[command(name = "neoterm", version, about = "NeoTerm v0.3 - GPU terminal")]
struct Args {
    /// Command / shell to run (overrides config)
    #[arg(short = 'e', long)]
    command: Option<String>,

    /// Directory to start the shell in (default: $HOME)
    #[arg(short = 'w', long, value_name = "DIR")]
    working_directory: Option<PathBuf>,

    #[command(subcommand)]
    cmd: Option<CliCmd>,
}

#[derive(Subcommand, Debug)]
enum SessionCmd {
    /// Save current session to a file
    Save {
        /// Session name (default: auto)
        name: Option<String>,
    },
    /// Load a saved session
    Load {
        /// Session name to load
        name: String,
    },
    /// List saved sessions
    List,
    /// Delete a saved session
    Delete {
        /// Session name to delete
        name: String,
    },
}

#[derive(Subcommand, Debug)]
enum ThemeCmd {
    /// List available built-in theme presets
    List,
    /// Set the theme preset
    Set {
        /// Preset name (tokyo-night, catppuccin-mocha, dracula, gruvbox-dark, or custom)
        preset: String,
    },
    /// Show current theme configuration
    Show,
}

#[derive(Subcommand, Debug)]
enum CliCmd {
    /// Session management
    #[command(subcommand)]
    Session(SessionCmd),
    /// Theme management
    #[command(subcommand)]
    Theme(ThemeCmd),
}

/// Per-window state for multiple window support.
struct WindowState {
    window: Arc<Window>,
    renderer: Renderer,
    tabs: Tabs,
    // Per-window UI state
    last_titles: Vec<String>,
    last_active: usize,
    blink_timer: Instant,
    blink_on: bool,
    last_blink_state: bool,
    force_draw: bool,
    last_strip: bool,
    bell_flash_until: Option<Instant>,
    toast: Option<(String, Instant)>,
    rename: Option<RenameState>,
    search: Option<SearchState>,
    copymode: Option<CopymodeState>,
    broadcast: bool,
    palette: Option<PaletteState>,
    // Mouse state
    cursor_pos: Option<PhysicalPosition<f64>>,
    selecting: bool,
    left_held: bool,
    press_app_mouse: bool,
    anchor: Option<(PaneId, i32, usize)>,
    click_count: u32,
    last_click_at: Instant,
    scroll_drag: Option<f32>,
    press_app_middle: bool,
    press_pane: Option<PaneId>,
    press_local: Option<(usize, usize)>,
    split_drag: Option<SplitDrag>,
    tab_drag: Option<usize>,
    pane_drag: Option<(usize, PaneId)>,
    split_cursor: Option<CursorIcon>,
}

struct App {
    windows: HashMap<WindowId, WindowState>,
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
    proxy: EventLoopProxy<()>,
    /// Long-lived clipboard handle. On X11 the clipboard content is hosted
    /// by our process, so this is kept alive for the whole session instead
    /// of opening/closing per operation.
    clipboard: Option<Clipboard>,
    /// Whether a session was loaded and should be saved on exit.
    session_loaded: bool,
    /// Loaded session state to restore in `resumed`.
    pending_session: Option<SessionState>,
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

/// Vi-style copy mode state.
struct CopymodeState {
    /// Origin pane and cursor position when entering copymode.
    pane: PaneId,
    /// Cursor position within the grid (row from top of viewport, col).
    cursor: (usize, usize),
    /// Selection anchor (row, col) if selecting, else None.
    anchor: Option<(usize, usize)>,
    /// Whether we're in line selection mode (V).
    line_mode: bool,
    /// Scroll offset from top of history (0 = viewport bottom).
    scroll_offset: usize,
}

impl App {
    fn new(cfg: NeotermConfig, proxy: EventLoopProxy<()>, cwd: PathBuf) -> Self {
        Self {
            windows: HashMap::new(),
            cfg,
            cwd,
            font_override: None,
            mods: Modifiers::default(),
            config_rx: None,
            _watcher: None,
            proxy,
            clipboard: None,
            session_loaded: false,
            pending_session: None,
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

    /// Set up file watcher for config hot-reload.
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

    /// Poll config reload and apply to all windows.
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
            // Apply to all windows
            for ws in self.windows.values_mut() {
                ws.renderer.apply_config(&self.cfg);
                // Keep a live zoom across reloads; only 0 / restart resets it.
                if let Some(size) = self.font_override {
                    ws.renderer.set_font_size(size);
                }
                ws.tabs.apply_config(&self.cfg);
                ws.force_draw = true;
                ws.window.request_redraw();
            }
        }
    }

    /// Find word boundaries on a grid line for double-click word selection.
    /// Returns (start_col, end_col) where the word spans.
    fn word_bounds_from_grid(tab: &Tab, pane_id: PaneId, row: i32, col: usize) -> (usize, usize) {
        if let Some(pane) = tab.pane(pane_id) {
            if let Ok(term) = pane.term.lock() {
                let grid = term.grid();
                let line_idx = Line(row);
                // Check bounds using screen_lines
                if (row as usize) < grid.screen_lines() {
                    let line = &grid[line_idx];
                    let chars: Vec<char> = line
                        .into_iter()
                        .filter(|c| !c.flags.contains(alacritty_terminal::term::cell::Flags::WIDE_CHAR_SPACER))
                        .map(|c| c.c)
                        .collect();
                    if chars.is_empty() {
                        return (col, col);
                    }
                    let char_idx = col.min(chars.len().saturating_sub(1));
                    let mut start = char_idx;
                    let mut end = char_idx;
                    // Expand left
                    while start > 0 && !chars[start - 1].is_whitespace() {
                        start -= 1;
                    }
                    // Expand right
                    while end < chars.len() && !chars[end].is_whitespace() {
                        end += 1;
                    }
                    return (start, end);
                }
            }
        }
        (col, col)
    }

    // ========================================================================
    // Per-window handler methods (take &mut WindowState instead of &mut self)
    // ========================================================================
    
    /// Show a short confirmation in the tab strip (copy / paste).
    fn set_toast(&mut self, ws: &mut WindowState, text: String) {
        ws.toast = Some((text, Instant::now() + Duration::from_millis(1400)));
        ws.force_draw = true;
        ws.window.request_redraw();
    }
    
    /// Apply a session font size to the renderer and reflow the grid for a specific window.
    fn apply_font_size(&mut self, ws: &mut WindowState, size: f32) {
        ws.renderer.set_font_size(size);
        self.reflow_grid(ws);
    }
    
    /// Current font size for a specific window.
    fn current_font_size(&self, ws: &WindowState) -> f32 {
        ws.renderer.font_size()
    }
    
    /// Ctrl+Shift+Plus/Minus: step the font size, session-only for a specific window.
    fn zoom_font(&mut self, ws: &mut WindowState, delta: f32) {
        let size = (self.current_font_size(ws) + delta).clamp(6.0, 48.0);
        self.font_override = Some(size);
        self.apply_font_size(ws, size);
    }
    
    /// Ctrl+Shift+0: back to the configured font size for a specific window.
    fn reset_font(&mut self, ws: &mut WindowState) {
        self.font_override = None;
        self.apply_font_size(ws, self.cfg.font_size);
    }
    
    /// Cols/rows fitting the window. Delegates to the renderer so the grid
    /// uses the same advance/padding as drawing and hit-testing.
    fn estimate_grid(cfg_tab_bar: &str, ws: &WindowState, width_px: u32, height_px: u32) -> (u16, u16) {
        let scale = ws.window.scale_factor() as f32;
        ws.renderer.grid_size(width_px, height_px, scale, Self::strip_on(cfg_tab_bar, ws))
    }
    
    /// Re-flow every pane after a metrics change (font zoom, config reload) for a specific window.
    fn reflow_grid(&mut self, ws: &mut WindowState) {
        let size = ws.window.inner_size();
        let (cols, rows) = Self::estimate_grid(&self.cfg.tab_bar, ws, size.width, size.height);
        ws.tabs.resize_all(cols, rows);
        ws.force_draw = true;
        ws.window.request_redraw();
    }
    
    /// Whether the tab strip is currently visible (Ghostty auto-hide) for a specific window.
    fn strip_on(cfg_tab_bar: &str, ws: &WindowState) -> bool {
        if ws.rename.is_some() {
            return true;
        }
        let n = ws.tabs.len();
        mouse::strip_visible(n, cfg_tab_bar)
    }
    
    /// Tab titles as shown in the strip, with an in-progress rename substituted in.
    fn tab_titles(&self, ws: &WindowState) -> Vec<String> {
        let mut titles = ws.tabs.titles();
        if let Some(r) = &ws.rename {
            if let Some(slot) = titles.get_mut(r.tab) {
                *slot = format!("{}_", r.text);
            }
        }
        titles
    }
    
    /// The tab the pointer is over, if it is in the strip band.
    fn hovered_tab(&self, ws: &WindowState) -> Option<usize> {
        if !Self::strip_on(&self.cfg.tab_bar, ws) {
            return None;
        }
        let pos = ws.cursor_pos?;
        let scale = ws.window.scale_factor() as f32;
        let g = ws.renderer.body_geometry(scale, Self::strip_on(&self.cfg.tab_bar, ws));
        if pos.y as f32 >= g.3 {
            return None;
        }
        let titles = self.tab_titles(ws);
        let adv = g.0 / scale.max(f32::EPSILON);
        let regions = mouse::tab_regions(&titles, adv, renderer::TAB_LEFT * scale, scale);
        mouse::tab_hit(&regions, pos.x as f32)
    }
    
    /// Resolve physical-pixel coordinates to the pane under the cursor.
    fn pane_cell_at(&self, ws: &WindowState, x: f64, y: f64) -> Option<(PaneId, usize, usize, Side)> {
        let scale = ws.window.scale_factor() as f32;
        let (cell_w, line_h, left, top) = ws.renderer.body_geometry(scale, Self::strip_on(&self.cfg.tab_bar, ws));
        let hit = mouse::hit_test(x, y, cell_w, line_h, left, top, 100_000, 10_000, 0)?;
        let tab = ws.tabs.active()?;
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
    
    /// Show a resize cursor for divider hover/drag (deduped).
    fn set_split_cursor(&mut self, ws: &mut WindowState, icon: Option<CursorIcon>) {
        if ws.split_cursor == icon {
            return;
        }
        ws.split_cursor = icon;
        ws.window.set_cursor(icon.unwrap_or(CursorIcon::Default));
    }
    
    /// Divider under the cursor, if any.
    fn divider_grab(&self, ws: &WindowState, x: f64, y: f64) -> Option<(usize, Vec<bool>, SplitDir, CellRect)> {
        let scale = ws.window.scale_factor() as f32;
        let (cell_w, line_h, left, top) = ws.renderer.body_geometry(scale, Self::strip_on(&self.cfg.tab_bar, ws));
        let hit = mouse::hit_test(x, y, cell_w, line_h, left, top, 100_000, 10_000, 0)?;
        let tab = ws.tabs.active()?;
        let (path, _) = tab.divider_at(hit.col, hit.grid_line as usize)?;
        let (dir, parent) = tab.divider_info(&path)?;
        Some((ws.tabs.active_index(), path, dir, parent))
    }
    
    /// Scrollbar live geometry + history.
    fn scrollbar_geom(&self, ws: &WindowState) -> Option<(mouse::ScrollBar, f32, usize, usize)> {
        let scale = ws.window.scale_factor() as f32;
        let tab = ws.tabs.active()?;
        let (cell_w, line_h, left, top) = ws.renderer.body_geometry(scale, Self::strip_on(&self.cfg.tab_bar, ws));
        let layout = tab.layout();
        let (_, rect) = layout
            .panes
            .iter()
            .find(|(id, _)| *id == tab.active_id())
            .copied()?;
        let track_top = top + rect.row as f32 * line_h;
        let track_h = (rect.rows as f32 * line_h - 4.0 * scale).max(0.0);
        let history = tab.history_size();
        let viewport_rows = tab.grid_size().1;
        let bar = mouse::scrollbar(
            track_top,
            track_h,
            scale,
            history,
            tab.display_offset(),
            viewport_rows,
        );
        if bar.thumb.is_none() {
            return None;
        }
        let bar_w = mouse::BAR_W * scale;
        let track_x = left + (rect.col + rect.cols) as f32 * cell_w - bar_w - mouse::BAR_MARGIN * scale;
        Some((bar, track_x, history, viewport_rows))
    }
    
    // ---- Mouse handlers ----
    
    fn on_left_press(&mut self, ws: &mut WindowState) {
        let Some(pos) = ws.cursor_pos else {
            return;
        };
        ws.left_held = true;
        
        // Tab bar first: `+` opens, close mark closes, elsewhere switches.
        let tab_click: Option<usize> = (|| {
            if !Self::strip_on(&self.cfg.tab_bar, ws) {
                return None;
            }
            let scale = ws.window.scale_factor() as f32;
            let g = ws.renderer.body_geometry(scale, Self::strip_on(&self.cfg.tab_bar, ws));
            if pos.y as f32 >= g.3 {
                return None;
            }
            let titles = self.tab_titles(ws);
            let adv = g.0 / scale.max(f32::EPSILON);
            let regions = mouse::tab_regions(&titles, adv, renderer::TAB_LEFT * scale, scale);
            mouse::tab_hit(&regions, pos.x as f32)
        })();
        
        if Self::strip_on(&self.cfg.tab_bar, ws) {
            // `+` new-tab mark (outside all tab segments).
            let new_tab = (|| {
                let scale = ws.window.scale_factor() as f32;
                let g = ws.renderer.body_geometry(scale, Self::strip_on(&self.cfg.tab_bar, ws));
                if pos.y as f32 >= g.3 {
                    return None;
                }
                let titles = self.tab_titles(ws);
                let adv = g.0 / scale.max(f32::EPSILON);
                let left = renderer::TAB_LEFT * scale;
                // Check if clicking on the `+` mark using tab_new_hit
                if mouse::tab_new_hit(&titles, adv, left, scale, pos.x as f32) {
                    Some(())
                } else {
                    None
                }
            })();
            if new_tab.is_some() {
                if let Err(e) = ws.tabs.new_tab(&self.cfg) {
                    log::error!("new tab failed: {e:#}");
                }
                ws.force_draw = true;
                ws.window.request_redraw();
                return;
            }
            if let Some(i) = tab_click {
                if let Some(tabs) = ws.tabs.get_mut(i) {
                    let close_x = (|| {
                        let scale = ws.window.scale_factor() as f32;
                        let g = ws.renderer.body_geometry(scale, Self::strip_on(&self.cfg.tab_bar, ws));
                        let titles = self.tab_titles(ws);
                        let adv = g.0 / scale.max(f32::EPSILON);
                        let left = renderer::TAB_LEFT * scale;
                        // Use tab_close_hit to check if clicking on close mark
                        mouse::tab_close_hit(&titles, adv, left, scale, pos.x as f32)
                    })();
                    if close_x == Some(i) {
                        ws.tabs.close_at(i, &self.cfg);
                        ws.force_draw = true;
                        ws.window.request_redraw();
                        return;
                    }
                    ws.tabs.select(i);
                    ws.force_draw = true;
                    ws.window.request_redraw();
                    return;
                }
            }
        }
        
        // Body: selection / divider / tab drag / pane drag
        let Some((pid, col, vrow, side)) = self.pane_cell_at(ws, pos.x, pos.y) else {
            return;
        };
        
        // Focus the pane on click
        if let Some(tab) = ws.tabs.active_mut() {
            if tab.active_id() != pid {
                tab.focus_id(pid);
                ws.force_draw = true;
                ws.window.request_redraw();
            }
        }
        
        let Some(tab) = ws.tabs.active() else {
            return;
        };
        let Some(pane) = tab.pane(pid) else {
            return;
        };
        
        // App mouse mode?
        if pane.term_mode().intersects(TermMode::MOUSE_MODE) {
            pane.write(&mouse::encode_sgr(
                mouse::BTN_LEFT,
                col,
                vrow,
                &self.mods,
                true,
            ));
            ws.press_app_mouse = true;
            ws.press_pane = Some(pid);
            ws.press_local = Some((col, vrow));
            ws.anchor = None;
            ws.selecting = false;
            return;
        }
        
        // Divider drag start (on divider edge)
        if let Some((tab_idx, path, dir, parent)) = self.divider_grab(ws, pos.x, pos.y) {
            ws.split_drag = Some(SplitDrag { tab: tab_idx, path, dir, parent });
            self.set_split_cursor(ws, Some(match dir {
                SplitDir::V => CursorIcon::EwResize,
                SplitDir::H => CursorIcon::NsResize,
            }));
            return;
        }
        
        // Selection anchor
        ws.anchor = Some((pid, vrow as i32, col));
        ws.selecting = true;
        ws.click_count += 1;
        ws.last_click_at = Instant::now();
        
        // Multi-click: expand selection
        let anchor = ws.anchor.unwrap();
        if ws.click_count == 2 {
            // Word select: use begin_selection + update_selection
            if let Some(pane) = tab.pane(anchor.0) {
                let point = Point::new(Line(anchor.1), Column(anchor.2));
                pane.begin_selection(SelectionType::Simple, point, Side::Left);
                let (start, end) = Self::word_bounds_from_grid(&tab, anchor.0, anchor.1, anchor.2);
                let end_point = Point::new(Line(anchor.1), Column(end));
                pane.update_selection(end_point, Side::Right);
            }
        } else if ws.click_count >= 3 {
            // Line select: use begin_selection + update_selection
            if let Some(pane) = tab.pane(anchor.0) {
                let start_point = Point::new(Line(anchor.1), Column(0));
                pane.begin_selection(SelectionType::Simple, start_point, Side::Left);
                let cols = tab.grid_size().0;
                let end_point = Point::new(Line(anchor.1), Column(cols));
                pane.update_selection(end_point, Side::Right);
            }
        }
        
        ws.press_pane = Some(pid);
        ws.press_local = Some((col, vrow));
        ws.force_draw = true;
        ws.window.request_redraw();
    }
    
    fn on_left_release(&mut self, ws: &mut WindowState) {
        ws.left_held = false;
        
        // Tab drag drop
        if let Some(src) = ws.tab_drag.take() {
            ws.force_draw = true;
            ws.window.request_redraw();
            return;
        }
        
        // Pane drag drop
        if let Some((src_tab, pid)) = ws.pane_drag.take() {
            if let Some(dst_tab_idx) = self.hovered_tab(ws) {
                if dst_tab_idx != src_tab {
                    if let Some(pane) = ws.tabs.get_mut(src_tab).and_then(|t| t.take_pane(pid)) {
                        if let Some(dst_tab) = ws.tabs.get_mut(dst_tab_idx) {
                            dst_tab.add_pane(pid, pane, &self.cfg);
                            dst_tab.focus_id(pid);
                            ws.force_draw = true;
                            ws.window.request_redraw();
                        }
                    }
                }
            }
            self.set_split_cursor(ws, None);
            ws.force_draw = true;
            ws.window.request_redraw();
            return;
        }
        
        // Divider drag end
        if ws.split_drag.take().is_some() {
            self.set_split_cursor(ws, None);
            ws.force_draw = true;
            ws.window.request_redraw();
            return;
        }
        
        // App mouse release
        if ws.press_app_mouse {
            ws.press_app_mouse = false;
            if let Some(pid) = ws.press_pane {
                let tabs = &ws.tabs;
                let tab = tabs.active();
                if let Some(pane) = tab.and_then(|t| t.pane(pid)) {
                    if pane.term_mode().contains(TermMode::MOUSE_REPORT_CLICK) {
                        let local = self
                            .pane_cell_at(ws, 
                                ws.cursor_pos.map(|p| p.x).unwrap_or(0.0),
                                ws.cursor_pos.map(|p| p.y).unwrap_or(0.0),
                            )
                            .filter(|(id, _, _, _)| *id == pid)
                            .map(|(_, c, r, _)| (c, r))
                            .or(ws.press_local);
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
            ws.press_pane = None;
            ws.press_local = None;
            return;
        }
        
        // Selection finalize: copy to clipboard (and PRIMARY on X11)
        if ws.selecting {
            ws.selecting = false;
            if let Some(tab) = ws.tabs.active() {
                if let Some(pane) = tab.pane(ws.anchor.map(|a| a.0).unwrap_or(0)) {
                    pane.clear_selection();
                }
            }
            self.copy_primary(ws);
            ws.force_draw = true;
            ws.window.request_redraw();
            ws.press_pane = None;
            ws.press_local = None;
        }
    }
    
    fn on_middle_press(&mut self, ws: &mut WindowState) {
        let Some(pos) = ws.cursor_pos else {
            return;
        };
        
        // Tab bar: middle-click closes the tab (hidden strip: paste).
        let tab_click: Option<usize> = (|| {
            if !Self::strip_on(&self.cfg.tab_bar, ws) {
                return None;
            }
            let scale = ws.window.scale_factor() as f32;
            let g = ws.renderer.body_geometry(scale, Self::strip_on(&self.cfg.tab_bar, ws));
            if pos.y as f32 >= g.3 {
                return None;
            }
            let titles = self.tab_titles(ws);
            let adv = g.0 / scale.max(f32::EPSILON);
            let regions = mouse::tab_regions(&titles, adv, renderer::TAB_LEFT * scale, scale);
            mouse::tab_hit(&regions, pos.x as f32)
        })();
        
        if let Some(i) = tab_click {
            ws.tabs.close_at(i, &self.cfg);
            ws.force_draw = true;
            ws.window.request_redraw();
            return;
        }
        
        let Some((pid, col, vrow, _)) = self.pane_cell_at(ws, pos.x, pos.y) else {
            return;
        };
        
        // Middle-click focuses the pane first
        if let Some(tabs) = ws.tabs.active_mut() {
            if tabs.active_id() != pid {
                tabs.focus_id(pid);
                ws.force_draw = true;
                ws.window.request_redraw();
            }
        }
        
        let Some(tab) = ws.tabs.active() else {
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
            ws.press_app_middle = true;
            ws.press_pane = Some(pid);
            ws.press_local = Some((col, vrow));
            return;
        }
        
        // PRIMARY paste
        self.do_paste_primary(ws);
    }
    
    fn on_middle_release(&mut self, ws: &mut WindowState) {
        if !ws.press_app_middle {
            return;
        }
        ws.press_app_middle = false;
        if let Some(pid) = ws.press_pane {
            let tab = ws.tabs.active();
            if let Some(pane) = tab.and_then(|t| t.pane(pid)) {
                if pane.term_mode().contains(TermMode::MOUSE_REPORT_CLICK) {
                    let local = self
                        .pane_cell_at(ws,
                            ws.cursor_pos.map(|p| p.x).unwrap_or(0.0),
                            ws.cursor_pos.map(|p| p.y).unwrap_or(0.0),
                        )
                        .filter(|(id, _, _, _)| *id == pid)
                        .map(|(_, c, r, _)| (c, r))
                        .or(ws.press_local);
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
        ws.press_pane = None;
        ws.press_local = None;
    }
    
    fn on_cursor_moved(&mut self, ws: &mut WindowState, pos: PhysicalPosition<f64>) {
        // A hover change re-styles the tab strip (the close mark appears),
        // so it needs a redraw of its own.
        let hover_before = self.hovered_tab(ws);
        ws.cursor_pos = Some(pos);
        if self.hovered_tab(ws) != hover_before {
            ws.force_draw = true;
            ws.window.request_redraw();
        }
        
        // Divider drag: ratio from the pointer, clamped to livable panes.
        if let Some(drag) = ws.split_drag.as_ref() {
            let (tab_idx, path, dir, parent) =
                (drag.tab, drag.path.clone(), drag.dir, drag.parent);
            let mut drop_drag = false;
            let geom = (|| {
                let scale = ws.window.scale_factor() as f32;
                Some(ws.renderer.body_geometry(scale, Self::strip_on(&self.cfg.tab_bar, ws)))
            })();
            if let (Some((cell_w, line_h, left, top)), Some(tabs)) = (geom, ws.tabs.get_mut(tab_idx)) {
                if let Some(hit) =
                    mouse::hit_test(pos.x, pos.y, cell_w, line_h, left, top, 100_000, 10_000, 0)
                {
                    // Abort if the tree changed under the drag
                    if tabs.divider_info(&path).is_none() {
                        drop_drag = true;
                    } else {
                        let ratio = tab::ratio_from_pointer(
                            dir,
                            parent,
                            hit.col,
                            hit.grid_line as usize,
                        );
                        tabs.set_ratio(&path, dir, parent, ratio);
                        ws.force_draw = true;
                        ws.window.request_redraw();
                    }
                }
            }
            if drop_drag {
                ws.split_drag = None;
                self.set_split_cursor(ws, None);
            }
        }
        
        // Tab drag: reorder on hover
        if let Some(src) = ws.tab_drag {
            if let Some(dst) = self.hovered_tab(ws) {
                if src != dst {
                    ws.tabs.move_tab(src, dst);
                    ws.tab_drag = Some(dst);
                    ws.force_draw = true;
                    ws.window.request_redraw();
                }
            }
        }
        
        // Pane drag: show target tab
        if ws.pane_drag.is_some() {
            ws.force_draw = true;
            ws.window.request_redraw();
        }
        
        // Selection drag
        if ws.selecting {
            if let Some(anchor) = ws.anchor {
                let Some((pid, col, vrow, _)) = self.pane_cell_at(ws, pos.x, pos.y) else {
                    return;
                };
                if pid != anchor.0 {
                    return;
                }
                let Some(tab) = ws.tabs.active() else {
                    return;
                };
                let Some(pane) = tab.pane(pid) else {
                    return;
                };
                if ws.click_count == 1 {
                    // Char select: use begin_selection + update_selection
                    let start_point = Point::new(Line(anchor.1), Column(anchor.2));
                    let end_point = Point::new(Line(vrow as i32), Column(col));
                    if start_point != end_point {
                        pane.begin_selection(SelectionType::Simple, start_point, Side::Left);
                        pane.update_selection(end_point, Side::Right);
                    } else {
                        pane.clear_selection();
                    }
                }
                ws.force_draw = true;
                ws.window.request_redraw();
            }
        }
        
        // Scrollbar hover
        if let Some((bar, track_x, _, _)) = self.scrollbar_geom(ws) {
            let scale = ws.window.scale_factor() as f32;
            let bar_w = mouse::BAR_W * scale;
            let over_bar = pos.x as f32 >= track_x && pos.x as f32 <= track_x + bar_w;
            if over_bar && bar.thumb.is_some() {
                self.set_split_cursor(ws, Some(CursorIcon::Default));
            } else if ws.split_cursor.is_none() {
                self.set_split_cursor(ws, None);
            }
        }
    }
    
    fn on_wheel(&mut self, ws: &mut WindowState, delta: MouseScrollDelta) {
        let lines = match delta {
            MouseScrollDelta::LineDelta(_, y) => y as i32,
            MouseScrollDelta::PixelDelta(p) => (p.y / 20.0) as i32,
        };
        if lines == 0 {
            return;
        }
        
        // Scrollbar drag
        if let Some(drag) = ws.scroll_drag {
            if let Some((bar, track_x, history, viewport_rows)) = self.scrollbar_geom(ws) {
                let scale = ws.window.scale_factor() as f32;
                let track_h = (bar.track_h * scale).max(1.0);
                let thumb_h = bar.thumb.map(|t| t.1 * scale).unwrap_or(0.0);
                let max_scroll = history.saturating_sub(viewport_rows);
                let new_scroll = ((drag + lines as f32 / track_h.max(1.0)) * max_scroll as f32)
                    .clamp(0.0, max_scroll as f32)
                    as usize;
                if let Some(tab) = ws.tabs.active_mut() {
                    tab.scroll_to(new_scroll);
                    ws.force_draw = true;
                    ws.window.request_redraw();
                }
            }
            return;
        }
        
        // Scrollbar thumb hit test to start drag
        if let Some((bar, track_x, _, _)) = self.scrollbar_geom(ws) {
            if let Some(pos) = ws.cursor_pos {
                let scale = ws.window.scale_factor() as f32;
                let bar_w = mouse::BAR_W * scale;
                let over_bar = pos.x as f32 >= track_x && pos.x as f32 <= track_x + bar_w;
                if over_bar {
                    if let Some(thumb) = bar.thumb {
                        let thumb_y = thumb.0;
                        let thumb_h = thumb.1;
                        let thumb_top = bar.track_top + thumb_y * scale;
                        let thumb_bottom = thumb_top + thumb_h * scale;
                        if pos.y as f32 >= thumb_top && pos.y as f32 <= thumb_bottom {
                            ws.scroll_drag = Some((pos.y as f32 - thumb_top) / (thumb_h * scale).max(1.0));
                            return;
                        }
                    }
                }
            }
        }
        
        // Regular scroll: forward to active pane
        if let Some(tab) = ws.tabs.active_mut() {
            if lines > 0 {
                tab.scroll_lines(-lines);
            } else {
                tab.scroll_lines(-lines);
            }
            ws.force_draw = true;
            ws.window.request_redraw();
        }
    }
    
    fn on_resized(&mut self, ws: &mut WindowState, size: winit::dpi::PhysicalSize<u32>) {
        ws.renderer.resize(size.width, size.height);
        let (cols, rows) = Self::estimate_grid(&self.cfg.tab_bar, ws, size.width, size.height);
        ws.tabs.resize_all(cols, rows);
        ws.force_draw = true;
        ws.window.request_redraw();
    }
    
    fn on_redraw_requested(&mut self, ws: &mut WindowState) {
        let tabs = &ws.tabs;
        let titles = self.tab_titles(ws);
        let active = tabs.active_index();
        let hover = self.hovered_tab(ws);
        let blink = ws.blink_on;
        let strip = Self::strip_on(&self.cfg.tab_bar, ws);
        
        // Drain reader wakeups + damage; skip the whole present when
        // nothing changed since the last frame.
        let pty_dirty = tabs.take_dirty();
        let term_damaged = tabs
            .active()
            .map(|tab| tab.take_damage())
            .unwrap_or(true);
        let ui_changed = titles != ws.last_titles
            || active != ws.last_active
            || blink != ws.last_blink_state
            || ws.force_draw;
        if !(pty_dirty || term_damaged || ui_changed) {
            return;
        }
        
        let r = &mut ws.renderer;
        let t = &ws.tabs;
        
        // All panes of the ACTIVE tab (inactive tabs don't draw).
        let fg = self.cfg.fg_rgb();
        let bg = self.cfg.bg_rgb();
        let mut panes = Vec::new();
        let mut divs = Vec::new();
        if let Some(tab) = t.active() {
            let layout = tab.layout();
            let focus = tab.active_id();
            // Search highlights: grid line -> viewport row mapping
            let search_rows = ws.search.as_ref().and_then(|s| {
                if s.matches().is_empty() {
                    None
                } else {
                    let offset = tab.display_offset() as i32;
                    let (_, rows) = tab.grid_size();
                    let highlight = Self::search_highlight_rgb();
                    Some((s, offset, rows as i32, highlight))
                }
            });
            for (id, rect) in layout.panes {
                if let Some(pane) = tab.pane(id) {
                    let cursor_on = if id == focus { blink } else { true };
                    let (mut rows, cursor) =
                        pane.rendered_rows(fg, bg, cursor_on);
                    // Apply search match highlights to visible rows.
                    if let Some((state, offset, rows_cnt, highlight)) = search_rows.as_ref() {
                        if id == focus {
                            for m in state.matches() {
                                let vrow = m.line + offset;
                                if vrow >= 0 && vrow < *rows_cnt {
                                    let row = &mut rows[vrow as usize];
                                    let rgb = *highlight;
                                    row.bg_rects.push((
                                        m.col_start,
                                        m.col_end.saturating_sub(1),
                                        rgb,
                                    ));
                                }
                            }
                        }
                    }
                    // Apply copymode selection highlights to visible rows.
                    if let Some(copymode) = &ws.copymode {
                        if copymode.pane == id && copymode.anchor.is_some() {
                            let anchor = copymode.anchor.unwrap();
                            let cursor = copymode.cursor;
                            let cols = if let Ok(term) = pane.term.lock() { term.columns().max(1) as usize } else { 80 };
                            let (start_row, end_row, start_col, end_col) = if copymode.line_mode {
                                let start = anchor.0.min(cursor.0);
                                let end = anchor.0.max(cursor.0);
                                (start, end, 0, cols - 1)
                            } else {
                                let start_r = anchor.0.min(cursor.0);
                                let end_r = anchor.0.max(cursor.0);
                                let start_c = anchor.1.min(cursor.1);
                                let end_c = anchor.1.max(cursor.1);
                                (start_r, end_r, start_c, end_c)
                            };
                            let highlight = Self::copymode_highlight_rgb();
                            for vrow in start_row..=end_row {
                                if vrow < rows.len() {
                                    let row = &mut rows[vrow];
                                    let sc = if copymode.line_mode { 0 } else { start_col };
                                    let ec = if copymode.line_mode { row.text.chars().count() } else { end_col };
                                    if sc <= ec {
                                        row.bg_rects.push((sc, ec, highlight));
                                    }
                                }
                            }
                        }
                    }
                    panes.push(PaneDraw {
                        id,
                        rect,
                        rows,
                        cursor,
                        focused: id == focus,
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
        let flash = ws
            .bell_flash_until
            .map(|until| Instant::now() < until)
            .unwrap_or(false);
        r.set_flash(flash);
        let search_bar = ws.search.as_ref().map(|s| (s.query.as_str(), s.matches().len()));
        r.draw(
            &titles,
            t.active_index(),
            hover,
            ws.toast.as_ref().map(|(text, _)| text.as_str()),
            &panes,
            &divs,
            scroll,
            strip,
            search_bar,
            ws.palette.as_ref(),
        );
        ws.last_titles = titles;
        ws.last_active = active;
        ws.last_blink_state = blink;
        ws.force_draw = false;
    }
    
    // ---- Clipboard ----
    
    fn do_copy(&mut self, ws: &mut WindowState) {
        let text = match ws.tabs.active().and_then(|t| t.copy_text()) {
            Some(s) => s,
            None => return,
        };
        let chars = text.chars().count();
        match self.clipboard() {
            Some(cb) => {
                if let Err(e) = cb.set_text(text) {
                    log::error!("copy failed: {e}");
                    return;
                }
                self.set_toast(ws, format!("copied \u{00b7} {chars} chars"));
            }
            None => log::error!("copy failed: no clipboard"),
        }
    }
    
    fn do_paste(&mut self, ws: &mut WindowState) {
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
        let chars = text.chars().count();
        self.write_paste(ws, text);
        self.set_toast(ws, format!("pasted \u{00b7} {chars} chars"));
    }
    
    /// X11 middle-click: paste the PRIMARY selection buffer.
    fn do_paste_primary(&mut self, ws: &mut WindowState) {
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
        let chars = text.chars().count();
        self.write_paste(ws, text);
        self.set_toast(ws, format!("pasted \u{00b7} {chars} chars"));
    }
    
    /// X11 select-to-copy: mirror the active selection into PRIMARY.
    fn copy_primary(&mut self, ws: &mut WindowState) {
        let has_sel = ws
            .tabs
            .active()
            .map(|t| t.has_selection())
            .unwrap_or(false);
        if !has_sel {
            return;
        }
        let text = match ws.tabs.active().and_then(|t| t.copy_text()) {
            Some(s) => s,
            None => return,
        };
        if let Some(cb) = self.clipboard() {
            if let Err(e) = cb.set().clipboard(LinuxClipboardKind::Primary).text(text) {
                log::error!("primary copy failed: {e}");
            }
        }
    }
    
    /// Shared paste core: normalize newlines, honor bracketed paste.
    fn write_paste(&mut self, ws: &mut WindowState, text: String) {
        if text.is_empty() {
            return;
        }
        let Some(tab) = ws.tabs.active() else {
            return;
        };
        let body = text.replace("\r\n", "\r").replace('\n', "\r");
        if tab.bracketed_paste() {
            tab.write(b"\x1b[200~");
            tab.write(body.as_bytes());
            tab.write(b"\x1b[201~");
        } else {
            tab.write(body.as_bytes());
        }
    }
    
    // ---- Search ----
    
    /// Open/close the search overlay.
    fn toggle_search(&mut self, ws: &mut WindowState) {
        if ws.search.is_some() {
            ws.search = None;
        } else {
            ws.search = Some(SearchState::new(String::new()));
        }
        ws.force_draw = true;
        ws.window.request_redraw();
    }
    
    /// Close the active search.
    fn search_cancel(&mut self, ws: &mut WindowState) {
        if ws.search.is_some() {
            ws.search = None;
            ws.force_draw = true;
            ws.window.request_redraw();
        }
    }
    
    /// Advance to the next match and scroll the focused pane to it.
    fn search_next(&mut self, ws: &mut WindowState) {
        let (id, m) = {
            let Some(tab) = ws.tabs.active_mut() else {
                return;
            };
            let id = tab.active_id();
            let Some(pane) = tab.pane(id) else {
                return;
            };
            let Some(state) = ws.search.as_mut() else {
                return;
            };
            let term = match pane.term.lock() {
                Ok(t) => t,
                Err(_) => return,
            };
            state.search(&term);
            drop(term);
            let Some(m) = state.next_match() else {
                return;
            };
            (id, m)
        };
        let Some(tab) = ws.tabs.active_mut() else {
            return;
        };
        Self::scroll_pane_to_match(tab, id, &m);
        ws.force_draw = true;
        ws.window.request_redraw();
    }
    
    /// Advance to the previous match and scroll the focused pane to it.
    fn search_prev(&mut self, ws: &mut WindowState) {
        let (id, m) = {
            let Some(tab) = ws.tabs.active_mut() else {
                return;
            };
            let id = tab.active_id();
            let Some(pane) = tab.pane(id) else {
                return;
            };
            let Some(state) = ws.search.as_mut() else {
                return;
            };
            let term = match pane.term.lock() {
                Ok(t) => t,
                Err(_) => return,
            };
            state.search(&term);
            drop(term);
            let Some(m) = state.prev_match() else {
                return;
            };
            (id, m)
        };
        let Some(tab) = ws.tabs.active_mut() else {
            return;
        };
        Self::scroll_pane_to_match(tab, id, &m);
        ws.force_draw = true;
        ws.window.request_redraw();
    }
    
    /// Scroll the focused pane so `match`'s grid line lands inside the viewport.
    fn scroll_pane_to_match(tab: &mut tab::Tab, _id: tab::PaneId, m: &SearchMatch) {
        let (_cols, rows) = tab.grid_size();
        let offset = tab.display_offset();
        let target = m.line as i32;
        let view_top = offset as i32;
        let view_bottom = view_top + rows as i32 - 1;
        if target >= view_top && target <= view_bottom {
            return;
        }
        let new_offset = target.saturating_sub(rows as i32 / 3).max(0) as usize;
        tab.scroll_to(new_offset);
    }
    
    /// Handle a typed character while search is active.
    fn search_type_char(&mut self, ws: &mut WindowState, ch: char) -> bool {
        let Some(state) = ws.search.as_mut() else {
            return false;
        };
        state.query.push(ch);
        self.rescan_search(ws);
        true
    }
    
    /// Handle backspace while search is active.
    fn search_backspace(&mut self, ws: &mut WindowState) {
        let mut popped = false;
        if let Some(state) = ws.search.as_mut() {
            popped = state.query.pop().is_some();
        }
        if popped {
            self.rescan_search(ws);
        }
    }
    
    /// Re-scan the focused pane's term with the current query and redraw.
    fn rescan_search(&mut self, ws: &mut WindowState) {
        let Some(tab) = ws.tabs.active() else {
            return;
        };
        let id = tab.active_id();
        let Some(pane) = tab.pane(id) else {
            return;
        };
        let Ok(term) = pane.term.lock() else {
            return;
        };
        let Some(state) = ws.search.as_mut() else {
            drop(term);
            return;
        };
        state.search(&term);
        drop(term);
        ws.force_draw = true;
        ws.window.request_redraw();
    }
    
    /// Return a search highlight color (linearized amber) for match bg_rects.
    fn search_highlight_rgb() -> (u8, u8, u8) {
        (255, 200, 50)
    }
    
    /// Return a copymode selection highlight color (linearized blue).
    fn copymode_highlight_rgb() -> (u8, u8, u8) {
        (100, 150, 255)
    }
    
    // ---- Command Palette ----
    
    /// Toggle the command palette overlay.
    fn toggle_palette(&mut self, ws: &mut WindowState) {
        if ws.palette.is_none() {
            ws.palette = Some(PaletteState::new());
        }
        if let Some(palette) = ws.palette.as_mut() {
            palette.toggle();
            log::info!("palette: {}", if palette.visible { "on" } else { "off" });
        }
        ws.force_draw = true;
        ws.window.request_redraw();
    }
    
    /// Handle keyboard input when palette is active.
    fn handle_palette_key(&mut self, event_loop: &ActiveEventLoop, ws: &mut WindowState, event: &KeyEvent) {
        let Some(palette) = ws.palette.as_mut() else { return };
        match &event.logical_key {
            Key::Named(NamedKey::Escape) => {
                palette.close();
            }
            Key::Named(NamedKey::ArrowDown) => {
                palette.select_next();
            }
            Key::Named(NamedKey::ArrowUp) => {
                palette.select_prev();
            }
            Key::Named(NamedKey::Enter) => {
                if let Some(action) = palette.execute_selected() {
                    palette.close();
                    self.execute_palette_action(event_loop, ws, action);
                    return;
                }
            }
            Key::Named(NamedKey::Backspace) => {
                palette.backspace();
            }
            _ => {
                if let Some(text) = &event.text {
                    for c in text.chars() {
                        if !c.is_control() {
                            palette.type_char(c);
                        }
                    }
                }
            }
        }
        ws.force_draw = true;
        ws.window.request_redraw();
    }
    
    /// Execute a palette action (converts to InputAction or handles specially).
    fn execute_palette_action(&mut self, event_loop: &ActiveEventLoop, ws: &mut WindowState, action: PaletteAction) {
        match action {
            // Theme actions are handled specially
            PaletteAction::ThemeTokyoNight => self.set_theme(ws, "tokyo-night"),
            PaletteAction::ThemeCatppuccinMocha => self.set_theme(ws, "catppuccin-mocha"),
            PaletteAction::ThemeDracula => self.set_theme(ws, "dracula"),
            PaletteAction::ThemeGruvboxDark => self.set_theme(ws, "gruvbox-dark"),
            PaletteAction::ThemeCustom => self.set_theme(ws, "custom"),
            // Session actions
            PaletteAction::SessionSave => self.handle_session_save(ws),
            PaletteAction::SessionLoad => self.handle_session_load(ws),
            PaletteAction::SessionList => self.handle_session_list(ws),
            // Performance HUD
            PaletteAction::TogglePerformanceHud => {
                log::info!("Performance HUD toggle not yet implemented");
            }
            // All other actions map to InputAction
            _ => {
                if let Some(input_action) = action.to_input_action() {
                    // Reuse the keyboard handling logic by calling handle_keyboard recursively
                    // We can't easily do that, so we'll handle the common ones directly
                    self.handle_input_action_direct(event_loop, ws, input_action);
                }
            }
        }
    }
    
    /// Handle InputAction directly without going through map_key.
    fn handle_input_action_direct(&mut self, event_loop: &ActiveEventLoop, ws: &mut WindowState, action: crate::input::InputAction) {
        use crate::input::InputAction;
        match action {
            InputAction::NewTab => {
                if let Err(e) = ws.tabs.new_tab(&self.cfg) {
                    log::error!("new tab failed: {e:#}");
                }
            }
            InputAction::NewWindow => {
                if let Some(new_window) = self.create_window_with_config(event_loop) {
                    log::info!("created new window: {:?}", new_window.id());
                }
            }
            InputAction::ClosePane => {
                ws.tabs.close_active_pane(&self.cfg);
            }
            InputAction::SplitV => {
                match ws.tabs.split_active_tab(crate::tab::SplitDir::V, &self.cfg) {
                    Ok(false) => log::info!("split refused: pane too small"),
                    Err(e) => log::error!("split failed: {e:#}"),
                    _ => {}
                }
            }
            InputAction::SplitH => {
                match ws.tabs.split_active_tab(crate::tab::SplitDir::H, &self.cfg) {
                    Ok(false) => log::info!("split refused: pane too small"),
                    Err(e) => log::error!("split failed: {e:#}"),
                    _ => {}
                }
            }
            InputAction::NextTab => ws.tabs.next(),
            InputAction::PrevTab => ws.tabs.prev(),
            InputAction::FocusPane(dir) => {
                ws.tabs.focus_active_tab(dir);
            }
            InputAction::ZoomPane => ws.tabs.zoom_active_tab(),
            InputAction::FontZoomIn => self.zoom_font(ws, 1.0),
            InputAction::FontZoomOut => self.zoom_font(ws, -1.0),
            InputAction::FontZoomReset => self.reset_font(ws),
            InputAction::RenameTab => self.start_rename(ws),
            InputAction::UnpinTab => ws.tabs.unpin_active(),
            InputAction::BroadcastToggle => {
                ws.broadcast = !ws.broadcast;
                log::info!("broadcast input: {}", if ws.broadcast { "on" } else { "off" });
            }
            InputAction::CopymodeToggle => self.toggle_copymode(ws),
            InputAction::SearchToggle => self.toggle_search(ws),
            InputAction::Copy => self.do_copy(ws),
            InputAction::Paste => self.do_paste(ws),
            InputAction::SelectTab(i) => ws.tabs.select(i),
            _ => {}
        }
        ws.force_draw = true;
        ws.window.request_redraw();
    }
    
    /// Set theme from palette.
    fn set_theme(&mut self, ws: &mut WindowState, preset: &str) {
        use crate::config::Theme;
        let mut cfg = NeotermConfig::load();
        if preset == "custom" {
            cfg.theme.preset = None;
        } else if Theme::preset(preset).is_some() {
            cfg.theme.preset = Some(preset.to_string());
        } else {
            log::warn!("Unknown theme preset: {}", preset);
            return;
        }
        if let Err(e) = cfg.save() {
            log::error!("Failed to save config: {}", e);
            return;
        }
        self.cfg = cfg;
        // Update renderer with new theme
        ws.renderer.update_theme(&self.cfg);
        log::info!("Theme set to {}", preset);
    }
    
    /// Handle session save from palette.
    fn handle_session_save(&mut self, ws: &mut WindowState) {
        use crate::session::{build_session_state, named_session_path};
        let state = build_session_state(self);
        let name = format!("session-{}", chrono::Local::now().format("%Y%m%d-%H%M%S"));
        let path = named_session_path(&name);
        match state.save(&path) {
            Ok(_) => log::info!("Session saved as {}", name),
            Err(e) => log::error!("Failed to save session: {}", e),
        }
    }
    
    /// Handle session load from palette.
    fn handle_session_load(&mut self, ws: &mut WindowState) {
        // For now just log - would need a session picker UI
        log::info!("Session load from palette not yet implemented");
    }
    
    /// Handle session list from palette.
    fn handle_session_list(&mut self, ws: &mut WindowState) {
        log::info!("Session list from palette not yet implemented");
    }
    
    // ---- Copymode (Vi-style selection) ----
    
    /// Ctrl+Shift+Space: toggle Vi-style copy mode.
    fn toggle_copymode(&mut self, ws: &mut WindowState) {
        if ws.copymode.is_some() {
            ws.copymode = None;
            log::info!("copymode: off");
        } else {
            let Some(tab) = ws.tabs.active() else { return };
            let pane_id = tab.active_id();
            let Some(pane) = tab.pane(pane_id) else { return };
            let Ok(term) = pane.term.lock() else { return };
            let cursor = term.grid().cursor.point;
            let cols = term.columns().max(1) as usize;
            let rows = term.screen_lines().max(1) as usize;
            // Convert cursor to viewport-relative coordinates
            let display_offset = term.grid().display_offset() as i32;
            let cursor_row = cursor.line.0 as i32 - display_offset;
            let cursor_col = cursor.column.0 as usize;
            let cursor_row = cursor_row.clamp(0, rows as i32 - 1) as usize;
            ws.copymode = Some(CopymodeState {
                pane: pane_id,
                cursor: (cursor_row, cursor_col),
                anchor: None,
                line_mode: false,
                scroll_offset: 0,
            });
            log::info!("copymode: on");
        }
        ws.force_draw = true;
        ws.window.request_redraw();
    }
    
    /// Exit copymode without copying.
    fn copymode_exit(&mut self, ws: &mut WindowState) {
        ws.copymode = None;
        log::info!("copymode: off");
        ws.force_draw = true;
        ws.window.request_redraw();
    }
    
    /// Move cursor up in copymode.
    fn copymode_up(&mut self, ws: &mut WindowState) {
        let history = self.get_history_size(ws);
        if let Some(state) = ws.copymode.as_mut() {
            if state.cursor.0 > 0 {
                state.cursor.0 -= 1;
            } else if state.scroll_offset < history {
                state.scroll_offset += 1;
            }
            if state.anchor.is_some() {
                // Extend selection
            }
        }
        ws.force_draw = true;
        ws.window.request_redraw();
    }
    
    /// Move cursor down in copymode.
    fn copymode_down(&mut self, ws: &mut WindowState) {
        let history = self.get_history_size(ws);
        let max_row = ws.renderer.grid_size(ws.window.inner_size().width, ws.window.inner_size().height, ws.window.scale_factor() as f32, true).1 as usize;
        if let Some(state) = ws.copymode.as_mut() {
            if state.cursor.0 < max_row.saturating_sub(1) {
                state.cursor.0 += 1;
            } else if state.scroll_offset > 0 {
                state.scroll_offset -= 1;
            }
            if state.anchor.is_some() {
                // Extend selection
            }
        }
        ws.force_draw = true;
        ws.window.request_redraw();
    }
    
    /// Move cursor left in copymode.
    fn copymode_left(&mut self, ws: &mut WindowState) {
        if let Some(state) = ws.copymode.as_mut() {
            if state.cursor.1 > 0 {
                state.cursor.1 -= 1;
            }
            if state.anchor.is_some() {
                // Extend selection
            }
        }
        ws.force_draw = true;
        ws.window.request_redraw();
    }
    
    /// Move cursor right in copymode.
    fn copymode_right(&mut self, ws: &mut WindowState) {
        let cols = ws.renderer.grid_size(ws.window.inner_size().width, ws.window.inner_size().height, ws.window.scale_factor() as f32, true).0 as usize;
        if let Some(state) = ws.copymode.as_mut() {
            if state.cursor.1 + 1 < cols {
                state.cursor.1 += 1;
            }
            if state.anchor.is_some() {
                // Extend selection
            }
        }
        ws.force_draw = true;
        ws.window.request_redraw();
    }
    
    /// Page up in copymode.
    fn copymode_page_up(&mut self, ws: &mut WindowState) {
        let viewport_rows = ws.renderer.grid_size(ws.window.inner_size().width, ws.window.inner_size().height, ws.window.scale_factor() as f32, true).1 as usize;
        let history = self.get_history_size(ws);
        if let Some(state) = ws.copymode.as_mut() {
            state.scroll_offset = (state.scroll_offset + viewport_rows / 2).min(history);
        }
        ws.force_draw = true;
        ws.window.request_redraw();
    }
    
    /// Page down in copymode.
    fn copymode_page_down(&mut self, ws: &mut WindowState) {
        let viewport_rows = ws.renderer.grid_size(ws.window.inner_size().width, ws.window.inner_size().height, ws.window.scale_factor() as f32, true).1 as usize;
        if let Some(state) = ws.copymode.as_mut() {
            state.scroll_offset = state.scroll_offset.saturating_sub(viewport_rows / 2);
        }
        ws.force_draw = true;
        ws.window.request_redraw();
    }
    
    /// Home in copymode (beginning of line).
    fn copymode_home(&mut self, ws: &mut WindowState) {
        if let Some(state) = ws.copymode.as_mut() {
            state.cursor.1 = 0;
        }
        ws.force_draw = true;
        ws.window.request_redraw();
    }
    
    /// End in copymode (end of line).
    fn copymode_end(&mut self, ws: &mut WindowState) {
        let cols = ws.renderer.grid_size(ws.window.inner_size().width, ws.window.inner_size().height, ws.window.scale_factor() as f32, true).0 as usize;
        if let Some(state) = ws.copymode.as_mut() {
            state.cursor.1 = cols.saturating_sub(1);
        }
        ws.force_draw = true;
        ws.window.request_redraw();
    }
    
    /// Start/extend character selection in copymode (v).
    fn copymode_select(&mut self, ws: &mut WindowState) {
        if let Some(state) = ws.copymode.as_mut() {
            if state.anchor.is_none() {
                state.anchor = Some(state.cursor);
            }
        }
        ws.force_draw = true;
        ws.window.request_redraw();
    }
    
    /// Start/extend line selection in copymode (V).
    fn copymode_select_line(&mut self, ws: &mut WindowState) {
        if let Some(state) = ws.copymode.as_mut() {
            state.line_mode = true;
            if state.anchor.is_none() {
                state.anchor = Some((state.cursor.0, 0));
            }
        }
        ws.force_draw = true;
        ws.window.request_redraw();
    }
    
    /// Copy selection and exit copymode (y).
    fn copymode_copy(&mut self, ws: &mut WindowState) {
        if let Some(state) = ws.copymode.take() {
            if let Some(tab) = ws.tabs.active() {
                if let Some(pane) = tab.pane(state.pane) {
                    if let Ok(term) = pane.term.lock() {
                        let text = if let Some(anchor) = state.anchor {
                            let (anchor_row, anchor_col) = anchor;
                            let (cursor_row, cursor_col) = state.cursor;
                            let (start_row, end_row, start_col, end_col) = if state.line_mode {
                                // Line mode: select full lines from anchor to cursor
                                let start = anchor_row.min(cursor_row);
                                let end = anchor_row.max(cursor_row);
                                (start, end, 0, term.columns().max(1) as usize - 1)
                            } else {
                                // Character mode: select rectangle from anchor to cursor
                                let start_r = anchor_row.min(cursor_row);
                                let end_r = anchor_row.max(cursor_row);
                                let start_c = anchor_col.min(cursor_col);
                                let end_c = anchor_col.max(cursor_col);
                                (start_r, end_r, start_c, end_c)
                            };
                            // Convert viewport-relative rows to grid line numbers
                            let display_offset = term.grid().display_offset() as i32;
                            let start_line = (start_row as i32 + display_offset) as i32;
                            let end_line = (end_row as i32 + display_offset) as i32;
                            let grid_start = Point::new(Line(start_line), Column(start_col));
                            let grid_end = Point::new(Line(end_line), Column(end_col));
                            term.bounds_to_string(grid_start, grid_end)
                        } else {
                            String::new()
                        };
                        if !text.trim().is_empty() {
                            if let Some(cb) = self.clipboard() {
                                if let Err(e) = cb.set().text(text) {
                                    log::error!("copymode copy failed: {e}");
                                } else {
                                    log::info!("copymode: copied");
                                }
                            }
                        } else {
                            log::info!("copymode: no selection to copy");
                        }
                    }
                }
            }
        }
        ws.force_draw = true;
        ws.window.request_redraw();
    }
    
    /// Get history size for the active pane.
    fn get_history_size(&self, ws: &WindowState) -> usize {
        if let Some(tab) = ws.tabs.active() {
            if let Some(pane) = tab.pane(tab.active_id()) {
                if let Ok(term) = pane.term.lock() {
                    return term.grid().history_size();
                }
            }
        }
        0
    }
    
    /// Handle keyboard input while copymode is active.
    fn handle_copymode_key(&mut self, ws: &mut WindowState, event: &KeyEvent) {
        use input::InputAction;
        use winit::keyboard::{Key, NamedKey};
        use winit::event::ElementState;
        
        if event.state != ElementState::Pressed {
            return;
        }
        
        let alt = ws
            .tabs
            .active()
            .map(|t| t.in_alt_screen())
            .unwrap_or(false);
        
        // Allow chrome actions to pass through (e.g., Ctrl+Shift+F for search)
        if let Some(action) = input::chrome_action(&event.logical_key, self.mods.state()) {
            match action {
                InputAction::SearchToggle | InputAction::CopymodeToggle => {
                    // These are handled by map_key below
                }
                _ => return, // Other chrome actions don't apply in copymode
            }
        }
        
        match input::map_key(event, &self.mods, alt) {
            InputAction::CopymodeUp => self.copymode_up(ws),
            InputAction::CopymodeDown => self.copymode_down(ws),
            InputAction::CopymodeLeft => self.copymode_left(ws),
            InputAction::CopymodeRight => self.copymode_right(ws),
            InputAction::CopymodePageUp => self.copymode_page_up(ws),
            InputAction::CopymodePageDown => self.copymode_page_down(ws),
            InputAction::CopymodeHome => self.copymode_home(ws),
            InputAction::CopymodeEnd => self.copymode_end(ws),
            InputAction::CopymodeSelect => self.copymode_select(ws),
            InputAction::CopymodeSelectLine => self.copymode_select_line(ws),
            InputAction::CopymodeCopy => self.copymode_copy(ws),
            InputAction::CopymodeExit => self.copymode_exit(ws),
            InputAction::CopymodeToggle => self.toggle_copymode(ws),
            InputAction::SearchToggle => self.toggle_search(ws),
            InputAction::SearchNext => self.search_next(ws),
            InputAction::SearchPrev => self.search_prev(ws),
            InputAction::SearchCancel => self.search_cancel(ws),
            _ => {
                // Escape, q, Ctrl+C exit copymode
                match &event.logical_key {
                    Key::Named(NamedKey::Escape) => self.copymode_exit(ws),
                    Key::Character(s) if s == "q" || s == "Q" => self.copymode_exit(ws),
                    Key::Character(s) if s == "c" && self.mods.state().control_key() => self.copymode_exit(ws),
                    _ => {}
                }
            }
        }
    }
    
    // ---- Rename ----
    
    /// Ctrl+Shift+R: start editing the active tab's title.
    fn start_rename(&mut self, ws: &mut WindowState) {
        let (tab, text) = match ws.tabs.active() {
            Some(tabs) => (
                ws.tabs.active_index(),
                tabs.title(),
            ),
            None => return,
        };
        ws.rename = Some(RenameState {
            tab,
            text,
            fresh: true,
        });
        ws.force_draw = true;
        ws.window.request_redraw();
    }
    
    /// Keyboard input while a rename is live: every key edits the title.
    fn on_rename_key(&mut self, ws: &mut WindowState, event: &KeyEvent) {
        enum Next {
            Commit(String),
            Cancel,
            Edit,
        }
        
        if event.state != ElementState::Pressed {
            return;
        }
        let ctrl = self.mods.state().control_key();
        let next = {
            let Some(state) = ws.rename.as_mut() else {
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
                        if let Key::Character(c) = &event.logical_key {
                            if c.eq_ignore_ascii_case("u") {
                                state.text.clear();
                                state.fresh = false;
                            }
                        }
                    } else if let Some(text) = &event.text {
                        if !text.chars().any(|c| c.is_control()) {
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
                let tab = ws.rename.as_ref().map(|s| s.tab).unwrap_or(0);
                ws.rename = None;
                if let Some(tabs) = ws.tabs.get_mut(tab) {
                    tabs.set_pin(if name.is_empty() { None } else { Some(name) });
                }
            }
            Next::Cancel => ws.rename = None,
            Next::Edit => {}
        }
        ws.force_draw = true;
        ws.window.request_redraw();
    }

    /// Create a new window with its own renderer and tabs.
    fn create_window(&mut self, event_loop: &ActiveEventLoop) -> Result<Arc<Window>> {
        let attrs = apply_window_effects(
            Window::default_attributes()
                .with_title("NeoTerm 0.3")
                .with_inner_size(Renderer::window_logical_size()),
            &self.cfg,
        );
        let window = Arc::new(event_loop.create_window(attrs)?);
        let window_id = window.id();
        
        let renderer = pollster::block_on(Renderer::new(window.clone(), event_loop, &self.cfg));
        
        let mut tabs = Tabs::new(&self.cfg, self.proxy.clone(), self.cwd.clone())?;
        let size = window.inner_size();
        let scale = window.scale_factor() as f32;
        let strip = true; // show strip on initial window
        let (cols, rows) = renderer.grid_size(size.width, size.height, scale, strip);
        tabs.resize_all(cols, rows);
        
        let state = WindowState {
            window: window.clone(),
            renderer,
            tabs,
            last_titles: Vec::new(),
            last_active: usize::MAX,
            blink_timer: Instant::now(),
            blink_on: true,
            last_blink_state: true,
            force_draw: true,
            last_strip: true,
            bell_flash_until: None,
            toast: None,
            rename: None,
            search: None,
            copymode: None,
            broadcast: false,
            palette: None,
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
            tab_drag: None,
            pane_drag: None,
            split_cursor: None,
        };
        
        self.windows.insert(window_id, state);
        self.setup_config_watch();
        
        log::info!(
            "NeoTerm v0.3 ready: Ctrl+Shift+T new tab, Ctrl+Shift+W close, \
             Ctrl+Shift+F search, Ctrl+Tab switch, Ctrl+Shift+± font size, Ctrl+Shift+N new window"
        );
        
        Ok(window)
    }

    /// Create a new window with the same config and working directory as an existing window.
    fn create_window_with_config(&mut self, event_loop: &ActiveEventLoop) -> Option<Arc<Window>> {
        let attrs = apply_window_effects(
            Window::default_attributes()
                .with_title("NeoTerm 0.3")
                .with_inner_size(Renderer::window_logical_size()),
            &self.cfg,
        );
        let window = Arc::new(event_loop.create_window(attrs).ok()?);
        let window_id = window.id();
        
        let renderer = pollster::block_on(Renderer::new(window.clone(), event_loop, &self.cfg));
        
        let mut tabs = Tabs::new(&self.cfg, self.proxy.clone(), self.cwd.clone())
            .ok()?;
        let size = window.inner_size();
        let scale = window.scale_factor() as f32;
        let strip = true; // show strip on initial window
        let (cols, rows) = renderer.grid_size(size.width, size.height, scale, strip);
        tabs.resize_all(cols, rows);
        
        let state = WindowState {
            window: window.clone(),
            renderer,
            tabs,
            last_titles: Vec::new(),
            last_active: usize::MAX,
            blink_timer: Instant::now(),
            blink_on: true,
            last_blink_state: true,
            force_draw: true,
            last_strip: true,
            bell_flash_until: None,
            toast: None,
            rename: None,
            search: None,
            copymode: None,
            broadcast: false,
            palette: None,
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
            tab_drag: None,
            pane_drag: None,
            split_cursor: None,
        };
        
        self.windows.insert(window_id, state);
        Some(window)
    }

    /// Handle keyboard input for a specific window.
    fn handle_keyboard(&mut self, event_loop: &ActiveEventLoop, ws: &mut WindowState, event: &KeyEvent) {
        // A live rename owns the keyboard: every key edits the
        // title instead of reaching the shell.
        if ws.rename.is_some() {
            self.on_rename_key(ws, event);
            return;
        }
        // A live palette owns the keyboard: keys navigate/filter/select.
        // Check visibility first without mutable borrow.
        let palette_visible = ws.palette.as_ref().map(|p| p.visible).unwrap_or(false);
        if palette_visible && event.state == ElementState::Pressed {
            self.handle_palette_key(event_loop, ws, event);
            return;
        }
        // A live copymode owns the keyboard: keys navigate/select/copy.
        if ws.copymode.is_some() && event.state == ElementState::Pressed {
            self.handle_copymode_key(ws, event);
            return;
        }
        // A live search owns printable keys: they append to the
        // query instead of reaching the shell. Chrome actions
        // (Ctrl+Shift+F etc.) still pass through to map_key below.
        if ws.search.is_some() && event.state == ElementState::Pressed {
            let shift = self.mods.state().shift_key();
            match &event.logical_key {
                Key::Named(NamedKey::Backspace) => {
                    self.search_backspace(ws);
                    return;
                }
                Key::Named(NamedKey::Escape) => {
                    self.search_cancel(ws);
                    return;
                }
                Key::Named(NamedKey::Enter) => {
                    // Enter: next match, Shift+Enter: previous match
                    if shift {
                        self.search_prev(ws);
                    } else {
                        self.search_next(ws);
                    }
                    return;
                }
                _ => {}
            }
            // Any printable text gets appended to the query.
            if let Some(text) = &event.text {
                for c in text.chars() {
                    if !c.is_control() {
                        self.search_type_char(ws, c);
                        return;
                    }
                }
            }
            // Non-printable keys (arrows, Ctrl+Shift+F, etc.) fall through
            // to chrome_action / map_key below so they still work.
        }

        let alt = ws
            .tabs
            .active()
            .map(|t| t.in_alt_screen())
            .unwrap_or(false);
        match input::map_key(event, &self.mods, alt) {
            InputAction::PtyInput(bytes) => {
                if let Some(tab) = ws.tabs.active() {
                    if ws.broadcast {
                        tab.write_all(&bytes);
                    } else {
                        tab.write(&bytes);
                    }
                }
                ws.window.request_redraw();
            }
            InputAction::NewTab => {
                if let Err(e) = ws.tabs.new_tab(&self.cfg) {
                    log::error!("new tab failed: {e:#}");
                }
                ws.window.request_redraw();
            }
            InputAction::SplitV => {
                match ws.tabs.split_active_tab(SplitDir::V, &self.cfg) {
                    Ok(false) => log::info!("split refused: pane too small"),
                    Err(e) => log::error!("split failed: {e:#}"),
                    _ => {}
                }
                ws.force_draw = true;
                ws.window.request_redraw();
            }
            InputAction::SplitH => {
                match ws.tabs.split_active_tab(SplitDir::H, &self.cfg) {
                    Ok(false) => log::info!("split refused: pane too small"),
                    Err(e) => log::error!("split failed: {e:#}"),
                    _ => {}
                }
                ws.force_draw = true;
                ws.window.request_redraw();
            }
            InputAction::ClosePane => {
                ws.tabs.close_active_pane(&self.cfg);
                ws.force_draw = true;
                ws.window.request_redraw();
            }
            InputAction::FocusPane(dir) => {
                let moved = ws.tabs.focus_active_tab(dir);
                if moved {
                    ws.force_draw = true;
                    ws.window.request_redraw();
                }
            }
            InputAction::ZoomPane => {
                ws.tabs.zoom_active_tab();
                ws.force_draw = true;
                ws.window.request_redraw();
            }
            InputAction::FontZoomIn => self.zoom_font(ws, 1.0),
            InputAction::FontZoomOut => self.zoom_font(ws, -1.0),
            InputAction::FontZoomReset => self.reset_font(ws),
            InputAction::RenameTab => self.start_rename(ws),
            InputAction::UnpinTab => {
                let was_pinned = ws
                    .tabs
                    .active()
                    .map(|t| t.pinned())
                    .unwrap_or(false);
                ws.tabs.unpin_active();
                if !was_pinned {
                    log::info!("tab title was not pinned");
                }
                ws.force_draw = true;
                ws.window.request_redraw();
            }
            InputAction::BroadcastToggle => {
                ws.broadcast = !ws.broadcast;
                log::info!("broadcast input: {}", if ws.broadcast { "on" } else { "off" });
                ws.force_draw = true;
                ws.window.request_redraw();
            }
            InputAction::NewWindow => {
                // Create a new window with the same config and working directory
                if let Some(new_window) = self.create_window_with_config(event_loop) {
                    log::info!("created new window: {:?}", new_window.id());
                }
            }
            InputAction::NextTab => {
                ws.tabs.next();
                ws.window.request_redraw();
            }
            InputAction::PrevTab => {
                ws.tabs.prev();
                ws.window.request_redraw();
            }
            InputAction::SelectTab(i) => {
                ws.tabs.select(i);
                ws.window.request_redraw();
            }
            InputAction::Copy => self.do_copy(ws),
            InputAction::Paste => self.do_paste(ws),
            InputAction::ScrollPageUp => {
                if let Some(tab) = ws.tabs.active() {
                    tab.scroll_page(true);
                }
                ws.force_draw = true;
                ws.window.request_redraw();
            }
            InputAction::ScrollPageDown => {
                if let Some(tab) = ws.tabs.active() {
                    tab.scroll_page(false);
                }
                ws.force_draw = true;
                ws.window.request_redraw();
            }
            InputAction::SearchToggle => self.toggle_search(ws),
            InputAction::SearchNext => self.search_next(ws),
            InputAction::SearchPrev => self.search_prev(ws),
            InputAction::SearchCancel => self.search_cancel(ws),
            InputAction::CopymodeToggle => self.toggle_copymode(ws),
            InputAction::CopymodeUp => self.copymode_up(ws),
            InputAction::CopymodeDown => self.copymode_down(ws),
            InputAction::CopymodeLeft => self.copymode_left(ws),
            InputAction::CopymodeRight => self.copymode_right(ws),
            InputAction::CopymodePageUp => self.copymode_page_up(ws),
            InputAction::CopymodePageDown => self.copymode_page_down(ws),
            InputAction::CopymodeHome => self.copymode_home(ws),
            InputAction::CopymodeEnd => self.copymode_end(ws),
            InputAction::CopymodeSelect => self.copymode_select(ws),
            InputAction::CopymodeSelectLine => self.copymode_select_line(ws),
            InputAction::CopymodeCopy => self.copymode_copy(ws),
            InputAction::CopymodeExit => self.copymode_exit(ws),
            InputAction::PaletteToggle => self.toggle_palette(ws),
            InputAction::Ignored => {}
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        // Restore session if we have one pending
        if let Some(session) = self.pending_session.take() {
            if let Err(e) = restore_session_state(&session, self, event_loop) {
                log::warn!("Failed to restore session: {}", e);
                let _ = self.create_window(event_loop);
            } else {
                self.session_loaded = true;
                log::info!("Session restored");
            }
            return;
        }

        // Create initial window if none exists.
        if self.windows.is_empty() {
            let _ = self.create_window(event_loop);
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        id: WindowId,
        event: WindowEvent,
    ) {
        // Update modifiers from this window (must be before get_mut to avoid borrow issues)
        if let WindowEvent::ModifiersChanged(m) = &event {
            self.mods = *m;
        }
        
        // Get or create window state - use remove/insert to avoid borrow checker issues
        let mut window_state = match self.windows.remove(&id) {
            Some(ws) => ws,
            None => {
                // If window doesn't exist in our map, create it (for new windows)
                let _ = self.create_window(event_loop);
                self.windows.remove(&id).expect("just created")
            }
        };
        
        // Process the event with the window state
        match &event {
            WindowEvent::CloseRequested => {
                // Window is closed, don't reinsert
                if self.windows.is_empty() {
                    event_loop.exit();
                }
                return;
            }
            WindowEvent::KeyboardInput { event, .. } => {
                self.handle_keyboard(event_loop, &mut window_state, event);
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.on_cursor_moved(&mut window_state, *position);
            }
            WindowEvent::MouseInput { state, button, .. } => {
                match (button, state) {
                    (MouseButton::Left, ElementState::Pressed) => self.on_left_press(&mut window_state),
                    (MouseButton::Left, ElementState::Released) => self.on_left_release(&mut window_state),
                    (MouseButton::Middle, ElementState::Pressed) => self.on_middle_press(&mut window_state),
                    (MouseButton::Middle, ElementState::Released) => self.on_middle_release(&mut window_state),
                    _ => {}
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                self.on_wheel(&mut window_state, *delta);
            }
            WindowEvent::Resized(size) => {
                self.on_resized(&mut window_state, *size);
            }
            WindowEvent::RedrawRequested => {
                self.on_redraw_requested(&mut window_state);
            }
            _ => {}
        }
        
        // Reinsert the window state
        self.windows.insert(id, window_state);
    }
    
    /// PTY-reader wakeups land here (coalesced per output burst).
    fn user_event(&mut self, _event_loop: &ActiveEventLoop, _event: ()) {
        for ws in self.windows.values() {
            ws.window.request_redraw();
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.poll_config_reload();
        
        let now = Instant::now();
        let mut earliest_deadline = now + Duration::from_millis(500); // blink deadline
        let mut any_window_active = false;
        
        // Collect window IDs first to avoid borrow checker issues
        let window_ids: Vec<WindowId> = self.windows.keys().copied().collect();
        
        for id in window_ids {
            any_window_active = true;
            
            // Get mutable access to this window
            let Some(ws) = self.windows.get_mut(&id) else { continue };
            
            // Strip show/hide reflows the grid (body reclaims the strip
            // height), same as a window resize. Runs here (no live borrows)
            // rather than in the redraw arm.
            let strip_on = Self::strip_on(&self.cfg.tab_bar, ws);
            if strip_on != ws.last_strip {
                ws.last_strip = strip_on;
                let size = ws.window.inner_size();
                let (cols, rows) = Self::estimate_grid(&self.cfg.tab_bar, ws, size.width, size.height);
                ws.tabs.resize_all(cols, rows);
                ws.force_draw = true;
                ws.window.request_redraw();
            }
            
            // Reap tabs whose shells exited (`exit` closes the tab). Runs on
            // every wake, so no dead tab lingers even if its wakeup coalesced.
            // Last tab out quits the app.
            let (reaped, empty) = {
                let reaped = ws.tabs.reap_dead();
                (reaped, ws.tabs.len() == 0)
            };
            if reaped {
                ws.force_draw = true;
                ws.window.request_redraw();
            }
            if empty {
                // This window has no tabs - close it
                // Note: we can't remove from self.windows here because we're iterating
                // We'll handle this in window_event on CloseRequested instead
            }
            
            // Toast: expire it and repaint the strip without it.
            if let Some((_, until)) = ws.toast {
                if now >= until {
                    ws.toast = None;
                    ws.force_draw = true;
                    ws.window.request_redraw();
                } else {
                    earliest_deadline = earliest_deadline.min(until);
                }
            }
            
            // Visual bell: a BEL from any pane flashes the body briefly. The
            // flag is drained even when the bell is disabled, so it never
            // lingers.
            let rang = ws.tabs.take_bell();
            if rang {
                log::debug!("bell: detected (enabled={})", self.cfg.bell_enabled());
            }
            if rang && self.cfg.bell_enabled() {
                ws.bell_flash_until = Some(now + Duration::from_millis(180));
                ws.force_draw = true;
                ws.window.request_redraw();
            }
            // Flash over: paint the normal background again.
            if let Some(until) = ws.bell_flash_until {
                if now >= until {
                    ws.bell_flash_until = None;
                    ws.force_draw = true;
                    ws.window.request_redraw();
                } else {
                    earliest_deadline = earliest_deadline.min(until);
                }
            }
            
            // Cursor blink at ~2Hz.
            if ws.blink_timer.elapsed() >= Duration::from_millis(500) {
                ws.blink_timer = now;
                ws.blink_on = !ws.blink_on;
                ws.window.request_redraw();
                earliest_deadline = earliest_deadline.min(now + Duration::from_millis(500));
            } else {
                let blink_deadline = ws.blink_timer + Duration::from_millis(500);
                earliest_deadline = earliest_deadline.min(blink_deadline);
            }
        }
        
        if !any_window_active {
            event_loop.exit();
            return;
        }
        
        event_loop.set_control_flow(ControlFlow::WaitUntil(earliest_deadline));
    }
}

fn main() -> Result<()> {
    env_logger::init();
    let args = Args::parse();

    // Handle CLI commands (exit after execution)
    if let Some(cmd) = &args.cmd {
        return match cmd {
            CliCmd::Session(s) => handle_session_cmd(s),
            CliCmd::Theme(t) => handle_theme_cmd(t),
        };
    }

    let mut cfg = NeotermConfig::load();
    if let Some(cmd) = args.command {
        cfg.shell = cmd;
    }
    let cwd = resolve_cwd(args.working_directory)?;

    let event_loop = EventLoop::new().expect("create event loop");
    event_loop.set_control_flow(ControlFlow::Wait);

    let proxy = event_loop.create_proxy();
    let mut app = App::new(cfg, proxy, cwd);

    // Try to load session for restoration in `resumed`
    let session_path = session_file_path();
    if session_path.exists() {
        match SessionState::load(&session_path) {
            Ok(session) => {
                app.pending_session = Some(session);
                log::info!("Session loaded from {:?}, will restore on startup", session_path);
            }
            Err(e) => {
                log::warn!("Failed to load session file: {}", e);
            }
        }
    }

    event_loop.run_app(&mut app).expect("run app");

    // Save session on exit if we loaded one
    if app.session_loaded {
        let session = build_session_state(&app);
        if let Err(e) = session.save(&session_path) {
            log::warn!("Failed to save session: {}", e);
        } else {
            log::info!("Session saved to {:?}", session_path);
        }
    }

    Ok(())
}

/// Handle session management CLI commands.
fn handle_session_cmd(cmd: &SessionCmd) -> Result<()> {
    use crate::session::*;

    match cmd {
        SessionCmd::Save { name } => {
            ensure_sessions_dir()?;
            // We can't build session state without running the app,
            // so we save a placeholder. The actual session is saved on exit.
            // This command mainly creates the sessions directory.
            let session_name = name.as_deref().unwrap_or("default");
            let path = named_session_path(session_name);
            // Write empty session as placeholder (real save happens on exit)
            let empty = SessionState::new();
            empty.save(&path)?;
            println!("Session directory ready at {:?}", sessions_dir());
            println!("Session will be saved on exit as '{}'", session_name);
        }
        SessionCmd::Load { name } => {
            let path = named_session_path(name);
            if !path.exists() {
                anyhow::bail!("Session '{}' not found at {:?}", name, path);
            }
            // Copy to default session file for auto-restore on next launch
            let default = session_file_path();
            if let Some(parent) = default.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&path, &default)?;
            println!("Session '{}' loaded. Will restore on next launch.", name);
        }
        SessionCmd::List => {
            let sessions = list_sessions()?;
            if sessions.is_empty() {
                println!("No saved sessions.");
            } else {
                println!("Saved sessions:");
                for s in sessions {
                    println!("  {}", s);
                }
            }
        }
        SessionCmd::Delete { name } => {
            delete_session(name)?;
            println!("Session '{}' deleted.", name);
        }
    }
    Ok(())
}

/// Handle theme CLI commands.
fn handle_theme_cmd(cmd: &ThemeCmd) -> Result<()> {
    use crate::config::Theme;

    match cmd {
        ThemeCmd::List => {
            println!("Built-in theme presets:");
            println!("  tokyo-night      (default)");
            println!("  catppuccin-mocha");
            println!("  dracula");
            println!("  gruvbox-dark");
            println!("\nOr use 'custom' to use the custom colors from your config file.");
        }
        ThemeCmd::Set { preset } => {
            let mut cfg = NeotermConfig::load();
            if preset == "custom" {
                cfg.theme.preset = None;
                println!("Theme set to custom (using config file colors).");
            } else if Theme::preset(preset).is_some() {
                cfg.theme.preset = Some(preset.clone());
                println!("Theme preset set to '{}'.", preset);
            } else {
                anyhow::bail!("Unknown theme preset '{}'. Run 'neoterm theme list' for available presets.", preset);
            }
            cfg.save()?;
            println!("Config saved. Restart neoterm to apply.");
        }
        ThemeCmd::Show => {
            let cfg = NeotermConfig::load();
            if let Some(p) = &cfg.theme.preset {
                println!("Theme preset: {}", p);
            } else {
                println!("Theme preset: custom");
            }
            println!("  background:      {}", cfg.theme.background);
            println!("  foreground:      {}", cfg.theme.foreground);
            println!("  cursor:          {}", cfg.theme.cursor);
            println!("  selection:       {}", cfg.theme.selection);
            println!("  tab_active_bg:   {}", cfg.theme.tab_active_bg);
            println!("  tab_active_fg:   {}", cfg.theme.tab_active_fg);
            println!("  tab_accent:      {}", cfg.theme.tab_accent);
            println!("  tab_bar_border:  {}", cfg.theme.tab_bar_border);
            println!("  tab_bar_bg:      {}", cfg.theme.tab_bar_bg.as_deref().unwrap_or("auto (follows body)"));
            println!("  tab_inactive_fg: {}", cfg.theme.tab_inactive_fg);
            println!("  tab_close_fg:    {}", cfg.theme.tab_close_fg);
            println!("  tab_index_fg:    {}", cfg.theme.tab_index_fg);
            println!("  scrollbar_track: {}", cfg.theme.scrollbar_track);
            println!("  scrollbar_thumb: {}", cfg.theme.scrollbar_thumb);
            println!("  palette:");
            for (i, c) in cfg.theme.palette.iter().enumerate() {
                print!("    [{}] {}", i, c);
                if i % 4 == 3 {
                    println!();
                }
            }
        }
    }
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
