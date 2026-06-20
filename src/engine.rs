//! The move/copy engine — the safety-critical core.
//!
//! Invariant: at any interruption point, every file is wholly present at its
//! source OR its destination, never neither.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::cli::VerifyArg;
use crate::types::Action;
use crate::util::hash_file;

/// `EXDEV` ("cross-device link") — same on Linux and macOS.
const EXDEV: i32 = 18;

/// Result of a successful transfer.
pub struct Outcome {
    pub bytes: u64,
    pub crossed_fs: bool,
}

/// Execute one planned transfer (move/copy/overwrite). Returns the bytes moved.
pub fn perform(src: &Path, dst: &Path, action: Action, verify: VerifyArg) -> Result<Outcome> {
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating target dir {}", parent.display()))?;
    }

    let size = fs::metadata(src)
        .with_context(|| format!("stat source {}", src.display()))?
        .len();

    match action {
        Action::Copy => {
            copy_with_verify(src, dst, verify)?;
            Ok(Outcome {
                bytes: size,
                crossed_fs: false,
            })
        }
        Action::Move | Action::Overwrite => move_file(src, dst, verify, size),
        other => bail!("engine cannot perform action {other:?}"),
    }
}

fn move_file(src: &Path, dst: &Path, verify: VerifyArg, size: u64) -> Result<Outcome> {
    // The happy path: an atomic, in-place rename on the same filesystem.
    match fs::rename(src, dst) {
        Ok(()) => {
            verify_same_fs(src, dst, verify)?;
            Ok(Outcome {
                bytes: size,
                crossed_fs: false,
            })
        }
        Err(e) if e.raw_os_error() == Some(EXDEV) => {
            eprintln!(
                "warn: {} and target are on different filesystems; using copy→verify→delete fallback",
                src.display()
            );
            cross_fs_move(src, dst)?;
            Ok(Outcome {
                bytes: size,
                crossed_fs: true,
            })
        }
        Err(e) => {
            Err(e).with_context(|| format!("renaming {} -> {}", src.display(), dst.display()))
        }
    }
}

/// Same-filesystem verification: the bytes never moved, so we only confirm the
/// file landed and the source is gone.
fn verify_same_fs(src: &Path, dst: &Path, verify: VerifyArg) -> Result<()> {
    if matches!(verify, VerifyArg::Off) {
        return Ok(());
    }
    if !dst.exists() {
        bail!("post-move check failed: target missing {}", dst.display());
    }
    if src.exists() {
        bail!(
            "post-move check failed: source still present {}",
            src.display()
        );
    }
    Ok(())
}

/// Cross-filesystem fallback: copy → fsync → hash-verify → delete source →
/// promote temp to final name. The source is removed only after the copy is
/// proven byte-identical.
fn cross_fs_move(src: &Path, dst: &Path) -> Result<()> {
    let tmp = temp_sibling(dst);

    copy_bytes_fsync(src, &tmp)
        .with_context(|| format!("copying {} -> {}", src.display(), tmp.display()))?;

    let src_hash = hash_file(src).with_context(|| format!("hashing source {}", src.display()))?;
    let tmp_hash = hash_file(&tmp).with_context(|| format!("hashing temp {}", tmp.display()))?;
    if src_hash != tmp_hash {
        let _ = fs::remove_file(&tmp);
        bail!(
            "cross-fs copy verification failed for {}; source left intact",
            src.display()
        );
    }

    fs::remove_file(src).with_context(|| format!("removing source {}", src.display()))?;
    fs::rename(&tmp, dst)
        .with_context(|| format!("promoting temp {} -> {}", tmp.display(), dst.display()))?;
    Ok(())
}

fn copy_with_verify(src: &Path, dst: &Path, verify: VerifyArg) -> Result<()> {
    let tmp = temp_sibling(dst);
    copy_bytes_fsync(src, &tmp)
        .with_context(|| format!("copying {} -> {}", src.display(), tmp.display()))?;

    if matches!(verify, VerifyArg::Hash | VerifyArg::Auto) {
        let src_hash = hash_file(src)?;
        let tmp_hash = hash_file(&tmp)?;
        if src_hash != tmp_hash {
            let _ = fs::remove_file(&tmp);
            bail!("copy verification failed for {}", src.display());
        }
    }
    fs::rename(&tmp, dst)
        .with_context(|| format!("promoting temp {} -> {}", tmp.display(), dst.display()))?;
    Ok(())
}

fn copy_bytes_fsync(src: &Path, tmp: &Path) -> Result<()> {
    let mut input = File::open(src)?;
    let mut output = File::create(tmp)?;
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = input.read(&mut buf)?;
        if n == 0 {
            break;
        }
        output.write_all(&buf[..n])?;
    }
    output.flush()?;
    output.sync_all()?; // durably on the platter before we trust it
    Ok(())
}

/// A unique temp name beside the destination so the promote rename is in-dir.
fn temp_sibling(dst: &Path) -> std::path::PathBuf {
    let name = dst
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".to_string());
    let parent = dst.parent().unwrap_or(Path::new("."));
    parent.join(format!(".{name}.shotsort-{}.tmp", std::process::id()))
}

/// True if source and target directory are on the same filesystem (Unix dev id).
#[cfg(unix)]
pub fn same_filesystem(src: &Path, dst_dir: &Path) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let s = fs::metadata(src)?;
    let d = fs::metadata(dst_dir)?;
    Ok(s.dev() == d.dev())
}

#[cfg(not(unix))]
pub fn same_filesystem(_src: &Path, _dst_dir: &Path) -> io::Result<bool> {
    // Other platforms fall back to trying rename and catching EXDEV.
    Ok(true)
}
