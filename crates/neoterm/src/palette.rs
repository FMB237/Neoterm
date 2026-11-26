/// Default number of command rows shown in the palette before it scrolls.
/// The renderer lowers this on short windows.
const DEFAULT_VISIBLE_ROWS: usize = 8;

/// A single command in the palette.
#[derive(Clone, Debug)]
pub struct PaletteCommand {
    pub id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub keybinds: &'static str,
    pub action: PaletteAction,
}

/// Action to execute when a command is selected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaletteAction {
    NewTab,
    NewWindow,
    SplitV,
    SplitH,
    ClosePane,
    NextTab,
    PrevTab,
    FocusPaneUp,
    FocusPaneDown,
    FocusPaneLeft,
    FocusPaneRight,
    ZoomPane,
    FontZoomIn,
    FontZoomOut,
    FontZoomReset,
    RenameTab,
    UnpinTab,
    BroadcastToggle,
    CopymodeToggle,
    SearchToggle,
    ThemeTokyoNight,
    ThemeCatppuccinMocha,
    ThemeDracula,
    ThemeGruvboxDark,
    ThemeCustom,
    Copy,
    Paste,
    SessionSave,
    SessionLoad,
    SessionList,
    TogglePerformanceHud,
}

impl PaletteAction {
    /// Convert to an InputAction for execution.
    pub fn to_input_action(self) -> Option<crate::input::InputAction> {
        use crate::input::InputAction;
        Some(match self {
            PaletteAction::NewTab => InputAction::NewTab,
            PaletteAction::NewWindow => InputAction::NewWindow,
            PaletteAction::SplitV => InputAction::SplitV,
            PaletteAction::SplitH => InputAction::SplitH,
            PaletteAction::ClosePane => InputAction::ClosePane,
            PaletteAction::NextTab => InputAction::NextTab,
            PaletteAction::PrevTab => InputAction::PrevTab,
            PaletteAction::FocusPaneUp => InputAction::FocusPane(crate::input::FocusDir::Up),
            PaletteAction::FocusPaneDown => InputAction::FocusPane(crate::input::FocusDir::Down),
            PaletteAction::FocusPaneLeft => InputAction::FocusPane(crate::input::FocusDir::Left),
            PaletteAction::FocusPaneRight => InputAction::FocusPane(crate::input::FocusDir::Right),
            PaletteAction::ZoomPane => InputAction::ZoomPane,
            PaletteAction::FontZoomIn => InputAction::FontZoomIn,
            PaletteAction::FontZoomOut => InputAction::FontZoomOut,
            PaletteAction::FontZoomReset => InputAction::FontZoomReset,
            PaletteAction::RenameTab => InputAction::RenameTab,
            PaletteAction::UnpinTab => InputAction::UnpinTab,
            PaletteAction::BroadcastToggle => InputAction::BroadcastToggle,
            PaletteAction::CopymodeToggle => InputAction::CopymodeToggle,
            PaletteAction::SearchToggle => InputAction::SearchToggle,
            PaletteAction::Copy => InputAction::Copy,
            PaletteAction::Paste => InputAction::Paste,
            _ => return None,
        })
    }
}

/// State for the command palette overlay.
pub struct PaletteState {
    pub query: String,
    pub commands: Vec<PaletteCommand>,
    pub filtered: Vec<usize>, // indices into commands
    pub selected: usize,
    /// First visible row in `filtered` (scrolling window).
    pub scroll: usize,
    /// Number of command rows the renderer can fit on screen.
    pub visible_rows: usize,
    pub visible: bool,
}

impl PaletteState {
    pub fn new() -> Self {
        let commands = Self::build_commands();
        let filtered = (0..commands.len()).collect();
        Self {
            query: String::new(),
            commands,
            filtered,
            selected: 0,
            scroll: 0,
            visible_rows: DEFAULT_VISIBLE_ROWS,
            visible: false,
        }
    }

