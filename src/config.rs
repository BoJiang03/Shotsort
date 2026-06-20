//! TOML config file + merge with CLI into a fully-resolved [`RunConfig`].
//!
//! Precedence: CLI option > config file value > built-in default.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::FixedOffset;
use serde::Deserialize;

use crate::cli::{
    Cli, DateSourceArg, DedupArg, OnConflictArg, OnMissingDateArg, TypeSel, VerifyArg,
};

/// Raw shape of the optional `shotsort.toml` file. Every field is optional so
/// it can be partially specified.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileConfig {
    pub dest: Option<PathBuf>,
    pub types: Option<Vec<String>>,
    pub ext: Option<Vec<String>>,
    pub folder_template: Option<String>,
    pub name_template: Option<String>,
    pub date_source: Option<String>,
    pub on_missing_date: Option<String>,
    pub on_conflict: Option<String>,
    pub dedup: Option<String>,
    pub verify: Option<String>,
    pub clean_empty_dirs: Option<bool>,
    pub journal: Option<PathBuf>,
    pub jobs: Option<usize>,
    pub tz_offset: Option<String>,
}

impl FileConfig {
    fn load(path: &Path) -> Result<FileConfig> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading config {}", path.display()))?;
        let cfg: FileConfig =
            toml::from_str(&text).with_context(|| format!("parsing config {}", path.display()))?;
        Ok(cfg)
    }
}

/// Final, resolved configuration for an organize run.
#[derive(Debug, Clone)]
pub struct RunConfig {
    pub source: PathBuf,
    pub dest: PathBuf,
    pub dry_run: bool,
    pub copy: bool,
    /// `None` means "every recognized media kind".
    pub kinds: Option<Vec<TypeSel>>,
    pub ext_whitelist: Option<Vec<String>>,
    pub folder_template: String,
    pub name_template: String,
    pub date_source: DateSourceArg,
    pub on_missing_date: OnMissingDateArg,
    pub on_conflict: OnConflictArg,
    pub dedup: DedupArg,
    pub verify: VerifyArg,
    pub clean_empty_dirs: bool,
    pub journal: PathBuf,
    pub jobs: usize,
    pub manifest: Option<PathBuf>,
    pub tz_offset: Option<FixedOffset>,
    pub yes: bool,
    pub quiet: bool,
    pub verbose: bool,
}

pub const DEFAULT_FOLDER_TEMPLATE: &str = "{YYYY}/{YYYY}-{MM}-{DD}";
pub const DEFAULT_NAME_TEMPLATE: &str = "{original}";
pub const JOURNAL_BASENAME: &str = ".shotsort-journal.jsonl";

impl RunConfig {
    /// Build the resolved config from parsed CLI args + optional config file.
    pub fn resolve(cli: &Cli) -> Result<RunConfig> {
        // Load config file: an explicit --config must exist; the default path
        // is used only when present.
        let file_cfg = match &cli.config {
            Some(p) => FileConfig::load(p)?,
            None => {
                let default = Path::new("shotsort.toml");
                if default.exists() {
                    FileConfig::load(default)?
                } else {
                    FileConfig::default()
                }
            }
        };

        let source = cli
            .source
            .clone()
            .context("missing SOURCE directory (e.g. /Volumes/SONY/DCIM)")?;

        let dest = cli
            .dest
            .clone()
            .or(file_cfg.dest.clone())
            .context("missing --dest (in-card destination root)")?;

        let kinds = resolve_kinds(cli, &file_cfg)?;
        let ext_whitelist = cli.ext.clone().or(file_cfg.ext.clone()).map(|v| {
            v.into_iter()
                .map(|e| e.trim_start_matches('.').to_ascii_lowercase())
                .collect()
        });

        let folder_template = cli
            .folder_template
            .clone()
            .or(file_cfg.folder_template.clone())
            .unwrap_or_else(|| DEFAULT_FOLDER_TEMPLATE.to_string());

        let name_template = cli
            .name_template
            .clone()
            .or(file_cfg.name_template.clone())
            .unwrap_or_else(|| DEFAULT_NAME_TEMPLATE.to_string());

        let date_source = cli
            .date_source
            .or_else(|| file_cfg.date_source.as_deref().and_then(parse_date_source))
            .unwrap_or(DateSourceArg::Exif);

        let on_missing_date = cli
            .on_missing_date
            .or_else(|| {
                file_cfg
                    .on_missing_date
                    .as_deref()
                    .and_then(parse_on_missing)
            })
            .unwrap_or(OnMissingDateArg::UnknownFolder);

        let on_conflict = cli
            .on_conflict
            .or_else(|| file_cfg.on_conflict.as_deref().and_then(parse_on_conflict))
            .unwrap_or(OnConflictArg::Rename);

        let dedup = cli
            .dedup
            .or_else(|| file_cfg.dedup.as_deref().and_then(parse_dedup))
            .unwrap_or(DedupArg::Name);

        let verify = cli
            .verify
            .or_else(|| file_cfg.verify.as_deref().and_then(parse_verify))
            .unwrap_or(VerifyArg::Auto);

        let clean_empty_dirs = cli.clean_empty_dirs || file_cfg.clean_empty_dirs.unwrap_or(false);

        let journal = cli
            .journal
            .clone()
            .or(file_cfg.journal.clone())
            .unwrap_or_else(|| dest.join(JOURNAL_BASENAME));

        let jobs = cli.jobs.or(file_cfg.jobs).unwrap_or(1).max(1);

        let tz_offset = match cli.tz_offset.clone().or(file_cfg.tz_offset.clone()) {
            Some(s) => Some(parse_tz_offset(&s)?),
            None => None,
        };

        Ok(RunConfig {
            source,
            dest,
            dry_run: cli.dry_run,
            copy: cli.copy,
            kinds,
            ext_whitelist,
            folder_template,
            name_template,
            date_source,
            on_missing_date,
            on_conflict,
            dedup,
            verify,
            clean_empty_dirs,
            journal,
            jobs,
            manifest: cli.manifest.clone(),
            tz_offset,
            yes: cli.yes,
            quiet: cli.quiet,
            verbose: cli.verbose,
        })
    }
}

