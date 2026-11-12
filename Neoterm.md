# NeoTerm

GPU terminal emulator in pure Rust. Tabs, splits, themes — no toolkit, no C
dependencies beyond the GPU stack. Currently at **v0.2.0**, installed via
`./install.sh` into `~/.local`.

> `PLAN.md` — continuation brief: what is shipped, the invariants that must
> not break, and the prioritized backlog (next up: `--working-directory`, bell
> support, manual tab rename).

| Layer | Crate |
|---|---|
| Windowing / input | winit 0.30 |
| GPU rendering | wgpu 30 |
| Text shaping | glyphon 0.12 + cosmic-text 0.19 |
| Terminal emulation (grid, vte, scrollback) | alacritty_terminal 0.26 |
| PTY + shell spawning | portable-pty 0.9 (+ vte 0.15 processor) |
| Clipboard (X11 PRIMARY + clipboard) | arboard 3 |
| Config file | serde 1 + toml 0.8, watched with notify 6 |
| CLI (`-e`, `--version`) | clap 4 |

~5,860 lines across 7 modules, 31 headless unit tests (`cargo test`).

## Milestones shipped

- **Core terminal** — PTY per shell, alacritty grid + scrollback, damage-gated
  rendering (frames present only when PTY output, damage, or UI state
  changed; the cursor toggles twice per second without waking the loop
  otherwise).
- **Tabs** — click to switch, `×` mark / middle-click to close, `+` button for
  new tabs, auto-hide with a single tab (`tab_bar = auto/always/never`),
  live titles from OSC 0/2 (tabs named by the running app, e.g. `vim`,
  `htop`), `exit` in the last tab quits the app.
- **Splits** — binary split tree per tab, 1-cell dividers (zero text drift),
  drag-resize with livable-minimum clamps, zoom toggle, click/Alt+arrow
  focus, per-pane PTY/scrollback/selection/palette. `exit` reaps just the
  pane; tree surgery collapses the sibling subtree into place.
- **Mouse** — drag-select (char/word/line via click counting), SGR (1006)
  reports to fullscreen apps with pane-local coordinates, scrollbar living
  inside the focused pane, hover cursors over dividers.
- **Clipboard** — X11 tradition: drag-select auto-copies to PRIMARY,
  middle-click pastes (bracketed-paste safe), `Ctrl+Shift+C/V` for the
  system clipboard.
- **Themes** — `tokyo-night` (default), `catppuccin-mocha`, `dracula`,
  `gruvbox-dark`, or fully custom `[theme]`; 16-color ANSI palette override
  (app OSC sequences still win), per-cell background rects, Ghostty-style
  seamless tab strip, live reload on config save — no restart.
- **Polish** — true sRGB color management (themes render as authored, not
  washed out), symmetric body padding, active-tab accent underline,
  alpha-blended selection/dividers/scrollbar, runtime font zoom
  (session-only), applied `font_family` with a comma-separated fallback
  list, and a config watcher that survives atomic saves.
- **Everyday features** — `--working-directory` to pick the shell's start
  directory, a visual bell (`BEL` tints the body; `bell = "none"` mutes it),
  and manual tab titles (`Ctrl+Shift+R` edits, `Ctrl+Shift+U` releases the
  tab back to its live OSC title).
- **Ship** — versioned binary, `assets/neoterm.desktop` (validated),
  hand-made SVG icon, `install.sh` / `uninstall.sh` (user `~/.local` by
  default, `--system` for `/usr/local`, `PREFIX`/`DESTDIR` honored).

## Keybindings

| Keys | Action |
|---|---|
| `Ctrl+Shift+T` | New tab |
| `Ctrl+Shift+E` / `Ctrl+Shift+O` | Split side-by-side / stacked (Terminator convention) |
| `Ctrl+Shift+W` | Close pane (last pane closes the tab) |
| `Ctrl+Shift+Z` | Zoom focused pane fullscreen |
| `Alt+←/→/↑/↓` | Move pane focus (stolen from apps, like tmux prefix+arrows) |
| `Ctrl+Shift+N` / `P`, `Ctrl+Tab`, `Ctrl+1..9` | Next / previous / numbered tab |
| `Ctrl+Shift+C` / `V` | Copy / paste (never reach the PTY) |
| `Ctrl+Shift+Plus` / `Minus` / `0` | Font size up / down / reset (session-only) |
| `Ctrl+Shift+R` / `U` | Rename the active tab (Enter commits, Esc cancels) / release it back to the live title |
| `PageUp/PageDown` (`Shift` forces local) | Scrollback, unless a fullscreen app owns the keys |
| Double / triple click | Word / line select (400 ms window) |
| Middle-click | Paste PRIMARY · close tab on the strip · SGR button-1 in mouse apps |
| Drag divider / scrollbar thumb | Resize panes / scroll history |

