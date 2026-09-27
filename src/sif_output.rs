// SIF output formatting.
//
// Emits glob results as SIF documents — the native output format for
// the SIF ecosystem. Zero dependencies: SIF is just formatted text.
//
// Output schema:
//   #!sif v1
//   #schema path:str:path size:uint kind:enum(...) tokens_est:uint is_dir:bool
//
// This lets any SIF-aware tool (sif-parser, SIL pipelines, STP tools,
// sif-scratch slots) consume glob results directly.

use std::fmt::Write;

use crate::entry::Entry;
use crate::preview::{PreviewBlock, PreviewMode};
use crate::walker::{BudgetMode, Prefer, PruneCounts, StopReason};

/// The `kind` column type: every [`FileKind`](crate::FileKind) name.
const KIND_ENUM: &str = "enum(source,test,config,build,doc,data,generated,binary,unknown)";

/// Options for SIF emission.
#[derive(Debug, Clone)]
pub struct SifOptions {
    /// Entries came from a `no_stat` walk: omit the size and token columns
    /// (they would be zeros and guesses) and the byte/token totals.
    pub no_stat: bool,
    /// Append a §summary section with these budget figures.
    pub summary: Option<BudgetInfo>,
    /// The order the entries are in, if known: emits `#sort`, a `prefer`
    /// summary line, and for `Recent` / `Scores` a `modified` / `score`
    /// column. `None` = unsorted (no `#sort`).
    pub order: Option<Prefer>,
    /// The result limit in effect (`-n`), emitted as `#limit`.
    pub limit: Option<usize>,
    /// Why the walk stopped early, emitted as `#truncated`.
    pub stopped_early: Option<StopReason>,
    /// Matches left out by a `--fit` budget, listed in a §skipped section.
    pub budget_skipped: Vec<Entry>,
    /// How many §skipped rows to list, largest first. `None` = all;
    /// `Some(0)` = no section (the summary still counts them).
    pub skipped_rows: Option<usize>,
    /// Append a §preview section.
    pub preview: Option<PreviewMode>,
    /// Cap on the estimated tokens of the whole document. When the
    /// document would exceed it, previews are shortened, then dropped,
    /// then §skipped rows, then records — each marked with `#truncated`.
    pub max_output_tokens: Option<u64>,
}

impl Default for SifOptions {
    fn default() -> Self {
        SifOptions {
            no_stat: false,
            summary: None,
            order: None,
            limit: None,
            stopped_early: None,
            budget_skipped: Vec::new(),
            skipped_rows: Some(DEFAULT_SKIPPED_ROWS),
            preview: None,
            max_output_tokens: None,
        }
    }
}

/// The output cap shortens previews to no fewer lines than this before
/// dropping whole preview blocks.
pub const MIN_PREVIEW_LINES: usize = 3;

/// Default number of §skipped rows.
pub const DEFAULT_SKIPPED_ROWS: usize = 50;

/// Budget and timing figures for the §summary section.
#[derive(Debug, Clone, Default)]
pub struct BudgetInfo {
    /// Token budget, if one was set.
    pub token_budget: Option<u64>,
    /// Byte budget, if one was set.
    pub byte_budget: Option<u64>,
    /// Wall-clock time of the walk. 0 = omit.
    pub wall_time_ms: u64,
    /// Directories that could not be read during the walk.
    pub unreadable_dirs: usize,
    /// Budget behavior, reported when a budget is set.
    pub budget_mode: BudgetMode,
    /// Why the walk stopped early, if it did.
    pub stopped_early: Option<StopReason>,
    /// Entries left out by pruning rules.
    pub pruned: PruneCounts,
    /// Matches dropped by the `-k` kind filter.
    pub filtered_kind: usize,
    /// Matches dropped by `-G` (unchanged since the ref).
    pub filtered_git_changed: usize,
}

/// Format a list of entries as a SIF document string.
///
/// Output is a complete SIF v1 document with schema and records.
/// Each entry becomes one tab-separated record.
pub fn to_sif(entries: &[Entry]) -> String {
    to_sif_with(entries, &SifOptions::default())
}

/// Format entries as a SIF document with a summary section.
pub fn to_sif_with_summary(entries: &[Entry]) -> String {
    to_sif_with_summary_and_budget(entries, &BudgetInfo::default())
}

