// Rich file entries with metadata.
//
// Unlike rust-lang/glob which yields bare PathBuf, globber yields Entry
// values that carry enough metadata for an AI agent to make decisions
// without additional syscalls:
//
//   - path, size, modified time
//   - is_dir / is_symlink flags
//   - estimated token count (size / 3.5 heuristic)
//   - file kind classification (Source, Config, Test, Generated, Binary, etc.)
//

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Classification of a file's role in a project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileKind {
    Source,
    Test,
    Config,
    Build,
    Doc,
    Data,
    Generated,
    Binary,
    Unknown,
}

/// Directory names whose contents are build output or installed deps.
const BUILD_DIRS: &[&str] = &[
    "target", "build", "dist", "node_modules", "__pycache__", ".next", ".nuxt", ".gradle",
    ".tox", ".mypy_cache", ".pytest_cache",
];

/// Directory names whose contents are tests.
const TEST_DIRS: &[&str] = &["test", "tests", "spec", "specs", "__tests__", "fixtures", "testdata"];

impl FileKind {
    /// Legacy numeric code for this kind.
    #[deprecated(
        since = "0.7.0",
        note = "these numbers do not match the SIF Classify registry (e.g. 310 is \
                reference.path.generic there); SIF output now uses the kind name"
    )]
    pub fn classify_code(self) -> u16 {
        match self {
            FileKind::Source => 310,
            FileKind::Test => 312,
            FileKind::Config => 320,
            FileKind::Build => 321,
            FileKind::Doc => 330,
            FileKind::Data => 340,
            FileKind::Generated => 350,
            FileKind::Binary => 360,
            FileKind::Unknown => 300,
        }
    }

    /// Human-readable name for SIF output.
    pub fn as_str(self) -> &'static str {
        match self {
            FileKind::Source => "source",
            FileKind::Test => "test",
            FileKind::Config => "config",
            FileKind::Build => "build",
            FileKind::Doc => "doc",
            FileKind::Data => "data",
            FileKind::Generated => "generated",
            FileKind::Binary => "binary",
            FileKind::Unknown => "unknown",
        }
    }

    /// Infer file kind from path using extension and path heuristics.
    ///
    /// This is a fast lookup-table approach — no file I/O, no magic bytes.
    /// Checks run in priority order: build-output directories, test
    /// directories and names, generated/lock files, then by name and
    /// extension.
    pub fn infer(path: &Path) -> Self {
        let name = match path.file_name() {
            Some(n) => n.to_string_lossy(),
            None => return FileKind::Unknown,
        };
        let name = &*name;
        let ext = path.extension().map(|e| e.to_string_lossy()).unwrap_or_default();
        let ext = &*ext;
        let in_dir = |dirs: &[&str]| {
            path.parent().is_some_and(|p| {
                p.components().any(|c| dirs.contains(&&*c.as_os_str().to_string_lossy()))
            })
        };

        if in_dir(BUILD_DIRS) {
            return FileKind::Build;
        }

        if in_dir(TEST_DIRS)
            || name.starts_with("test_")
            || name == "conftest.py"
            || [
                "_test.rs", "_test.go", "_test.py", "_spec.rb", "_test.exs", "Test.java",
                "Tests.java", "Test.kt", "Tests.cs", "Test.cs", "Tests.swift", "Test.swift",
            ]
            .iter()
            .any(|s| name.ends_with(s))
            || [".test.", ".spec.", "_test."].iter().any(|m| {
                name.contains(m)
                    && matches!(ext, "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "mts" | "cts" | "dart")
            })
        {
            return FileKind::Test;
        }

        // Generated files and lockfiles.
        match name {
            "Cargo.lock" | "package-lock.json" | "yarn.lock" | "pnpm-lock.yaml" | "Gemfile.lock"
            | "Pipfile.lock" | "poetry.lock" | "composer.lock" | "go.sum" | "uv.lock"
            | "flake.lock" | "Package.resolved" | "bun.lock" | "mix.lock" | "pubspec.lock" => {
                return FileKind::Generated;
            }
            _ => {}
        }
        if [
            ".generated.rs", ".gen.go", ".pb.go", ".pb.rs", "_pb2.py", "_pb2_grpc.py", ".g.dart",
            ".freezed.dart", ".min.js", ".min.css", ".js.map", ".css.map", ".d.ts.map",
        ]
        .iter()
        .any(|s| name.ends_with(s))
        {
            return FileKind::Generated;
        }

        // Config files by name.
        match name {
            "Cargo.toml" | "Makefile" | "makefile" | "GNUmakefile" | "CMakeLists.txt" | "build.rs"
            | "build.gradle" | "build.gradle.kts" | "settings.gradle" | "settings.gradle.kts"
            | "pom.xml" | "package.json" | "tsconfig.json" | "jsconfig.json" | "deno.json"
            | "pyproject.toml" | "setup.py" | "setup.cfg" | "tox.ini" | "requirements.txt"
            | "Pipfile" | "go.mod" | "go.work" | ".gitignore" | ".gitattributes" | ".gitmodules"
            | ".editorconfig" | ".dockerignore" | ".npmrc" | ".nvmrc" | ".prettierrc"
            | ".eslintrc" | ".eslintrc.json" | ".eslintrc.js" | ".babelrc" | "Dockerfile"
            | "Containerfile" | "docker-compose.yml" | "docker-compose.yaml" | "compose.yaml"
            | "Gemfile" | "Rakefile" | "Procfile" | "Justfile" | "justfile" | "flake.nix"
            | "Package.swift" | "pubspec.yaml" | "mix.exs" | ".env" => {
                return FileKind::Config;
            }
            _ => {}
        }
        if name.starts_with(".env.") || name.starts_with("Dockerfile.") {
            return FileKind::Config;
        }

        // Config by extension.
        match ext {
            "toml" | "yaml" | "yml" | "ini" | "cfg" | "conf" | "env" | "properties" | "tf"
            | "tfvars" | "hcl" | "cmake" | "gradle" | "plist" | "editorconfig" => {
                return FileKind::Config;
            }
            _ => {}
        }

        // Documentation.
        match ext {
            "md" | "markdown" | "rst" | "txt" | "adoc" | "org" | "tex" | "rtf" => {
                return FileKind::Doc;
            }
            _ => {}
        }
        let stem = name.split('.').next().unwrap_or(name);
        if matches!(
            stem,
            "LICENSE" | "LICENCE" | "COPYING" | "NOTICE" | "CHANGELOG" | "CHANGES" | "README"
                | "CONTRIBUTING" | "AUTHORS" | "CODEOWNERS" | "SECURITY"
        ) {
            return FileKind::Doc;
        }

        // Data files.
        match ext {
            "json" | "jsonl" | "ndjson" | "json5" | "jsonc" | "csv" | "tsv" | "xml" | "sif"
            | "sql" | "parquet" | "avro" | "svg" | "geojson" | "graphql" | "gql" => {
                return FileKind::Data;
            }
            _ => {}
        }

        // Binary files.
        match ext {
            "png" | "jpg" | "jpeg" | "gif" | "bmp" | "ico" | "webp" | "avif" | "heic" | "tif"
            | "tiff" | "psd" | "woff" | "woff2" | "ttf" | "otf" | "eot" | "zip" | "tar" | "gz"
            | "tgz" | "bz2" | "xz" | "zst" | "7z" | "rar" | "jar" | "war" | "exe" | "dll" | "so"
            | "dylib" | "a" | "lib" | "o" | "obj" | "rlib" | "wasm" | "class" | "pyc" | "pyd"
            | "pdb" | "bin" | "db" | "sqlite" | "sqlite3" | "mp3" | "mp4" | "wav" | "flac"
            | "ogg" | "m4a" | "aac" | "avi" | "mov" | "mkv" | "webm" | "pdf" | "onnx" | "pt"
            | "safetensors" | "gguf" | "npy" | "npz" | "pkl" | "lockb" => {
                return FileKind::Binary;
            }
            _ => {}
        }

        // Template files by extension.
        match ext {
            "in" | "j2" | "jinja" | "jinja2" | "erb" | "ejs" | "hbs" | "mustache" | "tmpl"
            | "tpl" | "tt" | "tt2" => {
                return FileKind::Generated;
            }
            _ => {}
        }

        // Source code by extension.
        match ext {
            "rs" | "go" | "py" | "pyi" | "pyx" | "js" | "mjs" | "cjs" | "ts" | "mts" | "cts"
            | "tsx" | "jsx" | "c" | "h" | "cpp" | "hpp" | "cc" | "cxx" | "hh" | "hxx" | "ino"
            | "java" | "kt" | "kts" | "scala" | "groovy" | "clj" | "cljs" | "ex" | "exs" | "rb"
            | "php" | "swift" | "m" | "mm" | "cs" | "fs" | "fsx" | "vb" | "zig" | "nim" | "v"
            | "d" | "lua" | "pl" | "pm" | "sh" | "bash" | "zsh" | "fish" | "ps1" | "psm1" | "bat"
            | "cmd" | "css" | "scss" | "sass" | "less" | "styl" | "html" | "htm" | "vue"
            | "svelte" | "astro" | "r" | "R" | "jl" | "hs" | "ml" | "mli" | "erl" | "hrl" | "elm"
            | "dart" | "proto" | "sol" | "cr" | "nix" | "el" | "scm" | "rkt" | "lisp" | "vim"
            | "f90" | "f95" | "asm" | "s" | "S" | "cu" | "glsl" | "wgsl" | "hlsl" | "metal"
            | "sil" => return FileKind::Source,
            _ => {}
        }

        FileKind::Unknown
    }
}

