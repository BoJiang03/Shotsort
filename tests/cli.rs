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
