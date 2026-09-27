// CLI integration tests — run the real binary against temp trees.
//
// Each test runs the binary with its working directory set to a fresh
// tempdir, so relative patterns behave exactly as they do for users.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn write(root: &Path, rel: &str, body: &str) {
    let full = root.join(rel);
    fs::create_dir_all(full.parent().unwrap()).unwrap();
    fs::write(full, body).unwrap();
}

fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let r = dir.path();
    write(r, "src/main.rs", "// Header comment\n\nuse std::env;\nfn main() {}\n");
    write(r, "src/lib.rs", "//! Crate docs\n#![allow(dead_code)]\npub fn lib() {}\n");
    write(r, "src/util/helpers.rs", "pub fn h() {}\n");
    write(r, "docs/guide.md", "# Guide\n");
    write(r, "docs/build/out.rs", "fn o() {}\n");
    write(r, "Cargo.toml", "[package]\nname = \"x\"\n");
    write(r, "README.md", "# Readme\n");
    write(r, "notes.txt", "notes\n");
    write(r, ".gitignore", "/target\ndocs/build/\n");
    write(r, "target/debug/gen.rs", "x\n");
    write(r, "src/target/keep.rs", "fn k() {}\n");
    dir
}

fn globber(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_globber"))
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn lines(o: &Output) -> Vec<String> {
    stdout(o).lines().map(String::from).collect()
}

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
        .status
        .success();
    assert!(ok, "git {:?} failed", args);
}

// ── Happy paths ─────────────────────────────────────────────────────

#[test]
fn plain_paths_have_no_dot_prefix() {
    let dir = project();
    let out = globber(dir.path(), &["src/**/*.rs", "-p"]);
    assert!(out.status.success());
    assert_eq!(
        lines(&out),
        vec!["src/lib.rs", "src/main.rs", "src/target/keep.rs", "src/util/helpers.rs"]
    );
    // Same result regardless of spelling.
    assert_eq!(lines(&globber(dir.path(), &["./src/**/*.rs", "-p"])), lines(&out));
    assert_eq!(lines(&globber(dir.path(), &["src/**/*.rs", "-r", ".", "-p"])), lines(&out));
}

#[test]
fn root_prefixes_output_paths() {
    let dir = project();
    let out = globber(dir.path(), &["*.rs", "-r", "src/", "-p"]);
    assert_eq!(lines(&out), vec!["src/lib.rs", "src/main.rs"]);
}

#[test]
fn sif_document_shape() {
    let dir = project();
    let out = stdout(&globber(dir.path(), &["Cargo.toml"]));
    let l: Vec<&str> = out.lines().collect();
    assert_eq!(l[0], "#!sif v1");
    assert!(l[2].starts_with("#schema path:str:path size:uint kind:enum("));
    assert!(!l[2].contains('\t'), "schema must be space-separated");
    assert_eq!(l[3], "#sort path");
    assert!(l[4].starts_with("Cargo.toml\t"));
    assert_eq!(l[4].split('\t').count(), 5);
}

#[test]
fn summary_section() {
    let dir = project();
    let out = stdout(&globber(dir.path(), &["**/*.md", "-S", "-t", "1M"]));
    assert!(out.contains("---\n§summary\n"));
    assert!(out.contains("total_files\t2\n"));
    assert!(out.contains("token_budget\t1000000\n"));
    assert!(out.contains("kind_doc\t2\n"));
}

#[test]
fn preview_code_skips_preamble() {
    let dir = project();
    let out = stdout(&globber(dir.path(), &["src/*.rs", "-P", "code:2"]));
    assert!(out.contains("#block code language=rust file=src/lib.rs lines=2-3\n#![allow(dead_code)]\npub fn lib() {}\n#/block"), "{}", out);
    assert!(out.contains("#block code language=rust file=src/main.rs lines=3-4\nuse std::env;\nfn main() {}\n#/block"), "{}", out);
}

