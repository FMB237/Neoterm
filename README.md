# NeoTerm

**A GPU terminal emulator written in pure Rust.** Tabs, splits, themes —
no GUI toolkit, no C dependencies beyond the GPU stack.

```
winit 0.30 · wgpu 30 · glyphon 0.12 + cosmic-text 0.19
alacritty_terminal 0.26 · portable-pty 0.9 · arboard 3
```

~7,000 lines of Rust across 8 modules, 41 headless unit tests.

---

## Features

- **Tabs** — click to switch, `×` / middle-click to close, `+` for a new
  tab, auto-hide with a single tab, live titles from OSC 0/2 (tabs named by
  the running app: `vim`, `htop`, …), manual titles with `Ctrl+Shift+R`.
- **Splits** — binary split tree per tab, 1-cell dividers (zero text drift),
  drag-resize with minimum clamps, fullscreen zoom of one pane,
  click / `Alt+arrow` focus, per-pane PTY, scrollback, selection and palette.
- **Mouse** — drag-select (char / word / line via click counting), SGR 1006
  with pane-local coordinates, a scrollbar inside the focused pane, hover
  cursors on dividers.
- **Clipboard** — X11 tradition: drag-select auto-copies to PRIMARY,
  middle-click pastes (bracketed-paste safe), `Ctrl+Shift+C/V` for the
  system clipboard.
- **Themes** — `tokyo-night` (default), `catppuccin-mocha`, `dracula`,
  `gruvbox-dark` or a fully custom palette; 16-color ANSI override (app OSC
  sequences still win); per-cell background rects; a seamless Ghostty-style
  tab strip with an accent underline; live reload on config save.
- **Typography** — any installed font (`font_family` takes a comma-separated
  preference list), true sRGB color management, runtime font zoom.
- **Search** — `Ctrl+Shift+F` searches the focused pane's scrollback *and*
  viewport, highlights every match in place, and jumps between them.
- **Links** — OSC 8 hyperlinks render underlined and open on `Ctrl+click`.
- **Cursor** — block, underline, beam and hollow block, honoring `DECSCUSR`,
  drawn so the character underneath stays readable.
- **Everyday** — `--working-directory` to pick where the shell starts, a
  visual bell, and a terminal that only redraws when something changed.

## Install

```bash
./install.sh              # → ~/.local/bin/neoterm
./install.sh --system     # → /usr/local (needs sudo)
./uninstall.sh            # removes the binary, desktop entry and icon
```

`PREFIX` and `DESTDIR` are honored for packaging.

## Usage

```bash
neoterm                          # open a terminal
neoterm -w ~/projects/app        # start the shell in a directory
neoterm -e htop                  # run a command instead of the shell
```

## Keybindings

| Keys | Action |
|---|---|
| `Ctrl+Shift+T` | New tab |
| `Ctrl+Shift+E` / `Ctrl+Shift+O` | Split side-by-side / stacked |
| `Ctrl+Shift+W` | Close pane (last pane closes the tab) |
| `Ctrl+Shift+Z` | Zoom the focused pane fullscreen |
| `Alt+←/→/↑/↓` | Move pane focus |
| `Ctrl+Shift+N` / `P`, `Ctrl+Tab`, `Ctrl+1..9` | Next / previous / numbered tab |
| `Ctrl+Shift+C` / `V` | Copy / paste (never reach the PTY) |
| `Ctrl+Shift+Plus` / `Minus` / `0` | Font size up / down / reset |
| `Ctrl+Shift+R` / `U` | Rename the active tab / release it to the live title |
| `Ctrl+Shift+F` | Search scrollback (`Enter` next, `Shift+Enter` previous, `Esc` closes) |
| `Ctrl+click` | Open a hyperlink |
| `PageUp/PageDown` (`Shift` forces local) | Scrollback |
| Double / triple click | Word / line select |
| Middle-click | Paste PRIMARY · close tab on the strip · SGR button-1 in mouse apps |
| Drag divider / scrollbar thumb | Resize panes / scroll history |

## Configuration

`~/.config/neoterm/config.toml` — hot-reloaded on save:

