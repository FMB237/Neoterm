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
    /// Toggle performance HUD overlay (Ctrl+Shift+Alt+F).
    TogglePerformanceHud,
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
///
/// `copymode` must be true only while Vi copy mode owns the keyboard. It
/// changes what `v`/`V`/`y` and the navigation keys mean; with it false they
/// are ordinary terminal input (this was the bug where typing `v` or `y`
/// produced nothing).
pub fn map_key(
    event: &KeyEvent,
    mods: &Modifiers,
    alt_screen: bool,
    copymode: bool,
) -> InputAction {
    if event.state != ElementState::Pressed {
        return InputAction::Ignored;
    }

    map_key_parts(
        &event.logical_key,
        event.text.as_deref(),
        mods.state(),
        alt_screen,
        copymode,
    )
}

/// Pure key mapping, split out of [`map_key`] so it can be tested without
/// constructing a winit `KeyEvent`.
fn map_key_parts(
    key: &Key,
    text: Option<&str>,
    mods: ModifiersState,
    alt_screen: bool,
    copymode: bool,
) -> InputAction {
    // Window/app chrome shortcuts first (never reach the PTY).
    if let Some(action) = chrome_action(key, mods) {
        return action;
    }

    let ctrl = mods.control_key();
    let shift = mods.shift_key();

    // Special keys.
    match key {
        Key::Named(NamedKey::Enter) => return InputAction::PtyInput(vec![b'\r']),
        Key::Named(NamedKey::Backspace) => return InputAction::PtyInput(vec![0x7f]),
        Key::Named(NamedKey::Tab) => return InputAction::PtyInput(vec![b'\t']),
        Key::Named(NamedKey::Escape) => return InputAction::PtyInput(vec![0x1b]),
        Key::Named(NamedKey::ArrowUp) => {
            if copymode && !alt_screen {
                if let Some(action) = copymode_key(key) {
                    return action;
                }
            }
            return InputAction::PtyInput(b"\x1b[A".to_vec());
        }
        Key::Named(NamedKey::ArrowDown) => {
            if copymode && !alt_screen {
                if let Some(action) = copymode_key(key) {
                    return action;
                }
            }
            return InputAction::PtyInput(b"\x1b[B".to_vec());
        }
        Key::Named(NamedKey::ArrowRight) => {
            if copymode && !alt_screen {
                if let Some(action) = copymode_key(key) {
                    return action;
                }
            }
            return InputAction::PtyInput(b"\x1b[C".to_vec());
        }
        Key::Named(NamedKey::ArrowLeft) => {
            if copymode && !alt_screen {
                if let Some(action) = copymode_key(key) {
                    return action;
                }
            }
            return InputAction::PtyInput(b"\x1b[D".to_vec());
        }
        Key::Named(NamedKey::Home) => {
            if copymode && !alt_screen {
                if let Some(action) = copymode_key(key) {
                    return action;
                }
            }
            return InputAction::PtyInput(b"\x1b[H".to_vec());
        }
        Key::Named(NamedKey::End) => {
            if copymode && !alt_screen {
                if let Some(action) = copymode_key(key) {
                    return action;
                }
            }
            return InputAction::PtyInput(b"\x1b[F".to_vec());
        }
        Key::Named(NamedKey::PageUp) => {
            if alt_screen && !shift {
                return InputAction::PtyInput(b"\x1b[5~".to_vec());
            }
            if copymode && !alt_screen {
                if let Some(action) = copymode_key(key) {
                    return action;
                }
            }
            return InputAction::ScrollPageUp;
        }
        Key::Named(NamedKey::PageDown) => {
            if alt_screen && !shift {
                return InputAction::PtyInput(b"\x1b[6~".to_vec());
            }
            if copymode && !alt_screen {
                if let Some(action) = copymode_key(key) {
                    return action;
                }
            }
            return InputAction::ScrollPageDown;
        }
        Key::Named(NamedKey::Delete) => return InputAction::PtyInput(b"\x1b[3~".to_vec()),
        Key::Character(s) if copymode && !alt_screen && (s == "v" || s == "V" || s == "y") => {
            if let Some(action) = copymode_key(key) {
                return action;
            }
        }
        _ => {}
    }

    // Ctrl+letter -> control codes (Ctrl+C, Ctrl+D, ...).
    if ctrl {
        if let Key::Character(s) = key {
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
    if let Some(text) = text {
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

    // Ctrl+Shift+Alt+F toggles the performance HUD.
    if ctrl && shift && mods.alt_key() {
        match key {
            Key::Character(s) => match s.to_uppercase().as_str() {
                "F" => return Some(InputAction::TogglePerformanceHud),
                _ => {}
            },
            _ => {}
        }
    }

    if ctrl && shift {
        match key {
            Key::Character(s) => match s.to_uppercase().as_str() {
                "T" => return Some(InputAction::NewTab),
                "W" => return Some(InputAction::ClosePane),
                "N" => return Some(InputAction::NewWindow),
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
                // Ctrl+P goes to previous tab (Ctrl+Shift+P is palette).
                if s.eq_ignore_ascii_case("p") {
                    return Some(InputAction::PrevTab);
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

    /// Ctrl+Shift+P opens the command palette (not PrevTab).
    #[test]
    fn palette_toggle_shortcut_maps() {
        assert!(matches!(
            press("P", ctrl_shift()),
            Some(InputAction::PaletteToggle)
        ));
        // Ctrl+P (without shift) goes to previous tab.
        assert!(matches!(
            press("p", ModifiersState::CONTROL),
            Some(InputAction::PrevTab)
        ));
        // Ctrl+Shift+P no longer maps to PrevTab.
        assert!(!matches!(
            press("P", ctrl_shift()),
            Some(InputAction::PrevTab)
        ));
    }

    fn map_key_for(key: Key, text: Option<&str>, copymode: bool) -> InputAction {
        map_key_parts(
            &key,
            text,
            ModifiersState::empty(),
            false,
            copymode,
        )
    }

    /// Regression: plain `v`/`V`/`y` were mapped to copymode actions even
    /// when copymode was off, so they never reached the PTY.
    #[test]
    fn plain_v_and_y_reach_the_pty_when_copymode_is_off() {
        for ch in ["v", "V", "y"] {
            match map_key_for(Key::Character(ch.into()), Some(ch), false) {
                InputAction::PtyInput(bytes) => assert_eq!(bytes, ch.as_bytes()),
                _ => panic!("{ch:?} was swallowed instead of sent to the PTY"),
            }
        }
    }

    #[test]
    fn copymode_owns_v_and_y_only_when_active() {
        assert!(matches!(
            map_key_for(Key::Character("v".into()), Some("v"), true),
            InputAction::CopymodeSelect
        ));
        assert!(matches!(
            map_key_for(Key::Character("V".into()), Some("V"), true),
            InputAction::CopymodeSelectLine
        ));
        assert!(matches!(
            map_key_for(Key::Character("y".into()), Some("y"), true),
            InputAction::CopymodeCopy
        ));
    }

    /// Regression: arrows/Home/End were also hijacked by copymode, so the
    /// shell never saw cursor movement.
    #[test]
    fn navigation_keys_reach_the_pty_when_copymode_is_off() {
        for (key, bytes) in [
            (NamedKey::ArrowUp, b"\x1b[A".as_slice()),
            (NamedKey::ArrowDown, b"\x1b[B".as_slice()),
            (NamedKey::ArrowLeft, b"\x1b[D".as_slice()),
            (NamedKey::ArrowRight, b"\x1b[C".as_slice()),
            (NamedKey::Home, b"\x1b[H".as_slice()),
            (NamedKey::End, b"\x1b[F".as_slice()),
        ] {
            match map_key_for(Key::Named(key), None, false) {
                InputAction::PtyInput(got) => assert_eq!(got, bytes.to_vec(), "{key:?}"),
                _ => panic!("{key:?} was swallowed instead of sent to the PTY"),
            }
        }
    }

    #[test]
    fn navigation_keys_move_the_copymode_cursor_when_active() {
        assert!(matches!(
            map_key_for(Key::Named(NamedKey::ArrowUp), None, true),
            InputAction::CopymodeUp
        ));
        assert!(matches!(
            map_key_for(Key::Named(NamedKey::PageDown), None, true),
            InputAction::CopymodePageDown
        ));
    }

    #[test]
    fn page_up_scrolls_locally_but_reaches_alt_screen_apps() {
        assert!(matches!(
            map_key_for(Key::Named(NamedKey::PageUp), None, false),
            InputAction::ScrollPageUp
        ));
        let in_app = map_key_parts(
            &Key::Named(NamedKey::PageUp),
            None,
            ModifiersState::empty(),
            true,
            false,
        );
        assert!(matches!(in_app, InputAction::PtyInput(_)));
    }

    /// Ctrl+Shift+Alt+F toggles the performance HUD.
    #[test]
    fn hud_toggle_shortcut_maps() {
        let ctrl_shift_alt = ModifiersState::CONTROL | ModifiersState::SHIFT | ModifiersState::ALT;
        assert!(matches!(
            press("f", ctrl_shift_alt),
            Some(InputAction::TogglePerformanceHud)
        ));
        assert!(matches!(
            press("F", ctrl_shift_alt),
            Some(InputAction::TogglePerformanceHud)
        ));
    }

    /// Ctrl+Shift+F (no Alt) still opens search, not the HUD.
    #[test]
    fn hud_needs_alt_modifier() {
        assert!(matches!(
            press("f", ctrl_shift()),
            Some(InputAction::SearchToggle)
        ));
        assert!(!matches!(
            press("f", ctrl_shift()),
            Some(InputAction::TogglePerformanceHud)
        ));
        // Ctrl+Alt+F without shift is not a chrome shortcut.
        let ctrl_alt = ModifiersState::CONTROL | ModifiersState::ALT;
        assert!(press("f", ctrl_alt).is_none());
    }
}
