// §preview sections — source excerpts for matched files.
//
// Three modes:
//   Head(n)        first n lines
//   Range(a, b)    lines a..=b (1-indexed)
//   Code(n)        n lines starting at the first line of actual code,
//                  skipping the leading preamble (blank lines, license
//                  headers, doc comments, block comments, docstrings,
//                  shebang). Comment syntax is chosen per language, so
//                  Rust `#[derive]` and C `#include` count as code while
//                  a Python `#` comment does not.
//
// Files are streamed line by line — only as much of each file is read
// as the preview needs. Invalid UTF-8 is replaced, not dropped, so the
// `lines=` range always matches the file.

use std::fmt::Write;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

use crate::entry::{Entry, FileKind};
use crate::sif_output::sif_attr;

/// Preview mode — controls which lines are included in §preview blocks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreviewMode {
    /// First N lines (literal). `-P 10`
    Head(usize),
    /// Line range (1-indexed, inclusive). `-P 15-30`
    Range(usize, usize),
    /// N consecutive lines starting at the first line of code, after the
    /// leading comment/blank preamble. `-P code:10`
    Code(usize),
}

impl PreviewMode {
    /// Parse a preview spec string.
    ///
    /// Formats: `10`, `15-30`, `code:10`. All numbers must be at least 1.
    pub fn parse(s: &str) -> Result<Self, String> {
        let count = |t: &str, what: &str| -> Result<usize, String> {
            match t.parse::<usize>() {
                Ok(0) => Err(format!("invalid preview spec {:?}: {} must be at least 1", s, what)),
                Ok(n) => Ok(n),
                Err(_) => Err(format!("invalid preview spec {:?} (expected N, N-M, or code:N)", s)),
            }
        };
        if let Some(rest) = s.strip_prefix("code:") {
            Ok(PreviewMode::Code(count(rest, "line count")?))
        } else if let Some((a, b)) = s.split_once('-') {
            let start = count(a, "range start")?;
            let end = count(b, "range end")?;
            if end < start {
                return Err(format!("invalid preview range {:?}: end is before start", s));
            }
            Ok(PreviewMode::Range(start, end))
        } else {
            Ok(PreviewMode::Head(count(s, "line count")?))
        }
    }
}

/// Append a §preview section with lines of each non-binary file.
pub fn write_preview(entries: &[Entry], mode: &PreviewMode, w: &mut dyn Write) -> std::fmt::Result {
    writeln!(w, "---")?;
    writeln!(w, "§preview")?;

    for entry in entries {
        if entry.is_dir || entry.kind == FileKind::Binary {
            continue;
        }
        let Some((start, lines)) = preview_file(&entry.path, mode) else {
            continue;
        };
        write!(w, "#block code")?;
        if let Some(lang) = language(&entry.path) {
            write!(w, " language={}", lang)?;
        }
        writeln!(
            w,
            " file={} lines={}-{}",
            sif_attr(&entry.path.to_string_lossy()),
            start,
            start + lines.len() - 1,
        )?;
        for line in &lines {
            writeln!(w, "{}", line)?;
        }
        writeln!(w, "#/block")?;
    }
    Ok(())
}

/// Compute the preview for one file: (first line number, lines).
/// Returns None for unreadable, binary, or empty previews.
pub fn preview_file(path: &Path, mode: &PreviewMode) -> Option<(usize, Vec<String>)> {
    let file = std::fs::File::open(path).ok()?;
    let mut reader = BufReader::with_capacity(16 * 1024, file);
    // Content sniff: a NUL byte in the first block means binary.
    if reader.fill_buf().ok()?.contains(&0) {
        return None;
    }
    let mut lines = Lines { reader, buf: Vec::new() };

    let (start, mut out) = match *mode {
        PreviewMode::Head(n) => (1, lines.by_ref().take(n).collect()),
        PreviewMode::Range(a, b) => (a, lines.by_ref().skip(a - 1).take(b - a + 1).collect()),
        PreviewMode::Code(n) => code_preview(&mut lines, &comment_syntax(path), n),
    };

    // A line that reads as the block terminator would end the block early.
    if let Some(pos) = out.iter().position(|l| l == "#/block") {
        out.truncate(pos);
    }
    if out.is_empty() { None } else { Some((start, out)) }
}

