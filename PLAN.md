# NeoTerm — continuation plan

Purpose: a cold-start brief for whoever (or whichever agent) continues this
work. It records what is done, what is in flight, what is next, and the rules
that must not be broken.

Project root: `/home/bruce/NeoTerm`
Project doc (user-facing): `Neoterm.md` — keep it in sync with reality.

---

## 1. Where the project stands

**NeoTerm** — GPU terminal emulator in pure Rust. Tabs, splits, themes, no
toolkit. Currently **v0.3.0**, installed via `./install.sh` into `~/.local`.

| Layer | Crate |
|---|---|
| Windowing / input | winit 0.30 |
| GPU rendering | wgpu 30 |
| Text shaping | glyphon 0.12 + cosmic-text 0.19 |
| Terminal emulation | alacritty_terminal 0.26 (grid, vte, scrollback) |
| PTY + shell spawn | portable-pty 0.9 (+ vte 0.15 processor) |
| Clipboard (X11 PRIMARY + clipboard) | arboard 3 |
| Config | serde 1 + toml 0.8, watched with notify 6 |
| CLI | clap 4 |

Size: **~6,980 lines / 8 modules / 41 headless tests** (`cargo test`).

### 1.1 Already shipped (do not redo)

- **Core terminal** — PTY per shell, alacritty grid + scrollback, damage-gated
  rendering (frames only when output/damage/UI changed; cursor blinks without
  waking the loop otherwise).
- **Tabs** — click to switch, `×` / middle-click to close, `+` to add,
  auto-hide with one tab (`tab_bar = auto|always|never`), live OSC 0/2 titles,
  `exit` in the last tab quits.
- **Splits** — binary split tree per tab, 1-cell dividers (no text drift),
  drag-resize with minimum clamps, zoom toggle, click / Alt+arrow focus,
  per-pane PTY, scrollback, selection, palette. `exit` reaps one pane; tree
  surgery collapses the sibling subtree.
- **Mouse** — drag-select (char/word/line via click counting), SGR 1006 with
  pane-local coordinates, scrollbar inside the focused pane, hover cursors on
  dividers.
- **Clipboard** — drag-select auto-copies to PRIMARY, middle-click pastes
  (bracketed-paste safe), `Ctrl+Shift+C/V` for the system clipboard.
- **Themes** — `tokyo-night` (default), `catppuccin-mocha`, `dracula`,
  `gruvbox-dark`, or custom `[theme]`; 16-color ANSI palette override (app OSC
  still wins); per-cell background rects; seamless tab strip; live reload.
- **Ship** — versioned binary, desktop entry, hand-made SVG icon,
  `install.sh` / `uninstall.sh` (`--system`, `PREFIX`, `DESTDIR` honored).

### 1.2 Completed in the last work session (2026-10-19)

This is the most recent chunk of work; it is done, built, installed and
verified. Treat it as the new baseline.

