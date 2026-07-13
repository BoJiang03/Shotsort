//! Command-line interface definitions (clap derive).

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

/// In-card photo/video organizer: move media out of `DCIM/` into readable,
/// capture-date folders on the *same* card, using atomic renames.
#[derive(Debug, Parser)]
#[command(name = "shotsort", version, about, long_about = None)]
pub struct Cli {
    /// Source directory to scan, usually `<card>/DCIM`.
    #[arg(value_name = "SOURCE")]
    pub source: Option<PathBuf>,

    /// In-card destination root, e.g. `/Volumes/SONY/Organized`.
    /// Must be on the same card and outside DCIM / camera-managed dirs.
    #[arg(short, long, value_name = "DIR")]
    pub dest: Option<PathBuf>,

    /// Compute and print the plan without moving anything.
    #[arg(long)]
    pub dry_run: bool,

    /// Copy instead of move (keeps the source files).
    #[arg(long)]
    pub copy: bool,

    /// Organize by creating a *relative* symlink at the destination instead of
    /// moving/copying bytes (keeps the source; no data duplicated). Intended for
    /// `--mode video`, e.g. a browsable date view of camera clips on the card.
    #[arg(long)]
    pub link: bool,

    /// Organize mode. `photo` (default) MOVES stills/clips out of `DCIM`.
    /// `video` COPIES camera video clips (Sony XAVC `M4ROOT`, AVCHD) out of the
    /// camera-managed dirs, keeping the originals so the camera can still play
    /// them. In `video` mode point SOURCE at the card root, not `DCIM`.
    #[arg(long, value_enum, value_name = "mode")]
    pub mode: Option<ModeArg>,

    /// Import categories: any of raw,jpeg,video or `all`.
    #[arg(long, value_delimiter = ',', value_name = "list")]
    pub types: Option<Vec<TypeSel>>,

    /// Explicit extension whitelist (overrides --types), e.g. `arw,jpg`.
    #[arg(long, value_delimiter = ',', value_name = "list")]
    pub ext: Option<Vec<String>>,

    /// Destination sub-folder template.
    #[arg(long, value_name = "TPL")]
    pub folder_template: Option<String>,

    /// File naming template.
    #[arg(long, value_name = "TPL")]
    pub name_template: Option<String>,

    /// Where capture dates come from.
    #[arg(long, value_enum, value_name = "src")]
    pub date_source: Option<DateSourceArg>,

    /// What to do with files that have no capture date.
    #[arg(long, value_enum, value_name = "m")]
    pub on_missing_date: Option<OnMissingDateArg>,

    /// What to do when the target exists with *different* content.
    #[arg(long, value_enum, value_name = "c")]
    pub on_conflict: Option<OnConflictArg>,

    /// Duplicate detection strategy.
    #[arg(long, value_enum, value_name = "d")]
    pub dedup: Option<DedupArg>,

    /// Post-move verification level.
    #[arg(long, value_enum, value_name = "mode")]
    pub verify: Option<VerifyArg>,

    /// After moving, delete source subfolders left empty (never managed dirs).
    #[arg(long)]
    pub clean_empty_dirs: bool,

    /// Journal path (used for resumable runs and `undo`).
    #[arg(long, value_name = "FILE")]
    pub journal: Option<PathBuf>,

    /// Parallelism. Moves are safest serial (1); copies may go higher.
    #[arg(short, long, value_name = "N")]
    pub jobs: Option<usize>,

    /// Write a per-file result manifest (.json or .csv).
    #[arg(long, value_name = "FILE")]
    pub manifest: Option<PathBuf>,

    /// Config file (CLI options override it).
    #[arg(long, value_name = "FILE")]
    pub config: Option<PathBuf>,

    /// Fixed timezone offset for UTC-sourced video times, e.g. `+08:00`.
    #[arg(long, value_name = "OFF")]
    pub tz_offset: Option<String>,

    /// Skip the interactive confirmation.
    #[arg(short, long)]
    pub yes: bool,

    /// Reduce output.
    #[arg(long, conflicts_with = "verbose")]
    pub quiet: bool,

    /// Increase output.
    #[arg(long)]
    pub verbose: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Roll back a previous organize run using its journal.
    Undo {
        /// Journal file written by the original run.
        #[arg(long, value_name = "FILE")]
        journal: PathBuf,
        /// Skip the interactive confirmation.
        #[arg(short, long)]
        yes: bool,
    },

    /// Match a PixCake project's kept photos to their RAW files and gather them.
    ///
    /// PixCake tethered shooting stores low-res preview JPEGs; the RAWs stay on
    /// the card. This reads the project's own database to learn which previews
    /// were kept (not moved to the project recycle bin), then finds each keeper's
    /// RAW by an exact capture-time match (`DateTimeOriginal` + `SubSecTimeOriginal`,
    /// to the millisecond — robust to 30fps bursts) and copies those RAWs to `--out`.
    Match {
        /// PixCake project name exactly as shown in the app, e.g. `My Shoot`.
        #[arg(long, value_name = "NAME")]
        project: String,

        /// Root folder holding the organized RAWs, scanned recursively across all
        /// date folders, e.g. `/Volumes/Picture/Organized`.
        #[arg(long, value_name = "DIR")]
        raw_src: PathBuf,

        /// Output folder to gather the matched RAWs into.
        #[arg(long, value_name = "DIR")]
        out: PathBuf,

        /// PixCake data directory (default:
        /// `~/Library/Application Support/PixCake-qt_pro`).
        #[arg(long, value_name = "DIR")]
        pixcake_dir: Option<PathBuf>,

        /// How to gather matched RAWs. `copy` (default) keeps the archive intact;
        /// `move` pulls them out (leaving gaps); `link` makes relative symlinks.
        #[arg(long, value_enum, default_value = "copy", value_name = "act")]
        action: MatchAction,

        /// Compute and print the matches without gathering anything.
        #[arg(long)]
        dry_run: bool,

        /// Journal path (default: `<out>/.shotsort-journal.jsonl`), for `undo`.
        #[arg(long, value_name = "FILE")]
        journal: Option<PathBuf>,

        /// Skip the interactive confirmation.
        #[arg(short, long)]
        yes: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum MatchAction {
    /// Copy matched RAWs to the output (keeps the archived originals).
    Copy,
    /// Move matched RAWs to the output (removes them from the archive).
    Move,
    /// Create relative symlinks at the output (no data copied).
    Link,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum ModeArg {
    /// Move stills/clips out of `DCIM` (default).
    #[default]
    Photo,
    /// Copy camera video clips out of managed dirs, keeping originals.
    Video,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum TypeSel {
    Raw,
    Jpeg,
    Video,
    All,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum DateSourceArg {
    Exif,
    Mtime,
    ExifThenMtime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OnMissingDateArg {
    Skip,
    Mtime,
    UnknownFolder,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OnConflictArg {
    Rename,
    Skip,
    Overwrite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum DedupArg {
    Name,
    Hash,
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum VerifyArg {
    Auto,
    Size,
    Hash,
    Off,
}
