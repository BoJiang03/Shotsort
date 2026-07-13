//! Capture-date and camera-metadata extraction.
//!
//! Images use EXIF (`kamadak-exif`); video containers (MP4/MOV) use the
//! `mvhd` movie-header creation time. Other containers (AVI/MTS/M2TS) have no
//! easily-read capture date and fall back to mtime per the date-source policy.

use std::fs::File;
use std::io::{BufReader, Cursor, Read, Seek, SeekFrom};
use std::path::Path;

use chrono::{DateTime, FixedOffset, Local, NaiveDateTime};
use exif::{In, Tag, Value};

use crate::types::{CaptureInfo, CaptureKey, FileKind};

/// Seconds between the 1904-01-01 (QuickTime/MP4) and 1970-01-01 (Unix) epochs.
const MAC_TO_UNIX_EPOCH: i64 = 2_082_844_800;

/// Extract embedded capture metadata. `tz_offset` only affects UTC-sourced
/// video times; EXIF datetimes are already local wall-clock and used as-is.
pub fn extract(path: &Path, kind: FileKind, tz_offset: Option<FixedOffset>) -> CaptureInfo {
    match kind {
        FileKind::Raw | FileKind::Jpeg => exif_capture(path).unwrap_or_default(),
        FileKind::Video => CaptureInfo {
            datetime: video_capture(path, tz_offset),
            make: None,
            model: None,
        },
        FileKind::Sidecar => CaptureInfo::default(),
    }
}

fn exif_capture(path: &Path) -> Option<CaptureInfo> {
    let exif = read_exif(path, has_datetime)?;
    Some(CaptureInfo {
        datetime: capture_dt(&exif),
        make: get_ascii(&exif, Tag::Make),
        model: get_ascii(&exif, Tag::Model),
    })
}

/// Read a millisecond-precise [`CaptureKey`] (`DateTimeOriginal` +
/// `SubSecTimeOriginal`) from an image's EXIF. Used by `shotsort match` to pair a
/// PixCake preview JPEG with its RAW: both carry the same value, so equal keys
/// mean the same shot. Returns `None` if the file has no usable EXIF datetime.
pub fn exif_capture_key(path: &Path) -> Option<CaptureKey> {
    let exif = read_exif(path, has_datetime)?;
    let dt = capture_dt(&exif)?;
    let subsec_ms = get_ascii(&exif, Tag::SubSecTimeOriginal)
        .as_deref()
        .map(parse_subsec_ms)
        .unwrap_or(0);
    Some(CaptureKey { dt, subsec_ms })
}

/// Read EXIF, reading only a bounded **header prefix** when it suffices.
/// `kamadak-exif` reads the *entire* file for TIFF-based RAW (Sony ARW is one), so
/// naively scanning an archive drags every RAW's pixel data off the card. The
/// capture tags all live in the first tens of KB, so we parse a 256 KB prefix and
/// accept it only if `want` is satisfied (e.g. a datetime is present); otherwise
/// we fall back to the full container for the rare file whose tags sit further in.
fn read_exif(path: &Path, want: impl Fn(&exif::Exif) -> bool) -> Option<exif::Exif> {
    const PREFIX: u64 = 256 * 1024;
    if let Ok(mut f) = File::open(path) {
        let mut buf = Vec::new();
        if (&mut f).take(PREFIX).read_to_end(&mut buf).is_ok()
            && let Ok(exif) = exif::Reader::new().read_from_container(&mut Cursor::new(&buf))
            && want(&exif)
        {
            return Some(exif);
        }
    }
    let file = File::open(path).ok()?;
    let mut reader = BufReader::new(file);
    exif::Reader::new().read_from_container(&mut reader).ok()
}

fn capture_dt(exif: &exif::Exif) -> Option<NaiveDateTime> {
    parse_exif_dt(exif, Tag::DateTimeOriginal)
        .or_else(|| parse_exif_dt(exif, Tag::DateTimeDigitized))
        .or_else(|| parse_exif_dt(exif, Tag::DateTime))
}

fn has_datetime(exif: &exif::Exif) -> bool {
    capture_dt(exif).is_some()
}

/// Normalize an EXIF SubSec string (a fraction of a second, most-significant
/// digit first) to whole milliseconds: `"851"` → 851, `"85"` → 850, `"8"` → 800,
/// `"8510"` → 851 (excess digits dropped). A blank/garbage value yields 0.
fn parse_subsec_ms(s: &str) -> u16 {
    let digits: String = s
        .trim()
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .take(3)
        .collect();
    if digits.is_empty() {
        return 0;
    }
    let mut v: u16 = digits.parse().unwrap_or(0);
    for _ in digits.len()..3 {
        v = v.saturating_mul(10); // pad centi-/deci-seconds out to milliseconds
    }
    v
}

