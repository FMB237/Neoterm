/// A single command in the palette.
#[derive(Clone, Debug)]
pub struct PaletteCommand {
    pub id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
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
            visible: false,
        }
    }

    fn build_commands() -> Vec<PaletteCommand> {
        vec![
            // Tabs & Windows
            PaletteCommand {
                id: "new-tab",
                label: "New Tab",
                description: "Open a new tab (Ctrl+Shift+T)",
                action: PaletteAction::NewTab,
            },
            PaletteCommand {
                id: "new-window",
                label: "New Window",
                description: "Open a new window (Ctrl+Shift+N)",
                action: PaletteAction::NewWindow,
            },
            PaletteCommand {
                id: "next-tab",
                label: "Next Tab",
                description: "Switch to next tab (Ctrl+Tab)",
                action: PaletteAction::NextTab,
            },
            PaletteCommand {
                id: "prev-tab",
                label: "Previous Tab",
                description: "Switch to previous tab (Ctrl+Shift+Tab)",
                action: PaletteAction::PrevTab,
            },
            PaletteCommand {
                id: "close-pane",
                label: "Close Pane / Tab",
                description: "Close active pane (last pane closes tab) (Ctrl+Shift+W)",
                action: PaletteAction::ClosePane,
            },
            PaletteCommand {
                id: "rename-tab",
                label: "Rename Tab",
                description: "Set a custom title for the active tab (Ctrl+Shift+R)",
                action: PaletteAction::RenameTab,
            },
            PaletteCommand {
                id: "unpin-tab",
                label: "Unpin Tab Title",
                description: "Restore automatic tab title from shell (Ctrl+Shift+U)",
                action: PaletteAction::UnpinTab,
            },
            // Splits
            PaletteCommand {
                id: "split-v",
                label: "Split Vertically",
                description: "Split pane side by side (Ctrl+Shift+E)",
                action: PaletteAction::SplitV,
            },
            PaletteCommand {
                id: "split-h",
                label: "Split Horizontally",
                description: "Split pane top/bottom (Ctrl+Shift+O)",
                action: PaletteAction::SplitH,
            },
            PaletteCommand {
                id: "focus-up",
                label: "Focus Pane Up",
                description: "Move focus to pane above (Alt+Up)",
                action: PaletteAction::FocusPaneUp,
            },
            PaletteCommand {
                id: "focus-down",
                label: "Focus Pane Down",
                description: "Move focus to pane below (Alt+Down)",
                action: PaletteAction::FocusPaneDown,
            },
            PaletteCommand {
                id: "focus-left",
                label: "Focus Pane Left",
                description: "Move focus to pane on the left (Alt+Left)",
                action: PaletteAction::FocusPaneLeft,
            },
            PaletteCommand {
                id: "focus-right",
                label: "Focus Pane Right",
                description: "Move focus to pane on the right (Alt+Right)",
                action: PaletteAction::FocusPaneRight,
            },
            PaletteCommand {
                id: "zoom-pane",
                label: "Toggle Pane Zoom",
                description: "Maximize/restore active pane (Ctrl+Shift+Z)",
                action: PaletteAction::ZoomPane,
            },
            // Font
            PaletteCommand {
                id: "font-zoom-in",
                label: "Increase Font Size",
                description: "Zoom in (Ctrl+Shift++)",
                action: PaletteAction::FontZoomIn,
            },
            PaletteCommand {
                id: "font-zoom-out",
                label: "Decrease Font Size",
                description: "Zoom out (Ctrl+Shift+-)",
                action: PaletteAction::FontZoomOut,
            },
            PaletteCommand {
                id: "font-zoom-reset",
                label: "Reset Font Size",
                description: "Reset to default (Ctrl+Shift+0)",
                action: PaletteAction::FontZoomReset,
            },
            // Clipboard
            PaletteCommand {
                id: "copy",
                label: "Copy",
                description: "Copy selection to clipboard (Ctrl+Shift+C)",
                action: PaletteAction::Copy,
            },
            PaletteCommand {
                id: "paste",
                label: "Paste",
                description: "Paste from clipboard (Ctrl+Shift+V)",
                action: PaletteAction::Paste,
            },
            // Search & Copymode
            PaletteCommand {
                id: "search",
                label: "Search Scrollback",
                description: "Open search overlay (Ctrl+Shift+F)",
                action: PaletteAction::SearchToggle,
            },
            PaletteCommand {
                id: "copymode",
                label: "Vi Copy Mode",
                description: "Toggle Vi-style keyboard selection (Ctrl+Shift+Space)",
                action: PaletteAction::CopymodeToggle,
            },
            // Broadcast
            PaletteCommand {
                id: "broadcast",
                label: "Toggle Broadcast Input",
                description: "Send keystrokes to all panes (Ctrl+Shift+B)",
                action: PaletteAction::BroadcastToggle,
            },
            // Themes
            PaletteCommand {
                id: "theme-tokyo-night",
                label: "Theme: Tokyo Night",
                description: "Default dark theme",
                action: PaletteAction::ThemeTokyoNight,
            },
            PaletteCommand {
                id: "theme-catppuccin-mocha",
                label: "Theme: Catppuccin Mocha",
                description: "Warm dark theme",
                action: PaletteAction::ThemeCatppuccinMocha,
            },
            PaletteCommand {
                id: "theme-dracula",
                label: "Theme: Dracula",
                description: "Purple dark theme",
                action: PaletteAction::ThemeDracula,
            },
            PaletteCommand {
                id: "theme-gruvbox-dark",
                label: "Theme: Gruvbox Dark",
                description: "Earthy dark theme",
                action: PaletteAction::ThemeGruvboxDark,
            },
            PaletteCommand {
                id: "theme-custom",
                label: "Theme: Custom",
                description: "Use custom colors from config",
                action: PaletteAction::ThemeCustom,
            },
            // Sessions
            PaletteCommand {
                id: "session-save",
                label: "Save Session",
                description: "Save current session for later restore",
                action: PaletteAction::SessionSave,
            },
            PaletteCommand {
                id: "session-load",
                label: "Load Session",
                description: "Load a saved session",
                action: PaletteAction::SessionLoad,
            },
            PaletteCommand {
                id: "session-list",
                label: "List Sessions",
                description: "Show all saved sessions",
                action: PaletteAction::SessionList,
            },
            // Performance
            PaletteCommand {
                id: "perf-hud",
                label: "Toggle Performance HUD",
                description: "Show FPS, frame time, GPU memory (Ctrl+Shift+Alt+F)",
                action: PaletteAction::TogglePerformanceHud,
            },
        ]
    }

    pub fn toggle(&mut self) {
        self.visible = !self.visible;
        if self.visible {
            self.query.clear();
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
        }
    }

    pub fn select_prev(&mut self) {
        if !self.filtered.is_empty() {
            self.selected = (self.selected + self.filtered.len() - 1) % self.filtered.len();
        }
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