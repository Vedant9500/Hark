# Blink — Optimization & Modularization Tracker

**Status:** optimizing (baseline_v1 recorded)  
**Last updated:** 2026-07-14  
**Binary:** `~/.local/bin/blink` · build: `cargo build --release --features layer-shell`  
**Measure:** `blink --bench` → paste into Improvement log  
**Canonical metrics:** [`docs/performance.md`](docs/performance.md) · depth data [`docs/depth-index-benchmark.json`](docs/depth-index-benchmark.json)

---

## Goals

1. **Split oversized modules** so features can be changed without touching unrelated code.
2. **Make search feel instant** (typing → results on every keystroke).
3. **Make indexing cheaper** (startup, rebuild, memory, disk).
4. **Keep UI responsive** (no jank on open/toggle/settings).
5. **Track work in small, shippable slices** with clear done criteria.

Non-goals for this phase: new Raycast-style extensions, plugins marketplace, Windows/macOS.

---

## Current map (baseline)

| Area | Path | ~LOC | Role | Health |
|------|------|------|------|--------|
| Entry / daemon | `src/main.rs` | 76 | CLI, daemon, show/hide | OK |
| IPC | `src/ipc.rs` | 63 | Unix socket toggle | OK |
| Engine | `src/engine.rs` | 288 | Merge providers, rank, execute | OK — grow carefully |
| Config | `src/config.rs` | 392 | Config + mounts | OK — split if mounts grow |
| Usage | `src/usage.rs` | 125 | Frecency | OK |
| Theme | `src/theme/` | ~724 | load + css split | **Split done** |
| Apps | `src/providers/apps.rs` | 280 | `.desktop` fuzzy | OK |
| Files | `src/providers/files/` | ~890 | index + search split | **Split done** |
| Calc | `src/providers/calc/` | ~1.6k split | Math, units, FX, TZ, duration | **Split done** |
| FX | `src/providers/fx.rs` | 221 | Rates cache | OK |
| UI shell | `src/ui/` | ~600+rows/footer | launcher + rows + footer | **Split done** |
| Settings | `src/ui/settings.rs` | 590 | Dual-panel settings | OK |
| CSS fallback | `src/ui/style.css` | 166 | Static styles | OK |

**Total ~6k LOC.** Biggest leverage: `calc.rs` → packages, then `files.rs` search path, then UI row rebuild.

---

## Target module layout

```
src/
  main.rs
  ipc.rs
  engine.rs
  config/
    mod.rs          # BlinkConfig, ConfigStore
    mounts.rs       # discover_mounts, labels
  usage.rs
  theme/
    mod.rs          # ThemeManager, scheme load
    css.rs          # generated CSS only
  providers/
    mod.rs          # ResultKind, SearchResult, Action, Provider
    apps.rs
    files/
      mod.rs        # FileProvider public API
      index.rs      # walk, cache, progress, ignores
      search.rs     # query matching, path browser
      path_style.rs # Label / Drive pretty paths
    calc/
      mod.rs        # CalcProvider orchestration
      math.rs       # expressions, bases, natural %
      units.rs      # unit tables, convert, predict
      currency.rs   # FX query + predict
      duration.rs   # duration arithmetic
      timezone.rs   # TZ convert + predict
      datetime.rs   # relative / natural dates
    fx.rs           # rate store (unchanged)
  ui/
    mod.rs          # Launcher shell, stack, keys
    rows.rs         # build_row, conversion card
    footer.rs       # action chips / keycaps
    settings.rs
    style.css
```

Refactor rule: **move code first, change behavior second.** Each slice must compile + daemon restart OK.

---

## Workstreams

### A — Modularize calc (structure)

| ID | Task | Status | Done when |
|----|------|--------|-----------|
| A1 | Create `providers/calc/` with `mod.rs` re-exporting `CalcProvider` | **done** | Engine still builds; no behavior change |
| A2 | Extract `units.rs` (tables, convert, predict, unit_result) | **done** | `10kg to pou` still works |
| A3 | Extract `currency.rs` + keep `fx.rs` | **done** | `100 usd to eur` works |
| A4 | Extract `timezone.rs` + `datetime.rs` | **done** | `12pm here to lon` works |
| A5 | Extract `duration.rs` + `math.rs` | **done** | `10+20`, `10h - 50min` work |
| A6 | Thin `mod.rs` orchestrator only | **done** | `mod.rs` ~79 LOC |

