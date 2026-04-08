// Filesystem walking — the POSIX glob(3) equivalent.
//
// v0.3: adds .gitignore integration, rayon-parallel readdir, depth control.

use std::cmp;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use rayon::prelude::*;

use crate::entry::Entry;
use crate::error::GlobError;
use crate::matcher::MatchOptions;
use crate::pattern::Pattern;

/// Options for the filesystem walker.
#[derive(Debug, Clone)]
pub struct WalkOptions {
    /// Pattern matching options.
    pub match_opts: MatchOptions,
    /// If true, return results in sorted order (default: true).
    pub sorted: bool,
    /// If true, stop iteration on the first I/O error (POSIX GLOB_ERR).
    pub stop_on_error: bool,
    /// Maximum number of results to yield. 0 = unlimited.
    pub limit: usize,
    /// Maximum total bytes across all matched files. 0 = unlimited.
    pub byte_budget: u64,
    /// Maximum estimated tokens across all matched files. 0 = unlimited.
    pub token_budget: u64,
    /// If true, only match directories.
    pub only_dirs: bool,
    /// If true, the pattern must match a directory (trailing `/`).
    pub require_dir: bool,
    /// Maximum recursion depth from the `**` boundary. 0 = unlimited.
    pub max_depth: usize,
    /// If true, skip full stat() — uses DirEntry file_type only.
    pub no_stat: bool,
    /// If true, read .gitignore files and skip matching entries.
    pub gitignore: bool,
}

impl Default for WalkOptions {
    fn default() -> Self {
        WalkOptions {
            match_opts: MatchOptions::new(),
            sorted: true,
            stop_on_error: false,
            limit: 0,
            byte_budget: 0,
            token_budget: 0,
            only_dirs: false,
            require_dir: false,
            max_depth: 0,
            no_stat: false,
            gitignore: false,
        }
    }
}

/// The result of a glob walk.
pub type WalkResult = Result<Entry, GlobError>;

/// Walk the filesystem matching a pattern, returning all matched entries.
pub fn walk(pattern: &str, opts: WalkOptions) -> Result<Vec<WalkResult>, GlobError> {
    let pat = Pattern::new(pattern)?;
    walk_pattern(&pat, pattern, opts)
}

