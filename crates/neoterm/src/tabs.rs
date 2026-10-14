use anyhow::Result;
use winit::event_loop::EventLoopProxy;

use crate::config::NeotermConfig;
use crate::input::FocusDir;
use crate::tab::{PaneId, SplitDir, Tab};

pub struct Tabs {
    tabs: Vec<Tab>,
    active: usize,
    next_id: usize,
    next_pane: PaneId,
    cols: u16,
    rows: u16,
    proxy: EventLoopProxy<()>,
}

impl Tabs {
    pub fn new(cfg: &NeotermConfig, proxy: EventLoopProxy<()>) -> Result<Self> {
        let mut t = Self {
            tabs: Vec::new(),
            active: 0,
            next_id: 1,
            next_pane: 1,
            cols: cfg.cols,
            rows: cfg.rows,
            proxy,
        };
        t.new_tab(cfg)?;
        Ok(t)
    }

    fn alloc_pane(&mut self) -> PaneId {
        let id = self.next_pane;
        self.next_pane += 1;
        id
    }

    pub fn new_tab(&mut self, cfg: &NeotermConfig) -> Result<()> {
        let pane = self.alloc_pane();
        let tab = Tab::new(cfg, self.cols, self.rows, self.next_id, pane, self.proxy.clone())?;
        self.next_id += 1;
        self.tabs.push(tab);
        self.active = self.tabs.len() - 1;
        log::info!("new tab, total={}", self.tabs.len());
        Ok(())
    }

    /// Close the tab at `idx` (middle-click / close mark). Always keeps ≥1 tab.
    pub fn close_at(&mut self, idx: usize, cfg: &NeotermConfig) {
        if self.tabs.is_empty() || idx >= self.tabs.len() {
            return;
        }
        self.tabs.remove(idx);
        if self.tabs.is_empty() {
            // Always keep at least one tab.
            if self.new_tab(cfg).is_err() {
                return;
            }
        } else if idx < self.active {
            // Tabs after the removed one shifted down.
            self.active -= 1;
        } else if self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        }
    }

    /// Remove dead panes inside every tab, then drop emptied tabs.
    /// Unlike `close_at` this never respawns: returns true when anything
    /// was removed, so the caller can quit when no tabs remain.
    pub fn reap_dead(&mut self) -> bool {
        let mut removed = false;
        for t in &mut self.tabs {
            if t.reap_dead_panes() {
                removed = true;
            }
        }
        let before = self.tabs.len();
        self.tabs.retain(|t| !t.is_empty());
        if self.tabs.len() != before {
            removed = true;
        }
        if !self.tabs.is_empty() && self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        }
        removed
    }

    /// Split the active pane of the active tab. False when too small.
    pub fn split_active_tab(&mut self, dir: SplitDir, cfg: &NeotermConfig) -> Result<bool> {
        if self.active >= self.tabs.len() {
            return Ok(false);
        }
        let id = self.alloc_pane();
        self.tabs[self.active].split_active(dir, cfg, self.proxy.clone(), id)
    }

    /// Close the active pane; last pane closes the tab (with respawn).
    pub fn close_active_pane(&mut self, cfg: &NeotermConfig) {
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return;
        };
        if tab.pane_count() > 1 {
            let id = tab.active_id();
            tab.close_pane(id);
        } else {
            self.close_at(self.active, cfg);
        }
    }

    /// Move focus inside the active tab. False when no neighbor exists.
    pub fn focus_active_tab(&mut self, dir: FocusDir) -> bool {
        self.tabs
            .get_mut(self.active)
            .map(|t| t.focus(dir))
            .unwrap_or(false)
    }

    /// Toggle fullscreen zoom of the active tab's active pane.
    pub fn zoom_active_tab(&mut self) {
        if let Some(t) = self.tabs.get_mut(self.active) {
            t.toggle_zoom();
        }
    }

    pub fn next(&mut self) {
        if !self.tabs.is_empty() {
            self.active = (self.active + 1) % self.tabs.len();
        }
    }

    pub fn prev(&mut self) {
        if !self.tabs.is_empty() {
            self.active = (self.active + self.tabs.len() - 1) % self.tabs.len();
        }
    }

    pub fn select(&mut self, idx: usize) {
        if idx < self.tabs.len() {
            self.active = idx;
        }
    }

    pub fn active(&self) -> Option<&Tab> {
        self.tabs.get(self.active)
    }

    pub fn active_mut(&mut self) -> Option<&mut Tab> {
        self.tabs.get_mut(self.active)
    }

    /// Tab by index (divider-drag targets a fixed tab across frames).
    pub fn get_mut(&mut self, idx: usize) -> Option<&mut Tab> {
        self.tabs.get_mut(idx)
    }

    pub fn active_index(&self) -> usize {
        self.active
    }

    pub fn len(&self) -> usize {
        self.tabs.len()
    }

    pub fn titles(&self) -> Vec<String> {
        self.tabs.iter().map(|t| t.title()).collect()
    }

    /// Drain all per-pane reader dirty flags. True when any PTY produced
    /// output since the last call.
    pub fn take_dirty(&self) -> bool {
        self.tabs.iter().any(|t| t.take_dirty())
    }

    pub fn resize_all(&mut self, cols: u16, rows: u16) {
        self.cols = cols;
        self.rows = rows;
        for t in &mut self.tabs {
            t.resize_body(cols as usize, rows as usize);
        }
    }
}
