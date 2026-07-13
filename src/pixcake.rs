//! Reading PixCake's on-disk project databases.
//!
//! PixCake (像素蛋糕) stores each project as its own SQLite database under
//! `<data>/db/user_<uid>/project_<pid>/project.db`, with a `thumbnail` row per
//! image. The columns that matter here:
//!
//! - `originalImagePath` — the image file on disk (a tethered preview JPEG in the
//!   common case, sometimes an already-imported RAW).
//! - `inRecycleBin` — 0 while the image is part of the project; set to 1 when the
//!   photographer/model "removes" it. Crucially, removal only drops the row from
//!   the project — the file on disk is left untouched. So the kept set is defined
//!   by this flag, **not** by which files still exist in the folder.
//! - `isValid` — 1 for a normal image row.
//!
//! The project *name* shown in the app lives separately, in
//! `<data>/db/base.db`'s `project_operation_log` (name → projectId; the same name
//! can map to several ids over time, so we take the newest whose project still
//! exists on disk).
//!
//! Everything here reads a **temp copy** of the SQLite files rather than the
//! originals: PixCake may be running with the database open in WAL mode, and we
//! must never lock or perturb it.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use rusqlite::Connection;

/// One kept image from a PixCake project.
#[derive(Debug, Clone)]
pub struct KeptItem {
    /// Path recorded by PixCake (usually a preview JPEG; sometimes a RAW).
    pub original: PathBuf,
    /// PixCake's stored capture time in Unix milliseconds, if any. Only
    /// second-precision in practice, so it is a coarse fallback when the file
    /// itself can no longer be read for its sub-second EXIF.
    pub capture_time_ms: Option<i64>,
}

/// The default macOS PixCake data directory.
pub fn default_data_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(
        PathBuf::from(home)
            .join("Library/Application Support")
            .join("PixCake-qt_pro"),
    )
}

/// Locate the live `project.db` for a project by its display name. Prefers the
/// newest matching project (a name can be reused) whose database still exists.
pub fn find_project(data_dir: &Path, name: &str) -> Result<PathBuf> {
    if !data_dir.exists() {
        bail!(
            "PixCake data directory not found: {}\n  \
             (pass --pixcake-dir to point at it)",
            data_dir.display()
        );
    }

    // Primary: resolve name -> projectId via base.db, newest first.
    if let Some(db) = resolve_via_base_db(data_dir, name)? {
        return Ok(db);
    }
    // Fallback: scan live project databases for one whose images carry the name
    // as their filename prefix (`<name>_0001.JPG`).
    if let Some(db) = resolve_via_scan(data_dir, name)? {
        return Ok(db);
    }

    let known = known_project_names(data_dir).unwrap_or_default();
    if known.is_empty() {
        bail!(
            "no PixCake project named {name:?} found under {}",
            data_dir.display()
        );
    }
    bail!(
        "no PixCake project named {name:?}. Known projects (newest first):\n  {}",
        known.join("\n  ")
    );
}

