// Filesystem walking — the POSIX glob(3) equivalent.
//
// One engine drives every walk. Each directory is read once, and every
// child is matched against the set of (pattern, component) states still
// alive at that depth, so multiple patterns and brace alternatives share
// a single traversal and can never yield the same path twice.
//
// Without a limit or budget, subdirectories fan out across rayon. With
// one, the same engine runs depth-first in sorted order so it can stop
// early. Both modes produce identical output: children are sorted by
// name per directory, so pre-order traversal is globally path-sorted.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

use rayon::prelude::*;

use crate::entry::Entry;
use crate::error::{GlobError, PatternError, PatternErrorKind};
use crate::ignore::IgnoreStack;
use crate::matcher::MatchOptions;
use crate::pattern::{try_expand_braces, Pattern};

/// What to do when the next matched entry would overrun a byte or token budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BudgetMode {
    /// Stop the walk at the first entry that does not fit (default).
    /// Enables early termination on large trees.
    #[default]
    Stop,
    /// Skip entries that do not fit and keep walking, packing as many
    /// entries as possible into the budget (in sorted order).
    Fit,
}

/// The order in which matches are considered for limits and budgets, and
/// in which they are returned.
///
/// Anything other than `Path` needs every match before the first can be
/// chosen, so the walk runs to completion before limits and budgets apply.
#[derive(Debug, Clone, Default)]
pub enum Prefer {
    /// Path order (default). Allows early termination.
    #[default]
    Path,
    /// Largest files first.
    Size,
    /// Smallest files first.
    SizeAsc,
    /// Most recently modified first (by mtime).
    Recent,
    /// Highest score from an outside ranking first; unscored entries last.
    Scores(ScoreMap),
}

impl Prefer {
    /// The name used on the command line and in §summary.
    pub fn as_str(&self) -> &'static str {
        match self {
            Prefer::Path => "path",
            Prefer::Size => "size",
            Prefer::SizeAsc => "size-asc",
            Prefer::Recent => "recent",
            Prefer::Scores(_) => "scores",
        }
    }

    /// The score shown for `entry` in `Scores` mode.
    pub fn score(&self, entry: &Entry) -> Option<f64> {
        match self {
            Prefer::Scores(map) => map.get(&entry.path),
            _ => None,
        }
    }

    fn compare(&self, a: &Entry, b: &Entry) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        // `None` sorts last in every mode.
        fn desc<T: PartialOrd>(a: Option<T>, b: Option<T>) -> Ordering {
            match (a, b) {
                (Some(x), Some(y)) => y.partial_cmp(&x).unwrap_or(Ordering::Equal),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => Ordering::Equal,
            }
        }
        let primary = match self {
            Prefer::Path => Ordering::Equal,
            Prefer::Size => b.size.cmp(&a.size),
            Prefer::SizeAsc => a.size.cmp(&b.size),
            Prefer::Recent => desc(a.modified, b.modified),
            Prefer::Scores(map) => desc(map.get(&a.path), map.get(&b.path)),
        };
        primary.then_with(|| a.path.cmp(&b.path))
    }
}

/// Scores from an outside ranking, keyed by path (`--prefer-from`).
#[derive(Clone, Default)]
pub struct ScoreMap {
    scores: Arc<std::collections::HashMap<PathBuf, f64>>,
    has_absolute: bool,
}

impl ScoreMap {
    /// Build from (path, score) pairs. Relative paths match both as given
    /// and relative to `base` (the walk root) when given. If a path repeats, the highest
    /// score wins.
    pub fn new<I: IntoIterator<Item = (PathBuf, f64)>>(pairs: I, base: Option<&Path>) -> Self {
        let mut scores = std::collections::HashMap::new();
        let mut has_absolute = false;
        for (path, score) in pairs {
            let path = path.strip_prefix("./").map(Path::to_path_buf).unwrap_or(path);
            let keys = if path.is_absolute() {
                has_absolute = true;
                vec![fs::canonicalize(&path).unwrap_or(path)]
            } else {
                // Accept paths relative to the root or already spelled
                // the way the walk spells them.
                match base {
                    Some(b) => vec![b.join(&path), path],
                    None => vec![path],
                }
            };
            for key in keys {
                let slot = scores.entry(key).or_insert(score);
                if score > *slot {
                    *slot = score;
                }
            }
        }
        ScoreMap { scores: Arc::new(scores), has_absolute }
    }

    /// Parse a ranking: one entry per line, as JSON Lines
    /// (`{"path": "...", "score": 12}`), `path<TAB>score`, or
    /// `score path` (the output of `uniq -c`). Blank lines and lines
    /// starting with `#` are skipped.
    pub fn parse(text: &str, base: Option<&Path>) -> Result<Self, String> {
        let mut pairs = Vec::new();
        for (n, line) in text.lines().enumerate() {
            let t = line.trim();
            if t.is_empty() || t.starts_with('#') {
                continue;
            }
            let pair = parse_score_line(t)
                .ok_or_else(|| format!("line {}: expected JSON, `path<TAB>score` or `score path`: {:?}", n + 1, line))?;
            pairs.push(pair);
        }
        Ok(ScoreMap::new(pairs, base))
    }

