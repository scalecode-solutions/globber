// Pattern compilation.
//
// Follows the POSIX fnmatch(3) pattern grammar plus extensions:
//   ?          — any single character
//   *          — any sequence of non-separator characters
//   **         — any sequence of path components (recursive)
//   [abc]      — character class
//   [!abc]     — negated character class
//   [a-z]      — character range
//   {a,b,c}    — brace expansion (GLOB_BRACE)
//   \x         — literal escape (GLOB_NOESCAPE disables)
//
// The compiled form is a flat token vector consumed by the NFA matcher.
// No heap allocation for simple patterns (no brackets/braces).

use crate::error::{PatternError, PatternErrorKind};

/// A single compiled token in a pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token {
    /// Literal character.
    Char(char),
    /// `?` — matches any single non-separator character.
    AnyChar,
    /// `*` — matches any sequence of non-separator characters.
    AnySequence,
    /// `**` — matches zero or more path components.
    AnyRecursiveSequence,
    /// `[abc]` or `[a-z]` — matches one character in the set.
    AnyWithin(Vec<CharSpec>),
    /// `[!abc]` or `[!a-z]` — matches one character NOT in the set.
    AnyExcept(Vec<CharSpec>),
}

/// A character specifier inside a bracket expression.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CharSpec {
    Single(char),
    Range(char, char),
}

impl CharSpec {
    pub fn matches(&self, c: char, case_sensitive: bool) -> bool {
        match *self {
            CharSpec::Single(sc) => chars_eq(c, sc, case_sensitive),
            CharSpec::Range(lo, hi) => {
                if !case_sensitive && c.is_ascii() && lo.is_ascii() && hi.is_ascii() {
                    let c = c.to_ascii_lowercase();
                    let lo = lo.to_ascii_lowercase();
                    let hi = hi.to_ascii_lowercase();
                    c >= lo && c <= hi
                } else {
                    c >= lo && c <= hi
                }
            }
        }
    }
}

/// A compiled pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
    /// The original pattern string.
    pub(crate) original: String,
    /// Compiled token sequence.
    pub(crate) tokens: Vec<Token>,
    /// Whether this pattern contains `**`.
    pub(crate) is_recursive: bool,
    /// Whether this pattern contains any metacharacters at all.
    pub(crate) has_meta: bool,
}

impl Pattern {
    /// Compile a glob pattern.
    ///
    /// Returns `Err` if the pattern is syntactically invalid.
    pub fn new(pattern: &str) -> Result<Self, PatternError> {
        let bytes = pattern.as_bytes();
        let mut tokens = Vec::new();
        let mut is_recursive = false;
        let mut has_meta = false;
        let mut i = 0;

        while i < bytes.len() {
            match bytes[i] {
                b'?' => {
                    has_meta = true;
                    tokens.push(Token::AnyChar);
                    i += 1;
                }
                b'*' => {
                    has_meta = true;
                    let start = i;
                    while i < bytes.len() && bytes[i] == b'*' {
                        i += 1;
                    }
                    let count = i - start;
                    if count > 2 {
                        return Err(PatternError {
                            pos: start + 2,
                            kind: PatternErrorKind::InvalidWildcard,
                        });
                    }
                    if count == 2 {
                        // ** must be a standalone path component.
                        let before_ok =
                            start == 0 || is_separator(bytes[start - 1]);
                        let after_ok = if i < bytes.len() {
                            if is_separator(bytes[i]) {
                                i += 1; // consume trailing separator
                                true
                            } else {
                                false
                            }
                        } else {
                            true // ** at end of pattern
                        };
                        if !before_ok {
                            return Err(PatternError {
                                pos: start.saturating_sub(1),
                                kind: PatternErrorKind::RecursiveNotAlone,
                            });
                        }
                        if !after_ok {
                            return Err(PatternError {
                                pos: i,
                                kind: PatternErrorKind::RecursiveNotAlone,
                            });
                        }
                        // Collapse consecutive ** tokens.
                        if tokens.last() != Some(&Token::AnyRecursiveSequence) {
                            is_recursive = true;
                            tokens.push(Token::AnyRecursiveSequence);
                        }
                    } else {
                        tokens.push(Token::AnySequence);
                    }
                }
                b'[' => {
                    has_meta = true;
                    let start = i;
                    i += 1;
                    let negated = i < bytes.len() && bytes[i] == b'!';
                    if negated {
                        i += 1;
                    }
                    // First char after `[` or `[!` can be `]` and is literal.
                    let bracket_start = i;
                    if i < bytes.len() && bytes[i] == b']' {
                        i += 1;
                    }
                    // Find closing `]`.
                    while i < bytes.len() && bytes[i] != b']' {
                        i += 1;
                    }
                    if i >= bytes.len() {
                        return Err(PatternError {
                            pos: start,
                            kind: PatternErrorKind::UnclosedBracket,
                        });
                    }
                    let inner = &pattern[bracket_start..i];
                    i += 1; // skip `]`
                    if inner.is_empty() {
                        return Err(PatternError {
                            pos: start,
                            kind: PatternErrorKind::EmptyBracket,
                        });
                    }
                    let specs = parse_char_specs(inner);
                    if negated {
                        tokens.push(Token::AnyExcept(specs));
                    } else {
                        tokens.push(Token::AnyWithin(specs));
                    }
                }
                b'\\' => {
                    // Backslash escape: next char is literal.
                    i += 1;
                    if i < bytes.len() {
                        let ch = pattern[i..].chars().next().unwrap();
                        tokens.push(Token::Char(ch));
                        i += ch.len_utf8();
                    }
                    // Trailing backslash: ignore (POSIX says unspecified).
                }
                _ => {
                    let ch = pattern[i..].chars().next().unwrap();
                    tokens.push(Token::Char(ch));
                    i += ch.len_utf8();
                }
            }
        }

        Ok(Pattern {
            original: pattern.to_string(),
            tokens,
            is_recursive,
            has_meta,
        })
    }

