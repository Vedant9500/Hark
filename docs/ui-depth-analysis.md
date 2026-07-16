# `src/ui` depth analysis — lightweight UI without visual loss

**Date:** 2026-07-16  
**Scope:** `src/ui/*` (+ brief theme/idle notes)  
**Goal:** cut wasted CPU / wakeups / main-thread work while **keeping** current visuals (frosted shell, rows, badges, preview, DnD, settings, animations as they are today).

Related:

- Preview tracker: [`preview-optimization.md`](./preview-optimization.md)
- Battery / wakeups: [`battery-optimization.md`](./battery-optimization.md)
- Power vs Rofi: [`power_performance.md`](./power_performance.md)
- Index/search: [`index-regression-depth-analysis.md`](./index-regression-depth-analysis.md)

---

## TL;DR

| Area | Health | Main issue |
|------|--------|------------|
| Idle daemon (hidden) | **Good** | No polling loops; theme uses `FileMonitor`; hide cancels debounce + deep gen |
| Typing / search results | **OK, improvable** | Full **ListBox teardown + rebuild** every debounced query; icon theme lookups per row |
| Selection / arrow keys | **Good** | Footer + preview update only; preview debounced + single-flight |
| Preview decode | **Good** | Off-main, scaled, LRU, FreeDesktop thumbs |
| Deep / translate async | **OK** | Event-driven (no 16 ms poll); **unbounded thread spawn** still possible while typing |
| Settings | **Heavy at boot** | Entire dual-pane UI built at daemon start (~2k LOC of widgets always resident) |
| CSS / chrome | **Good** | Explicitly **no** box-shadow; blur is compositor-side |

**Highest ROI (no visual change):**

1. **Reuse / pool result rows** instead of destroy+create all 25 widgets every search.  
2. **Cache `IconTheme` + icon resolve** (stop `has_icon` × N per refresh).  
3. **Drop duplicate footer/preview updates** on `select_row`.  
4. **Single-flight deep (and translate) worker** — latest query wins.  
5. **Defer Settings widget build** until first open (or keep structure but lazy-fill heavy pages).  
6. **Cache `hyprctl monitors -j`** across show (or only on monitor change).

Search **engine** µs are already fine; UI cost is almost all **GTK widget churn + icon theme + optional deep/ffmpeg**.

---

## 1. Map of `src/ui`

| File | ~LOC | Role | Hot? |
|------|-----:|------|------|
| `mod.rs` | 1067 | Window, search debounce, list rebuild, deep/translate async, keys, show/hide | **Yes** |
| `rows.rs` | 210 | Build one result row (icons, labels, DnD attach) | **Yes** (×N) |
| `preview.rs` | 1068 | Media panel, decode worker, texture LRU | **Yes** on select |
| `dnd.rs` | 349 | Drag sources, focus/keyboard during drag | On drag only |
| `thumbnails.rs` | 225 | FreeDesktop thumb path + write | Worker / drag icon |
| `footer.rs` | 85 | Footer action chips | Cheap |
| `settings.rs` | 1980 | Full settings dual pane | Boot + rare |
| `style.css` | 166 | Fallback static CSS | Once |

**Main loop architecture (already solid):**

```text
keystroke → cancel debounce timer → arm 40ms (180ms for auto CJK)
  → refresh_results:
       destroy all rows → engine.search → build_row×N → select_row
       maybe spawn deep thread + async_channel → main applies merge
       maybe spawn translate thread
arrow ↓/↑ → ListBox selection → footer + preview.update (debounced load)
hide → cancel debounce, deep_gen++, preview.clear
```

Shipped wins (do not regress):

- Search debounce **40 ms** (was per-keystroke).  
- Deep results via **channel**, not 16 ms `try_recv` poll.  
- Theme **FileMonitor**, not 2 s poll.  
- Preview: scale + debounce + single-flight + LRU + off-main thumbs.

---

## 2. Hot-path cost model (one debounced search)

Assume ≤25 hits (hard cap).

| Step | Thread | Cost class | Notes |
|------|--------|------------|-------|
| Debounce timer cancel/arm | Main | µs | Fine |
| `list.remove` all children | Main | **medium** | Widget destroy + CSS/layout |
| `engine.search` | Main | µs–low ms | Already optimized |
| `build_row` × N | Main | **medium–high** | IconTheme + labels + optional DragSource |
| `select_row(0)` | Main | low | Fires `row_selected` |
| Explicit `update_footer` + `preview.update` | Main | low–med | **Duplicates** selection handler |
| Spawn deep walk thread | New OS thread | scheduling | May run even if user types again |
| Spawn translate `curl` thread | New OS thread | net | Only if translate enabled |
| Preview image path | Worker | 10–50 ms | Already off main; ffmpeg/pdftoppm heavier |