    /// The score for a path as the walk spells it.
    pub fn get(&self, path: &Path) -> Option<f64> {
        if let Some(s) = self.scores.get(path) {
            return Some(*s);
        }
        if self.has_absolute {
            let canon = fs::canonicalize(path).ok()?;
            return self.scores.get(&canon).copied();
        }
        None
    }

    pub fn len(&self) -> usize {
        self.scores.len()
    }

    pub fn is_empty(&self) -> bool {
        self.scores.is_empty()
    }
}

impl fmt::Debug for ScoreMap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ScoreMap({} paths)", self.scores.len())
    }
}

fn parse_score_line(t: &str) -> Option<(PathBuf, f64)> {
    if t.starts_with('{') {
        let fields = crate::json::parse_flat_object(t)?;
        let path = fields.iter().find(|(k, _)| k == "path")?.1.as_str()?;
        let score = fields.iter().find(|(k, _)| k == "score")?.1.as_f64()?;
        return Some((PathBuf::from(path), score));
    }
    if let Some((a, b)) = t.split_once('\t') {
        let (a, b) = (a.trim(), b.trim());
        return match (b.parse::<f64>(), a.parse::<f64>()) {
            (Ok(score), _) => Some((PathBuf::from(a), score)),
            (Err(_), Ok(score)) => Some((PathBuf::from(b), score)),
            _ => None,
        };
    }
    let (first, rest) = t.split_once(char::is_whitespace)?;
    let score = first.parse::<f64>().ok()?;
    Some((PathBuf::from(rest.trim_start()), score))
}

/// A predicate applied to every matched entry before limits and budgets
/// are charged, so filtered-out entries never consume them.
#[derive(Clone)]
pub struct EntryFilter(Arc<dyn Fn(&Entry) -> bool + Send + Sync>);

impl EntryFilter {
    pub fn new<F: Fn(&Entry) -> bool + Send + Sync + 'static>(f: F) -> Self {
        EntryFilter(Arc::new(f))
    }

    pub fn matches(&self, entry: &Entry) -> bool {
        (self.0)(entry)
    }
}

impl fmt::Debug for EntryFilter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("EntryFilter(..)")
    }
}

/// Options for the filesystem walker.
#[derive(Debug, Clone)]
pub struct WalkOptions {
    /// Pattern matching options.
    pub match_opts: MatchOptions,
    /// If true, return results in path-sorted order (default: true).
    pub sorted: bool,
    /// If true, return the first I/O error instead of results (POSIX GLOB_ERR).
    pub stop_on_error: bool,
    /// Maximum number of results to yield. `None` = unlimited.
    pub limit: Option<usize>,
    /// Maximum total bytes across all matched entries. `None` = unlimited.
    pub byte_budget: Option<u64>,
    /// Maximum estimated tokens across all matched entries. `None` = unlimited.
    pub token_budget: Option<u64>,
    /// Behavior when an entry would overrun a budget.
    pub budget_mode: BudgetMode,
    /// Which matches limits and budgets go to first, and output order.
    pub prefer: Prefer,
    /// If true, only yield directories.
    pub only_dirs: bool,
    /// Maximum depth, counted from the first wildcard component: for
    /// `src/**/*.rs`, depth 1 means files directly inside `src/`.
    /// `None` = unlimited.
    pub max_depth: Option<usize>,
    /// If true, skip full stat() — uses DirEntry file_type only.
    pub no_stat: bool,
    /// If true, honor .gitignore files, .git/info/exclude, and the global
    /// git excludes file, and skip `.git` directories.
    pub gitignore: bool,
    /// If true, descend into symlinked directories found while walking
    /// (cycles are detected and skipped). Symlinks named literally in the
    /// pattern are always followed.
    pub follow_symlinks: bool,
    /// If true, don't descend into nested git repositories (directories
    /// containing `.git`, including submodules) found below the walk root.
    /// With `git_files`, don't list their files.
    pub skip_nested_repos: bool,
    /// If true, take the file list from git (`git ls-files --cached
    /// --others --exclude-standard`) instead of reading directories:
    /// exactly the files git considers part of the working tree, tracked
    /// files included even if they match a .gitignore rule, recursing into
    /// submodules and nested repositories. Yields files only; `gitignore`
    /// and `follow_symlinks` have no effect. Each pattern's literal prefix
    /// must be inside a git repository.
    pub git_files: bool,
    /// Exclude patterns. A pattern containing `/` is matched against the
    /// full entry path (with `*` not crossing `/`); one without `/` is
    /// matched against the file name at any depth. Matching directories
    /// are pruned along with everything under them.
    pub exclude: Vec<Pattern>,
    /// Extra predicate applied before limits and budgets are charged.
    pub filter: Option<EntryFilter>,
}

impl Default for WalkOptions {
    fn default() -> Self {
        WalkOptions {
            match_opts: MatchOptions::new(),
            sorted: true,
            stop_on_error: false,
            limit: None,
            byte_budget: None,
            token_budget: None,
            budget_mode: BudgetMode::Stop,
            prefer: Prefer::Path,
            only_dirs: false,
            max_depth: None,
            no_stat: false,
            gitignore: false,
            follow_symlinks: false,
            skip_nested_repos: false,
            git_files: false,
            exclude: Vec::new(),
            filter: None,
        }
    }
}

