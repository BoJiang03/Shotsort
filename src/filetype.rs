//! Extension-based classification and the camera-managed directory blocklist.

use crate::cli::TypeSel;
use crate::types::FileKind;

/// Camera-managed directories that must never be scanned, written into, or
/// cleaned. Compared case-insensitively against directory names.
pub const ADMIN_DIRS: &[&str] = &["PRIVATE", "MP_ROOT", "M4ROOT", "AVF_INFO", "MISC", "SONY"];

/// Returns true if a directory name is a camera-managed (forbidden) dir.
pub fn is_admin_dir(name: &str) -> bool {
    ADMIN_DIRS.iter().any(|d| name.eq_ignore_ascii_case(d))
}

/// Camera-internal subdirectories that never hold the primary, full-resolution
/// video masters — Sony proxy / thumbnail / audio / metadata trees and the
/// AVCHD info store. In `--mode video` we deliberately descend into the managed
/// *containers* (`PRIVATE`/`M4ROOT`/`MP_ROOT`/`AVCHD`/`BDMV`/`STREAM`/`CLIP`) to
/// reach the masters, but still skip these so proxies and thumbnails are not
/// copied out alongside the real clips.
pub const VIDEO_AUX_DIRS: &[&str] = &[
    "SUB", "THMBNL", "WAV", "GENERAL", "DATABASE", "AVF_INFO", "MISC", "SONY",
];

/// True if a directory name should be skipped while scanning in `--mode video`.
pub fn is_video_aux_dir(name: &str) -> bool {
    VIDEO_AUX_DIRS.iter().any(|d| name.eq_ignore_ascii_case(d))
}

/// Classify a lowercase extension (no dot) into a [`FileKind`].
pub fn classify(ext_lower: &str) -> Option<FileKind> {
    Some(match ext_lower {
        "arw" | "cr2" | "cr3" | "nef" | "raf" | "orf" | "rw2" | "dng" | "pef" | "srw" | "nrw" => {
            FileKind::Raw
        }
        "jpg" | "jpeg" | "heic" | "heif" | "tif" | "tiff" | "png" => FileKind::Jpeg,
        "mp4" | "mov" | "m4v" | "avi" | "mts" | "m2ts" => FileKind::Video,
        "xmp" => FileKind::Sidecar,
        _ => return None,
    })
}

/// Whether a media kind is selected, given the resolved type filter.
///
/// `kinds == None` means "all kinds". Sidecars are decided by their primary,
/// so they are not gated here.
pub fn kind_selected(kind: FileKind, kinds: &Option<Vec<TypeSel>>) -> bool {
    if kind == FileKind::Sidecar {
        return true;
    }
    match kinds {
        None => true,
        Some(sel) => sel.iter().any(|t| {
            matches!(
                (t, kind),
                (TypeSel::Raw, FileKind::Raw)
                    | (TypeSel::Jpeg, FileKind::Jpeg)
                    | (TypeSel::Video, FileKind::Video)
            )
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_mode_descends_containers_but_skips_aux() {
        // Containers we must enter to reach the masters.
        for c in ["PRIVATE", "M4ROOT", "MP_ROOT", "CLIP", "STREAM", "BDMV"] {
            assert!(
                !is_video_aux_dir(c),
                "{c} should be descended in video mode"
            );
        }
        // Proxy / thumbnail / metadata trees we must skip.
        for a in ["SUB", "THMBNL", "WAV", "GENERAL", "DATABASE", "AVF_INFO"] {
            assert!(is_video_aux_dir(a), "{a} should be skipped in video mode");
        }
        // Case-insensitive (cards are FAT/exFAT).
        assert!(is_video_aux_dir("sub"));
    }
}
