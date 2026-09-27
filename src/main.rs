//! `globber` CLI — AI-native glob with SIF output.
//!
//! A ground-up Rust rewrite of Unix glob, rooted in the POSIX glob(3)
//! specification, enhanced for AI agent workloads.
//!
//! Emits results as SIF v1 documents by default, or plain paths with `--paths`.

use std::env;
use std::process;

use globber::{
    expand_braces, to_paths, to_sif, Entry, EntryFilter, FileKind, MatchOptions,
    WalkOptions,
};

const VERSION: &str = env!("CARGO_PKG_VERSION");

// ── Argument parsing ─────────────────────────────────────────────────

struct Args {
    command: Command,
}

enum Command {
    Glob(GlobArgs),
    Match(MatchArgs),
    Expand(String),
    Help,
    Version,
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
    preview: Option<globber::PreviewMode>,
    git_changed: Option<String>,
}

struct MatchArgs {
    pattern: String,
    inputs: Vec<String>,
    case_insensitive: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum OutputFormat {
    Sif,
    Paths,
}

fn parse_args() -> Result<Args, String> {
    let mut args = env::args().skip(1).peekable();

    // No arguments → help.
    if args.peek().is_none() {
        return Ok(Args { command: Command::Help });
    }

    let first = args.peek().unwrap().clone();
    match first.as_str() {
        "--help" | "-h" | "help" => return Ok(Args { command: Command::Help }),
        "--version" | "-V" => return Ok(Args { command: Command::Version }),
        "match" => {
            args.next();
            return parse_match_args(args);
        }
        "expand" => {
            args.next();
            let pattern = args.next().ok_or("expand requires a pattern")?;
            return Ok(Args { command: Command::Expand(pattern) });
        }
        _ => {}
    }

    // Default command: glob
    parse_glob_args(args)
}

fn parse_glob_args(
    mut args: std::iter::Peekable<std::iter::Skip<env::Args>>,
) -> Result<Args, String> {
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
        preview: None,
        git_changed: None,
    };

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => return Ok(Args { command: Command::Help }),
            "--version" | "-V" => return Ok(Args { command: Command::Version }),
            "--paths" | "-p" => ga.format = OutputFormat::Paths,
            "--sif" | "-s" => ga.format = OutputFormat::Sif,
            "--summary" | "-S" => ga.summary = true,
            "--no-sort" => ga.sorted = false,
            "--no-stat" => ga.no_stat = true,
            "--gitignore" | "-g" => ga.gitignore = true,
            "--hidden" | "-a" => ga.hidden = true,
            "--follow" | "-L" => ga.follow = true,
            "--dirs" | "-d" => ga.only_dirs = true,
            "--preview" | "-P" => {
                let val = args.next().ok_or("--preview requires a spec (N, N-M, or code:N)")?;
                ga.preview = Some(globber::PreviewMode::parse(&val)?);
            }
            "--git-changed" | "-G" => {
                // Optional ref argument; defaults to HEAD if next arg looks like a flag or is absent.
                let ref_name = match args.peek() {
                    Some(next) if !next.starts_with('-') => args.next().unwrap(),
                    _ => "HEAD".to_string(),
                };
                ga.git_changed = Some(ref_name);
            }
            "--root" | "-r" => {
                let val = args.next().ok_or("--root requires a path")?;
                ga.root = Some(val);
            }
            "--depth" => {
                let val = args.next().ok_or("--depth requires a number")?;
                ga.max_depth = Some(val.parse().map_err(|_| "--depth must be a number")?).filter(|&d| d > 0);
            }
            "--exclude" | "-e" => {
                let val = args.next().ok_or("--exclude requires a pattern")?;
                ga.excludes.push(val);
            }
            "--limit" | "-n" => {
                let val = args.next().ok_or("--limit requires a number")?;
                ga.limit = Some(val.parse().map_err(|_| "--limit must be a number")?).filter(|&n| n > 0);
            }
            "--byte-budget" => {
                let val = args.next().ok_or("--byte-budget requires a number")?;
                ga.byte_budget = Some(parse_size(&val)?).filter(|&n| n > 0);
            }
            "--token-budget" | "-t" => {
                let val = args.next().ok_or("--token-budget requires a number")?;
                ga.token_budget = Some(parse_size(&val)?).filter(|&n| n > 0);
            }
            "--kind" | "-k" => {
                let val = args.next().ok_or("--kind requires a value")?;
                for k in val.split(',') {
                    ga.kind_filter.push(parse_kind(k.trim())?);
                }
            }
            s if s.starts_with('-') => {
                return Err(format!("unknown option: {}", s));
            }
            _ => {
                ga.patterns.push(arg);
            }
        }
    }

    if ga.patterns.is_empty() {
        return Err("no patterns given".to_string());
    }

    Ok(Args { command: Command::Glob(ga) })
}