    fn build_commands() -> Vec<PaletteCommand> {
        vec![
            // Tabs & Windows
            PaletteCommand {
                id: "new-tab",
                label: "New Tab",
                description: "Open a new tab",
                keybinds: "Ctrl+Shift+T",
                action: PaletteAction::NewTab,
            },
            PaletteCommand {
                id: "new-window",
                label: "New Window",
                description: "Open a new window",
                keybinds: "Ctrl+Shift+N",
                action: PaletteAction::NewWindow,
            },
            PaletteCommand {
                id: "next-tab",
                label: "Next Tab",
                description: "Switch to next tab",
                keybinds: "Ctrl+Tab",
                action: PaletteAction::NextTab,
            },
            PaletteCommand {
                id: "prev-tab",
                label: "Previous Tab",
                description: "Switch to previous tab",
                keybinds: "Ctrl+Shift+Tab",
                action: PaletteAction::PrevTab,
            },
            PaletteCommand {
                id: "close-pane",
                label: "Close Pane / Tab",
                description: "Close active pane (last pane closes tab)",
                keybinds: "Ctrl+Shift+W",
                action: PaletteAction::ClosePane,
            },
            PaletteCommand {
                id: "rename-tab",
                label: "Rename Tab",
                description: "Set a custom title for the active tab",
                keybinds: "Ctrl+Shift+R",
                action: PaletteAction::RenameTab,
            },
            PaletteCommand {
                id: "unpin-tab",
                label: "Unpin Tab Title",
                description: "Restore automatic tab title from shell",
                keybinds: "Ctrl+Shift+U",
                action: PaletteAction::UnpinTab,
            },
            // Splits
            PaletteCommand {
                id: "split-v",
                label: "Split Vertically",
                description: "Split pane side by side",
                keybinds: "Ctrl+Shift+E",
                action: PaletteAction::SplitV,
            },
            PaletteCommand {
                id: "split-h",
                label: "Split Horizontally",
                description: "Split pane top/bottom",
                keybinds: "Ctrl+Shift+O",
                action: PaletteAction::SplitH,
            },
            PaletteCommand {
                id: "focus-up",
                label: "Focus Pane Up",
                description: "Move focus to pane above",
                keybinds: "Alt+Up",
                action: PaletteAction::FocusPaneUp,
            },
            PaletteCommand {
                id: "focus-down",
                label: "Focus Pane Down",
                description: "Move focus to pane below",
                keybinds: "Alt+Down",
                action: PaletteAction::FocusPaneDown,
            },
            PaletteCommand {
                id: "focus-left",
                label: "Focus Pane Left",
                description: "Move focus to pane on the left",
                keybinds: "Alt+Left",
                action: PaletteAction::FocusPaneLeft,
            },
            PaletteCommand {
                id: "focus-right",
                label: "Focus Pane Right",
                description: "Move focus to pane on the right",
                keybinds: "Alt+Right",
                action: PaletteAction::FocusPaneRight,
            },
            PaletteCommand {
                id: "zoom-pane",
                label: "Toggle Pane Zoom",
                description: "Maximize/restore active pane",
                keybinds: "Ctrl+Shift+Z",
                action: PaletteAction::ZoomPane,
            },
            // Font
            PaletteCommand {
                id: "font-zoom-in",
                label: "Increase Font Size",
                description: "Zoom in",
                keybinds: "Ctrl+Shift++",
                action: PaletteAction::FontZoomIn,
            },
            PaletteCommand {
                id: "font-zoom-out",
                label: "Decrease Font Size",
                description: "Zoom out",
                keybinds: "Ctrl+Shift+-",
                action: PaletteAction::FontZoomOut,
            },
            PaletteCommand {
                id: "font-zoom-reset",
                label: "Reset Font Size",
                description: "Reset to default",
                keybinds: "Ctrl+Shift+0",
                action: PaletteAction::FontZoomReset,
            },
            // Clipboard
            PaletteCommand {
                id: "copy",
                label: "Copy",
                description: "Copy selection to clipboard",
                keybinds: "Ctrl+Shift+C",
                action: PaletteAction::Copy,
            },
            PaletteCommand {
                id: "paste",
                label: "Paste",
                description: "Paste from clipboard",
                keybinds: "Ctrl+Shift+V",
                action: PaletteAction::Paste,
            },
            // Search & Copymode
            PaletteCommand {
                id: "search",
                label: "Search Scrollback",
                description: "Open search overlay",
                keybinds: "Ctrl+Shift+F",
                action: PaletteAction::SearchToggle,
            },
            PaletteCommand {
                id: "copymode",
                label: "Vi Copy Mode",
                description: "Toggle Vi-style keyboard selection",
                keybinds: "Ctrl+Shift+Space",
                action: PaletteAction::CopymodeToggle,
            },
            // Broadcast
            PaletteCommand {
                id: "broadcast",
                label: "Toggle Broadcast Input",
                description: "Send keystrokes to all panes",
                keybinds: "Ctrl+Shift+B",
                action: PaletteAction::BroadcastToggle,
            },
            // Themes
            PaletteCommand {
                id: "theme-tokyo-night",
                label: "Theme: Tokyo Night",
                description: "Default dark theme",
                keybinds: "",
                action: PaletteAction::ThemeTokyoNight,
            },
            PaletteCommand {
                id: "theme-catppuccin-mocha",
                label: "Theme: Catppuccin Mocha",
                description: "Warm dark theme",
                keybinds: "",
                action: PaletteAction::ThemeCatppuccinMocha,
            },
            PaletteCommand {
                id: "theme-dracula",
                label: "Theme: Dracula",
                description: "Purple dark theme",
                keybinds: "",
                action: PaletteAction::ThemeDracula,
            },
            PaletteCommand {
                id: "theme-gruvbox-dark",
                label: "Theme: Gruvbox Dark",
                description: "Earthy dark theme",
                keybinds: "",
                action: PaletteAction::ThemeGruvboxDark,
            },
            PaletteCommand {
                id: "theme-custom",
                label: "Theme: Custom",
                description: "Use custom colors from config",
                keybinds: "",
                action: PaletteAction::ThemeCustom,
            },
            // Sessions
            PaletteCommand {
                id: "session-save",
                label: "Save Session",
                description: "Save current session for later restore",
                keybinds: "",
                action: PaletteAction::SessionSave,
            },
            PaletteCommand {
                id: "session-load",
                label: "Load Session",
                description: "Load a saved session",
                keybinds: "",
                action: PaletteAction::SessionLoad,
            },
            PaletteCommand {
                id: "session-list",
                label: "List Sessions",
                description: "Show all saved sessions",
                keybinds: "",
                action: PaletteAction::SessionList,
            },
            // Performance
            PaletteCommand {
                id: "perf-hud",
                label: "Toggle Performance HUD",
                description: "Show FPS, frame time, GPU memory",
                keybinds: "Ctrl+Shift+Alt+F",
                action: PaletteAction::TogglePerformanceHud,
            },
        ]
    }

