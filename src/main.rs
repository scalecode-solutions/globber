//! `globber` CLI — AI-native glob with SIF output.
//!
//! A ground-up Rust rewrite of Unix glob, rooted in the POSIX glob(3)
//! specification, enhanced for AI agent workloads.
//!
//! Emits results as SIF v1 documents by default, or plain paths with `--paths`.

use std::env;
use std::process;

use globber::{
    to_paths, to_sif_with, BudgetInfo, BudgetMode, Prefer, Entry, EntryFilter, FileKind, MatchOptions, PreviewMode,
    SifOptions, WalkOptions,
};

const VERSION: &str = env!("CARGO_PKG_VERSION");

// ── Argument parsing ─────────────────────────────────────────────────

enum Command {
    Glob(Box<GlobArgs>),
    Match(MatchArgs),
    Expand(String),
    Help(HelpTopic),
    Version,
}

#[derive(Clone, Copy)]
enum HelpTopic {
    Main,
    Match,
    Expand,
}

struct GlobArgs {
    patterns: Vec<String>,
    excludes: Vec<String>,
    root: Option<String>,
    format: OutputFormat,
    sorted: bool,
    limit: Option<usize>,
    byte_budget: Option<u64>,
    token_budget: Option<u64>,
    hidden: bool,
    only_dirs: bool,
    summary: bool,
    kind_filter: Vec<FileKind>,
    max_depth: Option<usize>,
    no_stat: bool,
    gitignore: bool,
    follow: bool,
    fit: bool,
    skip_nested_repos: bool,
    git_files: bool,
    prefer: Option<String>,
    prefer_from: Option<String>,
    preview: Option<PreviewMode>,
    git_changed: Option<String>,
}

struct MatchArgs {
    pattern: String,
    inputs: Vec<String>,
    case_insensitive: bool,
    pathname: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum OutputFormat {
    Sif,
    Paths,
}

/// Command-line arguments, with `--flag=value` split into flag and value.
struct ArgStream {
    args: std::iter::Peekable<std::vec::IntoIter<String>>,
    /// Value attached with `=` to the flag being processed.
    inline: Option<String>,
}

impl ArgStream {
    fn new(args: Vec<String>) -> Self {
        ArgStream { args: args.into_iter().peekable(), inline: None }
    }

    /// The next raw argument, splitting `--flag=value`.
    fn next_flag(&mut self) -> Option<String> {
        let arg = self.args.next()?;
        if arg.starts_with("--") {
            if let Some((flag, value)) = arg.split_once('=') {
                self.inline = Some(value.to_string());
                return Some(flag.to_string());
            }
        }
        Some(arg)
    }

    /// The value for `flag`: inline (`--flag=v`) or the next argument.
    fn value(&mut self, flag: &str, what: &str) -> Result<String, String> {
        self.inline
            .take()
            .or_else(|| self.args.next())
            .ok_or_else(|| format!("{} requires {}", flag, what))
    }