/// Skip the preamble, then take `n` lines. Falls back to the first `n`
/// lines if the file has no code at all (e.g. it is all comments).
fn code_preview<I: Iterator<Item = String>>(
    lines: &mut I,
    syntax: &CommentSyntax,
    n: usize,
) -> (usize, Vec<String>) {
    let mut head: Vec<String> = Vec::new();
    let mut in_block: Option<&str> = None;

    for (idx, line) in lines.by_ref().enumerate() {
        let is_preamble = preamble_line(&line, idx, syntax, &mut in_block);
        if !is_preamble {
            let mut out = vec![line];
            out.extend(lines.by_ref().take(n - 1));
            // Trailing blank lines carry no information.
            while out.len() > 1 && out.last().is_some_and(|l| l.trim().is_empty()) {
                out.pop();
            }
            return (idx + 1, out);
        }
        if head.len() < n {
            head.push(line);
        }
    }
    (1, head)
}

/// Whether a line belongs to the leading preamble, tracking open block
/// comments across lines.
fn preamble_line<'a>(
    line: &str,
    idx: usize,
    syntax: &CommentSyntax<'a>,
    in_block: &mut Option<&'a str>,
) -> bool {
    let t = line.trim();
    if let Some(close) = *in_block {
        if let Some(pos) = t.find(close) {
            *in_block = None;
            // Code after the closing marker on the same line ends the preamble.
            return t[pos + close.len()..].trim().is_empty();
        }
        return true;
    }
    if t.is_empty() {
        return true;
    }
    if idx == 0 && t.starts_with("#!") && !t.starts_with("#![") {
        return true; // Shebang.
    }
    if syntax.line.iter().any(|p| t.starts_with(p)) {
        return true;
    }
    for &(open, close) in syntax.block {
        if let Some(after) = t.strip_prefix(open) {
            match after.find(close) {
                Some(pos) => return after[pos + close.len()..].trim().is_empty(),
                None => {
                    *in_block = Some(close);
                    return true;
                }
            }
        }
    }
    false
}

/// Line-by-line reader that tolerates invalid UTF-8 and CRLF endings.
struct Lines<R> {
    reader: R,
    buf: Vec<u8>,
}

impl<R: BufRead> Iterator for Lines<R> {
    type Item = String;

    fn next(&mut self) -> Option<String> {
        self.buf.clear();
        match self.reader.by_ref().take(1 << 20).read_until(b'\n', &mut self.buf) {
            Ok(0) | Err(_) => None,
            Ok(_) => {
                if self.buf.last() == Some(&b'\n') {
                    self.buf.pop();
                    if self.buf.last() == Some(&b'\r') {
                        self.buf.pop();
                    }
                }
                Some(String::from_utf8_lossy(&self.buf).into_owned())
            }
        }
    }
}

// ── Language tables ──────────────────────────────────────────────────

/// Comment markers for one language family.
pub(crate) struct CommentSyntax<'a> {
    line: &'a [&'a str],
    block: &'a [(&'a str, &'a str)],
}