impl std::fmt::Display for FileKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A single matched file entry with rich metadata.
#[derive(Debug, Clone)]
pub struct Entry {
    /// The matched path (relative or absolute, as given by the glob root).
    pub path: PathBuf,
    /// File size in bytes. 0 for directories or if stat failed.
    pub size: u64,
    /// Estimated token count (size / 3.5, rounded up; an extension-based
    /// guess in no-stat walks).
    pub tokens_est: u64,
    /// Whether this is a directory.
    pub is_dir: bool,
    /// Whether this is a symlink.
    pub is_symlink: bool,
    /// Last modified time, if available.
    pub modified: Option<SystemTime>,
    /// Inferred file kind.
    pub kind: FileKind,
}

impl Entry {
    /// Build an Entry from a path by stat-ing it.
    pub fn from_path(path: PathBuf) -> Self {
        let (size, is_dir, is_symlink, modified) = match fs::symlink_metadata(&path) {
            Ok(meta) => {
                let is_symlink = meta.is_symlink();
                // If symlink, resolve to get real metadata.
                let real_meta = if is_symlink {
                    fs::metadata(&path).ok()
                } else {
                    Some(meta.clone())
                };
                let (size, is_dir, modified) = match real_meta {
                    Some(m) => (m.len(), m.is_dir(), m.modified().ok()),
                    None => (0, false, meta.modified().ok()),
                };
                (size, is_dir, is_symlink, modified)
            }
            Err(_) => (0, false, false, None),
        };

        let kind = if is_dir {
            FileKind::Unknown
        } else {
            FileKind::infer(&path)
        };
        let tokens_est = estimate_tokens(size);

        Entry {
            path,
            size,
            tokens_est,
            is_dir,
            is_symlink,
            modified,
            kind,
        }
    }