**Idle hidden:** essentially GTK main loop sleep + IPC. Good.

---

## 3. Inefficiencies (CPU / allocations) — ranked

### P0 — Full list rebuild every query (`mod.rs` `refresh_results`)

```text
while let Some(child) = list.first_child() { list.remove(&child); }
for item in found { list.append(&build_row(...)); }
```

**Why it hurts:** GTK4 `ListBox` is not a virtualized list. Destroying and recreating ~25 complex rows (box + image + 3 labels + badge + drag controller) forces:

- GObject churn  
- CSS node rebuild  
- Measure/allocate pass on the shell  
- Icon loading pipeline  

**Visual-safe fix options:**

| Approach | Visual impact | Effort |
|----------|---------------|--------|
| **A. Row pool / reuse** — keep N `ListBoxRow`s; update labels/icons/drag path; hide extras | None | Medium |
| **B. `SignalListItemFactory` + `ListView`/`GridView`** | May need CSS port; look can match | High |
| **C. Diff by id** — only rebuild changed indices | None if careful | Medium |

**Recommendation:** **A** first (pool of 25 rows). Cap is already 25 → virtualization was correctly marked n/a for scrolling, but **reuse still wins** for typing.

Also in `apply_deep_hits` / `apply_translate_hits` — same full teardown.

---

### P0 — Icon resolution cost per row (`rows.rs`)

Per row, on the main thread:

1. Optional: `IconTheme::for_display` + `has_icon("{name}-symbolic")`  
2. Again: `IconTheme::for_display` + `has_icon(icon_name)`  
3. `Image::from_icon_name`  

With 25 rows → **~25–50 theme lookups** after every search. `has_icon` walks theme search paths; not free on cold cache.

**Visual-safe fix:**

- Resolve theme **once** per refresh: `let theme = IconTheme::for_display(...)`.  
- Module-level or `Launcher`-held **`HashMap` icon name → resolved name** (or “missing → fallback”).  
- Precompute symbolic preference when settings change, not per row.  
- Prefer `Image::from_icon_name` without probing when the name is a known FreeDesktop generic (`folder`, `text-x-generic`, …).

No visual loss if resolve logic stays identical — only cache hits.

---

### P1 — Double chrome update on first selection

In `refresh_results` / apply_*:

```text
list.select_row(Some(&row0));   // → connect_row_selected → footer + preview
update_footer(...);             // again
preview.update(...);            // again
```

**Bug-class:** wasted work, not wrong UI. Preview `update` may re-enter path / metadata / debounce arm twice for the same item.

**Fix:** either:

- suppress selection handler while rebuilding (`Cell<bool> rebuilding`), **or**  
- rely only on selection signal and drop explicit footer/preview calls after `select_row`.

---

### P1 — Deep search: unbounded concurrent threads

Each `refresh_results` that passes `should_deep_search` does:

```text
std::thread::spawn(|| engine.search_files_deep(...))
```

Generation tokens drop **stale results**, but old threads **still walk disk** until budget ends (sync/async visit caps). Fast typing on weak queries can stack several workers → CPU + disk while panel is open.

**Visual-safe fix:** one **global deep worker** (channel of latest query / gen), same pattern as preview `worker_busy` + latest-wins. Optional cooperative cancel later.

Same pattern for **translate** network threads when enabled.

---

### P1 — `config.get()` clones full `BlinkConfig` on every refresh

```rust
let ui = engine.config().get().ui; // clones entire config incl. exclude lists
```

**Fix:** `config.with(|c| c.ui.clone())` or store `icon_size` / `symbolic_icons` in `Cell`s updated only from settings. Tiny but free.

---

### P2 — `hyprctl monitors -j` on every `show()` (`center_on_active_monitor`)

Spawns a process and parses JSON **every hotkey open**.

**Fix:**

- Cache last geometry for ~1–5 s, or  
- Cache until `hyprland` workspace/monitor event (optional), or  
- Only re-query if multi-monitor (if single monitor, static margin).

Visual: same placement; less open latency / CPU.

---

### P2 — Settings built eagerly at daemon start

`SettingsPanel::new` builds **all** category pages (indexing, mounts, deep roots, appearance, open-with, translate, …) into a `Stack` at `Launcher::new`.

**Cost:** RSS + widget tree forever (~daemon already GTK-heavy; this adds more).  
**Not** a typing CPU issue.

**Visual-safe fix:**

