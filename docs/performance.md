# Hark — performance reference

**Last updated:** 2026-09-18  
**Machine (reference):** Linux / Hyprland · 16 CPUs · ~15 GB RAM  
**Tracker:** [`OPTIMIZATION.md`](./OPTIMIZATION.md)  
**Depth study (raw):** [`depth-index-benchmark.json`](./depth-index-benchmark.json)  
**Docs index:** [`README.md`](./README.md)

Stable numbers for search latency, index cost, and binary size.  
Work in progress and historical logs → tracker + [`bench/`](./bench/) + [archive](./archive/optimization-tracker-2026-07-full.md).

---

## Quick snapshot (default depth = 2)

Order-of-magnitude; re-run `--bench` on your machine after big changes.

| Metric | Typical | Notes |
|--------|--------:|-------|
| Math / units / FX-parse | **~1–8 µs** | calc short-circuit; full FX conversion needs a warmed rates cache (§ Re-run 2026-09-18) |
| Battery `battery` | **~150–160 µs** | sysfs read per query |
| Timezone / date / duration / cooking / quickwin | **~1–6 µs** | added since 07, all sub-frame |
| Apps isolated | **~8 µs** | query-dependent; ~4× faster than 07 |
| Files isolated `doc` | **~140 µs @ 5.7k items** | scales with index (~24 ns/item); was ~50–70 µs @ ~2k items |
| File merged `doc` | **~155–160 µs** | + engine merge |
| Long free-text (11–12 chars) | **~1–2 ms** | Skim fuzzy; hot set cold on this box |
| Define owned / web forced | **~0–1 µs** | pending-row / pure construction |
| Strong hot free-text (len≥4) | **≪ full** | skips full index when usage hot hits |
| Index items (depth 2) | **~5.7k** | home + mounts + 17 deep pins; varies |
| Index rebuild | **~43 ms warm** | blocking bench rebuild; cold NTFS reads spike (287 ms seen once) |
| Cache on disk (v6) | **~526 KB** | ~5.7k items |
| Binary (layer-shell + bench) | **~7.8 MB** | stripped + LTO; daemon build without `bench` is smaller |
| Daemon RSS idle | **~60–65 MB** | GTK-dominated (unchanged; not re-measured) |
| GPU | **~0%** | no CUDA path |

Older depth-2 campaign (~1.8k items, 2026-07-14): see JSON + table below.

---

## Re-run 2026-09-18 (same reference machine, default depth = 2)

`--bench` extended since July: 16 extra merged cases (calc battery/tz/date/
duration/tip/cooking/quickwin/miss, web forced, translate, define, file
glob/single/long/path, empty state) + a `providers:` header line
(`translate=false · define=true · web=true` on this machine).
Raw log (local, gitignored): [`bench/run-2026-09-18.txt`](./bench/).
4 runs, medians stable run-to-run (±5 µs on search cases).