    /// Fail if a flag that takes no value was given one with `=`.
    fn no_value(&mut self, flag: &str) -> Result<(), String> {
        match self.inline.take() {
            Some(v) => Err(format!("{} does not take a value (got {:?})", flag, v)),
            None => Ok(()),
        }
    }
}

fn parse_args(raw: Vec<String>) -> Result<Command, String> {
    let mut args = ArgStream::new(raw);

    let Some(first) = args.args.peek().cloned() else {
        return Ok(Command::Help(HelpTopic::Main));
    };
    match first.as_str() {
        "--help" | "-h" => return Ok(Command::Help(HelpTopic::Main)),
        "--version" | "-V" => return Ok(Command::Version),
        "help" => {
            args.args.next();
            let topic = match args.args.next().as_deref() {
                Some("match") => HelpTopic::Match,
                Some("expand") => HelpTopic::Expand,
                _ => HelpTopic::Main,
            };
            return Ok(Command::Help(topic));
        }
        "match" => {
            args.args.next();
            return parse_match_args(args);
        }
        "expand" => {
            args.args.next();
            return match args.args.next() {
                Some(h) if h == "--help" || h == "-h" => Ok(Command::Help(HelpTopic::Expand)),
                Some(pattern) => Ok(Command::Expand(pattern)),
                None => Err("expand requires a pattern".to_string()),
            };
        }
        _ => {}
    }

    parse_glob_args(args)
}

fn parse_glob_args(mut args: ArgStream) -> Result<Command, String> {
    let mut ga = GlobArgs {
        patterns: Vec::new(),
        excludes: Vec::new(),
        root: None,
        format: OutputFormat::Sif,
        sorted: true,
        limit: None,
        byte_budget: None,
        token_budget: None,
        hidden: false,
        only_dirs: false,
        summary: false,
        kind_filter: Vec::new(),
        max_depth: None,
        no_stat: false,
        gitignore: false,
        follow: false,
        fit: false,
        skip_nested_repos: false,
        git_files: false,
        prefer: None,
        prefer_from: None,
        preview: None,
        git_changed: None,
    };

    while let Some(flag) = args.next_flag() {
        match flag.as_str() {
            "--" => {
                ga.patterns.extend(args.args.by_ref());
                break;
            }
            "--help" | "-h" => return Ok(Command::Help(HelpTopic::Main)),
            "--version" | "-V" => return Ok(Command::Version),
            "--paths" | "-p" => ga.format = OutputFormat::Paths,
            "--sif" | "-s" => ga.format = OutputFormat::Sif,
            "--summary" | "-S" => ga.summary = true,
            "--no-sort" => ga.sorted = false,
            "--no-stat" => ga.no_stat = true,
            "--gitignore" | "-g" => ga.gitignore = true,
            "--hidden" | "-a" => ga.hidden = true,
            "--follow" | "-L" => ga.follow = true,
            "--fit" => ga.fit = true,
            "--skip-nested-repos" => ga.skip_nested_repos = true,
            "--git-files" => ga.git_files = true,
            "--prefer" => {
                let val = args.value(&flag, "an order (path, size, size-asc, recent)")?;
                if !matches!(val.as_str(), "path" | "size" | "size-asc" | "recent") {
                    return Err(format!(
                        "unknown --prefer order: {:?} (try: path, size, size-asc, recent; or --prefer-from FILE)",
                        val
                    ));
                }
                ga.prefer = Some(val);
            }
            "--prefer-from" => ga.prefer_from = Some(args.value(&flag, "a file or -")?),
            "--dirs" | "-d" => ga.only_dirs = true,
            "--preview" | "-P" => {
                let val = args.value(&flag, "a spec (N, N-M, or code:N)")?;
                ga.preview = Some(PreviewMode::parse(&val)?);
            }
            "--git-changed" | "-G" => {
                // The ref is optional. The next argument is taken as the ref
                // unless it is a flag or looks like a glob (refs cannot
                // contain `*`, `?` or `[`); use --git-changed=REF to be explicit.
                let ref_name = match args.inline.take() {
                    Some(r) => r,
                    None => match args.args.peek() {
                        Some(next) if !next.starts_with('-') && !next.contains(['*', '?', '[']) => {
                            args.args.next().unwrap()
                        }
                        _ => "HEAD".to_string(),
                    },
                };
                ga.git_changed = Some(ref_name);
            }
            "--root" | "-r" => ga.root = Some(args.value(&flag, "a path")?),
            "--depth" => ga.max_depth = parse_count(&flag, &args.value(&flag, "a number")?)?,
            "--exclude" | "-e" => ga.excludes.push(args.value(&flag, "a pattern")?),
            "--limit" | "-n" => ga.limit = parse_count(&flag, &args.value(&flag, "a number")?)?,
            "--byte-budget" => {
                ga.byte_budget = parse_budget(&flag, &args.value(&flag, "a size")?)?;
            }
            "--token-budget" | "-t" => {
                ga.token_budget = parse_budget(&flag, &args.value(&flag, "a size")?)?;
            }
            "--kind" | "-k" => {
                let val = args.value(&flag, "a kind list")?;
                for k in val.split(',') {
                    ga.kind_filter.push(parse_kind(k.trim())?);
                }
            }
            s if s.starts_with('-') && s.len() > 1 => {
                return Err(format!("unknown option: {}", s));
            }
            _ => ga.patterns.push(flag.clone()),
        }
        args.no_value(&flag)?;
    }

    if ga.patterns.is_empty() {
        return Err("no patterns given".to_string());
    }
    if ga.prefer.is_some() && ga.prefer_from.is_some() {
        return Err("use either --prefer or --prefer-from, not both".to_string());
    }
    if ga.no_stat && matches!(ga.prefer.as_deref(), Some("size" | "size-asc" | "recent")) {
        return Err("--prefer size/recent needs file metadata; drop --no-stat".to_string());
    }
    if ga.git_files && ga.only_dirs {
        return Err("--git-files lists files only; it can't be combined with --dirs".to_string());
    }
    Ok(Command::Glob(Box::new(ga)))
}

fn parse_match_args(mut args: ArgStream) -> Result<Command, String> {
    let mut ma = MatchArgs {
        pattern: String::new(),
        inputs: Vec::new(),
        case_insensitive: false,
        pathname: false,
    };
    let mut have_pattern = false;

    while let Some(flag) = args.next_flag() {
        match flag.as_str() {
            "--help" | "-h" => return Ok(Command::Help(HelpTopic::Match)),
            "-i" | "--ignore-case" => ma.case_insensitive = true,
            "--pathname" => ma.pathname = true,
            "--" => {
                for a in args.args.by_ref() {
                    if have_pattern {
                        ma.inputs.push(a);
                    } else {
                        ma.pattern = a;
                        have_pattern = true;
                    }
                }
                break;
            }
            s if s.starts_with('-') && s.len() > 1 => {
                return Err(format!("unknown option: {}", s));
            }
            _ if !have_pattern => {
                ma.pattern = flag.clone();
                have_pattern = true;
            }
            _ => ma.inputs.push(flag.clone()),
        }
        args.no_value(&flag)?;
    }

    if !have_pattern {
        return Err("match requires a pattern".to_string());
    }
    Ok(Command::Match(ma))
}

/// Parse a count flag: a positive integer, or `unlimited`.
fn parse_count(flag: &str, val: &str) -> Result<Option<usize>, String> {
    if val == "unlimited" {
        return Ok(None);
    }
    match val.parse::<usize>() {
        Ok(0) => Err(format!(
            "{} must be at least 1 (use `{} unlimited` for no limit)",
            flag, flag
        )),
        Ok(n) => Ok(Some(n)),
        Err(_) => Err(format!(
            "{} expects a positive number or `unlimited`, got {:?}",
            flag, val
        )),
    }
}

/// Parse a budget flag: a positive size (see [`parse_size`]), or `unlimited`.
fn parse_budget(flag: &str, val: &str) -> Result<Option<u64>, String> {
    if val == "unlimited" {
        return Ok(None);
    }
    match parse_size(val)? {
        0 => Err(format!(
            "{} must be greater than 0 (use `{} unlimited` for no budget)",
            flag, flag
        )),
        n => Ok(Some(n)),
    }
}

/// Parse a size with an optional decimal suffix: `500`, `80K`, `1.5M`, `2G`.
fn parse_size(s: &str) -> Result<u64, String> {
    let s = s.trim();
    let invalid = || format!("invalid size: {:?} (examples: 500, 80K, 1.5M, 2G)", s);
    let (num, mult) = match s.char_indices().last() {
        Some((i, 'K' | 'k')) => (&s[..i], 1_000u64),
        Some((i, 'M' | 'm')) => (&s[..i], 1_000_000),
        Some((i, 'G' | 'g')) => (&s[..i], 1_000_000_000),
        _ => (s, 1),
    };
    if let Ok(n) = num.parse::<u64>() {
        return n.checked_mul(mult).ok_or_else(invalid);
    }
    let f: f64 = num.parse().map_err(|_| invalid())?;
    let v = f * mult as f64;
    if !v.is_finite() || v < 0.0 || v >= u64::MAX as f64 {
        return Err(invalid());
    }
    Ok(v.round() as u64)
}

fn parse_kind(s: &str) -> Result<FileKind, String> {
    match s {
        "source" => Ok(FileKind::Source),
        "test" => Ok(FileKind::Test),
        "config" => Ok(FileKind::Config),
        "build" => Ok(FileKind::Build),
        "doc" => Ok(FileKind::Doc),
        "data" => Ok(FileKind::Data),
        "generated" => Ok(FileKind::Generated),
        "binary" => Ok(FileKind::Binary),
        "unknown" => Ok(FileKind::Unknown),
        _ => Err(format!(
            "unknown kind: {:?} (try: source, test, config, build, doc, data, generated, binary, unknown)",
            s
        )),
    }
}

// ── Commands ─────────────────────────────────────────────────────────

fn cmd_glob(ga: GlobArgs) -> Result<(), String> {
    let t0 = std::time::Instant::now();

    if ga.format == OutputFormat::Paths {
        if ga.summary {
            eprintln!("warning: --summary has no effect with --paths");
        }
        if ga.preview.is_some() {
            eprintln!("warning: --preview has no effect with --paths");
        }
    }

    // Normalize --root: "." and "./" mean the current directory.
    let root = ga
        .root
        .as_deref()
        .map(|r| if r.len() > 1 { r.trim_end_matches('/') } else { r })
        .filter(|r| *r != "." && !r.is_empty());
    let with_root = |p: &str| -> String {
        let p = p.strip_prefix("./").unwrap_or(p);
        match root {
            Some(r) if !p.starts_with('/') => {
                let sep = if r.ends_with('/') { "" } else { "/" };
                format!("{}{}{}", r, sep, p)
            }
            _ => p.to_string(),
        }
    };

    let patterns: Vec<String> = ga.patterns.iter().map(|p| with_root(p)).collect();

    // Excludes containing `/` are paths relative to the root; bare names
    // match at any depth.
    let mut exclude = Vec::new();
    for e in &ga.excludes {
        for ep in globber::try_expand_braces(e).map_err(|e| e.to_string())? {
            let ep = if ep.contains('/') { with_root(&ep) } else { ep };
            exclude.push(globber::Pattern::new(&ep).map_err(|e| e.to_string())?);
        }
    }

    // Kind and git-changed filters run inside the walk, before limits and
    // budgets are charged.
    let changed = match ga.git_changed {
        Some(ref ref_name) => {
            let root_dir = root.unwrap_or(".");
            Some(globber::git::ChangedSet::new(
                &globber::git::changed_files(std::path::Path::new(root_dir), ref_name)?,
            ))
        }
        None => None,
    };
    let kinds = ga.kind_filter.clone();
    let filter = if kinds.is_empty() && changed.is_none() {
        None
    } else {
        Some(EntryFilter::new(move |e: &Entry| {
            (kinds.is_empty() || kinds.contains(&e.kind))
                && changed.as_ref().is_none_or(|c| c.contains(&e.path))
        }))
    };

    let prefer = match (&ga.prefer, &ga.prefer_from) {
        (_, Some(src)) => {
            let text = if src == "-" {
                std::io::read_to_string(std::io::stdin()).map_err(|e| format!("reading stdin: {}", e))?
            } else {
                std::fs::read_to_string(src).map_err(|e| format!("reading {}: {}", src, e))?
            };
            let map = globber::ScoreMap::parse(&text, root.map(std::path::Path::new))
                .map_err(|e| format!("--prefer-from {}: {}", src, e))?;
            Prefer::Scores(map)
        }
        (Some(p), None) => match p.as_str() {
            "size" => Prefer::Size,
            "size-asc" => Prefer::SizeAsc,
            "recent" => Prefer::Recent,
            _ => Prefer::Path,
        },
        (None, None) => Prefer::Path,
    };

    let opts = WalkOptions {
        match_opts: MatchOptions {
            require_literal_leading_dot: !ga.hidden,
            ..MatchOptions::new()
        },
        sorted: ga.sorted,
        limit: ga.limit,
        byte_budget: ga.byte_budget,
        token_budget: ga.token_budget,
        only_dirs: ga.only_dirs,
        max_depth: ga.max_depth,
        no_stat: ga.no_stat,
        gitignore: ga.gitignore,
        follow_symlinks: ga.follow,
        skip_nested_repos: ga.skip_nested_repos,
        git_files: ga.git_files,
        prefer: prefer.clone(),
        budget_mode: if ga.fit { BudgetMode::Fit } else { BudgetMode::Stop },
        exclude,
        filter,
        ..WalkOptions::default()
    };

    let report = globber::walk_many_report(&patterns, opts).map_err(|e| e.to_string())?;
    let mut entries: Vec<Entry> = Vec::with_capacity(report.results.len());
    let mut errors = Vec::new();
    for r in report.results {
        match r {
            Ok(e) => entries.push(e),
            Err(e) => errors.push(e),
        }
    }
    if let Some(first) = errors.first() {
        let more = match errors.len() {
            1 => String::new(),
            n => format!(" (and {} more)", n - 1),
        };
        eprintln!("warning: skipped unreadable directory: {}{}", first, more);
    }

    // A --fit skip leaves a hole in the middle of the sorted results; say so
    // wherever the SIF §skipped section won't be seen.
    if !report.budget_skipped.is_empty() && ga.format == OutputFormat::Paths {
        let largest = report.budget_skipped.iter().max_by_key(|e| e.tokens_est).unwrap();
        eprintln!(
            "note: --fit left out {} matching file(s) over budget, largest: {} (~{} tokens)",
            report.budget_skipped.len(),
            largest.path.display(),
            largest.tokens_est
        );
    }

    let output = match ga.format {
        OutputFormat::Paths => to_paths(&entries),
        OutputFormat::Sif => {
            let summary = ga.summary.then(|| BudgetInfo {
                token_budget: ga.token_budget,
                byte_budget: ga.byte_budget,
                wall_time_ms: t0.elapsed().as_millis() as u64,
                unreadable_dirs: errors.len(),
                budget_mode: if ga.fit { BudgetMode::Fit } else { BudgetMode::Stop },
                stopped_early: report.stopped_early,
            });
            let order = match prefer {
                Prefer::Path if !ga.sorted => None,
                p => Some(p),
            };
            let sif_opts = SifOptions {
                no_stat: ga.no_stat,
                summary,
                order,
                budget_skipped: report.budget_skipped,
            };
            let mut out = to_sif_with(&entries, &sif_opts);
            if let Some(ref mode) = ga.preview {
                globber::write_preview(&entries, mode, &mut out).map_err(|e| e.to_string())?;
            }
            out
        }
    };

    print!("{}", output);
    Ok(())
}

fn cmd_match(ma: MatchArgs) -> Result<(), String> {
    let pattern = globber::Pattern::new(&ma.pattern).map_err(|e| e.to_string())?;
    let opts = MatchOptions {
        case_sensitive: !ma.case_insensitive,
        require_literal_separator: ma.pathname,
        ..MatchOptions::new()
    };

    // If no inputs given, read from stdin.
    let inputs: Vec<String> = if ma.inputs.is_empty() {
        use std::io::BufRead;
        std::io::stdin()
            .lock()
            .lines()
            .map_while(Result::ok)
            .collect()
    } else {
        ma.inputs
    };

    let mut matched = false;
    for input in &inputs {
        if pattern.matches_with(input, opts) {
            println!("{}", input);
            matched = true;
        }
    }

    if !matched {
        process::exit(1);
    }
    Ok(())
}

fn cmd_expand(pattern: &str) -> Result<(), String> {
    for p in globber::try_expand_braces(pattern).map_err(|e| e.to_string())? {
        println!("{}", p);
    }
    Ok(())
}

// ── Help ─────────────────────────────────────────────────────────────

fn print_help(topic: HelpTopic) {
    match topic {
        HelpTopic::Main => print!("{}", HELP_MAIN.replace("{VERSION}", VERSION)),
        HelpTopic::Match => print!("{}", HELP_MATCH),
        HelpTopic::Expand => print!("{}", HELP_EXPAND),
    }
}

const HELP_MATCH: &str = "\
globber match [OPTIONS] <PATTERN> [INPUT]...

