// Walker engine tests — happy paths and adversarial trees.
//
// All tests use absolute paths into a tempdir to avoid the process-global
// set_current_dir race under parallel test execution.

use std::fs;
use std::path::Path;

use globber::{walk_many, BudgetMode, EntryFilter, FileKind, MatchOptions, Pattern, WalkOptions};

fn write(root: &Path, rel: &str, body: &str) {
    let full = root.join(rel);
    fs::create_dir_all(full.parent().unwrap()).unwrap();
    fs::write(full, body).unwrap();
}

fn tree() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let r = dir.path();
    write(r, "src/main.rs", "fn main() {}\n");
    write(r, "src/lib.rs", "pub fn lib() {}\n");
    write(r, "src/util/helpers.rs", "pub fn h() {}\n");
    write(r, "src/util/deep/nested.rs", "pub fn n() {}\n");
    write(r, "tests/unit.rs", "#[test] fn t() {}\n");
    write(r, "docs/guide.md", "# Guide\n");
    write(r, "docs/api.md", "# API\n");
    write(r, "Cargo.toml", "[package]\n");
    write(r, "README.md", "# Readme\n");
    write(r, ".hidden/secret.rs", "fn s() {}\n");
    write(r, "target/debug/build.rs", "fn b() {}\n");
    dir
}

