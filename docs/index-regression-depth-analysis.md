# Index size & search regression — depth analysis

**Date:** 2026-07-16  
**Status:** diagnosis (no code changes in this note)  
**Trigger:** `blink --bench` showed ~8.2k index items and multi‑× slower search vs 2026-07-14 D/E baselines, while the user had **not** changed global scan depth in Settings.

Related:

- Tracker / D–E history: [`OPTIMIZATION.md`](../OPTIMIZATION.md)
- Stable perf reference: [`performance.md`](./performance.md)
- Depth chart raw data: [`depth-index-benchmark.json`](./depth-index-benchmark.json)

---

## TL;DR

| Question | Answer |
|----------|--------|
| Did you raise **Scan depth**? | **No** — `max_depth` is still **2**. |
| Why is the index ~8k instead of ~1.8k? | **`index.deep_roots` pinned `$HOME` to depth 6** (plus `~/blink`). That alone added **~6.2k** items. |
| How did `$HOME` get pinned? | **Auto-promote on open**: walking up from an opened path finds **`~/package.json`** and treats `$HOME` as a “project root”. |
| Is search “broken”? | **Mostly workload**: latency scales with index size. A smaller residual gap remains vs the depth‑3 chart at similar item counts. |
| Math still OK? | **Yes** — ~2 µs (D6 short-circuit intact). |

**Primary fix (config, immediate):** remove `/home/vedant` from `deep_roots` (keep real projects like `~/blink` if wanted), rebuild index. Expect ~**2k** items again.

**Primary fix (code, prevent recurrence):** never promote a path that is the user’s home (or other overly broad roots); optionally require markers *and* not home; add excludes for known junk trees under deep walks.

---

## 1. What the baselines said (2026-07-14)

Post D/E optimization at **default depth 2**, without deep home pin:

| Metric | Baseline (docs / OPTIMIZATION log) |
|--------|-----------------------------------:|
| Index items | **1,798** |
| Rebuild | **~15–17 ms** |
| Cache (v6) | **~114 KB** |
| Warm load | **~53 ms** |
| Math `10 + 20` | **~2 µs** |
| App merged `fire` | **~68–70 µs** |
| File merged `doc` | **~34–37 µs** (later noise ~80–100) |
| Force files `f doc` | **~52 µs** |
| `iso_files` `doc` | **~49 µs** |
| `iso_apps` `fire` | **~34–37 µs** |
| Binary | **~4.3 MB** |

Depth chart (same machine, roots ≈ home + `/mnt/windows_d`, **no deep_roots**):

| depth | items | rebuild | iso_files `doc` | `f doc` | app `fire` |
|------:|------:|--------:|----------------:|--------:|-----------:|
| 2 | 1,798 | 15 ms | 49 µs | 52 µs | 68 µs |
| 3 | 7,220 | 48 ms | 128 µs | 130 µs | 150 µs |
| 4 | 14,831 | 106 ms | 352 µs | 327 µs | 347 µs |

---

## 2. What we measured now (2026-07-16)

`blink --bench` (release, 3 runs, averages):

| Metric | Current | vs baseline |
|--------|--------:|-------------|
| Index items | **8,203** | **×4.6** |
| Rebuild | **~66 ms** | **×4.4** |
| Cache | **~700 KB** | **×6.1** |
| Warm | **~104 ms** | **×2.0** |
| Math / unit | **2–4 µs** | same |
| FX `100 usd to eur` | **~5.5–5.8 ms** | network/stale rates path (not file search) |
| App merged `fire` | **~350 µs** | **×5** (see §5 — no app hits for “fire”) |
| File `doc` | **~195–200 µs** | **×5–6** |
| `f doc` | **~190–200 µs** | **×3.8** |
| Settings | **~135 µs** | worse |
| Binary | **~5.01 MB** | feature growth since 07-14 |

**Config at measure time** (`~/.config/blink/config.json`):

