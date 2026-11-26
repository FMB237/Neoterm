#!/usr/bin/env python3
"""
Auto-generate KEYBINDINGS.md from crates/neoterm/src/input.rs
Extracts chrome_action mappings and formats them into the markdown table.
"""
import re
from pathlib import Path

INPUT_RS = Path("crates/neoterm/src/input.rs")
OUTPUT_MD = Path("docs/KEYBINDINGS.md")

# Section definitions: (section_title, list of (action, description))
SECTIONS = [
    ("Tab & Window Management", [
        ("NewTab", "New tab"),
        ("ClosePane", "Close pane (last pane closes tab)"),
        ("NewWindow", "New window"),
        ("NextTab", "Next tab"),
        ("PrevTab", "Previous tab"),
        ("SelectTab", "Select tab number 1–9"),
    ]),
    ("Splits & Focus", [
        ("SplitV", "Split vertically (side-by-side)"),
        ("SplitH", "Split horizontally (stacked)"),
        ("FocusPane", "Move focus to neighboring pane"),
        ("ZoomPane", "Toggle pane zoom (fullscreen/restore)"),
    ]),
    ("Clipboard", [
        ("Copy", "Copy selection to system clipboard"),
        ("Paste", "Paste from system clipboard"),
    ]),
    ("Font Size (session-only)", [
        ("FontZoomIn", "Increase font size"),
        ("FontZoomOut", "Decrease font size"),
        ("FontZoomReset", "Reset font size to configured default"),
    ]),
    ("Tab Title", [
        ("RenameTab", "Rename active tab (Enter commits, Ctrl+U clears, Esc cancels)"),
        ("UnpinTab", "Release pinned title back to live OSC title"),
    ]),
    ("Search", [
        ("SearchToggle", "Open search overlay (type to filter, Enter next match, Shift+Enter previous, Esc closes)"),
    ]),
    ("Command Palette", [
        ("PaletteToggle", "Open fuzzy-searchable command palette (arrows navigate, Enter selects, Esc closes)"),
    ]),
    ("Copy Mode (Vi-style)", [
        ("CopymodeToggle", "Toggle copy mode"),
        ("CopymodeUp", "Move cursor up"),
        ("CopymodeDown", "Move cursor down"),
        ("CopymodeLeft", "Move cursor left"),
        ("CopymodeRight", "Move cursor right"),
        ("CopymodePageUp", "Page up"),
        ("CopymodePageDown", "Page down"),
        ("CopymodeHome", "Move to start"),
        ("CopymodeEnd", "Move to end"),
        ("CopymodeSelect", "Start character selection"),
        ("CopymodeSelectLine", "Start line selection"),
        ("CopymodeCopy", "Copy selection and exit"),
        ("CopymodeExit", "Exit without copying"),
    ]),
    ("Performance HUD", [
        ("TogglePerformanceHud", "Toggle performance HUD overlay (frame time, FPS, dirty panes)"),
    ]),
]

def parse_chrome_action(content):
    """Extract key bindings from chrome_action function."""
    bindings = {}
    
    match = re.search(r'pub fn chrome_action\(.*?\) -> Option<InputAction> \{', content)
    if not match:
        return bindings
    
    fn_start = match.end()
    brace_count = 1
    fn_end = fn_start
    for i, ch in enumerate(content[fn_start:]):
        if ch == '{':
            brace_count += 1
        elif ch == '}':
            brace_count -= 1
            if brace_count == 0:
                fn_end = fn_start + i + 1
                break
    
    fn_body = content[fn_start:fn_end]
    
    # First, find the Ctrl+Shift+Alt block
    alt_block_match = re.search(r'if ctrl && shift && mods\.alt_key\(\)\s*\{([^}]+)\}', fn_body, re.DOTALL)
    if alt_block_match:
        alt_block = alt_block_match.group(1)
        # Parse bindings in the Alt block
        alt_pattern = r'"([^"]+)"\s*=>\s*return Some\(InputAction::(\w+)'
        for match in re.finditer(alt_pattern, alt_block):
            key, action = match.groups()
            bindings[action] = f"Ctrl+Shift+Alt+{key}"
    
    # Then, find the main Ctrl+Shift block (excluding the Alt block)
    # Remove the alt block from fn_body to avoid double-matching
    fn_body_no_alt = re.sub(r'if ctrl && shift && mods\.alt_key\(\)\s*\{[^}]+\}', '', fn_body, flags=re.DOTALL)
    
    # Find the main Ctrl+Shift block
    ctrl_shift_block_match = re.search(r'if ctrl && shift\s*\{([^}]+)\}', fn_body_no_alt, re.DOTALL)
    if ctrl_shift_block_match:
        ctrl_shift_block = ctrl_shift_block_match.group(1)
        ctrl_shift_pattern = r'"([^"]+)"\s*=>\s*return Some\(InputAction::(\w+)'
        for match in re.finditer(ctrl_shift_pattern, ctrl_shift_block):
            key, action = match.groups()
            key_map = {
                "=": "+", "-": "-", "_": "-", "0": "0", ")": "0",
                "+": "+", " ": "Space",
            }
            display_key = key_map.get(key, key)
            if display_key == "Space":
                bindings[action] = "Ctrl+Shift+Space"
            else:
                bindings[action] = f"Ctrl+Shift+{display_key}"
    
    # Ctrl+1..9 for SelectTab
    bindings["SelectTab"] = "Ctrl+1 … Ctrl+9"
    
    # Ctrl+Tab / Ctrl+Shift+Tab for NextTab/PrevTab
    bindings["NextTab"] = "Ctrl+Tab"
    bindings["PrevTab"] = "Ctrl+Shift+Tab"
    
    # Ctrl+P for previous tab
    bindings["CtrlP"] = "Ctrl+P"
    
    # Alt+arrows
    bindings["FocusPane"] = "Alt+↑ / ↓ / ← / →"
    
    return bindings

