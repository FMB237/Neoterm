use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::{
    Config as TermConfig, Term, TermDamage, TermMode,
};
use alacritty_terminal::selection::{Selection, SelectionRange, SelectionType};
use alacritty_terminal::vte::ansi::{Color as AnsiColor, CursorShape, NamedColor, Rgb};
use anyhow::{Context, Result};
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use crate::shell_integration::{new_shared_shell_integration, SharedShellIntegration};
use crate::graphics::{new_shared_graphics_state, SharedGraphicsState};
use winit::event_loop::EventLoopProxy;

use std::collections::HashMap;

use crate::config::NeotermConfig;
use crate::input::FocusDir;

/// Dimensions adapter for alacritty_terminal.
#[derive(Clone, Copy)]
pub struct TermSize {
    pub cols: usize,
    pub rows: usize,
}

impl Dimensions for TermSize {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

pub type SharedTerm = Arc<Mutex<Term<TitleListener>>>;

/// Live tab title from OSC 0/2 sequences (Ghostty-style: tabs named by
/// the running app). Falls back to `tab-{id}`; `ResetTitle` restores it.
/// Title changes wake the loop through the shared dirty flag so the strip
/// re-renders immediately. The same listener catches terminal bells
/// (`Event::Bell`) and raises the shared bell flag for the visual bell.
#[derive(Clone)]
pub struct TitleListener {
    title: Arc<Mutex<String>>,
    fallback: String,
    dirty: Arc<AtomicBool>,
    /// Set when the app rang the bell; drained by the main loop.
    bell: Arc<AtomicBool>,
    proxy: Option<EventLoopProxy<()>>,
}

impl TitleListener {
    fn new(
        fallback: String,
        dirty: Arc<AtomicBool>,
        bell: Arc<AtomicBool>,
        proxy: Option<EventLoopProxy<()>>,
    ) -> (Self, Arc<Mutex<String>>) {
        let title = Arc::new(Mutex::new(fallback.clone()));
        (
            Self {
                title: Arc::clone(&title),
                fallback,
                dirty,
                bell,
                proxy,
            },
            title,
        )
    }

    /// Headless constructor for tests (no event loop to wake).
    #[cfg(test)]
    pub(crate) fn headless(fallback: &str) -> (Self, Arc<Mutex<String>>) {
        Self::new(
            fallback.to_string(),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            None,
        )
    }
}

impl EventListener for TitleListener {
    fn send_event(&self, event: Event) {
        match event {
            Event::Title(t) => self.set_title(t),
            Event::ResetTitle => self.set_title(self.fallback.clone()),
            Event::Bell => {
                // The reader thread must never touch the UI: raise a flag
                // and let the main loop start the flash.
                log::debug!("bell: app rang the bell");
                self.bell.store(true, Ordering::Relaxed);
                if !self.dirty.swap(true, Ordering::Relaxed) {
                    if let Some(p) = &self.proxy {
                        let _ = p.send_event(());
                    }
                }
            }
            _ => {}
        }
    }
}

impl TitleListener {
    fn set_title(&self, next: String) {
        if let Ok(mut slot) = self.title.lock() {
            if *slot != next {
                *slot = next;
                // Same coalescing as the reader thread: the pending
                // redraw (if any) already covers this change.
                if !self.dirty.swap(true, Ordering::Relaxed) {
                    if let Some(p) = &self.proxy {
                        let _ = p.send_event(());
                    }
                }
            }
        }
    }
}

pub struct Pane {
    /// Live OSC title slot (see `TitleListener`).
    title: Arc<Mutex<String>>,
    pub term: SharedTerm,
    master: Box<dyn MasterPty + Send>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    _child: Box<dyn portable_pty::Child + Send + Sync>,
    /// Set by the PTY reader thread when new output was parsed. Drained by
    /// the main loop; doubles as coalescing for event-loop wakeups.
    pub dirty: Arc<AtomicBool>,
    /// Set by the PTY reader thread when the shell exits (PTY EOF or read
    /// error). The main loop reaps dead tabs (`exit` closes the tab).
    pub dead: Arc<AtomicBool>,
    /// Set when the app rang the bell (BEL). Drained by the main loop,
    /// which turns it into a visual flash.
    pub bell: Arc<AtomicBool>,
    /// Theme ANSI palette (16 entries), applied to named colors at resolve
    /// time. Refreshed on config reload.
    palette: [(u8, u8, u8); 16],
    /// Last cursor position seen by `take_damage` (stale-cursor detection:
    /// cursor-only moves don't always mark grid damage).
    last_cursor: Mutex<(usize, usize)>,
    /// Current working directory of the shell (for session restore).
    cwd: PathBuf,
    /// Graphics state for Sixel/Kitty protocol support.
    graphics: SharedGraphicsState,
    /// Shell integration state for OSC 7/133/633.
    shell_integration: SharedShellIntegration,
}

impl Pane {
    pub fn new(
        cfg: &NeotermConfig,
        cols: u16,
        rows: u16,
        title_fallback: String,
        proxy: EventLoopProxy<()>,
        cwd: &Path,
    ) -> Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("openpty")?;

        let mut cmd = CommandBuilder::new(&cfg.shell);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.cwd(cwd);

        // Windows/ConPTY specific configuration
        #[cfg(target_os = "windows")]
        {
            // ConPTY on Windows: use Windows-appropriate defaults
            cmd.env("WT_SESSION", "neoterm");
            // On Windows, ConPTY handles the PTY differently; no extra setup needed
            // portable-pty's native_pty_system() returns ConPtySystem on Windows
        }

        let child = pair.slave.spawn_command(cmd).context("spawn shell")?;

        let mut term_config = TermConfig::default();
        term_config.scrolling_history = cfg.scrollback_lines;

        let size = TermSize {
            cols: cols as usize,
            rows: rows as usize,
        };
        // Slots shared with the title listener must exist before Term::new.
        let dirty = Arc::new(AtomicBool::new(true)); // ensure first paint
        let dead = Arc::new(AtomicBool::new(false));
        let bell = Arc::new(AtomicBool::new(false));
        let (listener, title) = TitleListener::new(
            title_fallback,
            Arc::clone(&dirty),
            Arc::clone(&bell),
            Some(proxy.clone()),
        );
        let term = Term::new(term_config, &size, listener);
        let term: SharedTerm = Arc::new(Mutex::new(term));

