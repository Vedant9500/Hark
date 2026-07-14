# Blink — performance reference

**Last updated:** 2026-07-14  
**Machine (reference runs):** Linux / Hyprland · 16 CPUs · ~15 GB RAM · RTX 4060 Laptop  
**Binary:** `cargo build --release --features layer-shell` → `~/.local/bin/blink`  
**How to re-measure:** `blink --bench` (daemon optional for RSS rows)

This doc is the **stable reference** for latency, index cost, and size.  
Day-to-day optimization tasks live in [`OPTIMIZATION.md`](../OPTIMIZATION.md).

---

## Quick snapshot (post-optimization, depth = 2)

| Metric | Value | Notes |
|--------|------:|-------|
| Math `10 + 20` | **~2 µs** | calc short-circuit; no apps/files |
| Unit `10kg to lb` | **~2 µs** | |
| Apps only `fire` | **~34 µs** | `iso_apps` |
| Files only `doc` | **~49 µs** | `iso_files` @ 1,798 items |
| App merged `fire` | **~68 µs** | apps + cheap files policy |
| File merged `doc` | **~34 µs** | strong app/name path |
| Force files `f doc` | **~52 µs** | full file provider |
| Index items (default) | **1,798** | home + windows_d, depth 2 |
| Index rebuild | **~15 ms** | blocking full walk (bench) |
| Cache on disk (v6) | **~114 KB** | compact rows |
| Binary size | **~4.3 MB** | stripped + LTO + filtered chrono-tz |
| Daemon RSS (idle) | **~60–64 MB** | GTK-dominated |
| GPU | **~0%** | no CUDA path |

---

## How to benchmark

```bash
cargo build --release --features layer-shell
install -Dm755 target/release/blink ~/.local/bin/blink

# Optional: keep daemon up for daemon RSS section
blink --daemon

# Search + resources + index rebuild timing
blink --bench
```

`blink --bench` prints:

1. **Engine warm** — items, warm_ms, cache_bytes  
2. **Index rebuild** — blocking `rebuild_ms` (bench-only API)  
3. **Merged search** — median / p95 µs per case  
4. **Isolated providers** — `iso_apps`, `iso_files`, `iso_calc`  
5. **Resources** — RSS/HWM/CPU/GPU/host/binary size  

**Rules for fair comparisons**

- Always release + `layer-shell`  
- Same machine, same index roots  
- Note `max_depth` and item count  
- Prefer **iso_*** rows when judging a single provider  

---

## Index depth chart (important)

Changing **scan depth** (Settings → Indexing → **− / +**, or `max_depth` in config) multiplies index size and cost.

### Setup for this chart

| Knob | Value |
|------|--------|
| Roots | `include_home` + `/mnt/windows_d` (data/C off) |
| Cache format | **v6** compact (`p` / `d` / `n` only on disk) |
| Cap | 100,000 items (not hit) |
| TTL | 30 minutes + fingerprint |
| Date | 2026-07-14 |

Raw JSON: [`depth-index-benchmark.json`](./depth-index-benchmark.json)

### Results

| depth | items | rebuild | cache | iso_files `doc` | `f doc` | app `fire` merged | relative items |
|------:|------:|--------:|------:|----------------:|--------:|------------------:|---------------:|
| **2** (default) | 1,798 | **15 ms** | **114 KB** | **49 µs** | 52 µs | 68 µs | ×1.0 |
| **3** | 7,220 | **48 ms** | **560 KB** | **128 µs** | 130 µs | 150 µs | **×4.0** |
| **4** | 14,831 | **106 ms** | **1.2 MB** | **352 µs** | 327 µs | 347 µs | **×8.3** |

### Relative cost (vs depth 2)

| depth | items | rebuild time | cache size | iso_files latency |
|------:|------:|-------------:|-----------:|------------------:|
| 2 | ×1.00 | ×1.00 | ×1.00 | ×1.00 |
| 3 | ×4.02 | ×3.20 | ×4.90 | ×2.6 |
| 4 | ×8.25 | ×7.07 | ×10.5 | ×7.2 |

### What stays flat

| Path | All depths |
|------|------------|
| Math / units / FX | ~1–2 µs |
| `iso_apps` | ~34 µs |

Calc short-circuit and app-only isolation do not scale with the file index.

### Recommendations

| Depth | When to use |
|------:|-------------|
| **2** | **Default** — snappy, small cache |
| **3** | Want more files under projects; still &lt;~150 µs search, &lt;50 ms rebuild here |
| **4** | Deep trees; ~7–10× cost — only if needed |
| 5–6 | Allowed (clamp); re-bench before shipping as default |

**UI:** Settings → **Indexing** → **Scan depth** (− / +). Changing depth **auto-rebuilds** the index and updates `~/.config/blink/config.json`.

### Pitfall (fixed 2026-07-14)