    /// Build a lightweight Entry from a DirEntry (avoids extra syscall on Linux).
    pub(crate) fn from_dir_entry(path: PathBuf, de: &fs::DirEntry) -> Self {
        let ft = de.file_type().ok();
        let is_symlink = ft.as_ref().is_some_and(|f| f.is_symlink());

        // On Linux, DirEntry gives us file_type for free from readdir.
        // For symlinks or if file_type is unavailable, fall back to stat.
        let meta = if is_symlink || ft.is_none() {
            fs::metadata(&path).ok()
        } else {
            de.metadata().ok()
        };

        let (size, is_dir, modified) = match meta {
            Some(m) => (m.len(), m.is_dir(), m.modified().ok()),
            None => (0, ft.as_ref().is_some_and(|f| f.is_dir()), None),
        };

        let kind = if is_dir {
            FileKind::Unknown
        } else {
            FileKind::infer(&path)
        };
        let tokens_est = estimate_tokens(size);

        Entry {
            path,
            size,
            tokens_est,
            is_dir,
            is_symlink,
            modified,
            kind,
        }
    }

    /// Build an Entry from DirEntry with NO full stat — uses file_type() only
    /// (free on Linux) and estimates tokens from extension heuristics.
    pub(crate) fn from_dir_entry_lightweight(path: PathBuf, de: &fs::DirEntry) -> Self {
        let ft = de.file_type().ok();
        let is_dir = ft.as_ref().is_some_and(|f| {
            f.is_dir() || (f.is_symlink() && fs::metadata(&path).is_ok_and(|m| m.is_dir()))
        });
        let is_symlink = ft.as_ref().is_some_and(|f| f.is_symlink());
        let kind = if is_dir { FileKind::Unknown } else { FileKind::infer(&path) };
        let tokens_est = if is_dir { 0 } else { estimate_tokens_by_extension(&path) };

        Entry {
            path,
            size: 0,
            tokens_est,
            is_dir,
            is_symlink,
            modified: None,
            kind,
        }
    }

