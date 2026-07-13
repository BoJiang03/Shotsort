# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`shotsort` — a Rust CLI that **moves** photos/videos out of a camera card's
`DCIM/` into capture-date folders (`<dest>/{YYYY}/{YYYY}-{MM}-{DD}/`) **on the
same card**, using atomic renames. The repo directory is `CameraTidy`, but the
crate/binary name is `shotsort`. The default action is a destructive MOVE on
single-copy, fragile media — correctness and the safety invariant below matter
more than features.

Two modes (`--mode`): **`photo`** (default) MOVES stills/clips out of `DCIM`;
**`video`** COPIES camcorder clips (Sony XAVC `M4ROOT/CLIP`, AVCHD `STREAM`) out
of the camera-managed dirs *without ever deleting an original* — point SOURCE at
the **card root**, not `DCIM`. `RunConfig::is_copy()` (`--copy` OR `--link` OR
video mode) is the single source of truth for "keep the source"; use it, not the
raw `cfg.copy` flag, for any move-vs-keep branching. `--link` swaps the
copy/move for a **relative** symlink (`Action::Link`, `engine::relative_path`) —
a no-duplication, Mac-only browsable view; the relative target is what survives
the card being renamed.

There is also a **`match` subcommand** (separate from the organize pipeline):
`shotsort match --project "<PixCake name>" --raw-src <Organized-root> --out <dir>`.
It reverse-engineers the workflow of PixCake (像素蛋糕) tethered shooting, where
low-res preview JPEGs land on the Mac while the camera writes RAW-only to the
card. It reads the project's own SQLite DB (`pixcake.rs`) to learn which previews
the model *kept* (rows with `inRecycleBin = 0`), then pairs each keeper with its
RAW by an **exact capture-time match** — `DateTimeOriginal` + `SubSecTimeOriginal`
to the millisecond (`datesrc::exif_capture_key`), which the camera writes
identically to a shot's JPEG and its RAW, so it is unique even within a 30fps
burst. Matched RAWs are copied (default; also `--action move|link`) into `--out`
via the same `engine`+`journal` path, so `undo` works. `matchcmd.rs` reads keeper
keys first, then scans only the archive day-folders those keepers fall on (a shot
can only match a RAW filed under its own capture date) — important because reading
EXIF off a card reader is slow. See "The match subcommand" below.

## Commands

```bash
cargo build                 # debug build -> target/debug/shotsort
cargo build --release       # release (lto+strip) -> target/release/shotsort
cargo test                  # 21 unit (in-module) + 7 integration (tests/cli.rs)
cargo test --test cli       # integration tests only
cargo test <name>           # single test, e.g. cargo test missing_date_goes_to_nodate
cargo clippy --all-targets -- -D warnings   # must stay warning-clean
cargo fmt                   # rustfmt (edition 2024)
cargo run -- /path/DCIM --dest /path/Organized --dry-run   # safe preview
```

After deps are fetched once, prefer `cargo build --offline` / `cargo test --offline`.

Integration tests (`tests/cli.rs`) drive the built binary via `CARGO_BIN_EXE_shotsort`
against throwaway temp "card" dirs, using `--date-source mtime` + `touch -t` for
deterministic dates (no real EXIF/video fixtures needed). Add new end-to-end
behavior there; add pure-logic tests in the relevant module's `#[cfg(test)]`.

## The safety invariant (do not break)

> At any instant and any interruption point, every file is wholly at its source
> OR its destination — never half-written, never neither.

Everything in `engine.rs` exists to preserve this. Same-filesystem moves are a
single `std::fs::rename` (atomic, no data copied). The only code path that ever
deletes a source is the **cross-filesystem fallback**, and only *after* a BLAKE3
hash of the copy matches. If you touch the move engine, preserve this property.

## Architecture (data flow)

The pipeline is `scan → plan → execute`, with one resolved config threaded through:

1. **`config.rs`** — `RunConfig::resolve(cli)` merges CLI args over an optional
   `shotsort.toml` over built-in defaults. CLI option fields are `Option<T>` so
   "unset" is distinguishable; defaults are filled here, not in clap. Everything
   downstream takes `&RunConfig`. Note: the config file is only auto-loaded from
   the **current working directory** (`./shotsort.toml`), else via `--config`.

2. **`scan.rs`** — `walkdir` over the source. `filter_entry` excludes the entire
   `--dest` subtree (anti-recursion: never re-scan moved files) and hidden/system
   dirs. In **photo** mode it skips every camera-managed dir and returns all
   recognized media + `.xmp` sidecars; in **video** mode it instead descends
   *into* the managed video containers but skips the aux trees
   (`filetype::VIDEO_AUX_DIRS`) and returns video files only.

3. **`plan.rs`** — the brain. Multi-pass, pure except for reading files for
   dates/hashes:
   - Group by `(parent, normalized_stem)` so a RAW + JPEG + `.xmp` of one shot
     stay together (same folder, same new name). Sidecar stems strip a trailing
     media extension (`IMG.ARW.xmp` → groups with `IMG`).
   - Resolve each group's date via the `--date-source` policy, then
     `--on-missing-date` (skip / mtime / `NoDate/`).
   - Assign a per-folder chronological `{counter}` (0-based, numbered by
     capture order). It is seeded past the highest counter-named file already in
     the dest folder (`plan::existing_counter_max`), so incremental re-runs
     *continue* the sequence (`…0005, 0006`) instead of restarting at `0000` and
     colliding — this is what makes `name_template = "{counter:04}"` safe for the
     shoot-a-batch-then-run-again workflow.
   - Emit per-file `PlanItem`s, resolving dedup + conflicts against both existing
     files on disk and an in-plan "claimed" set. On-disk collisions obey
     `--dedup`; in-plan name clashes (two distinct sources, same target name)
     always go through `--on-conflict` so a real photo is never silently dropped.