/// The result of a glob walk.
pub type WalkResult = Result<Entry, GlobError>;

/// Walk the filesystem matching a pattern, returning all matched entries.
///
/// Brace expressions are expanded, and all alternatives share one walk.
pub fn walk(pattern: &str, opts: WalkOptions) -> Result<Vec<WalkResult>, GlobError> {
    walk_many(&[pattern], opts)
}

/// Walk using a pre-compiled pattern.
pub fn walk_pattern(pat: &Pattern, opts: WalkOptions) -> Result<Vec<WalkResult>, GlobError> {
    walk(pat.as_str(), opts)
}

/// Why a walk stopped before examining every candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// `limit` results were collected; more matches may exist.
    Limit,
    /// The next match would have exceeded `token_budget` (BudgetMode::Stop).
    TokenBudget,
    /// The next match would have exceeded `byte_budget` (BudgetMode::Stop).
    ByteBudget,
}

impl StopReason {
    pub fn as_str(self) -> &'static str {
        match self {
            StopReason::Limit => "limit",
            StopReason::TokenBudget => "token_budget",
            StopReason::ByteBudget => "byte_budget",
        }
    }
}

/// The full outcome of a walk: results plus what was left out and why.
#[derive(Debug)]
pub struct WalkReport {
    /// Matched entries and I/O errors, in order.
    pub results: Vec<WalkResult>,
    /// Matches left out because they didn't fit a budget
    /// (BudgetMode::Fit), in walk order. Unlike a stop, these leave holes
    /// in the middle of the sorted results.
    pub budget_skipped: Vec<Entry>,
    /// Set if the walk stopped early; matches after that point were not
    /// examined.
    pub stopped_early: Option<StopReason>,
    /// Entries skipped by each pruning rule. A pruned directory counts
    /// once — its contents are never read.
    pub pruned: PruneCounts,
}

/// How many entries each pruning rule left out. Only entries that would
/// otherwise have matched or been descended into are counted, and a
/// pruned directory counts as one entry.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PruneCounts {
    /// Dot-names wildcards skipped (`-a` includes them).
    pub hidden: usize,
    /// Entries matching a gitignore rule (`-g`).
    pub gitignore: usize,
    /// Entries matching an exclude pattern (`-e`).
    pub exclude: usize,
    /// Nested repositories not entered (`skip_nested_repos`).
    pub nested_repos: usize,
    /// Symlinked directories listed but not entered (`follow_symlinks`).
    pub symlink_dirs: usize,
}

/// Walk the filesystem matching any of several patterns in a single pass.
///
/// Each path is yielded at most once, even if several patterns match it.
pub fn walk_many<S: AsRef<str>>(
    patterns: &[S],
    opts: WalkOptions,
) -> Result<Vec<WalkResult>, GlobError> {
    walk_many_report(patterns, opts).map(|r| r.results)
}

/// Like [`walk_many`], but also reports entries skipped for budget and
/// whether the walk stopped early — so callers can tell a complete result
/// from a partial one.
pub fn walk_many_report<S: AsRef<str>>(
    patterns: &[S],
    opts: WalkOptions,
) -> Result<WalkReport, GlobError> {
    if !matches!(opts.prefer, Prefer::Path) {
        return walk_ranked(patterns, opts);
    }
    // Expand braces, compile, and group patterns by their root ("", "/", ...).
    let mut groups: Vec<(PathBuf, Vec<Compiled>)> = Vec::new();
    for p in patterns {
        for expanded in try_expand_braces(p.as_ref())? {
            let (root, compiled) = compile(&expanded)?;
            match groups.iter_mut().find(|(r, _)| *r == root) {
                Some((_, pats)) => pats.push(compiled),
                None => groups.push((root, vec![compiled])),
            }
        }
    }

    let parallel =
        opts.limit.is_none() && opts.byte_budget.is_none() && opts.token_budget.is_none();
    let mut sink = Sink::new(&opts);
    let counts = Counters::default();
    for (root, pats) in &groups {
        if sink.stopped {
            break;
        }
        let walker = Walker { opts: &opts, pats, parallel, counts: &counts };
        if opts.git_files {
            walker.walk_git(root, &mut sink);
        } else {
            walker.walk_root(root, &mut sink);
        }
    }

    let mut results = std::mem::take(&mut sink.results);
    // In git_files mode a failed listing means the answer is unknown, not
    // partial: report it rather than returning what was found elsewhere.
    if let Some(pos) = results.iter().position(|r| matches!(r, Err(GlobError::Git { .. }))) {
        return Err(results.swap_remove(pos).unwrap_err());
    }
    if groups.len() > 1 && opts.sorted {
        results.sort_by(|a, b| result_path(a).cmp(result_path(b)));
        results.dedup_by(|a, b| {
            matches!((&*a, &*b), (Ok(x), Ok(y)) if x.path == y.path)
        });
    }
    if opts.stop_on_error {
        if let Some(pos) = results.iter().position(|r| r.is_err()) {
            return Err(results.swap_remove(pos).unwrap_err());
        }
    }
    Ok(WalkReport {
        results,
        budget_skipped: sink.budget_skipped,
        stopped_early: sink.stopped_early,
        pruned: counts.snapshot(),
    })
}

