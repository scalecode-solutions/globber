// SIF output formatting.
//
// Emits glob results as SIF documents — the native output format for
// the SIF ecosystem. Zero dependencies: SIF is just formatted text.
//
// Output schema:
//   #!sif v1
//   #schema path:str:path size:uint kind:str:311 tokens_est:uint is_dir:bool
//
// This lets any SIF-aware tool (sif-parser, SIL pipelines, STP tools,
// sif-scratch slots) consume glob results directly.

use std::fmt::Write;

use crate::entry::Entry;

/// Format a list of entries as a SIF document string.
///
/// Output is a complete SIF v1 document with schema and records.
/// Each entry becomes one tab-separated record.
pub fn to_sif(entries: &[Entry]) -> String {
    let mut buf = String::with_capacity(entries.len() * 80);
    write_sif(entries, &mut buf).unwrap();
    buf
}

/// Write entries as SIF to any fmt::Write sink.
pub fn write_sif(entries: &[Entry], w: &mut dyn Write) -> std::fmt::Result {
    // Detect no-stat mode: if all sizes are 0 and entries exist, omit size/tokens columns.
    let has_stat = entries.is_empty() || entries.iter().any(|e| e.size > 0 || e.is_dir);

    writeln!(w, "#!sif v1")?;
    writeln!(w, "#context File listing produced by globber")?;

    if has_stat {
        writeln!(
            w,
            "#schema path:str:path\tsize:uint\tkind:str:311\ttokens_est:uint\tis_dir:bool"
        )?;
        for entry in entries {
            writeln!(
                w, "{}\t{}\t{}\t{}\t{}",
                entry.path.display(), entry.size, entry.kind.as_str(),
                entry.tokens_est, entry.is_dir,
            )?;
        }
    } else {
        writeln!(w, "#schema path:str:path\tkind:str:311\tis_dir:bool")?;
        for entry in entries {
            writeln!(
                w, "{}\t{}\t{}",
                entry.path.display(), entry.kind.as_str(), entry.is_dir,
            )?;
        }
    }

    Ok(())
}

/// Format entries as a SIF document with a summary section.
///
/// Includes a second section with aggregate statistics useful for
/// AI agents planning context acquisition.
/// Budget information for the summary section.
#[derive(Debug, Default)]
pub struct BudgetInfo {
    pub token_budget: u64,
    pub byte_budget: u64,
    pub wall_time_ms: u64,
}

pub fn to_sif_with_summary(entries: &[Entry]) -> String {
    to_sif_with_summary_and_budget(entries, &BudgetInfo::default())
}

/// Format entries as a SIF document with a summary section and budget info.
pub fn to_sif_with_summary_and_budget(entries: &[Entry], budget: &BudgetInfo) -> String {
    let mut buf = String::with_capacity(entries.len() * 80 + 512);
    write_sif(entries, &mut buf).unwrap();

    // Summary section.
    let total_files = entries.iter().filter(|e| !e.is_dir).count();
    let total_dirs = entries.iter().filter(|e| e.is_dir).count();
    let total_bytes: u64 = entries.iter().map(|e| e.size).sum();
    let total_tokens: u64 = entries.iter().map(|e| e.tokens_est).sum();

    // Count by kind.
    let mut kind_counts: std::collections::HashMap<&str, usize> =
        std::collections::HashMap::new();
    for entry in entries {
        if !entry.is_dir {
            *kind_counts.entry(entry.kind.as_str()).or_insert(0) += 1;
        }
    }

    writeln!(buf, "---").unwrap();
    writeln!(buf, "§summary").unwrap();
    writeln!(buf, "#schema key:str:id value:str").unwrap();
    writeln!(buf, "total_files\t{}", total_files).unwrap();
    writeln!(buf, "total_dirs\t{}", total_dirs).unwrap();
    writeln!(buf, "total_bytes\t{}", total_bytes).unwrap();
    writeln!(buf, "total_tokens_est\t{}", total_tokens).unwrap();

    if budget.token_budget > 0 {
        let remaining = budget.token_budget.saturating_sub(total_tokens);
        writeln!(buf, "token_budget\t{}", budget.token_budget).unwrap();
        writeln!(buf, "token_budget_remaining\t{}", remaining).unwrap();
    }
    if budget.byte_budget > 0 {
        let remaining = budget.byte_budget.saturating_sub(total_bytes);
        writeln!(buf, "byte_budget\t{}", budget.byte_budget).unwrap();
        writeln!(buf, "byte_budget_remaining\t{}", remaining).unwrap();
    }
    if budget.wall_time_ms > 0 {
        writeln!(buf, "wall_time_ms\t{}", budget.wall_time_ms).unwrap();
    }

    let mut kinds: Vec<_> = kind_counts.into_iter().collect();
    kinds.sort_by(|a, b| b.1.cmp(&a.1));
    for (kind, count) in kinds {
        writeln!(buf, "kind_{}\t{}", kind, count).unwrap();
    }

    buf
}

/// Append a §preview section with the first N lines of each non-binary file.
pub fn write_preview(entries: &[Entry], lines: usize, w: &mut dyn Write) -> std::fmt::Result {
    use crate::entry::FileKind;
    use std::io::BufRead;

    writeln!(w, "---")?;
    writeln!(w, "§preview")?;

    for entry in entries {
        if entry.is_dir {
            continue;
        }
        // Skip binary files — they won't produce useful text previews.
        if entry.kind == FileKind::Binary {
            continue;
        }

        let file = match std::fs::File::open(&entry.path) {
            Ok(f) => f,
            Err(_) => continue,
        };
        let reader = std::io::BufReader::new(file);
        let mut preview_lines: Vec<String> = Vec::with_capacity(lines);
        for line in reader.lines().take(lines) {
            match line {
                Ok(l) => preview_lines.push(l),
                Err(_) => break, // Binary content or encoding error — stop.
            }
        }

        if preview_lines.is_empty() {
            continue;
        }

        let path_display = entry.path.display();
        let actual_lines = preview_lines.len();
        writeln!(w, "#block code path={} lines=1-{}", path_display, actual_lines)?;
        for line in &preview_lines {
            writeln!(w, "{}", line)?;
        }
        writeln!(w, "#/block")?;
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
    fn sif_output_has_records() {
        let sif = to_sif(&sample_entries());
        assert!(sif.contains("src/main.rs\t1024\tsource\t293\tfalse"));
        assert!(sif.contains("Cargo.toml\t256\tconfig\t74\tfalse"));
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
}