  Pure string pattern matching (no filesystem). Tests each INPUT against
  PATTERN and prints the ones that match. Reads inputs from stdin (one per
  line) if none are given. Exits 1 if nothing matched.

  By default `*` and `?` also match `/`, so '*.rs' matches src/main.rs.
  Use --pathname for filesystem-walk semantics, where they stop at `/`.

OPTIONS
  -i, --ignore-case   Case-insensitive matching (ASCII only).
      --pathname      `*` and `?` do not match `/` (POSIX FNM_PATHNAME).
  --                  Treat all following arguments as pattern/inputs.

EXAMPLES
  globber match '*.rs' main.rs lib.py          prints main.rs
  globber match --pathname '*.rs' src/main.rs  no match (exit 1)
  printf 'a.rs\\nb.py\\n' | globber match '*.rs'
";

const HELP_EXPAND: &str = "\
globber expand <PATTERN>

  Expand brace expressions and print each resulting pattern, one per line,
  in order. Braces nest; `\\{`, `\\}` and `\\,` are literal; an unmatched `{`
  is left as-is. Expansions are capped at 10,000 patterns.

EXAMPLE
  globber expand 'src/{lib,main,util/{a,b}}.rs'
";

const HELP_MAIN: &str = "\
globber {VERSION} — AI-native glob for the SIF ecosystem

