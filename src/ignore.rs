// Gitignore support.
//
// Implements gitignore(5) matching:
//   - blank lines and `#` comments are skipped; `\#` and `\!` are literal
//   - trailing spaces are dropped unless escaped with `\`
//   - `!pattern` re-includes (negation)
//   - `pattern/` matches directories only
//   - a pattern containing `/` (other than a trailing one) is anchored to
//     the directory of the file that defines it; otherwise it matches the
//     entry name at any depth below that directory
//   - later rules override earlier ones, and deeper files override
//     shallower ones, then .git/info/exclude, then the global excludes file
//
// Excluded directories are pruned by the walker, so (as in git) a file
// cannot be re-included if a parent directory is excluded.
//
// The walker keeps an Arc-linked stack of ignore files so entering a
// directory extends its parent's rules without copying them.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::matcher::MatchOptions;
use crate::pattern::Pattern;

/// Anchored rules: `*` does not cross `/`, dotfiles are not special.
const PATH_OPTS: MatchOptions = MatchOptions {
    case_sensitive: true,
    require_literal_separator: true,
    require_literal_leading_dot: false,
};

#[derive(Debug, Clone)]
struct Rule {
    pattern: Pattern,
    negated: bool,
    dir_only: bool,
    anchored: bool,
}

/// The rules from one ignore file, and where they apply.
#[derive(Debug, Clone)]
pub(crate) struct IgnoreFile {
    rules: Vec<Rule>,
    /// Directory the rules are relative to, in the caller's path space
    /// ("" for the current directory).
    anchor: PathBuf,
    /// For files above the walk root: the path from the file's directory
    /// down to `anchor`, prepended to relative paths before matching.
    prefix: PathBuf,
}

impl IgnoreFile {
    /// Parse gitignore-format text. Rules apply to paths under `anchor`.
    pub(crate) fn parse(content: &str, anchor: PathBuf) -> Option<Self> {
        let rules: Vec<Rule> = content.lines().filter_map(parse_rule).collect();
        if rules.is_empty() {
            None
        } else {
            Some(IgnoreFile { rules, anchor: normalize(anchor), prefix: PathBuf::new() })
        }
    }

    /// Load an ignore file whose rules apply under `anchor`.
    pub(crate) fn load(path: &Path, anchor: PathBuf) -> Option<Self> {
        Self::parse(&fs::read_to_string(path).ok()?, anchor)
    }

    fn with_prefix(mut self, prefix: PathBuf) -> Self {
        self.prefix = prefix;
        self
    }

    /// The last rule matching `path`: Some(true) = re-included,
    /// Some(false) = ignored, None = no rule applies.
    fn decide(&self, path: &Path, is_dir: bool) -> Option<bool> {
        let rel = path.strip_prefix(&self.anchor).ok()?;
        let joined;
        let rel = if self.prefix.as_os_str().is_empty() {
            rel
        } else {
            joined = self.prefix.join(rel);
            &joined
        };
        let rel_str = rel.to_str()?;
        let name = rel.file_name()?.to_str()?;
        self.rules.iter().rev().find_map(|rule| {
            if rule.dir_only && !is_dir {
                return None;
            }
            let hit = if rule.anchored {
                rule.pattern.matches_with(rel_str, PATH_OPTS)
            } else {
                rule.pattern.matches_with(name, PATH_OPTS)
            };
            hit.then_some(rule.negated)
        })
    }

    /// Whether `path` is ignored, treating each of its parent directories
    /// as a directory first (for path-only checks without a walk).
    pub(crate) fn is_path_ignored(files: &[IgnoreFile], path: &Path, is_dir: bool) -> bool {
        let stack = files.iter().fold(IgnoreStack::default(), |s, f| s.push(f.clone()));
        let mut prefix = PathBuf::new();
        let comps: Vec<_> = path.components().collect();
        for (i, c) in comps.iter().enumerate() {
            prefix.push(c);
            let last = i + 1 == comps.len();
            if stack.is_ignored(&prefix, if last { is_dir } else { true }) {
                return true;
            }
        }
        false
    }
}

fn normalize(anchor: PathBuf) -> PathBuf {
    if anchor == Path::new(".") { PathBuf::new() } else { anchor }
}

