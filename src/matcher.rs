// NFA-based pattern matching engine.
//
// Replaces the exponential-time recursive backtracking in rust-lang/glob
// with Thompson NFA simulation. Time complexity: O(tokens * input_len).
// This is safe for untrusted / LLM-generated patterns.
//
// The matcher implements POSIX fnmatch(3) semantics:
//   - `*` does not match `/` when require_literal_separator is true
//   - `?` does not match `/` when require_literal_separator is true
//   - Leading `.` requires a literal `.` when require_literal_leading_dot is true
//   - Matching is per-component or full-path depending on usage

use crate::pattern::{chars_eq, Pattern, Token};

/// Options controlling how patterns are matched against strings.
///
/// Follows the POSIX fnmatch(3) flag model:
/// - `case_sensitive` ↔ FNM_CASEFOLD (inverted)
/// - `require_literal_separator` ↔ FNM_PATHNAME
/// - `require_literal_leading_dot` ↔ FNM_PERIOD
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MatchOptions {
    /// Whether matching is case-sensitive. Default: `true`.
    pub case_sensitive: bool,
    /// Whether `*` and `?` should not match path separators. Default: `false`.
    ///
    /// When true, this maps to POSIX `FNM_PATHNAME` / the glob behavior
    /// where each pattern component is matched against one path component.
    pub require_literal_separator: bool,
    /// Whether `*` and `?` should not match a leading `.`. Default: `false`.
    ///
    /// When true, this maps to POSIX `FNM_PERIOD` / typical Unix behavior
    /// where dotfiles are hidden from wildcards.
    pub require_literal_leading_dot: bool,
}

impl Default for MatchOptions {
    fn default() -> Self {
        Self::new()
    }
}

impl MatchOptions {
    /// Default match options: case-sensitive, no special separator or dot handling.
    pub fn new() -> Self {
        MatchOptions {
            case_sensitive: true,
            require_literal_separator: false,
            require_literal_leading_dot: false,
        }
    }
}

impl Pattern {
    /// Test whether `input` matches this pattern using default options.
    pub fn matches(&self, input: &str) -> bool {
        self.matches_with(input, MatchOptions::new())
    }

    /// Test whether `input` matches this pattern with the given options.
    pub fn matches_with(&self, input: &str, opts: MatchOptions) -> bool {
        nfa_match(&self.tokens, input, opts)
    }

    /// Test whether a `Path` matches this pattern.
    pub fn matches_path(&self, path: &std::path::Path) -> bool {
        path.to_str().map_or(false, |s| self.matches(s))
    }

    /// Test whether a `Path` matches this pattern with options.
    pub fn matches_path_with(
        &self,
        path: &std::path::Path,
        opts: MatchOptions,
    ) -> bool {
        path.to_str().map_or(false, |s| self.matches_with(s, opts))
    }
}

// ── NFA simulation ──────────────────────────────────────────────────
//
// We maintain a set of "active states", where each state is an index
// into the token array. We advance all active states simultaneously
// for each input character. This guarantees O(T * N) time where T is
// the number of tokens and N is the input length.