const C_LIKE: CommentSyntax = CommentSyntax { line: &["//"], block: &[("/*", "*/")] };
const HASH: CommentSyntax = CommentSyntax { line: &["#"], block: &[] };
const PYTHON: CommentSyntax = CommentSyntax {
    line: &["#"],
    block: &[("\"\"\"", "\"\"\""), ("'''", "'''"), ("r\"\"\"", "\"\"\"")],
};
const RUBY: CommentSyntax = CommentSyntax { line: &["#"], block: &[("=begin", "=end")] };
const JULIA: CommentSyntax = CommentSyntax { line: &["#"], block: &[("#=", "=#")] };
const POWERSHELL: CommentSyntax = CommentSyntax { line: &["#"], block: &[("<#", "#>")] };
const PHP: CommentSyntax = CommentSyntax { line: &["//", "#"], block: &[("/*", "*/")] };
const LUA: CommentSyntax = CommentSyntax { line: &["--"], block: &[("--[[", "]]")] };
const SQL: CommentSyntax = CommentSyntax { line: &["--"], block: &[("/*", "*/")] };
const HASKELL: CommentSyntax = CommentSyntax { line: &["--"], block: &[("{-", "-}")] };
const OCAML: CommentSyntax = CommentSyntax { line: &[], block: &[("(*", "*)")] };
const FSHARP: CommentSyntax = CommentSyntax { line: &["//"], block: &[("(*", "*)")] };
const LISP: CommentSyntax = CommentSyntax { line: &[";"], block: &[] };
const PERCENT: CommentSyntax = CommentSyntax { line: &["%"], block: &[] };
const MARKUP: CommentSyntax = CommentSyntax { line: &[], block: &[("<!--", "-->")] };
const INI: CommentSyntax = CommentSyntax { line: &[";", "#"], block: &[] };
const NONE: CommentSyntax = CommentSyntax { line: &[], block: &[] };
/// Unknown extension: the most common markers, minus anything ambiguous.
const GENERIC: CommentSyntax = CommentSyntax { line: &["//", "#"], block: &[("/*", "*/")] };

fn extension(path: &Path) -> &str {
    path.extension().and_then(|e| e.to_str()).unwrap_or("")
}

pub(crate) fn comment_syntax(path: &Path) -> CommentSyntax<'static> {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    match name {
        "Dockerfile" | "Makefile" | "makefile" | "GNUmakefile" | "Gemfile" | "Rakefile"
        | "CMakeLists.txt" | "BUILD" | "WORKSPACE" | ".gitignore" | ".dockerignore" => {
            return HASH;
        }
        _ => {}
    }
    match extension(path) {
        "rs" | "c" | "h" | "cpp" | "hpp" | "cc" | "cxx" | "hh" | "m" | "mm" | "java" | "kt"
        | "kts" | "scala" | "go" | "js" | "mjs" | "cjs" | "ts" | "mts" | "cts" | "tsx" | "jsx"
        | "swift" | "cs" | "dart" | "css" | "scss" | "less" | "zig" | "proto" | "groovy"
        | "gradle" | "v" | "d" | "sil" | "json5" | "jsonc" => C_LIKE,
        "py" | "pyi" | "pyx" => PYTHON,
        "rb" => RUBY,
        "jl" => JULIA,
        "ps1" | "psm1" => POWERSHELL,
        "php" => PHP,
        "lua" => LUA,
        "sql" => SQL,
        "hs" | "elm" => HASKELL,
        "ml" | "mli" => OCAML,
        "fs" | "fsx" => FSHARP,
        "sh" | "bash" | "zsh" | "fish" | "pl" | "pm" | "r" | "R" | "tf" | "nix" | "toml"
        | "yaml" | "yml" | "cmake" | "mk" | "ex" | "exs" | "cr" | "nim" | "coffee" | "conf"
        | "cfg" | "env" | "graphql" | "gql" => HASH,
        "ini" => INI,
        "clj" | "cljs" | "edn" | "el" | "lisp" | "scm" | "rkt" => LISP,
        "erl" | "hrl" | "tex" => PERCENT,
        "html" | "htm" | "xml" | "svg" | "vue" | "svelte" | "astro" | "md" | "markdown" => MARKUP,
        "json" | "csv" | "tsv" | "txt" | "sif" => NONE,
        _ => GENERIC,
    }
}