  A ground-up Rust rewrite of Unix glob, rooted in the POSIX glob(3) and
  fnmatch(3) specifications, built for AI agent workloads. Linear-time NFA
  pattern matching, parallel directory walking, token-budget-aware traversal,
  file classification, and native SIF v1 output.

USAGE

  globber [OPTIONS] <PATTERN>...
      Walk the filesystem once, matching files against every PATTERN, and
      emit the results (each path at most once, sorted) as a SIF v1
      document, or plain paths with --paths.

  globber match [OPTIONS] <PATTERN> [INPUT]...     (see: globber help match)
      Pure string pattern matching (no filesystem).

  globber expand <PATTERN>                         (see: globber help expand)
      Expand brace expressions and print each resulting pattern.

PATTERNS

  ?              Match any single character (not path separator).
  *              Match any sequence of characters within one path component.
  **             Match zero or more path components (recursive descent).
  [abc]          Match one character in the set.
  [!abc] [^abc]  Match one character NOT in the set.
  [a-z]          Match a character range.
  [[:alpha:]]    POSIX character class (alpha digit alnum upper lower space
                 punct xdigit blank cntrl graph print).
  {a,b,c}        Brace expansion. All alternatives share a single walk.
  \\x             Literal escape — match the next character verbatim.
  dir/           A trailing slash matches directories only.