```json
"max_depth": 2,
"deep_roots": [
  "/home/vedant/blink",
  "/home/vedant"
],
"include_mounts": {
  "/mnt/windows_d": true,
  "/mnt/data": true,
  "/mnt/windows_c": false
}
```

So the global depth knob is still the default. The index is **not** “depth 2 everywhere”.

---

## 3. Why item count exploded without changing depth

### 3.1 Two different depth systems

Blink has **two** depth mechanisms:

| Mechanism | Config | Effect |
|-----------|--------|--------|
| Global scan depth | `index.max_depth` (Settings −/+) | Walks **every** root (home, mounts, extras) to this depth (clamped 1..=6). Default **2**. |
| Deep roots (pins) | `index.deep_roots[]` | **Additional** full walk of each pin to **depth 6**, independent of `max_depth`. |

From `src/providers/files/index.rs`:

1. Walk normal roots with `max_depth` (here: 2).
2. Then walk each `deep_roots` entry with depth **6**.
3. Dedup by path (`seen`).

Pinning **`$HOME`** therefore re-walks the entire home tree six levels deep and keeps every new path. That is equivalent to turning global depth into “home at 6” without touching the Settings depth control — so it is easy to miss.

### 3.2 How `$HOME` got into `deep_roots` (smoking gun)

On open of a file/folder, `Engine::execute(OpenPath)` calls `maybe_auto_promote_deep_root`:

1. Start at the opened path (or its parent).
2. Walk **up to 6 parents**.
3. If any ancestor contains a project marker, **pin that ancestor** as a deep root and force reindex.

Markers (`src/engine.rs`):

```text
.git  Cargo.toml  package.json  pyproject.toml
go.mod  CMakeLists.txt  Makefile  meson.build
```

**This machine has a stray project marker at the home root:**

```text
/home/vedant/package.json   (exists since 2026-07-04)
```

Contents are a one-off dependency dump (`@qoder-ai/qoder-agent-sdk`), **not** a real monorepo root.

**Consequence:** opening *any* path under home within six levels of `$HOME` (almost everything) can walk up and hit `~/package.json` → **`promote_deep_root("/home/vedant")`**.

That matches the live config:

- `/home/vedant` — accidental mega-pin  
- `/home/vedant/blink` — legitimate pin (repo has `Cargo.toml` / `.git`)

Also available in Settings → **Deep roots** (manual pin), but auto-promote alone is enough to explain this without the user changing **Scan depth**.

### 3.3 Quantitative decomposition of the 8,203 items

| Slice | Items | Notes |
|------:|------:|-------|
| **Current total** | **8,203** | cache v6 |
| Approx. **without** deep home (home `n≤2` + mounts) | **~1,983** | matches old ~1.8k order of magnitude |
| **Added only by deep home walk** (`n≥3` under home) | **~6,220** | **~76% of the index** |
| Under `~/blink` deep pin | **~75** | small / intended |
| Deep home **excluding** blink | **~6,187** | the accidental bulk |
| `/mnt/windows_d` | **1,554** | still depth 2 (hist: almost all `n≤2`) |
| `/mnt/data` | **146** | enabled by default for non-`windows_c` mounts |
| Home total | **~6,503** | |

**Depth histogram (overall):**

| depth `n` | count | Role |
|----------:|------:|------|
| 1 | 160 | shallow |
| 2 | 1,823 | normal max_depth + mount tops |
| 3 | 1,146 | deep pin |
| 4 | 1,675 | deep pin |
| 5 | 1,436 | deep pin |
| 6 | 1,963 | deep pin |

Almost all `n≥3` entries are under `/home/vedant` (mounts stay at depth 2).

### 3.4 Where the deep junk actually lives

Top **home** buckets in the live index:

| Bucket | ~items | Why it hurts |
|--------|-------:|--------------|
| `~/grok-cliproxy` | 1,520 | project tree fully deep-indexed |
| `~/.local` (esp. `lib/python3.14`) | 1,852 / 1,288 | site-packages noise |
| `~/.config` (Antigravity logs/cache, mpv, mozilla, rofi…) | 1,471 | editor/browser caches |
| `~/rofi` | 475 | theme previews |
| `~/go/pkg/mod` | 271 | module cache |
| `~/paru` | 197 | AUR build trees |
| `~/Android` | 120 | SDK bits |
| `~/.local/share/TelegramDesktop` | 103 | app data |
| Heavy prefixes listed above (combined) | **~5.3k** | majority of deep bulk |