/// Walk without limits, rank every match, then apply limits and budgets
/// in preference order.
fn walk_ranked<S: AsRef<str>>(patterns: &[S], opts: WalkOptions) -> Result<WalkReport, GlobError> {
    let unbounded = WalkOptions {
        limit: None,
        byte_budget: None,
        token_budget: None,
        prefer: Prefer::Path,
        ..opts.clone()
    };
    let all = walk_many_report(patterns, unbounded)?;
    let (mut entries, errors): (Vec<Entry>, Vec<GlobError>) = {
        let mut oks = Vec::new();
        let mut errs = Vec::new();
        for r in all.results {
            match r {
                Ok(e) => oks.push(e),
                Err(e) => errs.push(e),
            }
        }
        (oks, errs)
    };
    entries.par_sort_by(|a, b| opts.prefer.compare(a, b));

    let mut sink = Sink::new(&opts);
    for e in entries {
        if sink.stopped {
            break;
        }
        sink.push_entry(e);
    }
    let mut results = sink.results;
    results.extend(errors.into_iter().map(Err));
    Ok(WalkReport {
        results,
        budget_skipped: sink.budget_skipped,
        stopped_early: sink.stopped_early,
        pruned: all.pruned,
    })
}

fn result_path(r: &WalkResult) -> &Path {
    match r {
        Ok(e) => &e.path,
        Err(GlobError::Io { path, .. } | GlobError::Git { path, .. }) => path,
        Err(GlobError::Pattern(_)) => Path::new(""),
    }
}

// ── Pattern compilation ──────────────────────────────────────────────

/// One pattern, split into per-component matchers.
struct Compiled {
    comps: Vec<Pattern>,
    /// Number of leading components with no metacharacters.
    prefix_len: usize,
    /// Pattern ended in `/`: only directories match.
    require_dir: bool,
}

/// A live matching position: (pattern index, component index).
type State = (usize, usize);

/// Split a pattern into its literal root (`/`, a Windows prefix, or empty)
/// and per-component patterns. `.` components are dropped so `./src/*`
/// and `src/*` produce identical paths; runs of `**` collapse to one.
fn compile(pattern: &str) -> Result<(PathBuf, Compiled), GlobError> {
    // Validate the whole pattern first so error positions refer to it.
    Pattern::new(pattern)?;

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
            if pattern.as_bytes().get(rest_start) == Some(&b'\\') {
                root.push("\\");
                rest_start += 1;
            }
        }
    }

    let rest_start = rest_start.min(pattern.len());
    let rest = &pattern[rest_start..];
    let mut comps: Vec<Pattern> = Vec::new();
    let mut offset = rest_start;
    for s in rest.split(|c: char| c == '/' || (cfg!(windows) && c == '\\')) {
        let comp_start = offset;
        offset += s.len() + 1;
        if s.is_empty() || s == "." {
            continue;
        }
        // The whole pattern parsed, so a component that doesn't must hold
        // part of a bracket expression spanning a `/`. Wildcards never
        // match `/` in a walk, so such a bracket could never match.
        let comp = Pattern::new(s).map_err(|e| match e.kind {
            PatternErrorKind::UnclosedBracket | PatternErrorKind::EmptyBracket => PatternError {
                pos: comp_start + e.pos,
                kind: PatternErrorKind::SlashInBracket,
            },
            _ => e,
        })?;
        if comp.is_recursive && comps.last().is_some_and(|c| c.is_recursive) {
            continue;
        }
        comps.push(comp);
    }

    let prefix_len = comps.iter().take_while(|c| !c.has_meta).count();
    let require_dir = rest.len() > 1 && rest.ends_with('/');
    Ok((root, Compiled { comps, prefix_len, require_dir }))
}

// ── Output sink (limits and budgets) ─────────────────────────────────

struct Sink {
    results: Vec<WalkResult>,
    limit: Option<usize>,
    byte_budget: Option<u64>,
    token_budget: Option<u64>,
    mode: BudgetMode,
    stop_on_error: bool,
    count: usize,
    bytes: u64,
    tokens: u64,
    stopped: bool,
    stopped_early: Option<StopReason>,
    budget_skipped: Vec<Entry>,
}

impl Sink {
    fn new(opts: &WalkOptions) -> Self {
        Sink {
            results: Vec::new(),
            limit: opts.limit,
            byte_budget: opts.byte_budget,
            token_budget: opts.token_budget,
            mode: opts.budget_mode,
            stop_on_error: opts.stop_on_error,
            count: 0,
            bytes: 0,
            tokens: 0,
            stopped: false,
            stopped_early: None,
            budget_skipped: Vec::new(),
        }
    }

    /// An unbounded collector for one parallel subtree.
    fn collector(stop_on_error: bool) -> Self {
        Sink {
            results: Vec::new(),
            limit: None,
            byte_budget: None,
            token_budget: None,
            mode: BudgetMode::Stop,
            stop_on_error,
            count: 0,
            bytes: 0,
            tokens: 0,
            stopped: false,
            stopped_early: None,
            budget_skipped: Vec::new(),
        }
    }