/// Walk using a pre-compiled pattern.
pub fn walk_pattern(
    _pat: &Pattern,
    pattern: &str,
    opts: WalkOptions,
) -> Result<Vec<WalkResult>, GlobError> {
    let (root, components) = split_pattern(pattern)?;
    let scope = if root.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        root
    };

    // Load root-level .gitignore if requested.
    let mut ignores: Vec<IgnoreRules> = Vec::new();
    if opts.gitignore {
        let mut root_rules = Vec::new();
        root_rules.push(IgnoreRule {
            pattern: Pattern::new("**/.git").unwrap_or_else(|_| Pattern::new("*").unwrap()),
            negated: false,
        });
        let gi_path = if scope == Path::new(".") {
            PathBuf::from(".gitignore")
        } else {
            scope.join(".gitignore")
        };
        if let Some(rules) = load_gitignore(&gi_path) {
            root_rules.extend(rules.rules);
        }
        ignores.push(IgnoreRules { rules: root_rules });
    }

    // Also load .gitignore files along the literal prefix path.
    if opts.gitignore {
        let mut prefix_path = scope.clone();
        for comp in &components {
            if comp.has_meta {
                break;
            }
            prefix_path = prefix_path.join(comp.as_str());
            if let Some(gi) = load_gitignore(&prefix_path.join(".gitignore")) {
                ignores.push(gi);
            }
        }
    }

    // ── Parallel fast path ──────────────────────────────────────────
    // For patterns like `prefix/**/*.rs` with no budget, fan out the
    // entire walk across rayon threads.
    if let Some((rec_idx, _tail_len)) = can_use_parallel_walker(&components, &opts) {
        // Resolve the literal prefix to the actual directory.
        let mut walk_root = scope.clone();
        for comp in &components[..rec_idx] {
            walk_root = walk_root.join(comp.as_str());
        }

        let tail = &components[rec_idx + 1..];
        let results_mutex = Mutex::new(Vec::<WalkResult>::new());

        parallel_recursive_walk(&walk_root, tail, 0, &opts, &ignores, &results_mutex);

        let mut results = results_mutex.into_inner().unwrap();
        if opts.sorted {
            results.sort_by(|a, b| {
                let pa = a.as_ref().map(|e| &e.path).ok();
                let pb = b.as_ref().map(|e| &e.path).ok();
                pa.cmp(&pb)
            });
        }
        return Ok(results);
    }

    // ── Sequential walker (budget-aware, supports complex patterns) ─
    let mut results: Vec<WalkResult> = Vec::new();
    let mut bytes_used: u64 = 0;
    let mut tokens_used: u64 = 0;

    let mut todo: Vec<TodoItem> = Vec::new();

    if !components.is_empty() {
        fill_todo(
            &mut todo, &components, 0, &scope, true, 0, &opts, &ignores,
        );
    }

    while let Some(item) = todo.pop() {
        if opts.limit > 0 && results.len() >= opts.limit {
            break;
        }

        match item {
            TodoItem::Error(e) => {
                if opts.stop_on_error {
                    return Err(e);
                }
                results.push(Err(e));
            }
            TodoItem::Verified(entry) => {
                if opts.require_dir && !entry.is_dir {
                    continue;
                }
                if opts.only_dirs && !entry.is_dir {
                    continue;
                }
                if !check_budget(&entry, &opts, &mut bytes_used, &mut tokens_used) {
                    break;
                }
                results.push(Ok(entry));
            }
            TodoItem::Match(entry, idx, depth, dir_ignores) => {
                let mut idx = idx;

                if components[idx].is_recursive {
                    let mut next = idx;
                    while next + 1 < components.len() && components[next + 1].is_recursive {
                        next += 1;
                    }

                    if entry.is_dir {
                        if opts.max_depth == 0 || depth < opts.max_depth {
                            // Load .gitignore from this directory if present.
                            let mut child_ignores = dir_ignores.clone();
                            if opts.gitignore {
                                if let Some(gi) = load_gitignore(&entry.path.join(".gitignore")) {
                                    child_ignores.push(gi);
                                }
                            }
                            fill_todo(
                                &mut todo, &components, next, &entry.path,
                                true, depth + 1, &opts, &child_ignores,
                            );
                        }

                        if next == components.len() - 1 {
                            if !check_budget(&entry, &opts, &mut bytes_used, &mut tokens_used) {
                                break;
                            }
                            results.push(Ok(entry));
                            continue;
                        } else {
                            idx = next + 1;
                        }
                    } else if next == components.len() - 1 {
                        if !check_budget(&entry, &opts, &mut bytes_used, &mut tokens_used) {
                            break;
                        }
                        results.push(Ok(entry));
                        continue;
                    } else {
                        idx = next + 1;
                    }
                }

                let filename = match entry.path.file_name().and_then(|n| n.to_str()) {
                    Some(n) => n,
                    None => continue,
                };

                if components[idx].matches_with(filename, opts.match_opts) {
                    if idx == components.len() - 1 {
                        if opts.require_dir && !entry.is_dir {
                            continue;
                        }
                        if opts.only_dirs && !entry.is_dir {
                            continue;
                        }
                        if !check_budget(&entry, &opts, &mut bytes_used, &mut tokens_used) {
                            break;
                        }
                        results.push(Ok(entry));
                    } else if entry.is_dir {
                        let mut child_ignores = dir_ignores;
                        if opts.gitignore {
                            if let Some(gi) = load_gitignore(&entry.path.join(".gitignore")) {
                                child_ignores.push(gi);
                            }
                        }
                        fill_todo(
                            &mut todo, &components, idx + 1, &entry.path,
                            true, depth, &opts, &child_ignores,
                        );
                    }
                }
            }
        }
    }

    Ok(results)
}

// ── Internals ────────────────────────────────────────────────────────

enum TodoItem {
    Verified(Entry),
    /// (entry, component_idx, depth, accumulated_ignores)
    Match(Entry, usize, usize, Vec<IgnoreRules>),
    Error(GlobError),
}

