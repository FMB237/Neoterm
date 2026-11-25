use winit::event::{ElementState, KeyEvent, Modifiers};
use winit::keyboard::{Key, ModifiersState, NamedKey};

/// Result of handling a key event.
pub enum InputAction {
    /// Bytes to send to the PTY.
    PtyInput(Vec<u8>),
    NewTab,
    NextTab,
    PrevTab,
    SelectTab(usize),
    /// Copy active selection (or visible screen) to the OS clipboard.
    Copy,
    /// Paste OS clipboard text into the PTY.
    Paste,
    /// Open the find/search overlay.
    SearchToggle,
    /// Jump to the next search match.
    SearchNext,
    /// Jump to the previous search match.
    SearchPrev,
    /// Cancel the active search and close the overlay.
    SearchCancel,
    /// Scroll viewport one page (local scrollback, not sent to the app).
    ScrollPageUp,
    ScrollPageDown,
    /// Split the active pane: side-by-side (V) or stacked (H).
    /// Terminator convention: Ctrl+Shift+E / Ctrl+Shift+O.
    SplitV,
    SplitH,
    /// Close the active pane (last pane = close the tab).
    ClosePane,
    /// Move focus to the neighboring pane (Alt+arrows).
    FocusPane(FocusDir),
    /// Toggle fullscreen zoom of the active pane.
    ZoomPane,
    /// Font-size zoom (session-only): Ctrl+Shift+Plus / Minus / 0.
    FontZoomIn,
    FontZoomOut,
    FontZoomReset,
    /// Start editing the active tab's title (Ctrl+Shift+R).
    RenameTab,
    /// Drop the active tab's manual title, back to the live OSC one.
    UnpinTab,
    /// Toggle broadcast input mode (send keystrokes to all panes).
    BroadcastToggle,
    /// Create a new OS window.
    NewWindow,
    /// Toggle Vi-style copy mode (Ctrl+Shift+Space).
    CopymodeToggle,
    /// Toggle command palette (Ctrl+Shift+P).
    PaletteToggle,
    /// Copymode navigation (only active in copymode).
    CopymodeUp,
    CopymodeDown,
    CopymodeLeft,
    CopymodeRight,
    CopymodePageUp,
    CopymodePageDown,
    CopymodeHome,
    CopymodeEnd,
    /// Start/extend selection in copymode (v/V).
    CopymodeSelect,
    CopymodeSelectLine,
    /// Copy selection and exit copymode (y).
    CopymodeCopy,
    /// Exit copymode without copying (Escape, q, Ctrl+C).
    CopymodeExit,
    Ignored,
}

/// Focus movement direction (Alt+arrows).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FocusDir {
    Up,
    Down,
    Left,
    Right,
}

