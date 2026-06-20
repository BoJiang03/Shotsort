//! Plan preview, per-file manifest, and final summary.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Serialize;

use crate::config::RunConfig;
use crate::types::{Action, DateProvenance, PlanItem};
use crate::util::human_size;

/// Aggregate plan statistics for the preview / summary.
#[derive(Default)]
pub struct PlanStats {
    pub to_move: usize,
    pub move_bytes: u64,
    pub skip_duplicate: usize,
    pub skip_conflict: usize,
    pub skip_no_date: usize,
    pub conflicts_renamed: usize,
    pub overwrites: usize,
    pub no_date: usize,
    pub min_date: Option<chrono::NaiveDate>,
    pub max_date: Option<chrono::NaiveDate>,
    /// folder -> file count, for the per-folder breakdown.
    pub by_folder: BTreeMap<String, usize>,
}

pub fn summarize(items: &[PlanItem]) -> PlanStats {
    let mut s = PlanStats::default();
    for item in items {
        match item.action {
            Action::Move | Action::Copy => {
                s.to_move += 1;
                s.move_bytes += item.size;
                *s.by_folder.entry(item.rel_folder.clone()).or_default() += 1;
                if item.note.contains("renamed") {
                    s.conflicts_renamed += 1;
                }
            }
            Action::Overwrite => {
                s.to_move += 1;
                s.move_bytes += item.size;
                s.overwrites += 1;
                *s.by_folder.entry(item.rel_folder.clone()).or_default() += 1;
            }
            Action::SkipDuplicate => s.skip_duplicate += 1,
            Action::SkipConflict => s.skip_conflict += 1,
            Action::SkipNoDate => s.skip_no_date += 1,
        }
        if item.rel_folder == "NoDate" && !item.action.is_skip() {
            s.no_date += 1;
        }
        if let Some(d) = item.date.map(|dt| dt.date()) {
            s.min_date = Some(s.min_date.map_or(d, |m| m.min(d)));
            s.max_date = Some(s.max_date.map_or(d, |m| m.max(d)));
        }
    }
    s
}

/// Print the plan preview / confirmation summary.
pub fn print_preview(cfg: &RunConfig, stats: &PlanStats, dry_run: bool) {
    let verb = if cfg.copy { "copy" } else { "move" };
    let header = if dry_run {
        "Plan preview (dry-run, nothing moved)"
    } else {
        "Plan summary"
    };
    println!("\n{header}");
    println!(
        "  source: {}    dest: {} (same card → atomic {verb})",
        cfg.source.display(),
        cfg.dest.display()
    );

    let span = match (stats.min_date, stats.max_date) {
        (Some(a), Some(b)) => format!(", dates {a} ~ {b}"),
        _ => String::new(),
    };
    println!(
        "  to {verb}: {} ({}){span}",
        stats.to_move,
        human_size(stats.move_bytes)
    );

    for (folder, count) in &stats.by_folder {
        let label = if folder.is_empty() {
            "<dest root>"
        } else {
            folder
        };
        println!("  → {}/{:<28} {count}", cfg.dest.display(), label);
    }

    let mut flags = Vec::new();
    if stats.skip_duplicate > 0 {
        flags.push(format!("skipped(duplicate): {}", stats.skip_duplicate));
    }
    if stats.conflicts_renamed > 0 {
        flags.push(format!("conflicts(renamed): {}", stats.conflicts_renamed));
    }
    if stats.overwrites > 0 {
        flags.push(format!("overwrites: {}", stats.overwrites));
    }
    if stats.skip_conflict > 0 {
        flags.push(format!("skipped(conflict): {}", stats.skip_conflict));
    }
    if stats.no_date > 0 {
        flags.push(format!("no-date → NoDate/: {}", stats.no_date));
    }
    if stats.skip_no_date > 0 {
        flags.push(format!("skipped(no-date): {}", stats.skip_no_date));
    }
    if !flags.is_empty() {
        println!("  {}", flags.join("   "));
    }

    if !cfg.copy {
        println!(
            "  ⚠ this is a MOVE; afterwards these files leave DCIM and camera playback won't see them."
        );
    }
}

/// Final run summary.
pub fn print_final(stats: &FinalStats, elapsed: std::time::Duration) {
    println!("\nDone.");
    println!(
        "  {} {} ({}), skipped {}, no-date {}, errors {}, {:.1}s",
        stats.action_verb,
        stats.moved,
        human_size(stats.moved_bytes),
        stats.skipped,
        stats.no_date,
        stats.errors,
        elapsed.as_secs_f64()
    );
}

#[derive(Default)]
pub struct FinalStats {
    pub action_verb: &'static str,
    pub moved: usize,
    pub moved_bytes: u64,
    pub skipped: usize,
    pub no_date: usize,
    pub errors: usize,
}

#[derive(Serialize)]
struct ManifestRow<'a> {
    src: &'a str,
    dst: &'a str,
    kind: &'static str,
    date: Option<String>,
    date_source: &'a str,
    action: &'a str,
    note: &'a str,
}

fn provenance_label(p: DateProvenance) -> &'static str {
    match p {
        DateProvenance::Embedded => "embedded",
        DateProvenance::Mtime => "mtime",
        DateProvenance::None => "none",
    }
}

/// Write a per-file manifest as JSON (`.json`) or CSV (any other extension).
pub fn write_manifest(path: &Path, items: &[PlanItem]) -> Result<()> {
    let is_json = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("json"))
        .unwrap_or(false);

    let mut file =
        File::create(path).with_context(|| format!("creating manifest {}", path.display()))?;

    if is_json {
        let rows: Vec<ManifestRow> = items.iter().map(to_row).collect();
        let json = serde_json::to_string_pretty(&rows)?;
        file.write_all(json.as_bytes())?;
        file.write_all(b"\n")?;
    } else {
        writeln!(file, "src,dst,kind,date,date_source,action,note")?;
        for item in items {
            let row = to_row(item);
            writeln!(
                file,
                "{},{},{},{},{},{},{}",
                csv_escape(row.src),
                csv_escape(row.dst),
                row.kind,
                csv_escape(&row.date.unwrap_or_default()),
                row.date_source,
                row.action,
                csv_escape(row.note),
            )?;
        }
    }
    Ok(())
}

fn to_row(item: &PlanItem) -> ManifestRow<'_> {
    ManifestRow {
        src: item.src.to_str().unwrap_or(""),
        dst: item.dst.to_str().unwrap_or(""),
        kind: item.kind.label(),
        date: item.date.map(|d| d.format("%Y-%m-%d %H:%M:%S").to_string()),
        date_source: provenance_label(item.provenance),
        action: item.action.manifest_label(),
        note: &item.note,
    }
}

fn csv_escape(s: &str) -> String {
    if s.contains([',', '"', '\n']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}
