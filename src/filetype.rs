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