#[test]
fn preview_head_and_range() {
    let dir = project();
    let out = stdout(&globber(dir.path(), &["src/main.rs", "-P", "1"]));
    assert!(out.contains("lines=1-1\n// Header comment\n#/block"));
    let out = stdout(&globber(dir.path(), &["src/main.rs", "-P", "3-99"]));
    assert!(out.contains("lines=3-4\nuse std::env;\nfn main() {}\n#/block"));
}

#[test]
fn excludes_by_name_and_path() {
    let dir = project();
    let out = globber(dir.path(), &["**/*.rs", "-e", "target", "-e", "docs/**", "-p"]);
    assert_eq!(lines(&out), vec!["src/lib.rs", "src/main.rs", "src/util/helpers.rs"]);
    let out = globber(dir.path(), &["**", "-e", "*.{md,txt,toml}", "-e", "src", "-e", "target", "-p"]);
    assert_eq!(lines(&out), vec!["docs", "docs/build", "docs/build/out.rs"]);
}

#[test]
fn gitignore_anchoring() {
    let dir = project();
    let out = globber(dir.path(), &["**/*.rs", "-g", "-p"]);
    assert_eq!(
        lines(&out),
        vec!["src/lib.rs", "src/main.rs", "src/target/keep.rs", "src/util/helpers.rs"]
    );
}

#[test]
fn kind_filter_before_limit() {
    let dir = project();
    let out = globber(dir.path(), &["**", "-k", "source", "-n", "2", "-p"]);
    assert_eq!(lines(&out), vec!["src/lib.rs", "src/main.rs"]);
}

#[test]
fn depth_limits() {
    let dir = project();
    let out = globber(dir.path(), &["**", "--depth", "1", "-p"]);
    assert_eq!(
        lines(&out),
        vec!["Cargo.toml", "README.md", "docs", "notes.txt", "src", "target"]
    );
    let out = globber(dir.path(), &["src/**/*.rs", "--depth=1", "-p"]);
    assert_eq!(lines(&out), vec!["src/lib.rs", "src/main.rs"]);
}

#[test]
fn braces_single_walk_no_duplicates() {
    let dir = project();
    let out = globber(dir.path(), &["{src,src/util}/**/*.rs", "src/*.rs", "-p"]);
    let l = lines(&out);
    let mut dedup = l.clone();
    dedup.dedup();
    assert_eq!(l, dedup);
    assert_eq!(l.len(), 4);
}

#[test]
fn budget_fit_packs() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "a.txt", &"x".repeat(7000));
    write(dir.path(), "b.txt", "small");
    assert!(lines(&globber(dir.path(), &["*.txt", "-t", "100", "-p"])).is_empty());
    assert_eq!(lines(&globber(dir.path(), &["*.txt", "-t", "100", "--fit", "-p"])), vec!["b.txt"]);
}

#[test]
fn unlimited_overrides_earlier_limit() {
    let dir = project();
    let out = globber(dir.path(), &["**/*.md", "-n", "1", "-n", "unlimited", "-p"]);
    assert_eq!(lines(&out).len(), 2);
}

#[test]
fn double_dash_ends_options() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "-odd.rs", "x");
    assert_eq!(lines(&globber(dir.path(), &["-p", "--", "-odd.rs"])), vec!["-odd.rs"]);
}

#[test]
fn match_and_expand_subcommands() {
    let dir = project();
    let out = globber(dir.path(), &["match", "*.rs", "main.rs", "lib.py", "src/x.rs"]);
    assert_eq!(lines(&out), vec!["main.rs", "src/x.rs"]);
    let out = globber(dir.path(), &["match", "--pathname", "*.rs", "src/x.rs"]);
    assert_eq!(out.status.code(), Some(1));
    let out = globber(dir.path(), &["expand", "a/{b,c{1,2}}"]);
    assert_eq!(lines(&out), vec!["a/b", "a/c1", "a/c2"]);
}