  Patterns follow POSIX fnmatch(3) semantics. ** must be a standalone path
  component (a/**/b is valid, a**b is not). Braces can be nested. A leading
  ./ is ignored, so './src/*.rs' and 'src/*.rs' are the same pattern.
  src/** matches everything under src/, not src itself.

OPTIONS

  -r, --root <PATH>
      Walk root. Patterns and excludes containing / are relative to it.
      Output paths include the root as given. Default: current directory.

  -p, --paths
      Output plain file paths (one per line) instead of SIF.

  -s, --sif
      Output a SIF v1 document. This is the default.

  -S, --summary
      Append a §summary section: total files, dirs, bytes, estimated
      tokens, kind breakdown, wall time, budget remaining (if a budget
      was set), what --fit skipped, stopped_early (limit, token_budget or
      byte_budget) if the walk was cut short — more matches may exist —
      and unreadable directories. SIF output only.

  -P, --preview <SPEC>
      Append a §preview section with an excerpt of each matched text file.
      SIF output only. Every number must be at least 1.

        -P 10         The first 10 lines.
        -P 15-30      Lines 15 through 30 (1-indexed, inclusive).
        -P code:10    10 consecutive lines starting at the first line of
                      actual code. The leading preamble is skipped: blank
                      lines, shebang, license headers, line and block
                      comments, and docstrings, using the comment syntax
                      of the file's language (so Rust #[derive] and C
                      #include count as code, a Python # comment does not).
                      Lines after the first line of code are shown as-is,
                      comments included. A file with no code falls back to
                      its first 10 lines.

      Each excerpt is a SIF code block with language, file and lines
      attributes; lines= is the exact 1-indexed range shown. Binary files
      (by extension, or a NUL byte in the first 16 KB) are skipped.

  -a, --hidden
      Let wildcards match names starting with '.'. By default they don't
      (POSIX FNM_PERIOD); a pattern component that starts with a literal
      '.' (like .github/** or .env*) always matches.

  -d, --dirs
      Only yield directories.

  -L, --follow
      Descend into symlinked directories found during the walk (cycles
      are detected and skipped). By default they are listed but not
      entered. A symlink named literally in the pattern is always followed.

  -e, --exclude <PATTERN>
      Exclude entries matching PATTERN. Repeatable; supports braces. A
      pattern without / matches the file or directory name at any depth
      (-e '*.md', -e node_modules). A pattern with / matches the path
      relative to the root (-e 'docs/**', -e '**/generated/**'). An
      excluded directory is skipped entirely, contents included.

  -g, --gitignore
      Skip files ignored by git: .gitignore files (including those in
      parent directories up to the repository root), .git/info/exclude,
      and the global excludes file (core.excludesFile, or git's default
      $XDG_CONFIG_HOME/git/ignore / ~/.config/git/ignore). Also skips .git/ directories. Directories
      named literally at the start of the pattern are never skipped.
      A nested repository (a directory containing .git, e.g. a submodule)
      is a boundary: the outer repository's rules stop applying inside
      it and its own take over, as in git. Tracked files that match an
      ignore rule are skipped (a walk can't see git's index); use
      --git-files for git's exact view.

  --git-files
      Take the file list from git instead of reading directories: exactly
      the files git considers part of the working tree (git ls-files
      --cached --others --exclude-standard), including tracked files that
      match an ignore rule, and recursing into submodules and nested
      repositories. Patterns, -e, -k, -G, limits and budgets apply as
      usual. Yields files only (no directories); -g and -L are implied /
      irrelevant. Often faster than a walk on large repositories.

  --skip-nested-repos
      Don't descend into nested repositories or submodules below the root
      (with --git-files: don't list their files).

  -G, --git-changed [REF]
      Only include files changed since REF: committed, staged, unstaged,
      and untracked. Changes are measured from the merge base of REF and
      HEAD, so -G main lists what this branch changed. Default REF: HEAD
      (uncommitted work). An unknown REF is an error. The next argument
      is taken as REF unless it starts with - or contains * ? [ — use
      --git-changed=REF to be explicit. Examples: -G main, -G HEAD~5.

  -n, --limit <N | unlimited>
      Stop after N results (N >= 1). Results are sorted, so this is the
      first N paths in order; the walk stops as soon as they are found.

  -t, --token-budget <N | unlimited>
      Stop at the first result that would push the estimated token total
      over N (N > 0). Accepts suffixes: 80K, 1.5M. Estimates are size/3.5
      (or per-extension medians with --no-stat).

  --byte-budget <N | unlimited>
      Like --token-budget, for total file bytes.

  --prefer <ORDER>
      Which matches -n and budgets go to first, and the output order:
        path       Path order (default). The walk can stop early.
        size       Largest files first; size-asc for smallest first.
        recent     Most recently modified first (adds a modified column).
      Ties break by path. Any order but path ranks every match first, so
      the whole tree is walked. With -t 60K --fit --prefer size, the
      biggest files get the budget and smaller ones fill the gaps.

  --prefer-from <FILE | ->
      Order by scores from another tool (highest first; unscored files
      last) and add a score column. One entry per line, as JSON Lines
      {\"path\": \"src/a.rs\", \"score\": 12}, path<TAB>score, or `score path`
      (uniq -c output). Relative paths are relative to the root. For
      example, prefer the files changed most often in the last 90 days:
        git log --since=90.days --name-only --format= | sort | uniq -c \\
          | globber 'src/**' -t 60K --fit --prefer-from -

  --fit
      With a budget: instead of stopping at the first result that doesn't
      fit, skip it and keep going, packing as many results as fit (in
      sorted order). Walks the whole tree. Skipped files leave holes in
      the sorted listing, so they are always reported: a §skipped section
      lists the 50 largest (with --paths, a note on stderr), and -S adds
      budget_skipped_files / _tokens_est / _bytes.

  -k, --kind <KIND,...>
      Only yield files of the given kind(s). Comma-separated: source, test,
      config, build, doc, data, generated, binary, unknown.

  --depth <N | unlimited>
      Maximum depth (N >= 1), counted from the first wildcard component:
      '**' --depth 1 lists the root's immediate children; 'src/**/*.rs'
      --depth 1 finds files directly in src/.

