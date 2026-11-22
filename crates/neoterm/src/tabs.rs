use anyhow::Result;
use std::path::PathBuf;
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
    /// Directory every new shell starts in (from `--working-directory`).
    cwd: PathBuf,
    proxy: EventLoopProxy<()>,
}

impl Tabs {
    pub fn new(
        cfg: &NeotermConfig,
        proxy: EventLoopProxy<()>,
        cwd: PathBuf,
    ) -> Result<Self> {
        let mut t = Self {
            tabs: Vec::new(),
            active: 0,
            next_id: 1,
            next_pane: 1,
            cols: cfg.cols,
            rows: cfg.rows,
            cwd,
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
        let tab = Tab::new(
            cfg,
            self.cols,
            self.rows,
            self.next_id,
            pane,
            self.proxy.clone(),
            &self.cwd,
        )?;
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

    /// Move tab from `from` index to `to` index (reorder).
    pub fn move_tab(&mut self, from: usize, to: usize) {
        if from >= self.tabs.len() || to >= self.tabs.len() || from == to {
            return;
        }
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);
        // Update active index if it was affected.
        if self.active == from {
            self.active = to;
        } else if from < self.active && to >= self.active {
            self.active -= 1;
        } else if from > self.active && to <= self.active {
            self.active += 1;
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

    /// Pin (or clear) the manual title of the tab at `idx`.
    pub fn set_pin(&mut self, idx: usize, name: Option<String>) {
        if let Some(t) = self.tabs.get_mut(idx) {
            t.set_pin(name);
        }
    }

    /// Pin (or clear) the manual title of the active tab.
    pub fn set_active_pin(&mut self, name: Option<String>) {
        let idx = self.active;
        self.set_pin(idx, name);
    }

    /// Drop the manual title of the active tab (live OSC titles return).
    pub fn unpin_active(&mut self) {
        self.set_active_pin(None);
    }

    pub fn active_index(&self) -> usize {
        self.active
    }

    pub fn len(&self) -> usize {
        self.tabs.len()
    }

    /// Iterate over all tabs.
    pub fn iter_tabs(&self) -> std::slice::Iter<'_, Tab> {
        self.tabs.iter()
    }

    /// Iterate over all tabs mutably.
    pub fn iter_tabs_mut(&mut self) -> std::slice::IterMut<'_, Tab> {
        self.tabs.iter_mut()
    }

    /// Get a mutable reference to a tab by index.
    pub fn get_tab_mut(&mut self, idx: usize) -> Option<&mut Tab> {
        self.tabs.get_mut(idx)
    }

    pub fn titles(&self) -> Vec<String> {
        self.tabs.iter().map(|t| t.title()).collect()
    }

    /// Drain all per-pane reader dirty flags. True when any PTY produced
    /// output since the last call.
    pub fn take_dirty(&self) -> bool {
        self.tabs.iter().any(|t| t.take_dirty())
    }

    /// Drain bell flags from every pane of every tab. True when the app
    /// rang the bell (visual bell).
    pub fn take_bell(&self) -> bool {
        let mut rang = false;
        for t in &self.tabs {
            if t.take_bell() {
                rang = true;
            }
        }
        rang
    }

    /// Refresh theme colors on all tabs after a config reload.
    pub fn apply_config(&mut self, cfg: &NeotermConfig) {
        for t in &mut self.tabs {
            t.apply_config(cfg);
        }
    }

    pub fn resize_all(&mut self, cols: u16, rows: u16) {
        self.cols = cols;
        self.rows = rows;
        for t in &mut self.tabs {
            t.resize_body(cols as usize, rows as usize);
        }
    }
}
