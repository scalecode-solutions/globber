// Adversarial and edge-case tests.
//
// These test pathological patterns, weird filenames, boundary conditions,
// and anything an LLM or fuzzer might throw at globber.

use globber::{expand_braces, Pattern, MatchOptions};

// ── NFA backtracking safety ─────────────────────────────────────────
// These patterns would be exponential O(2^n) with naive backtracking.
// The NFA must handle them in linear time (< 1ms).

#[test]
fn adversarial_many_stars() {
    let pat = Pattern::new("*a*b*c*d*e*f*g*h*i*j*k*l").unwrap();
    // Non-matching input — worst case for backtracking.
    assert!(!pat.matches("aabbccddeeffgghhiijjkkzz"));
}

#[test]
fn adversarial_repeated_star_a() {
    let pat = Pattern::new("a*a*a*a*a*a*a*a*a*a*a*a*a*a*a*a").unwrap();
    // Long input of all 'a's — should match.
    let input = "a".repeat(100);
    assert!(pat.matches(&input));
    // One 'b' at the end — should not match.
    let mut bad = "a".repeat(99);
    bad.push('b');
    assert!(!pat.matches(&bad));
}

#[test]
fn adversarial_star_no_match_long_input() {
    let pat = Pattern::new("*x*y*z").unwrap();
    // 10K chars with no x, y, or z — must not hang.
    let input = "a".repeat(10_000);
    assert!(!pat.matches(&input));
}

#[test]
fn adversarial_double_star_deep_path() {
    let pat = Pattern::new("**/*.rs").unwrap();
    // Very deep path.
    let mut path = String::new();
    for i in 0..200 {
        path.push_str(&format!("dir{}/", i));
    }
    path.push_str("file.rs");
    assert!(pat.matches(&path));
}

#[test]
fn adversarial_double_star_many_components() {
    let pat = Pattern::new("a/**/b/**/c/**/d").unwrap();
    assert!(pat.matches("a/b/c/d"));
    assert!(pat.matches("a/x/b/y/c/z/d"));
    assert!(pat.matches("a/x/y/z/b/c/d"));
    assert!(!pat.matches("a/b/c/e"));
}

// ── Pattern compilation edge cases ──────────────────────────────────

#[test]
fn empty_pattern() {
    let pat = Pattern::new("").unwrap();
    assert!(pat.matches(""));
    assert!(!pat.matches("x"));
}

#[test]
fn only_star() {
    let pat = Pattern::new("*").unwrap();
    assert!(pat.matches(""));
    assert!(pat.matches("anything"));
    assert!(pat.matches("with spaces"));
    // Star doesn't match path separators by default in fnmatch mode
    // (but does when require_literal_separator is false, which is the default).
    assert!(pat.matches("a/b"));
}

#[test]
fn only_double_star() {
    let pat = Pattern::new("**").unwrap();
    assert!(pat.matches(""));
    assert!(pat.matches("a"));
    assert!(pat.matches("a/b/c/d/e"));
}

#[test]
fn only_question_mark() {
    let pat = Pattern::new("?").unwrap();
    assert!(!pat.matches(""));
    assert!(pat.matches("a"));
    assert!(!pat.matches("ab"));
}

#[test]
fn triple_star_is_error() {
    assert!(Pattern::new("***").is_err());
}

#[test]
fn double_star_not_standalone() {
    assert!(Pattern::new("a**b").is_err());
    assert!(Pattern::new("a**/b").is_err());
    assert!(Pattern::new("a/**b").is_err());
}

#[test]
fn double_star_valid_positions() {
    assert!(Pattern::new("**").is_ok());
    assert!(Pattern::new("**/a").is_ok());
    assert!(Pattern::new("a/**").is_ok());
    assert!(Pattern::new("a/**/b").is_ok());
    assert!(Pattern::new("a/**/**/b").is_ok());
}

// ── Bracket expressions ─────────────────────────────────────────────

#[test]
fn bracket_empty_is_error() {
    assert!(Pattern::new("[]").is_err());
    assert!(Pattern::new("[!]").is_err());
}

#[test]
fn bracket_unclosed_is_error() {
    assert!(Pattern::new("[abc").is_err());
    assert!(Pattern::new("[").is_err());
    assert!(Pattern::new("[!").is_err());
}

