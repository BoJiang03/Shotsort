//! `shotsort match` — pair a PixCake project's kept photos with their RAW files.
//!
//! Flow: read the project's kept previews from PixCake's database
//! ([`crate::pixcake`]), read each keeper's millisecond capture key
//! ([`crate::datesrc::exif_capture_key`]), index every RAW under `--raw-src` by
//! the same key, then join them. A shot's RAW and the tethered JPEG carry an
//! identical `DateTimeOriginal` + `SubSecTimeOriginal`, so an exact key match is
//! the same exposure — precise enough to separate 30fps bursts. Matched RAWs are
//! copied (or moved/linked) into `--out`, recorded in a journal for `undo`.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, Utc};
use indicatif::{ProgressBar, ProgressStyle};

use crate::cli::{MatchAction, VerifyArg};
use crate::config::JOURNAL_BASENAME;
use crate::datesrc;
use crate::filetype;
use crate::journal::{Journal, JournalEntry};
use crate::pixcake;
use crate::types::{Action, CaptureKey};

/// Parameters for a `match` run (mirrors the CLI subcommand fields).
pub struct MatchArgs {
    pub project: String,
    pub raw_src: PathBuf,
    pub out: PathBuf,
    pub pixcake_dir: Option<PathBuf>,
    pub action: MatchAction,
    pub dry_run: bool,
    pub journal: Option<PathBuf>,
    pub yes: bool,
    pub quiet: bool,
}

/// How one keeper resolved against the RAW archive.
enum Resolution {
    /// Exactly one RAW matched. `coarse` = matched only at second precision.
    Matched { raw: PathBuf, coarse: bool },
    /// Several RAWs share this keeper's timestamp — cannot disambiguate safely.
    Ambiguous(Vec<PathBuf>),
    /// A RAW matched, but a previous keeper already claimed it.
    Duplicate(PathBuf),
    /// No RAW in the archive carries this keeper's capture time.
    Unmatched,
}

struct Keeper {
    original: PathBuf,
    resolution: Resolution,
}

pub fn run(args: MatchArgs) -> Result<i32> {
    let data_dir = match &args.pixcake_dir {
        Some(d) => d.clone(),
        None => pixcake::default_data_dir()
            .context("cannot determine PixCake data dir (set $HOME or pass --pixcake-dir)")?,
    };

    let project_db = pixcake::find_project(&data_dir, &args.project)?;
    if !args.quiet {
        println!("Project {:?}\n  db: {}", args.project, project_db.display());
    }

    let kept = pixcake::kept_items(&project_db)?;
    if kept.is_empty() {
        println!("Project has no kept photos (all removed / empty). Nothing to do.");
        return Ok(0);
    }
    if !args.quiet {
        println!("  {} kept photo(s) in the project", kept.len());
    }

    // Read each keeper's capture key first — this touches only the kept files, so
    // it is cheap even off a slow card, and it tells us which day-folders the
    // keepers fall on. A shot can only match a RAW filed under its own capture
    // date, so we can skip every other day-folder in the archive.
    let keeper_keys: Vec<KeeperKey> = kept.iter().map(keeper_key).collect();
    let needed_dates = needed_dates(&keeper_keys);

    // Index the relevant RAWs under raw-src by their millisecond capture key.
    let (index, total_seen) = build_raw_index(&args.raw_src, &needed_dates, args.quiet)?;
    if total_seen == 0 {
        bail!("no RAW files found under {}", args.raw_src.display());
    }

    // Resolve each keeper to a RAW using its precomputed key.
    let mut claimed: HashSet<PathBuf> = HashSet::new();
    let mut keepers: Vec<Keeper> = Vec::with_capacity(kept.len());
    for (item, kk) in kept.iter().zip(&keeper_keys) {
        let resolution = resolve_key(kk, &index, &mut claimed);
        keepers.push(Keeper {
            original: item.original.clone(),
            resolution,
        });
    }

    // Build the gather list from the matched keepers.
    let mut plan: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut dst_claimed: HashSet<PathBuf> = HashSet::new();
    let mut matched = 0usize;
    let mut coarse = 0usize;
    for k in &keepers {
        if let Resolution::Matched { raw, coarse: c } = &k.resolution {
            matched += 1;
            if *c {
                coarse += 1;
            }
            let dst = plan_dst(&args.out, raw, &mut dst_claimed);
            if let Some(dst) = dst {
                plan.push((raw.clone(), dst));
            }
        }
    }

    print_preview(&keepers, matched, coarse, plan.len(), &args);

    if args.dry_run {
        return Ok(0);
    }
    if plan.is_empty() {
        println!("\nNothing to gather.");
        return Ok(if unmatched_count(&keepers) > 0 { 1 } else { 0 });
    }
    if !args.yes && !confirm("\nProceed? [y/N] ")? {
        println!("Cancelled.");
        return Ok(0);
    }

    let journal_path = args
        .journal
        .clone()
        .unwrap_or_else(|| args.out.join(JOURNAL_BASENAME));
    let errors = execute(&plan, args.action, &journal_path, args.quiet)?;

    let exit_bad = errors > 0 || unmatched_count(&keepers) > 0;
    Ok(if exit_bad { 1 } else { 0 })
}