/// Format entries as a SIF document with a summary section and budget info.
pub fn to_sif_with_summary_and_budget(entries: &[Entry], budget: &BudgetInfo) -> String {
    to_sif_with(
        entries,
        &SifOptions { summary: Some(budget.clone()), ..SifOptions::default() },
    )
}

/// Write entries as SIF to any fmt::Write sink.
pub fn write_sif(entries: &[Entry], w: &mut dyn Write) -> std::fmt::Result {
    write_sif_with(entries, &SifOptions::default(), w)
}

/// Write entries as SIF with explicit options.
pub fn write_sif_with(entries: &[Entry], opts: &SifOptions, w: &mut dyn Write) -> std::fmt::Result {
    w.write_str(&to_sif_with(entries, opts))
}

/// Estimated tokens for `bytes` of output (the same size/3.5 estimate
/// used for files).
fn tokens_for(bytes: usize) -> u64 {
    (bytes as u64 * 2).div_ceil(7)
}

/// What the output cap removed.
#[derive(Debug, Default, Clone, Copy)]
struct Cuts {
    records_dropped: usize,
    skipped_rows_dropped: usize,
    /// Lines kept per preview block, if previews were shortened.
    preview_lines: Option<usize>,
    preview_blocks_dropped: usize,
}

impl Cuts {
    fn any(&self) -> bool {
        self.records_dropped > 0
            || self.skipped_rows_dropped > 0
            || self.preview_lines.is_some()
            || self.preview_blocks_dropped > 0
    }

    fn sections(&self) -> String {
        let mut parts = Vec::new();
        if self.records_dropped > 0 {
            parts.push("records");
        }
        if self.skipped_rows_dropped > 0 {
            parts.push("skipped");
        }
        if self.preview_lines.is_some() || self.preview_blocks_dropped > 0 {
            parts.push("preview");
        }
        parts.join(",")
    }
}

/// Format entries as a SIF document with explicit options.
pub fn to_sif_with(entries: &[Entry], opts: &SifOptions) -> String {
    // Build every section as data, then fit it to the output cap.
    let records: Vec<String> = entries.iter().map(|e| record_line(e, opts)).collect();

    let mut skipped: Vec<&Entry> = opts.budget_skipped.iter().collect();
    skipped.sort_by(|a, b| b.tokens_est.cmp(&a.tokens_est).then(a.path.cmp(&b.path)));
    let listed = opts.skipped_rows.map_or(skipped.len(), |n| n.min(skipped.len()));
    let skipped_rows: Vec<String> = skipped[..listed].iter().map(|e| skipped_line(e, opts.no_stat)).collect();

    let (blocks, binary_skipped) = match &opts.preview {
        Some(mode) => {
            let (b, n) = crate::preview::collect_previews(entries, mode);
            (Some(b), n)
        }
        None => (None, 0),
    };

    let cuts = match opts.max_output_tokens {
        Some(cap) => fit(cap, entries, opts, &records, &skipped_rows, blocks.as_deref(), binary_skipped, skipped.len()),
        None => Cuts::default(),
    };

    render(entries, opts, &records, &skipped_rows, skipped.len(), blocks.as_deref(), binary_skipped, cuts)
}