/// Kept images of a project: rows still in the project (not in its recycle bin).
pub fn kept_items(project_db: &Path) -> Result<Vec<KeptItem>> {
    with_snapshot(project_db, |conn| {
        let mut stmt = conn.prepare(
            "SELECT originalImagePath, MAX(captureTime) \
             FROM thumbnail \
             WHERE inRecycleBin = 0 AND isValid = 1 \
               AND originalImagePath IS NOT NULL AND originalImagePath <> '' \
             GROUP BY originalImagePath",
        )?;
        let rows = stmt.query_map([], |row| {
            let path: String = row.get(0)?;
            let cap: Option<i64> = row.get(1).ok().flatten();
            Ok(KeptItem {
                original: PathBuf::from(path),
                capture_time_ms: cap.filter(|&v| v > 0),
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    })
}

fn resolve_via_base_db(data_dir: &Path, name: &str) -> Result<Option<PathBuf>> {
    let base = data_dir.join("db/base.db");
    if !base.exists() {
        return Ok(None);
    }
    let candidates = with_snapshot(&base, |conn| {
        let mut stmt = conn.prepare(
            "SELECT userId, projectId FROM project_operation_log \
             WHERE name = ?1 GROUP BY userId, projectId ORDER BY MAX(time) DESC",
        )?;
        let rows = stmt.query_map([name], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })?;
        let mut v = Vec::new();
        for r in rows {
            v.push(r?);
        }
        Ok(v)
    })?;

    for (user_id, project_id) in candidates {
        let db = data_dir
            .join("db")
            .join(format!("user_{user_id}"))
            .join(format!("project_{project_id}"))
            .join("project.db");
        if db.exists() {
            return Ok(Some(db));
        }
    }
    Ok(None)
}

fn resolve_via_scan(data_dir: &Path, name: &str) -> Result<Option<PathBuf>> {
    let like = format!("%/{name}\\_%");
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for db in live_project_dbs(data_dir) {
        let count = with_snapshot(&db, |conn| {
            let n: i64 = conn.query_row(
                "SELECT count(*) FROM thumbnail \
                 WHERE inRecycleBin = 0 AND originalImagePath LIKE ?1 ESCAPE '\\'",
                [&like],
                |row| row.get(0),
            )?;
            Ok(n)
        })
        .unwrap_or(0);
        if count > 0 {
            let mtime = fs::metadata(&db)
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            if best.as_ref().is_none_or(|(t, _)| mtime > *t) {
                best = Some((mtime, db));
            }
        }
    }
    Ok(best.map(|(_, db)| db))
}

/// All existing `project.db` files under `<data>/db/user_*/project_*/`.
fn live_project_dbs(data_dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let db_root = data_dir.join("db");
    let Ok(users) = fs::read_dir(&db_root) else {
        return out;
    };
    for user in users.flatten() {
        if !user.file_name().to_string_lossy().starts_with("user_") {
            continue;
        }
        let Ok(projects) = fs::read_dir(user.path()) else {
            continue;
        };
        for proj in projects.flatten() {
            if !proj.file_name().to_string_lossy().starts_with("project_") {
                continue;
            }
            let db = proj.path().join("project.db");
            if db.exists() {
                out.push(db);
            }
        }
    }
    out
}

/// Distinct project names known to base.db, newest first (for error messages).
fn known_project_names(data_dir: &Path) -> Result<Vec<String>> {
    let base = data_dir.join("db/base.db");
    if !base.exists() {
        return Ok(Vec::new());
    }
    with_snapshot(&base, |conn| {
        let mut stmt = conn.prepare(
            "SELECT name FROM project_operation_log \
             WHERE name IS NOT NULL AND name <> '' \
             GROUP BY name ORDER BY MAX(time) DESC LIMIT 30",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut v = Vec::new();
        for r in rows {
            v.push(r?);
        }
        Ok(v)
    })
}

/// Run `f` against a throwaway copy of a SQLite database (plus its `-wal`/`-shm`
/// sidecars), so a live PixCake process is never locked or disturbed. On open,
/// SQLite folds any copied WAL frames into the snapshot, so we still see the
/// latest committed state.
fn with_snapshot<T>(db: &Path, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = std::env::temp_dir().join(format!("shotsort-pixcake-{}-{stamp}", std::process::id()));
    fs::create_dir_all(&tmp).with_context(|| format!("creating temp dir {}", tmp.display()))?;

    let target = tmp.join("snapshot.db");
    let result = (|| {
        fs::copy(db, &target).with_context(|| format!("snapshotting {}", db.display()))?;
        for suffix in ["-wal", "-shm"] {
            let side = sidecar(db, suffix);
            if side.exists() {
                let _ = fs::copy(&side, sidecar(&target, suffix));
            }
        }
        let conn = Connection::open(&target)
            .with_context(|| format!("opening snapshot of {}", db.display()))?;
        f(&conn)
    })();

    let _ = fs::remove_dir_all(&tmp);
    result
}

/// `foo.db` + `-wal` → `foo.db-wal` (SQLite's sidecar naming).
fn sidecar(db: &Path, suffix: &str) -> PathBuf {
    let mut name = db.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    db.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_project(dir: &Path, rows: &[(&str, i64, i64)]) -> PathBuf {
        // rows: (originalImagePath, inRecycleBin, captureTime)
        let db = dir.join("project.db");
        let conn = Connection::open(&db).unwrap();
        conn.execute(
            "CREATE TABLE thumbnail (originalImagePath TEXT, inRecycleBin INT, \
             isValid INT, captureTime INTEGER)",
            [],
        )
        .unwrap();
        for (p, bin, cap) in rows {
            conn.execute(
                "INSERT INTO thumbnail VALUES (?1, ?2, 1, ?3)",
                rusqlite::params![p, bin, cap],
            )
            .unwrap();
        }
        db
    }

    #[test]
    fn kept_items_excludes_recycle_bin() {
        let tmp = std::env::temp_dir().join(format!("shotsort-pxtest-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let db = make_project(
            &tmp,
            &[
                ("/p/shot_0005.JPG", 0, 1783936195000),
                ("/p/shot_0006.JPG", 1, 1783936196000), // removed -> excluded
                ("/p/shot_0007.JPG", 0, 0),             // kept, no capture time
            ],
        );

        let mut kept = kept_items(&db).unwrap();
        kept.sort_by(|a, b| a.original.cmp(&b.original));
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0].original, PathBuf::from("/p/shot_0005.JPG"));
        assert_eq!(kept[0].capture_time_ms, Some(1783936195000));
        assert_eq!(kept[1].original, PathBuf::from("/p/shot_0007.JPG"));
        assert_eq!(kept[1].capture_time_ms, None); // 0 normalized to None

        let _ = fs::remove_dir_all(&tmp);
    }
}