fn parse_exif_dt(exif: &exif::Exif, tag: Tag) -> Option<NaiveDateTime> {
    let raw = get_ascii(exif, tag)?;
    // EXIF datetimes look like "2026:06:20 09:05:03"; some cameras pad with
    // NULs or use blanks for an unset value.
    let cleaned = raw.trim();
    if cleaned.starts_with("0000") || cleaned.is_empty() {
        return None;
    }
    NaiveDateTime::parse_from_str(cleaned, "%Y:%m:%d %H:%M:%S").ok()
}

fn get_ascii(exif: &exif::Exif, tag: Tag) -> Option<String> {
    let field = exif.get_field(tag, In::PRIMARY)?;
    match &field.value {
        Value::Ascii(parts) => {
            let bytes = parts.first()?;
            let s = String::from_utf8_lossy(bytes);
            let s = s.trim().trim_end_matches('\0').trim();
            if s.is_empty() {
                None
            } else {
                Some(s.to_string())
            }
        }
        _ => None,
    }
}

/// Read the `mvhd` creation time from an MP4/MOV file and convert it to a
/// local naive datetime.
fn video_capture(path: &Path, tz_offset: Option<FixedOffset>) -> Option<NaiveDateTime> {
    let unix = mp4_creation_unix(path)?;
    let utc = DateTime::from_timestamp(unix, 0)?;
    Some(match tz_offset {
        Some(off) => utc.with_timezone(&off).naive_local(),
        None => utc.with_timezone(&Local).naive_local(),
    })
}

/// Locate `moov > mvhd` and return its creation time as a Unix timestamp.
fn mp4_creation_unix(path: &Path) -> Option<i64> {
    let mut file = File::open(path).ok()?;
    let end = file.seek(SeekFrom::End(0)).ok()?;
    file.seek(SeekFrom::Start(0)).ok()?;
    search_mvhd(&mut file, 0, end, 0)
}

fn search_mvhd(file: &mut File, start: u64, end: u64, depth: u32) -> Option<i64> {
    if depth > 6 {
        return None;
    }
    let mut pos = start;
    while pos + 8 <= end {
        file.seek(SeekFrom::Start(pos)).ok()?;
        let mut header = [0u8; 8];
        if file.read_exact(&mut header).is_err() {
            break;
        }
        let mut size = u32::from_be_bytes(header[0..4].try_into().unwrap()) as u64;
        let typ = &header[4..8];
        let mut header_len = 8u64;

        if size == 1 {
            // 64-bit largesize follows the 8-byte header.
            let mut ext = [0u8; 8];
            if file.read_exact(&mut ext).is_err() {
                break;
            }
            size = u64::from_be_bytes(ext);
            header_len = 16;
        } else if size == 0 {
            // Box extends to the end of the file.
            size = end - pos;
        }

        if size < header_len || pos + size > end {
            break;
        }

        match typ {
            b"mvhd" => return read_mvhd_time(file, pos + header_len),
            b"moov" => {
                if let Some(t) = search_mvhd(file, pos + header_len, pos + size, depth + 1) {
                    return Some(t);
                }
            }
            _ => {}
        }
        pos += size;
    }
    None
}

fn read_mvhd_time(file: &mut File, body_start: u64) -> Option<i64> {
    // body: version(1) + flags(3) + creation_time(4 or 8) ...
    let mut version = [0u8; 1];
    file.seek(SeekFrom::Start(body_start)).ok()?;
    file.read_exact(&mut version).ok()?;

    let raw_secs = if version[0] == 1 {
        let mut buf = [0u8; 8];
        file.seek(SeekFrom::Start(body_start + 4)).ok()?;
        file.read_exact(&mut buf).ok()?;
        u64::from_be_bytes(buf)
    } else {
        let mut buf = [0u8; 4];
        file.seek(SeekFrom::Start(body_start + 4)).ok()?;
        file.read_exact(&mut buf).ok()?;
        u32::from_be_bytes(buf) as u64
    };

    if raw_secs == 0 {
        return None; // unset creation time
    }
    Some(raw_secs as i64 - MAC_TO_UNIX_EPOCH)
}

#[cfg(test)]
mod subsec_tests {
    use super::parse_subsec_ms;

    #[test]
    fn subsec_normalizes_to_milliseconds() {
        assert_eq!(parse_subsec_ms("851"), 851); // already ms
        assert_eq!(parse_subsec_ms("221"), 221);
        assert_eq!(parse_subsec_ms("85"), 850); // centiseconds -> ms
        assert_eq!(parse_subsec_ms("8"), 800); // deciseconds -> ms
        assert_eq!(parse_subsec_ms("8510"), 851); // excess digits dropped
        assert_eq!(parse_subsec_ms(""), 0);
        assert_eq!(parse_subsec_ms("  "), 0);
        assert_eq!(parse_subsec_ms("12 "), 120); // trailing junk after digits
    }
}