#[test]
fn help_goes_to_stdout() {
    let dir = project();
    for args in [&["--help"][..], &["help", "match"], &["match", "--help"], &["expand", "--help"], &[]] {
        let out = globber(dir.path(), args);
        assert!(out.status.success(), "{:?}", args);
        assert!(!stdout(&out).is_empty(), "{:?} printed nothing to stdout", args);
    }
}

#[cfg(unix)]
#[test]
fn follow_flag_controls_symlinked_dirs() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "real/a.rs", "x");
    write(dir.path(), "tree/b.rs", "x");
    std::os::unix::fs::symlink("../real", dir.path().join("tree/link")).unwrap();
    std::os::unix::fs::symlink("..", dir.path().join("tree/loop")).unwrap();
    assert_eq!(lines(&globber(dir.path(), &["tree/**/*.rs", "-p"])), vec!["tree/b.rs"]);
    let out = globber(dir.path(), &["tree/**/*.rs", "-L", "-p"]);
    assert_eq!(lines(&out), vec!["tree/b.rs", "tree/link/a.rs"]);
}

#[test]
fn git_changed_since_ref() {
    let dir = project();
    let d = dir.path();
    git(d, &["init", "-q", "-b", "main"]);
    git(d, &["add", "-A"]);
    git(d, &["commit", "-qm", "init"]);
    git(d, &["checkout", "-qb", "feature"]);
    write(d, "src/main.rs", "fn main() { changed(); }\n");
    git(d, &["commit", "-qam", "change"]);
    write(d, "src/new.rs", "fn n() {}\n");
    write(d, "src/lib.rs", "pub fn lib() { uncommitted(); }\n");

    let out = globber(d, &["**/*.rs", "-G", "main", "-p"]);
    assert_eq!(lines(&out), vec!["src/lib.rs", "src/main.rs", "src/new.rs"]);
    let out = globber(d, &["**/*.rs", "-G", "-p"]);
    assert_eq!(lines(&out), vec!["src/lib.rs", "src/new.rs"]);
    // A glob after -G is a pattern, not a ref.
    let out = globber(d, &["-G", "**/*.rs", "-p"]);
    assert_eq!(lines(&out), vec!["src/lib.rs", "src/new.rs"]);
    let out = globber(d, &["**/*.rs", "--git-changed=main", "-n", "1", "-p"]);
    assert_eq!(lines(&out), vec!["src/lib.rs"]);

    // Commits on main after the fork don't show up (merge-base semantics).
    git(d, &["stash", "-u", "-q"]);
    git(d, &["checkout", "-q", "main"]);
    write(d, "README.md", "# changed on main\n");
    git(d, &["commit", "-qam", "main moves"]);
    git(d, &["checkout", "-q", "feature"]);
    let out = globber(d, &["**", "-G", "main", "-p"]);
    assert_eq!(lines(&out), vec!["src/main.rs"]);
}

// ── Adversarial ─────────────────────────────────────────────────────

#[test]
fn zero_is_rejected_everywhere() {
    let dir = project();
    for args in [
        &["**", "-n", "0"][..],
        &["**", "-t", "0"],
        &["**", "--byte-budget", "0K"],
        &["**", "--depth", "0"],
        &["**", "-P", "0"],
        &["**", "-P", "code:0"],
    ] {
        let out = globber(dir.path(), args);
        assert_eq!(out.status.code(), Some(2), "{:?} should be a usage error", args);
        assert!(stdout(&out).is_empty());
    }
    assert!(stderr(&globber(dir.path(), &["**", "-n", "0"])).contains("unlimited"));
}

