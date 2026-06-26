//! Recursive source scan with destination-subtree and managed-dir exclusion.

use std::path::Path;

use anyhow::{Context, Result};
use walkdir::{DirEntry, WalkDir};

use crate::cli::ModeArg;
use crate::filetype::{classify, is_admin_dir, is_video_aux_dir};
use crate::guard::{is_within, normalize};
use crate::types::{FileKind, MediaFile};

/// Walk `source`, collecting recognized media + sidecar files while excluding
/// the `dest` subtree.
///
/// In [`ModeArg::Photo`] (default) every camera-managed dir is skipped and all
/// recognized kinds are collected. In [`ModeArg::Video`] we descend *into* the
/// managed video containers (to reach `M4ROOT/CLIP`, AVCHD `STREAM`, …) but skip
/// the proxy/thumbnail/metadata aux dirs, and collect video files only.
pub fn scan(source: &Path, dest: &Path, mode: ModeArg) -> Result<Vec<MediaFile>> {
    let source = normalize(source);
    let dest = normalize(dest);

    let mut files = Vec::new();

    let walker = WalkDir::new(&source).follow_links(false).into_iter();
    let it = walker.filter_entry(|e| {
        if e.file_type().is_dir() {
            // Always allow the root itself.
            let p = normalize(e.path());
            if p == source {
                return true;
            }
            if is_within(&p, &dest) {
                return false;
            }
            if let Some(name) = e.file_name().to_str() {
                // Hidden/system dirs (.Trashes, .Spotlight-V100, …) never hold
                // camera media and may hold trashed clips — never descend.
                if name.starts_with('.') {
                    return false;
                }
                let skip = match mode {
                    ModeArg::Photo => is_admin_dir(name),
                    ModeArg::Video => is_video_aux_dir(name),
                };
                if skip {
                    return false;
                }
            }
        }
        true
    });

    for entry in it {
        let entry: DirEntry = match entry {
            Ok(e) => e,
            Err(err) => {
                // Surface unreadable entries but keep scanning.
                eprintln!("warn: scan error: {err}");
                continue;
            }
        };

        if !entry.file_type().is_file() {
            continue;
        }

        let path = entry.path();
        // Skip dotfiles: macOS AppleDouble companions (`._IMG.JPG`) on exFAT and
        // junk like `.DS_Store` would otherwise be misclassified as real media.
        if let Some(name) = path.file_name().and_then(|n| n.to_str())
            && name.starts_with('.')
        {
            continue;
        }
        let ext = match path.extension().and_then(|e| e.to_str()) {
            Some(e) => e.to_string(),
            None => continue,
        };
        let kind = match classify(&ext.to_ascii_lowercase()) {
            Some(k) => k,
            None => continue,
        };

        // Video mode organizes clips only — ignore stills/thumbnails found in
        // the managed trees (e.g. THMBNL JPEGs slip past the dir filter).
        if mode == ModeArg::Video && kind != FileKind::Video {
            continue;
        }

        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        let parent = path
            .parent()
            .map(normalize)
            .unwrap_or_else(|| source.clone());

        let meta = entry
            .metadata()
            .with_context(|| format!("stat {}", path.display()))?;

        files.push(MediaFile {
            path: normalize(path),
            parent,
            stem,
            ext,
            kind,
            size: meta.len(),
            mtime: meta.modified().ok(),
        });
    }

    Ok(files)
}
