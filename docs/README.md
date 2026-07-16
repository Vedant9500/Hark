# Blink docs

| Doc | What |
|-----|------|
| **[performance.md](./performance.md)** | Search latency, index depth chart, binary size, RAM/CPU, how to re-bench |
| **[power_performance.md](./power_performance.md)** | Blink vs Rofi: power, memory, CPU spikes, background processes, which to pick |
| **[battery-optimization.md](./battery-optimization.md)** | How to optimize Blink for battery + low CPU spikes |
| **[preview-optimization.md](./preview-optimization.md)** | Preview pane decode / cache optimization tracker |
| **[translation.md](../translation.md)** | Translate-on-paste (CJK) implementation plan |
| **[depth-index-benchmark.json](./depth-index-benchmark.json)** | Raw depth 2/3/4 measurement (2026-07-14) |
| **[index-regression-depth-analysis.md](./index-regression-depth-analysis.md)** | Why index hit ~8k without changing scan depth; deep_roots auto-promote; bench caveats (2026-07-16) |
| **[providers-depth-analysis.md](./providers-depth-analysis.md)** | `src/providers` inefficiencies, bugs, dead code, optimization phases (2026-07-16) |
| **[ui-depth-analysis.md](./ui-depth-analysis.md)** | `src/ui` CPU/bugs/optimization plan (lightweight, keep visuals) |

Also see repo root:

| File | What |
|------|------|
| [`OPTIMIZATION.md`](../OPTIMIZATION.md) | Workstream tracker (A–G), improvement log, module layout |
| [`README.md`](../README.md) | User-facing features & install |
| [`FEATURES.md`](../FEATURES.md) | Complete feature list |
