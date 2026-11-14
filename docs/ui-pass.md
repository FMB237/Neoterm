# NeoTerm — global UI pass

A UI pass on the whole app: how the terminal *looks and feels*, not what it
does. The mockup that goes with this document lives next to it:
[`mockups/NeoTerm UI Pass.html`](mockups/NeoTerm%20UI%20Pass.html) (open it in
a browser; hover the tabs in the proposal).

**Direction: quiet chrome.** Chrome appears when it is relevant and fades when
it is not. Nothing permanent, nothing decorative. Every color below is either
already in the Tokyo Night pack or one new hairline derived from it.

---

## The problems, in order of how much they cost the user

| # | Problem | Kind | Fix |
|---|---|---|---|
| 1 | The cursor **hides the character under it** (the cursor is a `█` glyph that replaces the cell) | bug | draw the block in the background pass, keep the glyph on top |
| 2 | In a split, **focus is nearly invisible** — the only difference is which cursor blinks | usability | outline the focused pane, recede the others |
| 3 | **Copy and paste are silent** — you cannot tell that anything happened | feedback | a short toast in the strip |
| 4 | **No hover state anywhere** — the pointer can sit on a tab with zero feedback | affordance | hover reveals the close mark and lifts the label |
| 5 | The tab label **reads like a debug string**: `[1:bruce@host:~/projects x]` | taste | dim index badge, clean title, close mark on hover |
| 6 | The strip is **not legible as a region** — seamless means the top of the window reads as more text | taste | one hairline under the strip |

---

## 1. Cursor drawn behind the glyph

**Today.** `render_term_rows` replaces the cursor cell's character with `█`
(FULL BLOCK) in the cell's own foreground color. The character underneath is
gone. That is a fidelity bug, not a preference: you cannot read what you are
typing over, and `ls` output with a highlighted file loses its first letter
under the cursor.

**Proposal.** Draw a block *behind* the character:

```
cursor cell = [ block quad ][ glyph ]
block  = theme.cursor at 42 % over the background
glyph  = the cell's own characters, unchanged
```

**Implementation.**

- `tab.rs::render_term_rows` — stop substituting `█`. Return the cursor
  position instead of masking the cell. The cursor is already computed there
  (`cursor_char`); today it is used to blank the character. Keep the position,
  drop the substitution.
- `tab.rs::RowContent` — add `cursor: Option<usize>` (the character index in
  the row) alongside the existing `sel_cols`.
- `renderer.rs` — `PaneDraw` gains a cursor cell (col, row) and a
  `cursor_visible` flag. Draw one quad in the existing background pass, at
  the cursor cell, with `theme.cursor` at 42 % alpha. The text pass then
  draws the glyph on top, unchanged.
- Blink: the quad follows the existing blink flag (`blink && id == focus`).

**Invariant check.** The cursor must not move a single cell. The block is
drawn on the *same* cell geometry the text uses, so there is no drift — the
same guarantee the current glyph substitution gives.

**Accept.** The character under the cursor is readable; the cursor is still
unmistakable; blink still works; nothing shifts horizontally.

---

## 2. Focus indicator for split panes

**Today.** A split shows two panes that look identical. The focused pane is
only distinguishable by which cursor blinks — invisible in a screenshot, easy
to miss at speed.

**Proposal.** The focused pane carries a 1 px outline (accent at 38 %,
inset 1 px). Unfocused panes recede to 62 % opacity.

**Implementation.**

- `renderer.rs::draw` — for each pane rect, if it is not the focused pane,
  draw a scrim quad over it (background color at 38 %) to dim it; for the
  focused pane, draw a 1 px outline (four thin quads) just inside the rect.
- The focused pane id is already known (`t.active_id()`); pass it to `draw`.

**Alternative considered.** Dimming only (no outline) is less code but
ambiguous when both panes are equally important. Outlines read instantly.

**Accept.** With a split open, the focused pane is identifiable in a
screenshot; the outline never overlaps the text (it is inset by 1 px).

---

## 3. Copy / paste feedback

**Today.** `Ctrl+Shift+C`, a drag-select, or a middle-click paste does
something you cannot see. On X11 the PRIMARY buffer does get the selection,
but the system clipboard (the thing `Ctrl+Shift+V` reads) gives no signal at
all.

**Proposal.** A short pill in the **bottom-right corner of the body**:

```
copied · 42 chars
```

Surface `theme.tab_active_bg` (`#292e42`) — the one reserved field in the
theme finally gets a job. It holds for 1.4 s and then disappears.

*Design note:* the first draft put the toast in the tab strip. On screen it
was wrong: the strip is usually full of long tab titles, and a toast that
silently drops when there is no room is worse than no toast. Transient
feedback belongs over the content, where it has room and does not fight the
tabs.

**Implementation.**

- `App` — a `toast: Option<(String, Instant)>` state, set by `do_copy`,
  `do_paste`, `do_paste_primary` and the selection auto-copy.
