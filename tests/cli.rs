//! End-to-end CLI tests driving the built binary against a temp "card".

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// A unique temp directory, removed on drop.
struct TempCard(PathBuf);

impl TempCard {
    fn new(tag: &str) -> TempCard {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("shotsort-it-{}-{}-{tag}", std::process::id(), n));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        TempCard(dir)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempCard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_shotsort"))
}

fn write(path: &Path, content: &str, mtime: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
    // Set mtime so `--date-source mtime` is deterministic.
    let status = Command::new("touch")
        .args(["-t", mtime])
        .arg(path)
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
fn move_pairs_conflicts_and_excludes_managed_dirs() {
    let card = TempCard::new("move");
    let dcim = card.path().join("DCIM");
    let dest = card.path().join("Organized");

    // RAW + JPEG + sidecar share a stem and must travel together.
    write(
        &dcim.join("100MSDCF/DSC00001.ARW"),
        "raw",
        "202606150930.00",
    );
    write(
        &dcim.join("100MSDCF/DSC00001.JPG"),
        "jpg",
        "202606150930.00",
    );
    write(
        &dcim.join("100MSDCF/DSC00001.xmp"),
        "xmp",
        "202606150930.00",
    );
    // Two distinct files with the same name -> conflict -> rename.
    write(
        &dcim.join("101MSDCF/DSC00009.JPG"),
        "AAAA",
        "202606150930.00",
    );
    write(
        &dcim.join("102MSDCF/DSC00009.JPG"),
        "BBBB",
        "202606150930.00",
    );
    // Inside a managed dir -> must never move.
    write(
        &card.path().join("PRIVATE/M4ROOT/SECRET.JPG"),
        "x",
        "202606150930.00",
    );

    let status = bin()
        .arg(&dcim)
        .args(["--dest"])
        .arg(&dest)
        .args(["--date-source", "mtime", "--yes", "--clean-empty-dirs"])
        .status()
        .unwrap();
    assert!(status.success());

    let day = dest.join("2026/2026-06-15");
    assert!(day.join("DSC00001.ARW").exists());
    assert!(day.join("DSC00001.JPG").exists());
    assert!(day.join("DSC00001.xmp").exists());
    // One of the colliding files keeps its name, the other is renamed.
    assert!(day.join("DSC00009.JPG").exists());
    assert!(day.join("DSC00009_001.JPG").exists());
    // Managed dir untouched.
    assert!(card.path().join("PRIVATE/M4ROOT/SECRET.JPG").exists());
    // Emptied source subfolders were cleaned, but DCIM itself remains.
    assert!(dcim.exists());
    assert!(!dcim.join("100MSDCF").exists());

    // Journal exists and records five moves.
    let journal = dest.join(".shotsort-journal.jsonl");
    let lines = fs::read_to_string(&journal).unwrap();
    assert_eq!(lines.lines().filter(|l| !l.trim().is_empty()).count(), 5);

    // Undo restores the original layout.
    let undo = bin()
        .args(["undo", "--journal"])
        .arg(&journal)
        .arg("--yes")
        .status()
        .unwrap();
    assert!(undo.success());
    assert!(dcim.join("100MSDCF/DSC00001.ARW").exists());
    assert!(dcim.join("101MSDCF/DSC00009.JPG").exists());
    assert!(dcim.join("102MSDCF/DSC00009.JPG").exists());
}

#[test]
fn rejects_dest_inside_dcim() {
    let card = TempCard::new("guard");
    let dcim = card.path().join("DCIM");
    write(&dcim.join("100MSDCF/IMG_0001.JPG"), "a", "202606150930.00");

    let out = bin()
        .arg(&dcim)
        .args(["--dest"])
        .arg(dcim.join("Organized"))
        .arg("--yes")
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("inside SOURCE"), "stderr was: {stderr}");
}

#[test]
fn missing_date_goes_to_nodate() {
    let card = TempCard::new("nodate");
    let dcim = card.path().join("DCIM");
    let dest = card.path().join("Organized");
    // No EXIF, default date-source = exif -> no date -> NoDate folder.
    write(
        &dcim.join("100MSDCF/IMG_0001.JPG"),
        "noexif",
        "202606150930.00",
    );

    let status = bin()
        .arg(&dcim)
        .args(["--dest"])
        .arg(&dest)
        .arg("--yes")
        .status()
        .unwrap();
    assert!(status.success());
    assert!(dest.join("NoDate/IMG_0001.JPG").exists());
}