        // Reader thread: PTY -> vte -> Term, then wake the event loop.
        // Wakeups are coalesced: only the transition false->true sends.
        let mut reader: Box<dyn Read + Send> = pair
            .master
            .try_clone_reader()
            .context("clone pty reader")?;
        let term_clone = Arc::clone(&term);
        let dirty_clone = Arc::clone(&dirty);
        let dead_clone = Arc::clone(&dead);
        // Create graphics state before spawning reader thread so we can capture it
        let graphics = new_shared_graphics_state();
        let graphics_clone = Arc::clone(&graphics);
        // Create shell integration state
        let shell_integration = new_shared_shell_integration();
        let shell_integration_clone = Arc::clone(&shell_integration);
        thread::spawn(move || {
            let mut processor: vte::ansi::Processor = Default::default();
            let mut buf = [0u8; 8192];
            // EOF/error means the shell exited: flag the tab dead and wake
            // the loop once (one-shot, no coalescing needed).
            let mark_dead = || {
                dead_clone.store(true, Ordering::Relaxed);
                let _ = proxy.send_event(());
            };
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => {
                        mark_dead();
                        break;
                    }
                    Ok(n) => {
                        // Preprocess graphics escape sequences (Sixel/Kitty)
                        let processed = crate::graphics::preprocess_graphics(&buf[..n], &graphics_clone);
                        // Preprocess shell integration escape sequences (OSC 7/133/633)
                        let processed = crate::shell_integration::preprocess_shell_integration(&processed, &shell_integration_clone);
                        if let Ok(mut t) = term_clone.lock() {
                            processor.advance(&mut *t, &processed);
                        }
                        if !dirty_clone.swap(true, Ordering::Relaxed) {
                            let _ = proxy.send_event(());
                        }
                    }
                    Err(_) => {
                        mark_dead();
                        break;
                    }
                }
            }
        });

        let writer: Box<dyn Write + Send> =
            pair.master.take_writer().context("take pty writer")?;

        Ok(Self {
            title,
            term,
            master: pair.master,
            writer: Arc::new(Mutex::new(writer)),
            _child: child,
            dirty,
            dead,
            bell,
            palette: cfg.theme.palette_rgb(),
            last_cursor: Mutex::new((0, 0)),
            cwd: cwd.to_path_buf(),
            graphics,
            shell_integration,
        })
    }

    /// Refresh theme colors after a config reload (no respawn needed).
    pub fn apply_config(&mut self, cfg: &NeotermConfig) {
        self.palette = cfg.theme.palette_rgb();
    }

    /// Live tab title: OSC 0/2 name from the running app, else `tab-{id}`.
    pub fn title(&self) -> String {
        self.title
            .lock()
            .map(|t| t.clone())
            .unwrap_or_default()
    }

    /// Current working directory of the shell (for session restore).
    pub fn get_cwd(&self) -> PathBuf {
        self.cwd.clone()
    }

    /// Get the graphics state for Sixel/Kitty protocol support.
    pub fn graphics(&self) -> &SharedGraphicsState {
        &self.graphics
    }

    /// Get the shell integration state for OSC 7/133/633.
    pub fn shell_integration(&self) -> &SharedShellIntegration {
        &self.shell_integration
    }

    pub fn write(&self, bytes: &[u8]) {
        if let Ok(mut w) = self.writer.lock() {
            let _ = w.write_all(bytes);
            let _ = w.flush();
        }
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        let _ = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
        if let Ok(mut term) = self.term.lock() {
            term.resize(TermSize {
                cols: cols as usize,
                rows: rows as usize,
            });
        }
    }

    /// Drain damage state: true when grid lines changed OR the cursor moved
    /// since the last call (cursor-only moves don't always mark damage).
    /// Also resets the damage so the next call sees only newer changes.
    pub fn take_damage(&self) -> bool {
        let Ok(mut term) = self.term.lock() else {
            return true;
        };
        let grid_damaged = match term.damage() {
            TermDamage::Full => true,
            TermDamage::Partial(mut lines) => lines.next().is_some(),
        };
        term.reset_damage();
        let rows = term.screen_lines().max(1);
        let cols = term.columns().max(1);
        let point = term.grid().cursor.point;
        let cur = (
            (point.line.0.max(0) as usize).min(rows - 1),
            point.column.0.min(cols - 1),
        );
        drop(term);
        if let Ok(mut last) = self.last_cursor.lock() {
            let moved = *last != cur;
            *last = cur;
            grid_damaged || moved
        } else {
            true
        }
    }

    /// Text for `Ctrl+Shift+C`: the active selection when one exists,
    /// otherwise the whole visible viewport (copy-screen fallback).
    ///
    /// Stage 3 is keys-only with no mouse selection yet, so nothing ever
    /// sets `term.selection` — the fallback is the working path until
    /// mouse drag-select (Stage 4) lands.
    pub fn copy_text(&self) -> Option<String> {
        let Ok(term) = self.term.lock() else {
            return None;
        };
        if term.selection.is_some() {
            return term.selection_to_string().filter(|s| !s.is_empty());
        }
        let cols = term.columns().max(1);
        let rows = term.screen_lines().max(1);
        // Copy what is visible: viewport row r is grid line (r - offset).
        let offset = term.grid().display_offset() as i32;
        let text = term.bounds_to_string(
            Point::new(Line(-offset), Column(0)),
            Point::new(Line(rows as i32 - 1 - offset), Column(cols - 1)),
        );
        if text.trim().is_empty() {
            None
        } else {
            Some(text)
        }
    }

    /// Whether the running app asked for bracketed paste (`\e[?2004h`).
    pub fn bracketed_paste(&self) -> bool {
        self.term_mode().contains(TermMode::BRACKETED_PASTE)
    }

    /// Copy of the terminal mode flags (mouse reporting, alt screen, ...).
    pub fn term_mode(&self) -> TermMode {
        let Ok(term) = self.term.lock() else {
            return TermMode::empty();
        };
        *term.mode()
    }

    /// Whether the alternate screen is active (vim, htop, ...). Local
    /// scrollback viewing is meaningless there.
    pub fn in_alt_screen(&self) -> bool {
        self.term_mode().contains(TermMode::ALT_SCREEN)
    }

    /// Lines scrolled up into history (0 = live view).
    pub fn display_offset(&self) -> usize {
        let Ok(term) = self.term.lock() else {
            return 0;
        };
        term.grid().display_offset()
    }

    /// Total scrollback history lines available (0 = nothing to scroll).
    pub fn history_size(&self) -> usize {
        let Ok(term) = self.term.lock() else {
            return 0;
        };
        term.grid().history_size()
    }

    /// Scroll the viewport: positive = up into history, negative = down.
    /// Clamped by alacritty; marks full damage on change (wakes redraw).
    pub fn scroll_lines(&self, lines: i32) {
        if let Ok(mut term) = self.term.lock() {
            term.scroll_display(Scroll::Delta(lines));
        }
    }

    /// Scroll exactly one viewport page. `up=true` goes into history.
    /// No-op on the alternate screen (no scrollback there).
    pub fn scroll_page(&self, up: bool) {
        if let Ok(mut term) = self.term.lock() {
            if term.mode().contains(TermMode::ALT_SCREEN) {
                return;
            }
            term.scroll_display(if up {
                Scroll::PageUp
            } else {
                Scroll::PageDown
            });
        }
    }

    /// Jump to an absolute scroll offset (0 = live view). Clamped.
    pub fn scroll_to(&self, offset: usize) {
        if let Ok(mut term) = self.term.lock() {
            let cur = term.grid().display_offset();
            let delta = offset.min(100_000) as i32 - cur.min(100_000) as i32;
            if delta != 0 {
                term.scroll_display(Scroll::Delta(delta));
            }
        }
    }

    /// Viewport dimensions as (cols, rows).
    pub fn grid_size(&self) -> (usize, usize) {
        let Ok(term) = self.term.lock() else {
            return (80, 24);
        };
        (term.columns().max(1), term.screen_lines().max(1))
    }

    /// Start a local selection at a grid point.
    pub fn begin_selection(&self, ty: SelectionType, point: Point, side: Side) {
        if let Ok(mut term) = self.term.lock() {
            term.selection = Some(Selection::new(ty, point, side));
        }
    }

    /// Extend the active local selection (no-op when none).
    pub fn update_selection(&self, point: Point, side: Side) {
        if let Ok(mut term) = self.term.lock() {
            if let Some(sel) = term.selection.as_mut() {
                sel.update(point, side);
            }
        }
    }

    pub fn clear_selection(&self) {
        if let Ok(mut term) = self.term.lock() {
            term.selection = None;
        }
    }

    /// Whether a local selection is currently active.
    pub fn has_selection(&self) -> bool {
        let Ok(term) = self.term.lock() else {
            return false;
        };
        term.selection.is_some()
    }

    /// Snapshot visible rows with resolved per-cell styles.
    ///
    /// Wide-char spacers are skipped (the leading char already occupies both
    /// columns). Trailing blank cells are trimmed: invisible anyway since the
    /// Stage 1 renderer draws no per-cell backgrounds.
    ///
    /// When `cursor_visible`, the cursor cell is returned as
    /// `(viewport row, grid column, cursor shape)` and its glyph is
    /// recolored to the background color; the renderer draws the block
    /// behind it. The character under the cursor therefore stays readable.
    /// `None` when the cursor is hidden or scrolled away.
    pub fn rendered_rows(
        &self,
        default_fg: (u8, u8, u8),
        default_bg: (u8, u8, u8),
        cursor_visible: bool,
    ) -> (Vec<RowContent>, Option<(usize, usize, CursorShape)>) {
        let Ok(term) = self.term.lock() else {
            return (vec![], None);
        };
        render_term_rows(
            &term,
            default_fg,
            default_bg,
            cursor_visible,
            &self.palette,
        )
    }
}

// ============================== splits ==============================

/// Stable pane identifier (focus, layout frames, renderer caches).
pub type PaneId = u64;

/// Minimum livable pane size in grid cells. Splits that would produce a
/// smaller pane are refused; drag ratios clamp to the same minimums.
pub const MIN_PANE_COLS: usize = 15;
pub const MIN_PANE_ROWS: usize = 4;

/// Split direction: V = side-by-side (1-column divider), H = stacked.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SplitDir {
    V,
    H,
}

/// Binary split tree. Leaves hold pane ids; `Tab::panes` owns the panes.
/// Dividers are always exactly 1 cell wide/tall (no pixel rounding, text
/// never drifts; hit-testing is the gap cell itself).
#[derive(Clone, Debug)]
pub enum PaneNode {
    Leaf(PaneId),
    Split {
        dir: SplitDir,
        ratio: f32,
        first: Box<PaneNode>,
        second: Box<PaneNode>,
    },
}

