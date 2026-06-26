//! Append-only operation journal (JSON Lines) for resume + undo.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// One committed file operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalEntry {
    pub op: String, // "move" | "copy" | "overwrite" | "undo-move" | "undo-copy"
    pub src: String,
    pub dst: String,
    pub ts: String,
    pub bytes: u64,
}

/// Append-only journal writer. Each entry is flushed immediately so a crash
/// leaves a consistent record of exactly what completed.
pub struct Journal {
    writer: BufWriter<File>,
    path: PathBuf,
}

impl Journal {
    pub fn open_append(path: &Path) -> Result<Journal> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating journal dir {}", parent.display()))?;
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("opening journal {}", path.display()))?;
        Ok(Journal {
            writer: BufWriter::new(file),
            path: path.to_path_buf(),
        })
    }

    pub fn append(&mut self, entry: &JournalEntry) -> Result<()> {
        let line = serde_json::to_string(entry)?;
        self.writer.write_all(line.as_bytes())?;
        self.writer.write_all(b"\n")?;
        self.writer.flush()?;
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Read every entry from a journal file, skipping unparseable lines.
pub fn read_all(path: &Path) -> Result<Vec<JournalEntry>> {
    let file = File::open(path).with_context(|| format!("opening journal {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut entries = Vec::new();
    for (i, line) in reader.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<JournalEntry>(&line) {
            Ok(e) => entries.push(e),
            Err(err) => eprintln!("warn: journal line {} unparseable: {err}", i + 1),
        }
    }
    Ok(entries)
}

/// Set of source paths already recorded as moved (for resume skipping).
pub fn completed_sources(path: &Path) -> Vec<String> {
    match read_all(path) {
        Ok(entries) => entries.into_iter().map(|e| e.src).collect(),
        Err(_) => Vec::new(),
    }
}
