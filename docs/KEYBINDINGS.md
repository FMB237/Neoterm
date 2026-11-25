# NeoTerm Keybindings

Auto-generated from `crates/neoterm/src/input.rs`.

## Tab & Window Management

| Keys | Action |
|------|--------|
| `Ctrl+Shift+T` | New tab |
| `Ctrl+Shift+W` | Close pane (last pane closes tab) |
| `Ctrl+Shift+N` | New window |
| `Ctrl+Tab` / `Ctrl+Shift+Tab` | Next / previous tab |
| `Ctrl+1` … `Ctrl+9` | Select tab number 1–9 |

## Splits & Focus

| Keys | Action |
|------|--------|
| `Ctrl+Shift+E` | Split vertically (side-by-side) |
| `Ctrl+Shift+O` | Split horizontally (stacked) |
| `Alt+Up` / `Down` / `Left` / `Right` | Move focus to neighboring pane |
| `Ctrl+Shift+Z` | Toggle pane zoom (fullscreen/restore) |

## Clipboard

| Keys | Action |
|------|--------|
| `Ctrl+Shift+C` | Copy selection to system clipboard |
| `Ctrl+Shift+V` | Paste from system clipboard |
| (Middle-click) | Paste PRIMARY selection (X11) |

## Font Size (session-only)

| Keys | Action |
|------|--------|
| `Ctrl+Shift++` / `=` | Increase font size |
| `Ctrl+Shift+-` / `_` | Decrease font size |
| `Ctrl+Shift+0` / `)` | Reset font size to configured default |

## Tab Title

| Keys | Action |
|------|--------|
| `Ctrl+Shift+R` | Rename active tab (Enter commits, `Ctrl+U` clears, Esc cancels) |
| `Ctrl+Shift+U` | Release pinned title back to live OSC title |

## Search

| Keys | Action |
|------|--------|
| `Ctrl+Shift+F` | Open search overlay (type to filter, `Enter` next match, `Shift+Enter` previous, `Esc` closes) |

## Command Palette

| Keys | Action |
|------|--------|
| `Ctrl+Shift+P` | Open fuzzy-searchable command palette (arrows navigate, `Enter` selects, `Esc` closes) |

## Copy Mode (Vi-style)

| Keys | Action |
|------|--------|
| `Ctrl+Shift+Space` | Toggle copy mode |
| Arrow keys / `H`/`J`/`K`/`L` | Move cursor |
| `v` / `V` | Start character / line selection |
| `y` | Copy selection and exit |
| `Esc` / `q` / `Ctrl+C` | Exit without copying |

## Performance HUD

| Keys | Action |
|------|--------|
| `Ctrl+Shift+Alt+F` | Toggle performance HUD overlay (frame time, FPS, dirty panes) |