/// Walk patterns rooted at `root`, returning root-relative paths.
fn run(root: &Path, patterns: &[&str], opts: WalkOptions) -> Vec<String> {
    let full: Vec<String> = patterns
        .iter()
        .map(|p| format!("{}/{}", root.display(), p))
        .collect();
    walk_many(&full, opts)
        .unwrap()
        .into_iter()
        .filter_map(|r| r.ok())
        .map(|e| {
            e.path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect()
}

fn hidden_off() -> WalkOptions {
    WalkOptions {
        match_opts: MatchOptions {
            require_literal_leading_dot: true,
            ..MatchOptions::new()
        },
        ..WalkOptions::default()
    }
}

// ── Happy paths ─────────────────────────────────────────────────────

#[test]
fn recursive_suffix_pattern() {
    let dir = tree();
    let got = run(dir.path(), &["src/**/*.rs"], WalkOptions::default());
    assert_eq!(
        got,
        vec![
            "src/lib.rs",
            "src/main.rs",
            "src/util/deep/nested.rs",
            "src/util/helpers.rs",
        ]
    );
}

#[test]
fn results_are_globally_sorted() {
    let dir = tree();
    let got = run(dir.path(), &["**"], WalkOptions::default());
    let mut sorted = got.clone();
    sorted.sort_by(|a, b| Path::new(a).cmp(Path::new(b)));
    assert_eq!(got, sorted);
}

#[test]
fn trailing_recursive_yields_contents_not_prefix() {
    let dir = tree();
    let got = run(dir.path(), &["docs/**"], WalkOptions::default());
    assert_eq!(got, vec!["docs/api.md", "docs/guide.md"]);
}

#[test]
fn recursive_matches_zero_components() {
    let dir = tree();
    let got = run(dir.path(), &["src/**/main.rs"], WalkOptions::default());
    assert_eq!(got, vec!["src/main.rs"]);
}

#[test]
fn middle_recursive_with_multi_component_tail() {
    let dir = tree();
    let got = run(dir.path(), &["**/util/*/*.rs"], WalkOptions::default());
    assert_eq!(got, vec!["src/util/deep/nested.rs"]);
}

#[test]
fn trailing_slash_matches_only_dirs() {
    let dir = tree();
    let got = run(dir.path(), &["src/*/"], WalkOptions::default());
    assert_eq!(got, vec!["src/util"]);
}

#[test]
fn only_dirs_option() {
    let dir = tree();
    let opts = WalkOptions { only_dirs: true, ..hidden_off() };
    let got = run(dir.path(), &["**"], opts);
    assert_eq!(
        got,
        vec!["docs", "src", "src/util", "src/util/deep", "target", "target/debug", "tests"]
    );
}

#[test]
fn dot_components_are_normalized() {
    let dir = tree();
    let a = run(dir.path(), &["./src/./*.rs"], WalkOptions::default());
    let b = run(dir.path(), &["src/*.rs"], WalkOptions::default());
    assert_eq!(a, b);
}

#[test]
fn hidden_entries_skipped_unless_named() {
    let dir = tree();
    let got = run(dir.path(), &["**/*.rs"], hidden_off());
    assert!(!got.iter().any(|p| p.contains(".hidden")));
    // A literal dot component is always allowed.
    let got = run(dir.path(), &[".hidden/*.rs"], hidden_off());
    assert_eq!(got, vec![".hidden/secret.rs"]);
    // So is a wildcard that starts with a literal dot.
    let got = run(dir.path(), &[".hid*/*.rs"], hidden_off());
    assert_eq!(got, vec![".hidden/secret.rs"]);
}

#[test]
fn escaped_literal_component() {
    let dir = tree();
    write(dir.path(), "odd/a*b.txt", "x");
    write(dir.path(), "odd/axb.txt", "x");
    let got = run(dir.path(), &["odd/a\\*b.txt"], WalkOptions::default());
    assert_eq!(got, vec!["odd/a*b.txt"]);
}

// ── Multiple patterns and braces share one walk ─────────────────────

#[test]
fn overlapping_patterns_are_deduplicated() {
    let dir = tree();
    let got = run(
        dir.path(),
        &["**/*.rs", "src/*.rs", "./src/main.rs"],
        hidden_off(),
    );
    let mut unique = got.clone();
    unique.dedup();
    assert_eq!(got, unique, "duplicates in {:?}", got);
    assert_eq!(got.iter().filter(|p| *p == "src/main.rs").count(), 1);
}

#[test]
fn brace_alternatives_single_pass() {
    let dir = tree();
    let got = run(dir.path(), &["**/*.{md,toml}"], hidden_off());
    assert_eq!(got, vec!["Cargo.toml", "README.md", "docs/api.md", "docs/guide.md"]);
}

#[test]
fn overlapping_brace_alternatives_deduplicated() {
    let dir = tree();
    let got = run(dir.path(), &["{src,src/util}/**/*.rs"], hidden_off());
    assert_eq!(got.iter().filter(|p| *p == "src/util/helpers.rs").count(), 1);
}

#[test]
fn brace_explosion_is_rejected() {
    let pat = "{a,b}".repeat(20); // 2^20 alternatives
    let err = walk_many(&[pat], WalkOptions::default()).unwrap_err();
    assert!(err.to_string().contains("too many"), "{}", err);
}

// ── Depth ───────────────────────────────────────────────────────────

#[test]
fn depth_one_is_immediate_children() {
    let dir = tree();
    let opts = WalkOptions { max_depth: Some(1), ..hidden_off() };
    let got = run(dir.path(), &["**"], opts);
    assert_eq!(got, vec!["Cargo.toml", "README.md", "docs", "src", "target", "tests"]);
}

#[test]
fn depth_counts_from_first_wildcard() {
    let dir = tree();
    let opts = WalkOptions { max_depth: Some(1), ..hidden_off() };
    let got = run(dir.path(), &["src/**/*.rs"], opts);
    assert_eq!(got, vec!["src/lib.rs", "src/main.rs"]);
}

#[test]
fn parallel_and_sequential_walks_agree() {
    let dir = tree();
    for depth in [None, Some(1), Some(2), Some(3)] {
        for pat in ["**", "**/*.rs", "src/**", "*/**/*.md", "**/util/**"] {
            let par = run(dir.path(), &[pat], WalkOptions { max_depth: depth, ..hidden_off() });
            let seq = run(
                dir.path(),
                &[pat],
                WalkOptions { max_depth: depth, limit: Some(usize::MAX), ..hidden_off() },
            );
            assert_eq!(par, seq, "pattern {} depth {:?}", pat, depth);
        }
    }
}

// ── Excludes and filters run before limits ──────────────────────────

#[test]
fn exclude_by_name_and_by_path() {
    let dir = tree();
    let root = dir.path().display().to_string();
    let opts = WalkOptions {
        exclude: vec![
            Pattern::new("*.md").unwrap(),
            Pattern::new(&format!("{}/target/**", root)).unwrap(),
        ],
        ..hidden_off()
    };
    let got = run(dir.path(), &["**"], opts);
    assert!(!got.iter().any(|p| p.ends_with(".md")));
    assert!(!got.iter().any(|p| p.starts_with("target")), "{:?}", got);
    assert!(got.contains(&"src/main.rs".to_string()));
}

#[test]
fn excluded_dir_is_pruned() {
    let dir = tree();
    let opts = WalkOptions {
        exclude: vec![Pattern::new("util").unwrap()],
        ..hidden_off()
    };
    let got = run(dir.path(), &["src/**"], opts);
    assert_eq!(got, vec!["src/lib.rs", "src/main.rs"]);
}

#[test]
fn filter_applies_before_limit() {
    let dir = tree();
    let opts = WalkOptions {
        limit: Some(2),
        filter: Some(EntryFilter::new(|e| e.kind == FileKind::Source)),
        ..hidden_off()
    };
    // Cargo.toml, README.md and docs/ sort first; they must not use up the limit.
    let got = run(dir.path(), &["**"], opts);
    assert_eq!(got, vec!["src/lib.rs", "src/main.rs"]);
}

#[test]
fn exclude_applies_before_limit() {
    let dir = tree();
    let opts = WalkOptions {
        limit: Some(1),
        exclude: vec![Pattern::new("*.toml").unwrap(), Pattern::new("*.md").unwrap()],
        ..hidden_off()
    };
    let got = run(dir.path(), &["*"], opts);
    assert_eq!(got, vec!["docs"]);
}

// ── Budgets ─────────────────────────────────────────────────────────

fn budget_tree() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "a_big.txt", &"x".repeat(7000)); // 2000 tokens
    write(dir.path(), "b.txt", &"x".repeat(35)); // 10 tokens
    write(dir.path(), "c.txt", &"x".repeat(35));
    dir
}

