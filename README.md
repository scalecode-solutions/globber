# globber

AI-native glob for the SIF ecosystem.

A ground-up Rust rewrite of Unix glob, rooted in the POSIX `glob(3)` and `fnmatch(3)` specifications, built for AI agent workloads.

## What's different

| Feature | `glob` (rust-lang) | `globber` |
|---------|--------------------|-----------|
| Matching engine | Recursive backtracking O(2^n) | Thompson NFA O(n*m) |
| Output | `PathBuf` | `Entry` (path + size + kind + tokens_est) |
| Format | Rust iterator | SIF v1 document or plain paths |
| Patterns | Single | Single, multi-pattern `Ruleset`, or brace expansion |
| Negation | None | `--exclude`, `--gitignore`, `Ruleset::exclude()` |
| Budget | None | `--token-budget`, `--byte-budget`, `--limit` |
| Classification | None | `FileKind` (source, test, config, generated, ...) |
| Parallelism | None | rayon — parallel readdir + stat |
| Preview | None | `--preview code:15` — skip preamble, show code |
| Git | None | `--git-changed main` — only modified files |

## Install

```sh
cargo install globber
```

## Quick start

```sh
# Scope a project — files, sizes, token estimates, code previews, summary
globber '**/*.rs' -g -k source -P code:15 -S

# Budget-aware context packing for LLMs
globber '**/*.{rs,go,py}' -g -t 80K -S

# Files changed since main branch
globber '**/*.rs' -G main -P code:10

# Plain paths, gitignore-aware
globber '**' -g -k source -p
```

## Library usage

```rust
use globber::{Pattern, glob, Ruleset, MatchOptions};

// Pure pattern matching (POSIX fnmatch equivalent)
let pat = Pattern::new("*.rs").unwrap();
assert!(pat.matches("main.rs"));

// Filesystem walk (POSIX glob equivalent)
for entry in glob("src/**/*.rs").unwrap() {
    if let Ok(e) = entry {
        println!("{} ({} tokens)", e.path.display(), e.tokens_est);
    }
}

// Multi-pattern ruleset with priorities
let rules = Ruleset::new()
    .include("src/**/*.rs")
    .exclude("**/generated/**")
    .build()
    .unwrap();
assert!(rules.is_match("src/main.rs"));
```

## SIF output

Default output is a [SIF v1](https://github.com/scalecode-solutions/sif-parser) document:

```
#!sif v1
#context File listing produced by globber
#schema path:str:path	size:uint	kind:str:311	tokens_est:uint	is_dir:bool
src/main.rs	1024	source	293	false
src/lib.rs	856	source	245	false
```

Part of the SIF ecosystem: `sif-parser`, `sif-scratch`, STP, SWT, SIL.

## License

MIT OR Apache-2.0