    pub fn toggle(&mut self) {
        self.visible = !self.visible;
        if self.visible {
            self.query.clear();
            // A fresh open always starts at the top, not at whatever was
            // selected the last time the palette was shown.
            self.selected = 0;
            self.scroll = 0;
            self.filter();
        }
    }

    pub fn close(&mut self) {
        self.visible = false;
        self.query.clear();
        self.filter();
    }

    pub fn type_char(&mut self, c: char) {
        self.query.push(c);
        self.filter();
    }

    pub fn backspace(&mut self) {
        self.query.pop();
        self.filter();
    }

    pub fn select_next(&mut self) {
        if !self.filtered.is_empty() {
            self.selected = (self.selected + 1) % self.filtered.len();
            self.ensure_selected_visible();
        }
    }

    pub fn select_prev(&mut self) {
        if !self.filtered.is_empty() {
            self.selected = (self.selected + self.filtered.len() - 1) % self.filtered.len();
            self.ensure_selected_visible();
        }
    }

    /// Set how many command rows fit on screen and scroll the selection
    /// back into view. Called by the renderer once it knows the panel size.
    pub fn set_visible_rows(&mut self, rows: usize) {
        self.visible_rows = rows.max(1);
        self.ensure_selected_visible();
    }

    /// Scroll the window so the selected command is on screen.
    fn ensure_selected_visible(&mut self) {
        if self.filtered.is_empty() {
            self.scroll = 0;
            return;
        }
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + self.visible_rows {
            self.scroll = self.selected + 1 - self.visible_rows;
        }
        let max_scroll = self.filtered.len().saturating_sub(self.visible_rows);
        self.scroll = self.scroll.min(max_scroll);
    }

    /// The slice of `filtered` currently visible in the panel.
    pub fn visible_range(&self) -> std::ops::Range<usize> {
        let end = (self.scroll + self.visible_rows).min(self.filtered.len());
        self.scroll.min(end)..end
    }

    pub fn execute_selected(&self) -> Option<PaletteAction> {
        self.filtered.get(self.selected).map(|&idx| self.commands[idx].action)
    }