| Change | Where | Notes |
|---|---|---|
| Font zoom, session-only | `input.rs`, `main.rs`, `renderer.rs` | `Ctrl+Shift+Plus`/`Minus`/`0`; accepts `+`/`=`, `-`/`_`, `0`/`)`; 1 pt steps, clamped 6–48; resets to config `font_size`; grid reflows on every step; a live zoom survives config reloads |
| `font_family` actually applied | `renderer.rs` | comma-separated preference list, first installed family wins; unknown → warn + generic monospace; proportional family → warn about grid drift; applied to body text, bold/italic spans and the tab strip |
| **sRGB color-management fix** | `config.rs`, `renderer.rs` | the `*Srgb` swapchain was being fed raw sRGB bytes as linear color, so `#1a1b26` rendered as slate gray `(90,92,108)`. All quad colors, cell backgrounds and the clear color are now linearized (`srgb8_to_linear`); measured on screen: `(26,27,38)` |
| Symmetric body padding | `renderer.rs` | 12 px left/right, 10 px top/bottom; tab strip grows with the font so zoom never clips labels |
| Active-tab accent underline | `renderer.rs` | 2 px `tab_accent` bar under the active tab, theme-driven |
| Alpha-blended chrome | `renderer.rs` | selection 0.72, scrollbar track 0.35 / thumb 0.85, pipeline switched to `ALPHA_BLENDING` with RGBA vertices |
| Hairline split dividers | `renderer.rs` | the layout cell is still exactly 1 grid cell, but the paint is a 1 px line centered in it instead of a painted 11 px bar |
| Real glyph-advance probe | `renderer.rs` | a throwaway shaped run measures the advance before the first frame, so the initial grid matches the font instead of the 0.62 em heuristic |
| Config watcher survives atomic saves | `main.rs` | watches the parent directory instead of the file inode (`vim`, `sed -i` no longer kill live reload) |
| Shortcut table made testable | `input.rs` | `chrome_action(key, ModifiersState) -> Option<InputAction>` extracted as a pure function |
| New tests (23 → 26) | `config.rs`, `input.rs` | sRGB linearization, linear theme quads, zoom key mapping |
| Docs refreshed | `Neoterm.md` | keybindings, config notes, module map, roadmap, test count |

### 1.3 Completed in Batch A (2026-11-12)

The three quick wins are now built, tested and verified on a real display.