#[test]
fn bad_values_are_usage_errors() {
    let dir = project();
    for args in [
        &["**", "-n", "-5"][..],
        &["**", "-n", "abc"],
        &["**", "-t", "99999999999999999999G"],
        &["**", "-k", "sauce"],
        &["**", "-P", "5-2"],
        &["**", "--paths=yes"],
        &["**", "--bogus"],
        &["**", "-n"],
    ] {
        let out = globber(dir.path(), args);
        assert_eq!(out.status.code(), Some(2), "{:?}", args);
    }
    assert_eq!(globber(dir.path(), &["-p"]).status.code(), Some(2)); // no pattern
}

#[test]
fn bad_patterns_are_errors_not_panics() {
    let dir = project();
    for p in ["a**b", "[unclosed", "***", "[[:nope:]]", "src/[a/b].rs", &"{a,b}".repeat(20)] {
        let out = globber(dir.path(), &[p]);
        assert_eq!(out.status.code(), Some(1), "pattern {:?}", p);
        assert!(stderr(&out).starts_with("error:"), "{:?}: {}", p, stderr(&out));
    }
}

#[test]
fn unknown_git_ref_is_an_error() {
    let dir = project();
    git(dir.path(), &["init", "-q"]);
    let out = globber(dir.path(), &["**", "-G", "no-such-ref"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("unknown git ref"));
}

#[test]
fn ignored_flags_warn() {
    let dir = project();
    let out = globber(dir.path(), &["*.md", "-p", "-S", "-P", "3"]);
    assert!(out.status.success());
    assert!(stderr(&out).contains("--summary has no effect"));
    assert!(stderr(&out).contains("--preview has no effect"));
}

#[test]
fn awkward_paths_are_quoted_in_sif() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "a,b.rs", "x");
    write(dir.path(), "{x}.rs", "x");
    write(dir.path(), "plain.rs", "x");
    let out = stdout(&globber(dir.path(), &["*.rs"]));
    assert!(out.contains("\n\"a,b.rs\"\t"));
    assert!(out.contains("\n\"{x}.rs\"\t"));
    assert!(out.contains("\nplain.rs\t"));
}

#[test]
fn preview_skips_binary_and_block_terminators() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "bin.dat", "ab\0cd\n");
    write(dir.path(), "evil.txt", "line1\n#/block\nline3\n");
    let out = stdout(&globber(dir.path(), &["*", "-P", "5"]));
    assert!(!out.contains("bin.dat lines"));
    assert!(out.contains("file=evil.txt lines=1-1\nline1\n#/block\n"));
    assert_eq!(out.matches("#/block").count(), 1);
}

#[test]
fn invalid_utf8_content_keeps_line_numbers() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("x.txt"), b"ok\n\xff\xfe bad\nthird\n").unwrap();
    let out = stdout(&globber(dir.path(), &["x.txt", "-P", "3"]));
    assert!(out.contains("lines=1-3\n"));
    assert!(out.contains("third\n#/block"));
}

#[test]
fn no_matches_is_success_with_empty_output() {
    let dir = project();
    let out = globber(dir.path(), &["**/*.nothing", "-p"]);
    assert!(out.status.success());
    assert!(stdout(&out).is_empty());
}

#[test]
fn no_stat_schema() {
    let dir = project();
    let out = stdout(&globber(dir.path(), &["*.md", "--no-stat", "-S"]));
    assert!(out.contains("#schema path:str:path kind:"));
    assert!(out.contains("README.md\tdoc\tfalse\n"));
    assert!(!out.contains("total_bytes"));
}

#[test]
fn core_excludes_file_is_honored() {
    let dir = project();
    let d = dir.path();
    git(d, &["init", "-q"]);
    let cfg = tempfile::tempdir().unwrap();
    let excludes = cfg.path().join("my-excludes");
    fs::write(&excludes, "*.txt\n").unwrap();
    let gitconfig = cfg.path().join("gitconfig");
    fs::write(&gitconfig, format!("[core]\n\texcludesFile = {}\n", excludes.display())).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_globber"))
        .args(["*", "-g", "-p"])
        .current_dir(d)
        .env("GIT_CONFIG_GLOBAL", &gitconfig)
        .env("XDG_CONFIG_HOME", cfg.path())
        .output()
        .unwrap();
    let got = lines(&out);
    assert!(!got.contains(&"notes.txt".to_string()), "{:?}", got);
    assert!(got.contains(&"README.md".to_string()));
}

