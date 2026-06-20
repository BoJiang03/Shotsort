# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`shotsort` — a Rust CLI that **moves** photos/videos out of a camera card's
`DCIM/` into capture-date folders (`<dest>/{YYYY}/{YYYY}-{MM}-{DD}/`) **on the
same card**, using atomic renames. The repo directory is `CameraTidy`, but the
crate/binary name is `shotsort`. The default action is a destructive MOVE on
single-copy, fragile media — correctness and the safety invariant below matter
more than features.

## Commands

```bash
cargo build                 # debug build -> target/debug/shotsort
cargo build --release       # release (lto+strip) -> target/release/shotsort
cargo test                  # 11 unit (in-module) + 4 integration (tests/cli.rs)
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
   `--dest` subtree (anti-recursion: never re-scan moved files) and any
   camera-managed dir. Returns all recognized media + `.xmp` sidecars.

3. **`plan.rs`** — the brain. Multi-pass, pure except for reading files for
   dates/hashes:
   - Group by `(parent, normalized_stem)` so a RAW + JPEG + `.xmp` of one shot
     stay together (same folder, same new name). Sidecar stems strip a trailing
     media extension (`IMG.ARW.xmp` → groups with `IMG`).
   - Resolve each group's date via the `--date-source` policy, then
     `--on-missing-date` (skip / mtime / `NoDate/`).
   - Assign a per-folder chronological `{counter}`.
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

## Gotchas specific to this code

- **Forbidden-zone checks are relative to the card root**, not the whole path.
  A Sony card mounts at `/Volumes/SONY`, and `SONY` is also a managed-dir name —
  scanning the full absolute path for managed-dir components would falsely flag
  the volume itself. `guard.rs` computes the common ancestor of source and dest
  and only checks components *below* it. Keep this when editing guards.
- **Camera-managed dirs** (`PRIVATE`, `MP_ROOT`, `M4ROOT`, `AVF_INFO`, `MISC`,
  `SONY`) and `DCIM` are never scanned, written into, or cleaned. The list lives
  in `filetype.rs::ADMIN_DIRS`.
- **Dates are local wall-clock.** EXIF `DateTimeOriginal` is used as-is (no UTC
  shifting, so the day never moves). Video `mvhd` time is UTC-since-1904 and is
  converted to local (or a fixed `--tz-offset`); `datesrc.rs` has a hand-rolled
  MP4/MOV box parser for both v0 (32-bit) and v1 (64-bit) `mvhd`.
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