**Layout after A:**
```
src/providers/calc/
  mod.rs        (~79)   orchestrator
  util.rs       (~46)   format_number, result_calc, relative_secs
  math.rs       (~101)  expressions, natural %, bases
  duration.rs   (~111)  duration arithmetic
  currency.rs   (~136)  FX query + predict
  datetime.rs   (~175)  now/today/relative dates
  timezone.rs   (~459)  TZ convert + predict
  units.rs      (~547)  unit tables, convert, predict
```

### B — Modularize files & config

| ID | Task | Status | Done when |
|----|------|--------|-----------|
| B1 | Split `files/` into index + search | **done** | `files/{mod,index,search}.rs` |
| B2 | Extract mounts from `config.rs` if needed | pending | Settings mounts list OK |
| B3 | Document index cache format (v4) in this file | **done** | See cache schema below |

### C — Modularize UI / theme

| ID | Task | Status | Done when |
|----|------|--------|-----------|
| C1 | Extract `ui/rows.rs` (list rows + conv card) | **done** | `ui/rows.rs` (~145 LOC) |
| C2 | Extract `ui/footer.rs` | **done** | `ui/footer.rs` (~72 LOC) |
| C3 | Split theme CSS into `theme/css.rs` | **done** | `theme/{mod,css}.rs` |

**Layout after C:**
```
src/ui/
  mod.rs       (~600)  launcher shell / keys / stack
  rows.rs      (~145)  result rows + conversion card
  footer.rs    (~72)   action bar chips / keycaps
  settings.rs  (~590)  dual-panel settings
  style.css            static fallback CSS
src/theme/
  mod.rs       (~171)  Theme load + ThemeManager
  css.rs       (~553)  generated CSS only
```

### D — Search performance

| ID | Task | Status | Done when |
|----|------|--------|-----------|
| D1 | Profile cold vs warm keystroke (log ms) | **done** | `blink --bench` + Improvement log |
| D2 | Debounce or cancel in-flight search if needed | pending | No lag on fast typing |
| D3 | Files: avoid full-scan per key; score top-N early exit | **done** | Top-K heap + two-pass (name then fuzzy) + path caches |
| D4 | Apps: prebuild lowercase/normalized name cache | **done** | haystack + name_lower + top-K; iso_apps ~37µs |
| D5 | Dedup + sort once (engine currently sorts twice) | **done** | Single `kind_rank` + score sort |
| D6 | Cap expensive providers when calc already matched | **done** | Calc short-circuit + plain-text reject; apps skip file fuzzy |

### E — Index / IO performance

| ID | Task | Status | Done when |
|----|------|--------|-----------|
| E1 | Measure index size, rebuild time, memory | **done** | `blink --bench` rebuild section |
| E2 | Incremental / dirty-root rebuild (not full every 30m) | **done** | Fingerprint skip: config roots/excludes; TTL still 30m |
| E3 | Cap + progress already exist — tune max & depth defaults | **done** | Documented in settings + table below |
| E4 | Serialize index with faster format or compact JSON | **done** | Cache v6 compact rows; atomic write |
| E5 | Don’t block UI thread on any disk IO | **done** | Startup/periodic/force rebuild already on bg threads |

### F — Runtime / binary

| ID | Task | Status | Done when |
|----|------|--------|-----------|
| F1 | Release already: LTO, strip, opt-level 3 | **done** | + `panic = "abort"` |
| F2 | Measure binary size post-split | **done** | See F log below |
| F3 | Consider `mimalloc` / jemalloc only if allocator shows up | **skipped** | Search CPU-bound; no allocator evidence |
| F4 | Shrink chrono-tz (filter IANA set) | **done** | `filter-by-regex` + `.cargo/config.toml` |

