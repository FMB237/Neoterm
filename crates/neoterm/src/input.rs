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
        Key::Named(NamedKey::ArrowUp) => return InputAction::PtyInput(b"\x1b[A".to_vec()),
        Key::Named(NamedKey::ArrowDown) => return InputAction::PtyInput(b"\x1b[B".to_vec()),
        Key::Named(NamedKey::ArrowRight) => return InputAction::PtyInput(b"\x1b[C".to_vec()),
        Key::Named(NamedKey::ArrowLeft) => return InputAction::PtyInput(b"\x1b[D".to_vec()),
        Key::Named(NamedKey::Home) => return InputAction::PtyInput(b"\x1b[H".to_vec()),
        Key::Named(NamedKey::End) => return InputAction::PtyInput(b"\x1b[F".to_vec()),
        Key::Named(NamedKey::PageUp) => {
            if alt_screen && !shift {
                return InputAction::PtyInput(b"\x1b[5~".to_vec());
            }
            return InputAction::ScrollPageUp;
        }
        Key::Named(NamedKey::PageDown) => {
            if alt_screen && !shift {
                return InputAction::PtyInput(b"\x1b[6~".to_vec());
            }
            return InputAction::ScrollPageDown;
        }
        Key::Named(NamedKey::Delete) => return InputAction::PtyInput(b"\x1b[3~".to_vec()),
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
fn chrome_action(key: &Key, mods: ModifiersState) -> Option<InputAction> {
    let ctrl = mods.control_key();
    let shift = mods.shift_key();

    if ctrl && shift {
        match key {
            Key::Character(s) => match s.to_uppercase().as_str() {
                "T" => return Some(InputAction::NewTab),
                "W" => return Some(InputAction::ClosePane),
                "N" => return Some(InputAction::NextTab),
                "P" => return Some(InputAction::PrevTab),
                "C" => return Some(InputAction::Copy),
                "V" => return Some(InputAction::Paste),
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