/// Decide what to cut so the document fits `cap` tokens.
#[allow(clippy::too_many_arguments)]
fn fit(
    cap: u64,
    entries: &[Entry],
    opts: &SifOptions,
    records: &[String],
    skipped_rows: &[String],
    blocks: Option<&[PreviewBlock]>,
    binary_skipped: usize,
    skipped_total: usize,
) -> Cuts {
    let size = |cuts: Cuts| {
        render(entries, opts, records, skipped_rows, skipped_total, blocks, binary_skipped, cuts).len()
    };
    let fits = |cuts: Cuts| tokens_for(size(cuts)) <= cap;

    let mut cuts = Cuts::default();
    if fits(cuts) {
        return cuts;
    }

    // 1. Shorten every preview to the same number of lines, but not below
    //    a few lines: a one-line excerpt says almost nothing.
    if let Some(blocks) = blocks.filter(|b| !b.is_empty()) {
        let longest = blocks.iter().map(|b| b.lines.len()).max().unwrap_or(1);
        let floor = MIN_PREVIEW_LINES.min(longest);
        if fits(Cuts { preview_lines: Some(floor), ..cuts }) {
            let (mut lo, mut hi) = (floor, longest);
            while lo < hi {
                let mid = (lo + hi).div_ceil(2);
                if fits(Cuts { preview_lines: Some(mid), ..cuts }) { lo = mid } else { hi = mid - 1 }
            }
            cuts.preview_lines = Some(lo);
            return cuts;
        }
        // 2. Drop preview blocks from the end, at the floor length.
        cuts.preview_lines = Some(floor);
        let (mut lo, mut hi) = (0, blocks.len()); // blocks to drop
        while lo < hi {
            let mid = (lo + hi) / 2;
            if fits(Cuts { preview_blocks_dropped: mid, ..cuts }) { hi = mid } else { lo = mid + 1 }
        }
        cuts.preview_blocks_dropped = lo;
        if lo < blocks.len() {
            return cuts;
        }
    }

    // 3. Drop §skipped rows (smallest first; the summary keeps the counts).
    let (mut lo, mut hi) = (0, skipped_rows.len());
    while lo < hi {
        let mid = (lo + hi) / 2;
        if fits(Cuts { skipped_rows_dropped: mid, ..cuts }) { hi = mid } else { lo = mid + 1 }
    }
    cuts.skipped_rows_dropped = lo;
    if fits(cuts) {
        return cuts;
    }

    // 4. Drop records from the end.
    let (mut lo, mut hi) = (0, records.len());
    while lo < hi {
        let mid = (lo + hi) / 2;
        if fits(Cuts { records_dropped: mid, ..cuts }) { hi = mid } else { lo = mid + 1 }
    }
    cuts.records_dropped = lo;
    cuts
}

fn record_line(e: &Entry, opts: &SifOptions) -> String {
    let path = sif_str(&e.path.to_string_lossy());
    let mut line = if opts.no_stat {
        format!("{}\t{}\t{}", path, e.kind, e.is_dir)
    } else {
        format!("{}\t{}\t{}\t{}\t{}", path, e.size, e.kind, e.tokens_est, e.is_dir)
    };
    match &opts.order {
        Some(Prefer::Recent) => {
            line.push('\t');
            line.push_str(&e.modified.map(format_datetime).unwrap_or_else(|| "_".into()));
        }
        Some(p @ Prefer::Scores(_)) => {
            line.push('\t');
            line.push_str(&p.score(e).map(format_float).unwrap_or_else(|| "_".into()));
        }
        _ => {}
    }
    line.push('\n');
    line
}

fn skipped_line(e: &Entry, no_stat: bool) -> String {
    let path = sif_str(&e.path.to_string_lossy());
    if no_stat {
        format!("{}\t{}\n", path, e.kind)
    } else {
        format!("{}\t{}\t{}\t{}\n", path, e.size, e.kind, e.tokens_est)
    }
}

const RECOVER: &str = "recover=\"--max-output-tokens unlimited\"";

