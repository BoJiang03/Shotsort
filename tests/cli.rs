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
        .args([
            "--mode",
            "video",
            "--link",
            "--date-source",
            "mtime",
            "--yes",
        ])
        .status()
        .unwrap();
    assert!(status.success());

    let link = dest.join("2026/2026-06-25/C0005.MP4");
    // It's a symlink (not a copy)...
    let meta = fs::symlink_metadata(&link).unwrap();
    assert!(meta.file_type().is_symlink(), "expected a symlink");
    // ...with a RELATIVE target (survives the card being renamed)...
    let target = fs::read_link(&link).unwrap();
    assert!(
        target.is_relative(),
        "link target must be relative: {target:?}"
    );
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
fn counter_names_by_shot_order_and_continues_across_runs() {
    let card = TempCard::new("counter");
    let dcim = card.path().join("DCIM");
    let dest = card.path().join("Organized");
    let day = dest.join("2026/2026-06-15");

    // Three shots on one day, in ascending capture (mtime) order.
    write(&dcim.join("100MSDCF/DSC00001.JPG"), "A", "202606150901.00");
    write(&dcim.join("100MSDCF/DSC00002.JPG"), "B", "202606150902.00");
    write(&dcim.join("100MSDCF/DSC00003.JPG"), "C", "202606150903.00");

    let run = |src: &Path| {
        let status = bin()
            .arg(src)
            .args(["--dest"])
            .arg(&dest)
            .args([
                "--date-source",
                "mtime",
                "--name-template",
                "{counter:04}",
                "--yes",
            ])
            .status()
            .unwrap();
        assert!(status.success());
    };
    run(&dcim);

    // 0-based, numbered by shooting order within the date folder.
    assert_eq!(fs::read_to_string(day.join("0000.JPG")).unwrap(), "A");
    assert_eq!(fs::read_to_string(day.join("0001.JPG")).unwrap(), "B");
    assert_eq!(fs::read_to_string(day.join("0002.JPG")).unwrap(), "C");

    // A second batch shot later the same day, run separately...
    write(&dcim.join("100MSDCF/DSC00004.JPG"), "D", "202606150904.00");
    write(&dcim.join("100MSDCF/DSC00005.JPG"), "E", "202606150905.00");
    run(&dcim);

    // ...continues the sequence instead of restarting at 0000 and colliding.
    assert_eq!(fs::read_to_string(day.join("0003.JPG")).unwrap(), "D");
    assert_eq!(fs::read_to_string(day.join("0004.JPG")).unwrap(), "E");
    // Files from the first run keep their original numbers (never renamed).
    assert_eq!(fs::read_to_string(day.join("0000.JPG")).unwrap(), "A");
    assert_eq!(fs::read_to_string(day.join("0002.JPG")).unwrap(), "C");
}

/// Write a minimal JPEG carrying EXIF `DateTimeOriginal` + `SubSecTimeOriginal`,
/// enough for the matcher to read a millisecond capture key. `dt` is a 19-char
/// `"YYYY:MM:DD HH:MM:SS"`; `subsec` is up to 3 digits. Used for both the `.JPG`
/// previews and the `.ARW` archive files (kamadak-exif sniffs the JPEG content,
/// so the `.ARW` extension only affects shotsort's kind classification).
fn write_exif_image(path: &Path, dt: &str, subsec: &str) {
    assert_eq!(dt.len(), 19, "datetime must be YYYY:MM:DD HH:MM:SS");
    let subsec_bytes = subsec.as_bytes();
    let subsec_count = (subsec_bytes.len() + 1) as u32; // includes NUL
    assert!(
        subsec_count <= 4,
        "test helper only inlines short subsec values"
    );

    // TIFF (little-endian): header -> IFD0 (ExifIFD pointer) -> ExifIFD (two
    // ASCII tags) -> data area (the datetime string). Offsets are fixed by the
    // constant entry counts below.
    let exif_ifd_off: u32 = 26;
    let dt_off: u32 = 56;
    let mut tiff: Vec<u8> = Vec::new();
    tiff.extend_from_slice(b"II");
    tiff.extend_from_slice(&0x2Au16.to_le_bytes());
    tiff.extend_from_slice(&8u32.to_le_bytes()); // IFD0 at offset 8

    tiff.extend_from_slice(&1u16.to_le_bytes()); // IFD0: 1 entry
    tiff.extend_from_slice(&0x8769u16.to_le_bytes()); // ExifIFD pointer
    tiff.extend_from_slice(&4u16.to_le_bytes()); // LONG
    tiff.extend_from_slice(&1u32.to_le_bytes());
    tiff.extend_from_slice(&exif_ifd_off.to_le_bytes());
    tiff.extend_from_slice(&0u32.to_le_bytes()); // no next IFD

    tiff.extend_from_slice(&2u16.to_le_bytes()); // ExifIFD: 2 entries
    tiff.extend_from_slice(&0x9003u16.to_le_bytes()); // DateTimeOriginal
    tiff.extend_from_slice(&2u16.to_le_bytes()); // ASCII
    tiff.extend_from_slice(&20u32.to_le_bytes());
    tiff.extend_from_slice(&dt_off.to_le_bytes());
    tiff.extend_from_slice(&0x9291u16.to_le_bytes()); // SubSecTimeOriginal
    tiff.extend_from_slice(&2u16.to_le_bytes()); // ASCII
    tiff.extend_from_slice(&subsec_count.to_le_bytes());
    let mut inline = [0u8; 4];
    inline[..subsec_bytes.len()].copy_from_slice(subsec_bytes);
    tiff.extend_from_slice(&inline);
    tiff.extend_from_slice(&0u32.to_le_bytes()); // no next IFD

    assert_eq!(tiff.len() as u32, dt_off);
    tiff.extend_from_slice(dt.as_bytes());
    tiff.push(0); // NUL-terminate -> 20 bytes

    let mut jpeg: Vec<u8> = vec![0xFF, 0xD8, 0xFF, 0xE1];
    let app1_len = (2 + 6 + tiff.len()) as u16;
    jpeg.extend_from_slice(&app1_len.to_be_bytes());
    jpeg.extend_from_slice(b"Exif\0\0");
    jpeg.extend_from_slice(&tiff);
    jpeg.extend_from_slice(&[0xFF, 0xD9]);

    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, jpeg).unwrap();
}

