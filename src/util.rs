//! Small shared helpers.

use std::fs::File;
use std::io::{Read, Result as IoResult};
use std::path::Path;
use std::time::SystemTime;

use chrono::{DateTime, Local, NaiveDateTime};

/// BLAKE3 hash of a file's full contents (used only for hash dedup and
/// cross-filesystem verification).
pub fn hash_file(path: &Path) -> IoResult<blake3::Hash> {
    let mut file = File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize())
}

/// Convert a filesystem timestamp to a local naive datetime.
pub fn systemtime_to_local_naive(st: SystemTime) -> NaiveDateTime {
    let dt: DateTime<Local> = st.into();
    dt.naive_local()
}

/// Human-readable byte size (binary units).
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}