/// RAW files under `raw_src` indexed by capture time, both at full millisecond
/// precision and collapsed to the second (for keepers lacking sub-second data).
struct RawIndex {
    by_key: HashMap<CaptureKey, Vec<PathBuf>>,
    by_second: HashMap<NaiveDateTime, Vec<PathBuf>>,
}

/// Build the RAW index, restricted to day-folders in `needed_dates`. Returns the
/// index and the total number of RAW files seen (before date filtering) so the
/// caller can tell "wrong --raw-src" (0 files) apart from "no matching dates".
fn build_raw_index(
    raw_src: &Path,
    needed_dates: &HashSet<NaiveDate>,
    quiet: bool,
) -> Result<(RawIndex, usize)> {
    if !raw_src.exists() {
        bail!("--raw-src not found: {}", raw_src.display());
    }
    let (paths, total_seen) = collect_raw_paths(raw_src, needed_dates);
    if !quiet {
        let skipped = total_seen.saturating_sub(paths.len());
        if skipped > 0 {
            println!(
                "Reading capture times from {} RAW file(s) ({} in other date folders skipped) ...",
                paths.len(),
                skipped
            );
        } else {
            println!("Reading capture times from {} RAW file(s) ...", paths.len());
        }
    }

    let progress = if quiet {
        ProgressBar::hidden()
    } else {
        let pb = ProgressBar::new(paths.len() as u64);
        pb.set_style(
            ProgressStyle::with_template("{bar:40.cyan/blue} {pos}/{len} {wide_msg}")
                .unwrap()
                .progress_chars("=>-"),
        );
        pb
    };

    let mut by_key: HashMap<CaptureKey, Vec<PathBuf>> = HashMap::new();
    let mut by_second: HashMap<NaiveDateTime, Vec<PathBuf>> = HashMap::new();
    for p in paths {
        if let Some(key) = datesrc::exif_capture_key(&p) {
            by_second.entry(key.dt).or_default().push(p.clone());
            by_key.entry(key).or_default().push(p);
        }
        progress.inc(1);
    }
    progress.finish_and_clear();
    Ok((RawIndex { by_key, by_second }, total_seen))
}

/// Collect RAW paths under `root`. A file inside a `YYYY-MM-DD` folder is kept
/// only if that date is needed; files in non-date folders are always kept (we
/// can't safely date-filter them). Returns `(kept, total_raw_seen)`.
fn collect_raw_paths(root: &Path, needed_dates: &HashSet<NaiveDate>) -> (Vec<PathBuf>, usize) {
    use walkdir::WalkDir;
    let mut out = Vec::new();
    let mut total = 0usize;
    for entry in WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| !is_hidden(e.file_name().to_str()))
        .filter_map(|e| e.ok())
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let ext = entry
            .path()
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase());
        if let Some(ext) = ext
            && filetype::classify(&ext) == Some(crate::types::FileKind::Raw)
        {
            total += 1;
            let keep = match folder_date(entry.path()) {
                Some(d) => needed_dates.contains(&d),
                None => true, // unknown folder → can't filter, scan it
            };
            if keep {
                out.push(entry.into_path());
            }
        }
    }
    (out, total)
}

