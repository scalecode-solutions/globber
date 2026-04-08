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
// These map to SIF Classify codes when emitted as SIF.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Classification of a file's role in a project.
///
/// Maps to SIF Classify codes in the 300 range (references/resources):
///   Source     → 310 (source code)
///   Test       → 312 (test code)
///   Config     → 320 (configuration)
///   Build      → 321 (build artifact)
///   Doc        → 330 (documentation)
///   Data       → 340 (data file)
///   Generated  → 350 (generated code)
///   Binary     → 360 (binary/non-text)
///   Unknown    → 300 (unclassified resource)
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

impl FileKind {
    /// SIF Classify code for this kind.
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
    pub fn infer(path: &Path) -> Self {
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => return FileKind::Unknown,
        };
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let path_str = path.to_str().unwrap_or("");

        // Check path components for test indicators.
        if path_str.contains("/test/")
            || path_str.contains("/tests/")
            || path_str.contains("/spec/")
            || path_str.contains("/fixtures/")
            || path_str.starts_with("test/")
            || path_str.starts_with("tests/")
            || path_str.starts_with("spec/")
            || path_str.starts_with("fixtures/")
            || name.starts_with("test_")
            || name.ends_with("_test.rs")
            || name.ends_with("_test.go")
            || name.ends_with("_test.py")
            || name.ends_with(".test.ts")
            || name.ends_with(".test.js")
            || name.ends_with(".spec.ts")
            || name.ends_with(".spec.js")
        {
            return FileKind::Test;
        }

        // Check for generated file markers in name.
        if name.ends_with(".generated.rs")
            || name.ends_with(".gen.go")
            || name.ends_with(".pb.go")
            || name.ends_with(".pb.rs")
            || name == "Cargo.lock"
            || name == "package-lock.json"
            || name == "yarn.lock"
            || name == "pnpm-lock.yaml"
            || name == "Gemfile.lock"
            || name == "Pipfile.lock"
            || name == "poetry.lock"
            || name == "composer.lock"
        {
            return FileKind::Generated;
        }

        // Build artifacts by path.
        if path_str.contains("/target/")
            || path_str.contains("/build/")
            || path_str.contains("/dist/")
            || path_str.contains("/node_modules/")
            || path_str.contains("/__pycache__/")
        {
            return FileKind::Build;
        }

        // Config files by name.
        match name {
            "Cargo.toml" | "Makefile" | "CMakeLists.txt" | "build.rs"
            | "build.gradle" | "pom.xml" | "package.json" | "tsconfig.json"
            | "pyproject.toml" | "setup.py" | "setup.cfg" | "tox.ini"
            | ".gitignore" | ".gitattributes" | ".editorconfig"
            | ".eslintrc.json" | ".prettierrc" | "Dockerfile" | "docker-compose.yml"
            | ".env" | ".env.example" | "Gemfile" | "Rakefile" => {
                return FileKind::Config;
            }
            _ => {}
        }

        // Config by extension.
        match ext {
            "toml" | "yaml" | "yml" | "ini" | "cfg" | "conf" | "env" => {
                return FileKind::Config;
            }
            _ => {}
        }

        // Documentation.
        match ext {
            "md" | "rst" | "txt" | "adoc" | "org" => return FileKind::Doc,
            _ => {}
        }
        if name == "LICENSE" || name == "LICENSE-MIT" || name == "LICENSE-APACHE"
            || name == "CHANGELOG" || name == "CHANGELOG.md"
            || name == "README" || name == "README.md"
        {
            return FileKind::Doc;
        }

        // Data files.
        match ext {
            "json" | "jsonl" | "csv" | "tsv" | "xml" | "sif" | "sql"
            | "parquet" | "avro" | "ndjson" => return FileKind::Data,
            _ => {}
        }

        // Binary files.
        match ext {
            "png" | "jpg" | "jpeg" | "gif" | "bmp" | "ico" | "svg"
            | "woff" | "woff2" | "ttf" | "otf" | "eot"
            | "zip" | "tar" | "gz" | "bz2" | "xz" | "zst" | "7z"
            | "exe" | "dll" | "so" | "dylib" | "a" | "o" | "obj"
            | "wasm" | "class" | "pyc" | "pyd" | "pdb"
            | "mp3" | "mp4" | "wav" | "avi" | "mov" | "mkv"
            | "pdf" => return FileKind::Binary,
            _ => {}
        }

        // Template / generated files by extension.
        match ext {
            "in" | "j2" | "jinja" | "jinja2" | "erb" | "ejs" | "hbs"
            | "mustache" | "tmpl" | "tpl" | "tt" | "tt2" => {
                return FileKind::Generated;
            }
            _ => {}
        }

        // Source code by extension.
        match ext {
            "rs" | "go" | "py" | "js" | "ts" | "tsx" | "jsx"
            | "c" | "h" | "cpp" | "hpp" | "cc" | "cxx"
            | "java" | "kt" | "scala" | "clj" | "ex" | "exs"
            | "rb" | "php" | "swift" | "m" | "mm"
            | "zig" | "nim" | "v" | "d" | "lua" | "pl" | "pm"
            | "sh" | "bash" | "zsh" | "fish" | "ps1"
            | "css" | "scss" | "less" | "html" | "htm"
            | "vue" | "svelte" | "astro"
            | "r" | "R" | "jl" | "hs" | "ml" | "mli" | "fs" | "fsx"
            | "erl" | "hrl" | "elm" | "dart" | "proto"
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
    /// Estimated token count (size / 3.5, rounded up).
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
        let is_symlink = ft.as_ref().map_or(false, |f| f.is_symlink());

        // On Linux, DirEntry gives us file_type for free from readdir.
        // For symlinks or if file_type is unavailable, fall back to stat.
        let meta = if is_symlink || ft.is_none() {
            fs::metadata(&path).ok()
        } else {
            de.metadata().ok()
        };

        let (size, is_dir, modified) = match meta {
            Some(m) => (m.len(), m.is_dir(), m.modified().ok()),
            None => (0, ft.as_ref().map_or(false, |f| f.is_dir()), None),
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
        let is_dir = ft.as_ref().map_or(false, |f| {
            f.is_dir() || (f.is_symlink() && fs::metadata(&path).map_or(false, |m| m.is_dir()))
        });
        let is_symlink = ft.as_ref().map_or(false, |f| f.is_symlink());
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

    /// Build an Entry from a path with minimal stat — just enough to know if it's a dir.
    pub(crate) fn from_path_lightweight(path: PathBuf) -> Self {
        let is_dir = fs::metadata(&path).map_or(false, |m| m.is_dir());
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
    // (bytes * 2 + 6) / 7 ≈ bytes / 3.5, rounded up.
    (bytes * 2 + 6) / 7
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
