# Blink — performance reference

**Last updated:** 2026-07-17  
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
| Math / units / FX | **~1–4 µs** | calc short-circuit |
| Apps isolated | **~1–40 µs** | query-dependent |
| Files isolated `doc` | **~50–70 µs** | ~2k items, short query = full scan |
| File merged `doc` | **~55–80 µs** | + engine merge |
| Strong hot free-text (len≥4) | **≪ full** | skips full index when usage hot hits |
| Index items (depth 2) | **~1.8–2.2k** | home + mounts; varies |
| Index rebuild | **~15–25 ms** | blocking bench rebuild |
| Cache on disk (v6) | **~100–150 KB** | ~2k items |
| Binary (layer-shell, no bench) | **~6.4–6.7 MB** | stripped + LTO; grows with features |
| Daemon RSS idle | **~60–65 MB** | GTK-dominated |
| GPU | **~0%** | no CUDA path |

Older depth-2 campaign (~1.8k items, 2026-07-14): see JSON + table below.

---

## How to benchmark

```bash
cargo build --release --features "layer-shell,bench"
./target/release/blink --bench
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
# 1. Set index.max_depth in ~/.config/blink/config.json
# 2. rm ~/.cache/blink/file-index.json ~/.cache/blink/file-index.meta
# 3. cargo build --release --features "layer-shell,bench" && ./target/release/blink --bench
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
| Path | `~/.cache/blink/file-index.json` |
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
| Release + layer-shell | ~6.4–6.7 MB today (was ~4.3–4.5 MB early; deps/features grew) |
| Profile | LTO, `opt-level=3`, strip, `panic=abort` |
| Idle daemon RSS | ~60–65 MB |
| Idle CPU | &lt;1% |

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
| Bench CLI | `src/bench.rs` (`--features bench`) |
