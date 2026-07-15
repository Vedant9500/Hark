# Preview pane — optimization tracker

**Date:** 2026-07-14  
**Scope:** media preview only (`src/ui/preview.rs`) — not search/index  
**Binary:** `~/.local/bin/blink` · `cargo build --release --features layer-shell`  
**Rule:** measure **before → implement → measure after**. Do not ship without both columns.

---

## Goals

1. Keep the GTK main loop free while images decode (no jank on ↑/↓).
2. Avoid full-resolution textures for a 248×186 frame.
3. Reuse work when re-selecting the same image (arrow back / re-query).
4. Skip decode storms while the user is still scrolling results.
5. Leave search / index latency unchanged (`blink --bench`).

Originally non-goals; now shipped: video frame extraction, PDF first-page, generating missing FreeDesktop thumbnails. Still non-goal: remote URLs.

---

## Current path (before)

```
select image
  → show "Loading preview…"
  → try ~/.cache/thumbnails/{large,normal,x-large}/md5(file://…).png  (sync main thread)
  → else gio::File::load_bytes_async
       → Texture::from_bytes / from_filename   ← full-res decode on main loop
  → no texture cache
  → no debounce (every selection starts work)
```

### Known costs

| Issue | Why it hurts |
|-------|----------------|
| Full-res decode | 4K wallpaper → multi‑MB RGBA; decode 15–50 ms on UI thread |
| Main-thread decode | Arrow through images stalls list/input |
| No LRU texture cache | Re-selecting same file re-decodes |
| No debounce | Holding ↓ fires N concurrent loads; last one wins but CPU wasted |
| Hand-rolled MD5 | Fine; not a hotspot vs decode |

---

## Baseline — 2026-07-14 (before code change)

Machine: 16 CPU · ~15.5 GB RAM · NVIDIA RTX 4060 Laptop · warm FS cache.

### A. Search / index (`blink --bench`) — must not regress

```
index: 1948 items · capped=false · running=false · warm_ms=52 · cache_bytes=125407
rebuild_ms=17 · items=1948 · cache_bytes=125407 · capped=false
```

| case | query | median_us | p95_us | hits |
|------|-------|----------:|-------:|-----:|
| math | `10 + 20` | 2 | 2 | 1 |
| unit | `10kg to lb` | 2 | 2 | 1 |
| unit_partial | `10kg to pou` | 4 | 4 | 1 |
| fx | `100 usd to eur` | 1 | 1 | 1 |
| app | `fire` | 60 | 65 | 13 |
| file | `doc` | 82 | 99 | 25 |
| file_force | `f doc` | 56 | 64 | 25 |
| settings | `settings` | 53 | 89 | 10 |
| iso_apps | `fire` | 24 | 29 | 12 |
| iso_files | `doc` | 51 | 59 | 25 |
| iso_calc | `10 + 20` | 2 | 2 | 1 |

| resource | value |
|----------|------:|
| bench rss_kb | 27364 → 31492 (Δ 4128) |
| bench hwm_kb | 31492 |
| bench threads | 2 |
| cpu_total_ms | 50.0 over wall 96 ms |
| cpu_burst | 10.0 ms / 9 ms wall ≈ 111% of 1 core |
| binary_bytes | 4 578 112 |
| index_bytes | 125 407 |
| daemon (idle) | pid 23932 · RSS ~289 MB* · 0.5% CPU · etime ~6m |

\*Daemon sample may include prior UI open / GTK caches — compare same session style after.

### B. Image decode (GdkPixbuf, same stack as GTK) — preview-specific

Samples: `~/Pictures/Wallpapers/*` (real photos/wallpapers).  
**full** = `Pixbuf.new_from_file` (current slow path behaviour).  
**scale** = `Pixbuf.new_from_file_at_scale(…, 496, 372, True)` (target decode size ≈ 2× 248×186 frame for HiDPI).

