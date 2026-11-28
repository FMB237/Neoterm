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
use regex::RegexBuilder;

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
    pub regex: bool,
    pub whole_word: bool,
    matches: Vec<SearchMatch>,
    current: usize,
    // Cached regex to avoid recompilation on every search
    regex_cache: Option<regex::Regex>,
    // True when the last query failed to compile as a regex.
    invalid_pattern: bool,
}

impl SearchState {
    /// Create a new search state with an empty match list.
    pub fn new(query: String) -> Self {
        Self {
            query,
            case_sensitive: false,
            regex: false,
            whole_word: false,
            matches: Vec::new(),
            current: 0,
            regex_cache: None,
            invalid_pattern: false,
        }
    }

    /// Get the current match index (0-based).
    pub fn current_index(&self) -> usize {
        self.current
    }

    /// Set the current match index (clamped to match list length).
    pub fn set_current(&mut self, index: usize) {
        if !self.matches.is_empty() {
            self.current = index.min(self.matches.len().saturating_sub(1));
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
        self.current = 0;

        if self.query.is_empty() {
            self.regex_cache = None;
            self.invalid_pattern = false;
            return;
        }

        // A regex is needed when the user asked for one, or when whole-word
        // mode needs `\b` boundaries around a literal query.
        let needs_regex = self.regex || self.whole_word;
        if !needs_regex {
            self.regex_cache = None;
            self.invalid_pattern = false;
        } else if self.regex_cache.is_none() {
            let pattern = if self.regex {
                self.query.clone()
            } else {
                regex::escape(&self.query)
            };
            let pattern = if self.whole_word {
                if self.regex {
                    format!(r"\b(?:{})\b", pattern)
                } else {
                    format!(r"\b{}\b", pattern)
                }
            } else {
                pattern
            };
            match RegexBuilder::new(&pattern)
                .case_insensitive(!self.case_sensitive)
                .build()
            {
                Ok(re) => {
                    self.regex_cache = Some(re);
                    self.invalid_pattern = false;
                }
                Err(_) => {
                    self.regex_cache = None;
                    self.invalid_pattern = true;
                    return;
                }
            }
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

            if let Some(ref regex) = self.regex_cache {
                for m in regex.find_iter(&text) {
                    // Skip zero-width matches: they would highlight nothing.
                    if m.start() == m.end() {
                        continue;
                    }
                    let c_start = col_of_char.get(m.start()).copied().unwrap_or(0);
                    let c_end = col_of_char
                        .get(m.end())
                        .copied()
                        .unwrap_or_else(|| col_of_char.last().copied().unwrap_or(0) + 1);
                    self.matches.push(SearchMatch {
                        line: gl,
                        col_start: c_start,
                        col_end: c_end.max(c_start + 1),
                    });
                }
            } else {
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
                    // the start column of the next char, or the line width if
                    // this is the last char.
                    let c_end = if abs_end < col_of_char.len() {
                        col_of_char[abs_end]
                    } else {
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
        }
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

    /// Total number matches found.
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

    /// Append a character to the query.
    ///
    /// Any compiled regex is dropped: it was built from the previous query and
    /// would otherwise keep matching the old text.
    pub fn push_char(&mut self, c: char) {
        self.query.push(c);
        self.regex_cache = None;
    }

    /// Remove the last character of the query. Returns whether anything was
    /// removed. Drops the compiled regex for the same reason as `push_char`.
    pub fn backspace(&mut self) -> bool {
        let popped = self.query.pop().is_some();
        if popped {
            self.regex_cache = None;
        }
        popped
    }

    /// Whether the current query is an invalid regular expression.
    pub fn invalid_pattern(&self) -> bool {
        self.invalid_pattern
    }

    /// Toggle regex mode.
    pub fn toggle_regex(&mut self) {
        self.regex = !self.regex;
        self.regex_cache = None; // Force rebuild on next search
    }

    /// Toggle whole word mode.
    pub fn toggle_whole_word(&mut self) {
        self.whole_word = !self.whole_word;
        self.regex_cache = None; // Force rebuild on next search
    }

    /// Set regex mode explicitly.
    pub fn set_regex(&mut self, enabled: bool) {
        if self.regex != enabled {
            self.regex = enabled;
            self.regex_cache = None;
        }
    }

    /// Set whole word mode explicitly.
    pub fn set_whole_word(&mut self, enabled: bool) {
        if self.whole_word != enabled {
            self.whole_word = enabled;
            self.regex_cache = None;
        }
    }

    /// Set case sensitivity.
    pub fn set_case_sensitive(&mut self, enabled: bool) {
        if self.case_sensitive != enabled {
            self.case_sensitive = enabled;
            self.regex_cache = None;
        }
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

    #[test]
    fn regex_mode_matches_pattern() {
        let mut term = headless_term(20, 10);
        feed(&mut term, "hello world\nhello foo\n");
        let mut state = SearchState::new(r"hello \w+".to_string());
        state.set_regex(true);
        state.search(&term);
        assert_eq!(state.match_count(), 2);
    }

    #[test]
    fn regex_mode_case_insensitive() {
        let mut term = headless_term(20, 10);
        feed(&mut term, "Hello World\n");
        let mut state = SearchState::new("hello".to_string());
        state.set_regex(true);
        state.set_case_sensitive(false);
        state.search(&term);
        assert_eq!(state.match_count(), 1);
    }

    #[test]
    fn whole_word_mode_matches_only_whole_words() {
        let mut term = headless_term(20, 10);
        feed(&mut term, "hello world hellofoo\n");
        let mut state = SearchState::new("hello".to_string());
        state.set_whole_word(true);
        state.search(&term);
        assert_eq!(state.match_count(), 1);
    }

    #[test]
    fn toggle_regex_clears_cache() {
        let mut term = headless_term(20, 10);
        feed(&mut term, "hello world\n");
        let mut state = SearchState::new("hello".to_string());
        state.set_regex(true);
        state.search(&term);
        assert_eq!(state.match_count(), 1);
        
        // Toggle regex off
        state.toggle_regex();
        state.search(&term);
        // Should still find match in plain text mode
        assert_eq!(state.match_count(), 1);
    }

    #[test]
    fn toggle_whole_word_clears_cache() {
        let mut term = headless_term(20, 10);
        feed(&mut term, "hello world hellofoo\n");
        let mut state = SearchState::new("hello".to_string());
        state.set_whole_word(true);
        state.search(&term);
        assert_eq!(state.match_count(), 1);
        
        // Toggle whole word off
        state.toggle_whole_word();
        state.search(&term);
        assert_eq!(state.match_count(), 2);
    }

    #[test]
    fn editing_the_query_rebuilds_the_regex() {
        let mut term = headless_term(20, 10);
        feed(&mut term, "hello world hellofoo\r\n");
        let mut state = SearchState::new("hello".to_string());
        state.set_regex(true);
        state.search(&term);
        assert_eq!(state.match_count(), 2);

        // Editing the query must invalidate the compiled regex; otherwise the
        // cache keeps matching "hello" and the count never changes.
        while state.backspace() {}
        state.push_char('w');
        state.search(&term);
        assert_eq!(state.match_count(), 1, "query 'w' should match only 'world'");
    }
}