Default **excludes** skip `.git`, `node_modules`, `target`, `.cache`, etc., but **do not** skip:

- `~/.local/lib` (Python)
- `go/pkg`
- IDE log/cache dirs under `.config`
- AUR `paru` sources
- random cloned trees in `$HOME`

When the deep root is a **project**, that is fine. When the deep root is **`$HOME`**, every non-excluded tree becomes index fodder.

### 3.5 Mount defaults (secondary, not the main jump)

`ConfigStore::load` seeds newly discovered mounts:

- `windows_c` / `windowsEFI` → **off** by default  
- **everything else** (including `/mnt/data`, `/mnt/windows_d`) → **on**

`/mnt/data` (~146 items) is a small add. Windows D (~1.5k at depth 2) was already part of the 07-14 chart setup. **Mounts did not cause the 1.8k → 8k jump.**

---

## 4. Latency: same or worse? depth-aware reading

### 4.1 Absolute vs fair comparison

| Comparison | Rebuild | File search (`doc` / `f doc`) | Verdict |
|------------|--------:|------------------------------:|---------|
| vs depth‑2 baseline (1.8k) | 15 → 66 ms | ~50 → ~200 µs | **Much worse** (expected scale + extra) |
| vs depth‑3 chart (7.2k, no deep pin) | 48 → 66 ms (~1.4×) | 130 → ~197 µs (~1.5×) | **Still somewhat worse** |
| Math / units | unchanged | — | **Same** |

Rough linear model: cost ∝ item count for full index scan (name pass + optional fuzzy).  
`8203 / 1798 ≈ 4.6×` predicts file medians in the **~150–250 µs** band if algorithms held; measured **~200 µs** fits that for `f doc` / `doc`.

So the scary ×5 numbers are **mostly “we are not on the baseline workload anymore.”**

### 4.2 Residual gap vs depth‑3 chart (~7.2k)

Even at similar size, current is a bit slower than the 07-14 depth‑3 row:

| | Depth‑3 chart | Current ~8.2k | Gap |
|--|--------------:|--------------:|----:|
| Rebuild | 48 ms | 66 ms | ~1.4× |
| `f doc` | 130 µs | ~197 µs | ~1.5× |
| App `fire` merged | 150 µs | ~353 µs | ~2.4× |

Plausible contributors to the residual:

1. **Index shape** — deep pin fills more low-value / long-path entries (fuzzy/path scoring harder) vs a uniform depth‑3 walk of the same roots.
2. **More work per hit path** since baseline era — scoped queries, globs, live deep machinery, translate gates (translate is off here but code paths still exist).
3. **Merged `fire` is file-only now** (see §5) → full file policy, no “apps matched → skip file fuzzy” shortcut from the D-track merge fix.
4. **Binary / process growth** (~4.3 → 5.0 MB features) — minor for pure search µs, relevant for RSS/warm.
5. **Measurement noise / FS cache** — secondary.

This residual is **not** the main story; the deep-home pin is.

### 4.3 What did *not* regress

- **D6 calc short-circuit** — math still ~2 µs.
- **E4 cache format** — still v6 compact; size grew because **row count** grew, not because JSON bloated back to v5.
- **E2 fingerprint** — still skips useless rebuilds when roots/depth/excludes unchanged.
- **E5** — rebuild still off UI thread; bench rebuild is intentionally blocking.

---

## 5. Bench caveats (do not misread `iso_*` or `fire`)

### 5.1 `iso_apps fire` → 0 hits, ~1 µs

On this host, desktop discovery finds **no app name containing “fire”** (no Firefox desktop entry in the scanned dirs). Isolated apps correctly return empty; timing is not comparable to the old **~34–37 µs / hits>0** row.