#[test]
fn bracket_close_as_first_char() {
    // []] matches literal ]
    let pat = Pattern::new("[]]").unwrap();
    assert!(pat.matches("]"));
    assert!(!pat.matches("["));
}

#[test]
fn bracket_negated_close_first() {
    // [!]] matches anything except ]
    let pat = Pattern::new("[!]]").unwrap();
    assert!(!pat.matches("]"));
    assert!(pat.matches("a"));
    assert!(pat.matches("["));
}

#[test]
fn bracket_range() {
    let pat = Pattern::new("[a-z]").unwrap();
    assert!(pat.matches("m"));
    assert!(!pat.matches("M"));
    assert!(!pat.matches("5"));
}

#[test]
fn bracket_range_inverted() {
    // [z-a] is an empty range — matches nothing.
    let pat = Pattern::new("[z-a]").unwrap();
    assert!(!pat.matches("a"));
    assert!(!pat.matches("m"));
    assert!(!pat.matches("z"));
}

#[test]
fn bracket_dash_at_edges() {
    // [-a] matches - or a.
    let pat = Pattern::new("[-a]").unwrap();
    assert!(pat.matches("-"));
    assert!(pat.matches("a"));
    assert!(!pat.matches("b"));

    // [a-] matches a or -.
    let pat = Pattern::new("[a-]").unwrap();
    assert!(pat.matches("a"));
    assert!(pat.matches("-"));
    assert!(!pat.matches("b"));
}

// ── Escape handling ─────────────────────────────────────────────────

#[test]
fn escape_star() {
    let pat = Pattern::new("hello\\*world").unwrap();
    assert!(pat.matches("hello*world"));
    assert!(!pat.matches("helloXworld"));
    assert!(!pat.matches("helloworld"));
}

#[test]
fn escape_question() {
    let pat = Pattern::new("a\\?b").unwrap();
    assert!(pat.matches("a?b"));
    assert!(!pat.matches("axb"));
}

#[test]
fn escape_bracket() {
    let pat = Pattern::new("a\\[b").unwrap();
    assert!(pat.matches("a[b"));
}

#[test]
fn escape_backslash() {
    let pat = Pattern::new("a\\\\b").unwrap();
    assert!(pat.matches("a\\b"));
}

#[test]
fn trailing_backslash() {
    // Trailing backslash is ignored (POSIX says unspecified).
    let pat = Pattern::new("abc\\").unwrap();
    assert!(pat.matches("abc"));
}

#[test]
fn escape_roundtrip() {
    let original = "hello[world]*?.txt{a,b}";
    let escaped = Pattern::escape(original);
    let pat = Pattern::new(&escaped).unwrap();
    assert!(pat.matches(original));
    assert!(!pat.matches("helloXworldXXtxtXX"));
}

// ── MatchOptions ────────────────────────────────────────────────────

#[test]
fn case_insensitive() {
    let opts = MatchOptions {
        case_sensitive: false,
        ..MatchOptions::new()
    };
    let pat = Pattern::new("Hello.RS").unwrap();
    assert!(pat.matches_with("hello.rs", opts));
    assert!(pat.matches_with("HELLO.RS", opts));
    assert!(pat.matches_with("hElLo.rS", opts));
}

#[test]
fn require_literal_separator() {
    let opts = MatchOptions {
        require_literal_separator: true,
        ..MatchOptions::new()
    };
    let pat = Pattern::new("*").unwrap();
    assert!(pat.matches_with("hello", opts));
    assert!(!pat.matches_with("a/b", opts));

    let pat = Pattern::new("*/*").unwrap();
    assert!(pat.matches_with("a/b", opts));
    assert!(!pat.matches_with("a/b/c", opts));
}

#[test]
fn require_literal_leading_dot() {
    let opts = MatchOptions {
        require_literal_leading_dot: true,
        ..MatchOptions::new()
    };
    assert!(!Pattern::new("*").unwrap().matches_with(".hidden", opts));
    assert!(Pattern::new(".*").unwrap().matches_with(".hidden", opts));
    assert!(Pattern::new("*.txt").unwrap().matches_with("visible.txt", opts));
}