// ── --git-files ─────────────────────────────────────────────────────

fn git_project() -> tempfile::TempDir {
    let dir = project();
    let d = dir.path();
    git(d, &["init", "-q"]);
    write(d, "logs/tracked.log", "x");
    write(d, ".gitignore", "/target\ndocs/build/\n*.log\n");
    git(d, &["add", "-A"]);
    git(d, &["add", "-f", "logs/tracked.log"]);
    git(d, &["commit", "-qm", "init"]);
    write(d, "logs/untracked.log", "x"); // ignored, untracked
    write(d, "src/fresh.rs", "fn f() {}\n"); // untracked, not ignored
    fs::remove_file(d.join("src/util/helpers.rs")).unwrap(); // tracked, deleted
    // An untracked nested repository.
    write(d, "nested/inner.rs", "x");
    git(&d.join("nested"), &["init", "-q"]);
    dir
}

#[test]
fn git_files_is_gits_view() {
    let dir = git_project();
    let out = globber(dir.path(), &["**", "--git-files", "-p"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        lines(&out),
        vec![
            "Cargo.toml",
            "README.md",
            "docs/guide.md",
            "logs/tracked.log",
            "nested/inner.rs",
            "notes.txt",
            "src/fresh.rs",
            "src/lib.rs",
            "src/main.rs",
            "src/target/keep.rs",
        ]
    );
}

#[test]
fn git_files_with_patterns_filters_and_limits() {
    let dir = git_project();
    let d = dir.path();
    assert_eq!(
        lines(&globber(d, &["src/**/*.rs", "--git-files", "-p"])),
        vec!["src/fresh.rs", "src/lib.rs", "src/main.rs", "src/target/keep.rs"]
    );
    assert_eq!(
        lines(&globber(d, &["**/*.rs", "--git-files", "-e", "target", "-n", "2", "-p"])),
        vec!["nested/inner.rs", "src/fresh.rs"]
    );
    assert_eq!(
        lines(&globber(d, &["**", "--git-files", "-k", "doc", "-p"])),
        vec!["README.md", "docs/guide.md", "notes.txt"]
    );
    assert_eq!(
        lines(&globber(d, &["**", "--git-files", "--depth", "1", "-p"])),
        vec!["Cargo.toml", "README.md", "notes.txt"]
    );
    assert_eq!(
        lines(&globber(d, &["**/*.rs", "--git-files", "--skip-nested-repos", "-p"])),
        vec!["src/fresh.rs", "src/lib.rs", "src/main.rs", "src/target/keep.rs"]
    );
}

#[test]
fn git_files_hidden_rules_apply() {
    let dir = git_project();
    let d = dir.path();
    let out = lines(&globber(d, &["**", "--git-files", "-a", "-p"]));
    assert!(out.contains(&".gitignore".to_string()));
    let out = lines(&globber(d, &["**", "--git-files", "-p"]));
    assert!(!out.contains(&".gitignore".to_string()));
}

#[test]
fn git_files_errors() {
    let dir = project(); // not a git repository
    let out = globber(dir.path(), &["**", "--git-files", "-p"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("not a git repository"), "{}", stderr(&out));
    let out = globber(dir.path(), &["**", "--git-files", "-d"]);
    assert_eq!(out.status.code(), Some(2));
}

// ── Budget reporting ────────────────────────────────────────────────

fn fit_tree() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "Sources/App.swift", &"a".repeat(3500)); // 1000 tokens
    write(dir.path(), "Sources/Conversation.swift", &"a".repeat(70_000)); // 20000
    write(dir.path(), "Sources/Message.swift", &"a".repeat(3500));
    write(dir.path(), "Sources/Zebra.swift", &"a".repeat(35_000)); // 10000
    dir
}