    fn push_entry(&mut self, entry: Entry) {
        if self.stopped {
            return;
        }
        let over_bytes = self.byte_budget.is_some_and(|b| self.bytes + entry.size > b);
        let over_tokens = self
            .token_budget
            .is_some_and(|b| self.tokens + entry.tokens_est > b);
        if over_bytes || over_tokens {
            match self.mode {
                BudgetMode::Stop => {
                    self.stopped = true;
                    self.stopped_early = Some(if over_tokens {
                        StopReason::TokenBudget
                    } else {
                        StopReason::ByteBudget
                    });
                }
                BudgetMode::Fit => self.budget_skipped.push(entry),
            }
            return;
        }
        self.bytes += entry.size;
        self.tokens += entry.tokens_est;
        self.count += 1;
        self.results.push(Ok(entry));
        if self.limit.is_some_and(|l| self.count >= l) {
            self.stopped = true;
            self.stopped_early = Some(StopReason::Limit);
        }
    }

    fn push_err(&mut self, err: GlobError) {
        if self.stopped {
            return;
        }
        self.results.push(Err(err));
        if self.stop_on_error {
            self.stopped = true;
        }
    }

    fn absorb(&mut self, other: Sink) {
        self.results.extend(other.results);
        self.stopped |= other.stopped;
        self.budget_skipped.extend(other.budget_skipped);
    }
}

// ── The walker ───────────────────────────────────────────────────────

/// Entries left out by pruning, counted during a walk (shared by threads).
#[derive(Default)]
struct Counters {
    hidden: AtomicUsize,
    gitignore: AtomicUsize,
    exclude: AtomicUsize,
    nested_repos: AtomicUsize,
    symlink_dirs: AtomicUsize,
}

impl Counters {
    fn snapshot(&self) -> PruneCounts {
        PruneCounts {
            hidden: self.hidden.load(Relaxed),
            gitignore: self.gitignore.load(Relaxed),
            exclude: self.exclude.load(Relaxed),
            nested_repos: self.nested_repos.load(Relaxed),
            symlink_dirs: self.symlink_dirs.load(Relaxed),
        }
    }
}

struct Walker<'a> {
    counts: &'a Counters,
    opts: &'a WalkOptions,
    pats: &'a [Compiled],
    parallel: bool,
}

/// A child of a directory that matched at least one live state.
struct Child {
    entry: Entry,
    is_result: bool,
    /// States to match this directory's children against. Empty = don't descend.
    next: Vec<State>,
}

/// Canonical paths of the directories above the current one, used to
/// detect symlink cycles when following symlinks.
#[derive(Clone, Default)]
struct Ancestors(Option<Arc<(PathBuf, Ancestors)>>);

impl Ancestors {
    fn contains(&self, p: &Path) -> bool {
        let mut cur = &self.0;
        while let Some(node) = cur {
            if node.0 == p {
                return true;
            }
            cur = &node.1 .0;
        }
        false
    }

    fn push(&self, p: PathBuf) -> Ancestors {
        Ancestors(Some(Arc::new((p, self.clone()))))
    }
}