fn parse_rule(line: &str) -> Option<Rule> {
    // Drop trailing whitespace unless the last space is escaped.
    let mut line = line.trim_end_matches(['\r', '\n']);
    while line.ends_with(' ') && !line.ends_with("\\ ") {
        line = &line[..line.len() - 1];
    }
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let (negated, pat) = match line.strip_prefix('!') {
        Some(rest) => (true, rest),
        None => (false, line),
    };
    let (dir_only, pat) = match pat.strip_suffix('/') {
        Some(rest) => (true, rest),
        None => (false, pat),
    };
    let anchored = pat.contains('/');
    let pat = pat.strip_prefix('/').unwrap_or(pat);
    if pat.is_empty() {
        return None;
    }
    // Git treats `**` not adjacent to `/` as two `*`s.
    let pattern = Pattern::new(pat)
        .or_else(|_| Pattern::new(&pat.replace("**", "*")))
        .ok()?;
    Some(Rule { pattern, negated, dir_only, anchored })
}

// ── The walker's stack ───────────────────────────────────────────────

#[derive(Debug)]
struct Node {
    file: IgnoreFile,
    parent: Option<Arc<Node>>,
}

/// The ignore files in effect for a directory, innermost first.
#[derive(Debug, Clone, Default)]
pub(crate) struct IgnoreStack {
    top: Option<Arc<Node>>,
    /// Inside a git repository (its info/exclude and the global excludes
    /// are already on the stack).
    in_repo: bool,
}

impl IgnoreStack {
    fn push(&self, file: IgnoreFile) -> Self {
        IgnoreStack {
            top: Some(Arc::new(Node { file, parent: self.top.clone() })),
            in_repo: self.in_repo,
        }
    }

    /// Rules in effect at the root of a walk: if the root is inside a git
    /// repository, the global excludes, .git/info/exclude, and every
    /// .gitignore from the repository root down to (not including) the
    /// walk root. The walk root's own .gitignore is added by `enter_dir`.
    pub(crate) fn for_walk_root(scope: &Path) -> Self {
        let Ok(canon) = fs::canonicalize(scope) else {
            return IgnoreStack::default();
        };
        let Some(repo) = canon.ancestors().find(|a| a.join(".git").exists()) else {
            return IgnoreStack::default();
        };
        let anchor = normalize(scope.to_path_buf());
        let rel_to = |dir: &Path| canon.strip_prefix(dir).map(Path::to_path_buf).unwrap_or_default();

        let mut stack = IgnoreStack::default();
        for file in repo_level_files(repo, anchor.clone()) {
            stack = stack.push(file.with_prefix(rel_to(repo)));
        }
        stack.in_repo = true;
        if canon != repo {
            // Ancestors between the repo root and the walk root, outermost first.
            let mut dirs: Vec<&Path> = canon.ancestors().skip(1).take_while(|a| a.starts_with(repo)).collect();
            dirs.reverse();
            for dir in dirs {
                if let Some(f) = IgnoreFile::load(&dir.join(".gitignore"), anchor.clone()) {
                    stack = stack.push(f.with_prefix(rel_to(dir)));
                }
            }
        }
        stack
    }

    /// Extend the stack on entering `dir`: its .gitignore, and if `dir` is
    /// a repository root not yet seen, its repo-level ignore files first.
    pub(crate) fn enter_dir(&self, dir: &Path) -> Self {
        let anchor = normalize(dir.to_path_buf());
        let mut stack = self.clone();
        if !stack.in_repo && dir.join(".git").exists() {
            for file in repo_level_files(dir, anchor.clone()) {
                stack = stack.push(file);
            }
            stack.in_repo = true;
        }
        match IgnoreFile::load(&dir.join(".gitignore"), anchor) {
            Some(f) => stack.push(f),
            None => stack,
        }
    }

    /// Whether `path` is ignored. Deeper files take precedence, and within
    /// a file the last matching rule wins.
    pub(crate) fn is_ignored(&self, path: &Path, is_dir: bool) -> bool {
        let mut cur = self.top.as_deref();
        while let Some(node) = cur {
            if let Some(negated) = node.file.decide(path, is_dir) {
                return !negated;
            }
            cur = node.parent.as_deref();
        }
        false
    }
}

/// The global excludes file and .git/info/exclude for the repository at
/// `repo`, lowest precedence first. Rules are anchored at `anchor`.
fn repo_level_files(repo: &Path, anchor: PathBuf) -> Vec<IgnoreFile> {
    let mut files = Vec::new();
    if let Some(global) = global_excludes_path() {
        files.extend(IgnoreFile::load(&global, anchor.clone()));
    }
    files.extend(IgnoreFile::load(&repo.join(".git/info/exclude"), anchor));
    files
}

