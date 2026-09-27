# Changelog

All notable changes to `globber-ai` (binary and library: `globber`).
0.7.0–0.7.2 were not published separately; 0.7.3 was the first 0.7
release on crates.io.

## 0.7.4

### Fixed
- **Non-UTF-8 file names are no longer silently dropped.** Patterns match
  a U+FFFD-replaced copy of the name while the path keeps its exact
  bytes. `-p` prints exact bytes; SIF (which is UTF-8) shows the lossy
  form and `-S` counts them as `non_utf8_names`. `--git-files` and `-G`
  read git's raw output, so such paths survive there too.
- **Deleted files show up under `-G`.** Files changed since the ref but
  deleted from the working tree can't be walked, so matching ones (by
  pattern and `-k`) are listed in a `§deleted` section, counted as
  `git_deleted` in `-S`, and noted on stderr with `-p`.

### Library
- `PruneCounts::non_utf8_names`; `ChangedSet::deleted()`;
  `SifOptions::{deleted, deleted_since}`.

## 0.7.3

- `-S` shows `prefer_from <stdin|FILE>` with how many results got a score
  (`0 of N` means the ranking's paths didn't line up with the walk's).
- Clearer `--skipped 0` error.

## 0.7.2

### Added
- `--prefer size|size-asc|recent|path` chooses which matches `-n` and
  budgets go to first, and the output order. `--prefer-from FILE|-`
  ranks by another tool's scores (JSON Lines, `path<TAB>score`, or
  `uniq -c` output), so rankings such as churn stay outside globber.
- `--max-output-tokens N|unlimited` caps globber's own output (default
  25K for SIF; `-p` uncapped unless set). Previews shrink first (to at
  least 3 lines), then preview blocks, `§skipped` rows and records are
  dropped, each marked with a SIF `#truncated` directive.
- `-S` counts what each rule left out, with a note on how to include it:
  `pruned_hidden`, `pruned_gitignore`, `pruned_exclude`,
  `pruned_nested_repos`, `pruned_symlink_dirs`, `filtered_kind`,
  `filtered_git_changed`, `preview_skipped_binary`.
- `--skipped N|none|unlimited`; `--content-budget` alias for `-t`.
- SIF `#sort`, `#limit`, and `#truncated reason=limit|token_budget|byte_budget`.
- README "Scope" section with the tenets new features are weighed against.

### Changed
- `--git-files`: tracked dotfiles match wildcards without `-a`.
- The `§summary` schema gained an optional `note` column.

## 0.7.1

### Added
- `--fit` reporting: a `§skipped` section lists what the budget left out
  (largest first), `-S` adds `budget_skipped_*` and `stopped_early`.
- `--git-files`: take the file list from `git ls-files` (tracked files
  even if ignored, submodules included).
- Nested repositories are gitignore boundaries; `--skip-nested-repos`.
- `core.excludesFile` is honored.
- Clear error for `/` inside a bracket expression.

## 0.7.0

A rework of the walker, CLI and output. **Breaking changes** from 0.6.9
are marked ⚠.

### Walker
- One engine for every walk: all patterns and brace alternatives share a
  single traversal, results are deduplicated and globally path-sorted,
  and parallel and sequential walks give identical output. Up to ~14×
  faster for brace patterns.
- ⚠ Symlinked directories are listed but not entered by default; `-L`
  follows them with cycle detection.
- ⚠ Paths never carry a `./` prefix.
- ⚠ `--depth` counts from the first wildcard component (the parallel
  walker was off by one).
- Fixed: `-e` returned nothing for relative patterns; `-k`, `-G` and `-e`
  now apply before `-n` and budgets are charged; overlapping patterns
  produced duplicates.

### Gitignore
- Real gitignore semantics: anchored rules, `dir/` rules, per-file bases,
  last match wins, parent `.gitignore` files up to the repo root,
  `.git/info/exclude`, the global excludes file.

### CLI
- ⚠ `-n`, `-t`, `--byte-budget`, `--depth` and `-P` reject `0`; use
  `unlimited` for no cap.
- ⚠ `-G REF` measures from the merge base of REF and HEAD, so `-G main`
  lists what the branch changed. Unknown refs are an error, and a glob
  after `-G` is a pattern, not a ref.
- `--fit`, `--option=value`, `--`, `-L`; `--help` goes to stdout;
  warnings when `-S`/`-P` are ignored with `-p`.
- `-P code:N` chooses comment syntax per language (Rust `#[..]` and C
  `#include` count as code; docstrings and block comments are skipped).
- `[^abc]`, POSIX classes like `[[:alpha:]]`, and escapes inside brackets.
- Broader file classification (by path component; more extensions).

### SIF output
- ⚠ Spec fixes: `#schema` fields are space-separated; the kind column is
  `enum(...)`; code blocks use `file=` and `language=`; awkward paths
  are quoted.

### Library
- ⚠ `WalkOptions` limits, budgets and depth are `Option`; new
  `walk_many`, `walk_many_report`, `EntryFilter`, `BudgetMode`,
  `exclude`, `follow_symlinks`.
- ⚠ `PatternErrorKind` changed (`#[non_exhaustive]`); brace expansion
  is capped at `MAX_BRACE_EXPANSIONS`.
- `FileKind::classify_code` is deprecated (its numbers collide with the
  SIF Classify registry).