    /// A directory the walker passes through without yielding: no stat.
    pub(crate) fn bare_dir(path: PathBuf, is_symlink: bool) -> Self {
        Entry {
            path,
            size: 0,
            tokens_est: 0,
            is_dir: true,
            is_symlink,
            modified: None,
            kind: FileKind::Unknown,
        }
    }

    /// Build an Entry from a path with minimal stat — just enough to know if it's a dir.
    pub(crate) fn from_path_lightweight(path: PathBuf) -> Self {
        let is_dir = fs::metadata(&path).is_ok_and(|m| m.is_dir());
        let kind = if is_dir { FileKind::Unknown } else { FileKind::infer(&path) };
        let tokens_est = if is_dir { 0 } else { estimate_tokens_by_extension(&path) };

        Entry {
            path,
            size: 0,
            tokens_est,
            is_dir,
            is_symlink: false,
            modified: None,
            kind,
        }
    }
}

/// Estimate token count from byte size.
///
/// Heuristic: ~3.5 bytes per token for English source code.
/// This is intentionally conservative (overestimates tokens).
fn estimate_tokens(bytes: u64) -> u64 {
    // bytes / 3.5, rounded up.
    (bytes.saturating_mul(2)).div_ceil(7)
}

/// Estimate tokens from file extension when stat is unavailable.
///
/// Uses median file sizes by extension from typical codebases.
/// These are rough heuristics — better than 0, worse than stat.
fn estimate_tokens_by_extension(path: &Path) -> u64 {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let avg_bytes: u64 = match ext {
        // Source files — median sizes from typical projects
        "rs" | "go" | "java" | "kt" | "scala" => 3_000,
        "py" | "rb" | "php" | "pl" | "pm" => 2_500,
        "js" | "ts" | "tsx" | "jsx" => 4_000,
        "c" | "cpp" | "cc" | "cxx" => 5_000,
        "h" | "hpp" => 2_000,
        "swift" | "m" | "mm" => 3_500,
        "zig" | "nim" | "v" | "d" => 2_500,
        "lua" | "ex" | "exs" | "erl" => 2_000,
        "hs" | "ml" | "mli" | "elm" => 2_000,
        "sh" | "bash" | "zsh" | "fish" => 1_500,
        "css" | "scss" | "less" => 3_000,
        "html" | "htm" | "vue" | "svelte" => 5_000,
        "sql" => 3_000,
        "proto" => 2_000,
        "sil" => 1_500,
        // Config files
        "toml" | "yaml" | "yml" | "json" | "xml" => 1_500,
        "ini" | "cfg" | "conf" | "env" => 500,
        // Docs
        "md" | "rst" | "txt" | "adoc" => 4_000,
        // Data
        "csv" | "tsv" | "sif" | "jsonl" => 10_000,
        _ => 2_000,
    };
    estimate_tokens(avg_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn classify_rust_source() {
        assert_eq!(FileKind::infer(Path::new("src/main.rs")), FileKind::Source);
    }

    #[test]
    fn classify_test() {
        assert_eq!(
            FileKind::infer(Path::new("tests/unit_test.rs")),
            FileKind::Test
        );
    }

    #[test]
    fn classify_config() {
        assert_eq!(
            FileKind::infer(Path::new("Cargo.toml")),
            FileKind::Config
        );
    }

    #[test]
    fn classify_lock() {
        assert_eq!(
            FileKind::infer(Path::new("Cargo.lock")),
            FileKind::Generated
        );
    }

    #[test]
    fn classify_binary() {
        assert_eq!(
            FileKind::infer(Path::new("image.png")),
            FileKind::Binary
        );
    }

    #[test]
    fn classify_by_component_not_substring() {
        // Root-level build dirs count; names merely containing "test" don't.
        assert_eq!(FileKind::infer(Path::new("target/debug/x.rs")), FileKind::Build);
        assert_eq!(FileKind::infer(Path::new("node_modules/a/index.js")), FileKind::Build);
        assert_eq!(FileKind::infer(Path::new("target/debug/tests/t.rs")), FileKind::Build);
        assert_eq!(FileKind::infer(Path::new("src/contest/x.rs")), FileKind::Source);
        assert_eq!(FileKind::infer(Path::new("src/latest.rs")), FileKind::Source);
        assert_eq!(FileKind::infer(Path::new("__tests__/a.js")), FileKind::Test);
        assert_eq!(FileKind::infer(Path::new("web/a.test.tsx")), FileKind::Test);
        assert_eq!(FileKind::infer(Path::new("FooTest.java")), FileKind::Test);
    }

    #[test]
    fn classify_newer_extensions() {
        for p in ["a.mjs", "a.cjs", "a.mts", "a.cs", "a.kts", "a.sol", "a.cu"] {
            assert_eq!(FileKind::infer(Path::new(p)), FileKind::Source, "{}", p);
        }
        assert_eq!(FileKind::infer(Path::new("icon.svg")), FileKind::Data);
        assert_eq!(FileKind::infer(Path::new("requirements.txt")), FileKind::Config);
        assert_eq!(FileKind::infer(Path::new("main.tf")), FileKind::Config);
        assert_eq!(FileKind::infer(Path::new("go.sum")), FileKind::Generated);
        assert_eq!(FileKind::infer(Path::new("app.min.js")), FileKind::Generated);
        assert_eq!(FileKind::infer(Path::new("LICENSE.txt")), FileKind::Doc);
        assert_eq!(FileKind::infer(Path::new("model.safetensors")), FileKind::Binary);
    }

    #[test]
    fn classify_doc() {
        assert_eq!(FileKind::infer(Path::new("README.md")), FileKind::Doc);
    }

    #[test]
    fn classify_sif() {
        assert_eq!(
            FileKind::infer(Path::new("data.sif")),
            FileKind::Data
        );
    }

    #[test]
    fn classify_sil() {
        assert_eq!(
            FileKind::infer(Path::new("pipeline.sil")),
            FileKind::Source
        );
    }

    #[test]
    fn token_estimate() {
        assert_eq!(estimate_tokens(0), 0);
        assert_eq!(estimate_tokens(7), 2); // 7 / 3.5 = 2
        assert_eq!(estimate_tokens(100), 29); // 100 / 3.5 ≈ 28.6 → 29
    }
}