/// Grid-cell rectangle: pane bodies and 1-cell dividers share this type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellRect {
    pub col: usize,
    pub row: usize,
    pub cols: usize,
    pub rows: usize,
}

impl CellRect {
    pub fn contains(&self, col: usize, row: usize) -> bool {
        col >= self.col
            && col < self.col + self.cols
            && row >= self.row
            && row < self.row + self.rows
    }
}

/// Laid-out tree: pane bodies plus divider rects with the root path of
/// their split node (`false` = first child, `true` = second) for dragging.
#[derive(Clone, Debug, Default)]
pub struct PaneLayout {
    pub panes: Vec<(PaneId, CellRect)>,
    pub dividers: Vec<(Vec<bool>, CellRect)>,
}

impl PaneNode {
    pub fn contains(&self, id: PaneId) -> bool {
        match self {
            PaneNode::Leaf(pid) => *pid == id,
            PaneNode::Split { first, second, .. } => {
                first.contains(id) || second.contains(id)
            }
        }
    }

    /// Replace `Leaf(id)` with `with`. False when id is not in the tree.
    pub fn replace_leaf(&mut self, id: PaneId, with: PaneNode) -> bool {
        match self {
            PaneNode::Leaf(pid) => {
                if *pid == id {
                    *self = with;
                    true
                } else {
                    false
                }
            }
            PaneNode::Split { first, second, .. } => {
                first.replace_leaf(id, with.clone()) || second.replace_leaf(id, with)
            }
        }
    }

    /// Remove `Leaf(id)`, collapsing the parent split onto the sibling.
    /// Returns the replacement subtree (`None` = tree is now empty).
    pub fn remove_leaf(node: PaneNode, id: PaneId) -> Option<PaneNode> {
        match node {
            PaneNode::Leaf(pid) if pid == id => None,
            PaneNode::Leaf(_) => Some(node),
            PaneNode::Split {
                dir,
                ratio,
                first,
                second,
            } => match (
                Self::remove_leaf(*first, id),
                Self::remove_leaf(*second, id),
            ) {
                (None, None) => None,
                (Some(x), None) => Some(x),
                (None, Some(y)) => Some(y),
                (Some(x), Some(y)) => Some(PaneNode::Split {
                    dir,
                    ratio,
                    first: Box::new(x),
                    second: Box::new(y),
                }),
            },
        }
    }

    /// Mutable split node at `path` (empty path = this node).
    pub fn at_path_mut(&mut self, path: &[bool]) -> Option<&mut PaneNode> {
        let mut node = self;
        for &go_second in path {
            match node {
                PaneNode::Leaf(_) => return None,
                PaneNode::Split { first, second, .. } => {
                    node = if go_second { second } else { first };
                }
            }
        }
        Some(node)
    }

    pub fn first_leaf(&self) -> Option<PaneId> {
        match self {
            PaneNode::Leaf(pid) => Some(*pid),
            PaneNode::Split { first, .. } => first.first_leaf(),
        }
    }
}

/// Split `total` cells into (first, second) with a 1-cell divider between.
/// Defensive clamp: never returns a zero/negative side when avoidable.
fn split_lengths(total: usize, ratio: f32, min: usize) -> (usize, usize) {
    let avail = total.saturating_sub(1);
    if avail < 2 {
        return (avail, 0);
    }
    let lo = min.min(avail - 1);
    let hi = avail.saturating_sub(min).max(lo);
    let first = ((avail as f32 * ratio).floor() as usize).clamp(lo, hi);
    (first, avail - first)
}

/// Whether `rect` can host another split (both halves stay livable).
pub fn can_split(rect: CellRect, dir: SplitDir) -> bool {
    match dir {
        SplitDir::V => rect.cols >= 2 * MIN_PANE_COLS + 1,
        SplitDir::H => rect.rows >= 2 * MIN_PANE_ROWS + 1,
    }
}

/// Clamp a drag ratio so both halves of `total` cells keep `min` cells.
pub fn clamp_ratio(ratio: f32, total: usize, min: usize) -> f32 {
    let avail = total.saturating_sub(1) as f32;
    if avail < 2.0 {
        return 0.5;
    }
    let lo = (min as f32 / avail).min(0.5);
    ratio.clamp(lo, 1.0 - lo)
}

/// Ratio from a pointer cell inside `parent` (divider gap = 1 cell).
pub fn ratio_from_pointer(dir: SplitDir, parent: CellRect, col: usize, row: usize) -> f32 {
    let (pos, total, min) = match dir {
        SplitDir::V => (col.saturating_sub(parent.col), parent.cols, MIN_PANE_COLS),
        SplitDir::H => (row.saturating_sub(parent.row), parent.rows, MIN_PANE_ROWS),
    };
    let denom = total.saturating_sub(1).max(1) as f32;
    clamp_ratio(pos as f32 / denom, total, min)
}

/// Lay out `root` over `area`. A live `zoomed` id takes the whole area
/// with no dividers; a stale one is ignored (healed by the caller).
pub fn layout_tree(root: &PaneNode, area: CellRect, zoomed: Option<PaneId>) -> PaneLayout {
    let mut out = PaneLayout::default();
    if let Some(id) = zoomed {
        if root.contains(id) {
            out.panes.push((id, area));
            return out;
        }
    }
    layout_node(root, area, Vec::new(), &mut out);
    out
}

fn layout_node(node: &PaneNode, rect: CellRect, path: Vec<bool>, out: &mut PaneLayout) {
    match node {
        PaneNode::Leaf(id) => out.panes.push((*id, rect)),
        PaneNode::Split {
            dir,
            ratio,
            first,
            second,
        } => {
            let (f_rect, d_rect, s_rect) = split_rects(rect, *dir, *ratio);
            out.dividers.push((path.clone(), d_rect));
            let mut p2 = path.clone();
            let mut p1 = path;
            p2.push(true);
            p1.push(false);
            layout_node(first, f_rect, p1, out);
            layout_node(second, s_rect, p2, out);
        }
    }
}

/// First/divider/second rects for splitting `rect` (shared by layout and
/// divider hit-testing so drag math matches drawn geometry exactly).
fn split_rects(rect: CellRect, dir: SplitDir, ratio: f32) -> (CellRect, CellRect, CellRect) {
    match dir {
        SplitDir::V => {
            let (fw, sw) = split_lengths(rect.cols, ratio, MIN_PANE_COLS);
            (
                CellRect { cols: fw, ..rect },
                CellRect {
                    col: rect.col + fw,
                    cols: 1,
                    ..rect
                },
                CellRect {
                    col: rect.col + fw + 1,
                    cols: sw,
                    ..rect
                },
            )
        }
        SplitDir::H => {
            let (fh, sh) = split_lengths(rect.rows, ratio, MIN_PANE_ROWS);
            (
                CellRect { rows: fh, ..rect },
                CellRect {
                    row: rect.row + fh,
                    rows: 1,
                    ..rect
                },
                CellRect {
                    row: rect.row + fh + 1,
                    rows: sh,
                    ..rect
                },
            )
        }
    }
}

/// Neighboring pane id in `dir` from `active`: candidates must sit strictly
/// past the active edge with perpendicular overlap; most overlap wins,
/// ties break by smallest gap. Pure (headless-testable).
pub fn focus_neighbor(
    panes: &[(PaneId, CellRect)],
    active: PaneId,
    dir: FocusDir,
) -> Option<PaneId> {
    let cur = panes.iter().find(|(id, _)| *id == active)?.1;
    let mut best: Option<(PaneId, usize, usize)> = None; // id, overlap, gap
    for (id, r) in panes {
        if *id == active {
            continue;
        }
        let (past, overlap, gap) = match dir {
            FocusDir::Right => {
                if r.col < cur.col + cur.cols {
                    continue;
                }
                let ov = overlap_len(r.row, r.rows, cur.row, cur.rows);
                (true, ov, r.col - (cur.col + cur.cols))
            }
            FocusDir::Left => {
                if r.col + r.cols > cur.col {
                    continue;
                }
                let ov = overlap_len(r.row, r.rows, cur.row, cur.rows);
                (true, ov, cur.col - (r.col + r.cols))
            }
            FocusDir::Down => {
                if r.row < cur.row + cur.rows {
                    continue;
                }
                let ov = overlap_len(r.col, r.cols, cur.col, cur.cols);
                (true, ov, r.row - (cur.row + cur.rows))
            }
            FocusDir::Up => {
                if r.row + r.rows > cur.row {
                    continue;
                }
                let ov = overlap_len(r.col, r.cols, cur.col, cur.cols);
                (true, ov, cur.row - (r.row + r.rows))
            }
        };
        if !past || overlap == 0 {
            continue;
        }
        let replace = match best {
            None => true,
            Some((_, b_ov, b_gap)) => (overlap, std::cmp::Reverse(gap)) > (b_ov, std::cmp::Reverse(b_gap)),
        };
        if replace {
            best = Some((*id, overlap, gap));
        }
    }
    best.map(|(id, _, _)| id)
}