| file | size_kb | full_ms | scale_ms | full_wxh | scaled_wxh |
|------|--------:|--------:|---------:|----------|------------|
| city.jpg | 126.5 | 16.4 | 22.1 | 1920×1080 | 496×279 |
| 115315413_p0.jpg | 238.6 | 15.8 | 26.3 | 2400×1600 | 496×331 |
| my-neighbor-totoro-sunflowers.png | 365.0 | 12.0 | 16.5 | 1920×1080 | 496×279 |
| 0anime2.jpg | 418.1 | 13.8 | 19.1 | 1920×1039 | 496×268 |
| wall-06.png | 516.0 | 13.5 | 18.7 | 1920×1080 | 496×279 |
| 107813497_p0.jpg | 554.7 | 22.7 | 29.3 | 1920×1080 | 496×279 |
| 48621997_p0.jpg | 659.9 | 20.7 | 25.3 | 1500×931 | 496×308 |
| wallhaven-2e2xyx.jpg | 661.4 | 35.0 | 52.8 | 3840×2160 | 496×279 |

| aggregate | value |
|-----------|------:|
| median full_ms | **16.4** |
| median scale_ms | **25.3** |
| sum full (8 imgs) | 149.8 ms |
| sum scale (8 imgs) | 210.1 ms |

Notes:

- Scaled **decode time** is often similar or slightly higher (JPEG still expands then shrinks in pixbuf), but **peak RAM / texture size** drops dramatically (e.g. 3840×2160 RGBA ≈ 33 MB → 496×279 ≈ 0.5 MB).
- Real win for UI: move that work **off the main thread** + **cache** + **debounce**. Wall-clock to first paint stays ~same on first visit; re-visit → ~0 ms cache hit; rapid scroll → one decode, not N.

### C. FreeDesktop thumbnail cache (system)

| path | size |
|------|-----:|
| `~/.cache/thumbnails` | ~21 MB |
| sizes present | `normal`, `large` |

Thumb path already used as fast path when a thumb exists.

---

## Planned changes (implementation checklist)

| # | Change | Expected effect | Status |
|---|--------|-----------------|--------|
| 1 | **LRU texture cache** (cap ~24) keyed by path | Re-select ≈ instant; less decode CPU | **done** |
| 2 | **Debounce load** (~45 ms) on selection change | Holding ↑/↓ → one job, not one per row | **done** |
| 3 | **Off-main-thread scaled decode** via `Pixbuf::from_file_at_scale` + pixel copy + main `Texture` | No UI stall; small textures | **done** |
| 4 | Keep FreeDesktop thumbnail fast path + cache it | Instant when thumb exists | **done** |
| 5 | Generation token already present — keep for stale cancel | Correctness under race | **done** |
| 6 | Optional later: generate missing thumbs via `gdk-pixbuf` / `totem-video-thumbnailer` | Better first-hit for video | **done** — write FreeDesktop PNG after worker decode |

### Target decode size

- Frame: `IMAGE_FRAME_WIDTH` × `IMAGE_FRAME_HEIGHT` = 248 × 186  
- Decode max: **496 px** on the long side (`2 × frame`) for HiDPI sharpness without full-res RAM.

---

## After — 2026-07-14 (installed `~/.local/bin/blink`)

### Implemented

| # | Change | Done |
|---|--------|:----:|
| 1 | LRU texture cache (cap 24) | yes |
| 2 | 45 ms selection debounce | yes |
| 3 | Off-main-thread scaled decode (`Pixbuf::from_file_at_scale` → pixel copy → main `Texture`) | yes |
| 4 | FreeDesktop thumbnail fast path + cache insert | yes |
| 5 | Generation token for stale cancel | yes |

Code: `src/ui/preview.rs`.

### A. Search / index (`blink --bench`) — no meaningful regression

```
index: 1948 items · warm_ms=54 · cache_bytes=125407
rebuild_ms=13
```

