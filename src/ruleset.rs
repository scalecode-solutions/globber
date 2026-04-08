// Multi-pattern rulesets — the AI-native primitive.
//
// POSIX glob is one pattern → one walk. AI agents need:
//   - Multiple include patterns
//   - Multiple exclude patterns (negation, like .gitignore `!`)
//   - Priority weights for ranking results
//   - Automatic .gitignore loading
//
// A Ruleset compiles all patterns upfront and evaluates them
// per-file in a single pass.

use std::path::Path;

use crate::entry::{Entry, FileKind};
use crate::error::GlobError;
use crate::matcher::MatchOptions;
use crate::pattern::Pattern;
use crate::walker::WalkOptions;

/// A rule in a ruleset.
#[derive(Debug, Clone)]
struct Rule {
    pattern: Pattern,
    kind: RuleKind,
    weight: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum RuleKind {
    Include,
    Exclude,
}

/// A compiled set of include/exclude/priority rules.
///
/// This is the AI-native glob primitive. Instead of one pattern,
/// you express a full file selection policy.
///
/// ```
/// use globber::Ruleset;
///
/// let rules = Ruleset::new()
///     .include("src/**/*.rs")
///     .include("lib/**/*.rs")
///     .exclude("**/generated/**")
///     .exclude("**/target/**")
///     .build()
///     .unwrap();
/// ```
#[derive(Debug)]
pub struct Ruleset {
    rules: Vec<Rule>,
    match_opts: MatchOptions,
}

/// Builder for constructing a Ruleset.
pub struct RulesetBuilder {
    entries: Vec<(String, RuleKind, f32)>,
    match_opts: MatchOptions,
}

impl RulesetBuilder {
    /// Add an include pattern with default weight (1.0).
    pub fn include(self, pattern: &str) -> Self {
        self.include_weighted(pattern, 1.0)
    }

    /// Add an include pattern with a custom weight.
    pub fn include_weighted(mut self, pattern: &str, weight: f32) -> Self {
        self.entries
            .push((pattern.to_string(), RuleKind::Include, weight));
        self
    }

    /// Add an exclude pattern.
    pub fn exclude(mut self, pattern: &str) -> Self {
        self.entries
            .push((pattern.to_string(), RuleKind::Exclude, 0.0));
        self
    }

    /// Set match options for all patterns.
    pub fn match_options(mut self, opts: MatchOptions) -> Self {
        self.match_opts = opts;
        self
    }

    /// Load .gitignore rules from a file as exclude patterns.
    pub fn gitignore(self, path: &Path) -> Self {
        self.gitignore_from(path)
    }

    /// Load .gitignore-style rules from a file.
    fn gitignore_from(mut self, path: &Path) -> Self {
        if let Ok(content) = std::fs::read_to_string(path) {
            for line in content.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                if let Some(negated) = line.strip_prefix('!') {
                    // Negation in gitignore = include.
                    let pattern = if negated.contains('/') {
                        negated.to_string()
                    } else {
                        format!("**/{}", negated)
                    };
                    self.entries
                        .push((pattern, RuleKind::Include, 1.0));
                } else {
                    let pattern = if line.contains('/') {
                        line.to_string()
                    } else {
                        format!("**/{}", line)
                    };
                    self.entries
                        .push((pattern, RuleKind::Exclude, 0.0));
                }
            }
        }
        self
    }

    /// Compile all patterns and produce a Ruleset.
    pub fn build(self) -> Result<Ruleset, GlobError> {
        let mut rules = Vec::with_capacity(self.entries.len());
        for (pat_str, kind, weight) in self.entries {
            let pattern = Pattern::new(&pat_str)?;
            rules.push(Rule {
                pattern,
                kind,
                weight,
            });
        }
        Ok(Ruleset {
            rules,
            match_opts: self.match_opts,
        })
    }
}

impl Ruleset {
    /// Start building a new ruleset.
    pub fn new() -> RulesetBuilder {
        RulesetBuilder {
            entries: Vec::new(),
            match_opts: MatchOptions::new(),
        }
    }

