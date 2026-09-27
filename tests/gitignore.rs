// Gitignore integration tests against real repository layouts.
//
// Repositories are marked with an empty `.git/` directory, which is all
// the walker needs to find the repository root.

use std::fs;
use std::path::Path;

use globber::{walk, WalkOptions};

fn write(root: &Path, rel: &str, body: &str) {
    let full = root.join(rel);
    fs::create_dir_all(full.parent().unwrap()).unwrap();
    fs::write(full, body).unwrap();
}

fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let r = dir.path();
    fs::create_dir_all(r.join(".git/info")).unwrap();
    write(r, ".gitignore", "/target\nbuild/\n*.log\n!keep.log\ndocs/gen\n");
    write(r, "src/main.rs", "x");
    write(r, "src/target/keep.rs", "x"); // /target is anchored: not ignored
    write(r, "src/build", "a file named build"); // build/ is dir-only
    write(r, "target/debug/out.rs", "x");
    write(r, "lib/build/out.rs", "x");
    write(r, "app.log", "x");
    write(r, "keep.log", "x");
    write(r, "docs/gen/api.md", "x");
    write(r, "docs/guide.md", "x");
    write(r, "sub/.gitignore", "*.tmp\n!important.log\n/local\n");
    write(r, "sub/a.tmp", "x");
    write(r, "sub/important.log", "x");
    write(r, "sub/local/x.rs", "x");
    write(r, "sub/deeper/local/y.rs", "x");
    write(r, "secret.env", "x");
    write(r, ".git/info/exclude", "*.env\n");
    dir
}

fn run(root: &Path, pattern: &str) -> Vec<String> {
    let opts = WalkOptions { gitignore: true, ..WalkOptions::default() };
    walk(&format!("{}/{}", root.display(), pattern), opts)
        .unwrap()
        .into_iter()
        .filter_map(|r| r.ok())
        .map(|e| e.path.strip_prefix(root).unwrap().to_string_lossy().into_owned())
        .collect()
}

#[test]
fn repo_root_walk() {
    let dir = repo();
    let got = run(dir.path(), "**");
    let expected = [
        "app.log",
        "docs",
        "docs/guide.md",
        "keep.log",
        "lib",
        "secret.env",
        "src",
        "src/build",
        "src/main.rs",
        "src/target",
        "src/target/keep.rs",
        "sub",
        "sub/a.tmp",
        "sub/deeper",
        "sub/deeper/local",
        "sub/deeper/local/y.rs",
        "sub/important.log",
    ];
    // The walk runs from "/" through literal components, so the repo root
    // is discovered on the way down and its .git/info/exclude applies.
    let not_expected = ["app.log", "secret.env", "sub/a.tmp"];
    for p in expected.iter().filter(|p| !not_expected.contains(p)) {
        assert!(got.contains(&p.to_string()), "missing {} in {:?}", p, got);
    }
    for p in not_expected {
        assert!(!got.contains(&p.to_string()), "{} should be ignored: {:?}", p, got);
    }
    for p in ["target", "lib/build", "docs/gen", "sub/local", ".git"] {
        assert!(!got.iter().any(|g| g == p || g.starts_with(&format!("{}/", p))), "{} leaked: {:?}", p, got);
    }
}

#[test]
fn nested_gitignore_negation_overrides_parent() {
    let dir = repo();
    let got = run(dir.path(), "sub/*.log");
    assert_eq!(got, vec!["sub/important.log"]);
}

#[test]
fn literal_prefix_is_never_ignored() {
    // Naming an ignored directory explicitly still walks it.
    let dir = repo();
    let got = run(dir.path(), "target/**/*.rs");
    assert_eq!(got, vec!["target/debug/out.rs"]);
}

#[test]
fn walk_rooted_in_subdirectory_honors_parent_rules() {
    // Simulates `globber -g -r <repo>/lib '**'`: the repo root's
    // `build/` rule must still apply.
    let dir = repo();
    let lib = dir.path().join("lib");
    let opts = WalkOptions { gitignore: true, ..WalkOptions::default() };
    // Relative walks resolve against the cwd, so emulate by walking the
    // absolute path; the parent rules come from the repo above it.
    let got: Vec<String> = walk(&format!("{}/**", lib.display()), opts)
        .unwrap()
        .into_iter()
        .filter_map(|r| r.ok())
        .map(|e| e.path.strip_prefix(&lib).unwrap().to_string_lossy().into_owned())
        .collect();
    assert!(got.is_empty(), "{:?}", got);
}

#[test]
fn no_repo_still_reads_gitignore_files() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), ".gitignore", "*.o\n");
    write(dir.path(), "a.o", "x");
    write(dir.path(), "a.c", "x");
    assert_eq!(run(dir.path(), "*"), vec![".gitignore", "a.c"]);
}

#[test]
fn gitignore_off_sees_everything() {
    let dir = repo();
    let got: Vec<_> = walk(&format!("{}/*.log", dir.path().display()), WalkOptions::default())
        .unwrap()
        .into_iter()
        .filter_map(|r| r.ok())
        .collect();
    assert_eq!(got.len(), 2);
}

#[test]
fn pathological_gitignore_lines() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join(".git")).unwrap();
    write(
        dir.path(),
        ".gitignore",
        "\n\n#comment\n!\n/\na**b\n[unclosed\n   \n\\#hash\nok.txt\n.gitignore\n",
    );
    write(dir.path(), "ok.txt", "x");
    write(dir.path(), "#hash", "x");
    write(dir.path(), "aXYb", "x"); // a**b behaves as a*b
    write(dir.path(), "fine.rs", "x");
    assert_eq!(run(dir.path(), "*"), vec!["fine.rs"]);
}