Also: apps load on a **background thread** in `Engine::new`. Bench waits for **index** readiness, not apps. Early isolate can race empty app list (aggravates 0-hit cases).

### 5.2 `iso_files doc` → ~1–2 µs (false “win”)

`search_files_only` uses `DeepMode::Sync`, which **returns live-cache hits** without re-scanning the index. After the merged query hammer in `--bench`, `doc` is warm in live cache → isolate times are **not** index-scan times.

**Implication:** current `--bench` **iso_*** rows are **not** valid D-track regression signals until fixed (index-only isolate + wait for apps).

### 5.3 Merged `fire` is a file search

With 0 app matches, the engine runs the **file** path for `fire` (22 file/folder hits). Comparing that to baseline “app fire ~70 µs with Firefox” mixes two different code paths.

### 5.4 FX ~5–6 ms

`FxStore::convert` may **network-refresh** stale rates (`TTL` 12h; on-disk rates dated 2026-07-13, file mtime 07-14). That is intentional battery-oriented laziness, not a D/E file-index regression. Warm disk-only convert should stay ~µs.

---

## 6. Causal chain (single diagram)

```text
User opens any path under $HOME (normal launcher use)
        │
        ▼
maybe_auto_promote_deep_root walks parents
        │
        ▼
finds ~/package.json  ← stray marker, not a real project root
        │
        ▼
deep_roots += "/home/vedant"   (+ force reindex)
        │
        ▼
Indexer: global max_depth=2  THEN  walk $HOME to depth 6
        │
        ▼
+~6.2k items (python site-packages, IDE caches, clones, …)
        │
        ▼
Search scans larger index → median latency ×~4–6
Rebuild / cache / warm all scale up
        │
        ▼
Looks like “depth regression” though Settings depth never moved
```

---

## 7. Recommended fixes (priority order)

### P0 — User / config (restore baseline-like index today)

1. Edit `~/.config/blink/config.json` → remove `"/home/vedant"` from `deep_roots` (keep real projects only).
2. Optionally delete stray `~/package.json` **if** it is not needed (or move the real project elsewhere).
3. Settings → Rebuild index, or restart daemon / `blink --bench`.
4. Expect: **~2k items**, rebuild **~15–25 ms**, file medians back near baseline band.

### P1 — Code: stop promoting the universe

In `maybe_auto_promote_deep_root` / `promote_deep_root`:

- **Refuse** promoting `dirs::home_dir()`, `/`, and maybe `/home`.
- Prefer **nearest** marker directory, but **reject** if the marker is only at home and the opened path is “everything”.
- Cap pin breadth: e.g. refuse if estimated child count / walk would exceed N (or if path has fewer than K components below home).
- Optional: only auto-promote when opened path depth **> max_depth** (comment already implies this intent; enforce it).

### P2 — Indexer: safer deep walks

- Extra default excludes under deep roots: `.local/lib`, `go/pkg`, `*/Cache`, `*/logs`, `paru/src`, etc. (careful not to hide real user docs).
- Or: deep_roots walk uses a **stricter** exclude set than shallow home.

### P3 — Bench honesty (so this doesn’t scare again)

- Wait for apps loaded before isolate.
- `iso_files` should use **index-only** (`DeepMode::Skip`) and/or clear live cache between cases.
- Print `deep_roots`, `max_depth`, and item count in the bench header every run.
- Consider a named query that always hits a known installed app on the machine.

### P4 — Product / UX

- When auto-promote fires, show a one-shot notice: *“Pinned ~/foo for deeper indexing”* with Undo.
- Settings Deep roots list should make **`$HOME` pins visually scary** (warning badge).
- Document in Settings help: *Scan depth ≠ deep roots; pins always go to depth 6.*

---

## 8. Verification checklist after fix