/// Build a throwaway PixCake data dir: `base.db` (name → project) plus a project
/// `thumbnail` table. `rows` are `(preview path, inRecycleBin)`.
fn build_pixcake_db(pixdata: &Path, project: &str, rows: &[(PathBuf, i64)]) {
    let proj = pixdata.join("db/user_1/project_1");
    fs::create_dir_all(&proj).unwrap();
    let conn = rusqlite::Connection::open(proj.join("project.db")).unwrap();
    conn.execute(
        "CREATE TABLE thumbnail(originalImagePath TEXT, inRecycleBin INT, isValid INT, captureTime INTEGER)",
        [],
    )
    .unwrap();
    for (p, bin) in rows {
        conn.execute(
            "INSERT INTO thumbnail VALUES (?1, ?2, 1, 0)",
            rusqlite::params![p.to_str().unwrap(), bin],
        )
        .unwrap();
    }
    drop(conn);

    let conn = rusqlite::Connection::open(pixdata.join("db/base.db")).unwrap();
    conn.execute(
        "CREATE TABLE project_operation_log(id INTEGER PRIMARY KEY AUTOINCREMENT, \
         userId INTEGER, projectId INTEGER, name TEXT, type INTEGER, source INTEGER, time INTEGER)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO project_operation_log(userId,projectId,name,type,source,time) \
         VALUES (1,1,?1,1,0,1783953000000)",
        [project],
    )
    .unwrap();
}

#[test]
fn match_pairs_keepers_to_raws_by_subsecond() {
    let card = TempCard::new("match");
    let root = card.path();
    let archive = root.join("Organized/2026/2026-07-13");
    let previews = root.join("previews");
    let pixdata = root.join("pix");
    let out = root.join("Selects");

    // Archive RAWs: a same-second burst (A,B differ only in sub-second) + a later
    // frame (C). A wrong second-only match could grab A instead of B.
    write_exif_image(&archive.join("A.ARW"), "2026:07:13 17:00:00", "110");
    write_exif_image(&archive.join("B.ARW"), "2026:07:13 17:00:00", "451");
    write_exif_image(&archive.join("C.ARW"), "2026:07:13 17:00:05", "000");

    // Preview JPEGs: two kept (pointing at B and C by sub-second) + one removed
    // (which would map to A) to prove recycle-bin rows are excluded.
    write_exif_image(
        &previews.join("shot_0002.JPG"),
        "2026:07:13 17:00:00",
        "451",
    );
    write_exif_image(
        &previews.join("shot_0009.JPG"),
        "2026:07:13 17:00:05",
        "000",
    );
    write_exif_image(
        &previews.join("shot_0001.JPG"),
        "2026:07:13 17:00:00",
        "110",
    );

    build_pixcake_db(
        &pixdata,
        "SHOOT",
        &[
            (previews.join("shot_0002.JPG"), 0),
            (previews.join("shot_0009.JPG"), 0),
            (previews.join("shot_0001.JPG"), 1), // removed -> excluded
        ],
    );

    let status = bin()
        .args(["match", "--project", "SHOOT", "--pixcake-dir"])
        .arg(&pixdata)
        .arg("--raw-src")
        .arg(root.join("Organized"))
        .arg("--out")
        .arg(&out)
        .arg("--yes")
        .status()
        .unwrap();
    assert!(status.success());

    // Sub-second picked B (not A) for the 17:00:00 keeper, and C for the later one.
    assert!(out.join("B.ARW").exists(), "expected B (subsec match)");
    assert!(out.join("C.ARW").exists());
    // A's shot was removed from the project, so its RAW is NOT gathered.
    assert!(
        !out.join("A.ARW").exists(),
        "A belonged to a removed keeper"
    );
    // Copy, not move: originals remain in the archive.
    assert!(archive.join("A.ARW").exists());
    assert!(archive.join("B.ARW").exists());

    // Journal records two copies, so undo can reverse the gather.
    let journal = out.join(".shotsort-journal.jsonl");
    let lines = fs::read_to_string(&journal).unwrap();
    assert_eq!(lines.lines().filter(|l| l.contains("\"copy\"")).count(), 2);
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