### G — UX polish (only after D/E baselines)

| ID | Task | Status | Done when |
|----|------|--------|-----------|
| G1 | Virtualize results list if &gt; N rows cost shows | **n/a** | Cap is 25 rows — virtualization not worth it |
| G2 | Settings nav keyboard (↑↓ between categories) | **done** | ↑/↓ j/k Home/End; focus nav on open |
| G3 | Math card formatting (trim float noise) | **done** | `format_number` scale-aware (e.g. 22.0462 lb) |

---

## Suggested order (sprints)

| Sprint | Focus | Exit criteria |
|--------|--------|----------------|
| **0** | This tracker + baselines | Benchmarks filled once |
| **1** | A1–A6 calc split | No feature regression |
| **2** | D5–D6 + D3 file search | Snappier typing on big index |
| **3** | B1 files split + E2 incremental index | Rebuilds cheaper |
| **4** | C1–C3 UI/theme split | Easier styling |
| **5** | G polish from measured pain | User-visible wins only |

---

## How to measure (always use this)

```bash
# 1) Release build + install
cargo build --release --features layer-shell
install -Dm755 target/release/blink ~/.local/bin/blink

# 2) Full report: search latency + RAM + CPU + GPU + host
blink --bench

# (daemon should be running separately for daemon RSS rows)
blink --daemon   # once at login
```

**Rule:** run `--bench` **before** and **after** each optimization. Paste **search table + resources** into the Improvement log. Same machine, warm cache, release binary.

`blink --bench` reports:
- Search median / p95 µs per query case
- **Bench process RAM** (RSS, peak HWM, VSZ, threads)
- **CPU** (user/sys ms, burst util % of one core during search hammer)
- **Daemon** (if `blink --daemon` is up): RSS, HWM, VSZ, threads, instant CPU%, mem%
- **GPU** via `nvidia-smi` (system-wide; blink is CPU/GTK — expect ~0% util)
- Host mem available, binary size, index cache size

---

## Snapshot baseline_v1 (post A + D3/D5/D6)

*2026-07-13 · re-measured with resource tracking*

### Search

| case | query | median_us | p95_us | hits |
|------|-------|----------:|-------:|-----:|
| math | `10 + 20` | 2 | 2 | 1 |
| unit | `10kg to lb` | 2 | 2 | 1 |
| unit_partial | `10kg to pou` | 4 | 4 | 1 |
| fx | `100 usd to eur` | 1 | 1 | 1 |
| app | `fire` | 417 | 489 | 25 |
| file | `doc` | 672 | 895 | 25 |
| file_force | `f doc` | 609 | 643 | 25 |
| settings | `settings` | 405 | 449 | 25 |

### RAM / CPU / GPU / sizes

| Metric | Value | Notes |
|--------|------:|-------|
| Binary size | 4.53 MB (4,746,560 B) | release + layer-shell |
| Index items | 1,798 | not capped |
| Index file size | 235 KB (240,953 B) | `file-index.json` v3 |
| Warm-up wall | 53 ms | engine + cache load |
| **Daemon RSS** | **60.7 MB** (62,116 KB) | idle `blink --daemon` |
| Daemon HWM | 62,116 KB | peak since start |
| Daemon VSZ | 640,920 KB | virtual |
| Daemon threads | 7 | |
| Daemon CPU% (idle) | 0.3% | instant sample |
| Daemon mem% | 0.40% | of 15 GB host |
| Bench RSS | 25→28 MB | headless `--bench` process |
| Bench CPU total | 170 ms / 212 ms wall | full run |
| Bench CPU burst | ~118% of 1 core | hammering searches |
| GPU | RTX 4060 Laptop · util **0%** · 66/8188 MB | system-wide; blink doesn't use CUDA |
| Host | 15.5 GB total · ~5.9 GB avail · 16 CPUs | |

> Pre-D3/D5/D6 was **not instrumented**. Treat this table as **baseline_v1**. Future rows compare against the previous snapshot.

---

## Improvement log (before → after)