```bash
# 1. deep_roots must not contain $HOME
jq '.index.deep_roots, .index.max_depth' ~/.config/blink/config.json

# 2. Rebuild + measure
blink --bench | tee /tmp/bench-after-depin.txt

# 3. Expect roughly
#    items ~ 1.8k–2.5k (depends on mounts / excludes)
#    rebuild_ms ~ 15–30
#    file/doc median closer to <100 µs on this machine
```

Optional A/B:

| Config | Purpose |
|--------|---------|
| `deep_roots: []`, depth 2 | True baseline |
| `deep_roots: ["~/blink"]` only | Legitimate pin cost (~+75 items) |
| `deep_roots: ["$HOME"]` | Reproduce the regression |

---

## 9. Conclusions

1. **You did not change global index depth** — `max_depth` is still 2.  
2. **Index grew because auto deep-root promotion pinned `$HOME`**, triggered by a **stray `~/package.json`**, then walked home to depth 6.  
3. **~76% of the current index is that accidental deep home walk** (~6.2k of 8.2k items).  
4. **Search/rebuild “regression” is mostly linear in index size**; a modest residual gap remains vs the old depth‑3 chart.  
5. **Math path is fine**; FX slowness is rates/network; **`iso_*` bench rows are currently misleading**.  
6. **Immediate recovery is config** (de-pin home); **durable fix is refuse home promotion + better excludes + clearer UX/bench**.

---

## Appendix A — Live config snapshot (at diagnosis)

```json
{
  "index": {
    "include_home": true,
    "include_mounts": {
      "/mnt/windows_d": true,
      "/mnt/data": true,
      "/mnt/windows_c": false
    },
    "deep_roots": ["/home/vedant/blink", "/home/vedant"],
    "max_depth": 2
  }
}
```

## Appendix B — Key code pointers

| Area | Path |
|------|------|
| Auto-promote markers | `src/engine.rs` → `maybe_auto_promote_deep_root` |
| Pin API | `src/engine.rs` → `promote_deep_root` |
| Depth‑6 deep walk | `src/providers/files/index.rs` → `build_index` |
| Mount defaults | `src/config.rs` → `ConfigStore::load` |
| Search merge / file policy | `src/engine.rs` → `search` |
| Live cache skewing iso_files | `src/providers/files/mod.rs` → `search_with` |
| Bench harness | `src/main.rs` → `run_bench` |

## Appendix C — Bench numbers used in this note

Current (2026-07-16, representative run):

```text
index: 8203 items · warm_ms=104 · cache_bytes=717131
rebuild_ms=66 · items=8203 · cache_bytes=717131

math           10 + 20                     2          2
unit           10kg to lb                  2          3
fx             100 usd to eur           5752       6690
app            fire                      356        410
file           doc                       203        253
file_force     f doc                     186        201
settings       settings                  134        161

iso_apps       fire                        1          1   (0 hits — invalid)
iso_files      doc                         2          2   (live-cache — invalid)
iso_calc       10 + 20                     1          1

binary_bytes:  5009512
```

---

## 10. Other regressions checked (beyond deep_roots / index size)

This section answers: *“Did we only look at the 8k index, or was there a wider regression pass?”*

**Short answer:** the **dominant** absolute latency hit is still the accidental deep `$HOME` pin. A wider pass found **several additional real issues** (some perf, some measurement, some product footguns). They are smaller than ×4–6 index growth but worth fixing.

### 10.1 Summary matrix