4. **`engine.rs`** — executes one `PlanItem` (rename, or cross-fs copy→fsync→
   hash-verify→delete). **`journal.rs`** appends one JSONL line per committed
   move (flushed immediately) for resume + undo. **`undo.rs`** reverses the
   journal (`dst → src`) in reverse order.

5. **`main.rs`** — wires it together: validate, scan, build plan, print preview,
   confirm (unless `--yes`), execute with a progress bar, optionally clean
   emptied source dirs, write manifest. Returns a non-zero exit code if any file
   errored (errors are collected, not fatal per-file).

Supporting: `guard.rs` (path safety), `filetype.rs` (extension → `FileKind`,
managed-dir list), `template.rs` (folder/name token rendering), `datesrc.rs`
(date extraction), `types.rs` (shared types), `util.rs` (hashing, sizes).

### The match subcommand (`pixcake.rs` + `matchcmd.rs`)

A **separate pipeline** from organize, dispatched in `main.rs` alongside `undo`.
It does not scan `DCIM` or move anything on the card by default — it *gathers*
the RAWs behind a PixCake selection:

1. **`pixcake.rs`** reads PixCake's on-disk SQLite (`<data>/db/base.db` for the
   project-name→id map; `<data>/db/user_<uid>/project_<pid>/project.db` for the
   images). It always reads a **temp snapshot copy** of the `.db` (+ `-wal`/`-shm`)
   so a running PixCake is never locked. Kept set = `thumbnail` rows with
   `inRecycleBin = 0` (removing a photo in-app only flips this flag; the file on
   disk is left untouched, so folder contents are *not* the source of truth).
2. **`matchcmd.rs`** reads each keeper's millisecond `CaptureKey`
   (`datesrc::exif_capture_key` = `DateTimeOriginal` + `SubSecTimeOriginal`)
   *first*, derives the set of capture **dates**, then indexes only the RAWs in
   the archive day-folders matching those dates (±1 day) — a shot can only match a
   RAW filed under its own capture date, and this avoids reading EXIF off the
   whole (slow) card. Join is exact key first, second-level fallback only when a
   side lacks sub-second. A `claimed` set makes same-timestamp collisions
   deterministic; truly ambiguous / unmatched keepers are reported, never guessed.
3. Matched RAWs are gathered into `--out` via `engine::perform` + `journal`
   (`--action copy` default; `move`/`link` available), so `undo` reverses it.

## Gotchas specific to this code

- **Forbidden-zone checks are relative to the card root**, not the whole path.
  A Sony card mounts at `/Volumes/SONY`, and `SONY` is also a managed-dir name —
  scanning the full absolute path for managed-dir components would falsely flag
  the volume itself. `guard.rs` computes the common ancestor of source and dest
  and only checks components *below* it. Keep this when editing guards.
- **Camera-managed dirs** (`PRIVATE`, `MP_ROOT`, `M4ROOT`, `AVF_INFO`, `MISC`,
  `SONY`) and `DCIM` are never scanned, written into, or cleaned in photo mode.
  The list lives in `filetype.rs::ADMIN_DIRS`. **Video mode is the one exception
  to "never scan managed dirs"**: it reads clips out of `M4ROOT/CLIP` etc., but
  still only ever *copies* (never moves/deletes/cleans them), so the camera's
  database stays intact. It also relaxes the `validate_dest` "dest inside source"
  guard (video SOURCE is the card root, so the dest naturally sits below it; the
  scan already excludes the dest subtree).
- **Dates are local wall-clock.** EXIF `DateTimeOriginal` is used as-is (no UTC
  shifting, so the day never moves). Video `mvhd` time is UTC-since-1904 and is
  converted to local (or a fixed `--tz-offset`); `datesrc.rs` has a hand-rolled
  MP4/MOV box parser for both v0 (32-bit) and v1 (64-bit) `mvhd`.
- **`kamadak-exif` reads the ENTIRE file for TIFF-based RAW** (Sony ARW is TIFF):
  its `read_from_container` does `read_to_end` once it sees the TIFF magic, so a
  naive per-file EXIF read drags all ~40MB of pixel data off the card. The EXIF
  tags live in the first tens of KB, so `datesrc::read_exif` parses a **256KB
  header prefix** and only falls back to the full container if that prefix has no
  datetime. This is what makes the `match` archive scan seconds, not minutes —
  keep it if you touch EXIF reading. (JPEG is already cheap: kamadak extracts just
  the APP1 Exif segment.)
- **Default name template `{original}`** preserves RAW/JPEG pairing for free;
  changing naming defaults can split pairs — keep pairs sharing one base name.
- The journal is append-only across runs, so `undo` rolls back **all** recorded
  moves in that journal; use a per-run `--journal` path for per-run rollback.

## Constraints when changing behavior

- Always exclude the `--dest` subtree from scanning, and never produce a target
  inside `DCIM` or a managed dir.
- Keep `cargo clippy -- -D warnings` clean and the edition-2024 style (let-chains,
  `matches!`) that clippy enforces here.
- Moves run serially on purpose (one journal checkpoint per file); `--jobs` is
  currently accepted but advisory.