| Change | Where | Notes |
|---|---|---|
| `--working-directory` / `-w` | `main.rs`, `tabs.rs`, `tab.rs` | canonicalized and validated at startup (a bad path fails before the window opens); the directory is threaded `App → Tabs → Tab → Pane` and applied with `CommandBuilder::cwd`; every new tab and split inherits it; default is still `$HOME`; a config reload never moves a running shell |
| Visual bell | `config.rs`, `tab.rs`, `tabs.rs`, `renderer.rs`, `main.rs` | `Term::bell()` already emits `Event::Bell`; the `TitleListener` (the term's event proxy) now raises a shared `AtomicBool`, the main loop drains it and tints the clear color for ~180 ms (`Renderer::set_flash`); `bell = "none"` mutes it; the flash has its own wake deadline so it is cleared on time |
| Manual tab titles | `tab.rs`, `tabs.rs`, `input.rs`, `main.rs` | `Ctrl+Shift+R` enters a rename mode (the strip is forced visible and shows the text being edited with a `_` caret); Enter commits and *pins* the title (pin wins over OSC titles), Esc cancels, `Ctrl+U` clears the field; the prefilled title behaves like selected text (the first keystroke replaces it); `Ctrl+Shift+U` releases the pin back to the live OSC title |

Bugs found and fixed while verifying (both were caught by looking at the
screen, not by the tests):

- The rename editor handled **key releases** as well as presses, so every
  character was inserted twice. Fixed by checking `event.state`.
- The prefilled title was appended to instead of replaced. Fixed with a
  `fresh` flag on the rename state.

New tests (26 → 31): bell flag raises + wakes the loop, pinned title wins
over the live title, `bell` option parsing (default + explicit `none`),
and the rename/unpin shortcut mapping.

### 1.4 Completed in Batch B (2026-11-14)

Batch B shipped B0–B3; B4 and B5 remain (see §4). All of it was verified on
screen as well as by tests, and every item below is a real commit.

| Change | Where | Notes |
|---|---|---|
| **B0. Global UI pass** | `renderer.rs`, `main.rs`, `mouse.rs`, `config.rs` | The cursor is drawn *behind* the glyph (inverse video) instead of replacing the character; unfocused panes recede behind a scrim and the focused pane gets a hairline outline; the tab strip became an index badge + clean title + hairline, with the close mark revealed on hover; copy/paste report through a pill in the body's bottom-right corner. New theme fields `tab_bar_border`, `tab_index_fg`. |
| **B1. Scrollback search** | `search.rs` (new, 345 lines), `main.rs`, `input.rs` | `Ctrl+Shift+F` opens a search over the focused pane's history *and* viewport; typing rescans live, `Enter`/`Ctrl+G` jump to the next match, `Shift+Enter` to the previous, `Esc` closes. Matches are highlighted through the existing per-cell bg-rect pass, in a muted accent. The engine is pure and headless-testable (9 tests). |
| **B2. OSC 8 hyperlinks** | `tab.rs`, `renderer.rs`, `main.rs` | `RowContent::StyledSpan` carries the link URI; link cells render underlined; `Ctrl+click` opens the URI with `xdg-open` / `open` / `start` depending on the platform, taking priority over selection and SGR reporting. |
| **B3. Cursor styles** | `renderer.rs`, `tab.rs` | `DECSCUSR` is honored: block, underline, beam and hollow block, drawn as geometry in the background pass so the glyph stays visible. `CursorShape::Hidden` draws nothing. The block still keeps the character underneath readable. |

### 1.5 In flight / open threads

- Nothing half-finished. Batch B's published slice (B0–B3) ends with a clean
  build and **41/41 tests** green.
- Two known gaps, both deliberate and recorded rather than hidden:
  - **Search has no visible query field.** Matches highlight and the pane
    scrolls, but the typed query and match count are not drawn anywhere yet;
    a persistent search bar is the next B1 polish item.
  - **`SGR 4` (plain underline) is still not drawn.** B2 underlines hyperlink
    cells only; `CellStyle` carries bold/italic but no underline flag. This is
    the cheap fidelity fix listed in §4.

### 1.6 Git history (through v0.3.0)

The project was developed before the repository existed, so the first
publication had a single squashed commit. The history was rebuilt on
2026-11-12 into eight commits, each a real, compiling, test-passing state
(`cargo test` verified at every commit):

| Commit | Date | Tests | Content |
|---|---|---|---|
| `8ea591f` | Oct 12 | 0 | workspace + crate manifest + placeholder window |
| `5766ee0` | Oct 14 | 18 | terminal engine: PTY, grid, GPU text, tabs, splits, mouse |
| `ee0d325` | Oct 14 | 23 | theme packs, ANSI palette, per-cell backgrounds |
| `a2d4a94` | Oct 14 | 23 | clipboard (PRIMARY auto-copy, paste) |
| `2b86e08` | Oct 14 | 23 | desktop entry, icon, install scripts |
| `6ae0e8a` | Oct 19 | 26 | font family, font zoom, true sRGB, padding, accent underline |
| `108bfaa` | Nov 12 | 31 | `--working-directory`, visual bell, manual tab titles |
| `f5d4b46` | Nov 12 | 31 | README, project document, this plan |

The October commits are a *reconstruction* of the states the code had then
(rebuilt by stripping the later features back out and re-verifying), because
the original bytes were never kept anywhere. The two November commits are the
original work, commit for commit. The README says so too.

Everything after `f5d4b46` is original work as well, one commit per feature:

| Commit | Date | Tests | Content |
|---|---|---|---|
| `7c421d1` | Nov 13 | 31 | UI pass: mockup + implementation plan |
| `62400d8` | Nov 14 | 32 | cursor drawn behind the glyph |
| `c096f5b` | Nov 14 | 32 | focus indicator for split panes |
| `d844fda` | Nov 14 | 32 | tab strip redesign (badge, hover, hairline) |
| `f917712` | Nov 14 | 32 | copy/paste feedback in the body corner |
| `1b31670` | Nov 14 | 41 | B1 — scrollback search |
| `ecb7408` | Nov 14 | 41 | B2 — OSC 8 hyperlinks |
| `1580866` | Nov 14 | 41 | B3 — cursor styles |


---

## 2. Repository map

```
/home/bruce/NeoTerm
├── Cargo.toml                  workspace (1 member: crates/neoterm)
├── Neoterm.md                  project doc — user-facing source of truth
├── PLAN.md                     this file — continuation brief
├── install.sh / uninstall.sh / launcher.sh
├── assets/                     neoterm.desktop, neoterm.svg
└── crates/neoterm/src/
    ├── main.rs      (~1920)  App event loop, mouse/keyboard handlers, draw
    │                          orchestration, config watcher, strip-visibility
    │                          reflow, font zoom + grid reflow, visual bell,
    │                          tab rename mode, search bar + hyperlink opening
    ├── tab.rs       (~1860)  Pane (PTY, grid, selection, OSC title listener),
    │                          PaneNode split tree, layout_tree /
    │                          focus_neighbor / clamp_ratio, Tab container
    │                          (title pins), free render_term_rows
    ├── renderer.rs  (~1330)  wgpu setup, per-pane buffer caches, font-family
    │                          + advance probing, pass 1 alpha rects (incl.
    │                          cursor geometry, focus outline, toast),
    │                          pass 2 shaped text, bell flash
    ├── config.rs    (~550)   NeotermConfig + Theme pack, preset application,
    │                          sRGB→linear conversion, `bell` option
    ├── mouse.rs     (~480)   pure geometry: labels, regions, close/+ zones,
    │                          scrollbar math, SGR encoding
    ├── search.rs    (~345)   pure scrollback search: match list, next/prev,
    │                          case folding — no winit/wgpu dependency
    ├── input.rs     (~260)   key event → InputAction (chrome_action table)
    └── tabs.rs      (~235)   tab list, ids, split/close/focus/zoom dispatch,
                              dead-pane reaping, title pins
```

Architecture at a glance:

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

---

## 3. Load-bearing invariants — do not break these

1. **Dividers are exactly 1 grid cell.** No pixel rounding anywhere in the
   split path; selection, SGR and wide-char math must stay exact. (Painting a
   hairline *inside* the cell is fine — the cell is still the cell.)
2. **Pane coordinates are pane-local.** Body→pane mapping happens exactly once
   per event, in `pane_cell_at`.
3. **Mouse press/release form a capture pair** bound to the press pane
   (`press_pane`/`press_local`); a pane closed mid-press degrades to a no-op.
4. **`resize_body` only resizes panes whose grid changed** — divider drags must
   not SIGWINCH-storm.
5. **Renderer caches are keyed by pane id and pruned every frame**; stale ids
   never survive a close.
6. **`TitleListener` wakes the loop through the shared dirty flag** with the
   same coalescing as the reader thread; OSC titles never busy-loop.
7. **Reaping never respawns** (buttons do); the app quits only when the tab
   list itself empties.
8. **Geometry has a single source of truth.** `Renderer::body_geometry` /
   `Renderer::grid_size` own cell metrics, padding and the tab-strip band;
   `mouse::tab_regions` / `tab_new_hit` / `tab_close_hit` are called with
   `renderer::TAB_LEFT * scale` and the same advance. If you change one, change
   all — a mismatch shows up as clicks landing on the wrong tab.
9. **All colors fed to the swapchain are linear.** Use
   `NeotermConfig::srgb8_to_linear` for any theme/ANSI color; glyphon does the
   same conversion internally for text. Raw bytes wash the palette out.

---

## 4. Next work, in priority order

### Batch A — the three quick wins — **DONE (2026-11-12)**

Small, independent, each ~50–150 lines. All three shipped; see §1.3 for the
design notes and the two bugs that the visual pass caught.

- ~~**A1. `--working-directory` CLI flag**~~ — done.
- ~~**A2. Bell support (`\a`)**~~ — done (visual bell; `bell = "none"`).
- ~~**A3. Manual tab rename (pinned title)**~~ — done (`Ctrl+Shift+R` /
  `Ctrl+Shift+U`).

### Batch B — medium items — **B0–B3 done, B4–B5 left**

- **B0. Global UI pass — DONE** — the interface work specified in
  [`docs/ui-pass.md`](docs/ui-pass.md), with the mockup in
  [`docs/mockups/NeoTerm UI Pass.html`](docs/mockups/NeoTerm%20UI%20Pass.html).
  All six items shipped:
  1. `fix(cursor)` — the cursor is drawn *behind* the glyph (inverse video),
     so it no longer hides the character under it. A fidelity bug, not taste.
  2. `feat(ui)` — focus indicator for splits: a hairline outline on the
     focused pane, the others recede behind a scrim (a third render pass).
  3. `feat(ui)` — tab strip redesign: dim index badge, clean titles, the
     close mark revealed on hover, a hairline under the strip.
  4. `feat(ui)` — copy/paste feedback: a pill in the body's bottom-right
     corner (`copied · 42 chars`), fading after 1.4 s. It lives over the
     body, not in the strip — the strip is usually full of long titles.
  New theme fields: `tab_bar_border`, `tab_index_fg`; `tab_active_bg` now
  has a job (the toast surface). Direction: *quiet chrome* — nothing
  permanent, nothing decorative.
- ~~**B1. Scrollback search**~~ — done (`1b31670`): `search.rs` is a pure
  engine over history + viewport, `Ctrl+Shift+F` opens it, `Enter` /
  `Shift+Enter` jump, `Esc` closes, matches highlight through the bg-rect
  pass. **Open:** the typed query and match count are not drawn yet — a
  persistent search bar is the next polish item.
- ~~**B2. OSC 8 hyperlinks**~~ — done (`ecb7408`): link cells render
  underlined, `Ctrl+click` opens the URI (`xdg-open` / `open` / `start`).
  **Open:** plain `SGR 4` still renders without an underline (below).
- ~~**B3. Cursor styles**~~ — done (`1580866`): block, underline, beam and
  hollow block, honoring `DECSCUSR`, drawn as geometry in the background pass
  so the glyph underneath stays readable.
- **B4. Broadcast input** — send keystrokes to every pane of the active tab.
- **B5. Tab reorder + move pane between tabs** — drag tabs on the strip; drag a
  pane across the tab boundary.

**Still worth folding in** (cheap): ANSI underline (`SGR 4`) — `CellStyle`
carries only bold/italic today, so underlined text renders plain. Hyperlinks
work around it by underlining link cells directly; ordinary `SGR 4` text is
still plain. Add an `underline` flag to `CellStyle` and map it to
`Attrs::underline`.

### Batch C — big rocks

- **C1. Multiple OS windows** — renderer, window and tab state all assume a
  singleton today; this is an architectural change, not a patch.
- **C2. Session restore** — tabs, panes, cwd and commands persisted and
  restored on launch.
- **C3. Window transparency / blur** — needs a transparent surface + compositor
  cooperation.
- **C4. Vi keyboard selection (copymode)** — motions, visual mode, search.
- **C5. Graphics protocols** — Sixel / kitty inline images.

---

## 4. Next work, in priority order (continued)

### Batch D — polish, workflow, and power-user features

Batch D focuses on the "daily driver" experience: finishing the search UX,
making sessions first-class, adding a command palette, and hardening the
platform edges. Each item is scoped to ~100–300 lines and testable in
isolation.

| Item | Description | Scope |
|------|-------------|-------|
| **D1. Persistent search bar** | Draw the typed query + match count in the tab strip (reusing the rename overlay pattern). `Enter`/`Shift+Enter` still jump, `Esc` closes. | `main.rs`, `renderer.rs`, `search.rs` (~120 lines) |
| **D2. Command palette** | `Ctrl+Shift+P` opens a fuzzy-searchable overlay: new tab/split/window, theme picker, font zoom, copy mode, session save/load, config shortcuts. Pure keyboard, no mouse required. | `main.rs`, `renderer.rs`, new `palette.rs` (~250 lines) |
| **D3. Session manager CLI** | `neoterm session save <name>`, `load <name>`, `list`, `delete`, `autosave on/off`. Sessions are JSON (already implemented in C2); CLI just wires them. | `main.rs`, `session.rs`, `clap` (~150 lines) |
| ~~**D4. Shell integration**~~ | ~~OSC 7 (cwd), OSC 133 (prompt mark), OSC 633 (command status). Fish/Zsh/Bash snippets in `assets/`. Enables: per-pane cwd tracking, jump-to-previous-prompt, failed-command highlight.~~ | `tab.rs`, `main.rs`, `assets/` (~200 lines + snippets) | ✅ `ce61a2e`
| **D5. SGR 4 underline** | Add `underline` flag to `CellStyle` in `tab.rs`, map to `Attrs::underline(UnderlineStyle::Single)` in `renderer.rs::row_attrs`. Fixes the last ANSI fidelity gap from B2. | `tab.rs`, `renderer.rs` (~30 lines) |
| **D6. Performance HUD** | `Ctrl+Shift+Alt+F` toggles a corner overlay: frame time (ms), FPS, GPU memory, atlas usage, dirty pane count. Helps catch regressions. | `renderer.rs`, `main.rs` (~100 lines) |
| **D7. Lazy pane rendering** | Don't shape/upload glyphs for panes scrolled out of view or fully occluded. Only the focused pane + visible neighbors render every frame. | `renderer.rs`, `main.rs` (~150 lines) |
| **D8. Windows/ConPTY support** | `portable-pty` already supports ConPTY; wire it behind `cfg(windows)` in `tab.rs::spawn`. Add MSVC CI. | `tab.rs`, `Cargo.toml`, `.github/` (~200 lines) |
| **D9. Man page + keybindings cheatsheet** | `neoterm.1` (ronn/markdown → man), `KEYBINDINGS.md` (auto-generated from `input.rs::chrome_action`). Install via `install.sh`. | `docs/`, `install.sh`, `input.rs` (~100 lines) |
| **D10. CI/CD pipeline** | GitHub Actions: `cargo test`, `cargo build --release`, `cargo audit`, `cargo deny`, release artifacts + checksums on tag. | `.github/workflows/` (~150 lines YAML) |

**Suggested order:** D1 → D5 (fidelity) → D2 (UX centerpiece) → D3 (workflow) → D4 (shell integration) → D6/D7 (perf) → D8/D9/D10 (platform/polish).

---

## 5. Verification recipe

```bash
cd /home/bruce/NeoTerm
cargo test                      # 41 headless tests, must stay green
cargo build                     # debug
cargo build --release           # optimized (LTO, ~8 min)
./install.sh                    # release + install to ~/.local
```

### Testing without disturbing the user's desktop

The desktop has other windows (and other NeoTerm instances) that synthetic
key events can land in. Run the app on a **private X server** instead:

```bash
Xvfb :99 -screen 0 1200x800x24 &
DISPLAY=:99 XDG_CONFIG_HOME=/tmp/opencode/nt-xdg RUST_LOG=info \
  VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.x86_64.json \
  VK_DRIVER_FILES=/usr/share/vulkan/icd.d/lvp_icd.x86_64.json \
  ./target/debug/neoterm -w /tmp
```

- lavapipe (`lvp_icd.json`) is the only Vulkan ICD that works under Xvfb;
  without it wgpu fails with *"Surface does not support the adapter's queue
  family"*. The Intel ICD fails under Xvfb, but works on the real display.
- There is **no window manager** on Xvfb, so set the input focus yourself
  (`XSetInputFocus`) and warp the pointer into the window; `wmctrl` lists
  nothing, so find the window with `xwininfo -root -tree` + `xprop
  _NET_WM_PID`.
- Screenshots: `xwd -root -silent -out f.xwd` then
  `ffmpeg -i f.xwd -vf "crop=W:H:X:Y" f.png`; PIL is available for pixel
  checks (a Tokyo Night body must read exactly `(26, 27, 38)`).

### Manual checks on X11 (a display must be present; a Vulkan/GL adapter is
required — Xvfb alone cannot create a wgpu surface on this machine):

