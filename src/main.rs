//! shotsort — in-card photo/video organizer using atomic moves.

mod cli;
mod config;
mod datesrc;
mod engine;
mod filetype;
mod guard;
mod journal;
mod plan;
mod report;
mod scan;
mod template;
mod types;
mod undo;
mod util;

use std::io::Write;
use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use chrono::Utc;
use clap::Parser;
use indicatif::{ProgressBar, ProgressStyle};

use cli::{Cli, Command};
use config::RunConfig;
use journal::{Journal, JournalEntry};
use report::{FinalStats, PlanStats};
use types::{Action, PlanItem};

fn main() {
    let cli = Cli::parse();
    let code = match run(cli) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err:#}");
            1
        }
    };
    std::process::exit(code);
}

/// Returns the process exit code (0 = clean, non-zero = errors occurred).
fn run(cli: Cli) -> Result<i32> {
    if let Some(Command::Undo { journal, yes }) = &cli.command {
        let summary = undo::run(journal, *yes, cli.quiet)?;
        if !summary.reverse_journal.as_os_str().is_empty() {
            println!(
                "Undo complete: restored {}, skipped {}, errors {}.\n  reverse journal: {}",
                summary.restored,
                summary.skipped,
                summary.errors,
                summary.reverse_journal.display()
            );
        }
        return Ok(if summary.errors > 0 { 1 } else { 0 });
    }

    organize(cli)
}

fn organize(cli: Cli) -> Result<i32> {
    let cfg = RunConfig::resolve(&cli)?;

    // Normalize and validate the source/dest relationship.
    let source = guard::normalize(&cfg.source);
    let dest = guard::normalize(&cfg.dest);

    let meta = std::fs::metadata(&source)
        .with_context(|| format!("SOURCE not accessible: {}", source.display()))?;
    if !meta.is_dir() {
        bail!("SOURCE is not a directory: {}", source.display());
    }
    guard::validate_dest(&source, &dest, cfg.mode)?;

    // Build the effective config with normalized paths.
    let cfg = RunConfig {
        source: source.clone(),
        dest: dest.clone(),
        ..cfg
    };

    if !cfg.dry_run {
        std::fs::create_dir_all(&dest)
            .with_context(|| format!("creating dest {}", dest.display()))?;
    }

    warn_if_cross_filesystem(&source, &dest);

    if !cfg.quiet {
        println!("Scanning {} ...", source.display());
    }
    let files = scan::scan(&source, &dest, cfg.mode)?;
    if files.is_empty() {
        println!("No recognized media found under {}.", source.display());
        return Ok(0);
    }

    let items = plan::build(files, &cfg);
    let stats = report::summarize(&items);

    report::print_preview(&cfg, &stats, cfg.dry_run);

    if cfg.dry_run {
        if let Some(mp) = &cfg.manifest {
            report::write_manifest(mp, &items)?;
            println!("  manifest written: {}", mp.display());
        }
        return Ok(0);
    }

    if stats.to_move == 0 {
        println!("\nNothing to move.");
        return Ok(0);
    }

    if !cfg.yes && !confirm("\nProceed? [y/N] ")? {
        println!("Cancelled.");
        return Ok(0);
    }

    let final_stats = execute(&cfg, &items, &stats)?;

    if let Some(mp) = &cfg.manifest {
        report::write_manifest(mp, &items)?;
        if !cfg.quiet {
            println!("  manifest written: {}", mp.display());
        }
    }

    Ok(if final_stats.errors > 0 { 1 } else { 0 })
}