fn overlap_len(a0: usize, a1: usize, b0: usize, b1: usize) -> usize {
    let lo = a0.max(b0);
    let hi = (a0 + a1).min(b0 + b1);
    hi.saturating_sub(lo)
}

/// A tab: one split tree of panes sharing the body area. Single-pane tabs
/// behave exactly like the old `Tab` (all accessors delegate to the
/// active pane, so most of main.rs is untouched).
pub struct Tab {
    panes: HashMap<PaneId, Pane>,
    root: Option<PaneNode>,
    active: PaneId,
    zoomed: Option<PaneId>,
    /// Last laid-out body area in grid cells (drives splits + resizes).
    area: CellRect,
    /// Per-pane grid sizes (drag-resize only touches changed panes).
    sizes: HashMap<PaneId, (usize, usize)>,
    tab_id: usize,
    /// Directory new panes start their shell in.
    cwd: PathBuf,
    /// Manually set title. While set, OSC titles are ignored (the pin
    /// wins) until the pin is cleared.
    pinned_title: Option<String>,
}

impl Tab {
    pub fn new(
        cfg: &NeotermConfig,
        cols: u16,
        rows: u16,
        tab_id: usize,
        pane_id: PaneId,
        proxy: EventLoopProxy<()>,
        cwd: &Path,
    ) -> Result<Self> {
        let pane = Pane::new(
            cfg,
            cols,
            rows,
            format!("tab-{}", tab_id),
            proxy,
            cwd,
        )?;
        let mut panes = HashMap::new();
        panes.insert(pane_id, pane);
        let mut sizes = HashMap::new();
        sizes.insert(pane_id, (cols as usize, rows as usize));
        Ok(Self {
            panes,
            root: Some(PaneNode::Leaf(pane_id)),
            active: pane_id,
            zoomed: None,
            area: CellRect {
                col: 0,
                row: 0,
                cols: cols as usize,
                rows: rows as usize,
            },
            sizes,
            tab_id,
            cwd: cwd.to_path_buf(),
            pinned_title: None,
        })
    }

    pub fn active_id(&self) -> PaneId {
        self.active
    }

    pub fn pane_count(&self) -> usize {
        self.panes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.panes.is_empty()
    }

    pub fn active_pane(&self) -> Option<&Pane> {
        self.panes.get(&self.active)
    }

    pub fn pane(&self, id: PaneId) -> Option<&Pane> {
        self.panes.get(&id)
    }

    /// Get all panes.
    pub fn panes(&self) -> &HashMap<PaneId, Pane> {
        &self.panes
    }

    /// Get all pane sizes.
    pub fn sizes(&self) -> &HashMap<PaneId, (usize, usize)> {
        &self.sizes
    }

    /// Get the root of the pane tree.
    pub fn root(&self) -> &Option<PaneNode> {
        &self.root
    }

    /// Get the tab ID.
    pub fn tab_id(&self) -> usize {
        self.tab_id
    }

    /// Get the pinned title.
    pub fn pinned_title(&self) -> Option<&String> {
        self.pinned_title.as_ref()
    }

    /// Get the active pane ID.
    pub fn active_pane_id(&self) -> PaneId {
        self.active
    }

    /// Get the zoomed pane ID.
    pub fn zoomed_pane_id(&self) -> Option<PaneId> {
        self.zoomed
    }

    /// Current layout over the stored body area (zoom honored).
    pub fn layout(&self) -> PaneLayout {
        match &self.root {
            None => PaneLayout::default(),
            Some(root) => layout_tree(root, self.area, self.zoomed),
        }
    }

    pub fn divider_at(&self, col: usize, row: usize) -> Option<(Vec<bool>, CellRect)> {
        self.layout()
            .dividers
            .iter()
            .find(|(_, r)| r.contains(col, row))
            .cloned()
    }

    /// The split direction + full parent span of the divider at `path`
    /// (for drag-ratio math; the parent rect is ratio-invariant).
    pub fn divider_info(&self, path: &[bool]) -> Option<(SplitDir, CellRect)> {
        let mut node = self.root.as_ref()?;
        let mut rect = self.area;
        for &go_second in path {
            match node {
                PaneNode::Leaf(_) => return None,
                PaneNode::Split { dir, ratio, first, second } => {
                    let (f, _, s) = split_rects(rect, *dir, *ratio);
                    rect = if go_second { s } else { f };
                    node = if go_second { second } else { first };
                }
            }
        }
        match node {
            PaneNode::Split { dir, .. } => Some((*dir, rect)),
            PaneNode::Leaf(_) => None,
        }
    }

    /// Resize the body: re-layout and resize only panes whose grid size
    /// actually changed (drag-resize storms stay cheap).
    pub fn resize_body(&mut self, cols: usize, rows: usize) {
        self.area = CellRect {
            col: 0,
            row: 0,
            cols,
            rows,
        };
        // Stale zoom heals here (reaped pane no longer in the tree).
        if let Some(z) = self.zoomed {
            let in_tree = self.root.as_ref().map(|r| r.contains(z)).unwrap_or(false);
            if !in_tree {
                self.zoomed = None;
            }
        }
        for (id, rect) in self.layout().panes {
            let size = (rect.cols.max(1), rect.rows.max(1));
            if self.sizes.get(&id) != Some(&size) {
                self.sizes.insert(id, size);
                if let Some(p) = self.panes.get(&id) {
                    p.resize(size.0 as u16, size.1 as u16);
                }
            }
        }
    }

    /// Split the active pane. The new pane takes focus (tmux/Ghostty
    /// behavior). False when the pane is too small to split.
    pub fn split_active(
        &mut self,
        dir: SplitDir,
        cfg: &NeotermConfig,
        proxy: EventLoopProxy<()>,
        new_id: PaneId,
    ) -> Result<bool> {
        // Splitting unzooms first (new tree must be visible to size it).
        self.zoomed = None;
        let active_rect = self
            .layout()
            .panes
            .iter()
            .find(|(id, _)| *id == self.active)
            .map(|(_, r)| *r);
        let Some(rect) = active_rect else {
            return Ok(false);
        };
        if !can_split(rect, dir) {
            log::info!("split refused: pane too small");
            return Ok(false);
        }
        let pane = Pane::new(
            cfg,
            rect.cols as u16,
            rect.rows as u16,
            format!("tab-{}", self.tab_id),
            proxy,
            &self.cwd,
        )?;
        self.panes.insert(new_id, pane);
        if let Some(root) = self.root.as_mut() {
            root.replace_leaf(
                self.active,
                PaneNode::Split {
                    dir,
                    ratio: 0.5,
                    first: Box::new(PaneNode::Leaf(self.active)),
                    second: Box::new(PaneNode::Leaf(new_id)),
                },
            );
        } else {
            self.root = Some(PaneNode::Leaf(new_id));
        }
        self.active = new_id;
        let (cols, rows) = (self.area.cols, self.area.rows);
        self.resize_body(cols, rows);
        Ok(true)
    }

    /// Close one pane (tree surgery: the sibling subtree takes over).
    /// Returns true when the tab has no panes left.
    pub fn close_pane(&mut self, id: PaneId) -> bool {
        if let Some(root) = self.root.take() {
            self.root = PaneNode::remove_leaf(root, id);
        }
        self.panes.remove(&id);
        self.sizes.remove(&id);
        if self.zoomed == Some(id) {
            self.zoomed = None;
        }
        if self.panes.is_empty() {
            return true;
        }
        // Focus must always land on a live pane.
        let alive = self
            .root
            .as_ref()
            .map(|r| r.contains(self.active))
            .unwrap_or(false);
        if !alive {
            self.active = self
                .root
                .as_ref()
                .and_then(|r| r.first_leaf())
                .unwrap_or(self.active);
        }
        // Belt and braces: active id itself may be gone.
        if !self.panes.contains_key(&self.active) {
            self.active = self.root.as_ref().and_then(|r| r.first_leaf()).unwrap_or(0);
        }
        false
    }