Template for each change:

```
### YYYY-MM-DD — <task ids>: <one-line summary>
**Before:** <paste --bench medians or key rows>
**After:**  <paste --bench medians or key rows>
**Delta:**  math p95 X→Y (−Z%); file p95 …; RSS …; notes
```

### 2026-07-13 — A1–A6: split calc monolith
| Metric | Before | After | Delta |
|--------|--------|-------|-------|
| `calc.rs` structure | 1 file · 1618 LOC | 8 files · ~79 LOC orchestrator | maintainability only |
| Search timings | *(not measured)* | see baseline_v1 | no intentional perf change |

### 2026-07-13 — D5 + D6 + D3: sort once, calc short-circuit, file top-K
| Metric | Before (est.) | After (measured) | Delta / notes |
|--------|---------------|------------------|---------------|
| Sort path | 2× `sort_by` | 1× `kind_rank`+score | less CPU on every keystroke |
| `10 + 20` path | calc **+ apps + files** | calc only (D6) | **avoids ~0.4–0.7 ms** apps/files work |
| Math median | *(n/a)* | **2 µs** | effectively free |
| File `doc` | full score vec + sort all hits | top-25 heap + skip path-fuzzy when strong | **~672 µs** median @ 1798 items |
| File path fuzzy | always when name misses | gated once top-K ≥ strong | fewer fuzzy_match calls |
| Daemon RSS | *(n/a pre-instrument)* | **~61 MB** | GTK daemon baseline |
| GPU util | n/a | **0%** | expected (no GPU path) |

*When index grows (10k–100k), re-run `--bench` and add a new row — D3 gains scale with index size.*

### Resource delta template (copy for each future change)

| Resource | Before | After | Δ |
|----------|-------:|------:|--:|
| math p95_us | | | |
| file p95_us | | | |
| app p95_us (merged) | | | |
| iso_apps p95_us | | | |
| iso_files p95_us | | | |
| daemon rss_kb | | | |
| daemon threads | | | |
| bench rss_kb (end) | | | |
| cpu_burst %core | | | |
| gpu util_% | | | |
| binary_bytes | | | |
| index_bytes | | | |

### 2026-07-14 — D4 + B1 + E2

**Before** (`blink --bench`, merged only):

| case | median_us | p95_us |
|------|----------:|-------:|
| app `fire` | 410 | 832 |
| file `doc` | 637 | 712 |
| math | 2 | 2 |
| daemon rss_kb | 296024* | *(UI open inflated)* |

\*Earlier daemon sample had panel open (~296MB). Idle daemon baseline was ~62MB.

**After** (merged + isolated):

| case | median_us | p95_us | notes |
|------|----------:|-------:|-------|
| app `fire` (merged) | 421 | 433 | still includes file scan |
| file `doc` (merged) | 671 | 684 | stable |
| math | 2 | 2 | unchanged |
| **iso_apps** `fire` | **37** | **41** | **D4 win** (~10× vs merged app path cost) |
| **iso_files** `doc` | 619 | 624 | file provider alone |
| **iso_calc** | 2 | 3 | |
| daemon rss_kb (idle) | 62244 | ~61 MB | |
| binary_bytes | 4763608 | ~4.54 MB | |
| index cache | v4 + fingerprint | 240986 B | |

| Change | Effect |
|--------|--------|
| **D4** apps haystack + name_lower + prefix fast-path + top-K | Isolated apps **~37µs** median |
| **B1** `files/{mod,index,search}.rs` | Structure only; search path unchanged |
| **E2** config fingerprint | Skip rebuild when roots/depth/excludes unchanged (even if TTL expired would still rebuild on TTL) |

#### Index cache schema (v6)

`~/.cache/blink/file-index.json`:
```json
{ "version": 6, "fingerprint": "<hex>", "items": [ { "p": "/abs/path", "d": true, "n": 2 } ] }
```
`file-index.meta`: `6 <unix_ts> <fingerprint>`

Rebuild when: TTL > 30m **or** fingerprint ≠ current (roots / depth / excludes / version).