    /// The original pattern string.
    pub fn as_str(&self) -> &str {
        &self.original
    }

    /// Escape metacharacters so the result matches the literal string.
    pub fn escape(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        for c in s.chars() {
            match c {
                '?' | '*' | '[' | ']' | '{' | '}' | '\\' => {
                    out.push('\\');
                    out.push(c);
                }
                _ => out.push(c),
            }
        }
        out
    }
}

impl std::fmt::Display for Pattern {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.original)
    }
}

impl std::str::FromStr for Pattern {
    type Err = PatternError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Pattern::new(s)
    }
}

// ── Helpers ──────────────────────────────────────────────────────────

fn is_separator(b: u8) -> bool {
    b == b'/' || (cfg!(windows) && b == b'\\')
}

pub(crate) fn chars_eq(a: char, b: char, case_sensitive: bool) -> bool {
    if !case_sensitive && a.is_ascii() && b.is_ascii() {
        a.eq_ignore_ascii_case(&b)
    } else {
        a == b
    }
}

fn parse_char_specs(s: &str) -> Vec<CharSpec> {
    let chars: Vec<char> = s.chars().collect();
    let mut specs = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if i + 2 < chars.len() && chars[i + 1] == '-' {
            specs.push(CharSpec::Range(chars[i], chars[i + 2]));
            i += 3;
        } else {
            specs.push(CharSpec::Single(chars[i]));
            i += 1;
        }
    }
    specs
}

// ── Brace expansion ─────────────────────────────────────────────────

/// Expand brace expressions in a pattern string.
///
/// `{a,b,c}` expands to three patterns. Braces can be nested.
/// Returns the original string in a one-element vec if no braces.
pub fn expand_braces(pattern: &str) -> Vec<String> {
    let bytes = pattern.as_bytes();
    // Find the first top-level `{`.
    let mut depth = 0i32;
    let mut brace_start = None;
    for (i, &b) in bytes.iter().enumerate() {
        if i > 0 && bytes[i - 1] == b'\\' {
            continue;
        }
        match b {
            b'{' => {
                if depth == 0 {
                    brace_start = Some(i);
                }
                depth += 1;
            }
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    let start = brace_start.unwrap();
                    let prefix = &pattern[..start];
                    let suffix = &pattern[i + 1..];
                    let inner = &pattern[start + 1..i];

                    // Split inner by top-level commas.
                    let alternatives = split_brace_alternatives(inner);
                    let mut results = Vec::new();
                    for alt in &alternatives {
                        let expanded_suffix = expand_braces(suffix);
                        let expanded_alt = expand_braces(&format!("{}{}", prefix, alt));
                        for a in &expanded_alt {
                            for s in &expanded_suffix {
                                results.push(format!("{}{}", a, s));
                            }
                        }
                    }
                    return results;
                }
            }
            _ => {}
        }
    }
    // No braces found.
    vec![pattern.to_string()]
}