fn global_excludes_path() -> Option<PathBuf> {
    match std::env::var_os("XDG_CONFIG_HOME") {
        Some(x) if !x.is_empty() => Some(PathBuf::from(x).join("git/ignore")),
        _ => std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config/git/ignore")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stack(content: &str) -> IgnoreStack {
        IgnoreStack::default().push(IgnoreFile::parse(content, PathBuf::new()).unwrap())
    }

    fn ignored(s: &IgnoreStack, path: &str, is_dir: bool) -> bool {
        s.is_ignored(Path::new(path), is_dir)
    }

    #[test]
    fn unanchored_matches_any_depth() {
        let s = stack("*.o\ntarget\n");
        assert!(ignored(&s, "foo.o", false));
        assert!(ignored(&s, "a/b/foo.o", false));
        assert!(ignored(&s, "target", true));
        assert!(ignored(&s, "src/target", true));
        assert!(!ignored(&s, "src/main.rs", false));
    }

    #[test]
    fn leading_slash_anchors() {
        let s = stack("/target\n");
        assert!(ignored(&s, "target", true));
        assert!(!ignored(&s, "src/target", true));
    }

    #[test]
    fn middle_slash_anchors() {
        let s = stack("docs/build\n");
        assert!(ignored(&s, "docs/build", true));
        assert!(!ignored(&s, "x/docs/build", true));
    }

    #[test]
    fn anchored_star_does_not_cross_slash() {
        let s = stack("src/*.rs\n");
        assert!(ignored(&s, "src/a.rs", false));
        assert!(!ignored(&s, "src/a/b.rs", false));
    }

    #[test]
    fn dir_only_rules() {
        let s = stack("build/\n");
        assert!(ignored(&s, "build", true));
        assert!(!ignored(&s, "build", false));
    }

    #[test]
    fn negation_last_match_wins() {
        let s = stack("*.log\n!keep.log\n");
        assert!(ignored(&s, "a.log", false));
        assert!(!ignored(&s, "keep.log", false));
        let s = stack("!keep.log\n*.log\n");
        assert!(ignored(&s, "keep.log", false));
    }

    #[test]
    fn double_star_forms() {
        let s = stack("**/logs\nfoo/**\na/**/z\n");
        assert!(ignored(&s, "x/y/logs", true));
        assert!(ignored(&s, "foo/bar/baz", false));
        assert!(ignored(&s, "a/z", false));
        assert!(ignored(&s, "a/b/c/z", false));
    }

    #[test]
    fn escapes_and_whitespace() {
        let s = stack("\\#file\n\\!bang\ntrail   \n");
        assert!(ignored(&s, "#file", false));
        assert!(ignored(&s, "!bang", false));
        assert!(ignored(&s, "trail", false));
    }

    #[test]
    fn deeper_file_overrides() {
        let root = IgnoreFile::parse("*.log\n", PathBuf::new()).unwrap();
        let sub = IgnoreFile::parse("!*.log\n", PathBuf::from("sub")).unwrap();
        let s = IgnoreStack::default().push(root).push(sub);
        assert!(ignored(&s, "a.log", false));
        assert!(!ignored(&s, "sub/a.log", false));
    }

    #[test]
    fn nested_file_anchors_to_its_dir() {
        let sub = IgnoreFile::parse("/out\n", PathBuf::from("sub")).unwrap();
        let s = IgnoreStack::default().push(sub);
        assert!(ignored(&s, "sub/out", true));
        assert!(!ignored(&s, "sub/x/out", true));
        assert!(!ignored(&s, "out", true));
    }

    #[test]
    fn prefix_for_files_above_walk_root() {
        // Repo-root rule `/src/gen`, walk rooted at `src` (walk path "gen").
        let f = IgnoreFile::parse("/src/gen\n", PathBuf::new()).unwrap().with_prefix(PathBuf::from("src"));
        let s = IgnoreStack::default().push(f);
        assert!(ignored(&s, "gen", true));
        assert!(!ignored(&s, "other/gen", true));
    }

    #[test]
    fn path_check_honors_ignored_parents() {
        let f = IgnoreFile::parse("target/\n", PathBuf::new()).unwrap();
        assert!(IgnoreFile::is_path_ignored(std::slice::from_ref(&f), Path::new("target/debug/x"), false));
        assert!(!IgnoreFile::is_path_ignored(&[f], Path::new("src/x"), false));
    }
}
