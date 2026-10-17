# Hark — optimization tracker

**Last updated:** 2026-10-17  
**Metrics:** [`performance.md`](./performance.md) (§ Re-run 2026-09-18: apps ~7× faster vs 07, file/µs tracks 3.2× index growth, calc expansion free)  
**Depth raw data:** [`depth-index-benchmark.json`](./depth-index-benchmark.json)  
**Bench logs:** [`bench/`](./bench/)  
**Full historical worklog:** [`archive/optimization-tracker-2026-07-full.md`](./archive/optimization-tracker-2026-07-full.md)

---

## How to measure

```bash
# Install / daemon (lean binary)
cargo build --release --features layer-shell

# Micro-bench harness
cargo build --release --features "layer-shell,bench"
./target/release/hark --bench | tee docs/bench/run-$(date +%F).txt
```

Update [`performance.md`](./performance.md) when defaults or the reference machine change.  
Store batch notes under [`bench/`](./bench/) — do **not** paste full tables back into this file.

---

## Done (high level)

| Track | Outcome |
|-------|---------|
| **A** Calc split | `providers/calc/*` |
| **B** Files split | `providers/files/{mod,index,search,hot}` |
| **C** UI / theme split | `ui/*`, `theme/*` |
| **D** Search | Calc short-circuit, top-K, two-pass files, app haystack, merge policy |
| **E** Index I/O | Fingerprint (config-only) + TTL content re-walk, cache **v9**, background rebuild |
| **F** Binary | LTO, strip, `panic=abort`, chrono-tz filter; `bench` feature optional |
| **G** UX polish | Settings navigation, float trim, … |
| **Preview** | Off-main decode, LRU, thumbs, video/PDF soft deps |
| **Core hygiene** | Config dirty-save, usage debounce/cap, IPC, excludes, headless CLI |
| **Hot-path free-text** | Usage hot set; skip full scan when strong + len≥4; short queries = baseline |
| **Cold-start W4** | `hark-hotkey` GTK-free stub: 2.7 ms press p50 (same-harness `hark` 27.8 ms); execs `hark` for flags/cold start |
| **Cold-start W3** | Ghost warm-up: invisible warm window ~2 s after daemon boot; first press 0 ms present / 8 ms to frame (cold 153/230); promote 4 ms; kill switch `ui.ghost_warmup` / `HARK_NO_GHOST` |

Narrative + old before/after tables: [archive](./archive/optimization-tracker-2026-07-full.md).  
Hot-path runs: [`bench/hot-path-*`](./bench/).

---

## Open (optional next)

| ID | Item | Notes |
|----|------|--------|
| S1 | Cold-start plan (W1–W6) | [`cold-start-plan.md`](./cold-start-plan.md) — W1–W4 landed; remaining: W5 experiments (optional) |
| H2 | Bench `file_hot` / `file_cold` | Prove hot short-circuit with seeded usage |
| U1 | UI result row pool | Largest remaining *feel* win (ListBox rebuild) |
| P1 | Don’t hold index lock during live deep walk | Provider concurrency |
| C1 | Split `config` mounts module | Only if `config.rs` grows again |

Product gaps **not** in scope for optimization (intentionally deferred) are listed in [`FEATURES.md`](../FEATURES.md) §14: multi-select DnD, per-extension open overrides, one-shot “Open with…” on results.

---

## Next actions

1. Keep default **max_depth = 2**; re-bench before raising depth.  
2. Optional: hot/cold file bench cases (H2).  
3. Next *feel* work: UI row reuse (U1), not more µs on short free-text.  
4. Prefer linking files under [`bench/`](./bench/) over growing this tracker.

---

## Notes

- Index cache: `~/.cache/hark/file-index.json` (v9).  
- Bind `hark-hotkey` instead of `hark` for the fast press; it falls back to
  `hark` automatically (cold start, `--search`, `update`, help).  
- Config: `~/.config/hark/config.json`.  
- Daemon install should use **`--features layer-shell`** without `bench` for a smaller binary.
