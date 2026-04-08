//! `globber` CLI — AI-native glob with SIF output.
//!
//! A ground-up Rust rewrite of Unix glob, rooted in the POSIX glob(3)
//! specification, enhanced for AI agent workloads.
//!
//! Emits results as SIF v1 documents by default, or plain paths with `--paths`.

use std::env;
use std::process;

use globber::{
    expand_braces, glob_with, to_paths, to_sif, Entry, FileKind,
    MatchOptions, Ruleset, WalkOptions,
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
    limit: usize,
    byte_budget: u64,
    token_budget: u64,
    hidden: bool,
    only_dirs: bool,
    summary: bool,
    kind_filter: Vec<FileKind>,
    max_depth: usize,
    no_stat: bool,
    gitignore: bool,
    preview: usize,
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
        limit: 0,
        byte_budget: 0,
        token_budget: 0,
        hidden: false,
        only_dirs: false,
        summary: false,
        kind_filter: Vec::new(),
        max_depth: 0,
        no_stat: false,
        gitignore: false,
        preview: 0,
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
            "--dirs" | "-d" => ga.only_dirs = true,
            "--preview" | "-P" => {
                let val = args.next().ok_or("--preview requires a number of lines")?;
                ga.preview = val.parse().map_err(|_| "--preview must be a number")?;
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
                ga.max_depth = val.parse().map_err(|_| "--depth must be a number")?;
            }
            "--exclude" | "-e" => {
                let val = args.next().ok_or("--exclude requires a pattern")?;
                ga.excludes.push(val);
            }
            "--limit" | "-n" => {
                let val = args.next().ok_or("--limit requires a number")?;
                ga.limit = val.parse().map_err(|_| "--limit must be a number")?;
            }
            "--byte-budget" => {
                let val = args.next().ok_or("--byte-budget requires a number")?;
                ga.byte_budget = parse_size(&val)?;
            }
            "--token-budget" | "-t" => {
                let val = args.next().ok_or("--token-budget requires a number")?;
                ga.token_budget = parse_size(&val)?;
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

    // If --root is set, prepend it to patterns.
    let root_prefix = ga.root.as_deref().unwrap_or("");

    // Expand braces in all patterns, prepend root.
    let expanded: Vec<String> = ga
        .patterns
        .iter()
        .flat_map(|p| expand_braces(p))
        .map(|p| {
            if root_prefix.is_empty() {
                p
            } else {
                let sep = if root_prefix.ends_with('/') { "" } else { "/" };
                format!("{}{}{}", root_prefix, sep, p)
            }
        })
        .collect();

    let match_opts = MatchOptions {
        require_literal_leading_dot: !ga.hidden,
        ..MatchOptions::new()
    };

    let base_walk_opts = WalkOptions {
        match_opts,
        sorted: ga.sorted,
        limit: ga.limit,
        byte_budget: ga.byte_budget,
        token_budget: ga.token_budget,
        only_dirs: ga.only_dirs,
        max_depth: ga.max_depth,
        no_stat: ga.no_stat,
        gitignore: ga.gitignore,
        ..WalkOptions::default()
    };

    // If we have excludes, use a Ruleset. Otherwise, simple walk.
    let entries: Vec<Entry> = if !ga.excludes.is_empty() {
        let mut builder = Ruleset::new();
        for p in &expanded {
            builder = builder.include(p);
        }
        for e in &ga.excludes {
            for ep in expand_braces(e) {
                builder = builder.exclude(&ep);
            }
        }
        builder = builder.match_options(match_opts);
        let ruleset = builder.build().map_err(|e| e.to_string())?;

        // Walk with ** from root (or cwd), filter through ruleset.
        let walk_pattern = if root_prefix.is_empty() {
            "**".to_string()
        } else {
            let sep = if root_prefix.ends_with('/') { "" } else { "/" };
            format!("{}{}**", root_prefix, sep)
        };
        let results = globber::walk(&walk_pattern, base_walk_opts).map_err(|e| e.to_string())?;
        let all: Vec<Entry> = results.into_iter().filter_map(|r| r.ok()).collect();
        ruleset
            .filter(&all)
            .into_iter()
            .cloned()
            .collect()
    } else {
        let mut all = Vec::new();
        let mut remaining_opts = base_walk_opts.clone();
        for p in &expanded {
            let results = glob_with(p, remaining_opts.clone()).map_err(|e| e.to_string())?;
            for r in results {
                if let Ok(e) = r {
                    // Subtract consumed budget for subsequent variants.
                    if remaining_opts.token_budget > 0 {
                        remaining_opts.token_budget =
                            remaining_opts.token_budget.saturating_sub(e.tokens_est);
                    }
                    if remaining_opts.byte_budget > 0 {
                        remaining_opts.byte_budget =
                            remaining_opts.byte_budget.saturating_sub(e.size);
                    }
                    if remaining_opts.limit > 0 {
                        remaining_opts.limit = remaining_opts.limit.saturating_sub(1);
                    }
                    all.push(e);
                }
            }
            // Stop if budgets exhausted.
            if remaining_opts.limit == 0 && base_walk_opts.limit > 0 {
                break;
            }
            if remaining_opts.token_budget == 0 && base_walk_opts.token_budget > 0 {
                break;
            }
            if remaining_opts.byte_budget == 0 && base_walk_opts.byte_budget > 0 {
                break;
            }
        }
        all
    };

    // Filter by kind.
    let entries: Vec<Entry> = if ga.kind_filter.is_empty() {
        entries
    } else {
        entries
            .into_iter()
            .filter(|e| ga.kind_filter.contains(&e.kind))
            .collect()
    };

    // Filter by git-changed.
    let owned: Vec<Entry> = if let Some(ref ref_name) = ga.git_changed {
        let root_dir = ga.root.as_deref().unwrap_or(".");
        let changed = globber::git::changed_files(
            std::path::Path::new(root_dir),
            ref_name,
        )
        .map_err(|e| e.to_string())?;
        entries
            .into_iter()
            .filter(|e| {
                // Canonicalize for comparison — changed_files returns absolute paths.
                let canon = std::fs::canonicalize(&e.path).unwrap_or_else(|_| e.path.clone());
                changed.iter().any(|c| {
                    let c_canon = std::fs::canonicalize(c).unwrap_or_else(|_| c.clone());
                    canon == c_canon
                })
            })
            .collect()
    } else {
        entries
    };

    let elapsed = t0.elapsed();
    let budget = globber::BudgetInfo {
        token_budget: ga.token_budget,
        byte_budget: ga.byte_budget,
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
    if ga.preview > 0 && ga.format == OutputFormat::Sif {
        globber::write_preview(&owned, ga.preview, &mut output)
            .map_err(|e| e.to_string())?;
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
    eprint!(
        "\
globber {VERSION} — AI-native glob for the SIF ecosystem

POSIX-rooted pattern matching with linear-time NFA engine, multi-pattern
rulesets, budget-aware walking, file classification, and SIF v1 output.

USAGE
    globber [OPTIONS] <PATTERN>...          Walk filesystem, emit matches
    globber match [OPTIONS] <PATTERN> [INPUT]...   Pure pattern matching
    globber expand <PATTERN>                Expand braces, print variants
    globber --help                          This help
    globber --version                       Print version

PATTERNS
    ?            Match any single character
    *            Match any sequence (within one path component)
    **           Match zero or more path components (recursive)
    [abc]        Match one character in set
    [!abc]       Match one character NOT in set
    [a-z]        Match character range
    {{a,b,c}}      Brace expansion (generates multiple patterns)
    \\*           Literal escape

OPTIONS (glob)
    -r, --root <PATH>        Walk root directory (default: cwd)
    -p, --paths              Output plain paths instead of SIF
    -s, --sif                Output SIF v1 (default)
    -S, --summary            Append a §summary section with aggregates
    -a, --hidden             Include dotfiles (default: hidden)
    -d, --dirs               Only match directories
    -e, --exclude <PAT>      Exclude pattern (repeatable, like .gitignore !)
    -g, --gitignore          Respect .gitignore files (auto-exclude)
    -n, --limit <N>          Stop after N results
    -t, --token-budget <N>   Stop when estimated tokens exceed N (e.g. 80K)
        --byte-budget <N>    Stop when total bytes exceed N (e.g. 2M)
    -k, --kind <KIND,...>    Filter by kind (comma-separated, e.g. source,config)
    -P, --preview <N>        Include first N lines of each file in §preview section
    -G, --git-changed [REF]  Only files changed since REF (default: HEAD)
        --depth <N>          Max recursion depth (1=immediate children only)
        --no-sort            Disable alphabetical sorting (faster)
        --no-stat            Skip stat() — faster, omits size/token columns

OPTIONS (match)
    -i, --ignore-case        Case-insensitive matching

FILE KINDS
    source, test, config, build, doc, data, generated, binary

SIZE SUFFIXES
    K = 1,000   M = 1,000,000   G = 1,000,000,000

EXAMPLES
    globber 'src/**/*.rs'                   Find Rust source files
    globber '**/*.rs' -e '**/target/**'     Exclude build artifacts
    globber '**/*.rs' -r ~/project           Walk a specific directory
    globber '**/*.rs' -t 80K -S             Budget-aware with summary
    globber '**/*.rs' -k source -p          Only source files, plain paths
    globber '**/*.rs' -g -P 30 -S            Scope project: files + previews + summary
    globber '**/*.rs' -G main               Only files changed since main branch
    globber '**' --depth 1 -r ~/github      Shallow scan (immediate children)
    globber 'src/{{lib,main}}.rs'             Brace expansion
    globber match '*.rs' main.rs lib.rs     Pure pattern matching
    globber expand 'src/{{a,b,c}}.rs'         Print expanded patterns
    echo -e 'foo.rs\\nbar.py' | globber match '*.rs'   Filter stdin

SIF OUTPUT (default)
    #!sif v1
    #context File listing produced by globber
    #schema path:str:path  size:uint  kind:str:311  tokens_est:uint  is_dir:bool
    src/main.rs\t1024\tsource\t293\tfalse
    src/lib.rs\t856\tsource\t245\tfalse

    When used with --summary, appends:
    ---
    §summary
    #schema key:str:id  value:str
    total_files\t42
    total_tokens_est\t12400

DESIGN
    Pattern engine    NFA simulation — O(n*m), safe for LLM-generated patterns
    Matching          POSIX fnmatch(3) semantics + ** recursive + {{}} braces
    Walking           POSIX glob(3) model + budget + limit + fast literal paths
    Parallelism       rayon — parallel stat() calls during directory reads
    Gitignore         Reads .gitignore at each level, auto-skips .git/
    Output            SIF v1 documents — feeds directly into sif-parser / SIL / STP
    Classification    FileKind maps to SIF Classify codes (310=source, etc.)
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