#[test]
fn video_mode_copies_clips_and_keeps_originals() {
    let card = TempCard::new("video");
    // Sony XAVC clips live under a managed dir that photo mode never touches.
    let clip = card.path().join("PRIVATE/M4ROOT/CLIP");
    let dest = card.path().join("Organized");

    write(&clip.join("C0005.MP4"), "clip-five", "202606250839.00");
    write(&clip.join("C0006.MP4"), "clip-six", "202606260123.00");
    // A proxy under SUB and a thumbnail under THMBNL must NOT be copied out.
    write(
        &card.path().join("PRIVATE/M4ROOT/SUB/C0005S01.MP4"),
        "proxy",
        "202606250839.00",
    );
    write(
        &card.path().join("PRIVATE/M4ROOT/THMBNL/C0005T01.JPG"),
        "thumb",
        "202606250839.00",
    );

    // Point SOURCE at the card root in video mode.
    let status = bin()
        .arg(card.path())
        .args(["--dest"])
        .arg(&dest)
        .args(["--mode", "video", "--date-source", "mtime", "--yes"])
        .status()
        .unwrap();
    assert!(status.success());

    // Masters copied into date folders.
    assert!(dest.join("2026/2026-06-25/C0005.MP4").exists());
    assert!(dest.join("2026/2026-06-26/C0006.MP4").exists());
    // Proxy + thumbnail were skipped.
    assert!(!dest.join("2026/2026-06-25/C0005S01.MP4").exists());
    assert!(!dest.join("2026/2026-06-25/C0005T01.JPG").exists());
    // Originals are still on the card (copy, not move).
    assert!(clip.join("C0005.MP4").exists());
    assert!(clip.join("C0006.MP4").exists());

    // Journal records two copies.
    let journal = dest.join(".shotsort-journal.jsonl");
    let lines = fs::read_to_string(&journal).unwrap();
    assert_eq!(lines.lines().filter(|l| l.contains("\"copy\"")).count(), 2);

    // Undo deletes the copies but leaves the originals intact.
    let undo = bin()
        .args(["undo", "--journal"])
        .arg(&journal)
        .arg("--yes")
        .status()
        .unwrap();
    assert!(undo.success());
    assert!(!dest.join("2026/2026-06-25/C0005.MP4").exists());
    assert!(!dest.join("2026/2026-06-26/C0006.MP4").exists());
    assert!(clip.join("C0005.MP4").exists());
    assert!(clip.join("C0006.MP4").exists());
}

#[test]
fn video_link_mode_makes_relative_symlinks() {
    let card = TempCard::new("vlink");
    let clip = card.path().join("PRIVATE/M4ROOT/CLIP");
    let dest = card.path().join("Organized");

    write(&clip.join("C0005.MP4"), "clip-five", "202606250839.00");

    let status = bin()
        .arg(card.path())
        .args(["--dest"])
        .arg(&dest)
        .args(["--mode", "video", "--link", "--date-source", "mtime", "--yes"])
        .status()
        .unwrap();
    assert!(status.success());

    let link = dest.join("2026/2026-06-25/C0005.MP4");
    // It's a symlink (not a copy)...
    let meta = fs::symlink_metadata(&link).unwrap();
    assert!(meta.file_type().is_symlink(), "expected a symlink");
    // ...with a RELATIVE target (survives the card being renamed)...
    let target = fs::read_link(&link).unwrap();
    assert!(target.is_relative(), "link target must be relative: {target:?}");
    // ...that resolves to the original clip's bytes.
    assert_eq!(fs::read(&link).unwrap(), b"clip-five");
    // Original untouched.
    assert!(clip.join("C0005.MP4").exists());

    // Undo removes the link; the original stays.
    let journal = dest.join(".shotsort-journal.jsonl");
    assert!(fs::read_to_string(&journal).unwrap().contains("\"link\""));
    let undo = bin()
        .args(["undo", "--journal"])
        .arg(&journal)
        .arg("--yes")
        .status()
        .unwrap();
    assert!(undo.success());
    assert!(fs::symlink_metadata(&link).is_err(), "link should be gone");
    assert!(clip.join("C0005.MP4").exists());
}

#[test]
fn dry_run_moves_nothing() {
    let card = TempCard::new("dry");
    let dcim = card.path().join("DCIM");
    let dest = card.path().join("Organized");
    write(&dcim.join("100MSDCF/IMG_0001.JPG"), "a", "202606150930.00");

    let status = bin()
        .arg(&dcim)
        .args(["--dest"])
        .arg(&dest)
        .args(["--date-source", "mtime", "--dry-run"])
        .status()
        .unwrap();
    assert!(status.success());
    // Source untouched, dest never created.
    assert!(dcim.join("100MSDCF/IMG_0001.JPG").exists());
    assert!(!dest.exists());
}