| Area | Severity | Real regression? | Notes |
|------|----------|------------------|-------|
| Deep `$HOME` pin → 8.2k items | **P0** | **Yes** | Main cause of file/search/rebuild slowness |
| FX `convert` while rates **stale** | **P1** | **Yes** | Every FX query spawns `curl` (~5–6 ms on DNS fail here) even though disk rates exist |
| `--bench` `iso_*` validity | **P1** | Measurement bug | Live-cache + apps race → false numbers |
| Bench case `fire` as “app” | **P2** | Env / fixture | No Firefox on this host → file-only path |
| Auto-promote + stray `~/package.json` | **P0** | Design footgun | Will recur on any machine with home-level markers |
| Binary size 4.3 → 5.0 MB | **P3** | Expected | Translate, preview thumbs, DnD, battery, settings growth |
| Math / units | — | **No** | Still ~2 µs |
| Battery calc queries | — | **No** | Gated on keywords only; sysfs only then |
| Translate (disabled) | — | **No** | Early `is_enabled()` short-circuit |
| Search debounce / idle wakeups | — | **Improved** | 40 ms debounce, no 16 ms deep poll, theme poll removed |
| Periodic refresh 30 m → 45 m | — | **Improved** (battery) | Slightly less frequent background work |
| Index TTL 30 m vs periodic 45 m | **P3** | Mild inconsistency | Freshness still enforced on next ensure_fresh / open paths |
| Apps empty in headless race | **P2** | Bench/daemon timing | UI daemon usually fine after warm |
| Residual search cost @ similar N | **P2** | Possible mild | ~1.4–1.5× vs depth‑3 chart after normalizing size |
| Preview / DnD / thumbs | — | UI-only cost | Not in `engine.search` µs; can spike on arrow keys (already optimized path) |
| File search feature surface | **P3** | Tradeoff | Globs / `in` scope / live deep added code; free-text path still two-pass top‑K |

### 10.2 FX network on every stale conversion (**confirmed**)

**Baseline era:** warm rates → FX medians **~1 µs**.  
**Now:** bench FX **~5.5–5.8 ms**.

Root cause in `src/providers/fx.rs`:

1. Rates on disk are **stale** (`fetched_at` ≈ **50 h** old; TTL = **12 h**).
2. `convert()` calls `is_stale()` → **true** → spawns:

   `curl -fsSL --max-time 3 https://api.frankfurter.dev/v1/latest`

3. On this machine **DNS fails** (`Could not resolve host`) in a few ms.
4. Fetch returns `None`; **old cache is still used** — but you already paid for the failed `curl` **on every FX query**.

So this is a **real interactive regression** for currency searches whenever:

- network is down / flaky, or  
- rates are past TTL and refresh is slow,

even though the correct UX is “use last known rates instantly.”

**Fix direction:** attempt network refresh **off the search path** (background once); on the hot path always convert from memory/disk; never block `convert` on `curl`. Optionally backoff after failed fetch (e.g. 15 min) so failed DNS is not per-keystroke.

### 10.3 Benchmark harness regressions (**confirmed**)

| Issue | Effect |
|-------|--------|
| `iso_files` uses `DeepMode::Sync` + live cache | After merged warmup, isolate returns **cached** results → **~1–2 µs** (not index scan) |
| Bench waits for **index**, not **apps** | `iso_apps` / merged apps can race empty |
| Hard-coded query `fire` | **0 desktop apps** match “fire” on this host (35 loadable apps; Chrome exists, Firefox does not) |
| No print of `deep_roots` / `max_depth` | Easy to misread workload drift as algorithm regression |

Merged `fire` ~350 µs is therefore **file-search under 8k items**, not “apps got 5× slower.”

### 10.4 Auto-promote design footgun (**confirmed**, root of index growth)

Already covered in §3; restated as a **product regression class**:

- Any marker at `$HOME` (`package.json`, `Makefile`, …) can pin the entire home tree to depth 6.
- No guard against promoting home / `/`.
- No user-visible “we just pinned X” toast.
- Opening normal files is enough; user never touches Scan depth.

This is worse than a one-off config mistake — it will happen again.

### 10.5 Feature growth since D/E baseline (cost inventory)

Large additions after the 2026-07-14 D/E snapshot (~**+5.3k LOC** in `src/`):

| Feature | Search-path cost when idle/off | When active |
|---------|--------------------------------|-------------|
| Translate scaffold | Near zero if disabled | Network + debounce; can own query |
| Path globs + `name in scope` | Extra parse branches per file query | Live walks / scoped glob |
| Live deep search | Not in merged bench (`DeepMode::Skip`) | Async UI worker; visit caps 8k/40k |
| Preview thumbs / video / PDF | None in `--bench` | Decode spikes on selection |
| DnD | None in search | Drag start work |
| Battery / AC status | Keyword gate only | sysfs read on `battery`/`power`/… |
| Appearance / open-with settings | Config size only | — |
| Idle wakeup cuts (`a6bb6e5`) | **Positive** | — |