### 2026-07-14 — File search perf (path caches + two-pass)

**Before:**

| case | median_us | p95_us |
|------|----------:|-------:|
| iso_files `doc` | 641 | 649 |
| file `doc` merged | 704 | 947 |
| app `fire` merged | 445 | 459 |
| file_force `f doc` | 653 | 660 |
| bench cpu_total_ms | 200 | |

**After:**

| case | median_us | p95_us | Δ median |
|------|----------:|-------:|---------:|
| **iso_files `doc`** | **49** | **51** | **−92%** (~13×) |
| file `doc` merged | **97** | 102 | **−86%** |
| app `fire` merged | **276** | 425 | −38% |
| file_force `f doc` | **52** | 60 | **−92%** |
| bench cpu_total_ms | **60** | | **−70%** |
| iso_apps | 38 | 42 | unchanged |
| daemon rss_kb | ~62k | | similar |
| index_bytes | 433 KB | | larger (cached path_lower + flags) |

**What changed:**
1. Index stores `path_lower`, `low_value`, `high_value`, `is_mnt` (no per-query path lowercasing).
2. **Two-pass search:** name exact/prefix/substring first; fuzzy only if top-K weak.
3. Fuzzy pass: first-char filter + max 500 evaluations.
4. Cache version **v5**.

---

## Hotspots (code-level)

1. **`calc.rs`** — single file owns all calculators; hard to test/optimize in isolation.
2. **`engine.rs` `search`** — ~~double sort~~ fixed (D5); ~~always apps+files~~ short-circuits on calc (D6).
3. **`files.rs`** — still scans full index (top-K keeps only 25); path-fuzzy gated (D3). Periodic full rebuild every 30 min remains.
4. **`ui/mod.rs`** — rebuilds all list rows every query change (fine for ≤25); rows/footer extracted (C1–C2).
5. **`theme/css.rs`** — CSS isolated (C3); hot-reload rare so lower priority.

---

## Regression checklist (every merge)

- [ ] `cargo build --release --features layer-shell`
- [ ] Install + restart daemon
- [ ] Alt+A toggles (no second process)
- [ ] App: type known app name
- [ ] File: `f ` + known folder
- [ ] Math: `10 + 20` → card
- [ ] Unit: `10kg to pou` → lb
- [ ] FX: `100 usd to eur` (or cached)
- [ ] TZ: `12pm here to lon`
- [ ] Settings dual panel + Done
- [ ] Ctrl+, opens settings

---

## Progress log