#[test]
fn budget_stop_mode_stops_at_first_overrun() {
    let dir = budget_tree();
    let opts = WalkOptions { token_budget: Some(100), ..WalkOptions::default() };
    assert!(run(dir.path(), &["*"], opts).is_empty());
}

#[test]
fn budget_fit_mode_skips_oversized_entries() {
    let dir = budget_tree();
    let opts = WalkOptions {
        token_budget: Some(100),
        budget_mode: BudgetMode::Fit,
        ..WalkOptions::default()
    };
    assert_eq!(run(dir.path(), &["*"], opts), vec!["b.txt", "c.txt"]);
}

#[test]
fn byte_budget_is_exact() {
    let dir = budget_tree();
    let opts = WalkOptions {
        byte_budget: Some(70),
        budget_mode: BudgetMode::Fit,
        ..WalkOptions::default()
    };
    assert_eq!(run(dir.path(), &["*"], opts), vec!["b.txt", "c.txt"]);
}

#[test]
fn limit_is_exact_and_sorted() {
    let dir = tree();
    let opts = WalkOptions { limit: Some(3), ..hidden_off() };
    let got = run(dir.path(), &["**/*.rs"], opts);
    assert_eq!(got, vec!["src/lib.rs", "src/main.rs", "src/util/deep/nested.rs"]);
}

// ── Adversarial ─────────────────────────────────────────────────────

#[cfg(unix)]
#[test]
fn symlink_cycle_not_followed_by_default() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "l/x/file.txt", "x");
    std::os::unix::fs::symlink("..", dir.path().join("l/x/up")).unwrap();
    let got = run(dir.path(), &["l/**"], WalkOptions::default());
    assert_eq!(got, vec!["l/x", "l/x/file.txt", "l/x/up"]);
}

#[cfg(unix)]
#[test]
fn symlink_cycle_detected_when_following() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "l/x/file.txt", "x");
    write(dir.path(), "other/o.txt", "x");
    std::os::unix::fs::symlink("..", dir.path().join("l/x/up")).unwrap();
    std::os::unix::fs::symlink("../other", dir.path().join("l/ext")).unwrap();
    let opts = WalkOptions { follow_symlinks: true, ..WalkOptions::default() };
    let got = run(dir.path(), &["l/**"], opts);
    // The external link is followed; the cycle back to l/ is not.
    assert_eq!(got, vec!["l/ext", "l/ext/o.txt", "l/x", "l/x/file.txt", "l/x/up"]);
}