fn execute(cfg: &RunConfig, items: &[PlanItem], stats: &PlanStats) -> Result<FinalStats> {
    let start = Instant::now();
    let mut journal = Journal::open_append(&cfg.journal)?;
    let already_done: std::collections::HashSet<String> = journal::completed_sources(&cfg.journal)
        .into_iter()
        .collect();

    let progress = if cfg.quiet {
        ProgressBar::hidden()
    } else {
        let pb = ProgressBar::new(stats.to_move as u64);
        pb.set_style(
            ProgressStyle::with_template("{bar:40.cyan/blue} {pos}/{len} {wide_msg}")
                .unwrap()
                .progress_chars("=>-"),
        );
        pb
    };

    if cfg.jobs > 1 && cfg.verbose {
        println!(
            "note: --jobs {} requested; transfers run serially so every step has a journal checkpoint.",
            cfg.jobs
        );
    }

    let mut final_stats = FinalStats {
        action_verb: if cfg.link {
            "linked"
        } else if cfg.is_copy() {
            "copied"
        } else {
            "moved"
        },
        ..Default::default()
    };

    for item in items {
        if item.action.is_skip() {
            final_stats.skipped += 1;
            continue;
        }

        // Resume: a previously-committed source no longer needs moving.
        let src_key = item.src.to_string_lossy().to_string();
        if already_done.contains(&src_key) || !item.src.exists() {
            if cfg.verbose {
                progress.println(format!("skip (already done): {}", item.src.display()));
            }
            continue;
        }

        if let Err(e) = guard::assert_target_allowed(&item.dst, &cfg.dest) {
            final_stats.errors += 1;
            progress.println(format!("error: {e:#}"));
            continue;
        }

        progress.set_message(short_name(&item.dst));
        match engine::perform(&item.src, &item.dst, item.action, cfg.verify) {
            Ok(outcome) => {
                final_stats.moved += 1;
                final_stats.moved_bytes += outcome.bytes;
                if outcome.crossed_fs && cfg.verbose {
                    progress.println(format!(
                        "note: cross-filesystem fallback used for {}",
                        item.src.display()
                    ));
                }
                if item.rel_folder == "NoDate" {
                    final_stats.no_date += 1;
                }
                let op = match item.action {
                    Action::Copy => "copy",
                    Action::Link => "link",
                    Action::Overwrite => "overwrite",
                    _ => "move",
                };
                journal.append(&JournalEntry {
                    op: op.to_string(),
                    src: item.src.to_string_lossy().to_string(),
                    dst: item.dst.to_string_lossy().to_string(),
                    ts: Utc::now().to_rfc3339(),
                    bytes: outcome.bytes,
                })?;
                progress.inc(1);
            }
            Err(e) => {
                final_stats.errors += 1;
                progress.println(format!("error: {} : {e:#}", item.src.display()));
            }
        }
    }
    progress.finish_and_clear();

    if cfg.clean_empty_dirs && !cfg.is_copy() {
        let removed = clean_empty_dirs(&cfg.source)?;
        if removed > 0 && !cfg.quiet {
            println!("  removed {removed} empty source folder(s)");
        }
    }

    report::print_final(&final_stats, start.elapsed());
    if !cfg.quiet {
        println!("  journal: {}", journal.path().display());
    }
    Ok(final_stats)
}

/// Remove now-empty source subfolders. Never touches the source root itself or
/// any camera-managed directory.
fn clean_empty_dirs(source: &Path) -> Result<usize> {
    use walkdir::WalkDir;
    let source = guard::normalize(source);
    let mut removed = 0usize;

    for entry in WalkDir::new(&source)
        .contents_first(true)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if !entry.file_type().is_dir() {
            continue;
        }
        let p = guard::normalize(entry.path());
        if p == source {
            continue; // never remove the source root (e.g. DCIM)
        }
        if let Some(name) = entry.file_name().to_str()
            && filetype::is_admin_dir(name)
        {
            continue;
        }
        let empty = std::fs::read_dir(&p)
            .map(|mut it| it.next().is_none())
            .unwrap_or(false);
        if empty && std::fs::remove_dir(&p).is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}

/// Warn loudly if source and dest are on different filesystems — the design
/// assumes a single card, where moves are atomic renames. A cross-fs dest
/// silently degrades to copy→verify→delete.
fn warn_if_cross_filesystem(source: &Path, dest: &Path) {
    // Compare against the nearest existing ancestor of dest (it may not exist
    // yet under --dry-run).
    let mut probe = dest;
    while !probe.exists() {
        match probe.parent() {
            Some(p) => probe = p,
            None => return,
        }
    }
    if let Ok(false) = engine::same_filesystem(source, probe) {
        eprintln!(
            "⚠ warning: SOURCE and --dest appear to be on different filesystems.\n  \
             This tool is designed for same-card moves; it will fall back to copy→verify→delete,\n  \
             which is slower and not atomic. Make sure --dest is a folder on the same card."
        );
    }
}

fn short_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default()
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