    /// Remove panes whose shells exited. True when any pane was removed.
    pub fn reap_dead_panes(&mut self) -> bool {
        let dead: Vec<PaneId> = self
            .panes
            .iter()
            .filter(|(_, p)| p.dead.load(Ordering::Relaxed))
            .map(|(id, _)| *id)
            .collect();
        if dead.is_empty() {
            return false;
        }
        for id in dead {
            self.close_pane(id);
        }
        true
    }

    /// Take ownership of a pane, removing it from this tab.
    /// Returns the pane and its rect in the layout, or None if not found.
    pub fn take_pane(&mut self, id: PaneId) -> Option<Pane> {
        if let Some(root) = self.root.take() {
            self.root = PaneNode::remove_leaf(root, id);
        }
        let pane = self.panes.remove(&id);
        self.sizes.remove(&id);
        if self.zoomed == Some(id) {
            self.zoomed = None;
        }
        // Fix focus if it was on the removed pane.
        if self.active == id {
            self.active = self
                .root
                .as_ref()
                .and_then(|r| r.first_leaf())
                .unwrap_or(0);
        }
        pane
    }

    /// Add a pane to this tab (as a new split or standalone if empty).
    pub fn add_pane(&mut self, id: PaneId, pane: Pane, _cfg: &NeotermConfig) {
        self.panes.insert(id, pane);
        if let Some(root) = self.root.as_mut() {
            root.replace_leaf(
                self.active,
                PaneNode::Split {
                    dir: SplitDir::V,
                    ratio: 0.5,
                    first: Box::new(PaneNode::Leaf(self.active)),
                    second: Box::new(PaneNode::Leaf(id)),
                },
            );
        } else {
            self.root = Some(PaneNode::Leaf(id));
        }
        self.active = id;
        let (cols, rows) = (self.area.cols, self.area.rows);
        self.resize_body(cols, rows);
    }

    /// Move focus to the neighboring pane (unzooms first if needed).
    pub fn focus(&mut self, dir: FocusDir) -> bool {
        // Focus while zoomed: unzoom so the target is visible.
        if self.zoomed.is_some() {
            self.zoomed = None;
        }
        let layout = self.layout();
        if let Some(id) = focus_neighbor(&layout.panes, self.active, dir) {
            self.active = id;
            true
        } else {
            false
        }
    }

    pub fn focus_id(&mut self, id: PaneId) -> bool {
        if self.panes.contains_key(&id) {
            self.active = id;
            true
        } else {
            false
        }
    }

    /// Toggle fullscreen zoom of the active pane.
    pub fn toggle_zoom(&mut self) {
        if self.zoomed == Some(self.active) {
            self.zoomed = None;
        } else {
            self.zoomed = Some(self.active);
        }
        let (cols, rows) = (self.area.cols, self.area.rows);
        self.resize_body(cols, rows);
    }

    /// Drag a divider to a new ratio. `parent` is the split node's full
    /// span (captured at grab time; invariant under ratio changes) and
    /// clamps the ratio to livable minimums.
    pub fn set_ratio(&mut self, path: &[bool], dir: SplitDir, parent: CellRect, ratio: f32) {
        let total = match dir {
            SplitDir::V => parent.cols,
            SplitDir::H => parent.rows,
        };
        let min = match dir {
            SplitDir::V => MIN_PANE_COLS,
            SplitDir::H => MIN_PANE_ROWS,
        };
        let ratio = clamp_ratio(ratio, total, min);
        let Some(root) = self.root.as_mut() else {
            return;
        };
        if let Some(PaneNode::Split { ratio: r, .. }) = root.at_path_mut(path) {
            *r = ratio;
        }
        let (cols, rows) = (self.area.cols, self.area.rows);
        self.resize_body(cols, rows);
    }
}

/// Session restore methods for Tab.
impl Tab {
    /// Set the pinned title.
    pub fn set_pinned_title(&mut self, title: Option<String>) {
        self.pinned_title = title;
    }

    /// Set the active pane ID.
    pub fn set_active_pane_id(&mut self, id: PaneId) {
        self.active = id;
    }

    /// Set the zoomed pane ID.
    pub fn set_zoomed_pane_id(&mut self, id: Option<PaneId>) {
        self.zoomed = id;
    }

    /// Replace all panes (used during session restore).
    pub fn replace_panes(&mut self, panes: HashMap<PaneId, Pane>) {
        self.panes = panes;
    }

    /// Replace all pane sizes (used during session restore).
    pub fn replace_sizes(&mut self, sizes: HashMap<PaneId, (usize, usize)>) {
        self.sizes = sizes;
    }

    /// Replace the root of the pane tree (used during session restore).
    pub fn replace_root(&mut self, root: Option<PaneNode>) {
        self.root = root;
    }
}

// ---- Tab accessors: single-pane behavior delegates to the active pane,
// so mouse/keyboard/scroll/copy paths in main.rs are pane-aware for free.

impl Tab {
    pub fn write(&self, bytes: &[u8]) {
        if let Some(p) = self.active_pane() {
            p.write(bytes);
        }
    }

    /// Write bytes to all panes in this tab (broadcast).
    pub fn write_all(&self, bytes: &[u8]) {
        for p in self.panes.values() {
            p.write(bytes);
        }
    }

    /// Damage across all panes (drains every pane's damage state).
    pub fn take_damage(&self) -> bool {
        self.panes.values().any(|p| p.take_damage())
    }

    /// Any pane produced PTY output since the last call.
    pub fn take_dirty(&self) -> bool {
        let mut any = false;
        for p in self.panes.values() {
            if p.dirty.swap(false, Ordering::Relaxed) {
                any = true;
            }
        }
        any
    }

    pub fn copy_text(&self) -> Option<String> {
        self.active_pane()?.copy_text()
    }

    pub fn has_selection(&self) -> bool {
        self.active_pane().map(|p| p.has_selection()).unwrap_or(false)
    }

    pub fn bracketed_paste(&self) -> bool {
        self.active_pane()
            .map(|p| p.bracketed_paste())
            .unwrap_or(false)
    }

    pub fn term_mode(&self) -> TermMode {
        self.active_pane()
            .map(|p| p.term_mode())
            .unwrap_or_else(TermMode::empty)
    }

    pub fn in_alt_screen(&self) -> bool {
        self.active_pane().map(|p| p.in_alt_screen()).unwrap_or(false)
    }

    pub fn display_offset(&self) -> usize {
        self.active_pane().map(|p| p.display_offset()).unwrap_or(0)
    }

    pub fn history_size(&self) -> usize {
        self.active_pane().map(|p| p.history_size()).unwrap_or(0)
    }

    pub fn scroll_lines(&self, lines: i32) {
        if let Some(p) = self.active_pane() {
            p.scroll_lines(lines);
        }
    }

    pub fn scroll_page(&self, up: bool) {
        if let Some(p) = self.active_pane() {
            p.scroll_page(up);
        }
    }

    pub fn scroll_to(&self, offset: usize) {
        if let Some(p) = self.active_pane() {
            p.scroll_to(offset);
        }
    }

    pub fn grid_size(&self) -> (usize, usize) {
        self.active_pane().map(|p| p.grid_size()).unwrap_or((80, 24))
    }

    /// Tab title: the manual pin when set, else the active pane's live
    /// OSC title (falls back per pane).
    pub fn title(&self) -> String {
        let live = self
            .active_pane()
            .map(|p| p.title())
            .unwrap_or_default();
        resolve_title(self.pinned_title.as_deref(), &live)
    }

    /// Pin a manual title (or clear the pin with `None` / a blank name).
    pub fn set_pin(&mut self, name: Option<String>) {
        self.pinned_title = name.map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
    }

    pub fn pinned(&self) -> bool {
        self.pinned_title.is_some()
    }

    /// Drain bell flags from every pane. True when any pane rang.
    pub fn take_bell(&self) -> bool {
        let mut rang = false;
        for p in self.panes.values() {
            if p.bell.swap(false, Ordering::Relaxed) {
                rang = true;
            }
        }
        rang
    }

    pub fn apply_config(&mut self, cfg: &NeotermConfig) {
        for p in self.panes.values_mut() {
            p.apply_config(cfg);
        }
    }
}