#[test]
fn double_star_leading_dot() {
    let opts = MatchOptions {
        require_literal_leading_dot: true,
        ..MatchOptions::new()
    };
    assert!(!Pattern::new("**/*").unwrap().matches_with(".hidden", opts));
    assert!(!Pattern::new("**/*").unwrap().matches_with("dir/.hidden", opts));
    assert!(Pattern::new("**/.*").unwrap().matches_with(".hidden", opts));
    assert!(Pattern::new("**/.*").unwrap().matches_with("dir/.hidden", opts));
}

// ── Recursive pattern semantics ─────────────────────────────────────

#[test]
fn double_star_matches_zero_components() {
    let pat = Pattern::new("a/**/b").unwrap();
    assert!(pat.matches("a/b"));
}

#[test]
fn double_star_matches_one_component() {
    let pat = Pattern::new("a/**/b").unwrap();
    assert!(pat.matches("a/x/b"));
}

#[test]
fn double_star_matches_many_components() {
    let pat = Pattern::new("a/**/b").unwrap();
    assert!(pat.matches("a/x/y/z/b"));
}

#[test]
fn double_star_only_at_component_boundary() {
    // **/. should only match at component boundaries, not mid-filename.
    let pat = Pattern::new("**/.*").unwrap();
    assert!(pat.matches(".gitignore"));
    assert!(pat.matches("src/.hidden"));
    assert!(!pat.matches("src/not.hidden"));
    assert!(!pat.matches("ab.c"));
}

#[test]
fn consecutive_double_stars_collapse() {
    let pat = Pattern::new("a/**/**/b").unwrap();
    assert!(pat.matches("a/b"));
    assert!(pat.matches("a/x/b"));
    assert!(pat.matches("a/x/y/z/b"));
}

// ── Brace expansion ─────────────────────────────────────────────────

