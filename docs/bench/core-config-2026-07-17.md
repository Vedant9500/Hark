# Bench: core config fixes (excludes + dirty `update`)

**Date:** 2026-07-17  
**Machine:** Linux · 16 CPUs · ~15 GB RAM  
**Changes:** audit items **#1** and **#2** from [`core-depth-analysis.md`](../core-depth-analysis.md)

| Fix | Summary |
|-----|---------|
| **#1** | Config `version` v2: seed default excludes **once** on migrate; never re-merge on later loads so Settings removals stick |
| **#2** | `ConfigStore::update` compares before/after (`PartialEq`); skip Arc swap + disk write on no-ops |

**Binaries:**

| | Path | Size | Built |
|--|------|-----:|-------|
| Baseline | `target/release/blink` (pre-change, 2026-07-16) | 6,728,872 B | pre-fix |
| After | `target/release/blink` `--features layer-shell` | 6,744,584 B | 2026-07-17 |

Raw logs: [`core-config-2026-07-17-baseline.txt`](./core-config-2026-07-17-baseline.txt) · [`core-config-2026-07-17-after.txt`](./core-config-2026-07-17-after.txt)

---

## Index / warm (context)

| Metric | Baseline | After | Notes |
|--------|---------:|------:|-------|
| Index items | 2164 | 2165 | Noise / FS drift |
| warm_ms | 56 | 56 | Unchanged |
| cache_bytes | 142326 | 142326 | Unchanged |
| rebuild_ms | 19 | 24 | Noise (blocking rebuild) |
| apps | 35 | 35 | |

These fixes are **correctness / idle I/O**, not search-path optimisations. Expect flat search medians.

---

## `engine.search` (merged) — median_us

| case | query | baseline | after | Δ |
|------|-------|---------:|------:|--:|
| math | `10 + 20` | 2 | 2 | 0 |
| unit | `10kg to lb` | 2 | 2 | 0 |
| unit_partial | `10kg to pou` | 4 | 4 | 0 |
| fx | `100 usd to eur` | 2 | 1 | −1 |
| app | `chrom` | 47 | 44 | −3 |
| file | `doc` | 60 | 57 | −3 |
| file_force | `f doc` | 57 | 54 | −3 |
| settings | `settings` | 93 | 95 | +2 |

## Isolated providers — median_us

| case | baseline | after | Δ |
|------|---------:|------:|--:|
| iso_apps `chrom` | 2 | 1 | −1 |
| iso_files `doc` | 54 | 51 | −3 |
| iso_calc `10 + 20` | 2 | 1 | −1 |

## Resources (`--bench` process)

| Metric | Baseline | After |
|--------|---------:|------:|
| RSS start → end (kb) | 26604 → 33328 | 27404 → 34128 |
| HWM kb | 33328 | 34128 |
| CPU total ms | 70 | 70 |
| Wall (resource section) | 104 ms | 105 ms |
| binary_bytes | 6728872 | 6744584 (+15.7 KB; layer-shell feature in after build) |

**Verdict:** Search latency **flat within noise** (no regression). Binary size delta is dominated by rebuilding **with** `layer-shell` vs the older artifact, not by these config edits.

---

## Behavioural checks (not in `--bench`)

| Check | Expected after fix |
|-------|--------------------|
| Remove `node_modules` from excludes → restart daemon | Still removed (`version >= 2`, no re-merge) |
| `promote_deep_root` when already pinned | `config.json` mtime unchanged |
| `update(\|c\| { let _ = c.index.path_style; })` | No disk write (unit test) |
| Real `update` (e.g. `max_depth = 3`) | Atomic rewrite (unit test) |

Unit tests: `cargo test config_store_tests` → 3 passed.

---

## How to re-run later

```bash
cargo build --release --features layer-shell
./target/release/blink --bench | tee docs/bench/core-config-YYYY-MM-DD-run.txt
# Compare medians to tables above / baseline txt
```