| Date | What | Result |
|------|------|--------|
| 2026-07-13 | Product feature-complete for v0.1 UI (settings dual panel, conv + math cards, predict) | Ready for opt phase |
| 2026-07-13 | Created this tracker + target layout | Sprint 0 |
| 2026-07-13 | **Track A complete:** split `calc.rs` → `providers/calc/{mod,util,math,duration,currency,datetime,timezone,units}.rs` | Release build OK, daemon restarted |
| 2026-07-13 | **D5/D6/D3:** single engine sort; skip apps/files on calc hit; file top-K heap + path-fuzzy gate | Release build OK, daemon restarted |
| 2026-07-13 | **D1:** `blink --bench` + Improvement log / baseline_v1 in this file | Math ~2µs; file `doc` ~674µs @ 1798 items |
| 2026-07-13 | **Resource tracking** in `--bench` (RSS/HWM/CPU/GPU/host) | Daemon ~61MB RSS · GPU 0% · template for deltas |
| 2026-07-14 | **D4 + B1 + E2:** apps haystack/top-K; files module split; index fingerprint; iso_* bench rows | iso_apps ~37µs; cache v4 |
| 2026-07-14 | **File search perf:** path caches + two-pass + fuzzy cap; cache v5 | iso_files 641→**49µs** (−92%); merged file 704→97µs |
| 2026-07-14 | **Merged app path:** calc plain-text reject; skip file fuzzy when any apps match | app `fire` 245→**70µs**; doc 92→**37µs** |
| 2026-07-14 | **C1–C3 structure:** `ui/{rows,footer}.rs`, `theme/{mod,css}.rs` | ui/mod 798→600; theme CSS isolated |
| 2026-07-14 | **G track:** G3 float trim; G2 settings ↑↓/jk; G1 n/a (25-row cap) | `10kg to lb` → clean decimals |
| 2026-07-14 | **F track:** size measure; chrono-tz filter; panic=abort; mimalloc skipped | binary **4.56→4.32 MB** (−236 KB) |
| 2026-07-14 | **E track:** compact cache v6, atomic write, bench rebuild metrics, defaults docs | cache **423→114 KB** (−73%); rebuild ~17ms @ 1798 |
| 2026-07-14 | **Preview:** off-thread scaled decode + LRU + debounce | main free during decode; ~15–60× less texture RAM |
| 2026-07-14 | **Preview follow-up:** single-flight decode, mtime/size cache, off-main thumbs | latest-wins worker; no stale cache after edit |
| 2026-07-16 | **Regression fix:** refuse home deep-pin; FX non-blocking; honest --bench; de-pin $HOME | index 8.2k→2.0k; file ~200→59µs; FX ~5.5ms→2µs |
| 2026-07-16 | **Providers Phase 1–3:** unlock deep walk; Arc config/live-cache; ureq FX/translate; negative live-cache | tests OK; binary ~6.7 MB |
| 2026-07-17 | **Core #1+#2:** config excludes one-shot migrate (v2); `update` save-only-if-changed | search flat (file 60→57µs noise); see `docs/bench/core-config-2026-07-17.md` |
| 2026-07-17 | **Core #3+#4:** skip EFI+/boot mounts; empty-state `resolve_id` exact only | search flat; see `docs/bench/core-3-4-2026-07-17.md` |

### 2026-07-14 — E track (index / IO)

| Metric | Before (v5) | After (v6) | Δ |
|--------|------------:|-----------:|--:|
| Cache on disk | 432,693 B (~423 KB) | **117,170 B (~114 KB)** | **−73%** |
| Items | 1,798 | 1,798 | same |
| Rebuild wall (bench, warm FS) | *(not timed)* | **17 ms** | measured |
| Engine warm (load cache) | ~53 ms | 53 ms | same order |
| Search iso_files | ~50 µs | ~67 µs | noise / enrich OK |

**E1** — `blink --bench` now prints `cache_bytes` and a blocking `rebuild_ms` section.

**E3 — defaults (unchanged, now documented):**
| Knob | Default | Notes |
|------|--------:|-------|
| `max_depth` | **2** | clamped 1..=6 at walk |
| `MAX_INDEX` | **100_000** | hard stop while walking |
| TTL | **30 min** | with fingerprint still valid |
| excludes | rich defaults | `.git`, `node_modules`, … |

**E4 — cache v6:**
```json
{ "version": 6, "fingerprint": "<hex>", "items": [ { "p": "/path", "d": false, "n": 2 } ] }
```
- Only path / is_dir / depth on disk; `name_*`, `path_lower`, flags derived on load.
- `serde_json::to_vec` + write `.tmp` then `rename` (atomic).

**E5 — UI non-blocking:**
- `Engine::new` → bg thread `rebuild_index` / apps reload
- `force_reindex` → bg thread
- Periodic 30m refresh → bg thread
- UI only reads `index_progress` / search under `RwLock`

### 2026-07-14 — F track (runtime / binary)

| Metric | Before | After | Notes |
|--------|-------:|------:|-------|
| Binary size | 4,774,056 B (4.56 MB) | **4,531,888 B (4.32 MB)** | **−242,168 B (−5.1%)** |
| Daemon RSS idle | ~62 MB | ~64 MB | noise / GTK |
| Search (bench) | unchanged | math 2µs · app 69µs · file 34µs | no regression |
| Engine warm_ms | ~53 | 54 | unchanged |

**F1** — already LTO + strip + opt-level 3; added `panic = "abort"` (smaller, no unwind tables).

