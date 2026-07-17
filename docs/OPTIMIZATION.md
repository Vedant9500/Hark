# Blink — optimization tracker

**Last updated:** 2026-07-17  
**Metrics:** [`performance.md`](./performance.md)  
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
./target/release/blink --bench | tee docs/bench/run-$(date +%F).txt
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
| **E** Index I/O | Fingerprint + TTL, cache **v6**, background rebuild |
| **F** Binary | LTO, strip, `panic=abort`, chrono-tz filter; `bench` feature optional |
| **G** UX polish | Settings navigation, float trim, … |
| **Preview** | Off-main decode, LRU, thumbs, video/PDF soft deps |
| **Core hygiene** | Config dirty-save, usage debounce/cap, IPC, excludes, headless CLI |
| **Hot-path free-text** | Usage hot set; skip full scan when strong + len≥4; short queries = baseline |

Narrative + old before/after tables: [archive](./archive/optimization-tracker-2026-07-full.md).  
Hot-path runs: [`bench/hot-path-*`](./bench/).

---

## Open (optional next)

| ID | Item | Notes |
|----|------|--------|
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

- Index cache: `~/.cache/blink/file-index.json` (v6).  
- Config: `~/.config/blink/config.json`.  
- Daemon install should use **`--features layer-shell`** without `bench` for a smaller binary.