1. Open a terminal, `Ctrl+Shift+T` twice → tabs appear and switch.
2. `Ctrl+Shift+Plus` / `Minus` / `0` → font zooms, grid reflows, selection and
   mouse mapping still line up.
3. `Ctrl+Shift+E` → split; the divider is a 1 px hairline; drag it; panes
   resize without SIGWINCH storms.
4. `Ctrl+Shift+R` → the strip appears and shows the title being edited; type,
   press Enter → the title is pinned; `Ctrl+Shift+U` → the live OSC title
   returns.
5. `printf '\a'` → the body tints for ~180 ms; with `bell = "none"` it does
   nothing.
6. `neoterm -w /tmp` → the shell starts in `/tmp` (`pwd` says so).
7. Add `font_family = "JetBrains Mono, monospace"` to the config → applies on
   save (live reload), with the family logged.
8. Background must read as true `#1a1b26` (Tokyo Night), not slate gray.

### Testing hygiene (learned the hard way)

- Use a throwaway config for experiments:
  `XDG_CONFIG_HOME=/tmp/opencode/neoterm-xdg ./target/debug/neoterm`.
- **Never** look a test window up by title alone — another NeoTerm instance may
  be running on the same desktop, and synthetic key events will land in it.
  Identify the window by PID:
  ```bash
  ID=$(for w in $(wmctrl -l | awk '/NeoTerm/{print $1}'); do
          p=$(xprop -id $w _NET_WM_PID | awk '{print $3}')
          [ "$p" = "$TESTPID" ] && echo $w
        done)
  ```