#[allow(clippy::too_many_arguments)]
fn render(
    entries: &[Entry],
    opts: &SifOptions,
    records: &[String],
    skipped_rows: &[String],
    skipped_total: usize,
    blocks: Option<&[PreviewBlock]>,
    binary_skipped: usize,
    cuts: Cuts,
) -> String {
    let mut w = String::with_capacity(records.iter().map(String::len).sum::<usize>() + 1024);
    let budget = opts.max_output_tokens.unwrap_or(0);

    // Main section.
    let _ = writeln!(w, "#!sif v1");
    let _ = writeln!(w, "#context File listing produced by globber");
    let extra = match &opts.order {
        Some(Prefer::Recent) => " modified:datetime?",
        Some(Prefer::Scores(_)) => " score:float?",
        _ => "",
    };
    if opts.no_stat {
        let _ = writeln!(w, "#schema path:str:path kind:{} is_dir:bool{}", KIND_ENUM, extra);
    } else {
        let _ = writeln!(
            w,
            "#schema path:str:path size:uint kind:{} tokens_est:uint is_dir:bool{}",
            KIND_ENUM, extra
        );
    }
    if let Some(order) = &opts.order {
        let sort = match order {
            Prefer::Path => "path",
            Prefer::Size => "size desc",
            Prefer::SizeAsc => "size asc",
            Prefer::Recent => "modified desc",
            Prefer::Scores(_) => "score desc",
        };
        let _ = writeln!(w, "#sort {}", sort);
    }
    if let Some(limit) = opts.limit {
        let _ = writeln!(w, "#limit {}", limit);
    }
    if let Some(reason) = opts.stopped_early {
        let _ = writeln!(w, "#truncated reason={}", reason.as_str());
    }
    if cuts.records_dropped > 0 {
        let _ = writeln!(
            w,
            "#truncated reason=output_budget dropped={} budget={} {}",
            cuts.records_dropped, budget, RECOVER
        );
    }
    for r in &records[..records.len() - cuts.records_dropped] {
        w.push_str(r);
    }

    if let Some(info) = &opts.summary {
        write_summary(entries, info, &opts.budget_skipped, opts, binary_skipped, cuts, &mut w);
    }

    let rows_shown = skipped_rows.len() - cuts.skipped_rows_dropped;
    if skipped_total > 0 && opts.skipped_rows != Some(0) {
        let _ = writeln!(w, "---");
        let _ = writeln!(w, "§skipped");
        let _ = writeln!(
            w,
            "#context Matched but left out to fit the budget, largest first ({} of {})",
            rows_shown, skipped_total
        );
        if opts.no_stat {
            let _ = writeln!(w, "#schema path:str:path kind:{}", KIND_ENUM);
        } else {
            let _ = writeln!(w, "#schema path:str:path size:uint kind:{} tokens_est:uint", KIND_ENUM);
        }
        let _ = writeln!(w, "#sort tokens_est desc");
        if skipped_rows.len() < skipped_total {
            let _ = writeln!(w, "#limit {}", skipped_rows.len());
        }
        if cuts.skipped_rows_dropped > 0 {
            let _ = writeln!(
                w,
                "#truncated reason=output_budget dropped={} budget={} {}",
                cuts.skipped_rows_dropped, budget, RECOVER
            );
        }
        for r in &skipped_rows[..rows_shown] {
            w.push_str(r);
        }
    }

    if let Some(blocks) = blocks {
        let _ = writeln!(w, "---");
        let _ = writeln!(w, "§preview");
        if binary_skipped > 0 {
            let _ = writeln!(w, "#context {} binary file(s) not previewed", binary_skipped);
        }
        if cuts.preview_lines.is_some() || cuts.preview_blocks_dropped > 0 {
            let _ = write!(w, "#truncated reason=output_budget");
            if let Some(n) = cuts.preview_lines {
                let _ = write!(w, " lines_per_file={}", n);
            }
            let _ = writeln!(w, " dropped={} budget={} {}", cuts.preview_blocks_dropped, budget, RECOVER);
        }
        let shown = blocks.len() - cuts.preview_blocks_dropped.min(blocks.len());
        for b in &blocks[..shown] {
            let _ = b.render(cuts.preview_lines.unwrap_or(usize::MAX), &mut w);
        }
    }
    w
}