#[test]
fn fit_reports_holes_in_summary_and_skipped_section() {
    let dir = fit_tree();
    let out = stdout(&globber(dir.path(), &["Sources/*.swift", "-t", "5K", "--fit", "-S"]));
    assert!(out.contains("budget_mode\tfit\n"), "{}", out);
    assert!(out.contains("budget_skipped_files\t2\n"));
    assert!(out.contains("budget_skipped_tokens_est\t30000\n"));
    assert!(!out.contains("stopped_early"));
    // Largest first.
    let skipped = &out[out.find("§skipped").expect("§skipped section")..];
    let conv = skipped.find("Sources/Conversation.swift\t").unwrap();
    let zebra = skipped.find("Sources/Zebra.swift\t").unwrap();
    assert!(conv < zebra);
    assert!(skipped.contains("(2 of 2)"));
}

#[test]
fn fit_skipped_section_without_summary_and_stderr_with_paths() {
    let dir = fit_tree();
    let out = stdout(&globber(dir.path(), &["Sources/*.swift", "-t", "5K", "--fit"]));
    assert!(out.contains("§skipped"));
    assert!(!out.contains("§summary"));
    let out = globber(dir.path(), &["Sources/*.swift", "-t", "5K", "--fit", "-p"]);
    assert_eq!(lines(&out), vec!["Sources/App.swift", "Sources/Message.swift"]);
    assert!(stderr(&out).contains("left out 2 matching file(s)"));
    assert!(stderr(&out).contains("Sources/Conversation.swift"));
}

#[test]
fn fit_with_nothing_skipped_says_zero() {
    let dir = fit_tree();
    let out = stdout(&globber(dir.path(), &["Sources/*.swift", "-t", "1M", "--fit", "-S"]));
    assert!(out.contains("budget_skipped_files\t0\n"));
    assert!(!out.contains("§skipped"));
}

#[test]
fn stop_and_limit_report_stopped_early() {
    let dir = fit_tree();
    let out = stdout(&globber(dir.path(), &["Sources/*.swift", "-t", "5K", "-S"]));
    assert!(out.contains("stopped_early\ttoken_budget\n"));
    assert!(!out.contains("budget_mode"));
    let out = stdout(&globber(dir.path(), &["Sources/*.swift", "--byte-budget", "5K", "-S"]));
    assert!(out.contains("stopped_early\tbyte_budget\n"));
    let out = stdout(&globber(dir.path(), &["Sources/*.swift", "-n", "1", "-S"]));
    assert!(out.contains("stopped_early\tlimit\n"));
    let out = stdout(&globber(dir.path(), &["Sources/*.swift", "-S"]));
    assert!(!out.contains("stopped_early"));
}

// ── --prefer / --prefer-from ────────────────────────────────────────

fn set_mtime(path: &Path, secs_ago: u64) {
    let t = std::time::SystemTime::now() - std::time::Duration::from_secs(secs_ago);
    fs::File::options().write(true).open(path).unwrap().set_modified(t).unwrap();
}

#[test]
fn prefer_size_gives_budget_to_big_files() {
    let dir = fit_tree();
    // Alphabetical --fit keeps the small files; --prefer size keeps the big one first.
    let out = globber(dir.path(), &["Sources/*.swift", "-t", "22K", "--fit", "--prefer", "size", "-p"]);
    assert_eq!(lines(&out), vec!["Sources/Conversation.swift", "Sources/App.swift", "Sources/Message.swift"]);
    let out = stdout(&globber(dir.path(), &["Sources/*.swift", "--prefer", "size", "-S"]));
    assert!(out.contains("#sort size desc\n"));
    assert!(out.contains("prefer\tsize\n"));
    let out = globber(dir.path(), &["Sources/*.swift", "--prefer", "size-asc", "-n", "1", "-p"]);
    assert_eq!(lines(&out), vec!["Sources/App.swift"]); // tie with Message.swift breaks by path
}