impl Walker<'_> {
    fn walk_root(&self, root: &Path, sink: &mut Sink) {
        let scope = if root.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            root.to_path_buf()
        };

        // A pattern with no components (".", "/") names the root itself.
        if self.pats.iter().any(|p| p.comps.is_empty()) && fs::symlink_metadata(&scope).is_ok() {
            sink.push_entry(self.make_entry(scope.clone(), None));
        }

        let mut states = Vec::new();
        for (p, pat) in self.pats.iter().enumerate() {
            if !pat.comps.is_empty() {
                self.add_state(&mut states, p, 0);
            }
        }
        if states.is_empty() {
            return;
        }

        let ignores = if self.opts.gitignore {
            IgnoreStack::for_walk_root(&scope)
        } else {
            IgnoreStack::default()
        };
        let ancestors = if self.opts.follow_symlinks {
            let canon = fs::canonicalize(&scope).unwrap_or_else(|_| scope.clone());
            Ancestors::default().push(canon)
        } else {
            Ancestors::default()
        };
        self.visit_dir(&scope, 0, &states, &ignores, &ancestors, sink);
    }

    /// `git_files` mode: take candidate paths from `git ls-files` in each
    /// pattern's literal base directory, then match them component by
    /// component with the same state machine the directory walk uses.
    fn walk_git(&self, root: &Path, sink: &mut Sink) {
        let scope = if root.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            root.to_path_buf()
        };
        let mut states = Vec::new();
        for (p, pat) in self.pats.iter().enumerate() {
            if !pat.comps.is_empty() {
                self.add_state(&mut states, p, 0);
            }
        }
        if states.is_empty() {
            return;
        }

        // List each outermost literal base directory once.
        let mut bases: Vec<PathBuf> = self
            .pats
            .iter()
            .filter(|p| !p.comps.is_empty())
            .map(|p| {
                let n = p.prefix_len.min(p.comps.len() - 1);
                p.comps[..n].iter().filter_map(|c| c.literal()).collect()
            })
            .collect();
        bases.sort();
        bases.dedup();
        let mut outer: Vec<PathBuf> = Vec::new();
        for b in bases {
            if !outer.iter().any(|o| b.starts_with(o)) {
                outer.push(b);
            }
        }

        let mut candidates: Vec<(PathBuf, bool)> = Vec::new();
        for base in &outer {
            let dir = base
                .iter()
                .fold(scope.clone(), |d, c| join(&d, &c.to_string_lossy()));
            if !dir.is_dir() {
                continue;
            }
            match crate::git::list_files(&dir, !self.opts.skip_nested_repos) {
                Ok(files) => candidates.extend(files.into_iter().map(|(f, t)| (base.join(f), t))),
                Err(message) => sink.push_err(GlobError::Git { path: dir, message }),
            }
        }

        let mut matched: Vec<PathBuf> = candidates
            .into_par_iter()
            .filter_map(|(rel, tracked)| self.match_rel(&scope, &rel, tracked, &states))
            .collect();
        matched.par_sort();
        matched.dedup();

        let entry = |path: PathBuf| -> Option<Entry> {
            // Tracked files deleted from the working tree are skipped.
            fs::symlink_metadata(&path).ok()?;
            let e = self.make_entry(path, None);
            // Tracked symlinks count as files even when they point at a dir.
            let keep = (!e.is_dir || e.is_symlink)
                && !self.opts.only_dirs
                && self.opts.filter.as_ref().is_none_or(|f| f.matches(&e));
            keep.then_some(e)
        };
        if self.parallel {
            let entries: Vec<Entry> = matched.into_par_iter().filter_map(entry).collect();
            for e in entries {
                sink.push_entry(e);
            }
        } else {
            for path in matched {
                if sink.stopped {
                    return;
                }
                if let Some(e) = entry(path) {
                    sink.push_entry(e);
                }
            }
        }
    }

    /// Match a scope-relative file path against the patterns, treating
    /// every component but the last as a directory. Returns the walk path.
    ///
    /// Tracked files may be dotfiles: git already chose them, so wildcards
    /// match them without -a.
    fn match_rel(&self, scope: &Path, rel: &Path, tracked: bool, initial: &[State]) -> Option<PathBuf> {
        let names: Vec<&str> = rel.iter().map(|c| c.to_str()).collect::<Option<_>>()?;
        let mut states = initial.to_vec();
        let mut path = scope.to_path_buf();
        for (level, name) in names.iter().enumerate() {
            let last = level + 1 == names.len();
            path = join(&path, name);
            let (matched, next, _) = self.step(name, !last, level, &states, tracked);
            if (last && !matched) || (!last && next.is_empty()) {
                if !tracked {
                    self.count_if_hidden(name, !last, level, &states);
                }
                return None;
            }
            if self.is_excluded(&path, name, !last) {
                self.counts.exclude.fetch_add(1, Relaxed);
                return None;
            }
            if last {
                return Some(path);
            }
            states = next;
        }
        None
    }

    /// Add a state, plus the zero-component closure if it is `**`.
    fn add_state(&self, states: &mut Vec<State>, p: usize, i: usize) {
        if !states.contains(&(p, i)) {
            states.push((p, i));
        }
        let comps = &self.pats[p].comps;
        if comps[i].is_recursive && i + 1 < comps.len() && !states.contains(&(p, i + 1)) {
            states.push((p, i + 1));
        }
    }

    fn depth_ok(&self, p: usize, level: usize) -> bool {
        match self.opts.max_depth {
            Some(d) => level.saturating_sub(self.pats[p].prefix_len) <= d,
            None => true,
        }
    }

    fn visit_dir(
        &self,
        dir: &Path,
        level: usize,
        states: &[State],
        ignores: &IgnoreStack,
        ancestors: &Ancestors,
        sink: &mut Sink,
    ) {
        let (children, ignores) = match self.children(dir, level, states, ignores) {
            Ok(c) => c,
            Err(error) => {
                sink.push_err(GlobError::Io { path: dir.to_path_buf(), error });
                return;
            }
        };

        if self.parallel {
            let parts: Vec<Sink> = children
                .into_par_iter()
                .map(|c| {
                    let mut s = Sink::collector(self.opts.stop_on_error);
                    self.visit_child(c, level, &ignores, ancestors, &mut s);
                    s
                })
                .collect();
            for part in parts {
                sink.absorb(part);
            }
        } else {
            for c in children {
                if sink.stopped {
                    return;
                }
                self.visit_child(c, level, &ignores, ancestors, sink);
            }
        }
    }

    fn visit_child(
        &self,
        child: Child,
        level: usize,
        ignores: &IgnoreStack,
        ancestors: &Ancestors,
        sink: &mut Sink,
    ) {
        let Child { entry, is_result, next } = child;
        let descend_path = if next.is_empty() { None } else { Some(entry.path.clone()) };

        if is_result
            && (!self.opts.only_dirs || entry.is_dir)
            && self.opts.filter.as_ref().is_none_or(|f| f.matches(&entry))
        {
            sink.push_entry(entry);
        }

        if let Some(path) = descend_path {
            if sink.stopped {
                return;
            }
            let ancestors = if self.opts.follow_symlinks {
                let canon = fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
                if ancestors.contains(&canon) {
                    return; // Symlink cycle.
                }
                ancestors.push(canon)
            } else {
                ancestors.clone()
            };
            self.visit_dir(&path, level + 1, &next, ignores, &ancestors, sink);
        }
    }

    /// Enumerate and classify the children of `dir` against `states`.
    /// Also returns the ignore stack in effect inside `dir`, which depends
    /// on whether the listing shows `dir` to be a repository root.
    fn children(
        &self,
        dir: &Path,
        level: usize,
        states: &[State],
        parent_ignores: &IgnoreStack,
    ) -> std::io::Result<(Vec<Child>, IgnoreStack)> {
        let enter = |is_repo_root: bool| {
            if self.opts.gitignore {
                parent_ignores.enter_dir(dir, is_repo_root)
            } else {
                parent_ignores.clone()
            }
        };
        // Fast path: every live state is a literal name — stat, don't readdir.
        let literals: Option<Vec<String>> = if self.opts.match_opts.case_sensitive {
            states
                .iter()
                .map(|&(p, i)| self.pats[p].comps[i].literal())
                .collect()
        } else {
            None
        };

        let ignores;
        let mut children: Vec<Child> = if let Some(mut names) = literals {
            names.sort();
            names.dedup();
            ignores = enter(self.opts.gitignore && dir.join(".git").exists());
            let ignores = &ignores;
            names
                .into_par_iter()
                .filter_map(|name| {
                    let path = join(dir, &name);
                    let meta = fs::symlink_metadata(&path).ok()?;
                    let is_symlink = meta.file_type().is_symlink();
                    let is_dir = if is_symlink {
                        fs::metadata(&path).is_ok_and(|m| m.is_dir())
                    } else {
                        meta.is_dir()
                    };
                    self.classify(&name, path, is_dir, is_symlink, None, level, states, ignores)
                })
                .collect()
        } else {
            let dir_entries: Vec<fs::DirEntry> =
                fs::read_dir(dir)?.filter_map(|r| r.ok()).collect();
            ignores = enter(dir_entries.iter().any(|de| de.file_name() == ".git"));
            let ignores = &ignores;
            dir_entries
                .into_par_iter()
                .filter_map(|de| {
                    let name = de.file_name();
                    let name = name.to_str()?;
                    let ft = de.file_type().ok()?;
                    let path = join(dir, name);
                    let is_symlink = ft.is_symlink();
                    let is_dir = if is_symlink {
                        fs::metadata(&path).is_ok_and(|m| m.is_dir())
                    } else {
                        ft.is_dir()
                    };
                    self.classify(name, path, is_dir, is_symlink, Some(&de), level, states, ignores)
                })
                .collect()
        };

        if self.opts.sorted {
            children.sort_by(|a, b| a.entry.path.file_name().cmp(&b.entry.path.file_name()));
        }
        Ok((children, ignores))
    }

    /// Decide whether a child is a result and which states survive into it.
    /// Returns None for children that neither match nor lead anywhere,
    /// before any stat() is spent on them.
    #[allow(clippy::too_many_arguments)]
    fn classify(
        &self,
        name: &str,
        path: PathBuf,
        is_dir: bool,
        is_symlink: bool,
        de: Option<&fs::DirEntry>,
        level: usize,
        states: &[State],
        ignores: &IgnoreStack,
    ) -> Option<Child> {
        let (matched, mut next, explicit) = self.step(name, is_dir, level, states, false);

        if !is_dir {
            next.clear();
        }
        if is_symlink && !self.opts.follow_symlinks && !explicit && !next.is_empty() {
            next.clear();
            self.counts.symlink_dirs.fetch_add(1, Relaxed);
        }
        if self.opts.skip_nested_repos && !explicit && !next.is_empty() && path.join(".git").exists() {
            next.clear();
            self.counts.nested_repos.fetch_add(1, Relaxed);
        }

        if !matched && next.is_empty() {
            self.count_if_hidden(name, is_dir, level, states);
            return None;
        }

        if self.opts.gitignore && !explicit {
            if name == ".git" {
                return None;
            }
            if ignores.is_ignored(&path, is_dir) {
                self.counts.gitignore.fetch_add(1, Relaxed);
                return None;
            }
        }
        if self.is_excluded(&path, name, is_dir) {
            self.counts.exclude.fetch_add(1, Relaxed);
            return None;
        }

        let entry = if matched {
            match (de, self.opts.no_stat) {
                (Some(de), false) => Entry::from_dir_entry(path, de),
                (Some(de), true) => Entry::from_dir_entry_lightweight(path, de),
                (None, _) => self.make_entry(path, None),
            }
        } else {
            // Only descending through it: skip the stat.
            Entry::bare_dir(path, is_symlink)
        };

        Some(Child { entry, is_result: matched, next })
    }

    /// Count a non-matching dot-name that would have matched with -a.
    fn count_if_hidden(&self, name: &str, is_dir: bool, level: usize, states: &[State]) {
        if self.opts.match_opts.require_literal_leading_dot && name.starts_with('.') && name != ".git" {
            let (m, n, _) = self.step(name, is_dir, level, states, true);
            if m || !n.is_empty() {
                self.counts.hidden.fetch_add(1, Relaxed);
            }
        }
    }

    /// Match one path component (a child of a directory at `level`)
    /// against the live states. Returns whether it completes a pattern,
    /// the states to match its children against (depth-limited), and
    /// whether it was reached only through literal-prefix components.
    fn step(
        &self,
        name: &str,
        is_dir: bool,
        level: usize,
        states: &[State],
        allow_hidden: bool,
    ) -> (bool, Vec<State>, bool) {
        let child_level = level + 1;
        let mut mopts = self.opts.match_opts;
        if allow_hidden {
            mopts.require_literal_leading_dot = false;
        }
        let hidden = mopts.require_literal_leading_dot && name.starts_with('.');

        let mut next: Vec<State> = Vec::new();
        let mut matched = false;
        let mut explicit = true; // Reached only through literal-prefix components.

        for &(p, i) in states {
            if !self.depth_ok(p, child_level) {
                continue;
            }
            let pat = &self.pats[p];
            let comp = &pat.comps[i];
            let last = i + 1 == pat.comps.len();
            let hit = if comp.is_recursive {
                if hidden {
                    continue;
                }
                // `**` consumes this component and stays live below it.
                self.add_state(&mut next, p, i);
                last
            } else if comp.matches_with(name, mopts) {
                if !last {
                    self.add_state(&mut next, p, i + 1);
                }
                last
            } else {
                continue;
            };
            if i >= pat.prefix_len {
                explicit = false;
            }
            if hit && (is_dir || !pat.require_dir) {
                matched = true;
            }
        }

        if !is_dir {
            next.clear();
        }
        // Drop states that could only match beyond max_depth.
        next.retain(|&(p, _)| self.depth_ok(p, child_level + 1));
        (matched, next, explicit)
    }

    fn make_entry(&self, path: PathBuf, de: Option<&fs::DirEntry>) -> Entry {
        match (de, self.opts.no_stat) {
            (Some(de), false) => Entry::from_dir_entry(path, de),
            (Some(de), true) => Entry::from_dir_entry_lightweight(path, de),
            (None, false) => Entry::from_path(path),
            (None, true) => Entry::from_path_lightweight(path),
        }
    }

    fn is_excluded(&self, path: &Path, name: &str, is_dir: bool) -> bool {
        if self.opts.exclude.is_empty() {
            return false;
        }
        let path_opts = MatchOptions {
            case_sensitive: self.opts.match_opts.case_sensitive,
            require_literal_separator: true,
            require_literal_leading_dot: false,
        };
        let name_opts = MatchOptions { require_literal_separator: false, ..path_opts };
        let path_str = match path.to_str() {
            Some(s) => s,
            None => return false,
        };
        let dir_form = if is_dir { Some(format!("{}/", path_str)) } else { None };
        self.opts.exclude.iter().any(|pat| {
            if pat.as_str().contains('/') {
                pat.matches_with(path_str, path_opts)
                    || dir_form.as_deref().is_some_and(|d| pat.matches_with(d, path_opts))
            } else {
                pat.matches_with(name, name_opts)
            }
        })
    }
}