fn nfa_match(tokens: &[Token], input: &str, opts: MatchOptions) -> bool {
    // Special case: empty token list matches only empty input.
    if tokens.is_empty() {
        return input.is_empty();
    }

    // `current` and `next` are sets of (token_index, follows_separator) pairs.
    // We use Vec + dedup instead of HashSet to avoid hashing overhead
    // for the small state counts typical of glob patterns.
    let mut current: Vec<(usize, bool)> = Vec::with_capacity(tokens.len() + 1);
    let mut next: Vec<(usize, bool)> = Vec::with_capacity(tokens.len() + 1);

    // Seed: start at token 0, follows_separator = true (start of path).
    add_state(&mut current, tokens, 0, true, opts);

    for c in input.chars() {
        next.clear();

        for &(ti, follows_sep) in &current {
            if ti >= tokens.len() {
                // Already past the end — this state is only valid if input is exhausted.
                continue;
            }

            let is_sep = is_separator(c);

            match &tokens[ti] {
                Token::AnySequence => {
                    // AnySequence: try consuming this char (stay in same state)
                    // and try skipping (epsilon to next state, handled by add_state).

                    // Check leading dot.
                    if follows_sep && opts.require_literal_leading_dot && c == '.' {
                        // Cannot match leading dot — this branch dies.
                        continue;
                    }
                    if opts.require_literal_separator && is_sep {
                        // Cannot match separator — this branch dies.
                        // But epsilon to next state was already added by add_state.
                        continue;
                    }
                    // Consume c, stay in AnySequence state.
                    add_state(&mut next, tokens, ti, is_sep, opts);
                }
                Token::AnyRecursiveSequence => {
                    // **: match any character, including separators.
                    // When we consume a separator, add_state will fire the
                    // epsilon transition to the next token (component boundary).

                    if follows_sep && opts.require_literal_leading_dot && c == '.' {
                        continue;
                    }
                    // Consume c, stay in ** state.
                    // is_sep tells add_state whether we're at a boundary.
                    add_state(&mut next, tokens, ti, is_sep, opts);
                }
                Token::AnyChar => {
                    if follows_sep && opts.require_literal_leading_dot && c == '.' {
                        continue;
                    }
                    if opts.require_literal_separator && is_sep {
                        continue;
                    }
                    // Consume c, advance to next token.
                    add_state(&mut next, tokens, ti + 1, is_sep, opts);
                }
                Token::AnyWithin(specs) => {
                    if follows_sep && opts.require_literal_leading_dot && c == '.' {
                        continue;
                    }
                    if opts.require_literal_separator && is_sep {
                        continue;
                    }
                    if specs.iter().any(|s| s.matches(c, opts.case_sensitive)) {
                        add_state(&mut next, tokens, ti + 1, is_sep, opts);
                    }
                }
                Token::AnyExcept(specs) => {
                    if follows_sep && opts.require_literal_leading_dot && c == '.' {
                        continue;
                    }
                    if opts.require_literal_separator && is_sep {
                        continue;
                    }
                    if !specs.iter().any(|s| s.matches(c, opts.case_sensitive)) {
                        add_state(&mut next, tokens, ti + 1, is_sep, opts);
                    }
                }
                Token::Char(expected) => {
                    if chars_eq(c, *expected, opts.case_sensitive) {
                        add_state(&mut next, tokens, ti + 1, is_sep, opts);
                    }
                }
            }
        }

        std::mem::swap(&mut current, &mut next);

        if current.is_empty() {
            return false; // Early exit: no active states.
        }
    }

    // End-of-input epsilon closure: any remaining wildcard states
    // (*, **) can match empty, so follow epsilon transitions forward.
    let mut final_states = Vec::new();
    for &(ti, follows_sep) in &current {
        add_end_state(&mut final_states, tokens, ti, follows_sep);
    }

    // Accept if any state is at or past the end of tokens.
    final_states.iter().any(|&(ti, _)| ti >= tokens.len())
}

/// Add a state, following epsilon transitions for wildcards.
///
/// When we reach a `*` or `**` token, we can epsilon-transition to
/// the next token (the wildcard matches empty string). We follow
/// these chains eagerly.
fn add_state(
    states: &mut Vec<(usize, bool)>,
    tokens: &[Token],
    ti: usize,
    follows_sep: bool,
    opts: MatchOptions,
) {
    // Avoid duplicates.
    let entry = (ti, follows_sep);
    if states.contains(&entry) {
        return;
    }
    states.push(entry);

    if ti >= tokens.len() {
        return;
    }

    // Epsilon transitions for wildcards: they can match empty string.
    match &tokens[ti] {
        Token::AnySequence => {
            // * can always epsilon to next token (match empty).
            add_state(states, tokens, ti + 1, follows_sep, opts);
        }
        Token::AnyRecursiveSequence => {
            // ** can only start the next sub-pattern at a component boundary.
            // It epsilon-transitions to the next token only when follows_sep
            // is true (at start of input or right after a `/`).
            if follows_sep {
                add_state(states, tokens, ti + 1, follows_sep, opts);
            }
        }
        _ => {}
    }
}

/// End-of-input epsilon closure.
///
/// At end of input, wildcards (* and **) can unconditionally match empty,
/// regardless of separator state. This follows the chain forward to find
/// all reachable acceptance states.
fn add_end_state(
    states: &mut Vec<(usize, bool)>,
    tokens: &[Token],
    ti: usize,
    follows_sep: bool,
) {
    let entry = (ti, follows_sep);
    if states.contains(&entry) {
        return;
    }
    states.push(entry);

    if ti >= tokens.len() {
        return;
    }

    match &tokens[ti] {
        Token::AnySequence | Token::AnyRecursiveSequence => {
            // At end of input, wildcards match empty unconditionally.
            add_end_state(states, tokens, ti + 1, follows_sep);
        }
        _ => {}
    }
}

