use crate::config::NeotermConfig;
use crate::tab::{PaneId, PaneNode, SplitDir};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::collections::HashMap;

/// Session state for a single pane.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaneState {
    pub id: PaneId,
    pub cwd: PathBuf,
    pub cols: usize,
    pub rows: usize,
}

/// Session state for a split node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SplitNodeState {
    pub dir: SplitDir,
    pub ratio: f32,
    pub children: Vec<NodeState>,
}

/// Session state for a tree node (either leaf pane or split).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum NodeState {
    Leaf(PaneState),
    Split(SplitNodeState),
}

/// Session state for a single tab.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TabState {
    pub id: usize,
    pub title: Option<String>, // pinned title if any
    pub active_pane: PaneId,
    pub zoomed_pane: Option<PaneId>,
    pub root: NodeState,
    pub panes: Vec<PaneState>, // all panes in this tab
}

/// Session state for a single window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowState {
    pub tabs: Vec<TabState>,
    pub active_tab: usize,
    pub size: Option<(u32, u32)>, // window size
}

/// Full session state (all windows).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionState {
    pub windows: Vec<WindowState>,
    pub version: u32,
}

impl SessionState {
    pub const VERSION: u32 = 1;

    pub fn new() -> Self {
        Self {
            windows: Vec::new(),
            version: Self::VERSION,
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(path, json).context("write session file")?;
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Self> {
        let json = std::fs::read_to_string(path).context("read session file")?;
        let state: SessionState = serde_json::from_str(&json).context("parse session file")?;
        if state.version != Self::VERSION {
            anyhow::bail!("session version mismatch: expected {}, got {}", Self::VERSION, state.version);
        }
        Ok(state)
    }
}

impl Default for SessionState {
    fn default() -> Self {
        Self::new()
    }
}

/// Build session state from current application state.
pub fn build_session_state(app: &crate::App) -> SessionState {
    let mut session = SessionState::new();

    for (_window_id, ws) in &app.windows {
        let mut window_state = WindowState {
            tabs: Vec::new(),
            active_tab: ws.tabs.active_index(),
            size: Some(ws.window.inner_size().into()),
        };

        for tab in ws.tabs.iter_tabs() {
            let tab_state = build_tab_state(tab);
            window_state.tabs.push(tab_state);
        }

        session.windows.push(window_state);
    }

    session
}

/// Build tab state from a Tab.
fn build_tab_state(tab: &crate::tab::Tab) -> TabState {
    let mut panes = Vec::new();
    for (id, pane) in tab.panes() {
        let (cols, rows) = tab.sizes().get(id).copied().unwrap_or((80, 24));
        panes.push(PaneState {
            id: *id,
            cwd: pane.get_cwd(),
            cols,
            rows,
        });
    }

    let root = build_node_state(tab.root(), tab.panes(), tab.sizes());

    TabState {
        id: tab.tab_id(),
        title: tab.pinned_title().cloned(),
        active_pane: tab.active_pane_id(),
        zoomed_pane: tab.zoomed_pane_id(),
        root,
        panes,
    }
}

/// Recursively build node state from PaneNode.
fn build_node_state(
    node: &Option<PaneNode>,
    panes: &HashMap<PaneId, crate::tab::Pane>,
    sizes: &HashMap<PaneId, (usize, usize)>,
) -> NodeState {
    match node {
        Some(PaneNode::Leaf(id)) => {
            let (cols, rows) = sizes.get(id).copied().unwrap_or((80, 24));
            if let Some(pane) = panes.get(id) {
                NodeState::Leaf(PaneState {
                    id: *id,
                    cwd: pane.get_cwd(),
                    cols,
                    rows,
                })
            } else {
                NodeState::Leaf(PaneState {
                    id: *id,
                    cwd: std::env::current_dir().unwrap_or_default(),
                    cols,
                    rows,
                })
            }
        }
        Some(PaneNode::Split { dir, ratio, first, second }) => {
            let mut child_states = Vec::new();
            child_states.push(build_node_state(&Some(*first.clone()), panes, sizes));
            child_states.push(build_node_state(&Some(*second.clone()), panes, sizes));
            NodeState::Split(SplitNodeState {
                dir: *dir,
                ratio: *ratio,
                children: child_states,
            })
        }
        None => NodeState::Leaf(PaneState {
            id: 0,
            cwd: std::env::current_dir().unwrap_or_default(),
            cols: 80,
            rows: 24,
        }),
    }
}

/// Restore a session state into the application.
pub fn restore_session_state(
    session: &SessionState,
    app: &mut crate::App,
    event_loop: &winit::event_loop::ActiveEventLoop,
) -> Result<()> {
    // Clear existing windows
    app.windows.clear();

    for (window_idx, window_state) in session.windows.iter().enumerate() {
        let window = if window_idx == 0 {
            // Reuse the first window creation logic
            let window = app.create_window(event_loop)?;
            window
        } else {
            app.create_window_with_config(event_loop)
                .context("create additional window")?
        };

        let window_id = window.id();
        let ws = app.windows.get_mut(&window_id).context("window state not found")?;

        // Restore tabs
        for (tab_idx, tab_state) in window_state.tabs.iter().enumerate() {
            let tab = if tab_idx == 0 {
                // First tab already exists
                ws.tabs.get_tab_mut(0).context("first tab missing")?
            } else {
                ws.tabs.new_tab(&app.cfg)?;
                ws.tabs.get_tab_mut(ws.tabs.len() - 1).context("new tab missing")?
            };

            restore_tab_state(tab, tab_state, &app.cfg, app.proxy.clone())?;
        }

        // Select active tab
        ws.tabs.select(window_state.active_tab);
        // TODO: Restore window size when winit API supports it
        // if let Some((w, h)) = window_state.size {
        //     ws.window.set_inner_size(winit::dpi::PhysicalSize::new(w, h));
        // }
    }

    Ok(())
}

/// Restore a single tab's state.
fn restore_tab_state(
    tab: &mut crate::tab::Tab,
    state: &TabState,
    cfg: &NeotermConfig,
    proxy: winit::event_loop::EventLoopProxy<()>,
) -> Result<()> {
    // Set pinned title
    tab.set_pinned_title(state.title.clone());

    // Set active and zoomed panes
    tab.set_active_pane_id(state.active_pane);
    tab.set_zoomed_pane_id(state.zoomed_pane);

    // Restore panes and layout
    let mut new_panes = HashMap::new();
    let mut new_sizes = HashMap::new();

    // First create all panes
    for pane_state in &state.panes {
        let pane = crate::tab::Pane::new(
            cfg,
            pane_state.cols as u16,
            pane_state.rows as u16,
            format!("tab-{}-pane-{}", state.id, pane_state.id),
            proxy.clone(),
            &pane_state.cwd,
        )?;
        new_panes.insert(pane_state.id, pane);
        new_sizes.insert(pane_state.id, (pane_state.cols, pane_state.rows));
    }

    // Replace panes and sizes
    tab.replace_panes(new_panes);
    tab.replace_sizes(new_sizes);

    // Then restore the tree structure
    tab.replace_root(Some(restore_node_state(&state.root, &tab.panes(), &tab.sizes())));

    Ok(())
}

/// Recursively restore node state into PaneNode.
fn restore_node_state(
    state: &NodeState,
    panes: &HashMap<PaneId, crate::tab::Pane>,
    sizes: &HashMap<PaneId, (usize, usize)>,
) -> PaneNode {
    match state {
        NodeState::Leaf(pane_state) => PaneNode::Leaf(pane_state.id),
        NodeState::Split(split_state) => {
            // SplitNodeState has exactly 2 children (binary tree)
            let first = Box::new(restore_node_state(&split_state.children[0], panes, sizes));
            let second = Box::new(restore_node_state(&split_state.children[1], panes, sizes));
            PaneNode::Split {
                dir: split_state.dir,
                ratio: split_state.ratio,
                first,
                second,
            }
        }
    }
}

/// Get the session file path (default session).
pub fn session_file_path() -> PathBuf {
    let config_dir = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    config_dir.join("neoterm").join("session.json")
}

/// Get the sessions directory path.
pub fn sessions_dir() -> PathBuf {
    let config_dir = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    config_dir.join("neoterm").join("sessions")
}

/// Get a named session file path.
pub fn named_session_path(name: &str) -> PathBuf {
    let dir = sessions_dir();
    // Sanitize name: only alphanumeric, dash, underscore
    let safe: String = name.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
    if safe.is_empty() {
        dir.join("default.json")
    } else {
        dir.join(format!("{}.json", safe))
    }
}

/// List all saved session names.
pub fn list_sessions() -> Result<Vec<String>> {
    let dir = sessions_dir();
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut names = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("json") {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                names.push(stem.to_string());
            }
        }
    }
    names.sort();
    Ok(names)
}