- When typing synthetic keys, remember that Shift changes the produced
  character: `\` and `'` are the *unshifted* forms of their keys, so pressing
  Shift while sending them types `|` and `"` instead.
- Screenshots without ImageMagick: `xwd -root -silent -out shot.xwd` then
  `ffmpeg -i shot.xwd -vf "crop=W:H:X:Y" shot.png`; `PIL` is available for
  pixel checks.

---

## 6. House rules

- Keep `Neoterm.md` truthful: update the module map, keybinding table, config
  block, milestone list and test count in the same change that alters the code.
- New behavior gets a headless test wherever it can be pure (config parsing,
  geometry, input mapping). The GUI itself is not test-covered — verify it by
  hand and record what you saw.
- Prefer small, reviewable diffs; the codebase is not `cargo fmt`-clean
  (pre-existing deviations exist), so do not reformat unrelated code.
- Version bumps happen in `crates/neoterm/Cargo.toml` (and the `--version`
  output) — keep them in sync with the milestone list.

---

## 7. Fixed Bugs (Batch D)

### Search Bar (D1) — `Ctrl+Shift+F`

| Bug | Root Cause | Fix | Commit |
|-----|------------|-----|--------|
| **Terminal freezes when search opens** | Keyboard handler consumed ALL keys but only handled Backspace/Escape/printable. Enter, arrows, Ctrl+Shift+F again were silently swallowed. | Only consume search-relevant keys (Backspace, Escape, Enter, printable). Others fall through to normal chrome_action/map_key. Added Enter=next match, Shift+Enter=prev match. | `be31f5b` |
| **Search bar not visible (panic)** | Double `unwrap()` on `Option<(&str, usize)>` — first unwrap consumed the Option, second panicked. | Properly destructure once with `if let Some((query, match_count)) = search`. | `83f9fb7` |
| **Search bar invisible when tab strip hidden** | Search bar rendering was inside `if strip { ... }` block. Invisible when `tab_bar=never` or `auto` with 1 tab. | Move search bar rendering outside strip condition — always renders when search active. | `7219b84` |

### Shell Integration (D4) — OSC 7/133/633

| Bug | Root Cause | Fix | Commit |
|-----|------------|-----|--------|
| **OSC 7/133/633 sequences not parsed** | PTY reader thread passed all bytes directly to vte processor without intercepting shell integration sequences. | Added `shell_integration.rs` with `preprocess_shell_integration()` called in reader thread before vte. Parses OSC 7 (cwd), OSC 133 (prompt), OSC 633 (status). Non-shell OSC pass through to vte. | `ce61a2e` |
| **Test failure: OSC 7 URL parsing** | Test expected empty result but OSC 8 hyperlink sequence was consumed instead of passing through. | Only consume OSC sequences we handle (7, 133, 633); return `None` for others so they pass through to vte. | `ce61a2e` (same commit) |