## Configuration (`~/.config/neoterm/config.toml`)

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
# tab_bar_bg (unset = seamless, follows background), tab_active_fg,
# tab_inactive_fg, tab_close_fg, scrollbar_track, scrollbar_thumb
```

Notes: `font_family` accepts a comma-separated preference list and uses the
first installed family (unknown names warn and fall back to monospace);
`font_size` is the startup size while `Ctrl+Shift+±/0` zooms for the session
(live config reloads keep an active zoom); `bell` controls what `BEL` does
(`visual` tints the body for ~180 ms, `none` ignores it); a named `preset`
fills the whole theme; old two-field theme files keep loading and inherit
the pack.

The shell's start directory comes from the command line:
`neoterm --working-directory ~/projects/app` (default: `$HOME`). It is a
launch-time choice — a config reload never moves a running shell, and every
new tab or split inherits the same directory.

## Architecture

```
winit events ──▶ input::map_key ──▶ InputAction ──▶ App (main.rs)
                                                      │
                              ┌───────────────────────┼───────────────────────┐
                              ▼                       ▼                       ▼
                         Tabs (tabs.rs)          Renderer (renderer.rs)   mouse.rs (pure
                         tab list, active,       wgpu 2-pass: bg quads    hit-test math:
                         pane ids                + glyphon text           single source
                              │                                          of truth)
                              ▼
                         Tab (tab.rs): split tree (PaneNode) + Pane leaves
                              │  layout() → grid-cell frames + 1-cell dividers
                              ▼
                         Pane: Term<TitleListener> + PTY master/writer/child
                              │  1 reader thread each: PTY → vte → Term
                              ▼  (dirty flag + EventLoopProxy wakeup, coalesced)
                         rendered_rows() → RowContent { text, spans, sel_cols, bg_rects }
```

**Module map** (`crates/neoterm/src/`, lines approx):

- `main.rs` (~1570) — `App` event loop, all mouse/keyboard handlers,
  draw orchestration, config watcher, strip-visibility reflow, font zoom
  and grid reflow, visual bell, tab rename mode.
- `tab.rs` (~1840) — `Pane` (one terminal: PTY, grid, selection, OSC title
  listener), `PaneNode` split tree, pure cell layout (`layout_tree`,
  `focus_neighbor`, `clamp_ratio`), `Tab` container delegating single-pane
  behavior to the focused pane, free `render_term_rows` for headless tests.
- `renderer.rs` (~1010) — wgpu setup, per-pane text-buffer caches keyed by
  pane id (pruned on close), font-family/glyph-advance probing, pass 1:
  alpha-blended solid rects (strip, accent underline, dividers, cell
  backgrounds, selection, scrollbar) in linear color, pass 2: shaped text
  clipped per pane.
- `tabs.rs` (~240) — tab list, ids, split/close/focus/zoom dispatch,
  dead-pane reaping (never respawns; empty list quits the app), title pins.
- `mouse.rs` (~440) — pure geometry: labels, regions, close/`+` zones,
  scrollbar math, SGR encoding. Clicks always land on what is drawn.
- `input.rs` (~250) — key event → `InputAction` mapping (`chrome_action`
  holds the pure shortcut table).
- `config.rs` (~510) — `NeotermConfig` + `Theme` pack with legacy-tolerant
  serde defaults, preset application, and sRGB→linear conversion for the
  swapchain.

**Load-bearing invariants** (do not break these):

1. Dividers are always exactly 1 grid cell — no pixel rounding anywhere in
   the split path; selection, SGR, and wide-char math stay exact.
2. All pane coordinates handed to apps (SGR) and selections are pane-local;
   body→pane mapping happens once per event in `pane_cell_at`.
3. Mouse press/release form a capture pair bound to the press pane
   (`press_pane`/`press_local`); a closed mid-press pane degrades to no-op.
4. `resize_body` only resizes panes whose grid size changed — divider drags
   must not SIGWINCH-storm.
5. Renderer caches are keyed by pane id and pruned every frame; stale ids
   never survive a close.
6. `TitleListener` wakes the loop through the shared dirty flag with the
   same coalescing as the reader thread; OSC titles neverbusy-loop.
7. Reaping never respawns (buttons do); the app quits only when the tab
   list itself empties.

## Not yet built (roadmap, audited)

**Quick wins** — ~~`--working-directory` CLI flag~~, ~~bell support~~ and
~~manual tab rename~~ are done (see *Everyday features* above). Still open:
ANSI underline (`SGR 4` is parsed but not drawn), `--working-directory` for
*new* tabs (today every tab starts in the launch directory), a bell sound,
and a themed scrollbar that follows the accent color.

**Medium** — scrollback search (`Ctrl+Shift+F`, match highlights via the
existing bg-rect pass); hyperlink support (OSC 8, underline + `Ctrl+click`
to open); cursor styles (block/underline/beam + blink, honoring `DECSCUSR`);
broadcast input to all panes; tab reorder + moving panes between tabs.

**Big rocks** — multiple OS windows (renderer/window/tabs all assume a
singleton today); session restore (tabs, panes, cwd, commands); window
transparency/blur; Vi keyboard selection (copymode); graphics protocols
(Sixel/kitty inline images).

## Verify / build

```bash
cargo test                      # 31 headless tests
cargo build                     # debug
cargo build --release           # optimized (LTO, ~8 min)
./install.sh                    # release + install to ~/.local
./install.sh --system           # to /usr/local (sudo)
./uninstall.sh                  # removes binary/desktop/icon, keeps config
```