/// Delete a named session.
pub fn delete_session(name: &str) -> Result<()> {
    let path = named_session_path(name);
    if path.exists() {
        std::fs::remove_file(path).context("delete session file")?;
    }
    Ok(())
}

/// Ensure sessions directory exists.
pub fn ensure_sessions_dir() -> Result<()> {
    let dir = sessions_dir();
    if !dir.exists() {
        std::fs::create_dir_all(dir).context("create sessions directory")?;
    }
    Ok(())
}

/// Get the autosave preference file path.
pub fn autosave_path() -> PathBuf {
    sessions_dir().join("autosave")
}

/// Read the autosave-on-exit preference. Defaults to `false`.
pub fn get_autosave() -> bool {
    let path = autosave_path();
    match std::fs::read_to_string(&path) {
        Ok(contents) => contents.trim() == "on",
        Err(_) => false,
    }
}

/// Write the autosave-on-exit preference.
pub fn set_autosave(enabled: bool) -> Result<()> {
    ensure_sessions_dir()?;
    let path = autosave_path();
    std::fs::write(path, if enabled { "on" } else { "off" }).context("write autosave preference")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // The autosave preference is a single file on disk, so both assertions
    // live in one test: splitting them let the parallel test harness delete
    // the file between `set_autosave` and `get_autosave`.
    #[test]
    fn autosave_preference_round_trip() {
        let _ = std::fs::remove_file(autosave_path());
        assert!(!get_autosave(), "missing file means autosave is off");

        // If the filesystem is read-only (e.g., sandbox), skip the write half.
        if set_autosave(true).is_err() {
            return;
        }
        assert!(get_autosave());

        set_autosave(false).unwrap();
        assert!(!get_autosave());
    }
}