fn write_summary(
    entries: &[Entry],
    budget: &BudgetInfo,
    skipped: &[Entry],
    opts: &SifOptions,
    binary_skipped: usize,
    cuts: Cuts,
    w: &mut String,
) {
    let total_files = entries.iter().filter(|e| !e.is_dir).count();
    let total_dirs = entries.iter().filter(|e| e.is_dir).count();
    let total_bytes: u64 = entries.iter().map(|e| e.size).sum();
    let total_tokens: u64 = entries.iter().map(|e| e.tokens_est).sum();

    let mut kind_counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for entry in entries.iter().filter(|e| !e.is_dir) {
        *kind_counts.entry(entry.kind.as_str()).or_insert(0) += 1;
    }

    let _ = writeln!(w, "---");
    let _ = writeln!(w, "§summary");
    let _ = writeln!(w, "#schema key:str:id value:str note:str?");
    let _ = writeln!(w, "total_files\t{}", total_files);
    let _ = writeln!(w, "total_dirs\t{}", total_dirs);
    if !opts.no_stat {
        let _ = writeln!(w, "total_bytes\t{}", total_bytes);
        let _ = writeln!(w, "total_tokens_est\t{}", total_tokens);
    }
    if let Some(b) = budget.token_budget {
        let _ = writeln!(w, "token_budget\t{}", b);
        let _ = writeln!(w, "token_budget_remaining\t{}", b.saturating_sub(total_tokens));
    }
    if let Some(b) = budget.byte_budget {
        let _ = writeln!(w, "byte_budget\t{}", b);
        let _ = writeln!(w, "byte_budget_remaining\t{}", b.saturating_sub(total_bytes));
    }
    if let Some(p) = opts.order.as_ref().filter(|p| !matches!(p, Prefer::Path)) {
        let _ = writeln!(w, "prefer\t{}", p.as_str());
    }
    let has_budget = budget.token_budget.is_some() || budget.byte_budget.is_some();
    if has_budget && budget.budget_mode == BudgetMode::Fit {
        let _ = writeln!(w, "budget_mode\tfit");
        let _ = writeln!(w, "budget_skipped_files\t{}", skipped.len());
        let _ = writeln!(w, "budget_skipped_tokens_est\t{}", skipped.iter().map(|e| e.tokens_est).sum::<u64>());
        let _ = writeln!(w, "budget_skipped_bytes\t{}", skipped.iter().map(|e| e.size).sum::<u64>());
    }
    if let Some(reason) = budget.stopped_early {
        let _ = writeln!(w, "stopped_early\t{}\tmore matches may exist", reason.as_str());
    }
    if cuts.any() {
        let _ = writeln!(
            w,
            "output_truncated\t{}\toutput capped at --max-output-tokens {}; use --max-output-tokens unlimited",
            cuts.sections(),
            opts.max_output_tokens.unwrap_or(0)
        );
    }

    // What was left out, with how to get it back. Zero counts are omitted.
    let p = &budget.pruned;
    for (key, count, note) in [
        ("pruned_hidden", p.hidden, "dot-names skipped by wildcards; use -a to include"),
        ("pruned_gitignore", p.gitignore, "matched a gitignore rule; drop -g to include"),
        ("pruned_exclude", p.exclude, "matched an -e pattern"),
        ("pruned_nested_repos", p.nested_repos, "nested repositories not entered; drop --skip-nested-repos"),
        ("pruned_symlink_dirs", p.symlink_dirs, "symlinked directories not entered; use -L to follow"),
        ("filtered_kind", budget.filtered_kind, "matches of other kinds, dropped by -k"),
        ("filtered_git_changed", budget.filtered_git_changed, "matches unchanged since the -G ref"),
        ("unreadable_dirs", budget.unreadable_dirs, "directories that could not be read"),
        ("preview_skipped_binary", binary_skipped, "binary files are not previewed"),
    ] {
        if count > 0 {
            let _ = writeln!(w, "{}\t{}\t{}", key, count, note);
        }
    }
    if budget.wall_time_ms > 0 {
        let _ = writeln!(w, "wall_time_ms\t{}", budget.wall_time_ms);
    }

    let mut kinds: Vec<_> = kind_counts.into_iter().collect();
    kinds.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    for (kind, count) in kinds {
        let _ = writeln!(w, "kind_{}\t{}", kind, count);
    }
}

/// Format entries as plain paths (one per line), for non-SIF consumers.
pub fn to_paths(entries: &[Entry]) -> String {
    let mut buf = String::with_capacity(entries.len() * 40);
    for entry in entries {
        writeln!(buf, "{}", entry.path.display()).unwrap();
    }
    buf
}

/// ISO 8601 UTC, `YYYY-MM-DDTHH:MM:SSZ` (SIF datetime).
pub(crate) fn format_datetime(t: std::time::SystemTime) -> String {
    let secs = match t.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    };
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil-from-days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year,
        month,
        day,
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

fn format_float(f: f64) -> String {
    if f.fract() == 0.0 && f.abs() < 1e15 { format!("{}", f as i64) } else { format!("{}", f) }
}

// ── Value quoting (SIF Core §10–11) ──────────────────────────────────

/// Render a string field value, quoting it when the unquoted form would
/// be ambiguous: SIF unquoted strings cannot contain tabs, commas,
/// brackets, braces or quotes, cannot start or end with a space, and a
/// record must not look like a directive, section break, or null.
pub(crate) fn sif_str(s: &str) -> String {
    let safe = |c: char| {
        !c.is_control() && !matches!(c, ',' | '[' | ']' | '{' | '}' | '"' | '\\')
    };
    let needs_quotes = s.is_empty()
        || s == "_"
        || s == "---"
        || s.starts_with('#')
        || s.starts_with('§')
        || s.starts_with(' ')
        || s.ends_with(' ')
        || !s.chars().all(safe);
    if needs_quotes { quote(s) } else { s.to_string() }
}