```toml
font_family = "JetBrains Mono, Fira Code, monospace"  # first installed wins
font_size = 15.0
cols = 80
rows = 24
scrollback_lines = 10000
shell = "/bin/bash"
tab_bar = "auto"          # auto | always | never
bell = "visual"           # visual | none

[theme]
preset = "tokyo-night"    # tokyo-night | catppuccin-mocha | dracula |
                          # gruvbox-dark | "" for fully custom colors
# background, foreground, cursor, selection, palette (16 ANSI colors),
# tab_bar_bg, tab_active_fg, tab_inactive_fg, tab_close_fg, tab_index_fg,
# tab_bar_border, tab_active_bg (toast surface),
# scrollbar_track, scrollbar_thumb
```

## Building from source

```bash
cargo test             # 41 headless tests
cargo build --release  # optimized (LTO)
```

Rust 2021. No system libraries beyond the GPU stack (Vulkan or GL).

## Design

The interface follows one rule: **quiet chrome**. Chrome appears when it is
relevant and fades when it is not.

- [`docs/mockups/NeoTerm UI Pass.html`](docs/mockups/NeoTerm%20UI%20Pass.html)
  — NeoTerm before and after the v0.3.0 UI pass: a tab strip that reads as
  titles instead of debug output, hover states, a focus indicator for splits,
  copy/paste feedback, and a cursor that no longer hides the character under
  it. Both panels are drawn from the same tokens and cell metrics; open it in
  a browser and hover the tabs.
- [`docs/ui-pass.md`](docs/ui-pass.md) — the implementation plan behind that
  mockup: exact tokens, the files involved, and the acceptance criteria.

## Architecture
```
winit events ─▶ input::map_key ─▶ InputAction ─▶ App (main.rs)
                                                      │
              ┌───────────────────────────────────────┼────────────────────┐
              ▼                                       ▼                    ▼
        Tabs (tabs.rs)                    Renderer (renderer.rs)      mouse.rs
        tab list, active, pane ids        wgpu 2-pass: bg quads       hit-test math
              │                           + glyphon text              (single source
              ▼                            │                          of truth)
        Tab (tab.rs): split tree (PaneNode) + Pane leaves
              │  layout() → grid-cell frames + 1-cell dividers
              ▼
        Pane: Term<TitleListener> + PTY master/writer/child
              │  1 reader thread each: PTY → vte → Term
              ▼  (dirty flag + EventLoopProxy wakeup, coalesced)
        rendered_rows() → RowContent { text, spans, sel_cols, bg_rects }
```

Design rules that must not break:

1. Dividers are always exactly **1 grid cell** — no pixel rounding in the
   split path; selection, SGR and wide-char math stay exact.
2. Coordinates handed to apps (SGR) and selections are **pane-local**.
3. Mouse press/release form a **capture pair** bound to the press pane.
4. `resize_body` only resizes panes whose grid size actually changed —
   divider drags must not SIGWINCH-storm.
5. Renderer caches are keyed by pane id and pruned every frame.
6. OSC titles wake the loop through the shared dirty flag (no busy-loop).
7. Reaping never respawns; the app quits only when the tab list empties.
8. Geometry has a **single source of truth** (`Renderer::body_geometry` /
   `grid_size`); the mouse helpers take the same advance and insets.
9. All colors handed to the swapchain are **linear** (the target is sRGB).

See [`Neoterm.md`](Neoterm.md) for the full project document and
[`PLAN.md`](PLAN.md) for the roadmap and the continuation brief.

## Status

v0.3.0 — tabs, splits, themes, mouse, clipboard, font zoom and live config
reload, plus the global UI pass, scrollback search, OSC 8 hyperlinks and
`DECSCUSR` cursor styles. Next up: broadcast input and tab reordering, then
the big rocks (session restore, multiple OS windows).

## Development history

NeoTerm was developed before this repository existed, so the earliest commits
were reconstructed from the development log when the project was first
published: each commit is a real, compiling, test-passing state, dated to
when the feature actually landed. From the "polish" commit on, the history is
the original work, commit for commit.

## License

MIT — see [`LICENSE`](LICENSE). Use it, fork it, ship it; keep the copyright
notice.