def format_keybinding(action, bindings):
    """Format a keybinding for the markdown table."""
    if action in bindings:
        return f"`{bindings[action]}`"
    
    # Explicit fallbacks matching current input.rs
    fallbacks = {
        "NewTab": "`Ctrl+Shift+T`",
        "ClosePane": "`Ctrl+Shift+W`",
        "NewWindow": "`Ctrl+Shift+N`",
        "NextTab": "`Ctrl+Tab`",
        "PrevTab": "`Ctrl+Shift+Tab`",
        "SelectTab": "`Ctrl+1` … `Ctrl+9`",
        "Copy": "`Ctrl+Shift+C`",
        "Paste": "`Ctrl+Shift+V`",
        "SplitV": "`Ctrl+Shift+E`",
        "SplitH": "`Ctrl+Shift+O`",
        "FocusPane": "`Alt+↑/↓/←/→`",
        "ZoomPane": "`Ctrl+Shift+Z`",
        "FontZoomIn": "`Ctrl+Shift++` / `=`",
        "FontZoomOut": "`Ctrl+Shift+-` / `_`",
        "FontZoomReset": "`Ctrl+Shift+0` / `)`",
        "RenameTab": "`Ctrl+Shift+R`",
        "UnpinTab": "`Ctrl+Shift+U`",
        "BroadcastToggle": "`Ctrl+Shift+B`",
        "SearchToggle": "`Ctrl+Shift+F`",
        "PaletteToggle": "`Ctrl+Shift+P`",
        "CopymodeToggle": "`Ctrl+Shift+Space`",
        "CopymodeUp": "`↑` / `K`",
        "CopymodeDown": "`↓` / `J`",
        "CopymodeLeft": "`←` / `H`",
        "CopymodeRight": "`→` / `L`",
        "CopymodePageUp": "`PageUp`",
        "CopymodePageDown": "`PageDown`",
        "CopymodeHome": "`Home`",
        "CopymodeEnd": "`End`",
        "CopymodeSelect": "`v`",
        "CopymodeSelectLine": "`V`",
        "CopymodeCopy": "`y`",
        "CopymodeExit": "`Esc` / `q` / `Ctrl+C`",
        "TogglePerformanceHud": "`Ctrl+Shift+Alt+F`",
    }
    return fallbacks.get(action, "")

def generate_markdown(bindings):
    """Generate the full KEYBINDINGS.md content."""
    lines = [
        "# NeoTerm Keybindings",
        "",
        "Auto-generated from `crates/neoterm/src/input.rs`.",
        "",
    ]
    
    for section_title, actions in SECTIONS:
        lines.append(f"## {section_title}")
        lines.append("")
        lines.append("| Keys | Action |")
        lines.append("|------|--------|")
        
        for action, desc in actions:
            key = format_keybinding(action, bindings)
            if key:
                lines.append(f"| {key} | {desc} |")
        
        lines.append("")
    
    return "\n".join(lines)

def main():
    content = INPUT_RS.read_text()
    bindings = parse_chrome_action(content)
    markdown = generate_markdown(bindings)
    OUTPUT_MD.write_text(markdown + "\n")
    print(f"Generated {OUTPUT_MD}")

if __name__ == "__main__":
    main()
