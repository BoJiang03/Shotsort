//! Path safety: destination validation, forbidden-zone checks, anti-recursion.

use std::path::{Component, Path, PathBuf};

use anyhow::{Result, bail};

use crate::filetype::is_admin_dir;

/// Lexically absolutize and normalize a path (resolves `.`/`..` without
/// touching the filesystem, so it works for not-yet-created destinations).
pub fn normalize(path: &Path) -> PathBuf {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("/"))
            .join(path)
    };

    let mut out = PathBuf::new();
    for comp in abs.components() {
        match comp {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// True if `path` is at or below `prefix`.
pub fn is_within(path: &Path, prefix: &Path) -> bool {
    path == prefix || path.starts_with(prefix)
}

fn normal_name(c: Component<'_>) -> Option<&str> {
    match c {
        Component::Normal(os) => os.to_str(),
        _ => None,
    }
}

/// The shared leading path of `a` and `b` (case-insensitive), i.e. the card
/// root when one is `<card>/DCIM` and the other `<card>/Organized`.
fn common_ancestor(a: &Path, b: &Path) -> PathBuf {
    let mut out = PathBuf::from("/");
    let mut ai = a.components();
    let mut bi = b.components();
    while let (Some(x), Some(y)) = (ai.next(), bi.next()) {
        let same = match (normal_name(x), normal_name(y)) {
            (Some(nx), Some(ny)) => nx.eq_ignore_ascii_case(ny),
            _ => x == y,
        };
        if !same {
            break;
        }
        out.push(x.as_os_str());
    }
    out
}

/// True if any component of `path` *below* `base` is `DCIM` or a managed dir.
/// Components at or above `base` (e.g. a volume literally named `SONY`) are
/// ignored so the card's own name never trips the guard.
fn has_forbidden_below(base: &Path, path: &Path) -> bool {
    let rest = match path.strip_prefix(base) {
        Ok(r) => r,
        Err(_) => return false,
    };
    rest.components().any(|c| {
        if let Some(name) = normal_name(c) {
            name.eq_ignore_ascii_case("DCIM") || is_admin_dir(name)
        } else {
            false
        }
    })
}

/// Validate the source/destination relationship before doing anything.
/// `source` and `dest` should already be normalized & absolute.
pub fn validate_dest(source: &Path, dest: &Path) -> Result<()> {
    if dest == source {
        bail!("--dest must differ from SOURCE ({})", source.display());
    }
    if is_within(dest, source) {
        bail!(
            "--dest ({}) is inside SOURCE ({}); that would re-scan moved files",
            dest.display(),
            source.display()
        );
    }
    if is_within(source, dest) {
        bail!(
            "SOURCE ({}) is inside --dest ({}); refusing to organize a tree into its own ancestor",
            source.display(),
            dest.display()
        );
    }
    // The destination must not live inside DCIM or a camera-managed dir. Check
    // only components *below the card root* so the volume name itself (which on
    // a Sony card is literally "SONY") is not mistaken for the managed dir.
    let card_root = common_ancestor(source, dest);
    if has_forbidden_below(&card_root, dest) {
        bail!(
            "--dest ({}) lies inside DCIM or a camera-managed directory; choose a folder at the card root",
            dest.display()
        );
    }
    Ok(())
}

/// Final guard before writing: a computed target must stay under the validated
/// destination root and never escape into a sibling tree.
pub fn assert_target_allowed(dst: &Path, dest_root: &Path) -> Result<()> {
    if !is_within(dst, dest_root) {
        bail!(
            "refusing to write outside the destination root: {} (root {})",
            dst.display(),
            dest_root.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_dest_in_dcim() {
        let src = Path::new("/Volumes/SONY/DCIM");
        let dst = Path::new("/Volumes/SONY/DCIM/Organized");
        assert!(validate_dest(src, dst).is_err());
    }

    #[test]
    fn rejects_dest_equals_source() {
        let p = Path::new("/Volumes/SONY/DCIM");
        assert!(validate_dest(p, p).is_err());
    }

    #[test]
    fn rejects_admin_dest() {
        let src = Path::new("/Volumes/SONY/DCIM");
        let dst = Path::new("/Volumes/SONY/PRIVATE/x");
        assert!(validate_dest(src, dst).is_err());
    }

    #[test]
    fn accepts_sibling_dest() {
        let src = Path::new("/Volumes/SONY/DCIM");
        let dst = Path::new("/Volumes/SONY/Organized");
        assert!(validate_dest(src, dst).is_ok());
    }

    #[test]
    fn normalize_resolves_dotdot() {
        assert_eq!(normalize(Path::new("/a/b/../c")), PathBuf::from("/a/c"));
    }
}