fn parse_match_args(
    mut args: std::iter::Peekable<std::iter::Skip<env::Args>>,
) -> Result<Args, String> {
    let mut ma = MatchArgs {
        pattern: String::new(),
        inputs: Vec::new(),
        case_insensitive: false,
    };

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-i" | "--ignore-case" => ma.case_insensitive = true,
            s if s.starts_with('-') => return Err(format!("unknown option: {}", s)),
            _ => {
                if ma.pattern.is_empty() {
                    ma.pattern = arg;
                } else {
                    ma.inputs.push(arg);
                }
            }
        }
    }

    if ma.pattern.is_empty() {
        return Err("match requires a pattern".to_string());
    }

    Ok(Args { command: Command::Match(ma) })
}

fn parse_size(s: &str) -> Result<u64, String> {
    let s = s.trim();
    let (num, mult) = if s.ends_with('K') || s.ends_with('k') {
        (&s[..s.len() - 1], 1_000u64)
    } else if s.ends_with('M') || s.ends_with('m') {
        (&s[..s.len() - 1], 1_000_000)
    } else if s.ends_with('G') || s.ends_with('g') {
        (&s[..s.len() - 1], 1_000_000_000)
    } else {
        (s, 1)
    };
    let n: u64 = num.parse().map_err(|_| format!("invalid number: {}", s))?;
    Ok(n * mult)
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
            "unknown kind: {:?} (try: source, test, config, build, doc, data, generated, binary)",
            s
        )),
    }
}

// ── Commands ─────────────────────────────────────────────────────────

fn cmd_glob(ga: GlobArgs) -> Result<(), String> {
    let t0 = std::time::Instant::now();

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
        exclude,
        filter,
        ..WalkOptions::default()
    };

    let results = globber::walk_many(&patterns, opts).map_err(|e| e.to_string())?;
    let owned: Vec<Entry> = results.into_iter().filter_map(|r| r.ok()).collect();

    let elapsed = t0.elapsed();
    let budget = globber::BudgetInfo {
        token_budget: ga.token_budget.unwrap_or(0),
        byte_budget: ga.byte_budget.unwrap_or(0),
        wall_time_ms: elapsed.as_millis() as u64,
    };

    // Build the SIF output string so we can append preview.
    let mut output = match ga.format {
        OutputFormat::Sif => {
            if ga.summary {
                globber::to_sif_with_summary_and_budget(&owned, &budget)
            } else {
                to_sif(&owned)
            }
        }
        OutputFormat::Paths => to_paths(&owned),
    };

    // Append preview section.
    if let Some(ref mode) = ga.preview {
        if ga.format == OutputFormat::Sif {
            globber::write_preview(&owned, mode, &mut output)
                .map_err(|e| e.to_string())?;
        }
    }

    print!("{}", output);
    Ok(())
}