fn resolve_kinds(cli: &Cli, file_cfg: &FileConfig) -> Result<Option<Vec<TypeSel>>> {
    let from_cli = cli.types.clone();
    let from_file = file_cfg
        .types
        .as_ref()
        .map(|v| v.iter().map(|s| parse_type(s)).collect::<Result<Vec<_>>>())
        .transpose()?;

    let sel = match from_cli.or(from_file) {
        Some(v) => v,
        None => return Ok(None), // default: all
    };
    if sel.contains(&TypeSel::All) {
        Ok(None)
    } else {
        Ok(Some(sel))
    }
}

fn parse_type(s: &str) -> Result<TypeSel> {
    Ok(match s.trim().to_ascii_lowercase().as_str() {
        "raw" => TypeSel::Raw,
        "jpeg" | "jpg" => TypeSel::Jpeg,
        "video" => TypeSel::Video,
        "all" => TypeSel::All,
        other => anyhow::bail!("unknown type in config: {other}"),
    })
}

fn parse_date_source(s: &str) -> Option<DateSourceArg> {
    match s.trim().to_ascii_lowercase().as_str() {
        "exif" => Some(DateSourceArg::Exif),
        "mtime" => Some(DateSourceArg::Mtime),
        "exif-then-mtime" => Some(DateSourceArg::ExifThenMtime),
        _ => None,
    }
}

fn parse_on_missing(s: &str) -> Option<OnMissingDateArg> {
    match s.trim().to_ascii_lowercase().as_str() {
        "skip" => Some(OnMissingDateArg::Skip),
        "mtime" => Some(OnMissingDateArg::Mtime),
        "unknown-folder" => Some(OnMissingDateArg::UnknownFolder),
        _ => None,
    }
}

fn parse_on_conflict(s: &str) -> Option<OnConflictArg> {
    match s.trim().to_ascii_lowercase().as_str() {
        "rename" => Some(OnConflictArg::Rename),
        "skip" => Some(OnConflictArg::Skip),
        "overwrite" => Some(OnConflictArg::Overwrite),
        _ => None,
    }
}

fn parse_dedup(s: &str) -> Option<DedupArg> {
    match s.trim().to_ascii_lowercase().as_str() {
        "name" => Some(DedupArg::Name),
        "hash" => Some(DedupArg::Hash),
        "off" => Some(DedupArg::Off),
        _ => None,
    }
}

fn parse_verify(s: &str) -> Option<VerifyArg> {
    match s.trim().to_ascii_lowercase().as_str() {
        "auto" => Some(VerifyArg::Auto),
        "size" => Some(VerifyArg::Size),
        "hash" => Some(VerifyArg::Hash),
        "off" => Some(VerifyArg::Off),
        _ => None,
    }
}

/// Parse `+08:00` / `-0530` / `Z` into a [`FixedOffset`].
fn parse_tz_offset(s: &str) -> Result<FixedOffset> {
    let s = s.trim();
    if s.eq_ignore_ascii_case("z") || s == "+00:00" || s == "0" {
        return Ok(FixedOffset::east_opt(0).unwrap());
    }
    let (sign, rest) = match s.as_bytes().first() {
        Some(b'+') => (1, &s[1..]),
        Some(b'-') => (-1, &s[1..]),
        _ => anyhow::bail!("tz-offset must start with + or - (got {s:?})"),
    };
    let digits: String = rest.chars().filter(|c| c.is_ascii_digit()).collect();
    let (h, m) = match digits.len() {
        2 => (digits.parse::<i32>()?, 0),
        4 => (digits[..2].parse::<i32>()?, digits[2..].parse::<i32>()?),
        _ => anyhow::bail!("tz-offset must look like +08:00 or -0530 (got {s:?})"),
    };
    let secs = sign * (h * 3600 + m * 60);
    FixedOffset::east_opt(secs).context("tz-offset out of range")
}