- Build settings **on first open** (`connect` once), keep shell placeholder, **or**  
- Lazy-build each stack page on first nav select.

Instant settings open can stay “fast enough” if first open builds once and caches.

---

### P2 — DragSource attached on every file/folder/app row

`attach_path_drag` adds a `DragSource` controller per row. Rebuild tears them down.

With row pool: attach **once** per pooled row; `set_path` like `PathDragBinding` already does for preview.

---

### P3 — Stack Crossfade 120 ms (search ↔ settings)

```rust
stack.set_transition_duration(120);
```

Minor compositor work when opening settings. Optional: `None`/0 for lower GPU; **slight** visual change — only do if user accepts.

---

### P3 — Preview `file_meta` / path checks on main thread

`preview.update` does `path.is_dir()`, `media_kind`, `file_meta` (stat) on main for media files. Cheap vs decode; OK. Avoid expanding main-thread work here.

**ffmpeg / pdftoppm** already on worker — good. Still expensive if user arrows through videos; debounce 45 ms + single-flight already limits this. Optional: skip video frame extract unless selection stable **>150 ms** (still same UI, later paint).

---

### P3 — `show()` always `refresh_results("", …)` + `settings.refresh_status()`

Empty query builds frecency/app filler rows — intentional.  
`refresh_status` only sets a label — fine.

Optional: skip full empty rebuild if results already empty and gen unchanged (micro).

---

## 4. Bugs / correctness footguns

| ID | Severity | Issue | Impact |
|----|----------|-------|--------|
| B1 | Low | **Double preview/footer** on rebuild select | Extra CPU; rare race if preview gen races with itself |
| B2 | Med (UX) | **`refresh_results` no-op while drag active** | Results freeze mid-drag if user types — intentional to protect DnD; OK if documented |
| B3 | Low | Deep/translate threads **outlive hide** until walk finishes | Hide bumps `deep_gen` so UI won’t apply; still burns CPU after Esc until budget ends |
| B4 | Low | `apply_deep_hits` rebuilds rows with `build_row(..., selected=false)` for all then reselects | Works; pairs with P0 churn |
| B5 | Low | Row `_selected` arg unused in `build_row` | Dead param; selection styling is CSS `:selected` — OK |
| B6 | Info | Settings index poll **200 ms** while rebuild button pressed | Bounded (n≤300); only during rebuild — OK |
| B7 | Info | Focus-loss hide **80 ms** timer | Needed for DnD; good |

No show-stopping UI correctness bugs found in the search/preview path for normal use.

---

## 5. What’s already optimized (keep)

| Item | Where | Why keep |
|------|-------|----------|
| 40 ms search debounce | `mod.rs` | Cuts typing storms |
| 180 ms auto-translate debounce | `mod.rs` | IME/CJK |
| Deep via `async_channel` | `mod.rs` | No 16 ms UI poll |
| hide cancels debounce + gen + preview | `mod.rs` | No work while hidden |
| Preview debounce 45 ms + single-flight | `preview.rs` | Arrow-key CPU |
| Scaled decode ≤ ~2× frame | `preview.rs` | Texture RAM |
| Texture LRU 24 + FileFp | `preview.rs` | Re-select free |
| FreeDesktop thumbs off-main | `preview.rs` / `thumbnails.rs` | Shared cache |
| Theme FileMonitor | `theme/mod.rs` | No 2 s poll |
| CSS no box-shadow | `style.css` / `theme/css.rs` | Avoid square shadow halo + extra paint |
| Result cap 25 | engine + UI | Virtualization unnecessary for scroll |
| DnD ignores focus-loss | `dnd.rs` | Correct Wayland behavior |

---

## 6. External / docs alignment

From project docs + common GTK launcher practice:

| Source | Guidance | Blink status |
|--------|----------|--------------|
| `battery-optimization.md` | Debounce search; no poll; cancel on hide | **Done** |
| `preview-optimization.md` | Off-main decode, debounce, LRU | **Done** |
| GTK4 lists | Prefer model/factory or reuse widgets | **Not done** — full rebuild |
| Icon themes | Cache lookups; avoid repeated `has_icon` | **Not done** |
| Process spawn | Avoid hot path (`hyprctl` every show) | **Improvable** |
| Rofi comparison | Idle RSS dominated by GTK | Accept; fight **spikes**, not 60 MB floor |

---

## 7. Recommended plan (visual features preserved)

### Phase 1 — quick wins (half-day)