    fn filter(&mut self) {
        if self.query.is_empty() {
            self.filtered = (0..self.commands.len()).collect();
        } else {
            let q = self.query.to_lowercase();
            self.filtered = self
                .commands
                .iter()
                .enumerate()
                .filter(|(_, cmd)| {
                    cmd.label.to_lowercase().contains(&q)
                        || cmd.description.to_lowercase().contains(&q)
                        || cmd.id.contains(&q)
                })
                .map(|(i, _)| i)
                .collect();
        }
        if self.selected >= self.filtered.len() {
            self.selected = self.filtered.len().saturating_sub(1);
        }
        // A narrower filter always starts its window at the top, then pulls
        // the selection back into view if it survived the filter.
        self.scroll = 0;
        self.ensure_selected_visible();
    }

    pub fn get_selected_command(&self) -> Option<&PaletteCommand> {
        self.filtered
            .get(self.selected)
            .map(|&idx| &self.commands[idx])
    }

    pub fn visible_commands(&self) -> impl Iterator<Item = &PaletteCommand> {
        self.filtered.iter().map(|&idx| &self.commands[idx])
    }
}

impl Default for PaletteState {
    fn default() -> Self {
        Self::new()
    }
}

/// Fuzzy score for ranking (simple substring match for now).
pub fn fuzzy_score(query: &str, text: &str) -> Option<usize> {
    let q = query.to_lowercase();
    let t = text.to_lowercase();
    t.find(&q).map(|pos| pos)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_toggle() {
        let mut state = PaletteState::new();
        assert!(!state.visible);
        state.toggle();
        assert!(state.visible);
        state.toggle();
        assert!(!state.visible);
    }

    #[test]
    fn palette_filter_matches_commands() {
        let mut state = PaletteState::new();
        state.type_char('n');
        state.type_char('e');
        state.type_char('w');
        // Should filter to commands containing "new"
        assert!(!state.filtered.is_empty());
        assert!(state
            .filtered
            .iter()
            .any(|&i| state.commands[i].id == "new-tab"
                || state.commands[i].id == "new-window"));
    }

    #[test]
    fn palette_select_next_and_prev() {
        let mut state = PaletteState::new();
        state.filter(); // show all
        let initial = state.selected;
        state.select_next();
        assert_eq!(state.selected, (initial + 1) % state.filtered.len());
        state.select_prev();
        assert_eq!(state.selected, initial);
    }

    #[test]
    fn palette_execute_selected_returns_action() {
        let mut state = PaletteState::new();
        state.filter(); // show all
        state.selected = 0;
        let action = state.execute_selected();
        assert!(action.is_some());
    }

    #[test]
    fn palette_scrolls_to_keep_selection_visible() {
        let mut state = PaletteState::new();
        state.set_visible_rows(5);
        state.filter();
        assert!(state.filtered.len() > 5, "need a list longer than the window");

        for _ in 0..7 {
            state.select_next();
        }
        assert_eq!(state.selected, 7);
        assert_eq!(state.visible_range(), 3..8);
        assert_eq!(state.scroll, 3);
    }

    #[test]
    fn palette_scrolls_back_up_with_previous() {
        let mut state = PaletteState::new();
        state.set_visible_rows(4);
        state.filter();
        for _ in 0..6 {
            state.select_next();
        }
        assert_eq!(state.scroll, 3);
        for _ in 0..4 {
            state.select_prev();
        }
        assert_eq!(state.selected, 2);
        assert_eq!(state.scroll, 2);
        assert_eq!(state.visible_range(), 2..6);
    }

    #[test]
    fn palette_opens_at_the_top_again() {
        let mut state = PaletteState::new();
        state.set_visible_rows(5);
        state.filter();
        for _ in 0..6 {
            state.select_next();
        }
        state.close();
        state.toggle();
        assert_eq!(state.selected, 0);
        assert_eq!(state.scroll, 0);
    }

    #[test]
    fn palette_filter_pulls_selection_into_view() {
        let mut state = PaletteState::new();
        state.set_visible_rows(3);
        state.filter();
        for _ in 0..5 {
            state.select_next();
        }
        assert_eq!(state.scroll, 3);
        // Typing a filter that keeps the selection but shrinks the list must
        // not leave the highlight below the visible window.
        state.type_char('t');
        assert!(state.selected < state.filtered.len());
        let range = state.visible_range();
        assert!(range.contains(&state.selected), "{range:?} vs {}", state.selected);
    }

    #[test]
    fn fuzzy_score_returns_position() {
        assert_eq!(fuzzy_score("neo", "NeoTerm"), Some(0));
        assert_eq!(fuzzy_score("term", "NeoTerm"), Some(3));
        assert_eq!(fuzzy_score("xyz", "NeoTerm"), None);
    }
}