**Binary:** ~4.3 MB → **~5.01 MB** — expected from features, not a search algorithm fail.

### 10.6 Things explicitly checked and **not** regressed

| Check | Result |
|-------|--------|
| Math / unit short-circuit (D6) | **OK** (~2 µs) |
| Calc plain-text reject (D4/D6 era) | Still present before heavy regex |
| File two-pass + top‑K + fuzzy cap 500 | Still present |
| Cache format v6 compact | Still in use |
| Fingerprint skip rebuild | Still in use |
| UI off-thread index rebuild | Still in use |
| Battery provider polling in background | **No** — event-style on query only |
| Translate when `enabled: false` | No network / no handle |
| Search debounce | Present (~40 ms; longer for translate/CJK) |

### 10.7 Mild / possible residual issues (not fully proven as “code regressed”)

1. **~1.4–1.5× slower** rebuild/search vs depth‑3 chart at similar item counts — may be index **shape** (deep junk paths), extra parse branches (glob/scope), or measurement variance. Re-bench after de-pinning home to settle.
2. **Deep-root walk re-traverses FS** for paths already inserted at shallow depth (`seen` only skips **insert**, not directory visit cost) — rebuild tax when home is pinned.
3. **INDEX_TTL 30 m** vs **periodic refresh 45 m** — not a user-visible search regression; battery-oriented.
4. **Apps inventory is small (35)** vs a full desktop image — product completeness, not an engine regression.

### 10.8 Priority if fixing “other” issues after de-pin

1. **Refuse auto-promote of `$HOME` / `/`** (+ optional toast).  
2. **FX: never `curl` on the search hot path**; stale-while-revalidate.  
3. **Fix `--bench` iso_*** (index-only files, wait for apps, print config header, better app query).  
4. Re-measure file medians at ~2k items; only then hunt residual ×1.5 if still present.  
5. Tighten excludes for deep walks (python lib, `go/pkg`, IDE caches) so even large pins hurt less.


---

## 11. Fixes applied (2026-07-16)

| Fix | Where | Result |
|-----|-------|--------|
| Refuse overbroad deep roots (`$HOME`, `/`, `/home`, …) | `engine.rs` `promote_deep_root` / `maybe_auto_promote` | Unit tests pass; home can no longer auto-pin |
| Strip overbroad pins on config load | `config.rs` `ConfigStore::load` | Migrates bad configs when save is writable |
| FX stale-while-revalidate (no `curl` on search path) | `providers/fx.rs` | FX bench **~5.5 ms → ~2 µs** with disk rates |
| Honest `--bench` | `main.rs` + `engine.rs` | Wait for apps; print `max_depth` / `deep_roots` / `apps`; pick real app query; `iso_files` = index-only |
| Live config de-pin | `~/.config/blink/config.json` | `deep_roots` = `["/home/vedant/blink"]` only |
| Fresh cache | `~/.cache/blink/file-index.json` | **2,017 items · ~127 KB** |

### After fix (`blink --bench`, 2026-07-16)

```text
index: 2017 items · warm_ms=54 · cache_bytes=129839
config: max_depth=2 · deep_roots=/home/vedant/blink · apps=35
rebuild_ms=14

math           10 + 20                     2 µs
fx             100 usd to eur              2 µs   (was ~5500)
app            chrom                      45 µs
file           doc                        59 µs   (was ~200 @ 8k)
file_force     f doc                      55 µs
iso_files      doc                        52 µs   (was fake ~1–2 µs)
iso_apps       chrom                       1–2 µs · 2 hits
```

Compared to 07-14 baseline (~1.8k items, no deep pin): back in the same band (file ~50–70 µs, rebuild ~15 ms). Slightly higher item count from `~/blink` deep pin + `/mnt/data`.