#[test]
fn brace_simple() {
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
fn brace_nested() {
    let mut r = expand_braces("{a,{b,c}}");
    r.sort();
    assert_eq!(r, vec!["a", "b", "c"]);
}

#[test]
fn brace_deeply_nested() {
    let mut r = expand_braces("x/{a,{b,{c,d}}}");
    r.sort();
    assert_eq!(r, vec!["x/a", "x/b", "x/c", "x/d"]);
}

#[test]
fn brace_multiple() {
    let mut r = expand_braces("{a,b}/{c,d}");
    r.sort();
    assert_eq!(r, vec!["a/c", "a/d", "b/c", "b/d"]);
}

#[test]
fn brace_no_braces() {
    assert_eq!(expand_braces("hello"), vec!["hello"]);
}

#[test]
fn brace_single_alternative() {
    assert_eq!(expand_braces("{a}"), vec!["a"]);
}

#[test]
fn brace_empty_alternative() {
    let r = expand_braces("{a,}");
    assert!(r.contains(&"a".to_string()));
    assert!(r.contains(&"".to_string()));
}

#[test]
fn brace_with_glob_chars() {
    let r = expand_braces("**/*.{rs,py,go}");
    assert_eq!(r.len(), 3);
    assert!(r.contains(&"**/*.rs".to_string()));
    assert!(r.contains(&"**/*.py".to_string()));
    assert!(r.contains(&"**/*.go".to_string()));
}

// ── FileKind classification ─────────────────────────────────────────

#[test]
fn kind_source_extensions() {
    use globber::FileKind;
    use std::path::Path;
    for ext in &["rs", "go", "py", "js", "ts", "tsx", "c", "h", "cpp", "java", "swift", "sh", "sil"] {
        let kind = FileKind::infer(Path::new(&format!("file.{}", ext)));
        assert_eq!(kind, FileKind::Source, "expected source for .{}", ext);
    }
}

#[test]
fn kind_test_paths() {
    use globber::FileKind;
    use std::path::Path;
    assert_eq!(FileKind::infer(Path::new("tests/unit.rs")), FileKind::Test);
    assert_eq!(FileKind::infer(Path::new("src/test_main.py")), FileKind::Test);
    assert_eq!(FileKind::infer(Path::new("foo.test.ts")), FileKind::Test);
    assert_eq!(FileKind::infer(Path::new("bar.spec.js")), FileKind::Test);
    assert_eq!(FileKind::infer(Path::new("spec/helper.rb")), FileKind::Test);
}

#[test]
fn kind_config() {
    use globber::FileKind;
    use std::path::Path;
    for name in &["Cargo.toml", "package.json", "Makefile", "Dockerfile", ".gitignore", ".editorconfig"] {
        let kind = FileKind::infer(Path::new(name));
        assert_eq!(kind, FileKind::Config, "expected config for {}", name);
    }
}

#[test]
fn kind_generated() {
    use globber::FileKind;
    use std::path::Path;
    assert_eq!(FileKind::infer(Path::new("Cargo.lock")), FileKind::Generated);
    assert_eq!(FileKind::infer(Path::new("package-lock.json")), FileKind::Generated);
    assert_eq!(FileKind::infer(Path::new("version.h.in")), FileKind::Generated);
    assert_eq!(FileKind::infer(Path::new("template.j2")), FileKind::Generated);
    assert_eq!(FileKind::infer(Path::new("page.erb")), FileKind::Generated);
}

#[test]
fn kind_binary() {
    use globber::FileKind;
    use std::path::Path;
    for ext in &["png", "jpg", "zip", "exe", "dll", "so", "wasm", "pdf", "mp4"] {
        let kind = FileKind::infer(Path::new(&format!("file.{}", ext)));
        assert_eq!(kind, FileKind::Binary, "expected binary for .{}", ext);
    }
}

#[test]
fn kind_doc() {
    use globber::FileKind;
    use std::path::Path;
    assert_eq!(FileKind::infer(Path::new("README.md")), FileKind::Doc);
    assert_eq!(FileKind::infer(Path::new("CHANGELOG")), FileKind::Doc);
    assert_eq!(FileKind::infer(Path::new("LICENSE")), FileKind::Doc);
    assert_eq!(FileKind::infer(Path::new("docs.rst")), FileKind::Doc);
}

#[test]
fn kind_data() {
    use globber::FileKind;
    use std::path::Path;
    for ext in &["json", "csv", "tsv", "xml", "sif", "sql", "jsonl"] {
        let kind = FileKind::infer(Path::new(&format!("file.{}", ext)));
        assert_eq!(kind, FileKind::Data, "expected data for .{}", ext);
    }
}

// ── PreviewMode parsing ─────────────────────────────────────────────

#[test]
fn preview_mode_head() {
    use globber::PreviewMode;
    match PreviewMode::parse("10").unwrap() {
        PreviewMode::Head(n) => assert_eq!(n, 10),
        _ => panic!("expected Head"),
    }
}

#[test]
fn preview_mode_range() {
    use globber::PreviewMode;
    match PreviewMode::parse("15-30").unwrap() {
        PreviewMode::Range(a, b) => { assert_eq!(a, 15); assert_eq!(b, 30); }
        _ => panic!("expected Range"),
    }
}

#[test]
fn preview_mode_code() {
    use globber::PreviewMode;
    match PreviewMode::parse("code:20").unwrap() {
        PreviewMode::Code(n) => assert_eq!(n, 20),
        _ => panic!("expected Code"),
    }
}

#[test]
fn preview_mode_invalid() {
    use globber::PreviewMode;
    assert!(PreviewMode::parse("abc").is_err());
    assert!(PreviewMode::parse("0-10").is_err());   // 0 is not valid (1-indexed)
    assert!(PreviewMode::parse("10-5").is_err());    // end < start
    assert!(PreviewMode::parse("code:abc").is_err());
}

// ── Unicode and special characters ──────────────────────────────────

#[test]
fn unicode_literal_match() {
    let pat = Pattern::new("café.txt").unwrap();
    assert!(pat.matches("café.txt"));
    assert!(!pat.matches("cafe.txt"));
}

#[test]
fn unicode_star_match() {
    let pat = Pattern::new("*.txt").unwrap();
    assert!(pat.matches("café.txt"));
    assert!(pat.matches("日本語.txt"));
    assert!(pat.matches("🦀.txt"));
}

#[test]
fn unicode_question_mark() {
    let pat = Pattern::new("??.txt").unwrap();
    // Two unicode chars + .txt
    assert!(pat.matches("ab.txt"));
    // Multi-byte unicode chars still count as one ? each.
    assert!(pat.matches("日本.txt"));
}

#[test]
fn spaces_in_pattern() {
    let pat = Pattern::new("hello world.txt").unwrap();
    assert!(pat.matches("hello world.txt"));
}

#[test]
fn star_matches_spaces() {
    let pat = Pattern::new("*.txt").unwrap();
    assert!(pat.matches("hello world.txt"));
    assert!(pat.matches("  .txt"));
}