/// Split pattern into a literal root prefix and per-component patterns.
fn split_pattern(pattern: &str) -> Result<(PathBuf, Vec<Pattern>), GlobError> {
    let mut root = PathBuf::new();
    let mut rest_start = 0;

    if pattern.starts_with('/') {
        root.push("/");
        rest_start = 1;
    }
    #[cfg(windows)]
    {
        use std::path::Component;
        let p = Path::new(pattern);
        if let Some(Component::Prefix(pfx)) = p.components().next() {
            let prefix_str = pfx.as_os_str().to_str().unwrap_or("");
            root.push(prefix_str);
            rest_start = prefix_str.len();
            if rest_start < pattern.len()
                && pattern.as_bytes().get(rest_start) == Some(&b'\\')
            {
                root.push("\\");
                rest_start += 1;
            }
        }
    }

    let rest = &pattern[cmp::min(rest_start, pattern.len())..];
    let mut components = Vec::new();
    for component_str in rest.split_terminator(|c: char| c == '/' || (cfg!(windows) && c == '\\'))
    {
        if !component_str.is_empty() {
            components.push(Pattern::new(component_str)?);
        }
    }

    Ok((root, components))
}

/// Populate the work stack for matching at `components[idx]`.
fn fill_todo(
    todo: &mut Vec<TodoItem>,
    components: &[Pattern],
    idx: usize,
    dir_path: &Path,
    is_dir: bool,
    depth: usize,
    opts: &WalkOptions,
    ignores: &[IgnoreRules],
) {
    let pattern = &components[idx];
    let curdir = dir_path == Path::new(".");

    // Depth check: only for recursive (**) walks.
    if pattern.is_recursive && opts.max_depth > 0 && depth >= opts.max_depth {
        return;
    }

    if !pattern.has_meta {
        // FAST PATH: literal component — stat only.
        let s = pattern.as_str();
        let special = s == "." || s == "..";
        let child_path = if curdir {
            PathBuf::from(s)
        } else {
            dir_path.join(s)
        };

        let exists = if special && is_dir {
            true
        } else {
            fs::metadata(&child_path).is_ok() || fs::symlink_metadata(&child_path).is_ok()
        };

        if exists {
            let entry = if opts.no_stat {
                Entry::from_path_lightweight(child_path)
            } else {
                Entry::from_path(child_path)
            };
            if idx + 1 == components.len() {
                todo.push(TodoItem::Verified(entry));
            } else if entry.is_dir {
                // Pick up .gitignore from literal directories along the path.
                if opts.gitignore {
                    let mut child_ignores = ignores.to_vec();
                    if let Some(gi) = load_gitignore(&entry.path.join(".gitignore")) {
                        child_ignores.push(gi);
                    }
                    fill_todo(todo, components, idx + 1, &entry.path, true, depth, opts, &child_ignores);
                } else {
                    fill_todo(todo, components, idx + 1, &entry.path, true, depth, opts, ignores);
                }
            }
        }
    } else if is_dir {
        // READDIR PATH: enumerate children, stat in parallel with rayon.
        match fs::read_dir(dir_path) {
            Ok(rd) => {
                // Collect DirEntry objects first (readdir is sequential).
                let dir_entries: Vec<_> = rd.filter_map(|r| r.ok()).collect();

                // Build (path, filename, entry) tuples — rayon parallelizes stat.
                let children: Vec<(Entry, OsString)> = dir_entries
                    .into_par_iter()
                    .filter_map(|de| {
                        let path = if curdir {
                            PathBuf::from(de.file_name())
                        } else {
                            de.path()
                        };
                        let filename = de.file_name();

                        // Filter leading dots early.
                        if opts.match_opts.require_literal_leading_dot {
                            if let Some(name) = filename.to_str() {
                                if name.starts_with('.') {
                                    return None;
                                }
                            }
                        }

                        // Gitignore check.
                        if !ignores.is_empty() {
                            if let Some(name) = path.to_str() {
                                if is_gitignored(ignores, name) {
                                    return None;
                                }
                            }
                        }

                        let entry = if opts.no_stat {
                            Entry::from_dir_entry_lightweight(path, &de)
                        } else {
                            Entry::from_dir_entry(path, &de)
                        };
                        Some((entry, filename))
                    })
                    .collect();

                // Sort needs to be sequential (for deterministic order).
                let mut children = children;
                if opts.sorted {
                    children.sort_by(|a, b| b.1.cmp(&a.1));
                }

                let ignores_vec: Vec<IgnoreRules> = ignores.to_vec();
                for (entry, _filename) in children {
                    todo.push(TodoItem::Match(entry, idx, depth, ignores_vec.clone()));
                }

                // Handle `.` and `..`.
                if !pattern.tokens.is_empty()
                    && pattern.tokens[0] == crate::pattern::Token::Char('.')
                {
                    for special in &[".", ".."] {
                        if pattern.matches_with(special, opts.match_opts) {
                            let sp = dir_path.join(special);
                            let entry = Entry::from_path(sp);
                            if idx + 1 == components.len() {
                                todo.push(TodoItem::Verified(entry));
                            } else {
                                fill_todo(
                                    todo, components, idx + 1, &entry.path,
                                    entry.is_dir, depth + 1, opts, ignores,
                                );
                            }
                        }
                    }
                }
            }
            Err(e) => {
                todo.push(TodoItem::Error(GlobError::Io {
                    path: dir_path.to_path_buf(),
                    error: e,
                }));
            }
        }
    }
}