/// Map winit key events to terminal input or tab actions.
///
/// Chrome shortcuts (tabs, splits, clipboard, font zoom, Alt+arrows) are
/// matched by `chrome_action` first; everything else becomes PTY bytes.
/// `alt_screen` routes PageUp/PageDown: apps on the alternate screen
/// (vim, less) get the key, otherwise we scroll locally — unless Shift is
/// held, which always scrolls locally.
pub fn map_key(event: &KeyEvent, mods: &Modifiers, alt_screen: bool) -> InputAction {
    if event.state != ElementState::Pressed {
        return InputAction::Ignored;
    }

    // Window/app chrome shortcuts first (never reach the PTY).
    if let Some(action) = chrome_action(&event.logical_key, mods.state()) {
        return action;
    }

    let ctrl = mods.state().control_key();
    let shift = mods.state().shift_key();

    // Special keys.
    match &event.logical_key {
        Key::Named(NamedKey::Enter) => return InputAction::PtyInput(vec![b'\r']),
        Key::Named(NamedKey::Backspace) => return InputAction::PtyInput(vec![0x7f]),
        Key::Named(NamedKey::Tab) => return InputAction::PtyInput(vec![b'\t']),
        Key::Named(NamedKey::Escape) => return InputAction::PtyInput(vec![0x1b]),
        Key::Named(NamedKey::ArrowUp) => {
            if !alt_screen {
                if let Some(action) = copymode_key(&Key::Named(NamedKey::ArrowUp)) {
                    return action;
                }
            }
            return InputAction::PtyInput(b"\x1b[A".to_vec());
        }
        Key::Named(NamedKey::ArrowDown) => {
            if !alt_screen {
                if let Some(action) = copymode_key(&Key::Named(NamedKey::ArrowDown)) {
                    return action;
                }
            }
            return InputAction::PtyInput(b"\x1b[B".to_vec());
        }
        Key::Named(NamedKey::ArrowRight) => {
            if !alt_screen {
                if let Some(action) = copymode_key(&Key::Named(NamedKey::ArrowRight)) {
                    return action;
                }
            }
            return InputAction::PtyInput(b"\x1b[C".to_vec());
        }
        Key::Named(NamedKey::ArrowLeft) => {
            if !alt_screen {
                if let Some(action) = copymode_key(&Key::Named(NamedKey::ArrowLeft)) {
                    return action;
                }
            }
            return InputAction::PtyInput(b"\x1b[D".to_vec());
        }
        Key::Named(NamedKey::Home) => {
            if !alt_screen {
                if let Some(action) = copymode_key(&Key::Named(NamedKey::Home)) {
                    return action;
                }
            }
            return InputAction::PtyInput(b"\x1b[H".to_vec());
        }
        Key::Named(NamedKey::End) => {
            if !alt_screen {
                if let Some(action) = copymode_key(&Key::Named(NamedKey::End)) {
                    return action;
                }
            }
            return InputAction::PtyInput(b"\x1b[F".to_vec());
        }
        Key::Named(NamedKey::PageUp) => {
            if alt_screen && !shift {
                return InputAction::PtyInput(b"\x1b[5~".to_vec());
            }
            if !alt_screen {
                if let Some(action) = copymode_key(&Key::Named(NamedKey::PageUp)) {
                    return action;
                }
            }
            return InputAction::ScrollPageUp;
        }
        Key::Named(NamedKey::PageDown) => {
            if alt_screen && !shift {
                return InputAction::PtyInput(b"\x1b[6~".to_vec());
            }
            if !alt_screen {
                if let Some(action) = copymode_key(&Key::Named(NamedKey::PageDown)) {
                    return action;
                }
            }
            return InputAction::ScrollPageDown;
        }
        Key::Named(NamedKey::Delete) => return InputAction::PtyInput(b"\x1b[3~".to_vec()),
        Key::Character(s) if !alt_screen && (s == "v" || s == "V" || s == "y") => {
            if let Some(action) = copymode_key(&Key::Character(s.clone())) {
                return action;
            }
        }
        _ => {}
    }

    // Ctrl+letter -> control codes (Ctrl+C, Ctrl+D, ...).
    if ctrl {
        if let Key::Character(s) = &event.logical_key {
            if s.len() == 1 {
                let c = s.chars().next().unwrap().to_ascii_lowercase();
                if ('a'..='z').contains(&c) {
                    return InputAction::PtyInput(vec![(c as u8) - b'a' + 1]);
                }
            }
        }
        return InputAction::Ignored;
    }

    // Printable text (includes paste via IME / dead keys resolved by winit).
    if let Some(text) = event.text.as_ref() {
        if !text.is_empty() {
            return InputAction::PtyInput(text.as_bytes().to_vec());
        }
    }

    InputAction::Ignored
}

/// Chrome shortcuts: tabs, splits, clipboard, font zoom, focus movement.
///
/// NOTE: Ctrl+Shift+C/V never reach the PTY (unlike Ctrl+C = SIGINT).
/// This matches GNOME Terminal / Alacritty behavior.
pub fn chrome_action(key: &Key, mods: ModifiersState) -> Option<InputAction> {
    let ctrl = mods.control_key();
    let shift = mods.shift_key();

    if ctrl && shift {
        match key {
            Key::Character(s) => match s.to_uppercase().as_str() {
                "T" => return Some(InputAction::NewTab),
                "W" => return Some(InputAction::ClosePane),
                "N" => return Some(InputAction::NewWindow),
                "P" => return Some(InputAction::PrevTab),
                "C" => return Some(InputAction::Copy),
                "V" => return Some(InputAction::Paste),
                "F" => return Some(InputAction::SearchToggle),
                "E" => return Some(InputAction::SplitV),
                "O" => return Some(InputAction::SplitH),
                "Z" => return Some(InputAction::ZoomPane),
                // Layout-dependent symbols: accept both the shifted and
                // unshifted forms (`+`/`=`, `-`/`_`, `0`/`)`).
                "+" | "=" => return Some(InputAction::FontZoomIn),
                "-" | "_" => return Some(InputAction::FontZoomOut),
                "0" | ")" => return Some(InputAction::FontZoomReset),
                "R" => return Some(InputAction::RenameTab),
                "U" => return Some(InputAction::UnpinTab),
                "B" => return Some(InputAction::BroadcastToggle),
                " " => return Some(InputAction::CopymodeToggle),
                "P" => return Some(InputAction::PaletteToggle),
                _ => {}
            },
            Key::Named(NamedKey::Tab) => return Some(InputAction::NextTab),
            _ => {}
        }
    }
    if ctrl && !shift {
        match key {
            Key::Named(NamedKey::Tab) => return Some(InputAction::NextTab),
            Key::Character(s) => {
                // Alt-style: Ctrl+1..9 switches tabs.
                if let Ok(n) = s.parse::<usize>() {
                    if (1..=9).contains(&n) {
                        return Some(InputAction::SelectTab(n - 1));
                    }
                }
            }
            _ => {}
        }
    }

    // Alt+arrows move pane focus (stolen from apps: multiplexers own it,
    // same as tmux prefix+arrows). Plain/shifted arrows still reach the PTY.
    if mods.alt_key() {
        match key {
            Key::Named(NamedKey::ArrowUp) => return Some(InputAction::FocusPane(FocusDir::Up)),
            Key::Named(NamedKey::ArrowDown) => return Some(InputAction::FocusPane(FocusDir::Down)),
            Key::Named(NamedKey::ArrowRight) => {
                return Some(InputAction::FocusPane(FocusDir::Right))
            }
            Key::Named(NamedKey::ArrowLeft) => {
                return Some(InputAction::FocusPane(FocusDir::Left))
            }
            _ => {}
        }
    }
    None
}