fn cmd_match(ma: MatchArgs) -> Result<(), String> {
    let pattern = globber::Pattern::new(&ma.pattern).map_err(|e| e.to_string())?;
    let opts = MatchOptions {
        case_sensitive: !ma.case_insensitive,
        ..MatchOptions::new()
    };

    // If no inputs given, read from stdin.
    let inputs: Vec<String> = if ma.inputs.is_empty() {
        use std::io::BufRead;
        std::io::stdin()
            .lock()
            .lines()
            .filter_map(|l| l.ok())
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

fn cmd_expand(pattern: &str) {
    for p in expand_braces(pattern) {
        println!("{}", p);
    }
}

// ── Help ─────────────────────────────────────────────────────────────

fn print_help() {
    eprint!("\
globber {VERSION} — AI-native glob for the SIF ecosystem

  A ground-up Rust rewrite of Unix glob, rooted in the POSIX glob(3) and
  fnmatch(3) specifications, built for AI agent workloads. Linear-time NFA
  pattern matching, parallel directory walking, token-budget-aware traversal,
  file classification, and native SIF v1 output.

  Part of the SIF ecosystem: sif-parser, sif-scratch, STP, SWT, SIL.

USAGE

  globber [OPTIONS] <PATTERN>...
      Walk the filesystem, match files against one or more glob patterns,
      and emit results as a SIF v1 document (or plain paths with --paths).

  globber match [OPTIONS] <PATTERN> [INPUT]...
      Pure string pattern matching (no filesystem). Tests each INPUT against
      PATTERN and prints matches. Reads from stdin if no INPUTs given.

  globber expand <PATTERN>
      Expand brace expressions and print each resulting pattern.

PATTERNS

  ?              Match any single character (not path separator).
  *              Match any sequence of characters within one path component.
  **             Match zero or more path components (recursive descent).
  [abc]          Match one character in the set.
  [!abc]         Match one character NOT in the set.
  [a-z]          Match a character range.
  {{a,b,c}}        Brace expansion — generates one pattern per alternative.
  \\x             Literal escape — match the next character verbatim.

  Patterns follow POSIX fnmatch(3) semantics. ** must be a standalone path
  component (a/**/b is valid, a**b is not). Braces can be nested.

OPTIONS

  -r, --root <PATH>
      Set the walk root directory. Patterns are relative to this path.
      Default: current working directory.

  -p, --paths
      Output plain file paths (one per line) instead of SIF.

  -s, --sif
      Output SIF v1 document. This is the default.

  -S, --summary
      Append a §summary section with aggregate statistics: total files,
      total bytes, total estimated tokens, kind breakdown, wall time,
      and budget remaining (if a budget was set).

  -P, --preview <SPEC>
      Append a §preview section with source lines from each matched file.
      Three formats:

        -P 10         First 10 lines (literal, includes comments/blanks).
        -P 15-30      Lines 15 through 30 (1-indexed, inclusive range).
        -P code:10    10 lines of code, skipping the leading comment block
                      and blank lines. Jumps past // headers, # shebangs,
                      /* block comments */, and blank lines to the first
                      line of actual code.

      Previews are emitted as SIF #block/#/block pairs with path and line
      metadata. Binary files are skipped automatically.

  -a, --hidden
      Include dotfiles and dot-directories in results. By default, entries
      starting with '.' are hidden (matching POSIX FNM_PERIOD behavior).

  -d, --dirs
      Only match directories. Files are excluded from results.

  -e, --exclude <PATTERN>
      Exclude files matching PATTERN. Can be repeated. Supports the same
      glob syntax as include patterns. Excludes are applied after includes,
      like .gitignore negation.

  -g, --gitignore
      Respect .gitignore files found during the walk. Loads .gitignore at
      each directory level and filters entries accordingly. Also auto-
      excludes .git/ directories. Handles negation patterns (! prefix).

  -G, --git-changed [REF]
      Only include files that have changed since REF. Uses git diff under
      the hood. Includes uncommitted changes, staged changes, and untracked
      files. Default REF: HEAD. Examples: -G main, -G HEAD~5, -G v1.0.0.

  -n, --limit <N>
      Stop after N matched results. The walk terminates early — directories
      that would only produce results beyond the limit are never read.

  -t, --token-budget <N>
      Stop when the cumulative estimated token count across all matched
      files exceeds N. Tokens are estimated at ~3.5 bytes/token for files
      with stat, or by extension heuristics in --no-stat mode. Accepts
      size suffixes: 80K, 500K, 2M. The budget carries across brace-
      expanded pattern variants.

  --byte-budget <N>
      Stop when cumulative matched file bytes exceed N. Accepts suffixes.

  -k, --kind <KIND,...>
      Filter results to files of the specified kind(s). Comma-separated.
      Kinds: source, test, config, build, doc, data, generated, binary.
      Example: -k source,config

  --depth <N>
      Maximum recursion depth for ** patterns. Depth 1 returns only
      immediate children of the walk root. Depth is counted from where
      the ** starts, not from the filesystem root.

  --no-sort
      Disable alphabetical sorting of results. Faster for large trees
      when order doesn't matter.

  --no-stat
      Skip full stat() calls on each file. Uses DirEntry::file_type()
      (free on Linux) and extension-based heuristics for token estimation.
      SIF output omits size and tokens_est columns. Significantly faster
      for large trees when you only need paths and kinds.

OPTIONS (match)

  -i, --ignore-case
      Case-insensitive pattern matching (ASCII only).

EXAMPLES

  Basics:
    globber 'src/**/*.rs'                       Find all Rust files under src/
    globber '**/*.{{rs,go,py}}' -r ~/project      Multi-language search with braces
    globber '**' --depth 1 -r ~/github          Shallow scan of a directory

  AI context packing:
    globber '**/*.rs' -g -t 80K -S              Budget-aware: stop at 80K tokens
    globber '**/*.rs' -g -k source -P code:15   Scope a project in one shot
    globber '**' -g -k source,config -S         Source + config files with summary

  Git workflow:
    globber '**/*.rs' -G main                   Files changed since main branch
    globber '**' -G HEAD -k source -P code:10   Preview uncommitted source changes

  Filtering:
    globber '**/*.rs' -e '**/target/**'         Manual exclude
    globber '**' -g -k source -p                Plain paths, gitignore-aware
    globber '**/*.rs' --no-stat -p              Fast listing without metadata

  Pure matching (no filesystem):
    globber match '*.rs' main.rs lib.rs         Test strings against a pattern
    echo -e 'foo.rs\\nbar.py' | globber match '*.rs'

  Brace expansion:
    globber expand 'src/{{lib,main,util/{{a,b}}}}.rs'

SIF OUTPUT

  Default output is a SIF v1 document:

    #!sif v1
    #context File listing produced by globber
    #schema path:str:path  size:uint  kind:str:311  tokens_est:uint  is_dir:bool
    src/main.rs     1024    source  293     false
    src/lib.rs      856     source  245     false

  With --summary (-S), appends:

    ---
    §summary
    #schema key:str:id  value:str
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
    #block code path=src/main.rs lines=8-17
    use std::env;
    use std::process;
    fn main() {{
        let args = parse_args();
    ...
    #/block

  The SIF output is directly consumable by sif-parser, SIL pipelines,
  sif-scratch slots, and any SIF-aware toolchain.

DESIGN

  Pattern engine     Thompson NFA simulation — O(pattern * input) worst case.
                     Safe for untrusted and LLM-generated patterns. No
                     exponential backtracking.

  Matching           POSIX fnmatch(3) semantics: *, ?, [...], [!...], \\escape.
                     Extended with ** (recursive) and {{}} (brace expansion).

  Walking            POSIX glob(3) model. Fast path: literal components skip
                     readdir and go straight to stat. Recursive ** patterns
                     use a fully parallel walker (rayon) that fans out readdir
                     calls across threads. Sequential fallback for budget-
                     limited walks requiring early termination.

  Parallelism        rayon thread pool. Parallel walker achieves 800-1000%
                     CPU utilization on large trees. 430K files in ~1.4s.

  Gitignore          Reads .gitignore at each directory level during the walk.
                     Supports negation (! patterns). Auto-excludes .git/.

  Classification     Extension-based FileKind inference (source, test, config,
                     build, doc, data, generated, binary). Maps to SIF Classify
                     codes (310=source, 312=test, 320=config, etc.).

  Token estimation   Byte-based (size / 3.5) with stat, or extension-heuristic
                     median file sizes without stat. Budget-precise to within
                     ~600 tokens of a 500K budget.

  Git integration    git diff --name-only for changed-file filtering. Covers
                     committed, staged, and untracked changes.
"
    );
}

// ── Entry point ──────────────────────────────────────────────────────

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {}", e);
            eprintln!("try: globber --help");
            process::exit(2);
        }
    };

    let result = match args.command {
        Command::Help => {
            print_help();
            Ok(())
        }
        Command::Version => {
            println!("globber {}", VERSION);
            Ok(())
        }
        Command::Glob(ga) => cmd_glob(ga),
        Command::Match(ma) => cmd_match(ma),
        Command::Expand(pat) => {
            cmd_expand(&pat);
            Ok(())
        }
    };

    if let Err(e) = result {
        eprintln!("error: {}", e);
        process::exit(1);
    }
}