fn is_separator(c: char) -> bool {
    c == '/' || (cfg!(windows) && c == '\\')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Pattern {
        Pattern::new(s).unwrap()
    }

    // ── Basic matching ──────────────────────────────────────────────

    #[test]
    fn literal() {
        assert!(p("hello").matches("hello"));
        assert!(!p("hello").matches("world"));
        assert!(!p("hello").matches("hell"));
        assert!(!p("hello").matches("helloo"));
    }

    #[test]
    fn question_mark() {
        assert!(p("h?llo").matches("hello"));
        assert!(p("h?llo").matches("hallo"));
        assert!(!p("h?llo").matches("hllo"));
        assert!(!p("h?llo").matches("heello"));
    }

    #[test]
    fn star() {
        assert!(p("*.rs").matches("main.rs"));
        assert!(p("*.rs").matches(".rs"));
        assert!(!p("*.rs").matches("main.txt"));
        assert!(p("h*o").matches("hello"));
        assert!(p("h*o").matches("ho"));
        assert!(!p("h*o").matches("hox"));
    }

    #[test]
    fn double_star() {
        let pat = p("src/**/*.rs");
        assert!(pat.matches("src/main.rs"));
        assert!(pat.matches("src/a/b/c/main.rs"));
        assert!(!pat.matches("test/main.rs"));
    }

    #[test]
    fn double_star_alone() {
        let pat = p("**");
        assert!(pat.matches(""));
        assert!(pat.matches("anything"));
        assert!(pat.matches("a/b/c"));
    }

    #[test]
    fn bracket() {
        assert!(p("[abc]").matches("a"));
        assert!(p("[abc]").matches("b"));
        assert!(!p("[abc]").matches("d"));
        assert!(p("[a-z]").matches("m"));
        assert!(!p("[a-z]").matches("M"));
    }

    #[test]
    fn negated_bracket() {
        assert!(!p("[!abc]").matches("a"));
        assert!(p("[!abc]").matches("d"));
        assert!(p("[!0-9]").matches("x"));
        assert!(!p("[!0-9]").matches("5"));
    }

    #[test]
    fn escape() {
        assert!(p("hello\\*world").matches("hello*world"));
        assert!(!p("hello\\*world").matches("helloXworld"));
    }

    // ── POSIX options ───────────────────────────────────────────────

    #[test]
    fn literal_separator() {
        let opts = MatchOptions {
            require_literal_separator: true,
            ..MatchOptions::new()
        };
        assert!(p("*").matches_with("hello", opts));
        assert!(!p("*").matches_with("a/b", opts));
        assert!(p("*/*").matches_with("a/b", opts));
    }

    #[test]
    fn literal_leading_dot() {
        let opts = MatchOptions {
            require_literal_leading_dot: true,
            ..MatchOptions::new()
        };
        assert!(!p("*").matches_with(".hidden", opts));
        assert!(p(".*").matches_with(".hidden", opts));
    }

    #[test]
    fn case_insensitive() {
        let opts = MatchOptions {
            case_sensitive: false,
            ..MatchOptions::new()
        };
        assert!(p("Hello").matches_with("hello", opts));
        assert!(p("HELLO").matches_with("hello", opts));
        assert!(p("[a-z]").matches_with("M", opts));
    }

    // ── Anti-backtracking: patterns that would be pathological ──────

    #[test]
    fn many_stars_no_match() {
        // This pattern would be O(2^9) with naive backtracking.
        // NFA handles it in linear time.
        let pat = p("a*a*a*a*a*a*a*a*a");
        assert!(pat.matches("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"));
        assert!(!pat.matches("aaaaaaaaaaaaaaaaaaaaaaaaaaaaab"));
    }

    #[test]
    fn adversarial_star_pattern() {
        // Classic pathological case for backtracking matchers.
        let pat = p("*a*b*c*d*e*f*g*h*i*j");
        assert!(!pat.matches("xxxxxxxxxxxxxxxxxxxx"));
    }

    // ── Recursive patterns ──────────────────────────────────────────

    #[test]
    fn recursive_needle() {
        let pat = p("some/**/needle.txt");
        assert!(pat.matches("some/needle.txt"));
        assert!(pat.matches("some/one/needle.txt"));
        assert!(pat.matches("some/one/two/needle.txt"));
        assert!(!pat.matches("some/other/notthis.txt"));
    }

    #[test]
    fn recursive_start() {
        let pat = p("**/test");
        assert!(pat.matches("test"));
        assert!(pat.matches("one/test"));
        assert!(pat.matches("one/two/test"));
        assert!(!pat.matches("one/notthis"));
    }

    #[test]
    fn recursive_end() {
        let pat = p("src/**");
        assert!(pat.matches("src/"));
        assert!(pat.matches("src/a"));
        assert!(pat.matches("src/a/b/c"));
    }

    // ── Empty patterns and inputs ───────────────────────────────────

    #[test]
    fn empty_pattern_empty_input() {
        assert!(p("").matches(""));
    }

    #[test]
    fn empty_pattern_nonempty_input() {
        assert!(!p("").matches("x"));
    }

    #[test]
    fn star_empty_input() {
        assert!(p("*").matches(""));
    }

    // ── Dot handling with ** ────────────────────────────────────────

    #[test]
    fn recursive_only_start_on_separator() {
        let pat = p("**/.*");
        assert!(pat.matches(".abc"));
        assert!(pat.matches("abc/.abc"));
        assert!(!pat.matches("ab.c"));
        assert!(!pat.matches("abc/ab.c"));
    }
}