1. **Icon resolve cache** + single `IconTheme` per refresh.  
2. **Suppress double** footer/preview on programmatic `select_row`.  
3. **Cache hyprctl** monitor geometry (TTL 2 s or until show count).  
4. Avoid full `BlinkConfig` clone for `icon_size` / symbolic flags.

**Expected:** lower main-thread time per keystroke; same pixels.

### Phase 2 — row pool (1–2 days)

5. Pool of 25 rows: update title/subtitle/badge/icon/drag path; hide unused.  
6. Same pool path for deep/translate apply.  
7. One DragSource per pooled row (`PathDragBinding`-style).

**Expected:** largest drop in typing jank / CPU spikes.

### Phase 3 — async single-flight (half-day)

8. Single deep worker + latest gen/query.  
9. Single translate worker when enabled.  
10. On hide: still discard results; optionally set a cancel flag for new walks.

**Expected:** less disk CPU after Esc / fast typing.

### Phase 4 — settings weight (optional)

11. Lazy-build settings on first open **or** lazy pages.  
12. Keeps visuals; first open may take 10–50 ms extra once.

### Do **not** do (visual / product loss)

- Remove preview, badges, DnD, frosted shell, layer-shell.  
- Virtualize with visible scrollbars / different density unless redesigned.  
- Block main thread for ffmpeg “to simplify”.  
- Reintroduce polling.

---

## 8. Measurement checklist

```bash
# After each phase: search still same medians
blink --bench

# Typing CPU (manual): open launcher, hold a key or type fast "doc"
# Compare `perf top -p $(pgrep -n blink)` or `pidstat -u -p … 1`

# Open latency: time from hotkey to painted (phone slow-mo / hyprland logs)
```

Instrument (optional, temporary):

- `refresh_results` wall µs (destroy / search / build / apply).  
- Count `build_row` calls per second.  
- Deep thread live count.

---

## 9. Priority matrix

| ID | Item | CPU | Visual risk | Priority |
|----|------|-----|-------------|----------|
| U1 | Row pool / reuse | High | None | **P0** |
| U2 | Icon theme cache | High | None | **P0** |
| U3 | Dedup select footer/preview | Med | None | **P1** |
| U4 | Single-flight deep/translate | Med | None | **P1** |
| U5 | hyprctl cache | Low–med open | None | **P2** |
| U6 | Config clone / ui cells | Low | None | **P2** |
| U7 | Lazy settings | RSS | None | **P2** |
| U8 | Longer video preview settle | Low | Later paint | **P3** |
| U9 | Disable stack crossfade | Tiny | Slight | **P3** |

---

## 10. Conclusions

1. **`src/ui` is already battery-aware for idle** (debounce, no poll, hide cancel, preview worker).  
2. The **main remaining waste** is **rebuilding the entire result list and re-resolving icons on every search**, not engine search µs.  
3. Preview path is in good shape; don’t regress it.  
4. Deep/translate are **correct** with gen tokens but not **cheap** under rapid typing — need single-flight.  
5. Settings and `hyprctl` are **daemon/open** costs, not per-keystroke.  
6. Goal “really lightweight, no visual loss” is best hit by **U1+U2+U3+U4** without touching CSS or layout.

---

## Appendix A — file touch list for Phase 1–2

| Change | Files |
|--------|-------|
| Icon cache | `rows.rs`, maybe `mod.rs` (pass theme) |
| Row pool | `mod.rs`, `rows.rs`, `dnd.rs` |
| Select dedup | `mod.rs` |
| Deep single-flight | `mod.rs` (+ optional `engine` helper) |
| hyprctl cache | `mod.rs` |
| Lazy settings | `mod.rs`, `settings.rs` |

## Appendix B — hot functions

```text
Launcher::show / hide
search.connect_changed → refresh_results
refresh_results → build_row × N
list.connect_row_selected → update_footer + PreviewPanel::update
PreviewPanel::queue_image_load → pump_worker → decode_preview_media
apply_deep_hits / apply_translate_hits
center_on_active_monitor → hypr_focused_monitor
SettingsPanel::new (boot)
```


---

## 11. Implemented (2026-07-16) — Phase 1 + Phase 3

| Item | Status |
|------|--------|
| U2 Icon resolve cache (`rows.rs` thread-local map) | **done** |
| U3 Suppress select side-effects during programmatic select | **done** |
| U5 hyprctl monitor geometry cache (2s TTL) | **done** |
| U6 ui_icon_size / ui_symbolic cells (no full config clone per search) | **done** |
| U4 Single-flight deep + translate workers (latest-wins) | **done** |
| Clear icon cache on symbolic toggle | **done** |

Still open: U1 row pool (Phase 2), U7 lazy settings.