/// Parse a file's immediate parent folder name as a `YYYY-MM-DD` date, if it is
/// one (shotsort files RAWs into these per-capture-date folders).
fn folder_date(file: &Path) -> Option<NaiveDate> {
    let parent = file.parent()?.file_name()?.to_str()?;
    NaiveDate::parse_from_str(parent, "%Y-%m-%d").ok()
}

fn is_hidden(name: Option<&str>) -> bool {
    name.map(|n| n.starts_with('.') && n != "." && n != "..")
        .unwrap_or(false)
}

/// A keeper's capture time, read once up front.
enum KeeperKey {
    /// Exact millisecond key from the keeper file's own EXIF (burst-safe).
    Precise(CaptureKey),
    /// Second-only fallback from PixCake's stored time (file unreadable/gone).
    Coarse(NaiveDateTime),
    /// No capture time available at all.
    None,
}

fn keeper_key(item: &pixcake::KeptItem) -> KeeperKey {
    if let Some(key) = datesrc::exif_capture_key(&item.original) {
        return KeeperKey::Precise(key);
    }
    if let Some(ms) = item.capture_time_ms
        && let Some(dt) = local_naive_from_millis(ms)
    {
        return KeeperKey::Coarse(dt);
    }
    KeeperKey::None
}

/// The set of capture dates the keepers fall on, widened by ±1 day to absorb any
/// midnight/timezone edge between a keeper's key and the archive's folder date.
fn needed_dates(keys: &[KeeperKey]) -> HashSet<NaiveDate> {
    let mut set = HashSet::new();
    for k in keys {
        let d = match k {
            KeeperKey::Precise(key) => Some(key.dt.date()),
            KeeperKey::Coarse(dt) => Some(dt.date()),
            KeeperKey::None => None,
        };
        if let Some(d) = d {
            set.insert(d);
            if let Some(p) = d.pred_opt() {
                set.insert(p);
            }
            if let Some(n) = d.succ_opt() {
                set.insert(n);
            }
        }
    }
    set
}

/// Resolve a keeper (via its precomputed key) against the RAW index.
fn resolve_key(kk: &KeeperKey, index: &RawIndex, claimed: &mut HashSet<PathBuf>) -> Resolution {
    match kk {
        KeeperKey::Precise(key) => pick(
            index.by_key.get(key),
            index.by_second.get(&key.dt),
            false,
            claimed,
        ),
        KeeperKey::Coarse(dt) => pick(None, index.by_second.get(dt), true, claimed),
        KeeperKey::None => Resolution::Unmatched,
    }
}

/// Choose a RAW from the precise-key bucket, else the second-level bucket,
/// honoring the already-claimed set. `coarse` flags a second-only fallback.
fn pick(
    exact: Option<&Vec<PathBuf>>,
    second: Option<&Vec<PathBuf>>,
    coarse: bool,
    claimed: &mut HashSet<PathBuf>,
) -> Resolution {
    if let Some(v) = exact {
        return commit(v, coarse, claimed);
    }
    if let Some(v) = second {
        return commit(v, true, claimed);
    }
    Resolution::Unmatched
}

fn commit(candidates: &[PathBuf], coarse: bool, claimed: &mut HashSet<PathBuf>) -> Resolution {
    let free: Vec<&PathBuf> = candidates
        .iter()
        .filter(|p| !claimed.contains(*p))
        .collect();
    match (candidates.len(), free.as_slice()) {
        (_, []) => Resolution::Duplicate(candidates[0].clone()),
        (1, [only]) => {
            claimed.insert((*only).clone());
            Resolution::Matched {
                raw: (*only).clone(),
                coarse,
            }
        }
        (_, [only]) => {
            // Only one free candidate among several sharing the timestamp.
            claimed.insert((*only).clone());
            Resolution::Matched {
                raw: (*only).clone(),
                coarse,
            }
        }
        _ => Resolution::Ambiguous(candidates.to_vec()),
    }
}

