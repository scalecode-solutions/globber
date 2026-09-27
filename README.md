# globber

AI-native glob for the SIF ecosystem.

A ground-up Rust rewrite of Unix glob, rooted in the POSIX `glob(3)` and `fnmatch(3)` specifications, built for AI agent workloads.

## What's different

| Feature | `glob` (rust-lang) | `globber` |
|---------|--------------------|-----------|
| Matching engine | Recursive backtracking O(2^n) | Thompson NFA O(n*m) |
| Output | `PathBuf` | `Entry` (path + size + kind + tokens_est) |
| Format | Rust iterator | SIF v1 document or plain paths |
| Patterns | Single | Many patterns and brace alternatives in **one** walk, deduplicated |
| Negation | None | `--exclude`, `--gitignore` (full gitignore semantics), `Ruleset::exclude()` |
| Budget | None | `--token-budget`, `--byte-budget`, `--limit`, `--fit` packing that reports what it left out |
| Classification | None | `FileKind` (source, test, config, build, doc, data, generated, binary) |
| Parallelism | None | rayon — parallel readdir + stat, identical output to the sequential walk |
| Preview | None | `--preview code:15` — skip the preamble, show code |
| Git | None | `--git-changed main` (only what this branch changed), `--git-files` (exactly git's view) |

## Scope

glob answers *which paths match?* globber adds *what will reading them
cost, and did I see everything?* Requests are weighed against four tenets:

1. **Select and account; don't interpret.** globber chooses files and
   reports sizes, token estimates, kinds and what it left out. Deciding
   what a file *means* or how *important* it is belongs to other tools —
   feed their rankings in with `--prefer-from`.
2. **Filesystem and git's current state only.** Directory contents,
   metadata, `.gitignore`, the index and the working tree — not commit
   history.
3. **Nothing disappears silently.** Every file left out by a budget,
   limit, filter or output cap is counted or listed.
4. **One pass, one dependency.** A single walk per invocation; rayon is
   the only runtime dependency.

## Install

```sh
cargo install globber-ai
```

The crate is `globber-ai`; the binary and library are both `globber`.

## Quick start

```sh
# Scope a project — files, sizes, token estimates, code previews, summary
globber '**/*.rs' -g -k source -P code:15 -S

# Budget-aware context packing for LLMs (stop at 80K, or --fit to pack)
globber '**/*.{rs,go,py}' -g -t 80K -S
globber '**/*.{rs,go,py}' -g -t 80K --fit -S

# Files this branch changed relative to main
globber '**/*.rs' -G main -P code:10

# Plain paths, gitignore-aware, skipping generated code
globber '**' -g -k source -e '**/generated/**' -p

# Exactly the files git sees (tracked + untracked-not-ignored, submodules included)
globber '**/*.rs' --git-files -p
```

With `--fit`, files that don't fit leave holes in the sorted listing, so
globber always names them: a `§skipped` section (largest first), a stderr
note with `-p`, and `budget_skipped_*` lines in `-S`. A walk cut short by
`-n` or a stop-mode budget reports `stopped_early` in `-S`.

Limits never take `0`: `-n`, `-t`, `--byte-budget` and `--depth` require a
positive value, or `unlimited` to remove a cap explicitly. See
`globber --help` for the full reference.

## Library usage

```rust
use globber::{Pattern, glob, walk_many, Ruleset, WalkOptions};

// Pure pattern matching (POSIX fnmatch equivalent)
let pat = Pattern::new("*.rs").unwrap();
assert!(pat.matches("main.rs"));

// Filesystem walk (POSIX glob equivalent)
for entry in glob("src/**/*.rs").unwrap() {
    if let Ok(e) = entry {
        println!("{} ({} tokens)", e.path.display(), e.tokens_est);
    }
}

// Several patterns, one walk, with a budget
let opts = WalkOptions { token_budget: Some(80_000), ..WalkOptions::default() };
let results = walk_many(&["src/**/*.rs", "Cargo.toml"], opts).unwrap();

// Multi-pattern ruleset with priorities
let rules = Ruleset::new()
    .include("src/**/*.rs")
    .exclude("**/generated/**")
    .build()
    .unwrap();
assert!(rules.is_match("src/main.rs"));
```

## SIF output

Default output is a [SIF v1](https://github.com/scalecode-solutions/sif-parser) document (records are tab-separated):

```
#!sif v1
#context File listing produced by globber
#schema path:str:path size:uint kind:enum(source,test,config,build,doc,data,generated,binary,unknown) tokens_est:uint is_dir:bool
src/lib.rs	856	source	245	false
src/main.rs	1024	source	293	false
```

Previews are SIF code blocks:

```
---
§preview
#block code language=rust file=src/main.rs lines=3-12
use std::env;
...
#/block
```

Part of the SIF ecosystem: `sif-parser`, `sif-scratch`, STP, SWT, SIL.

## License

MIT OR Apache-2.0