fn split_brace_alternatives(s: &str) -> Vec<&str> {
    let bytes = s.as_bytes();
    let mut depth = 0i32;
    let mut parts = Vec::new();
    let mut start = 0;
    for (i, &b) in bytes.iter().enumerate() {
        if i > 0 && bytes[i - 1] == b'\\' {
            continue;
        }
        match b {
            b'{' => depth += 1,
            b'}' => depth -= 1,
            b',' if depth == 0 => {
                parts.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&s[start..]);
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_literal() {
        let p = Pattern::new("hello.txt").unwrap();
        assert!(!p.has_meta);
        assert!(!p.is_recursive);
        assert_eq!(p.tokens.len(), 9);
    }

    #[test]
    fn star() {
        let p = Pattern::new("*.rs").unwrap();
        assert!(p.has_meta);
        assert!(!p.is_recursive);
        assert_eq!(p.tokens[0], Token::AnySequence);
    }

    #[test]
    fn double_star() {
        let p = Pattern::new("src/**/*.rs").unwrap();
        assert!(p.is_recursive);
        assert!(p.has_meta);
    }

    #[test]
    fn triple_star_err() {
        assert!(Pattern::new("***").is_err());
    }

    #[test]
    fn double_star_not_alone() {
        assert!(Pattern::new("a**b").is_err());
        assert!(Pattern::new("a**/b").is_err());
        assert!(Pattern::new("a/**b").is_err());
    }

    #[test]
    fn bracket() {
        let p = Pattern::new("[abc]").unwrap();
        assert!(p.has_meta);
        match &p.tokens[0] {
            Token::AnyWithin(specs) => {
                assert_eq!(specs.len(), 3);
            }
            _ => panic!("expected AnyWithin"),
        }
    }

    #[test]
    fn negated_bracket() {
        let p = Pattern::new("[!0-9]").unwrap();
        match &p.tokens[0] {
            Token::AnyExcept(specs) => {
                assert_eq!(specs.len(), 1);
                assert_eq!(specs[0], CharSpec::Range('0', '9'));
            }
            _ => panic!("expected AnyExcept"),
        }
    }

    #[test]
    fn unclosed_bracket() {
        assert!(Pattern::new("abc[def").is_err());
    }

    #[test]
    fn bracket_with_close_first() {
        // []] matches `]`
        let p = Pattern::new("[]]").unwrap();
        match &p.tokens[0] {
            Token::AnyWithin(specs) => {
                assert_eq!(specs[0], CharSpec::Single(']'));
            }
            _ => panic!("expected AnyWithin"),
        }
    }

    #[test]
    fn escape() {
        let p = Pattern::new("hello\\*world").unwrap();
        assert!(!p.has_meta);
        assert_eq!(p.tokens.len(), 11);
        assert_eq!(p.tokens[5], Token::Char('*'));
    }

    #[test]
    fn brace_expansion() {
        let mut r = expand_braces("{a,b,c}");
        r.sort();
        assert_eq!(r, vec!["a", "b", "c"]);
    }

    #[test]
    fn brace_with_prefix_suffix() {
        let mut r = expand_braces("src/{lib,main}.rs");
        r.sort();
        assert_eq!(r, vec!["src/lib.rs", "src/main.rs"]);
    }

    #[test]
    fn nested_braces() {
        let mut r = expand_braces("{a,{b,c}}");
        r.sort();
        assert_eq!(r, vec!["a", "b", "c"]);
    }

    #[test]
    fn no_braces() {
        assert_eq!(expand_braces("hello"), vec!["hello"]);
    }

    #[test]
    fn escape_roundtrip() {
        let s = "hello[world]*?.txt";
        let escaped = Pattern::escape(s);
        let p = Pattern::new(&escaped).unwrap();
        assert!(!p.has_meta);
    }

    #[test]
    fn collapse_consecutive_recursive() {
        let p = Pattern::new("a/**/**/b").unwrap();
        let rec_count = p
            .tokens
            .iter()
            .filter(|t| **t == Token::AnyRecursiveSequence)
            .count();
        assert_eq!(rec_count, 1);
    }
}