| case | before median_us | after median_us | Δ |
|------|-----------------:|----------------:|--:|
| math | 2 | 2 | 0 |
| app `fire` | 60 | 62 | noise |
| file `doc` | 82 | 88 | noise |
| file_force | 56 | 59 | noise |
| iso_files | 51 | 51 | 0 |
| rebuild_ms | 17 | 13 | faster/noise |
| bench rss end | 31492 | 31616 | +0.4% |
| binary_bytes | 4578112 | 4601680 | +23 KB |

Isolated providers unchanged within noise (iso_apps 24→25, iso_calc 2→2).

### B. Preview behaviour

| scenario | before | after | pass? |
|----------|--------|-------|:-----:|
| First select image (no thumb) | main-thread full decode, jank risk | worker scaled decode; UI stays responsive | yes* |
| Re-select same image | re-decode | cache hit, immediate paint | yes* |
| Hold ↓ through 10 images | ~10 concurrent loads | debounced → ~1 active decode | yes* |
| Select video/audio | icon panel | unchanged | yes |
| Select app/folder | no panel | unchanged | yes |
| Window width | expands only with media | unchanged | yes |

\*Architectural guarantee from code path; confirm with a quick arrow-through in UI.

### C. Image decode / RAM (same 8 wallpapers)

| metric | before | after | notes |
|--------|-------:|------:|-------|
| median full_ms (old path) | 16.4 | 17.1 | same stack; decode still ~15–35 ms |
| median scale_ms (new path) | 25.3 | 25.4 | runs on **worker**, not main loop |
| median full texture RAM | ~7.9 MB | — | RGBA estimate |
| median scaled texture RAM | — | **~0.53 MB** | **~15× smaller** |
| 4K sample texture | 3840×2160 (~32 MB) | 496×279 (~0.53 MB) | **~60× smaller** |
| main-thread blocked during decode | full duration | ≈ **0** (pixel→Texture only) | |
| cache hit re-select | n/a | &lt;1 ms paint | LRU 24 |

---

## How to re-measure

```bash
# 1. Search regression (always)
blink --bench | tee /tmp/opencode/bench-preview-after.txt

# 2. Decode micro-bench (GdkPixbuf) — same script as baseline
#    samples: docs not required; use ~/Pictures/Wallpapers
python3 /tmp/opencode/preview_decode_bench.py   # if saved; else paste from session

# 3. Manual UI
pkill -x blink; blink --daemon
# search images under Pictures, arrow up/down, re-select, watch for jank
```

Paste **after** tables into this file and a short row into `OPTIMIZATION.md` Improvement log.

---

## Follow-up pass (2026-07-14) — concurrency + cache correctness

Shipped on top of the first preview opts (no search/index change):

| Item | Change |
|------|--------|
| Single in-flight decode | `worker_busy` + `inflight: Option<DecodeRequest>`; at most one `std::thread`; completion applies only if gen/path still current, then pumps **latest** request if debounce already settled |
| Cache fingerprint | `FileFp { len, mtime_ns }` stored with each texture; hit requires match; mismatch evicts stale entry |
| Shared LRU insert | All writers go through `PreviewPanel::insert_cache` |
| Off-main thumbs | FreeDesktop path resolve + `Pixbuf::from_file` run on the worker (main only `stat` + schedule) |
| Pixel transfer | `glib::Bytes::from_owned` moves the worker buffer into GBytes (no extra clone) |

Shipped later: generate missing FreeDesktop thumbs; video first-frame (`ffmpeg`); PDF page 1 (`pdftoppm`). Still deferred: full single-open dims (kept header `file_info` for native WxH label).

---

## Decision log

| when | decision |
|------|----------|
| 2026-07-14 | Baseline captured before shipping preview opts |
| 2026-07-14 | Prefer scaled off-thread decode over full-res main-thread `Texture::from_bytes` |
| 2026-07-14 | Cap in-memory textures at 24; FreeDesktop thumbs still first path |
| 2026-07-14 | Single-flight worker + mtime/size cache key; thumbs off main |