/// Choose a collision-free destination path in `out` for a RAW. If a same-named,
/// same-size file is already there, returns `None` (already gathered).
fn plan_dst(out: &Path, raw: &Path, dst_claimed: &mut HashSet<PathBuf>) -> Option<PathBuf> {
    let name = raw.file_name().unwrap_or_default();
    let first = out.join(name);
    if !dst_claimed.contains(&first)
        && let (Ok(a), Ok(b)) = (std::fs::metadata(&first), std::fs::metadata(raw))
        && a.len() == b.len()
    {
        return None; // already present with matching size — skip
    }

    let stem = raw.file_stem().and_then(|s| s.to_str()).unwrap_or("file");
    let ext = raw.extension().and_then(|e| e.to_str());
    let mut n = 1u32;
    loop {
        let candidate = if n == 1 {
            out.join(name)
        } else {
            let fname = match ext {
                Some(e) => format!("{stem}_{n}.{e}"),
                None => format!("{stem}_{n}"),
            };
            out.join(fname)
        };
        if !dst_claimed.contains(&candidate) && !candidate.exists() {
            dst_claimed.insert(candidate.clone());
            return Some(candidate);
        }
        n += 1;
    }
}

fn execute(
    plan: &[(PathBuf, PathBuf)],
    action: MatchAction,
    journal_path: &Path,
    quiet: bool,
) -> Result<usize> {
    std::fs::create_dir_all(journal_path.parent().unwrap_or(Path::new("."))).ok();
    let mut journal = Journal::open_append(journal_path)?;
    let start = Instant::now();

    let (engine_action, op) = match action {
        MatchAction::Copy => (Action::Copy, "copy"),
        MatchAction::Move => (Action::Move, "move"),
        MatchAction::Link => (Action::Link, "link"),
    };

    let progress = if quiet {
        ProgressBar::hidden()
    } else {
        let pb = ProgressBar::new(plan.len() as u64);
        pb.set_style(
            ProgressStyle::with_template("{bar:40.cyan/blue} {pos}/{len} {wide_msg}")
                .unwrap()
                .progress_chars("=>-"),
        );
        pb
    };

    let mut errors = 0usize;
    let mut done = 0usize;
    for (src, dst) in plan {
        progress.set_message(
            dst.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default(),
        );
        match crate::engine::perform(src, dst, engine_action, VerifyArg::Auto) {
            Ok(outcome) => {
                done += 1;
                journal.append(&JournalEntry {
                    op: op.to_string(),
                    src: src.to_string_lossy().to_string(),
                    dst: dst.to_string_lossy().to_string(),
                    ts: Utc::now().to_rfc3339(),
                    bytes: outcome.bytes,
                })?;
                progress.inc(1);
            }
            Err(e) => {
                errors += 1;
                progress.println(format!("error: {} : {e:#}", src.display()));
            }
        }
    }
    progress.finish_and_clear();

    if !quiet {
        let verb = match action {
            MatchAction::Copy => "Copied",
            MatchAction::Move => "Moved",
            MatchAction::Link => "Linked",
        };
        println!(
            "\n{verb} {done} RAW file(s) in {:.1}s.\n  journal: {}",
            start.elapsed().as_secs_f64(),
            journal_path.display()
        );
    }
    Ok(errors)
}

