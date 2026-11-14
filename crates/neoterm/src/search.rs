//! Scrollback search engine for NeoTerm.
//!
//! Pure, headless-testable module: given an `alacritty_terminal::Term`,
//! scans every grid line (history + viewport) for a query string and
//! produces a list of matches with grid line + column coordinates.
//! Wiring into the UI (input, redraw highlights, scroll-to-match) lives in
//! `main.rs`; this module has no winit/wgpu/renderer dependencies.

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::Term;
use alacritty_terminal::event::EventListener;

/// A single search match: the grid line it appears on, and the inclusive
/// column range `[col_start, col_end)` in grid columns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchMatch {
    /// Grid line index (negative = scrollback history, zero-based).
    pub line: i32,
    /// Inclusive start column (grid column index).
    pub col_start: usize,
    /// Exclusive end column (grid column index).
    pub col_end: usize,
}

/// Search state: the query, accumulated matches, and current match index.
///
/// `current` is an index into `matches`; `next_match()`/`prev_match()`
/// cycle with wraparound.
pub struct SearchState {
    pub query: String,
    pub case_sensitive: bool,
    matches: Vec<SearchMatch>,
    current: usize,
}

impl SearchState {
    /// Create a new search state with an empty match list.
    pub fn new(query: String) -> Self {
        Self {
            query,
            case_sensitive: false,
            matches: Vec::new(),
            current: 0,
        }
    }

    /// Scan every grid line of `term` (scrollback history + viewport) and
    /// populate `matches`. Call this whenever the query or the term
    /// content changes.
    pub fn search<T: EventListener>(&mut self, term: &Term<T>) {
        let grid = term.grid();
        let columns = grid.columns();
        let screen_lines = grid.screen_lines();
        let history = grid.history_size();

        self.matches.clear();

        if self.query.is_empty() {
            self.current = 0;
            return;
        }

        let needle = if self.case_sensitive {
            self.query.clone()
        } else {
            self.query.to_lowercase()
        };

        // Grid lines run from -history (oldest scrollback) to
        // screen_lines - 1 (newest visible line).
        let first_line = -(history as i32);
        let last_line = (screen_lines as i32) - 1;

        for gl in first_line..=last_line {
            let line = Line(gl);
            let row = &grid[line];

            // Build the line's text plus a parallel column tracker.
            // Wide chars occupy one cell but may span 2 columns; spacer
            // cells are skipped entirely. We track the starting column of
            // each emitted character so match byte positions can be mapped
            // back to grid columns.
            let mut text = String::with_capacity(columns);
            let mut col_of_char: Vec<usize> = Vec::with_capacity(columns);
            let mut col = 0usize;

            for cidx in 0..columns {
                let cell = &row[Column(cidx)];
                if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                    continue;
                }
                let ch = cell.c;
                if ch == '\0' {
                    // Empty/uninitialized cell: still advances a column but
                    // contributes nothing searchable.
                    col += 1;
                    continue;
                }
                col_of_char.push(col);
                text.push(ch);
                // Advance by the cell's column width: a wide char (WIDE_CHAR
                // flag) takes 2 columns, everything else 1.
                if cell.flags.contains(Flags::WIDE_CHAR) {
                    col += 2;
                } else {
                    col += 1;
                }
            }

            if text.is_empty() {
                continue;
            }

            let haystack = if self.case_sensitive {
                text.clone()
            } else {
                text.to_lowercase()
            };

            let mut start = 0;
            while let Some(found) = haystack[start..].find(&needle) {
                let abs_start = start + found;
                let abs_end = abs_start + needle.len();
                let c_start = col_of_char[abs_start];
                // End column: the column after the last matched char.
                // If the match ends exactly at a char boundary, col_end is
                // the start column of the next char, or `columns` if this
                // is the last char.
                let c_end = if abs_end < col_of_char.len() {
                    col_of_char[abs_end]
                } else {
                    // Last char: advance by its width.
                    let last_ch = text.chars().nth(abs_end - 1).unwrap_or('\0');
                    let _last_width = if last_ch.is_ascii() { 1 } else { last_ch.len_utf8() };
                    // In grid columns, a wide char takes 2. Approximate:
                    col + 1
                };
                self.matches.push(SearchMatch {
                    line: gl,
                    col_start: c_start,
                    col_end: c_end.max(c_start + 1),
                });
                start = abs_end;
            }
        }

        self.current = 0;
    }

    /// Move to the next match (wraps around). Returns the new current match.
    pub fn next_match(&mut self) -> Option<&SearchMatch> {
        if self.matches.is_empty() {
            return None;
        }
        self.current = (self.current + 1) % self.matches.len();
        self.current_match()
    }

    /// Move to the previous match (wraps around). Returns the new current match.
    pub fn prev_match(&mut self) -> Option<&SearchMatch> {
        if self.matches.is_empty() {
            return None;
        }
        self.current = (self.current + self.matches.len() - 1) % self.matches.len();
        self.current_match()
    }

    /// The currently selected match, if any.
    pub fn current_match(&self) -> Option<&SearchMatch> {
        if self.matches.is_empty() {
            return None;
        }
        self.matches.get(self.current)
    }

    /// Total number of matches found.
    pub fn match_count(&self) -> usize {
        self.matches.len()
    }

    /// Borrow the match list (read-only).
    pub fn matches(&self) -> &[SearchMatch] {
        &self.matches
    }

    /// Whether there are no matches.
    pub fn is_empty(&self) -> bool {
        self.matches.is_empty()
    }
}