| Metric (merged median) | 2026-07-14 | 2026-09-18 | Notes |
|--------|--------:|-------:|-------|
| Math `10 + 20` | ~2 µs | **~4 µs** | calc stack grew (battery/cooking/finance/…) — still free |
| Units `10kg to lb` | — | **~4 µs** | |
| Unit partial `10kg to pou` | — | **~7 µs** | prediction intact |
| FX `100 usd to eur` | — | **~1.0 ms ⚠** | **not a conversion:** no rates cache on this box, calc misses (~8 µs, 0 hits) → 14-char file-fuzzy miss + web row. Re-run with a warmed FX cache for true conversion cost |
| Apps `fire` | ~68 µs | **~9 µs** | **~7× faster** (haystack + alloc-cut audits) |
| Files `doc` | ~34 µs | **~155–160 µs** | index 1,798 → **5,710 items** (see below); per-item ~19 → ~28 ns (+47% merge overhead: usage boost, typo alias, web/define/translate gating) |
| Files `f doc` | ~52 µs | **~135–145 µs** | same scaling |
| Settings | — | **~130 µs** | 12 hits (command + app/file mix) |
| iso_apps `fire` | ~34 µs | **~8 µs** | ~4× faster |
| iso_files `doc` | ~49 µs | **~140 µs** | items 3.18× → **per-item 27 → 24 ns (~11% faster)**; absolute rise is index size, not regression |
| iso_calc | ~2 µs | **~3 µs** | flat |
| Index items (depth 2) | 1,798 | **5,710** | 17 deep-root pins + `/mnt/data` mount since July (was home + windows_d) |
| Index rebuild | ~15 ms | **~43 ms** (one cold-FS outlier 287 ms) | ~linear in items; NTFS cold reads dominate the outlier |
| Cache on disk (v6) | ~114 KB | **~526 KB** | 4.6× bytes / 3.18× items → ~94 B/item (longer deep paths) |
| Binary (layer-shell + bench) | ~6.4–6.7 MB | **~7.77 MB** | +~1.2 MB: web/define/translate providers, calc cards, action panel, previews. Daemon build (no `bench`) is smaller |
| Warm | ~53 ms | ~50 ms | flat (cache hit) |
| Bench RSS | — | ~27 → ~37.5 MB peak, 2 threads | first recording; no 07 baseline |

### Extended prompts (new, 2026-09-18 medians)

| Case | Query | Median | Hits | Notes |
|------|-------|-------:|-----:|-------|
| calc_battery | `battery` | ~157 µs | 1 | sysfs read per query |
| calc_tz | `now in tokyo` | ~2 µs | 1 | chrono-tz, offline |
| calc_date | `tomorrow` | ~3 µs | 1 | |
| calc_dur | `2d + 3h` | ~1 µs | 1 | |
| calc_tip | `tip 20% on 45` | ~6 µs | 1 | natural-language math |
| calc_cook | `1 cup sugar in g` | ~3 µs | 1 | |
| calc_quick | `255 to hex` | ~2 µs | 1 | |
| calc_miss | `hello world` | ~1.1 ms | 1 | plain-text reject works (calc ~0 µs); cost is 11-char file fuzzy + web fallback row |
| web_forced | `? hello world` | ~0 µs | 1 | pure construction, owns query |
| translate | `tr hello world` | ~2.1 ms ⚠ | 1 | **translate OFF on this box** → falls through to 14-char file fuzzy. Owned-path cost is ~1 µs (same pending-row shape as define, verified isolated) |
| define | `what does rust mean` | ~1 µs | 1 | owned pending row, mem-cache only |
| file_glob | `*.md` | ~340–360 µs | 25 | |
| file_single | `a` | ~360 µs | 25 | exact/prefix policy, no fuzzy |
| file_long | `optimization` | ~2.1 ms | 25 | 12-char Skim fuzzy over 5.7k items; hot set cold (usage has 2 keys) so no short-circuit |
| file_path | `~/` | ~130 µs | 25 | completions |
| empty | `(empty)` | ~19 µs | 15 | frecency + app fill |

Takeaways:

1. **Apps got much faster** (4–7×) — the audit work paid off.
2. **File absolute times track index size** (3.2× items → 2.8–4.7× time);
   per-item cost is flat-to-better. Keep default depth 2.
3. **Calc expansion was free**: 7 new calc shapes all ≤7 µs (battery excepted —
   sysfs, still sub-frame). The plain-text reject gate holds.
4. **New provider gating is free when owned** (define 1 µs, forced-web 0 µs).
   Unowned long free-text pays file-fuzzy (~1–2 ms at 11–14 chars) — same
   budget class as before, just newly visible in the bench.
5. Next µs (if ever needed): warm FX mem-cache note above; long-query fuzzy
   is the only file cost that outgrew items — hot-set seeding (H2) is the
   lever, already tracked in `OPTIMIZATION.md`.

---

## How to benchmark

```bash
cargo build --release --features "layer-shell,bench"
./target/release/hark --bench
```