    /// Test whether a path matches (included and not excluded).
    pub fn is_match(&self, path: &str) -> bool {
        let mut dominated = false;
        let mut included = false;

        // Rules are evaluated in order. Later rules override earlier ones
        // for the same path (like .gitignore).
        for rule in &self.rules {
            if rule.pattern.matches_with(path, self.match_opts) {
                match rule.kind {
                    RuleKind::Include => {
                        included = true;
                        dominated = false;
                    }
                    RuleKind::Exclude => {
                        dominated = true;
                    }
                }
            }
        }

        included && !dominated
    }

    /// Compute a relevance score for a path.
    ///
    /// Returns 0.0 if excluded, or the sum of matching include weights.
    pub fn relevance(&self, path: &str) -> f32 {
        let mut score: f32 = 0.0;
        let mut excluded = false;

        for rule in &self.rules {
            if rule.pattern.matches_with(path, self.match_opts) {
                match rule.kind {
                    RuleKind::Include => {
                        score += rule.weight;
                        excluded = false;
                    }
                    RuleKind::Exclude => {
                        excluded = true;
                    }
                }
            }
        }

        if excluded {
            0.0
        } else {
            score
        }
    }

    /// Walk a directory tree applying this ruleset.
    ///
    /// Returns matched entries sorted by relevance (highest first),
    /// with excluded entries removed.
    pub fn walk(&self, root: &Path, opts: WalkOptions) -> Result<Vec<Entry>, GlobError> {
        // Walk with ** to get everything, then filter through rules.
        let pattern = format!("{}/**", root.display());
        let walk_opts = WalkOptions {
            match_opts: self.match_opts,
            sorted: false, // We'll sort by relevance instead.
            ..opts
        };

        let results = crate::walker::walk(&pattern, walk_opts)?;
        let mut entries: Vec<(Entry, f32)> = Vec::new();

        for result in results {
            match result {
                Ok(entry) => {
                    let path_str = entry.path.to_str().unwrap_or("");
                    let rel = self.relevance(path_str);
                    if rel > 0.0 {
                        entries.push((entry, rel));
                    }
                }
                Err(_) => continue,
            }
        }

        // Sort by relevance descending, then path ascending for stability.
        entries.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.path.cmp(&b.0.path))
        });

        Ok(entries.into_iter().map(|(e, _)| e).collect())
    }

    /// Filter a pre-existing list of entries through this ruleset.
    pub fn filter<'a>(&self, entries: &'a [Entry]) -> Vec<&'a Entry> {
        entries
            .iter()
            .filter(|e| {
                let path_str = e.path.to_str().unwrap_or("");
                self.is_match(path_str)
            })
            .collect()
    }

    /// Filter entries, returning only those of a specific FileKind.
    pub fn filter_kind<'a>(&self, entries: &'a [Entry], kind: FileKind) -> Vec<&'a Entry> {
        entries
            .iter()
            .filter(|e| {
                e.kind == kind && {
                    let path_str = e.path.to_str().unwrap_or("");
                    self.is_match(path_str)
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_include_exclude() {
        let rs = Ruleset::new()
            .include("**/*.rs")
            .exclude("**/target/**")
            .build()
            .unwrap();

        assert!(rs.is_match("src/main.rs"));
        assert!(rs.is_match("src/lib/util.rs"));
        assert!(!rs.is_match("target/debug/main.rs"));
        assert!(!rs.is_match("src/main.py"));
    }

    #[test]
    fn negation_re_includes() {
        let rs = Ruleset::new()
            .include("**/*.rs")
            .exclude("**/generated/**")
            .include("**/generated/keep.rs") // negation: re-include
            .build()
            .unwrap();

        assert!(rs.is_match("src/main.rs"));
        assert!(!rs.is_match("src/generated/nope.rs"));
        assert!(rs.is_match("src/generated/keep.rs"));
    }

    #[test]
    fn relevance_scoring() {
        let rs = Ruleset::new()
            .include_weighted("src/**/*.rs", 2.0)
            .include_weighted("tests/**/*.rs", 0.5)
            .exclude("**/target/**")
            .build()
            .unwrap();

        assert!(rs.relevance("src/core/lib.rs") > rs.relevance("tests/unit.rs"));
        assert_eq!(rs.relevance("target/debug/main.rs"), 0.0);
        assert_eq!(rs.relevance("README.md"), 0.0);
    }

    #[test]
    fn empty_ruleset_matches_nothing() {
        let rs = Ruleset::new().build().unwrap();
        assert!(!rs.is_match("anything"));
    }
}
