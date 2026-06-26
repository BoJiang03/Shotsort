//! Turn scanned files into a fully-decided move/copy plan.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use chrono::NaiveDateTime;

use crate::cli::{DateSourceArg, DedupArg, OnConflictArg, OnMissingDateArg};
use crate::config::RunConfig;
use crate::datesrc;
use crate::filetype::{classify, kind_selected};
use crate::template::{self, Ctx, sanitize_component};
use crate::types::{Action, DateProvenance, FileKind, MediaFile, PlanItem};
use crate::util::{hash_file, systemtime_to_local_naive};

/// One shot's worth of files that must stay together (same folder, same base
/// name): a RAW/JPEG pair, optionally with an `.xmp` sidecar.
struct Group {
    files: Vec<MediaFile>,
    date: Option<NaiveDateTime>,
    provenance: DateProvenance,
    make: String,
    model: String,
    /// Destination folder relative to dest (`2026/2026-06-20` or `NoDate`).
    folder: String,
    use_name_template: bool,
    /// Whole-group skip (no date + on-missing-date=skip).
    skip_no_date: bool,
    rep_stem: String,
    sort_key: NaiveDateTime,
    counter: u32,
}

/// Build the plan. Pure aside from reading files for date/hash extraction.
pub fn build(files: Vec<MediaFile>, cfg: &RunConfig) -> Vec<PlanItem> {
    let groups = group_files(files, cfg);
    let mut groups = assign_dates(groups, cfg);
    assign_counters(&mut groups);
    emit_items(groups, cfg)
}

/// Group by (parent, normalized stem) so RAW+JPEG+XMP of one shot stay together.
fn group_files(files: Vec<MediaFile>, cfg: &RunConfig) -> Vec<Group> {
    use std::collections::BTreeMap;

    // Key keeps groups deterministic across runs.
    let mut map: BTreeMap<(PathBuf, String), Vec<MediaFile>> = BTreeMap::new();
    for f in files {
        let norm_stem = normalized_stem(&f);
        map.entry((f.parent.clone(), norm_stem))
            .or_default()
            .push(f);
    }

    let mut groups = Vec::new();
    for ((_, _), members) in map {
        // A group is processed only if at least one *selected* primary exists.
        let has_selected_primary = members
            .iter()
            .any(|m| m.kind != FileKind::Sidecar && is_selected(m, cfg));
        if !has_selected_primary {
            continue;
        }
        // Drop primaries that are not selected (e.g. JPEG when types=raw), but
        // keep sidecars to follow the surviving primary.
        let kept: Vec<MediaFile> = members
            .into_iter()
            .filter(|m| m.kind == FileKind::Sidecar || is_selected(m, cfg))
            .collect();

        let rep_stem = kept
            .iter()
            .find(|m| m.kind != FileKind::Sidecar)
            .map(|m| m.stem.clone())
            .unwrap_or_default();

        groups.push(Group {
            files: kept,
            date: None,
            provenance: DateProvenance::None,
            make: String::new(),
            model: String::new(),
            folder: String::new(),
            use_name_template: false,
            skip_no_date: false,
            rep_stem,
            sort_key: NaiveDateTime::default(),
            counter: 0,
        });
    }
    groups
}

/// True if a primary file passes the type / ext filter. Sidecars always pass.
fn is_selected(m: &MediaFile, cfg: &RunConfig) -> bool {
    if let Some(white) = &cfg.ext_whitelist {
        return white.iter().any(|w| w.eq_ignore_ascii_case(&m.ext));
    }
    kind_selected(m.kind, &cfg.kinds)
}

/// Strip a trailing media extension from a sidecar stem so `IMG.ARW.xmp`
/// groups with `IMG.ARW` (→ key stem `IMG`).
fn normalized_stem(f: &MediaFile) -> String {
    if f.kind == FileKind::Sidecar
        && let Some((base, ext)) = f.stem.rsplit_once('.')
        && classify(&ext.to_ascii_lowercase())
            .map(|k| k != FileKind::Sidecar)
            .unwrap_or(false)
    {
        return base.to_string();
    }
    f.stem.clone()
}