#[test]
fn prefer_recent_orders_by_mtime() {
    let dir = fit_tree();
    let d = dir.path();
    set_mtime(&d.join("Sources/App.swift"), 3000);
    set_mtime(&d.join("Sources/Conversation.swift"), 2000);
    set_mtime(&d.join("Sources/Message.swift"), 10);
    set_mtime(&d.join("Sources/Zebra.swift"), 1000);
    let out = globber(d, &["Sources/*.swift", "--prefer", "recent", "-n", "2", "-p"]);
    assert_eq!(lines(&out), vec!["Sources/Message.swift", "Sources/Zebra.swift"]);
    let out = stdout(&globber(d, &["Sources/*.swift", "--prefer", "recent"]));
    assert!(out.contains(" modified:datetime?\n#sort modified desc\n"), "{}", out);
    let rec = out.lines().find(|l| l.starts_with("Sources/Message.swift")).unwrap();
    let stamp = rec.split('\t').next_back().unwrap();
    assert_eq!(stamp.len(), 20);
    assert!(stamp.ends_with('Z') && stamp.contains('T'));
}

fn run_with_stdin(dir: &Path, args: &[&str], input: &str) -> Output {
    use std::io::Write;
    let mut child = Command::new(env!("CARGO_BIN_EXE_globber"))
        .args(args)
        .current_dir(dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn prefer_from_all_three_formats() {
    let dir = fit_tree();
    let d = dir.path();
    let inputs = [
        "{\"path\": \"Sources/Zebra.swift\", \"score\": 9}\n{\"path\": \"./Sources/App.swift\", \"score\": 3.5}\n",
        "Sources/Zebra.swift\t9\nSources/App.swift\t3.5\n",
        "      9 Sources/Zebra.swift\n    3.5 Sources/App.swift\n# comment\n\n",
    ];
    for input in inputs {
        let out = run_with_stdin(d, &["Sources/*.swift", "--prefer-from", "-", "-p"], input);
        assert!(out.status.success(), "{}", stderr(&out));
        // Scored first (highest first), then unscored by path.
        assert_eq!(
            lines(&out),
            vec!["Sources/Zebra.swift", "Sources/App.swift", "Sources/Conversation.swift", "Sources/Message.swift"],
            "input {:?}",
            input
        );
    }
    let out = stdout(&run_with_stdin(d, &["Sources/*.swift", "--prefer-from", "-"], inputs[1]));
    assert!(out.contains(" score:float?\n#sort score desc\n"));
    assert!(out.contains("Sources/Zebra.swift\t35000\tsource\t10000\tfalse\t9\n"));
    assert!(out.contains("Sources/Message.swift\t3500\tsource\t1000\tfalse\t_\n"));
}

#[test]
fn prefer_from_file_and_root_relative_paths() {
    let dir = fit_tree();
    let d = dir.path();
    let rank = d.join("rank.tsv");
    fs::write(&rank, "Message.swift\t5\n").unwrap();
    let out = globber(d, &["*.swift", "-r", "Sources", "--prefer-from", rank.to_str().unwrap(), "-n", "1", "-p"]);
    assert_eq!(lines(&out), vec!["Sources/Message.swift"]);
}

#[test]
fn prefer_usage_errors() {
    let dir = fit_tree();
    let d = dir.path();
    for args in [
        &["**", "--prefer", "churn"][..],
        &["**", "--prefer", "size", "--prefer-from", "x"],
        &["**", "--prefer", "recent", "--no-stat"],
    ] {
        assert_eq!(globber(d, args).status.code(), Some(2), "{:?}", args);
    }
    let out = run_with_stdin(d, &["**", "--prefer-from", "-"], "not a valid line\n");
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("line 1"));
}
