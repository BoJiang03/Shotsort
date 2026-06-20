//! `undo` — roll a previous organize run back to the source layout.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;

use crate::journal::{self, Journal, JournalEntry};

pub struct UndoSummary {
    pub restored: usize,
    pub skipped: usize,
    pub errors: usize,
    pub reverse_journal: PathBuf,
}

/// Reverse every committed move (`dst -> src`) in reverse order. A new journal
/// records the reverse operations for traceability.
pub fn run(journal_path: &Path, yes: bool, quiet: bool) -> Result<UndoSummary> {
    let entries = journal::read_all(journal_path)
        .with_context(|| format!("reading journal {}", journal_path.display()))?;

    let movable: Vec<&JournalEntry> = entries.iter().filter(|e| e.op != "undo-move").collect();

    if movable.is_empty() {
        println!(
            "Nothing to undo: no move entries in {}",
            journal_path.display()
        );
        return Ok(UndoSummary {
            restored: 0,
            skipped: 0,
            errors: 0,
            reverse_journal: PathBuf::new(),
        });
    }

    println!(
        "About to roll back {} file(s) recorded in {}",
        movable.len(),
        journal_path.display()
    );
    if !yes && !confirm("Proceed with undo? [y/N] ")? {
        anyhow::bail!("undo cancelled");
    }

    let reverse_path = reverse_journal_path(journal_path);
    let mut reverse = Journal::open_append(&reverse_path)?;

    let mut restored = 0usize;
    let mut skipped = 0usize;
    let mut errors = 0usize;

    // Reverse order so later moves are undone first.
    for entry in movable.iter().rev() {
        let dst = Path::new(&entry.dst); // current location
        let src = Path::new(&entry.src); // original location

        if !dst.exists() {
            skipped += 1;
            if !quiet {
                eprintln!("skip: target gone, cannot restore {}", entry.dst);
            }
            continue;
        }
        if src.exists() {
            skipped += 1;
            if !quiet {
                eprintln!("skip: source already occupied {}", entry.src);
            }
            continue;
        }
        if let Some(parent) = src.parent()
            && let Err(e) = fs::create_dir_all(parent)
        {
            errors += 1;
            eprintln!("error: cannot recreate {}: {e}", parent.display());
            continue;
        }
        match fs::rename(dst, src) {
            Ok(()) => {
                restored += 1;
                let _ = reverse.append(&JournalEntry {
                    op: "undo-move".to_string(),
                    src: entry.dst.clone(),
                    dst: entry.src.clone(),
                    ts: Utc::now().to_rfc3339(),
                    bytes: entry.bytes,
                });
            }
            Err(e) => {
                errors += 1;
                eprintln!("error: restoring {} -> {}: {e}", entry.dst, entry.src);
            }
        }
    }

    Ok(UndoSummary {
        restored,
        skipped,
        errors,
        reverse_journal: reverse_path,
    })
}

fn reverse_journal_path(journal_path: &Path) -> PathBuf {
    let ts = Utc::now().format("%Y%m%d-%H%M%S");
    let name = format!(
        "{}.undo-{ts}.jsonl",
        journal_path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "journal".to_string())
    );
    journal_path.with_file_name(name)
}

fn confirm(prompt: &str) -> Result<bool> {
    use std::io::Write;
    print!("{prompt}");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(matches!(
        line.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}