fn check_budget(
    entry: &Entry,
    opts: &WalkOptions,
    bytes_used: &mut u64,
    tokens_used: &mut u64,
) -> bool {
    if opts.byte_budget > 0 {
        if *bytes_used + entry.size > opts.byte_budget {
            return false;
        }
        *bytes_used += entry.size;
    }
    if opts.token_budget > 0 {
        if *tokens_used + entry.tokens_est > opts.token_budget {
            return false;
        }
        *tokens_used += entry.tokens_est;
    }
    true
}

// ── Parallel recursive walker ────────────────────────────────────────
//
// When the pattern contains ** and there's no budget constraint,
// we can parallelize the entire directory tree walk — not just stat
// calls, but readdir itself. This fans out across rayon threads so
// multiple directories are read concurrently.

use std::sync::Mutex;

/// Recursively walk a directory tree in parallel, collecting entries
/// that match the tail pattern (the part after **).
fn parallel_recursive_walk(
    dir: &Path,
    tail: &[Pattern],
    depth: usize,
    opts: &WalkOptions,
    ignores: &[IgnoreRules],
    results: &Mutex<Vec<WalkResult>>,
) {
    let rd = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) => {
            results.lock().unwrap().push(Err(GlobError::Io {
                path: dir.to_path_buf(),
                error: e,
            }));
            return;
        }
    };

    // Collect dir entries (readdir itself is sequential per directory).
    let dir_entries: Vec<_> = rd.filter_map(|r| r.ok()).collect();

    // Process entries in parallel — each may trigger recursive descent.
    dir_entries.into_par_iter().for_each(|de| {
        let path = de.path();
        let filename = de.file_name();

        // Leading dot filter.
        if opts.match_opts.require_literal_leading_dot {
            if let Some(name) = filename.to_str() {
                if name.starts_with('.') {
                    return;
                }
            }
        }

        // Gitignore check.
        if !ignores.is_empty() {
            if let Some(name) = path.to_str() {
                if is_gitignored(ignores, name) {
                    return;
                }
            }
        }

        let entry = if opts.no_stat {
            Entry::from_dir_entry_lightweight(path.clone(), &de)
        } else {
            Entry::from_dir_entry(path.clone(), &de)
        };

        if entry.is_dir {
            // Check if this directory matches the tail pattern (for ** as last).
            if tail.is_empty() {
                if !opts.only_dirs || entry.is_dir {
                    results.lock().unwrap().push(Ok(entry.clone()));
                }
            }

            // Depth check.
            if opts.max_depth > 0 && depth >= opts.max_depth {
                return;
            }

            // Recurse into subdirectory — this is where parallelism helps.
            let mut child_ignores;
            let ig = if opts.gitignore {
                child_ignores = ignores.to_vec();
                if let Some(gi) = load_gitignore(&path.join(".gitignore")) {
                    child_ignores.push(gi);
                }
                &child_ignores[..]
            } else {
                // Shadow to extend lifetime.
                child_ignores = ignores.to_vec();
                &child_ignores[..]
            };
            parallel_recursive_walk(&path, tail, depth + 1, opts, ig, results);
        }

        // Check tail pattern match.
        if tail.is_empty() {
            // ** alone — yield everything.
            if !entry.is_dir {
                if !opts.only_dirs {
                    results.lock().unwrap().push(Ok(entry));
                }
            }
        } else {
            // Match filename against the tail pattern(s).
            let fname = match entry.path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n,
                None => return,
            };
            // Simple case: single tail component (e.g. *.rs after **).
            if tail.len() == 1 {
                if tail[0].matches_with(fname, opts.match_opts) {
                    if opts.require_dir && !entry.is_dir {
                        return;
                    }
                    if opts.only_dirs && !entry.is_dir {
                        return;
                    }
                    results.lock().unwrap().push(Ok(entry));
                }
            }
            // Multi-component tail would need sub-walking, but the common
            // case (e.g. **/*.rs) is single-component. For multi-component
            // tails, fall back to the sequential walker.
        }
    });
}