`ConfigStore::load` used to clamp `max_depth > 3` back to **2**, so depth 4 never applied. Clamp is now **1..=6** (matches the walker).

---

## Search optimization timeline (high level)

Approximate **merged** / **isolated** latency at **depth 2 · ~1.8k items**:

| Era | app `fire` | file `doc` / iso_files | Notes |
|-----|----------:|-----------------------:|-------|
| Early (pre-instrument) | — | full scan + double sort | not measured |
| baseline_v1 (after D3/D5/D6) | ~410–450 µs merged | ~640–670 µs iso | top-K heap, calc short-circuit |
| After D4 apps cache | iso_apps **~37 µs** | — | haystack + name_lower |
| After file two-pass + path caches | — | iso_files **~49 µs** | path_lower + name-first pass |
| After merged app policy + calc plain-text reject | merged **~70 µs** | merged file **~35 µs** | skip file fuzzy when apps hit |
| Current (depth 2) | **~68 µs** | **~49 µs** iso | + compact cache v6 |

Detailed before/after rows: [`OPTIMIZATION.md`](../OPTIMIZATION.md) Improvement log.

### Techniques that mattered most

1. **Calc short-circuit** — skip apps/files on calc/conversion hits  
2. **Calc plain-text reject** — skip regex stack on app-like queries  
3. **File two-pass** — name exact/prefix/substring first; fuzzy only if needed  
4. **Path metadata cache** — `path_lower`, low/high value flags  
5. **Top-K heaps** — keep only 25 results while scanning  
6. **App haystack** — prebuilt search string + name_lower  
7. **Merged policy** — if any apps match → files name-only (no path fuzzy)  
8. **Index fingerprint + TTL** — skip useless rebuilds  
9. **Compact cache v6** — smaller disk; derive fields on load  

---

## Index / cache

| Item | Value |
|------|--------|
| Path | `~/.cache/blink/file-index.json` |
| Meta | `~/.cache/blink/file-index.meta` → `version ts fingerprint` |
| Version | **6** |
| Schema | `{ "version", "fingerprint", "items": [ { "p", "d", "n" } ] }` |
| Write | atomic: `.json.tmp` → rename |
| Cap | `MAX_INDEX = 100_000` |
| TTL | 30 minutes (if fingerprint still matches) |
| Rebuild triggers | TTL expired **or** fingerprint change (roots / depth / excludes / version) |

### Cache size evolution (same ~1.8k items)

| Version | Approx size | Shape |
|--------:|------------:|-------|
| v3–v4 | ~236 KB | fuller JSON |
| v5 | ~423 KB | + path_lower + flags on disk |
| **v6** | **~114 KB** | path + is_dir + depth only |

---

## Binary / runtime (F track)

| Metric | Approx |
|--------|--------|
| Binary | **~4.32 MB** (was ~4.56 MB) |
| Change | −~236 KB (−5%) |
| Stack | LTO, `opt-level=3`, `strip`, `panic=abort` |
| chrono-tz | `filter-by-regex` + `CHRONO_TZ_TIMEZONE_FILTER` in `.cargo/config.toml` |
| mimalloc | **Not used** — no allocator evidence; GTK owns RSS |

Rebuild after TZ filter changes:

```bash
cargo clean -p chrono-tz
cargo build --release --features layer-shell
```

---

## Resources (typical idle daemon)

| Metric | Typical |
|--------|--------:|
| RSS | 60–65 MB |
| Threads | ~7–8 |
| CPU idle | &lt;1% |
| GPU util | 0% (system-wide sample) |

Panel open can inflate RSS (GTK/CSS); compare idle daemon for baselines.

---

## Defaults checklist

| Setting | Default | Where |
|---------|--------:|-------|
| Index depth | **2** | Settings → Indexing, or `config.json` `index.max_depth` |
| Depth clamp | 1–6 | walker + config load |
| Index cap | 100,000 | `files/index.rs` `MAX_INDEX` |
| TTL | 30 min | `INDEX_TTL_SECS` |
| Path style | Label / Drive | Settings → Display |

---

## Re-running the depth study

```bash
# For each depth in 2 3 4:
# 1. Set max_depth in ~/.config/blink/config.json
# 2. rm ~/.cache/blink/file-index.json ~/.cache/blink/file-index.meta
# 3. blink --bench | tee docs/bench-depth-N.txt
# 4. Restore max_depth=2 and rebuild cache
```

Update [`depth-index-benchmark.json`](./depth-index-benchmark.json) and the table above when roots or machine change.

---

## Related code

| Area | Path |
|------|------|
| Engine merge / short-circuit | `src/engine.rs` |
| File index + cache | `src/providers/files/index.rs` |
| File search | `src/providers/files/search.rs` |
| Apps | `src/providers/apps.rs` |
| Calc | `src/providers/calc/` |
| Bench CLI | `src/main.rs` → `run_bench` |
| Depth UI | `src/ui/settings.rs` |
