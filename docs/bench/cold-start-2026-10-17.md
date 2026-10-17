# Cold-start baseline — 2026-10-17 (M0, post-W2)

Evidence for [`cold-start-plan.md`](../cold-start-plan.md) §7 / M0. Lab only:
private `XDG_*` dirs, headless swayfx (invisible), private D-Bus; only tracked
PIDs were killed — the live session's daemon was never touched.

## Environment

- Binary: `target/release/hark` (layer-shell), 7,775,480 B
- Dynamic deps: `ldd` 115 lines · 12 `NEEDED` · 48,549 `R_X86_64` relocs
- Harness: `run-m0.sh` + `measure.py` (wall from `monotonic`; CPU/RSS sampled
  from `/proc`; CPU ticks quantize at 10 ms)

## Results

| Metric | Result | Notes |
|---|---:|---|
| Warm press, 30 runs | **p50 28.1 ms** (25.8–48.3, p90 35.7) | ~all exec; W4 target ≤5 ms, same harness |
| Stale-socket overhead, 5+5 runs | **0.1 ms** | 29.9 vs 29.8 ms absolute in a no-display env — target ≤10 ms |
| Daemon boot → socket ready | **37.1 ms** | single `--watch-socket` run |
| `hark --help` socket-after | **absent 3/3** | present 3/3 before W2 (commit `980ceb9`) |
| IPC test suite (incl. ignored socket tests) | 11/11 pass | revert-proofs recorded in the plan (§W2) |

Caveats: the press wall includes the daemon `ok\n` ack read, so it is not
directly comparable to the `/usr/bin/time -v` numbers in `performance.md`;
it is the like-for-like baseline for W4's stub-client comparison.

## Raw

[`cold-start-2026-10-17.txt`](./cold-start-2026-10-17.txt) — full per-run JSON.