/// Resolve each group's capture date, folder, and naming mode.
fn assign_dates(mut groups: Vec<Group>, cfg: &RunConfig) -> Vec<Group> {
    for g in &mut groups {
        // Pick the best primary for embedded metadata.
        let mut embedded: Option<NaiveDateTime> = None;
        let mut make = String::new();
        let mut model = String::new();
        let mut primary_mtime: Option<NaiveDateTime> = None;

        for f in g.files.iter().filter(|m| m.kind != FileKind::Sidecar) {
            if primary_mtime.is_none() {
                primary_mtime = f.mtime.map(systemtime_to_local_naive);
            }
            if embedded.is_none() {
                let info = datesrc::extract(&f.path, f.kind, cfg.tz_offset);
                if let Some(dt) = info.datetime {
                    embedded = Some(dt);
                }
                if make.is_empty() {
                    make = info.make.unwrap_or_default();
                }
                if model.is_empty() {
                    model = info.model.unwrap_or_default();
                }
            }
        }
        g.make = make;
        g.model = model;

        // Apply the date-source policy.
        let (date, provenance) = match cfg.date_source {
            DateSourceArg::Mtime => (primary_mtime, DateProvenance::Mtime),
            DateSourceArg::Exif => (embedded, DateProvenance::Embedded),
            DateSourceArg::ExifThenMtime => match embedded {
                Some(d) => (Some(d), DateProvenance::Embedded),
                None => (primary_mtime, DateProvenance::Mtime),
            },
        };

        match date {
            Some(d) => {
                g.date = Some(d);
                g.provenance = provenance;
                g.use_name_template = cfg.name_template != crate::config::DEFAULT_NAME_TEMPLATE;
                g.folder = render_folder(cfg, Some(d), &g.make, &g.model, &g.rep_stem);
                g.sort_key = d;
            }
            None => match cfg.on_missing_date {
                OnMissingDateArg::Skip => {
                    g.skip_no_date = true;
                    g.folder = "NoDate".to_string();
                }
                OnMissingDateArg::Mtime => match primary_mtime {
                    Some(d) => {
                        g.date = Some(d);
                        g.provenance = DateProvenance::Mtime;
                        g.use_name_template =
                            cfg.name_template != crate::config::DEFAULT_NAME_TEMPLATE;
                        g.folder = render_folder(cfg, Some(d), &g.make, &g.model, &g.rep_stem);
                        g.sort_key = d;
                    }
                    None => {
                        g.folder = "NoDate".to_string();
                        g.use_name_template = false;
                    }
                },
                OnMissingDateArg::UnknownFolder => {
                    g.folder = "NoDate".to_string();
                    g.use_name_template = false;
                }
            },
        }
    }
    groups
}

fn render_folder(
    cfg: &RunConfig,
    date: Option<NaiveDateTime>,
    make: &str,
    model: &str,
    rep_stem: &str,
) -> String {
    let ctx = Ctx {
        dt: date,
        original: rep_stem,
        ext: "",
        counter: None,
        make,
        model,
    };
    let rendered = template::render(&cfg.folder_template, &ctx);
    rendered
        .split('/')
        .filter(|s| !s.is_empty())
        .map(sanitize_component)
        .collect::<Vec<_>>()
        .join("/")
}

/// Assign a per-folder chronological counter (used by `{counter}` templates).
fn assign_counters(groups: &mut [Group]) {
    let mut order: Vec<usize> = (0..groups.len()).collect();
    order.sort_by(|&a, &b| {
        groups[a]
            .folder
            .cmp(&groups[b].folder)
            .then(groups[a].sort_key.cmp(&groups[b].sort_key))
            .then(groups[a].rep_stem.cmp(&groups[b].rep_stem))
    });
    let mut current_folder = String::new();
    let mut n = 0u32;
    for idx in order {
        if groups[idx].folder != current_folder {
            current_folder = groups[idx].folder.clone();
            n = 0;
        }
        n += 1;
        groups[idx].counter = n;
    }
}

/// Expand groups into per-file plan items, resolving dedup/conflicts.
fn emit_items(mut groups: Vec<Group>, cfg: &RunConfig) -> Vec<PlanItem> {
    // Deterministic processing order keeps conflict numbering stable.
    groups.sort_by(|a, b| {
        a.folder
            .cmp(&b.folder)
            .then(a.sort_key.cmp(&b.sort_key))
            .then(a.rep_stem.cmp(&b.rep_stem))
    });

    let mut claimed: HashSet<String> = HashSet::new();
    let mut items = Vec::new();

    for g in &groups {
        let folder_path = if g.folder.is_empty() {
            cfg.dest.clone()
        } else {
            let mut p = cfg.dest.clone();
            for part in g.folder.split('/') {
                p.push(part);
            }
            p
        };

        for f in order_group_files(&g.files) {
            if g.skip_no_date {
                items.push(skip_item(
                    f,
                    Action::SkipNoDate,
                    &g.folder,
                    "no capture date",
                ));
                continue;
            }

            let filename = target_filename(f, g, cfg);
            let candidate = folder_path.join(&filename);

            let item = resolve_target(f, g, cfg, candidate, &mut claimed);
            items.push(item);
        }
    }
    items
}

/// Primaries first (RAW, JPEG, video), then sidecars — stable within a group.
fn order_group_files(files: &[MediaFile]) -> Vec<&MediaFile> {
    let mut v: Vec<&MediaFile> = files.iter().collect();
    v.sort_by_key(|f| {
        let rank = match f.kind {
            FileKind::Raw => 0,
            FileKind::Jpeg => 1,
            FileKind::Video => 2,
            FileKind::Sidecar => 3,
        };
        (rank, f.ext.to_ascii_lowercase(), f.stem.clone())
    });
    v
}