/// Language identifier for the `language=` attribute of code blocks.
pub(crate) fn language(path: &Path) -> Option<&'static str> {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    match name {
        "Dockerfile" => return Some("dockerfile"),
        "Makefile" | "makefile" | "GNUmakefile" => return Some("make"),
        "CMakeLists.txt" => return Some("cmake"),
        _ => {}
    }
    Some(match extension(path) {
        "rs" => "rust",
        "py" | "pyi" => "python",
        "js" | "mjs" | "cjs" => "javascript",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "tsx",
        "jsx" => "jsx",
        "go" => "go",
        "c" | "h" => "c",
        "cpp" | "hpp" | "cc" | "cxx" | "hh" => "cpp",
        "m" | "mm" => "objective-c",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "scala" => "scala",
        "swift" => "swift",
        "cs" => "csharp",
        "rb" => "ruby",
        "php" => "php",
        "sh" | "bash" => "bash",
        "zsh" => "zsh",
        "fish" => "fish",
        "ps1" | "psm1" => "powershell",
        "lua" => "lua",
        "sql" => "sql",
        "html" | "htm" => "html",
        "css" => "css",
        "scss" => "scss",
        "less" => "less",
        "md" | "markdown" => "markdown",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "json" | "json5" | "jsonc" => "json",
        "xml" | "svg" => "xml",
        "dart" => "dart",
        "zig" => "zig",
        "hs" => "haskell",
        "elm" => "elm",
        "ex" | "exs" => "elixir",
        "erl" | "hrl" => "erlang",
        "ml" | "mli" => "ocaml",
        "fs" | "fsx" => "fsharp",
        "clj" | "cljs" | "edn" => "clojure",
        "r" | "R" => "r",
        "jl" => "julia",
        "pl" | "pm" => "perl",
        "nix" => "nix",
        "tf" => "hcl",
        "proto" => "protobuf",
        "graphql" | "gql" => "graphql",
        "vue" => "vue",
        "svelte" => "svelte",
        "sil" => "sil",
        "sif" => "sif",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(src: &str, ext: &str, n: usize) -> (usize, Vec<String>) {
        let mut lines = src.lines().map(String::from);
        code_preview(&mut lines, &comment_syntax(Path::new(&format!("f.{}", ext))), n)
    }

    #[test]
    fn rust_attributes_are_code() {
        let (start, lines) = code("// header\n\n#![allow(x)]\nuse std::fs;\n", "rs", 2);
        assert_eq!(start, 3);
        assert_eq!(lines, vec!["#![allow(x)]", "use std::fs;"]);
    }

    #[test]
    fn c_block_comment_and_include() {
        let src = "/*\n Copyright\n */\n#include <stdio.h>\nint main(){}\n";
        let (start, lines) = code(src, "c", 2);
        assert_eq!(start, 4);
        assert_eq!(lines, vec!["#include <stdio.h>", "int main(){}"]);
    }

    #[test]
    fn python_multiline_docstring() {
        let src = "#!/usr/bin/env python\n\"\"\"Module doc\nspanning lines.\n\"\"\"\nimport os\n";
        let (start, lines) = code(src, "py", 1);
        assert_eq!(start, 5);
        assert_eq!(lines, vec!["import os"]);
    }

    #[test]
    fn one_line_docstring() {
        let (start, _) = code("\"\"\"Doc.\"\"\"\nx = 1\n", "py", 1);
        assert_eq!(start, 2);
    }

    #[test]
    fn code_after_block_close_on_same_line() {
        let (start, lines) = code("/* hi */ int x;\n", "c", 1);
        assert_eq!(start, 1);
        assert_eq!(lines, vec!["/* hi */ int x;"]);
    }

    #[test]
    fn markdown_heading_is_content() {
        let (start, _) = code("<!-- badge -->\n# Title\n", "md", 1);
        assert_eq!(start, 2);
    }

    #[test]
    fn all_comments_falls_back_to_head() {
        let (start, lines) = code("// only\n// comments\n", "rs", 5);
        assert_eq!(start, 1);
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn trailing_blank_lines_trimmed() {
        let (_, lines) = code("fn a() {}\n\n\n", "rs", 3);
        assert_eq!(lines, vec!["fn a() {}"]);
    }

    #[test]
    fn pointer_deref_is_code_in_c() {
        let (start, _) = code("*p = 1;\n", "c", 1);
        assert_eq!(start, 1);
    }

    #[test]
    fn parse_rejects_zero() {
        assert!(PreviewMode::parse("0").is_err());
        assert!(PreviewMode::parse("code:0").is_err());
        assert!(PreviewMode::parse("0-3").is_err());
        assert!(PreviewMode::parse("3-0").is_err());
        assert_eq!(PreviewMode::parse("code:4"), Ok(PreviewMode::Code(4)));
    }
}