- `renderer.rs` — the toast has its own text buffer and its own text area:
  a pill quad (in the background pass, so the label rides on top) plus the
  label. Its position is the body's bottom-right corner, inset by the body
  padding.
- The toast expires on a timer; `about_to_wait` already schedules wakeups.

**Accept.** Copying shows `copied · N chars` for ~1.2 s and then fades; the
toast never overlaps a tab label (it is right-aligned and the strip reserves
space); pasting shows `pasted · N chars`.

---

## 4. Hover states

**Today.** The pointer can sit on a tab and nothing happens.

**Proposal.** On hover: the close mark fades in (120 ms), the label lifts one
step (`#565f89` → `#8a93b8`), and a 5 % accent wash appears behind the tab.
No animation beyond the opacity fade.

**Implementation.**

- `main.rs` — the hovered tab index is derived from the cursor position with
  the existing `mouse::tab_regions` + `tab_hit` helpers. Pass `hover:
  Option<usize>` to `renderer::draw`.
- `renderer.rs` — the strip text is cached by `last_tab_text`; a hover change
  invalidates that cache and re-shapes the strip. That is one re-shape per
  hover change — cheap, and it only happens when the pointer crosses a tab.
- The close mark must stay **cell-accurate**: the hit target is a fixed cell
  box, and the mark is drawn *inside* that box. If the font measures `×` as
  wider than one cell (fallback fonts do), fall back to ASCII `x` — the
  current implementation deliberately uses `x` for exactly this reason, and
  that rule stays.

**Accept.** Hovering a tab reveals its close mark and lifts the label;
moving away reverses it in 120 ms; the close mark is still exactly one cell
wide in every font.

---

## 5. Tab label redesign

**Today.** `[1:bruce@host:~/projects x]` — an index, a colon, brackets and an
ASCII `x`, all crammed together.

**Proposal.**

```
1  bruce@host:~/projects  ×        <- active: accent title, 2px underline
2  vim                             <- inactive: dim
3  deploy                          <- pinned: palette magenta
```

- The index becomes a dim badge (`#4a5170`, one step below the inactive
  label) so the number never competes with the title.
- The brackets and colon go away.
- The close mark is revealed on hover (see #4) instead of always being there.
- A pinned (manually renamed) tab borrows `theme.palette[13]` (`#bb9af7`) so
  a renamed tab is distinguishable from a live OSC title at a glance. Never a
  new color.

**Implementation.** `mouse::tab_label` builds the label; `mouse::tab_bar_text`
joins them. Both are the single source of truth shared by the renderer and
hit-testing — change them together and the hit targets follow for free. The
label length is still counted in cells, so the region math stays exact.

**Careful.** `tab_span_ranges` splits the strip text into styled pieces
(active / inactive / close marks). The new label needs the same treatment:
the index, the title and the close mark become three separate spans.

**Accept.** The strip reads as titles, not as debug output; clicking still
hits the right tab; the close mark still closes the right tab.

---

## 6. Strip / body separation

**Today.** The strip is the body color with nothing between it and the text
below. The top of the window reads as more terminal output.

**Proposal.** One hairline under the strip: `#2b2c3d` (the scrollbar thumb
color, roughly). It separates the regions without adding a band — the
seamless Ghostty feel survives.

**Implementation.** One more quad in the background pass at
`body_top - 1 .. body_top`. New theme field `tab_bar_border`, default
`#2b2c3d`.

**Accept.** The strip is legible as a region; the body background is
untouched; the hairline is 1 physical pixel at every scale.

---

## New configuration

All optional, all with defaults that reproduce the proposal:

```toml
[theme]
tab_bar_border = "#2b2c3d"   # hairline under the strip
tab_index_fg   = "#4a5170"   # dim tab index badge
focus_outline  = "#7aa2f7"   # focused-pane outline (drawn at 38 %)
```

Old config files keep parsing — the new fields have defaults, like every
other theme field.

---

## Order of work

| Step | Item | Size | Depends on |
|---|---|---|---|
| 1 | Cursor behind the glyph | ~80 lines | — |
| 2 | Focus outline + pane scrim | ~60 lines | — |
| 3 | Hover state on tabs | ~90 lines | tab strip drawing |
| 4 | Toast (copy/paste feedback) | ~120 lines | strip drawing |
| 5 | Tab label redesign | ~120 lines | 3 (hover) |
| 6 | Strip hairline | ~30 lines | — |

Steps 1 and 2 are independent of the strip work and can land first — step 1
is a bug fix and should not wait for the rest.

---

## What this pass does **not** do

- Search UI, scroll position indicator, session restore, transparency: those
  are features, not polish.
- Cursor styles (underline / beam / hollow, `DECSCUSR`): a separate
  milestone. The mockup shows the direction only.
- Keyboard-navigation chrome: the strip is mouse-driven today and stays that
  way in this pass.
