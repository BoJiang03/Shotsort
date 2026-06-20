//! Shared domain types.

use std::path::PathBuf;
use std::time::SystemTime;

use chrono::NaiveDateTime;

/// Broad media category derived from the file extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Raw,
    Jpeg,
    Video,
    /// `.xmp` sidecar — never processed on its own; follows its primary file.
    Sidecar,
}

impl FileKind {
    pub fn label(self) -> &'static str {
        match self {
            FileKind::Raw => "raw",
            FileKind::Jpeg => "jpeg",
            FileKind::Video => "video",
            FileKind::Sidecar => "sidecar",
        }
    }
}

impl std::fmt::Display for FileKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// A recognized media file discovered during the scan.
#[derive(Debug, Clone)]
pub struct MediaFile {
    /// Absolute source path.
    pub path: PathBuf,
    /// Absolute parent directory.
    pub parent: PathBuf,
    /// File name without the final extension.
    pub stem: String,
    /// Extension without the dot, original case (e.g. `ARW`).
    pub ext: String,
    pub kind: FileKind,
    pub size: u64,
    pub mtime: Option<SystemTime>,
}

/// Capture metadata extracted from a file (or absent).
#[derive(Debug, Clone, Default)]
pub struct CaptureInfo {
    pub datetime: Option<NaiveDateTime>,
    pub make: Option<String>,
    pub model: Option<String>,
}

/// Where a planned date ultimately came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateProvenance {
    Embedded,
    Mtime,
    None,
}

/// The decision made for one source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Atomic move (default).
    Move,
    /// Copy, keeping the source.
    Copy,
    /// Move/copy that overwrites an existing target.
    Overwrite,
    /// Skipped: name/content already present at target.
    SkipDuplicate,
    /// Skipped: target exists with different content (on-conflict=skip).
    SkipConflict,
    /// Skipped: no capture date (on-missing-date=skip).
    SkipNoDate,
}

impl Action {
    pub fn is_skip(self) -> bool {
        matches!(
            self,
            Action::SkipDuplicate | Action::SkipConflict | Action::SkipNoDate
        )
    }

    pub fn manifest_label(self) -> &'static str {
        match self {
            Action::Move => "moved",
            Action::Copy => "copied",
            Action::Overwrite => "overwritten",
            Action::SkipDuplicate => "skipped-duplicate",
            Action::SkipConflict => "skipped-conflict",
            Action::SkipNoDate => "skipped-nodate",
        }
    }
}

/// A fully-decided plan entry for one source file.
#[derive(Debug, Clone)]
pub struct PlanItem {
    pub src: PathBuf,
    /// Target path (only meaningful for move/copy/overwrite actions).
    pub dst: PathBuf,
    pub kind: FileKind,
    pub size: u64,
    pub date: Option<NaiveDateTime>,
    pub provenance: DateProvenance,
    pub action: Action,
    /// Destination folder relative to `dest`, for reporting.
    pub rel_folder: String,
    /// Human-readable note (reason for skip / conflict resolution).
    pub note: String,
}