/// Resolve a tab's displayed title: a manual pin wins over the live OSC
/// title; a blank pin is treated as no pin.
pub fn resolve_title(pinned: Option<&str>, live: &str) -> String {
    match pinned {
        Some(p) if !p.trim().is_empty() => p.to_string(),
        _ => live.to_string(),
    }
}

/// Snapshot visible rows of a terminal with resolved per-cell styles.
///
/// Free function (takes an already-locked `Term`) so headless tests can
/// drive a `Term` directly without PTY, event loop, or window.
///
/// The cursor is **not** baked into the text: the caller gets its cell back
/// and the renderer draws a block *behind* the glyph, so the character under
/// the cursor stays readable. The glyph itself is recolored to the
/// background color (inverse video) — the classic terminal cursor.
pub fn render_term_rows(
    term: &Term<TitleListener>,
    default_fg: (u8, u8, u8),
    default_bg: (u8, u8, u8),
    cursor_visible: bool,
    palette: &[(u8, u8, u8); 16],
) -> (Vec<RowContent>, Option<(usize, usize, CursorShape)>) {
        let cols = term.columns();
        let rows = term.screen_lines();
        let colors = term.colors();
        let grid = term.grid();
        // Visible viewport row r shows grid line (r - offset).
        let offset = grid.display_offset() as i32;

        // Active selection as an ordered grid range (drives highlight).
        // Damage tracking excludes selection, so callers must force a
        // redraw on selection change (see main.rs).
        let sel_range: Option<SelectionRange> =
            term.selection.as_ref().and_then(|s| s.to_range(&*term));

        let cpoint = grid.cursor.point;
        let cursor_shape = term.cursor_style().shape;
        // Cursor in viewport-row coordinates; off-screen while scrolled.
        let cursor_vrow = cpoint.line.0 + offset;
        let cursor_col = cpoint.column.0.min(cols.saturating_sub(1));
        // The cursor cell the renderer draws its block in: (viewport row,
        // grid column, shape). `None` when the cursor is hidden or scrolled
        // away; Hidden shape means no block is drawn.
        let cursor_cell: Option<(usize, usize, CursorShape)> =
            if cursor_visible && cursor_vrow >= 0 && (cursor_vrow as usize) < rows {
                Some((cursor_vrow as usize, cursor_col, cursor_shape))
            } else {
                None
            };

        let mut out = Vec::with_capacity(rows);
        for row_idx in 0..rows {
            let grid_line = row_idx as i32 - offset;
            let line = Line(grid_line);
            // Selected column range on this grid line, if any.
            let sel_cols: Option<(usize, usize)> = sel_range.as_ref().and_then(|r| {
                if grid_line < r.start.line.0 || grid_line > r.end.line.0 {
                    return None;
                }
                let s = if grid_line == r.start.line.0 {
                    r.start.column.0
                } else {
                    0
                };
                let e = if grid_line == r.end.line.0 {
                    r.end.column.0
                } else {
                    cols.saturating_sub(1)
                };
                Some((s.min(e), s.max(e)))
            });
            // (char, style) per pushed glyph; wide-char spacers skipped.
            let mut cells: Vec<(char, CellStyle)> = Vec::with_capacity(cols);
            // Char index of the cursor cell within `cells`.
            let mut cursor_char: Option<usize> = None;
            // Background runs in exact grid columns (spacers included for
            // continuity). Only non-default backgrounds are recorded.
            let mut bg_rects: Vec<(usize, usize, (u8, u8, u8))> = Vec::new();
            let mut bg_run: Option<(usize, (u8, u8, u8))> = None;
            for col_idx in 0..cols {
                let cell = &grid[line][Column(col_idx)];
                let bold = cell.flags.contains(Flags::BOLD);
                let italic = cell.flags.contains(Flags::ITALIC);
                let underline = cell.flags.contains(Flags::UNDERLINE);
                let dim = cell.flags.contains(Flags::DIM);
                let mut fg =
                    resolve_color(cell.fg, colors, true, bold, dim, default_fg, default_bg, palette);
                let mut bg =
                    resolve_color(cell.bg, colors, false, false, false, default_fg, default_bg, palette);
                if cell.flags.contains(Flags::INVERSE) {
                    std::mem::swap(&mut fg, &mut bg);
                }
                // Track the bg run (every column, spacers included).
                let continues = matches!(bg_run, Some((_, rgb)) if rgb == bg);
                if !continues {
                    if let Some((lo, rgb)) = bg_run.take() {
                        bg_rects.push((lo, col_idx - 1, rgb));
                    }
                    if bg != default_bg {
                        bg_run = Some((col_idx, bg));
                    }
                }
                if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                    if cursor_vrow == row_idx as i32 && cursor_col == col_idx {
                        cursor_char = Some(cells.len().saturating_sub(1));
                    }
                    continue;
                }
                // NOTE: selection is NOT swapped here. Swapping fg/bg without
                // drawing a background makes default-colored text invisible
                // (black on black). The selection background is drawn by the
                // renderer's rect pass from `sel_cols` instead.
                if cursor_vrow == row_idx as i32 && cursor_col == col_idx {
                    cursor_char = Some(cells.len());
                }
                cells.push((
                    cell.c,
                    CellStyle {
                        fg,
                        bg,
                        bold,
                        italic,
                        underline,
                        hyperlink: cell.hyperlink().map(|h| h.uri().to_string()),
                    },
                ));
            }
            // Flush the trailing bg run.
            if let Some((lo, rgb)) = bg_run.take() {
                bg_rects.push((lo, cols.saturating_sub(1), rgb));
            }
            // Trim trailing blanks, but keep colored-background padding: its
            // bg rect is part of the rendered output (e.g. vim statuslines).
            while cells
                .last()
                .map(|(c, s)| *c == ' ' && s.bg == default_bg)
                .unwrap_or(false)
            {
                cells.pop();
            }
            // The cursor is drawn by the renderer as a block *behind* the
            // glyph, so the character underneath stays readable. Only the
            // glyph's color changes here (inverse video): the block itself
            // is a quad in the renderer's background pass.
            if cursor_visible && cursor_vrow == row_idx as i32 {
                if let Some(idx) = cursor_char {
                    if let Some(slot) = cells.get_mut(idx) {
                        slot.1.fg = default_bg;
                    }
                }
            }
            // Build text + grouped spans (byte ranges over `text`).
            let mut text =
                String::with_capacity(cells.iter().map(|(c, _)| c.len_utf8()).sum());
            let mut spans: Vec<(Range<usize>, CellStyle)> = Vec::new();
            for (ch, style) in &cells {
                let start = text.len();
                text.push(*ch);
                let end = text.len();
                if let Some((range, prev)) = spans.last_mut() {
                    if *prev == *style {
                        range.end = end;
                        continue;
                    }
                }
                spans.push((start..end, style.clone()));
            }
            out.push(RowContent {
                text,
                spans,
                sel_cols,
                bg_rects,
            });
        }
        (out, cursor_cell)
    }

// Note: `impl Pane` ends at `rendered_rows` above; `render_term_rows` is a
// free function so headless tests can use it without PTY or window.

/// Resolved per-cell style (8-bit RGB + attrs).
///
/// `bg` is resolved for INVERSE handling and future per-cell background
/// rects. Selection backgrounds are drawn by the renderer's rect pass.
/// `hyperlink` holds the OSC 8 URI when this cell is a hyperlink target.
/// `underline` tracks the SGR 4 underline style (Single/Double).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CellStyle {
    pub fg: (u8, u8, u8),
    pub bg: (u8, u8, u8),
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub hyperlink: Option<String>,
}

/// One visible terminal row: plain text plus byte-range style spans.
///
/// Spans are ascending, non-overlapping, and expressed in **bytes** (for
/// cosmic-text `AttrsList`). The cursor block (when visible) is already
/// baked into `text` as `█` with its own span. `sel_cols` is the selected
/// column range in *grid* columns (for the selection background pass);
/// `None` when this row has no selection.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RowContent {
    pub text: String,
    pub spans: Vec<(Range<usize>, CellStyle)>,
    pub sel_cols: Option<(usize, usize)>,
    /// Non-default background runs as exact (lo_col, hi_col, rgb) triples.
    pub bg_rects: Vec<(usize, usize, (u8, u8, u8))>,
}

impl RowContent {
    pub fn empty() -> Self {
        Self {
            text: String::new(),
            spans: Vec::new(),
            sel_cols: None,
            bg_rects: Vec::new(),
        }
    }
}