fn print_preview(
    keepers: &[Keeper],
    matched: usize,
    coarse: usize,
    to_gather: usize,
    args: &MatchArgs,
) {
    let action = match args.action {
        MatchAction::Copy => "copy",
        MatchAction::Move => "move",
        MatchAction::Link => "link",
    };
    println!("\nMatch summary");
    println!("  kept photos : {}", keepers.len());
    println!("  matched RAW : {matched}");
    if coarse > 0 {
        println!("    (of these, {coarse} matched at second precision only)");
    }
    println!("  to {action:<9}: {to_gather}  ->  {}", args.out.display());

    let unmatched: Vec<&Keeper> = keepers
        .iter()
        .filter(|k| matches!(k.resolution, Resolution::Unmatched))
        .collect();
    let ambiguous: Vec<&Keeper> = keepers
        .iter()
        .filter(|k| matches!(k.resolution, Resolution::Ambiguous(_)))
        .collect();
    let duplicate: Vec<&Keeper> = keepers
        .iter()
        .filter(|k| matches!(k.resolution, Resolution::Duplicate(_)))
        .collect();

    if !unmatched.is_empty() {
        println!(
            "  unmatched   : {} (no RAW with this capture time)",
            unmatched.len()
        );
        for k in unmatched.iter().take(20) {
            println!("      {}", short(&k.original));
        }
        if unmatched.len() > 20 {
            println!("      ... and {} more", unmatched.len() - 20);
        }
    }
    if !ambiguous.is_empty() {
        println!(
            "  ambiguous   : {} (several RAWs share the timestamp)",
            ambiguous.len()
        );
        for k in &ambiguous {
            if let Resolution::Ambiguous(c) = &k.resolution {
                println!(
                    "      {}  ->  {}",
                    short(&k.original),
                    c.iter().map(|p| short(p)).collect::<Vec<_>>().join(", ")
                );
            }
        }
    }
    if !duplicate.is_empty() {
        println!(
            "  duplicate   : {} (keeper resolves to an already-claimed RAW)",
            duplicate.len()
        );
        for k in &duplicate {
            if let Resolution::Duplicate(raw) = &k.resolution {
                println!("      {}  ->  {}", short(&k.original), short(raw));
            }
        }
    }
}

fn unmatched_count(keepers: &[Keeper]) -> usize {
    keepers
        .iter()
        .filter(|k| {
            matches!(
                k.resolution,
                Resolution::Unmatched | Resolution::Ambiguous(_)
            )
        })
        .count()
}

fn local_naive_from_millis(ms: i64) -> Option<NaiveDateTime> {
    let utc: DateTime<Utc> = DateTime::from_timestamp_millis(ms)?;
    Some(utc.with_timezone(&Local).naive_local())
}

fn short(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| p.to_string_lossy().to_string())
}

fn confirm(prompt: &str) -> Result<bool> {
    print!("{prompt}");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(matches!(
        line.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn commit_matches_single_free_candidate() {
        let mut claimed = HashSet::new();
        let r = commit(&[p("/raw/a.ARW")], false, &mut claimed);
        assert!(matches!(r, Resolution::Matched { coarse: false, .. }));
        // The RAW is now claimed, so a second keeper hitting it is a duplicate.
        let r2 = commit(&[p("/raw/a.ARW")], false, &mut claimed);
        assert!(matches!(r2, Resolution::Duplicate(_)));
    }

    #[test]
    fn commit_flags_true_ambiguity() {
        // Two distinct RAWs sharing one timestamp, none claimed -> ambiguous.
        let mut claimed = HashSet::new();
        let r = commit(&[p("/raw/a.ARW"), p("/raw/b.ARW")], false, &mut claimed);
        assert!(matches!(r, Resolution::Ambiguous(v) if v.len() == 2));
    }

    #[test]
    fn commit_picks_the_only_free_of_several() {
        // A burst pair where one frame was already claimed by an earlier keeper:
        // the second keeper deterministically takes the remaining free RAW.
        let mut claimed = HashSet::new();
        claimed.insert(p("/raw/a.ARW"));
        let r = commit(&[p("/raw/a.ARW"), p("/raw/b.ARW")], false, &mut claimed);
        match r {
            Resolution::Matched { raw, .. } => assert_eq!(raw, p("/raw/b.ARW")),
            _ => panic!("expected a match to the free RAW"),
        }
    }

    #[test]
    fn plan_dst_disambiguates_same_named_raws() {
        // Two different day folders can both hold `0005.ARW` (counter naming).
        let out = std::env::temp_dir().join(format!("shotsort-dsttest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&out);
        let mut claimed = HashSet::new();

        let d1 = plan_dst(&out, &p("/card/2026-07-12/0005.ARW"), &mut claimed).unwrap();
        let d2 = plan_dst(&out, &p("/card/2026-07-13/0005.ARW"), &mut claimed).unwrap();
        assert_eq!(d1, out.join("0005.ARW"));
        assert_eq!(d2, out.join("0005_2.ARW"));
        assert_ne!(d1, d2);
    }
}
