# shotsort

**In-card photo & video organizer.** Move media out of a camera card's
`DCIM/` jungle into readable, capture-date folders — *on the same card*, using
atomic renames. No copying to a computer, no database, no fuss.

```
<card>/DCIM/100MSDCF/DSC00001.ARW   →   <card>/Organized/2026/2026-06-20/DSC00001.ARW
```

---

## ⚠️ Read this first — this is not a backup

shotsort **moves** files. After a run, the organized result lives **only on
that one card**. A single in-camera format, or a card failure, loses
everything.

- This is a **destructive + single-copy + fragile-media** combination. shotsort
  uses atomic renames and a journal to make "losing a file mid-operation"
  essentially impossible, **but that is not a substitute for a backup.**
- Back the whole card up to a computer or drive at least occasionally.
- Once files leave `DCIM/`, the camera's playback can no longer see them and
  may show an "Image Database File error". **This is expected.**

If you want a copy on your computer instead of an in-card move, use `--copy`.

---

## What makes it safe

- **Atomic moves.** On one filesystem, every file is moved with a single
  `rename(2)`. At any instant — and any interruption point — each file is
  *wholly* at its old path or its new path, never half-written, never gone.
- **Never invents deletions.** A source file only "disappears" because a rename
  put it somewhere else. The only path that ever deletes is the cross-filesystem
  fallback, and only **after** a byte-for-byte hash check passes.
- **Anti-recursion.** The destination subtree is excluded from the scan, so
  freshly-moved files are never picked up and moved again.
- **Forbidden zones.** It refuses to write into `DCIM/` or any camera-managed
  directory (`PRIVATE`, `MP_ROOT`, `M4ROOT`, `AVF_INFO`, `MISC`, `SONY`), and
  never scans them.
- **Preview + journal + undo.** `--dry-run` shows the full plan without touching
  anything; every committed move is appended to a journal; `undo` rolls a whole
  run back.

---

## Install

Requires a Rust toolchain (`rustup`).

```bash
git clone <your-repo-url> shotsort
cd shotsort
cargo build --release
# binary at target/release/shotsort
```

## Usage

```bash
# 1) Preview the plan, move nothing
shotsort /Volumes/SONY/DCIM --dest /Volumes/SONY/Organized --dry-run

# 2) Organize in place (the default action is MOVE)
shotsort /Volumes/SONY/DCIM --dest /Volumes/SONY/Organized

# 3) Move + rename to readable names + clean out emptied DCIM subfolders
shotsort /Volumes/SONY/DCIM --dest /Volumes/SONY/Organized \
  --name-template "{YYYY}{MM}{DD}_{HH}{mm}{ss}" --clean-empty-dirs

# 4) Roll back the last run
shotsort undo --journal /Volumes/SONY/Organized/.shotsort-journal.jsonl
```

Before doing anything destructive shotsort prints a plan summary and asks for
confirmation (skip it with `--yes`).

## Two modes: `photo` and `video`

`--mode photo` (the default) is everything above: it scans `DCIM/` and **moves**
stills and clips into date folders.

`--mode video` handles the videos that photo mode deliberately leaves alone.
Camcorder-style footage (Sony XAVC S in `PRIVATE/M4ROOT/CLIP`, AVCHD in
`PRIVATE/AVCHD/.../STREAM`) is **not** a loose file in `DCIM` — each clip is tied
to a camera database, thumbnails, and sidecars. Moving the bare `.MP4` out would
break in-camera playback. So video mode **copies** the master clips out (never
moves, never deletes the originals), into the same date-folder layout:

```bash
# Point SOURCE at the CARD ROOT (not DCIM) and copy clips into date folders.
shotsort /Volumes/SONY --dest /Volumes/SONY/Videos --mode video --dry-run
shotsort /Volumes/SONY --dest /Volumes/SONY/Videos --mode video
```

Proxies, thumbnails, audio and metadata trees (`SUB`, `THMBNL`, `WAV`,
`GENERAL`, `DATABASE`, …) are skipped — only the full-resolution masters are
copied. `undo` on a video run deletes the copies and leaves every original in
place.

### `--link`: a date view without duplicating the data

Add `--link` to make the destination entries **relative symlinks** into the
source instead of byte-for-byte copies — a browsable date view that costs no
extra space:

```bash
shotsort /Volumes/SONY --dest /Volumes/SONY/Videos --mode video --link
```