#[cfg(unix)]
#[test]
fn literal_symlink_in_pattern_is_followed() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "real/a.txt", "x");
    std::os::unix::fs::symlink("real", dir.path().join("link")).unwrap();
    let got = run(dir.path(), &["link/*.txt"], WalkOptions::default());
    assert_eq!(got, vec!["link/a.txt"]);
}

#[test]
fn deep_tree_does_not_blow_up() {
    let dir = tempfile::tempdir().unwrap();
    let deep = (0..60).map(|i| format!("d{}", i)).collect::<Vec<_>>().join("/");
    write(dir.path(), &format!("{}/leaf.rs", deep), "x");
    let got = run(dir.path(), &["**/leaf.rs"], WalkOptions::default());
    assert_eq!(got.len(), 1);
}

#[test]
fn many_patterns_one_walk() {
    let dir = tree();
    let pats: Vec<String> = (0..500).map(|i| format!("**/nomatch{}.rs", i)).chain(["**/main.rs".to_string()]).collect();
    let refs: Vec<&str> = pats.iter().map(|s| s.as_str()).collect();
    assert_eq!(run(dir.path(), &refs, WalkOptions::default()), vec!["src/main.rs"]);
}

#[test]
fn weird_filenames() {
    let dir = tempfile::tempdir().unwrap();
    for name in ["sp ace.rs", "tab\there.rs", "uni-日本.rs", "[brackets].rs", "{braces}.rs", "star*.rs"] {
        write(dir.path(), name, "x");
    }
    let got = run(dir.path(), &["*.rs"], WalkOptions::default());
    assert_eq!(got.len(), 6, "{:?}", got);
}

#[test]
fn missing_root_yields_nothing() {
    let dir = tree();
    assert!(run(dir.path(), &["nope/**/*.rs"], WalkOptions::default()).is_empty());
}

#[test]
fn invalid_pattern_is_an_error() {
    assert!(walk_many(&["a**b/*.rs"], WalkOptions::default()).is_err());
    assert!(walk_many(&["src/[abc"], WalkOptions::default()).is_err());
}

#[test]
fn case_insensitive_walk_skips_literal_fast_path() {
    let dir = tree();
    let opts = WalkOptions {
        match_opts: MatchOptions { case_sensitive: false, ..MatchOptions::new() },
        ..WalkOptions::default()
    };
    let got = run(dir.path(), &["SRC/MAIN.RS"], opts);
    assert_eq!(got, vec!["src/main.rs"]);
}

#[test]
fn report_lists_fit_skips_and_stop_reason() {
    let dir = budget_tree();
    let pat = format!("{}/*", dir.path().display());
    let opts = WalkOptions {
        token_budget: Some(100),
        budget_mode: BudgetMode::Fit,
        ..WalkOptions::default()
    };
    let report = globber::walk_many_report(&[&pat], opts).unwrap();
    assert_eq!(report.results.len(), 2);
    assert_eq!(report.budget_skipped.len(), 1);
    assert!(report.budget_skipped[0].path.ends_with("a_big.txt"));
    assert_eq!(report.stopped_early, None);

    let opts = WalkOptions { token_budget: Some(100), ..WalkOptions::default() };
    let report = globber::walk_many_report(&[&pat], opts).unwrap();
    assert_eq!(report.stopped_early, Some(globber::StopReason::TokenBudget));
    assert!(report.budget_skipped.is_empty());
}

// macOS (APFS) refuses non-UTF-8 names, so this only runs on Linux.
#[cfg(target_os = "linux")]
#[test]
fn non_utf8_names_are_listed_and_counted() {
    use std::os::unix::ffi::OsStrExt;
    let dir = tempfile::tempdir().unwrap();
    let bad = std::ffi::OsStr::from_bytes(b"bad\xff.rs");
    fs::write(dir.path().join(bad), "x").unwrap();
    fs::write(dir.path().join("good.rs"), "x").unwrap();
    let pat = format!("{}/*.rs", dir.path().display());
    let report = globber::walk_many_report(&[&pat], WalkOptions::default()).unwrap();
    let names: Vec<_> = report
        .results
        .iter()
        .filter_map(|r| r.as_ref().ok())
        .map(|e| e.path.file_name().unwrap().to_os_string())
        .collect();
    assert_eq!(names, vec![bad.to_os_string(), "good.rs".into()]);
    assert_eq!(report.pruned.non_utf8_names, 1);
}
