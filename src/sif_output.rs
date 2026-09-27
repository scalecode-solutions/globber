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
use crate::walker::{BudgetMode, StopReason};

/// The `kind` column type: every [`FileKind`](crate::FileKind) name.
const KIND_ENUM: &str = "enum(source,test,config,build,doc,data,generated,binary,unknown)";

/// Options for SIF emission.
#[derive(Debug, Clone, Default)]
pub struct SifOptions {
    /// Entries came from a `no_stat` walk: omit the size and token columns
    /// (they would be zeros and guesses) and the byte/token totals.
    pub no_stat: bool,
    /// Append a §summary section with these budget figures.
    pub summary: Option<BudgetInfo>,
    /// Matches left out by a `--fit` budget. When non-empty, a §skipped
    /// section lists the largest of them (see [`MAX_SKIPPED_LISTED`]).
    pub budget_skipped: Vec<Entry>,
}

/// How many budget-skipped entries the §skipped section lists.
pub const MAX_SKIPPED_LISTED: usize = 50;

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

/// Format entries as a SIF document with explicit options.
pub fn to_sif_with(entries: &[Entry], opts: &SifOptions) -> String {
    let mut buf = String::with_capacity(entries.len() * 80 + 512);
    write_sif_with(entries, opts, &mut buf).unwrap();
    buf
}

/// Write entries as SIF to any fmt::Write sink.
pub fn write_sif(entries: &[Entry], w: &mut dyn Write) -> std::fmt::Result {
    write_sif_with(entries, &SifOptions::default(), w)
}

/// Write entries as SIF with explicit options.
pub fn write_sif_with(entries: &[Entry], opts: &SifOptions, w: &mut dyn Write) -> std::fmt::Result {
    writeln!(w, "#!sif v1")?;
    writeln!(w, "#context File listing produced by globber")?;

    if opts.no_stat {
        writeln!(w, "#schema path:str:path kind:{} is_dir:bool", KIND_ENUM)?;
        for e in entries {
            writeln!(w, "{}\t{}\t{}", sif_str(&e.path.to_string_lossy()), e.kind, e.is_dir)?;
        }
    } else {
        writeln!(
            w,
            "#schema path:str:path size:uint kind:{} tokens_est:uint is_dir:bool",
            KIND_ENUM
        )?;
        for e in entries {
            writeln!(
                w,
                "{}\t{}\t{}\t{}\t{}",
                sif_str(&e.path.to_string_lossy()),
                e.size,
                e.kind,
                e.tokens_est,
                e.is_dir,
            )?;
        }
    }

    if let Some(budget) = &opts.summary {
        write_summary(entries, budget, &opts.budget_skipped, opts.no_stat, w)?;
    }
    if !opts.budget_skipped.is_empty() {
        write_skipped(&opts.budget_skipped, opts.no_stat, w)?;
    }
    Ok(())
}

/// The §skipped section: matches a --fit budget left out, largest first.
fn write_skipped(skipped: &[Entry], no_stat: bool, w: &mut dyn Write) -> std::fmt::Result {
    let mut by_size: Vec<&Entry> = skipped.iter().collect();
    by_size.sort_by(|a, b| b.tokens_est.cmp(&a.tokens_est).then(a.path.cmp(&b.path)));
    let shown = by_size.len().min(MAX_SKIPPED_LISTED);

    writeln!(w, "---")?;
    writeln!(w, "§skipped")?;
    writeln!(
        w,
        "#context Matched but left out to fit the budget, largest first ({} of {})",
        shown,
        by_size.len()
    )?;
    if no_stat {
        writeln!(w, "#schema path:str:path kind:{}", KIND_ENUM)?;
        for e in &by_size[..shown] {
            writeln!(w, "{}\t{}", sif_str(&e.path.to_string_lossy()), e.kind)?;
        }
    } else {
        writeln!(w, "#schema path:str:path size:uint kind:{} tokens_est:uint", KIND_ENUM)?;
        for e in &by_size[..shown] {
            writeln!(
                w,
                "{}\t{}\t{}\t{}",
                sif_str(&e.path.to_string_lossy()),
                e.size,
                e.kind,
                e.tokens_est
            )?;
        }
    }
    Ok(())
}

fn write_summary(
    entries: &[Entry],
    budget: &BudgetInfo,
    skipped: &[Entry],
    no_stat: bool,
    w: &mut dyn Write,
) -> std::fmt::Result {
    let total_files = entries.iter().filter(|e| !e.is_dir).count();
    let total_dirs = entries.iter().filter(|e| e.is_dir).count();
    let total_bytes: u64 = entries.iter().map(|e| e.size).sum();
    let total_tokens: u64 = entries.iter().map(|e| e.tokens_est).sum();

    let mut kind_counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for entry in entries.iter().filter(|e| !e.is_dir) {
        *kind_counts.entry(entry.kind.as_str()).or_insert(0) += 1;
    }

    writeln!(w, "---")?;
    writeln!(w, "§summary")?;
    writeln!(w, "#schema key:str:id value:str")?;
    writeln!(w, "total_files\t{}", total_files)?;
    writeln!(w, "total_dirs\t{}", total_dirs)?;
    if !no_stat {
        writeln!(w, "total_bytes\t{}", total_bytes)?;
        writeln!(w, "total_tokens_est\t{}", total_tokens)?;
    }
    if let Some(b) = budget.token_budget {
        writeln!(w, "token_budget\t{}", b)?;
        writeln!(w, "token_budget_remaining\t{}", b.saturating_sub(total_tokens))?;
    }
    if let Some(b) = budget.byte_budget {
        writeln!(w, "byte_budget\t{}", b)?;
        writeln!(w, "byte_budget_remaining\t{}", b.saturating_sub(total_bytes))?;
    }
    let has_budget = budget.token_budget.is_some() || budget.byte_budget.is_some();
    if has_budget && budget.budget_mode == BudgetMode::Fit {
        writeln!(w, "budget_mode\tfit")?;
        writeln!(w, "budget_skipped_files\t{}", skipped.len())?;
        writeln!(w, "budget_skipped_tokens_est\t{}", skipped.iter().map(|e| e.tokens_est).sum::<u64>())?;
        writeln!(w, "budget_skipped_bytes\t{}", skipped.iter().map(|e| e.size).sum::<u64>())?;
    }
    if let Some(reason) = budget.stopped_early {
        writeln!(w, "stopped_early\t{}", reason.as_str())?;
    }
    if budget.unreadable_dirs > 0 {
        writeln!(w, "unreadable_dirs\t{}", budget.unreadable_dirs)?;
    }
    if budget.wall_time_ms > 0 {
        writeln!(w, "wall_time_ms\t{}", budget.wall_time_ms)?;
    }

    let mut kinds: Vec<_> = kind_counts.into_iter().collect();
    kinds.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    for (kind, count) in kinds {
        writeln!(w, "kind_{}\t{}", kind, count)?;
    }
    Ok(())
}

/// Format entries as plain paths (one per line), for non-SIF consumers.
pub fn to_paths(entries: &[Entry]) -> String {
    let mut buf = String::with_capacity(entries.len() * 40);
    for entry in entries {
        writeln!(buf, "{}", entry.path.display()).unwrap();
    }
    buf
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