/// Join a child name onto a directory, keeping walks from "." free of a
/// `./` prefix.
fn join(dir: &Path, name: &str) -> PathBuf {
    if dir == Path::new(".") {
        PathBuf::from(name)
    } else {
        dir.join(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn comps(c: &Compiled) -> Vec<&str> {
        c.comps.iter().map(|p| p.as_str()).collect()
    }

    #[test]
    fn split_relative() {
        let (root, c) = compile("src/**/*.rs").unwrap();
        assert_eq!(root, PathBuf::new());
        assert_eq!(comps(&c), vec!["src", "**", "*.rs"]);
        assert_eq!(c.prefix_len, 1);
        assert!(!c.require_dir);
    }

    #[test]
    fn split_absolute() {
        let (root, c) = compile("/usr/lib/*.so").unwrap();
        assert_eq!(root, PathBuf::from("/"));
        assert_eq!(c.comps.len(), 3);
        assert_eq!(c.prefix_len, 2);
    }

    #[test]
    fn split_drops_dot_components() {
        let (_, c) = compile("./src/./*.rs").unwrap();
        assert_eq!(comps(&c), vec!["src", "*.rs"]);
    }

    #[test]
    fn split_collapses_recursive() {
        let (_, c) = compile("a/**/**/b").unwrap();
        assert_eq!(comps(&c), vec!["a", "**", "b"]);
    }

    #[test]
    fn slash_in_bracket_is_a_clear_error() {
        for p in ["src/[a/b].rs", "[/]x", "a/[!/]"] {
            match compile(p) {
                Err(GlobError::Pattern(e)) => {
                    assert_eq!(e.kind, PatternErrorKind::SlashInBracket, "{}", p);
                    assert_eq!(&p[e.pos..e.pos + 1], "[", "{} pos {}", p, e.pos);
                }
                _ => panic!("{} should fail", p),
            }
        }
    }

    #[test]
    fn split_trailing_slash_requires_dir() {
        let (_, c) = compile("src/*/").unwrap();
        assert!(c.require_dir);
    }
}