/// Base-16 entries of the xterm table (kept for tests; the live path
/// resolves 0..15 through the theme palette instead).
#[cfg(test)]
const XTERM_BASE: [(u8, u8, u8); 16] = [
    (0, 0, 0),
    (205, 0, 0),
    (0, 205, 0),
    (205, 205, 0),
    (0, 0, 238),
    (205, 0, 205),
    (0, 205, 205),
    (229, 229, 229),
    (127, 127, 127),
    (255, 0, 0),
    (0, 255, 0),
    (255, 255, 0),
    (92, 92, 255),
    (255, 0, 255),
    (0, 255, 255),
    (255, 255, 255),
];

/// xterm 256-color lookup (16..232 color cube, 232..256 grayscale ramp).
/// Entries 0..15 come from the theme palette.
fn xterm_indexed(i: u8, palette: &[(u8, u8, u8); 16]) -> (u8, u8, u8) {
    if i < 16 {
        return palette[i as usize];
    }
    if i < 232 {
        const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
        let v = i - 16;
        return (
            LEVELS[(v / 36) as usize],
            LEVELS[((v % 36) / 6) as usize],
            LEVELS[(v % 6) as usize],
        );
    }
    let gray = 8 + 10 * (i - 232);
    (gray, gray, gray)
}

fn dim_rgb((r, g, b): (u8, u8, u8)) -> (u8, u8, u8) {
    (
        (r as u16 * 2 / 3) as u8,
        (g as u16 * 2 / 3) as u8,
        (b as u16 * 2 / 3) as u8,
    )
}

/// Resolve a named color through runtime OSC overrides, then the theme
/// palette, falling back to the xterm table and the configured defaults.
/// Priority (standard terminal behavior): app OSC sequences win over the
/// configured theme.
fn lookup_named(
    named: NamedColor,
    colors: &Colors,
    default_fg: (u8, u8, u8),
    default_bg: (u8, u8, u8),
    palette: &[(u8, u8, u8); 16],
) -> (u8, u8, u8) {
    if let Some(Rgb { r, g, b }) = colors[named] {
        return (r, g, b);
    }
    match named {
        NamedColor::Foreground => default_fg,
        NamedColor::Background => default_bg,
        NamedColor::Cursor => default_fg,
        NamedColor::BrightForeground => default_fg,
        NamedColor::DimForeground => dim_rgb(default_fg),
        NamedColor::DimBlack => dim_rgb(palette[0]),
        NamedColor::DimRed => dim_rgb(palette[1]),
        NamedColor::DimGreen => dim_rgb(palette[2]),
        NamedColor::DimYellow => dim_rgb(palette[3]),
        NamedColor::DimBlue => dim_rgb(palette[4]),
        NamedColor::DimMagenta => dim_rgb(palette[5]),
        NamedColor::DimCyan => dim_rgb(palette[6]),
        NamedColor::DimWhite => dim_rgb(palette[7]),
        other => {
            let idx = other as usize;
            if idx < 16 {
                palette[idx]
            } else {
                default_fg
            }
        }
    }
}