  --no-sort
      Don't sort results. Slightly faster for large trees.

  --no-stat
      Skip stat() on each file: classification and extension-based token
      estimates only. SIF output drops the size and tokens_est columns.

  --
      Treat every following argument as a pattern.

  -n, -t, --byte-budget and --depth never accept 0; `unlimited` (the
  default) explicitly removes a cap, e.g. to override an earlier value.
  Long options also accept --option=value. Kind, git-changed and exclude
  filters are applied before limits and budgets are charged.

EXAMPLES

  Basics:
    globber 'src/**/*.rs'                       Find all Rust files under src/
    globber '**/*.{rs,go,py}' -r ~/project      Multi-language search, one walk
    globber '**' --depth 1 -r ~/github          Shallow scan of a directory

  AI context packing:
    globber '**/*.rs' -g -t 80K -S              Budget-aware: stop at 80K tokens
    globber '**/*.rs' -g -t 80K --fit           Pack as many files as fit in 80K
    globber '**/*.rs' -g -k source -P code:15   Scope a project in one shot
    globber '**' -g -k source,config -S         Source + config files with summary

  Git workflow:
    globber '**/*.rs' -G main                   Files this branch changed vs main
    globber '**' -G -k source -P code:10        Preview uncommitted source changes

  Filtering:
    globber '**/*.rs' -e target -e '**/gen/**'  Manual excludes
    globber '**' -g -k source -p                Plain paths, gitignore-aware
    globber '**/*.rs' --git-files -p            Exactly git's view of the repo
    globber '**/*.rs' --no-stat -p              Fast listing without metadata

SIF OUTPUT