/// Compute the target file name (without the directory).
fn target_filename(f: &MediaFile, g: &Group, cfg: &RunConfig) -> String {
    if !g.use_name_template {
        return format!("{}.{}", f.stem, f.ext);
    }
    let ctx = Ctx {
        dt: g.date,
        original: &f.stem,
        ext: &f.ext.to_ascii_lowercase(),
        counter: Some(g.counter),
        make: &g.make,
        model: &g.model,
    };
    let mut base = sanitize_component(&template::render(&cfg.name_template, &ctx));
    if base.is_empty() || base == "_" {
        base = f.stem.clone();
    }
    format!("{base}.{}", f.ext)
}

/// Decide the final action for one file against existing files + in-plan claims.
fn resolve_target(
    f: &MediaFile,
    g: &Group,
    cfg: &RunConfig,
    candidate: PathBuf,
    claimed: &mut HashSet<String>,
) -> PlanItem {
    let move_action = if cfg.link {
        Action::Link
    } else if cfg.is_copy() {
        Action::Copy
    } else {
        Action::Move
    };

    // Defensive: the candidate is built from dest + sanitized components, so it
    // must stay under dest. If it somehow does not, skip rather than escape.
    if !candidate.starts_with(&cfg.dest) {
        return PlanItem {
            src: f.path.clone(),
            dst: candidate,
            kind: f.kind,
            size: f.size,
            date: g.date,
            provenance: g.provenance,
            action: Action::SkipConflict,
            rel_folder: g.folder.clone(),
            note: "target would escape the destination root".to_string(),
        };
    }

    let key = claim_key(&candidate);
    let on_disk = candidate.exists();
    let in_plan = claimed.contains(&key);

    if !on_disk && !in_plan {
        claimed.insert(key);
        return move_item(f, g, candidate, move_action, "");
    }

    // Collision. On-disk collisions respect dedup; in-plan collisions (two
    // distinct sources -> same name) always go through on-conflict so a real
    // photo is never silently dropped as a "duplicate".
    if on_disk {
        match cfg.dedup {
            DedupArg::Name => {
                return skip_item(
                    f,
                    Action::SkipDuplicate,
                    &g.folder,
                    "same name already at target",
                );
            }
            DedupArg::Hash => {
                if files_identical(&f.path, &candidate) {
                    return skip_item(
                        f,
                        Action::SkipDuplicate,
                        &g.folder,
                        "identical content already at target",
                    );
                }
                // different content -> fall through to conflict handling
            }
            DedupArg::Off => { /* always a conflict */ }
        }
    }

    // Conflict (different content, or in-plan name clash).
    match cfg.on_conflict {
        OnConflictArg::Skip => {
            skip_item(f, Action::SkipConflict, &g.folder, "target exists (skip)")
        }
        OnConflictArg::Overwrite => {
            claimed.insert(claim_key(&candidate));
            move_item(
                f,
                g,
                candidate,
                Action::Overwrite,
                "overwriting existing target",
            )
        }
        OnConflictArg::Rename => {
            let renamed = next_free_name(&candidate, claimed);
            claimed.insert(claim_key(&renamed));
            move_item(f, g, renamed, move_action, "renamed to avoid conflict")
        }
    }
}

fn move_item(f: &MediaFile, g: &Group, dst: PathBuf, action: Action, note: &str) -> PlanItem {
    PlanItem {
        src: f.path.clone(),
        dst,
        kind: f.kind,
        size: f.size,
        date: g.date,
        provenance: g.provenance,
        action,
        rel_folder: g.folder.clone(),
        note: note.to_string(),
    }
}

fn skip_item(f: &MediaFile, action: Action, folder: &str, note: &str) -> PlanItem {
    PlanItem {
        src: f.path.clone(),
        dst: PathBuf::new(),
        kind: f.kind,
        size: f.size,
        date: None,
        provenance: DateProvenance::None,
        action,
        rel_folder: folder.to_string(),
        note: note.to_string(),
    }
}

/// Find `base_NNN.ext` that exists neither on disk nor in the plan.
fn next_free_name(candidate: &Path, claimed: &HashSet<String>) -> PathBuf {
    let parent = candidate.parent().unwrap_or(Path::new("."));
    let stem = candidate
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("file");
    let ext = candidate.extension().and_then(|s| s.to_str());

    for n in 1..100_000u32 {
        let name = match ext {
            Some(e) => format!("{stem}_{n:03}.{e}"),
            None => format!("{stem}_{n:03}"),
        };
        let p = parent.join(name);
        if !p.exists() && !claimed.contains(&claim_key(&p)) {
            return p;
        }
    }
    // Pathological fallback.
    parent.join(format!("{stem}_{}", std::process::id()))
}

fn claim_key(p: &Path) -> String {
    // Cards are FAT/exFAT (case-insensitive); compare lowercased.
    p.to_string_lossy().to_ascii_lowercase()
}

fn files_identical(a: &Path, b: &Path) -> bool {
    match (hash_file(a), hash_file(b)) {
        (Ok(ha), Ok(hb)) => ha == hb,
        _ => false,
    }
}
