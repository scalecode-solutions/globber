// Integration tests — exercises the walker against a real temp filesystem.
//
// All tests use absolute paths into a tempdir to avoid the process-global
// set_current_dir race condition under parallel test execution.

use globber::{glob_with, to_sif, to_sif_with_summary, WalkOptions};
use std::fs;
use std::path::PathBuf;

fn mk(root: &std::path::Path, path: &str, is_dir: bool) {
    let full = root.join(path);
    if is_dir {
        fs::create_dir_all(&full).unwrap();
    } else {
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&full, format!("// {}", path)).unwrap();
    }
}

fn setup() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let r = dir.path();

    mk(r, "src", true);
    mk(r, "src/main.rs", false);
    mk(r, "src/lib.rs", false);
    mk(r, "src/util", true);
    mk(r, "src/util/helpers.rs", false);
    mk(r, "tests", true);
    mk(r, "tests/unit.rs", false);
    mk(r, "Cargo.toml", false);
    mk(r, "README.md", false);
    mk(r, ".gitignore", false);
    mk(r, "target", true);
    mk(r, "target/debug", true);
    mk(r, "target/debug/main", false);

    dir
}

/// Helper: glob with an absolute pattern rooted in the tempdir, strip the
/// tempdir prefix from returned paths so assertions stay readable.
fn glob_abs(root: &std::path::Path, pattern: &str) -> Vec<PathBuf> {
    let full_pattern = format!("{}/{}", root.display(), pattern);
    let results = glob_with(&full_pattern, WalkOptions::default()).unwrap();
    results
        .into_iter()
        .filter_map(|r| r.ok())
        .map(|e| e.path.strip_prefix(root).unwrap().to_path_buf())
        .collect()
}

fn glob_abs_opts(root: &std::path::Path, pattern: &str, opts: WalkOptions) -> Vec<PathBuf> {
    let full_pattern = format!("{}/{}", root.display(), pattern);
    let results = glob_with(&full_pattern, opts).unwrap();
    results
        .into_iter()
        .filter_map(|r| r.ok())
        .map(|e| e.path.strip_prefix(root).unwrap().to_path_buf())
        .collect()
}

#[test]
fn find_rust_files() {
    let dir = setup();
    let mut paths = glob_abs(dir.path(), "src/**/*.rs");
    paths.sort();
    assert_eq!(
        paths,
        vec![
            PathBuf::from("src/lib.rs"),
            PathBuf::from("src/main.rs"),
            PathBuf::from("src/util/helpers.rs"),
        ]
    );
}

#[test]
fn find_all_rs_recursive() {
    let dir = setup();
    let mut paths = glob_abs(dir.path(), "**/*.rs");
    paths.sort();
    assert!(paths.contains(&PathBuf::from("src/main.rs")));
    assert!(paths.contains(&PathBuf::from("tests/unit.rs")));
    assert!(paths.contains(&PathBuf::from("src/util/helpers.rs")));
}

#[test]
fn literal_path() {
    let dir = setup();
    let paths = glob_abs(dir.path(), "Cargo.toml");
    assert_eq!(paths, vec![PathBuf::from("Cargo.toml")]);
}

#[test]
fn question_mark() {
    let dir = setup();
    let mut paths = glob_abs(dir.path(), "src/???.rs");
    paths.sort();
    assert_eq!(paths, vec![PathBuf::from("src/lib.rs")]);
}

#[test]
fn budget_limit() {
    let dir = setup();
    let opts = WalkOptions {
        limit: 2,
        ..WalkOptions::default()
    };
    let paths = glob_abs_opts(dir.path(), "**/*.rs", opts);
    assert!(paths.len() <= 2, "limit should cap results at 2, got {}", paths.len());
}

#[test]
fn entries_have_metadata() {
    let dir = setup();
    let full_pattern = format!("{}/src/main.rs", dir.path().display());
    let results = glob_with(&full_pattern, WalkOptions::default()).unwrap();
    let entries: Vec<_> = results.into_iter().filter_map(|r| r.ok()).collect();
    assert_eq!(entries.len(), 1);
    let e = &entries[0];
    assert!(!e.is_dir);
    assert!(e.size > 0);
    assert!(e.tokens_est > 0);
    assert_eq!(e.kind, globber::FileKind::Source);
}

#[test]
fn sif_output_roundtrip() {
    let dir = setup();
    let full_pattern = format!("{}/src/**/*.rs", dir.path().display());
    let results = glob_with(&full_pattern, WalkOptions::default()).unwrap();
    let entries: Vec<_> = results.into_iter().filter_map(|r| r.ok()).collect();
    let sif = to_sif(&entries);

    assert!(sif.starts_with("#!sif v1\n"));
    assert!(sif.contains("#schema"));
    assert!(sif.contains("source"));

    let sif_full = to_sif_with_summary(&entries);
    assert!(sif_full.contains("§summary"));
    assert!(sif_full.contains("total_files"));
}

#[test]
fn dotfile_hidden_by_default_with_leading_dot_option() {
    let dir = setup();
    let opts = WalkOptions {
        match_opts: globber::MatchOptions {
            require_literal_leading_dot: true,
            ..globber::MatchOptions::new()
        },
        ..WalkOptions::default()
    };
    let paths = glob_abs_opts(dir.path(), "*", opts);
    // .gitignore should NOT appear (leading dot hidden).
    assert!(!paths.contains(&PathBuf::from(".gitignore")));
    // Cargo.toml SHOULD appear.
    assert!(paths.contains(&PathBuf::from("Cargo.toml")));
}

#[test]
fn empty_pattern() {
    let dir = setup();
    let paths = glob_abs(dir.path(), "nonexistent_file_xyz");
    assert!(paths.is_empty());
}