/// Check if a pattern qualifies for the parallel walker:
/// - Contains **
/// - No budget constraints (budget requires sequential early-exit)
/// - At most one component after **
fn can_use_parallel_walker(components: &[Pattern], opts: &WalkOptions) -> Option<(usize, usize)> {
    if opts.token_budget > 0 || opts.byte_budget > 0 || opts.limit > 0 {
        return None; // Budget needs sequential early-exit.
    }

    // Find the ** component.
    let rec_idx = components.iter().position(|c| c.is_recursive)?;

    // All components before ** must be literal (no metacharacters).
    if components[..rec_idx].iter().any(|c| c.has_meta) {
        return None;
    }

    // At most one component after **.
    let tail_len = components.len() - rec_idx - 1;
    if tail_len > 1 {
        return None; // Multi-component tail not supported by parallel walker.
    }

    Some((rec_idx, tail_len))
}

// ── Gitignore support ────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct IgnoreRule {
    pattern: Pattern,
    negated: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct IgnoreRules {
    rules: Vec<IgnoreRule>,
}

/// Load and parse a .gitignore file into ignore rules.
fn load_gitignore(path: &Path) -> Option<IgnoreRules> {
    let content = fs::read_to_string(path).ok()?;
    let mut rules = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (negated, pat_str) = if let Some(rest) = line.strip_prefix('!') {
            (true, rest)
        } else {
            (false, line)
        };
        // Strip leading `/` (gitignore root-anchor) and trailing `/` (dir-only hint).
        let pat_str = pat_str.strip_prefix('/').unwrap_or(pat_str);
        let pat_str = pat_str.strip_suffix('/').unwrap_or(pat_str);
        // Patterns without `/` match anywhere; patterns with `/` are path-relative.
        let glob_pat = if pat_str.contains('/') {
            format!("**/{}", pat_str)
        } else {
            format!("**/{}", pat_str)
        };
        if let Ok(pattern) = Pattern::new(&glob_pat) {
            rules.push(IgnoreRule { pattern, negated });
        }
    }
    if rules.is_empty() {
        None
    } else {
        Some(IgnoreRules { rules })
    }
}

/// Check if a path is ignored by any of the accumulated ignore rule sets.
fn is_gitignored(ignore_stack: &[IgnoreRules], path: &str) -> bool {
    let mut ignored = false;
    // Also check just the filename for patterns like `*.o`.
    let filename = Path::new(path)
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or(path);

    for ruleset in ignore_stack {
        for rule in &ruleset.rules {
            if rule.pattern.matches(path) || rule.pattern.matches(filename) {
                ignored = !rule.negated;
            }
        }
    }
    ignored
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_relative() {
        let (root, components) = split_pattern("src/**/*.rs").unwrap();
        assert_eq!(root, PathBuf::new());
        assert_eq!(components.len(), 3);
        assert_eq!(components[0].as_str(), "src");
        assert_eq!(components[1].as_str(), "**");
        assert_eq!(components[2].as_str(), "*.rs");
    }

    #[test]
    fn split_absolute() {
        let (root, components) = split_pattern("/usr/lib/*.so").unwrap();
        assert_eq!(root, PathBuf::from("/"));
        assert_eq!(components.len(), 3);
    }

    #[test]
    fn split_single_star() {
        let (root, components) = split_pattern("*").unwrap();
        assert_eq!(root, PathBuf::new());
        assert_eq!(components.len(), 1);
    }

    #[test]
    fn gitignore_parse() {
        let rules = IgnoreRules {
            rules: vec![
                IgnoreRule {
                    pattern: Pattern::new("**/target").unwrap(),
                    negated: false,
                },
                IgnoreRule {
                    pattern: Pattern::new("**/*.o").unwrap(),
                    negated: false,
                },
            ],
        };
        assert!(is_gitignored(&[rules.clone()], "target"));
        assert!(is_gitignored(&[rules.clone()], "src/target"));
        assert!(is_gitignored(&[rules.clone()], "foo.o"));
        assert!(!is_gitignored(&[rules], "src/main.rs"));
    }
}