  Default output is a SIF v1 document (records are tab-separated):

    #!sif v1
    #context File listing produced by globber
    #schema path:str:path size:uint kind:enum(source,test,...) tokens_est:uint is_dir:bool
    src/main.rs     1024    source  293     false
    src/lib.rs      856     source  245     false

  With --summary (-S), appends:

    ---
    §summary
    #schema key:str:id value:str
    total_files         42
    total_tokens_est    12400
    token_budget        80000
    token_budget_remaining  67600
    wall_time_ms        24
    kind_source         38
    kind_config         4

  With --preview (-P code:10), appends:

    ---
    §preview
    #block code language=rust file=src/main.rs lines=8-17
    use std::env;
    ...
    #/block

DESIGN

  Pattern engine     Thompson NFA simulation — O(pattern * input) worst case.
                     Safe for untrusted and LLM-generated patterns. Brace
                     expansion is capped at 10,000 alternatives.

  Walking            One engine for every walk. Each directory is read once
                     and its children matched against all patterns at once.
                     Literal components are stat()ed, not listed. Without a
                     limit or budget, subdirectories fan out across threads
                     (rayon); with one, the walk runs depth-first in sorted
                     order and stops early. Both give identical output.

  Classification     Extension- and path-based FileKind inference (source,
                     test, config, build, doc, data, generated, binary).

