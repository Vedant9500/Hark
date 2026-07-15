# Battery & CPU optimization for Blink

**Goal:** best battery life and low CPU spikes.  
**Out of scope (for now):** shrinking the ~60 MB idle RSS — that is mostly GTK and is a weak battery lever compared to wakeups / disk / network.

**Last updated:** 2026-07-15  

Related: [`power_performance.md`](./power_performance.md), [`performance.md`](./performance.md).

---

## Why RAM is the wrong first target

| Cost | Idle impact | Spike impact |
|------|-------------|--------------|
| ~60 MB resident | small DRAM power | none if idle |
| Periodic index walk | **medium** (disk + CPU every N min) | **high** when it runs |
| Network (`curl` FX) | wakes radios / power domains | short but expensive |
| 16 ms UI poll / 2 s theme poll | keeps main loop busy | continuous drip |
| Per-keystroke full search + deep walk | — | **high while typing** |
| Preview full-res decode | — | **high on arrow-through images** |

Prefer **fewer wakeups** and **cheaper work on the hot path** over shaving MB of RSS.

---

## Architecture hotspots (code map)

| Hotspot | Location | What it does | Battery issue |
|---------|----------|--------------|---------------|
| Periodic apps+files | `engine.rs` `Engine::new` | sleep → reload apps + `ensure_fresh` | Wake + possible full tree walk |
| File index rebuild | `files/index.rs` | walk roots to depth N | CPU + disk |
| FX warm at boot | `engine.rs` + `fx.rs` | `curl` rates | Network at every login (fixed: deferred) |
| Theme fallback poll | `theme/mod.rs` | was every 2 s | Unnecessary CPU if FileMonitor fails (fixed) |
| Search on every `changed` | `ui/mod.rs` | rebuild list per key | Typing storms (fixed: debounce) |
| Deep result poll @ 16 ms | `ui/mod.rs` | `try_recv` loop | Wakes UI while deep walk runs (fixed: channel) |
| Preview decode | `ui/preview.rs` | image scale/decode | Arrow-key spikes (partially optimized) |
| Always-on GTK daemon | `main.rs` | resident process | Necessary for instant toggle; accept ~60 MB |

---

## What we shipped (2026-07-15)

| Change | Effect |
|--------|--------|
| **No FX network at daemon boot** | Rates load from disk; `curl` only when a currency conversion is requested |
| **Periodic refresh 30m → 45m** | Fewer background wakes; still TTL-aligned enough |
| **`ensure_fresh` RAM fast-path** | If in-memory index is still valid (TTL + fingerprint), skip cache re-read / rebuild |
| **Theme: drop 2 s poll fallback** | Apply once if monitor fails; no continuous timer |
| **Search debounce 40 ms** | One search per typing pause instead of per key |
| **Deep hits via `async_channel`** | No 16 ms busy-poll on the GTK main loop |
| **`hide()` cancels debounce + bumps `deep_gen` + clears preview** | No background decode/search after Esc |

Measure with `blink --bench` (latency) and `ps` / `perf` while typing (spikes).

---

## Priority backlog (next)

### P0 — still worth it for spikes / battery

1. **Search on hide should stay free** — already cancels work; verify no deep thread continues after hide (generation check is enough; thread may finish but result is dropped). Optional: cooperative cancel token for deep walk.
2. **Preview** — keep scaled decode, debounce, LRU (see `preview-optimization.md`); never decode when window hidden.
3. **Apps reload cheap path** — periodic `apps.reload()` re-reads all `.desktop` files. Prefer mtime of desktop dirs or `inotify`/`gio::FileMonitor` and reload only when changed.
4. **Cap concurrent deep walks** — if user types quickly, older threads still run. Single global “deep worker” with cancel/latest-query-wins.

### P1 — power without UX loss

5. **Index only when panel is used often** — optional: skip periodic rebuild entirely; rebuild on first show after TTL, or Settings only. Tradeoff: first open after hours may hitch.
6. **Configurable refresh interval** in Settings (Battery section): e.g. 30 / 45 / 90 min / “on open only”.
7. **Defer non-critical work until first show** — start daemon with minimal GTK; load file index after first hotkey (slower first open, cooler idle). Flag: `blink --daemon --lazy-index`.
8. **FX:** never block UI on `curl`; keep fetch on a worker with timeout; prefer stale rates over hanging.

### P2 — later / optional

9. **Memory (later)** — destroy window widgets on hide and rebuild on show (saves RAM, costs open latency); drop texture cache on hide; smaller CSS/icon themes. Not first for battery.
10. **Process niceness** — `nice` / `ionice` on index rebuild thread so rebuilds don’t contend with interactive work.
11. **cgroup / systemd user unit** — `CPUQuota=`, `MemoryHigh=` for the daemon if you want hard caps.

---

## Recommended defaults for “best battery”

| Setting | Recommendation | Why |
|---------|----------------|-----|
| Index depth | **2** | Rebuild & search cost scale ~linear with items |
| Extra roots / deep roots | Only real project dirs | Deep roots → depth 6 walks |
| Daemon | Keep `blink --daemon` | Instant open; RAM tax is OK per product goals |
| Autostart FX network | Off (current) | No radio wake at login |
| Periodic refresh | 45+ min or on-open | Fewer background walks |

---

## How to verify improvements

### Idle (daemon only)

```bash
blink --daemon &
sleep 5
# Should stay near 0% CPU
pidstat -p $(pgrep -f 'blink --daemon' | head -1) 1 10
# Or:
top -b -n 3 -d 1 -p $(pgrep -f 'blink --daemon' | head -1)
```

Expect: **~0% CPU** after warm-up; no `curl` in process tree until FX use.

### Typing spikes

```bash
# In one terminal, watch CPU while typing fast in Blink UI
pidstat -u -p $(pgrep -f 'blink --daemon' | head -1) 0.2
```

Expect: lower peak % with debounce vs per-key refresh.

### Bench (must not regress badly)

```bash
blink --bench
```

Math/apps/files medians should stay in the same ballpark as `docs/performance.md`.

### Power (optional)

```bash
# Compare 10 min idle with daemon vs without (same session otherwise)
sudo powerstat -d 0 60
```

---

## Design rules (ongoing)

1. **No polling** on the GTK main loop while waiting for workers — use channels / idle once.
2. **No network** until the user action needs it.
3. **No full index rebuild** if fingerprint + TTL still valid.
4. **Debounce** any path that runs on every keystroke or selection.
5. **Cancel or ignore** work when the window hides.
6. **Measure** before/after with `--bench` + a typing `pidstat` sample.

---

## Changelog

| Date | Notes |
|------|-------|
| 2026-07-15 | Initial plan + P0 code: FX defer, 45m period, ensure_fresh fast-path, theme no-poll, search debounce, deep channel, hide cleanup |