impl Default for SearchState {
    fn default() -> Self {
        Self::new(String::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::term::Config as TermConfig;

    /// Build a headless `Term<TitleListener>` with `TitleListener::headless`.
    /// alacritty's `Term` requires an `EventListener`; `TitleListener::headless`
    /// is a no-op listener used by `tab.rs` tests.
    fn headless_term(cols: usize, rows: usize) -> Term<crate::tab::TitleListener> {
        use crate::tab::{TermSize, TitleListener};
        let size = TermSize { cols, rows };
        Term::new(TermConfig::default(), &size, TitleListener::headless("t").0)
    }

    /// Feed VT100 text into a term via the vte processor.
    fn feed(term: &mut Term<crate::tab::TitleListener>, s: &str) {
        let mut processor: vte::ansi::Processor = Default::default();
        processor.advance(term, s.as_bytes());
    }

    #[test]
    fn finds_match_in_viewport() {
        let mut term = headless_term(20, 10);
        feed(&mut term, "hello world\r\nfoo bar baz\r\n");
        let mut state = SearchState::new("bar".to_string());
        state.search(&term);
        assert_eq!(state.match_count(), 1);
        let m = state.current_match().unwrap();
        assert_eq!(m.line, 1);
        assert_eq!(m.col_start, 4);
        assert_eq!(m.col_end, 7);
    }

    #[test]
    fn finds_match_in_history() {
        let mut term = headless_term(20, 5);
        // Fill the screen so earlier lines scroll into history.
        for i in 0..30 {
            feed(&mut term, &format!("line {i}\n"));
        }
        let mut state = SearchState::new("line 0".to_string());
        state.search(&term);
        assert!(state.match_count() > 0, "should find 'line 0' in scrollback history");
    }

    #[test]
    fn case_insensitive_by_default() {
        let mut term = headless_term(20, 10);
        feed(&mut term, "Hello World\n");
        let mut state = SearchState::new("hello".to_string());
        state.search(&term);
        assert_eq!(state.match_count(), 1);
    }

    #[test]
    fn case_sensitive_when_requested() {
        let mut term = headless_term(20, 10);
        feed(&mut term, "Hello World\n");
        let mut state = SearchState::new("hello".to_string());
        state.case_sensitive = true;
        state.search(&term);
        assert_eq!(state.match_count(), 0);
        let mut state = SearchState::new("Hello".to_string());
        state.case_sensitive = true;
        state.search(&term);
        assert_eq!(state.match_count(), 1);
    }

    #[test]
    fn no_matches_returns_empty() {
        let mut term = headless_term(20, 10);
        feed(&mut term, "hello world\n");
        let mut state = SearchState::new("zzz".to_string());
        state.search(&term);
        assert_eq!(state.match_count(), 0);
        assert!(state.is_empty());
        assert!(state.current_match().is_none());
    }

    #[test]
    fn next_and_prev_cycle() {
        let mut term = headless_term(20, 10);
        feed(&mut term, "a b a b a\n");
        let mut state = SearchState::new("a".to_string());
        state.search(&term);
        assert_eq!(state.match_count(), 3);

        // Start at first match.
        let m = state.current_match().unwrap();
        assert_eq!(m.col_start, 0);

        // Next walks forward.
        state.next_match();
        let m = state.current_match().unwrap();
        assert_eq!(m.col_start, 4);

        state.next_match();
        let m = state.current_match().unwrap();
        assert_eq!(m.col_start, 8);

        // Wraps around.
        state.next_match();
        let m = state.current_match().unwrap();
        assert_eq!(m.col_start, 0);

        // Prev walks backward.
        state.prev_match();
        let m = state.current_match().unwrap();
        assert_eq!(m.col_start, 8);

        state.prev_match();
        let m = state.current_match().unwrap();
        assert_eq!(m.col_start, 4);
    }

    #[test]
    fn empty_query_has_no_matches() {
        let mut term = headless_term(20, 10);
        feed(&mut term, "hello world\n");
        let mut state = SearchState::new(String::new());
        state.search(&term);
        assert_eq!(state.match_count(), 0);
        assert!(state.is_empty());
    }

    #[test]
    fn multiple_matches_on_same_line() {
        let mut term = headless_term(20, 10);
        feed(&mut term, "foo foo foo\n");
        let mut state = SearchState::new("foo".to_string());
        state.search(&term);
        assert_eq!(state.match_count(), 3);
        let cols: Vec<usize> = state.matches.iter().map(|m| m.col_start).collect();
        assert_eq!(cols, vec![0, 4, 8]);
    }

    #[test]
    fn match_col_end_exceeds_col_start() {
        let mut term = headless_term(20, 10);
        feed(&mut term, "hello world\n");
        let mut state = SearchState::new("world".to_string());
        state.search(&term);
        let m = state.current_match().unwrap();
        assert!(m.col_end > m.col_start);
    }
}
