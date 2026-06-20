//! Recursive source scan with destination-subtree and managed-dir exclusion.

use std::path::Path;

use anyhow::{Context, Result};
use walkdir::{DirEntry, WalkDir};

use crate::filetype::{classify, is_admin_dir};
use crate::guard::{is_within, normalize};
use crate::types::MediaFile;

/// Walk `source`, collecting all recognized media + sidecar files while
/// excluding the `dest` subtree and any camera-managed directory.
pub fn scan(source: &Path, dest: &Path) -> Result<Vec<MediaFile>> {
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
            if let Some(name) = e.file_name().to_str()
                && is_admin_dir(name)
            {
                return false;
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
        let ext = match path.extension().and_then(|e| e.to_str()) {
            Some(e) => e.to_string(),
            None => continue,
        };
        let kind = match classify(&ext.to_ascii_lowercase()) {
            Some(k) => k,
            None => continue,
        };

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