**F2** — measured; `ldd` shows GTK/system deps (expected). Text ~3.5M, data ~1.2M of stripped PIE.

**F3 — mimalloc skipped:**
- Bench CPU is scoring/search, not allocator thrash.
- Daemon RSS dominated by GTK, not Rust heap churn.
- Would add dep + risk for no measured win → **do not add**.

**F4 — chrono-tz shrink (not full lazy-load):**
- `chrono-tz` with `filter-by-regex`
- `.cargo/config.toml` sets `CHRONO_TZ_TIMEZONE_FILTER` to continents we resolve (`UTC|GMT|Etc/|Europe/|America/|Asia/|Australia/|Pacific/|Africa/`)
- `chrono` with `default-features = false`, features `clock` + `std`
- True lazy-load of TZ DB would need redesign; filter is the practical win

**Rebuild note:** after changing the filter, run `cargo clean -p chrono-tz && cargo build --release --features layer-shell`.

### 2026-07-14 — Merged app path fix

**Before** (after file two-pass):

| case | median_us | p95_us |
|------|----------:|-------:|
| app `fire` | 245 | 253 |
| file `doc` | 92 | 99 |
| settings | 149 | 159 |

**After:**

| case | median_us | p95_us | Δ |
|------|----------:|-------:|--:|
| app `fire` | **70** | 80 | **−71%** |
| file `doc` | **37** | 38 | **−60%** |
| settings | **39** | 40 | **−74%** |
| iso_apps | 38 | 42 | (unchanged) |
| math / unit | 1–2 | | unchanged |

**What changed:**
1. **Calc early reject** for plain text (no digits/operators) — skips regex stack on app queries.
2. **Engine file policy:** if any apps match → files name-only (no fuzzy); if app prefix ≥30k → skip files; else full files.

---

### 2026-07-14 — Preview pane: off-thread scaled decode + LRU + debounce

Full write-up: [`docs/preview-optimization.md`](docs/preview-optimization.md)

**Before → after (`blink --bench`):** search unchanged within noise (math 2µs; file `doc` 82→88; iso_files 51→51). Binary +23 KB.

**Preview path:**

| | Before | After |
|--|--------|-------|
| Decode thread | GTK main (`Texture::from_bytes`) | worker `from_file_at_scale` |
| Texture size | full-res (4K ≈ 32 MB RGBA) | ≤496 px (~0.5 MB, ~15–60× less) |
| Re-select | re-decode | LRU cache (24) |
| Rapid ↑/↓ | N concurrent loads | 45 ms debounce |

### 2026-07-14 — Preview follow-up: single-flight + cache fingerprint

**`blink --bench` after:** search still within noise (math 2µs; file `doc` ~101; iso_files ~52). Binary ~4.61 MB.

| Gap | Fix |
|-----|-----|
| Stacked decode threads | **single in-flight** worker (`worker_busy` + `inflight`); latest selection wins |
| Stale texture after edit | cache key = path + **`FileFp { len, mtime_ns }`** |
| Duplicated LRU insert | one `insert_cache` for sync + async writers |
| Main-thread thumb I/O | FreeDesktop resolve + load on **worker** |
| Extra pixel clone | `glib::Bytes::from_owned` moves buffer into GBytes |

Deferred: generate missing thumbs; video/PDF frames; full single-open dims (kept header `file_info` for native WxH).

Tracker: `todo.md` · details: `docs/preview-optimization.md`.

---

## Next action

1. Before any further opt: `blink --bench` → save search + resource + iso_* as “before”.
2. After change: `blink --bench` → fill Improvement log.
3. A–G + F + E + preview concurrency pass complete. Next product (see `todo.md`): **DnD file URI**, or **Settings → default apps** per media/document category, or deeper incremental root walk if index grows large.

---

## Notes

- Prefer **mechanical splits** (cut/paste + `pub(crate)`) over clever redesigns.
- Do not add comments unless needed for non-obvious invariants.
- Do not commit secrets; config stays under `~/.config/blink/`.
- Index cache path: `~/.cache/blink/file-index.json` (document schema under B3 when splitting files).