  Git integration    git merge-base + git diff + git ls-files.
";

// ── Entry point ──────────────────────────────────────────────────────

fn main() {
    let command = match parse_args(env::args().skip(1).collect()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {}", e);
            eprintln!("try: globber --help");
            process::exit(2);
        }
    };

    let result = match command {
        Command::Help(topic) => {
            print_help(topic);
            Ok(())
        }
        Command::Version => {
            println!("globber {}", VERSION);
            Ok(())
        }
        Command::Glob(ga) => cmd_glob(*ga),
        Command::Match(ma) => cmd_match(ma),
        Command::Expand(pat) => cmd_expand(&pat),
    };

    if let Err(e) = result {
        eprintln!("error: {}", e);
        process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_suffixes() {
        assert_eq!(parse_size("500"), Ok(500));
        assert_eq!(parse_size("80K"), Ok(80_000));
        assert_eq!(parse_size("1.5M"), Ok(1_500_000));
        assert_eq!(parse_size("2g"), Ok(2_000_000_000));
        assert!(parse_size("99999999999999999999G").is_err());
        assert!(parse_size("20000000000G").is_err());
        assert!(parse_size("-1").is_err());
        assert!(parse_size("K").is_err());
        assert!(parse_size("1e400").is_err());
    }

    #[test]
    fn counts_reject_zero() {
        assert!(parse_count("-n", "0").is_err());
        assert_eq!(parse_count("-n", "5"), Ok(Some(5)));
        assert_eq!(parse_count("-n", "unlimited"), Ok(None));
        assert!(parse_budget("-t", "0").is_err());
        assert!(parse_budget("-t", "0K").is_err());
        assert_eq!(parse_budget("-t", "unlimited"), Ok(None));
    }
}
