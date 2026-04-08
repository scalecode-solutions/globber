//! `globber` — AI-native glob for the SIF ecosystem.
//!
//! A ground-up Rust rewrite of Unix glob, rooted in the POSIX glob(3)/fnmatch(3)
//! specification, enhanced for AI agent workloads.
//!
//! # What's different from `glob`
//!
//! | Feature | `glob` (rust-lang) | `globber` |
//! |---------|--------------------|-----------|
//! | Matching engine | Recursive backtracking O(2^n) | NFA simulation O(n*m) |
//! | Output type | `PathBuf` | `Entry` (path + size + kind + tokens_est) |
//! | Output format | Rust iterator | Rust types or SIF document |
//! | Pattern count | Single | Single or `Ruleset` (multi-pattern) |
//! | Negation | None | `Ruleset::exclude()` / `!` in gitignore |
//! | Brace expansion | None | `{a,b,c}` |
//! | Budget control | None | Byte budget, token budget, result limit |
//! | File classification | None | `FileKind` (source, test, config, generated, ...) |
//! | Backslash escape | None | `\*` escapes metacharacters (POSIX) |
//! | Dependencies | 0 | 0 |
//!
//! # Quick start
//!
//! ## Single pattern (POSIX glob equivalent)
//!
//! ```no_run
//! use globber::glob;
//!
//! for entry in glob("src/**/*.rs").unwrap() {
//!     match entry {
//!         Ok(e) => println!("{} ({} bytes, ~{} tokens)",
//!             e.path.display(), e.size, e.tokens_est),
//!         Err(e) => eprintln!("{}", e),
//!     }
//! }
//! ```
//!
//! ## Pure pattern matching (POSIX fnmatch equivalent)
//!
//! ```
//! use globber::Pattern;
//!
//! let pat = Pattern::new("*.rs").unwrap();
//! assert!(pat.matches("main.rs"));
//! assert!(!pat.matches("main.py"));
//! ```
//!
//! ## Multi-pattern ruleset (AI-native)
//!
//! ```no_run
//! use globber::Ruleset;
//!
//! let rules = Ruleset::new()
//!     .include("src/**/*.rs")
//!     .include("lib/**/*.rs")
//!     .exclude("**/generated/**")
//!     .exclude("**/target/**")
//!     .build()
//!     .unwrap();
//!
//! assert!(rules.is_match("src/main.rs"));
//! assert!(!rules.is_match("target/debug/main.rs"));
//! ```
//!
//! ## Budget-aware walking
//!
//! ```no_run
//! use globber::{glob_with, WalkOptions};
//!
//! let opts = WalkOptions {
//!     token_budget: 80_000,   // Stop when ~80K tokens matched
//!     limit: 100,             // Or after 100 files
//!     ..WalkOptions::default()
//! };
//! let results = glob_with("src/**/*.rs", opts).unwrap();
//! ```
//!
//! ## SIF output
//!
//! ```no_run
//! let results = globber::glob("src/**/*.rs").unwrap();
//! let entries: Vec<_> = results.into_iter().filter_map(|r| r.ok()).collect();
//! let sif = globber::to_sif(&entries);
//! println!("{}", sif);
//! // #!sif v1
//! // #schema path:str:path	size:uint	kind:str:311	tokens_est:uint	is_dir:bool
//! // src/main.rs	1024	source	293	false
//! // ...
//! ```
//!
//! ## Brace expansion
//!
//! ```
//! use globber::expand_braces;
//!
//! let patterns = expand_braces("src/{lib,main}.rs");
//! assert_eq!(patterns, vec!["src/lib.rs", "src/main.rs"]);
//! ```

// ── Modules ──────────────────────────────────────────────────────────

pub mod error;
pub mod pattern;
pub mod matcher;
pub mod entry;
pub mod walker;
pub mod ruleset;
pub mod sif_output;
pub mod git;

// ── Re-exports ───────────────────────────────────────────────────────

pub use error::{GlobError, PatternError, PatternErrorKind};
pub use pattern::{Pattern, expand_braces};
pub use matcher::MatchOptions;
pub use entry::{Entry, FileKind};
pub use walker::{WalkOptions, WalkResult, walk};
pub use ruleset::Ruleset;
pub use sif_output::{to_sif, to_sif_with_summary, to_sif_with_summary_and_budget, to_paths, write_preview, BudgetInfo, PreviewMode};

// ── Convenience functions ────────────────────────────────────────────

/// Walk the filesystem matching a glob pattern.
///
/// This is the primary entry point — the equivalent of POSIX `glob(3)`.
/// Returns a vector of results (entries or errors) in sorted order.
///
/// For more control, use [`glob_with`] or [`walker::walk`] directly.
pub fn glob(pattern: &str) -> Result<Vec<WalkResult>, GlobError> {
    walk(pattern, WalkOptions::default())
}

/// Walk the filesystem matching a glob pattern with custom options.
pub fn glob_with(pattern: &str, opts: WalkOptions) -> Result<Vec<WalkResult>, GlobError> {
    walk(pattern, opts)
}
