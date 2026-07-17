# Blink docs

## Start here

| Doc | What |
|-----|------|
| **[performance.md](./performance.md)** | Latency, index depth, cache, binary, how to bench |
| **[OPTIMIZATION.md](./OPTIMIZATION.md)** | Short tracker (done / open / measure) |
| **[../README.md](../README.md)** | Install & overview |
| **[../FEATURES.md](../FEATURES.md)** | User-facing feature list |
| **[../todo.md](../todo.md)** | Product backlog / known gaps |

## Data & history

| Path | What |
|------|------|
| **[bench/](./bench/)** | Before/after `blink --bench` logs (hot-path, etc.) |
| **[depth-index-benchmark.json](./depth-index-benchmark.json)** | Raw depth 2/3/4 campaign (2026-07-14) |
| **[archive/](./archive/)** | Full historical optimization tracker (pre-cleanup) |

## Conventions

- **Tracker stays short** — no full bench tables; link `bench/*.txt` instead.
- **Metrics live in** `performance.md` — update when defaults or the reference machine change.
- **Raw depth numbers** stay in the JSON; narrative stays in `performance.md`.
- Old deep-dive analysis docs (core/ui/providers, power, battery, hot-path design) were removed 2026-07-17; history is in git + `archive/`.