/// Copymode navigation keys (no modifiers - only active when copymode is on).
/// These are mapped by `map_key` when `alt_screen` is false to ensure
/// they don't reach the PTY. The copymode handler in main.rs intercepts
/// them first when `ws.copymode.is_some()`.
fn copymode_key(key: &Key) -> Option<InputAction> {
    match key {
        Key::Named(NamedKey::ArrowUp) => Some(InputAction::CopymodeUp),
        Key::Named(NamedKey::ArrowDown) => Some(InputAction::CopymodeDown),
        Key::Named(NamedKey::ArrowLeft) => Some(InputAction::CopymodeLeft),
        Key::Named(NamedKey::ArrowRight) => Some(InputAction::CopymodeRight),
        Key::Named(NamedKey::PageUp) => Some(InputAction::CopymodePageUp),
        Key::Named(NamedKey::PageDown) => Some(InputAction::CopymodePageDown),
        Key::Named(NamedKey::Home) => Some(InputAction::CopymodeHome),
        Key::Named(NamedKey::End) => Some(InputAction::CopymodeEnd),
        Key::Character(s) if s == "v" => Some(InputAction::CopymodeSelect),
        Key::Character(s) if s == "V" => Some(InputAction::CopymodeSelectLine),
        Key::Character(s) if s == "y" => Some(InputAction::CopymodeCopy),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(ch: &str, mods: ModifiersState) -> Option<InputAction> {
        chrome_action(&Key::Character(ch.into()), mods)
    }

    fn ctrl_shift() -> ModifiersState {
        ModifiersState::CONTROL | ModifiersState::SHIFT
    }

    #[test]
    fn font_zoom_keys_match_shifted_and_unshifted_symbols() {
        assert!(matches!(
            press("+", ctrl_shift()),
            Some(InputAction::FontZoomIn)
        ));
        assert!(matches!(
            press("=", ctrl_shift()),
            Some(InputAction::FontZoomIn)
        ));
        assert!(matches!(
            press("-", ctrl_shift()),
            Some(InputAction::FontZoomOut)
        ));
        assert!(matches!(
            press("_", ctrl_shift()),
            Some(InputAction::FontZoomOut)
        ));
        assert!(matches!(
            press("0", ctrl_shift()),
            Some(InputAction::FontZoomReset)
        ));
        // US layout reports Shift+0 as ")".
        assert!(matches!(
            press(")", ctrl_shift()),
            Some(InputAction::FontZoomReset)
        ));
        // Existing shortcuts are untouched.
        assert!(matches!(
            press("T", ctrl_shift()),
            Some(InputAction::NewTab)
        ));
        assert!(matches!(press("C", ctrl_shift()), Some(InputAction::Copy)));
        // Plain symbols are not chrome.
        assert!(press("+", ModifiersState::empty()).is_none());
    }

    /// Tab rename / unpin shortcuts.
    #[test]
    fn rename_shortcuts_map() {
        assert!(matches!(
            press("R", ctrl_shift()),
            Some(InputAction::RenameTab)
        ));
        assert!(matches!(
            press("U", ctrl_shift()),
            Some(InputAction::UnpinTab)
        ));
        // Lowercase (no shift) is not a chrome shortcut.
        assert!(press("r", ModifiersState::CONTROL).is_none());
    }
}