/// Resolve an SGR color to 8-bit RGB.
///
/// `is_fg` selects the default for `Named(Foreground/Background)`.
/// Bold brightens plain named fg colors (standard terminal behavior);
/// DIM dims the foreground. Both apply to named colors only — indexed and
/// RGB-spec colors are dimmed numerically instead.
fn resolve_color(
    color: AnsiColor,
    colors: &Colors,
    is_fg: bool,
    bold: bool,
    dim: bool,
    default_fg: (u8, u8, u8),
    default_bg: (u8, u8, u8),
    palette: &[(u8, u8, u8); 16],
) -> (u8, u8, u8) {
    match color {
        AnsiColor::Named(mut named) => {
            if is_fg && bold {
                named = named.to_bright();
            }
            if is_fg && dim {
                named = named.to_dim();
            }
            lookup_named(named, colors, default_fg, default_bg, palette)
        }
        AnsiColor::Indexed(i) => {
            let rgb = colors[i as usize]
                .map(|Rgb { r, g, b }| (r, g, b))
                .unwrap_or_else(|| xterm_indexed(i, palette));
            if is_fg && dim {
                dim_rgb(rgb)
            } else {
                rgb
            }
        }
        AnsiColor::Spec(Rgb { r, g, b }) => {
            if is_fg && dim {
                dim_rgb((r, g, b))
            } else {
                (r, g, b)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Theme;

    fn feed(term: &mut Term<TitleListener>, text: &str) {
        let mut processor: vte::ansi::Processor = Default::default();
        processor.advance(term, text.as_bytes());
    }

    const WHITE: (u8, u8, u8) = (255, 255, 255);
    const BLACK: (u8, u8, u8) = (0, 0, 0);

    /// Proves the scroll mapping: viewport row r shows grid line (r - offset).
    /// Regression guard for "scrolled view renders blank".
    #[test]
    fn scrollback_renders_history_in_order() {
        let size = TermSize { cols: 20, rows: 10 };
        let mut term_config = TermConfig::default();
        term_config.scrolling_history = 100;
        let mut term = Term::new(term_config, &size, TitleListener::headless("tab-0").0);

        for i in 0..49 {
            feed(&mut term, &format!("line-{i:02}\r\n"));
        }
        feed(&mut term, "line-49");

        // Live view shows the last 10 content lines.
        let (rows, _) = render_term_rows(&term, WHITE, BLACK, false, &XTERM_BASE);
        assert_eq!(rows.len(), 10);
        assert_eq!(rows[0].text, "line-40");
        assert_eq!(rows[9].text, "line-49");

        // Scroll up 5: viewport shows older history, top-first.
        term.scroll_display(Scroll::Delta(5));
        assert_eq!(term.grid().display_offset(), 5);
        let (rows, _) = render_term_rows(&term, WHITE, BLACK, false, &XTERM_BASE);
        assert_eq!(rows.len(), 10);
        for r in &rows {
            println!("|{}|", r.text);
        }
        assert_eq!(rows[0].text, "line-35");
        assert_eq!(rows[9].text, "line-44");

        // Back to bottom restores the live view.
        term.scroll_display(Scroll::Bottom);
        assert_eq!(term.grid().display_offset(), 0);
        let (rows, _) = render_term_rows(&term, WHITE, BLACK, false, &XTERM_BASE);
        assert_eq!(rows[0].text, "line-40");
        assert_eq!(rows[9].text, "line-49");
    }

    /// Selection highlight data follows the selection, not the cursor.
    #[test]
    fn selection_marks_row_ranges() {
        let size = TermSize { cols: 20, rows: 10 };
        let mut term = Term::new(TermConfig::default(), &size, TitleListener::headless("tab-0").0);
        feed(&mut term, "hello world\r\nsecond row");

        term.selection = Some(Selection::new(
            SelectionType::Simple,
            Point::new(Line(0), Column(0)),
            Side::Left,
        ));
        term.selection
            .as_mut()
            .unwrap()
            .update(Point::new(Line(0), Column(4)), Side::Right);

        let (rows, _) = render_term_rows(&term, WHITE, BLACK, false, &XTERM_BASE);
        assert_eq!(rows[0].text, "hello world");
        assert_eq!(rows[0].sel_cols, Some((0, 4)));
        assert_eq!(rows[1].sel_cols, None);
    }

    /// Explicit SGR backgrounds and INVERSE emit exact-column bg rects.
    #[test]
    fn colored_bg_emits_rects() {
        let size = TermSize { cols: 20, rows: 10 };
        let mut term = Term::new(TermConfig::default(), &size, TitleListener::headless("tab-0").0);
        // Red on xterm blue, then default colors.
        feed(&mut term, "\x1b[31;44mhi\x1b[0m ok");
        // Inverse video on defaults: bg becomes light, fg dark.
        feed(&mut term, "\r\n\x1b[7mrev\x1b[0m");

        let (rows, _) = render_term_rows(&term, WHITE, BLACK, false, &XTERM_BASE);
        assert_eq!(rows[0].text, "hi ok");
        assert_eq!(rows[0].bg_rects, vec![(0, 1, (0, 0, 238))]);
        assert_eq!(rows[1].text, "rev");
        assert_eq!(rows[1].bg_rects, vec![(0, 2, (255, 255, 255))]);
    }

    /// OSC 0/2 sequences rename the tab live; reset restores the fallback.
    #[test]
    fn osc_title_updates_listener_slot() {
        let (listener, slot) = TitleListener::headless("tab-3");
        let size = TermSize { cols: 20, rows: 10 };
        let mut term = Term::new(TermConfig::default(), &size, listener);
        assert_eq!(*slot.lock().unwrap(), "tab-3");
        feed(&mut term, "\x1b]0;myapp\x07");
        assert_eq!(*slot.lock().unwrap(), "myapp");
        // OSC 2 (tab title) lands in the same slot.
        feed(&mut term, "\x1b]2;second\x07");
        assert_eq!(*slot.lock().unwrap(), "second");
    }

    /// A manual pin wins over the live OSC title; a blank pin does not.
    #[test]
    fn pinned_title_overrides_live_title() {
        assert_eq!(resolve_title(Some("build"), "vim"), "build");
        assert_eq!(resolve_title(None, "vim"), "vim");
        assert_eq!(resolve_title(Some("   "), "vim"), "vim");
    }

    /// A BEL from the app raises the bell flag and wakes the loop so the
    /// visual bell is drawn.
    #[test]
    fn bell_event_raises_flag() {
        let dirty = Arc::new(AtomicBool::new(false));
        let bell = Arc::new(AtomicBool::new(false));
        let (listener, _title) = TitleListener::new(
            "tab-1".to_string(),
            Arc::clone(&dirty),
            Arc::clone(&bell),
            None,
        );
        listener.send_event(Event::Bell);
        assert!(bell.load(Ordering::Relaxed));
        assert!(dirty.load(Ordering::Relaxed));
    }

    /// The cursor is a block drawn *behind* the glyph: the character under
    /// it survives (it is only recolored to the background color).
    #[test]
    fn cursor_keeps_the_character_underneath() {
        let size = TermSize { cols: 20, rows: 10 };
        let mut term = Term::new(TermConfig::default(), &size, TitleListener::headless("tab-0").0);
        // "abc" then backspace: the cursor sits on the 'c'.
        feed(&mut term, "abc\x08");

        let (rows, cursor) = render_term_rows(&term, WHITE, BLACK, true, &XTERM_BASE);
        assert_eq!(cursor, Some((0, 2, CursorShape::Block)));
        // The character is still there (no block glyph substituted).
        assert_eq!(rows[0].text, "abc");
        // ...and it is recolored to the background (inverse video), so the
        // renderer can draw the block behind it.
        let last = rows[0].spans.last().expect("spans");
        assert_eq!(last.1.fg, BLACK);

        // A hidden cursor reports no cell at all.
        let (_, cursor) = render_term_rows(&term, WHITE, BLACK, false, &XTERM_BASE);
        assert_eq!(cursor, None);
    }

    /// Named colors resolve through the theme palette, not the xterm table.
    #[test]
    fn theme_palette_overrides_named_colors() {
        let size = TermSize { cols: 20, rows: 10 };
        let mut term = Term::new(TermConfig::default(), &size, TitleListener::headless("tab-0").0);
        feed(&mut term, "\x1b[31mred\x1b[0m \x1b[38;5;1midx");
        let palette = Theme::default().palette_rgb();
        // Tokyo Night red, not xterm red.
        assert_eq!(palette[1], (247, 118, 142));
        let (rows, _) = render_term_rows(&term, WHITE, BLACK, false, &palette);
        assert_eq!(rows[0].text, "red idx");
        // SGR 31 and 38;5;1 agree (named + indexed paths).
        assert_eq!(rows[0].spans[0].1.fg, (247, 118, 142));
        assert_eq!(rows[0].spans[2].1.fg, (247, 118, 142));
    }

    fn rect(col: usize, row: usize, cols: usize, rows: usize) -> CellRect {
        CellRect { col, row, cols, rows }
    }

    fn vsplit_tree() -> PaneNode {
        PaneNode::Split {
            dir: SplitDir::V,
            ratio: 0.5,
            first: Box::new(PaneNode::Leaf(1)),
            second: Box::new(PaneNode::Leaf(2)),
        }
    }

    /// V splits divide columns with a 1-cell divider, no gaps/overlaps.
    #[test]
    fn layout_v_split_exact() {
        let area = rect(0, 0, 80, 24);
        let lay = layout_tree(&vsplit_tree(), area, None);
        assert_eq!(
            lay.panes,
            vec![(1, rect(0, 0, 39, 24)), (2, rect(40, 0, 40, 24))]
        );
        assert_eq!(lay.dividers.len(), 1);
        assert_eq!(lay.dividers[0].1, rect(39, 0, 1, 24));
        // Tiles exactly: 39 + 1 + 40 = 80.
        assert_eq!(39 + 1 + 40, 80);
    }

    /// H splits divide rows; zoom takes the whole area divider-free.
    #[test]
    fn layout_h_split_and_zoom() {
        let root = PaneNode::Split {
            dir: SplitDir::H,
            ratio: 0.5,
            first: Box::new(PaneNode::Leaf(1)),
            second: Box::new(PaneNode::Leaf(2)),
        };
        let area = rect(0, 0, 80, 24);
        let lay = layout_tree(&root, area, None);
        assert_eq!(
            lay.panes,
            vec![(1, rect(0, 0, 80, 11)), (2, rect(0, 12, 80, 12))]
        );
        assert_eq!(lay.dividers[0].1, rect(0, 11, 80, 1));
        let zoomed = layout_tree(&root, area, Some(2));
        assert_eq!(zoomed.panes, vec![(2, area)]);
        assert!(zoomed.dividers.is_empty());
        // Stale zoom id is ignored (caller heals it).
        let stale = layout_tree(&root, area, Some(99));
        assert_eq!(stale.panes.len(), 2);
    }

    /// Removing a leaf collapses the parent onto the sibling subtree.
    #[test]
    fn remove_leaf_collapses_tree() {
        let root = PaneNode::Split {
            dir: SplitDir::V,
            ratio: 0.5,
            first: Box::new(PaneNode::Leaf(1)),
            second: Box::new(PaneNode::Split {
                dir: SplitDir::H,
                ratio: 0.5,
                first: Box::new(PaneNode::Leaf(2)),
                second: Box::new(PaneNode::Leaf(3)),
            }),
        };
        let after = PaneNode::remove_leaf(root, 2).expect("tree survives");
        let area = rect(0, 0, 80, 24);
        let lay = layout_tree(&after, area, None);
        assert_eq!(lay.panes.len(), 2);
        assert_eq!(lay.panes[0].0, 1);
        assert_eq!(lay.panes[1].0, 3);
        // Whole tree gone when the last leaf is removed.
        let solo = PaneNode::remove_leaf(PaneNode::Leaf(7), 7);
        assert!(solo.is_none());
    }

    /// Focus moves to the overlapped neighbor in each direction.
    #[test]
    fn focus_neighbor_four_ways() {
        // 2x2 grid: 1|2 over 3|4.
        let panes = vec![
            (1, rect(0, 0, 39, 11)),
            (2, rect(40, 0, 40, 11)),
            (3, rect(0, 12, 39, 12)),
            (4, rect(40, 12, 40, 12)),
        ];
        assert_eq!(focus_neighbor(&panes, 1, FocusDir::Right), Some(2));
        assert_eq!(focus_neighbor(&panes, 2, FocusDir::Left), Some(1));
        assert_eq!(focus_neighbor(&panes, 1, FocusDir::Down), Some(3));
        assert_eq!(focus_neighbor(&panes, 4, FocusDir::Up), Some(2));
        assert_eq!(focus_neighbor(&panes, 1, FocusDir::Left), None);
        assert_eq!(focus_neighbor(&panes, 1, FocusDir::Up), None);
    }

    /// Drag ratios clamp to livable minimums; splits refuse tiny panes.
    #[test]
    fn drag_ratio_clamps_and_splits_gate() {
        assert!((clamp_ratio(0.5, 80, MIN_PANE_COLS) - 0.5).abs() < 1e-6);
        assert!(clamp_ratio(0.01, 80, MIN_PANE_COLS) > 0.15);
        assert!(clamp_ratio(0.99, 80, MIN_PANE_COLS) < 0.85);
        let parent = rect(0, 0, 80, 24);
        let r = ratio_from_pointer(SplitDir::V, parent, 20, 0);
        assert!((r - 20.0 / 79.0).abs() < 1e-6);
        assert!(can_split(rect(0, 0, 80, 24), SplitDir::V));
        assert!(!can_split(rect(0, 0, 20, 24), SplitDir::V));
        assert!(!can_split(rect(0, 0, 80, 5), SplitDir::H));
    }
}