/// Render a directive attribute value (`key=value`), quoting it unless it
/// is plain visible ASCII with no `=`.
pub(crate) fn sif_attr(s: &str) -> String {
    let plain = !s.is_empty()
        && s.bytes().all(|b| (0x21..=0x7e).contains(&b) && b != b'=' && b != b'"' && b != b'\\');
    if plain { s.to_string() } else { quote(s) }
}

fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\x{:02x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::{Entry, FileKind};
    use std::path::PathBuf;

    fn sample_entries() -> Vec<Entry> {
        vec![
            Entry {
                path: PathBuf::from("src/main.rs"),
                size: 1024,
                tokens_est: 293,
                is_dir: false,
                is_symlink: false,
                modified: None,
                kind: FileKind::Source,
            },
            Entry {
                path: PathBuf::from("Cargo.toml"),
                size: 256,
                tokens_est: 74,
                is_dir: false,
                is_symlink: false,
                modified: None,
                kind: FileKind::Config,
            },
        ]
    }

    #[test]
    fn sif_output_has_header() {
        let sif = to_sif(&sample_entries());
        assert!(sif.starts_with("#!sif v1\n"));
        assert!(sif.contains("#schema"));
    }

    #[test]
    fn schema_fields_are_space_separated() {
        let sif = to_sif(&sample_entries());
        let schema = sif.lines().find(|l| l.starts_with("#schema")).unwrap();
        assert!(!schema.contains('\t'));
        assert!(schema.contains(" kind:enum(source,"));
    }

    #[test]
    fn sif_output_has_records() {
        let sif = to_sif(&sample_entries());
        assert!(sif.contains("src/main.rs\t1024\tsource\t293\tfalse"));
        assert!(sif.contains("Cargo.toml\t256\tconfig\t74\tfalse"));
    }

    #[test]
    fn no_stat_schema_is_explicit() {
        let sif = to_sif_with(&sample_entries(), &SifOptions { no_stat: true, ..SifOptions::default() });
        assert!(sif.contains("#schema path:str:path kind:"));
        assert!(sif.contains("src/main.rs\tsource\tfalse"));
    }

    #[test]
    fn empty_files_keep_full_schema() {
        let mut entries = sample_entries();
        for e in &mut entries {
            e.size = 0;
            e.tokens_est = 0;
        }
        assert!(to_sif(&entries).contains("tokens_est:uint"));
    }

    #[test]
    fn sif_with_summary() {
        let sif = to_sif_with_summary(&sample_entries());
        assert!(sif.contains("§summary"));
        assert!(sif.contains("total_files\t2"));
        assert!(sif.contains("total_bytes\t1280"));
    }

    #[test]
    fn plain_paths() {
        let out = to_paths(&sample_entries());
        assert_eq!(out, "src/main.rs\nCargo.toml\n");
    }

    #[test]
    fn datetimes() {
        use std::time::{Duration, UNIX_EPOCH};
        assert_eq!(format_datetime(UNIX_EPOCH), "1970-01-01T00:00:00Z");
        assert_eq!(
            format_datetime(UNIX_EPOCH + Duration::from_secs(1_790_000_000)),
            "2026-09-21T14:13:20Z"
        );
        assert_eq!(
            format_datetime(UNIX_EPOCH + Duration::from_secs(951_782_400)),
            "2000-02-29T00:00:00Z"
        );
    }

    #[test]
    fn str_quoting() {
        assert_eq!(sif_str("src/main.rs"), "src/main.rs");
        assert_eq!(sif_str("with space.rs"), "with space.rs");
        assert_eq!(sif_str("a,b.rs"), "\"a,b.rs\"");
        assert_eq!(sif_str("{x}.rs"), "\"{x}.rs\"");
        assert_eq!(sif_str("tab\there"), "\"tab\\there\"");
        assert_eq!(sif_str("#notes"), "\"#notes\"");
        assert_eq!(sif_str("_"), "\"_\"");
        assert_eq!(sif_str(" lead"), "\" lead\"");
        assert_eq!(sif_str("日本.rs"), "日本.rs");
    }

    #[test]
    fn attr_quoting() {
        assert_eq!(sif_attr("src/main.rs"), "src/main.rs");
        assert_eq!(sif_attr("my file.rs"), "\"my file.rs\"");
        assert_eq!(sif_attr("a=b"), "\"a=b\"");
        assert_eq!(sif_attr("日本.rs"), "\"日本.rs\"");
    }
}
