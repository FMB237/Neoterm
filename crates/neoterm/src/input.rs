use winit::event::{ElementState, KeyEvent, Modifiers};
use winit::keyboard::{Key, NamedKey};

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
/// Ctrl+Shift+T/W/N/P/C/V handled here; everything else becomes PTY bytes.
/// `alt_screen` routes PageUp/PageDown: apps on the alternate screen
/// (vim, less) get the key, otherwise we scroll locally — unless Shift is
/// held, which always scrolls locally.
pub fn map_key(event: &KeyEvent, mods: &Modifiers, alt_screen: bool) -> InputAction {
    if event.state != ElementState::Pressed {
        return InputAction::Ignored;
    }

    let ctrl = mods.state().control_key();
    let shift = mods.state().shift_key();

    // Tab management + clipboard shortcuts.
    // NOTE: Ctrl+Shift+C/V never reach the PTY (unlike Ctrl+C = SIGINT).
    // This matches GNOME Terminal / Alacritty behavior.
    if ctrl && shift {
        match &event.logical_key {
            Key::Character(s) => match s.to_uppercase().as_str() {
                "T" => return InputAction::NewTab,
                "W" => return InputAction::ClosePane,
                "N" => return InputAction::NextTab,
                "P" => return InputAction::PrevTab,
                "C" => return InputAction::Copy,
                "V" => return InputAction::Paste,
                "E" => return InputAction::SplitV,
                "O" => return InputAction::SplitH,
                "Z" => return InputAction::ZoomPane,
                _ => {}
            },
            Key::Named(NamedKey::Tab) => return InputAction::NextTab,
            _ => {}
        }
    }
    if ctrl && !shift {
        match &event.logical_key {
            Key::Named(NamedKey::Tab) => return InputAction::NextTab,
            Key::Character(s) => {
                // Alt-style: Ctrl+1..9 switches tabs.
                if let Ok(n) = s.parse::<usize>() {
                    if (1..=9).contains(&n) {
                        return InputAction::SelectTab(n - 1);
                    }
                }
            }
            _ => {}
        }
    }

    // Alt+arrows move pane focus (stolen from apps: multiplexers own it,
    // same as tmux prefix+arrows). Plain/shifted arrows still reach the PTY.
    if mods.state().alt_key() {
        match &event.logical_key {
            Key::Named(NamedKey::ArrowUp) => return InputAction::FocusPane(FocusDir::Up),
            Key::Named(NamedKey::ArrowDown) => return InputAction::FocusPane(FocusDir::Down),
            Key::Named(NamedKey::ArrowRight) => return InputAction::FocusPane(FocusDir::Right),
            Key::Named(NamedKey::ArrowLeft) => return InputAction::FocusPane(FocusDir::Left),
            _ => {}
        }
    }

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