The links are *relative*, so they keep working when the card is renamed or
remounted. Caveats worth knowing: they are a **macOS-on-exFAT** feature —
Windows, the camera, and other devices see them as broken stubs; and copying the
view elsewhere with a normal tool copies the *links*, not the videos. Use it for
on-Mac browsing, not as a backup. `undo` removes the links and never touches an
original.

## Options

| Option | Default | Meaning |
|---|---|---|
| `--dry-run` | off | Compute and print the plan; touch nothing |
| `--mode <m>` | `photo` | `photo` MOVES from `DCIM`; `video` COPIES camera clips out of managed dirs (point SOURCE at the card root) |
| `--copy` | off | Copy instead of move (keeps the source) |
| `--link` | off | Write relative symlinks instead of copying bytes (Mac-only date view; keeps the source) |
| `--types <list>` | `all` | `raw,jpeg,video` (comma-separated) or `all` |
| `--ext <list>` | — | Explicit extension whitelist; overrides `--types` |
| `--folder-template <TPL>` | `{YYYY}/{YYYY}-{MM}-{DD}` | Destination sub-folder template |
| `--name-template <TPL>` | `{original}` | File-name template |
| `--date-source <src>` | `exif` | `exif` / `mtime` / `exif-then-mtime` |
| `--on-missing-date <m>` | `unknown-folder` | `skip` / `mtime` / `unknown-folder` (→ `NoDate/`) |
| `--on-conflict <c>` | `rename` | Target exists, different content: `rename` / `skip` / `overwrite` |
| `--dedup <d>` | `name` | Duplicate test: `name` / `hash` / `off` |
| `--verify <mode>` | `auto` | `auto` / `size` / `hash` / `off` |
| `--clean-empty-dirs` | off | Remove source subfolders left empty (never managed dirs) |
| `--journal <FILE>` | `<dest>/.shotsort-journal.jsonl` | Journal path (resume + undo) |
| `--manifest <FILE>` | — | Per-file result manifest (`.json` or `.csv`) |
| `--tz-offset <OFF>` | — | Fixed offset (e.g. `+08:00`) for UTC video times |
| `--config <FILE>` | `./shotsort.toml` | Config file (CLI options override it) |
| `--jobs <N>` | 1 | Reserved; transfers currently run serially for safety |
| `--yes` | off | Skip the confirmation prompt |
| `--quiet` / `--verbose` | — | Output detail |

## Templates

Tokens: `{YYYY} {YY} {MM} {DD} {HH} {mm} {ss} {original} {ext} {counter}
{counter:03} {camera_make} {camera_model}`.

- Default folder `{YYYY}/{YYYY}-{MM}-{DD}` → `2026/2026-06-20/`
- Default name `{original}` keeps the original name — the safest choice, and it
  keeps RAW/JPEG pairs together automatically.
- `{counter}` is a per-folder sequence number in capture order; the extension is
  always appended automatically.

## File types

- **RAW**: arw, cr2, cr3, nef, raf, orf, rw2, dng, pef, srw, nrw
- **Image**: jpg, jpeg, heic, heif, tif, tiff, png
- **Video**: mp4, mov, m4v, avi, mts, m2ts
- **Sidecar**: xmp (never processed alone; follows its same-stem primary)

RAW + JPEG with the same stem are grouped so they share a date and land in the
same folder under the same new name.

## Dates & time zones

- **Images**: EXIF `DateTimeOriginal` → `DateTimeDigitized` → `DateTime`, used as
  local wall-clock time (no UTC shifting, so the day never moves).
- **Videos** (MP4/MOV): the `mvhd` creation time, which is stored as UTC seconds
  since 1904. By default it's converted to your system local time; pass
  `--tz-offset` to pin a fixed offset and avoid a day-boundary surprise.

## Config file

`shotsort.toml` in the working directory (or `--config <path>`):

```toml
dest             = "/Volumes/SONY/Organized"
types            = ["raw", "jpeg", "video"]
folder_template  = "{YYYY}/{YYYY}-{MM}-{DD}"
name_template    = "{original}"
on_conflict      = "rename"
dedup            = "name"
jobs             = 1
clean_empty_dirs = false
```

## Exit codes

`0` on a clean run; non-zero if any file errored (each error is reported and the
run continues past it).

## License

MIT (see `LICENSE`).
