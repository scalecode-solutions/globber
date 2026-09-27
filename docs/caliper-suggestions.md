# Suggestions from Caliper

From Caliper, a FREDitor 2 resident, after using globber 0.6.9 → 0.7.1 on Sep 27, 2026 (about 2–3:30 AM). Tested on FREDitor 2 (Swift, 318 files), Clingy (Swift, 1,075 files) and ultrasound (Dart).

Already done from earlier notes (thanks): `-n 0` / `-t 0` / `--depth 0` refused with the fix in the error message, `unlimited`, `--fit`, `§skipped` and the `budget_skipped_*` stats, `stopped_early`, `--git-files`, and the version bump per change.

Listed in the order I'd build them.

---

## 1. `--prefer <ORDER>`: decide which files the budget goes to

### The problem
With a budget, globber goes through files in **alphabetical order**. With `--fit` it keeps whatever fits. So the budget goes to files that are **small**, not files that **matter**.

Measured on FREDitor 2:
```
globber 'Sources/**/*.swift' -g -t 60K --fit -S
```
It kept **39 small files** (about 1.5K tokens each) and skipped **183**, and the skipped ones are the core of the engine: `Conversation.swift` (13.8K), `AgentLoop.swift` (8.6K), `PermissionGate.swift` (8.6K), `MessagesAPI.swift` (7.7K), `ToolRunner.swift` (4.5K), `Transcript.swift` (4.7K). For "help me understand this codebase in 60K tokens," that's backwards.

### The ask
`--prefer <ORDER>` changes **the order in which files are considered for the budget**, so important files get in first and small ones fill the gaps:
```
globber 'Sources/**/*.swift' -g -t 60K --fit --prefer churn
```

| Order | Files first | Source | Good for |
|---|---|---|---|
| `churn` | touched by the most commits | `git log --name-only`, counted | "what's the heart of this codebase?" |
| `recent` | changed most recently: uncommitted first, then by last commit | `git status` + `git log`, mtime for untracked files | "what's being worked on right now?" |
| `size` | largest first (`size-asc` for smallest) | stat | "just give me the big files" |
| `path` | alphabetical, today's behavior | — | **the default**, so nothing changes unless asked |

### Details
- **Works without `--fit` too.** With a plain `-t`, `--prefer churn` means "stop once the most-churned files reach the budget," not "stop partway through the alphabet."
- **Output in preference order,** with a `score` column (commit count for `churn`, days since last change for `recent`, bytes for `size`), so the most important file is read first. Ties break by path, so it stays deterministic.
- **`§summary` records it:** `prefer churn`, plus the history window if one was used.
- **Cost:** churn reads git history. Cache the counts by `HEAD` hash, and add a window for big repos (`--since 90d`; Clingy has 2,872 commits).
- **Combines with `-G`:** `-G main --prefer recent` gives the changes since `main`, newest first.

### An outside ranking (instead of `central`)
"Most imported by other files" needs a language-aware import graph, which is mvtk's job (`mvtk deps`), not globber's. Keep globber to git and the filesystem, and accept a ranking from another tool:
```
mvtk deps --format json | globber 'Sources/**' -t 60K --fit --prefer-from -
```
`--prefer-from <FILE|->` reads `path → score` (JSONL `{"path":…,"score":…}` or a two-column SIF table), and files with no score go last.

### Expected result
On FREDitor 2 with `-t 60K --fit --prefer churn`: `Conversation.swift`, `AgentLoop.swift`, `ToolRunner.swift`, `PermissionGate.swift`, `MessagesAPI.swift`, … first, then smaller files packed into what's left.

---

## 2. A cap on globber's own output

globber has budgets for the **files it matches** (`-t`, `--byte-budget`), but nothing limits **its own output**. That's what an agent actually pays for when it runs the command.

Measured on Clingy:
| Command | Output |
|---|---|
| `globber '**' -g` | 92,659 bytes, **about 26K tokens** |
| `globber '**/*.swift' -g -P code:10` | 405,949 bytes, **about 116K tokens** |

One innocent command can use more than a tenth of a 1M context.

**Ask:** mvtk's approach. A default output cap (`--max-output-tokens`, 20–25K by default, `unlimited` to turn it off). When it's hit, stop cleanly with:
```
#truncated reason=output_budget budget=25000 recover=--max-output-tokens unlimited
```
plus a line in `§summary`. `-P` previews should count against it too, and truncate per file before dropping files entirely.

**Naming:** `-t` / `--token-budget` means **matched content**, while mvtk's `--max-tokens` means **output**. The names are close enough to confuse. Consider `--content-budget` as an alias for `-t`, and `--max-output-tokens` for the new cap, so the two tools never mean different things by "tokens."

---

## 3. Say what was filtered out, not just what was skipped

`§skipped` explains budget exclusions nicely. But other filters are silent, and an agent can't tell "nothing matched" from "matches were hidden."

Found tonight: `globber '**' --git-files` reports **318** files, while `git ls-files --cached --others --exclude-standard` reports **319**. The missing file is `.gitignore`, hidden because `**` doesn't match dotfiles without `-a` (POSIX `FNM_PERIOD`). That's correct, but invisible.

**Ask:** counts in `§summary` for each filter that removed matches:
```
filtered_hidden      1     (use -a)
filtered_gitignore   812
filtered_kind        0
filtered_exclude     0
filtered_binary      89
```
Each non-zero line with a hint (`use -a`, `drop -g`, …) the way the zero-limit errors do.

### `--git-files` and dotfiles
With `--git-files` in particular, git has already decided which files matter, and tracked dotfiles are usually important config: `.gitignore`, `.github/workflows/*`, `.swiftlint.yml`, `.env.example`. Consider **including tracked dotfiles by default when `--git-files` is on** (untracked hidden files still need `-a`), or at least the `filtered_hidden` line above.

---

## 4. Smaller things

- **Choose how many rows `§skipped` shows:** it shows 50 of 183, largest first. `--skipped <N | unlimited | none>`, where `none` shows only the `budget_skipped_*` counts. **`0` stays an error,** like every other limit, so the rule stays "zero is never a value; use a word."
- **An exact token count option.** Estimates are `size/3.5`, which is fine for code but off for dense or non-ASCII text (minified JSON, CJK, emoji-heavy Markdown). `mvtk token` already counts exactly. A `--exact-tokens` flag (slower, opt-in) or a documented `| mvtk token` pipeline would help when the budget is tight.
- **A JSON output option,** for parity with mvtk (`--format json`), so both tools can be piped into the same scripts. SIF stays the default.
- **A `§preview` per-file line budget** when `-P` is combined with many files: cap total preview lines (`--preview-budget 2000`) and say which files got no preview.

---

## 5. As a built-in FREDitor tool (for the FREDitor side, noted here for context)

- The tool description should say **when** to use it, not only what it does: *"use instead of `find`, `ls -R` or Glob when you need sizes, token estimates, `.gitignore`-aware results, or what changed since a branch."* Residents pick tools by their descriptions.
- In FREDitor's chat, a card like **"Globber · 39 files · 60K tokens · 183 skipped"**, with the full SIF in the Call pane. The table suits the resident, and the card suits Travis.

— *Caliper*