Daemon-only install (no micro-bench in binary):

```bash
cargo build --release --features layer-shell
```

`--bench` prints: warm → blocking rebuild → merged medians → iso_* → resources.

---

## Index depth chart

Raising `max_depth` multiplies **items, rebuild time, and cache size**. Search stays in the same ballpark longer than rebuild does.

### Results (2026-07-14, same roots)

> Historical: roots have since changed (17 deep pins + `/mnt/data`), so
> depth ratios below are stale for this machine — re-run the depth study
> before quoting them. Default-depth numbers above are current.

| max_depth | Items | rebuild_ms | cache | iso_files `doc` | Notes |
|----------:|------:|-----------:|------:|----------------:|-------|
| **2** (default) | 1,798 | 15 | 114 KB | ~49 µs | **Recommended** |
| 3 | 7,220 | 48 | 560 KB | ~128 µs | ~4× items |
| 4 | 14,831 | 106 | 1.2 MB | (see JSON) | ~8× items |

Raw rows + relative ratios: [`depth-index-benchmark.json`](./depth-index-benchmark.json).

### Relative cost (vs depth 2)

| Depth | Items | Rebuild | Cache |
|------:|------:|--------:|------:|
| 2 | 1× | 1× | 1× |
| 3 | ~4× | ~3× | ~5× |
| 4 | ~8× | ~7× | ~10× |

### What stays flat

Math / units / FX and pure app isolation do **not** scale with the file index (calc short-circuit).

### Recommendations

| Depth | When |
|------:|------|
| **2** | Default — snappy, small cache |
| 3 | More project files; still fine on this machine |
| 4+ | Only if needed; re-bench |

**UI:** Settings → Indexing → Scan depth (clamped **1..=6**).

### Pitfall (fixed)

Older builds clamped `max_depth > 3` to 2. Current clamp is **1..=6**.

### Re-run depth study

```bash
# For each depth in 2 3 4:
# 1. Set index.max_depth in ~/.config/hark/config.json
# 2. rm ~/.cache/hark/file-index.json ~/.cache/hark/file-index.meta
# 3. cargo build --release --features "layer-shell,bench" && ./target/release/hark --bench
# 4. Restore max_depth=2
```

Update the JSON + table when roots or machine change.

---

## Techniques that mattered (search / index)

1. Calc short-circuit + plain-text reject  
2. File two-pass (name first, fuzzy gated)  
3. Top-K heaps (25)  
4. App haystack / name_lower  
5. Merged policy: apps hit → files name-only  
6. Index fingerprint + TTL  
7. Compact cache v6  
8. Hot set free-text (long strong names only)  

---

## Index / cache

| Item | Value |
|------|--------|
| Path | `~/.cache/hark/file-index.json` |
| Meta | `file-index.meta` → `version ts fingerprint` |
| Version | **6** |
| Schema | `{ "version", "fingerprint", "items": [ { "p", "d", "n" } ] }` |
| Cap | 100_000 |
| TTL | 30 min (if fingerprint matches) |

### Cache size evolution (~1.8k items)

| Version | Size | Shape |
|--------:|-----:|-------|
| v3–v4 | ~236 KB | fuller JSON |
| v5 | ~423 KB | path_lower + flags on disk |
| **v6** | **~114 KB** | path + is_dir + depth only |

---

## Binary / daemon

| Metric | Typical |
|--------|--------:|
| Release + layer-shell + bench | ~7.8 MB (2026-09-18; was ~6.4–6.7 MB in 07 — web/define/translate, calc cards, action panel, previews) |
| Profile | LTO, `opt-level=3`, strip, `panic=abort` |
| Idle daemon RSS | ~60–65 MB |
| Idle CPU | &lt;1% |

---

## Cold start / warm start (2026-09-18, same reference machine)

Heads-up on method: your live daemon was **not** touched. Daemon and
toggle numbers below come from an isolated env (temp `XDG_*` dirs + real
config/cache copies, hidden daemon, scratch IPC socket). No Xvfb/existing
compositor nesting available, so *window-build* and *first-frame* costs are
unmeasured — bounded by reasoning, marked ≈.

| Stage | Median | Notes |
|-------|-------:|-------|
| Process exec baseline | ~21 ms | spawn → arg parse; dominates every short-lived `hark` invocation |
| Engine construct (`new_headless`) | ~0 ms | lazy — real work is the background warm |
| Index ready, **warm** disk cache | **~7 ms** after spawn | 5,720 items from `file-index.json` |
| Index ready, **cold** cache (full walk) | **~40–43 ms** | first launch / cache wiped; matches `--bench` rebuild |
| First `doc` search after ready | ~250–300 µs | steady-state ~140–160 µs |
| `hark --search` wall, warm cache | ~73 ms | 21 exec + ~7 work + 50 ms poll-quantum artifact in `run_search_once` |
| Daemon spawn → resident (hidden, warm cache) | **~24 ms** (3/3 runs) | exec + GTK init + `Engine::new` + socket bind; SIGTERM cleanup verified |
| Warm toggle client (real `hark` binary) | **~21–22 ms** wall | ~all exec; in-process IPC is median **29 µs** / p95 44 µs |

### Window: build + first frame (measured in headless Sway + grim)

Method (2026-09-18): `WLR_BACKENDS=headless sway` (fully invisible — no host
window), temp `XDG_*` dirs with real config/cache copies, private D-Bus
(`dbus-run-session`, so the probe can't collide with the live daemon's
app-id). Two probes: frame-clock `after-paint` in-process, and end-to-end
`grim` pixel-diff (`HEADLESS-1`, 1280×720, ~15 ms/shot quantum).

| Stage | Measured | Notes |
|-------|---------:|-------|
| `Launcher::new` (widget build) | **~19 ms** steady (31 ms first-ever: icon/CSS/font caches) | full tree: search, rows, preview, settings, footer |
| `show()` return | ~106–149 ms | `reload_apps` desktop rescan + empty-state refresh + `present` |
| First frame (`after-paint`) | **~108–111 ms** after show (fresh process; 366 ms first-ever: render-pipeline compile) | client-side draw done |
| Toggle → first pixels, **warm daemon** | **48–65 ms** wall | incl. ~21 ms toggle-client exec + ≤15 ms grim quantum → daemon-side ≈ **15–30 ms** |
| Toggle → first pixels, **fresh daemon** | ~257 ms (n=1) | ≈ build + cold render; matches fresh-process total above |

Derived budgets:

- **Warm start** (daemon resident, hotkey): **~50–65 ms toggle → visible**,
  measured end-to-end. The daemon saves the window build + engine warm, not
  the ~21 ms client exec.
- **Cold start** (no daemon): ~24 ms to resident + ~20 ms Launcher build
  (built at startup activate, before first toggle) + ~240 ms first render
  ≈ **~280 ms to visible**; cold disk cache adds ~35 ms of index walk.
  `request_toggle` fast-paths missing sockets (no retry sleep), so the cold
  path pays no IPC penalty.

If toggle ever feels slow, suspect client exec (binary page-in under memory
pressure) before the daemon — IPC itself is 30 µs, and re-show render is
15–30 ms.

---

## Defaults

| Setting | Default |
|---------|--------:|
| max_depth | **2** |
| Depth clamp | 1–6 |
| MAX_INDEX | 100_000 |
| Index TTL | 30 min |

---

## Related code

| Area | Path |
|------|------|
| Engine | `src/engine.rs` |
| File index / search / hot | `src/providers/files/` |
| Apps | `src/providers/apps.rs` |
| Calc | `src/providers/calc/` |
| Translate / define / web | `src/providers/{translate,define,web}.rs` |
| Bench CLI | `src/bench.rs` (`--features bench`) — merged + isolated + extended tables; `providers:` header shows translate/define/web switches |
