# Providers depth analysis — `src/providers`

**Date:** 2026-07-16  
**Status:** diagnosis; dead/footgun + Phase 1–2 applied 2026-07-16  
**Goal:** find inefficiencies, bugs, dead code, and optimizations so providers stay **fast on the search hot path**, light on CPU/IO when idle, and correct under concurrent UI + deep/translate workers — without dropping features.

Related:

- Tracker: [`OPTIMIZATION.md`](../OPTIMIZATION.md) (tracks A/D/E already hit apps/files/calc/FX)
- File-index regression: [`index-regression-depth-analysis.md`](./index-regression-depth-analysis.md)
- UI analysis (row pool / workers): [`ui-depth-analysis.md`](./ui-depth-analysis.md) *(if present)*
- Translate plan: [`translation.md`](./translation.md)
- Perf baselines: [`performance.md`](./performance.md)

---

## TL;DR

| Area | Health | Biggest issue |
|------|--------|----------------|
| **Apps** | Good | Minor allocs (`resolve_id`, result cloning); name-only search already optimized |
| **Files index** | Good after deep-root fix | `ensure_fresh` still rediscovers mounts + recomputes FP when RAM is “fresh” |
| **Files search** | Good structure | **Deep walk holds `index` read lock** for up to ~200 ms; live-cache clones full hit vectors |
| **Calc** | Good | Plain-text reject + engine short-circuit; residual regex order cost on near-misses |
| **FX** | Fixed (2026-07-16) | Non-blocking convert OK; still spawns `curl` process for refresh |
| **Translate** | Good (async) | Multi `cfg()` clones; process-local success map unbounded; sequential `curl` backends |
| **Shared types** | OK | `SearchResult` is allocation-heavy; `ConfigStore::get()` **clones full config** on hot paths |

**Top 3 fixes (if implementing next):**

1. **Drop the index `RwLock` before live deep walks** (roots first, then walk). Unblocks UI search while async deep runs.  
2. **Stop cloning whole config / whole live-cache hit lists** on every keystroke (`get` → view/has API; config snapshot or `with`).  
3. **Use `name_lower` in file fuzzy** + skip redundant `is_scoped` re-parse when engine already decided force-files.

`blink --bench` still measures **engine/providers only** — UI row pool wins do not show there. Provider CPU wins will show in `iso_*` / merged medians and in concurrent typing + deep-walk jank.

---

## 1. Map of the tree (~8k LOC)

```
src/providers/
  mod.rs              SearchResult, Action, Provider trait
  apps.rs             .desktop load + fuzzy top-K
  fx.rs               ECB rates via curl, disk + bg refresh
  translate.rs        detect + disk/mem cache + multi-backend HTTP
  calc/
    mod.rs            orchestrator + plain-text reject
    math/expr         evaluator (replaced meval)
    currency          FX query/predict
    units/timezone/datetime/duration/battery
  files/
    mod.rs            FileProvider API, open helpers
    index.rs          walk, fingerprint, JSON cache v6
    search.rs         ~2.6k LOC: globs, scoped `in`, deep walk, scoring
    live_cache.rs     TTL + LRU deep-hit cache
```

**Call graph (UI keystroke):**

```
Engine::search
  ├─ calc.search          always (cheap reject)
  ├─ translate.search     if enabled + match (no curl)
  ├─ apps.search          unless calc/translate/files forced
  └─ files.search_with(…, DeepMode::Skip)
       └─ live_cache merge if prior deep hit

async (single-flight UI):
  Engine::search_files_deep → search_with(…, DeepMode::Async)
  Engine::search_translate_network → translate.search_network (curl)
```

Engine already short-circuits apps/files when calc or strong translate owns the query ([`src/engine.rs`](../src/engine.rs)).

---

## 2. What’s already solid (don’t re-litigate)

| Item | Where | Why it matters |
|------|--------|----------------|
| Calc plain-text reject | `calc/mod.rs` `looks_like_plain_text` | App/file typing skips regex stack |
| Engine D6 short-circuit | `engine.rs` | Calc hit → no apps/files |
| Apps `name_lower` + top-K heap | `apps.rs` | `iso_apps` ~ tens of µs |
| Files two-pass score + fuzzy cap | `search.rs` | Strong top-K skips fuzzy; fuzzy eval cap 500 |
| Deep skip when index strong | `DEEP_SKIP_IF_INDEX_SCORE` | Exact/prefix folders don’t spawn walks |
| Async deep budgets | 40 ms sync / 200 ms async | Bounded CPU |
| Live cache TTL 5 min, LRU 64 | `live_cache.rs` | Retype without re-walk |
| FX non-blocking convert | `fx.rs` | Stale rates convert; bg refresh + backoff |
| Translate UI path | `translate.search` | Cache / fail / “Translating…” only |
| Index fingerprint + TTL + atomic JSON | `index.rs` | No thrash on mtime noise; crash-safe write |
| Overbroad `deep_roots` strip | `config.rs` / engine promote | Fixed home pin regression |

---

## 3. Inefficiencies (by priority)

### P0 — Correctness / concurrency

#### P0.1 Deep search holds the **index read lock** for the whole walk

```text
FileProvider::search_with
  index = state.index.read()     // held
  search_index(..., deep)        // may WalkDir up to DEEP_TIME_BUDGET_ASYNC (200ms)
  drop(index)
```

**Effect:** While the async deep worker walks disk, the UI thread’s next `search_with` / `is_scoped_query` / rebuild path must wait on the same `RwLock`. That can feel like “typing jank” even after UI single-flight — the stall is **inside the provider lock**, not the GTK queue.

**Fix shape:** In `search_index` / `search_with`, resolve roots + index hits under the lock, **clone the small root list + needed hits**, drop the lock, then walk. Index mutation (rebuild) can proceed; deep results merge via id set as today.

#### P0.2 `should_deep_search` clones live-cache hits only to test presence

```rust
if self.live_cache.get(query).is_some() { return false; }
```

`get` clones `Vec<SearchResult>` (each row: multiple `String`s + `PathBuf`). Called from UI after every index search when deep might be considered.

**Fix:** `LiveCache::contains(query) -> bool` (touch LRU optional) without cloning hits.

---

### P1 — Hot-path allocations / redundant work

#### P1.1 `ConfigStore::get()` clones entire `BlinkConfig`

Used on almost every file search (`search_with`), fingerprint, open helpers, translate `cfg()`, etc. Config is large (index roots, excludes, UI, open-with, translate).

**Fix options (pick one):**

- `config.with(|c| …)` read guard API for hot paths  
- `Arc<BlinkConfig>` swap-on-update (cheap `Arc::clone`)  
- Cache `path_style` / `exclude` / `deep_roots` pointers inside `FileProvider` invalidated on config update

Highest ROI on multi-provider keystrokes that still call files.

#### P1.2 Live cache `get` always clones hits; Skip path merges with another clone path

Merge does `id` `HashSet` + sort + `title.to_lowercase()` for up to 25 rows — fine absolute cost, but **clone of 25 full results** per retype is pure waste when the cache already owns the data.

**Fix:** `Arc<[SearchResult]>` or `Arc<Vec<SearchResult>>` in cache entries; merge by id with shared ownership; or return `Cow` / temporary guard (harder with Mutex).

#### P1.3 Double scoped-query parse + double index lock

Per query with possible `in`:

1. `Engine` → `files.is_scoped_query` → may `index.read` + `parse_scoped_query`  
2. `search_with` → `search_index` → `parse_scoped_query` again  

**Fix:** Engine can pass a `force_files` bit without re-parse; or parse once and pass `Option<ScopedQuery>` (API change). Cheap win: early `is_scoped_file_query` (no index) already short-circuits; only bare folder scopes pay double.

#### P1.4 File fuzzy matches on `item.name` instead of `item.name_lower`

```rust
matcher.fuzzy_match(&item.name, q)  // ignore_case still folds each call
```

Apps already fuzzy on `name_lower`. Files should do the same for fewer case folds inside skim.

#### P1.5 `SearchResult` construction is string-heavy

Every hit: `format!("path:…")` / `app:…`, `pretty_path` → `String`, `icon: Some(…into())`, clones of name/exec/path. Top-K is 20–25 so absolute cost is small vs deep walk, but it dominates **micro-benches** at ~2k index.

**Later:** intern icon statics (already `&'static str` then `.into()` — can store `Cow<'static, str>` or enum icon), reuse path display buffers, build id from path without format when possible.

#### P1.6 Apps `resolve_id`

```rust
.find(|a| format!("app:{}", a.id) == id)
```

Allocates per app. Use `id.strip_prefix("app:") == Some(a.id.as_str())` or store full id on `DesktopApp`.

#### P1.7 Currency path double-normalizes

`is_currency(from)` then `normalize_currency(from)` (and same for `to`). Normalize once; `is_currency` is thin wrapper.

#### P1.8 Translate / calc repeated config clone

`should_handle` / `is_auto_query` / `needs_network` / `search` each call `cfg()` → full config clone. UI often calls several of these per keystroke.

**Fix:** take `&TranslateConfig` once at engine/UI boundary, or `Arc` snapshot.

#### P1.9 Index `ensure_fresh` “fast path” still runs `discover_mounts()`

Comment claims battery path, but when RAM index non-empty and meta TTL OK it still:

1. `discover_mounts()` (findmnt /proc — process + parse)  
2. recompute fingerprint  
3. compare  

Periodic 45 m is fine; **if** anything calls `ensure_fresh` more often, this is avoidable. Prefer: compare config-only fingerprint first; rediscover mounts only on TTL expiry or explicit rebuild.

#### P1.10 Index build `seen: HashSet<PathBuf>` + `path.clone()` every entry

Walk cost is disk-bound; still extra allocs. Could hash `OsStr` bytes or use `HashSet` of owned paths only when multi-root overlap matters (home vs deep_root under home).

---

### P2 — Process / network weight (not typing, but battery)

| Item | Notes |
|------|--------|
| FX + translate use **`Command::curl`** | Cold process spawn (~ms) each fetch; no connection reuse |
| Translate tries backends **sequentially** (Libre → Google → MyMemory) | Worst case ~3× timeouts (connect 1s / max 2s each) — mitigated by fail cache 90s |
| FX refresh backoff 15 m + inflight flag | Good; keep |
| Translate mem success map | **Unbounded** process-local `HashMap` (disk sweeps at 500 files only) |

**Optional:** small in-process HTTP (`ureq`) for FX/translate workers only — fewer forks, keep GTK free. Not on search hot path.

---

### P3 — Code health / dead code

| Symbol | Status |
|--------|--------|
| `CalcProvider::fx_store` | **removed** (was dead after FX lazy convert) |
| `FxStore::ensure_fresh` | **removed** (`convert` + bg refresh only) |
| `Engine::search_files_only` | **removed** (bench uses `search_files_index_only` / deep API) |
| `Provider` trait | **removed** — concrete `pub fn search` / `search_with` only |
| `FileProvider::search` (trait) | **removed** — footgun Sync-deep default gone; use `search_with` + `DeepMode` |
| Apps `TryExec` soft check | **removed** — no-op `which` path dropped; still filter Hidden/NoDisplay |
| Apps `resolve_id` | **fixed** — strip `app:` prefix, no per-app `format!` |
| Apps: no `GenericName` / `Keywords` | Feature gap, not bug; name-only was intentional (avoid letter-soup) |
| `normalize_currency` nested match | Redundant second match for ISO list — tidy only (still open) |

---

## 4. Bugs & footguns (beyond perf)

| ID | Issue | Severity | Notes |
|----|--------|----------|--------|
| B1 | Index lock held across deep walk | **High** (UX) | See P0.1 — not data corruption, but stalls concurrent search/rebuild |
| B2 | Trait `FileProvider::search` → Sync deep | **fixed** | Trait + default Sync search removed |
| B3 | Live cache empty hits not stored | Intentional | Failed deep leaves no negative cache → may re-walk; optional short negative TTL |
| B4 | Auto-promote deep roots | Fixed | Home pin; keep guards in engine/config |
| B5 | Translate `simple_hash` (FNV-ish 64-bit) | Low | Collision theoretical; disk path uses hash hex only |
| B6 | Scoped `in` + calc ` looks_like_plain_text` | Low | Queries with ` in ` skip plain-text reject → more calc regex; engine may still force files |
| B7 | Apps ignore `Hidden` after parse… | OK | `hidden` filtered; `NoDisplay` filtered at insert |
| B8 | Concurrent deep + index rebuild | Medium | Write waits on long read (B1); after P0.1, rebuild can run during walk (deep may see old roots — acceptable) |

No evidence of FX still blocking the search path after 2026-07-16 fix.

---

## 5. Provider-by-provider notes

### 5.1 Apps (`apps.rs` ~360 LOC)

**Hot path:** read lock → prefix/contains on `name_lower` → optional fuzzy → top-K heap → `to_result` clones.

**Good:** bands align with engine (50k / 30k / 15k); fuzzy threshold 40; no comment fuzzy.

**Improve:** `resolve_id` strip prefix; consider storing `id_full: "app:…"` once; reload is off hot path (desktop dirs flat — no recursive applications subdirs; some distros use nested dirs — **feature gap** if apps missing).

### 5.2 Files — index (`index.rs` ~580 LOC)

**Good:** derived fields on load; compact cache entries; progress atomics; always-skip junk set shared with deep.

**Improve:** mount rediscover policy (P1.9); optional binary/postcard cache later (size/speed); `MAX_INDEX` 100k hard stop OK.

### 5.3 Files — search (`search.rs` ~2.6k LOC)

Largest module: path completions, globs, scoped `in`, soft folder hints, live deep, scoring.

**Good:** deep gates (`looks_specific_for_deep`, broad `*.md` skip), pinned deep roots only (no whole-home walk), tests for scope/glob/deep.

**Improve:** P0.1 lock scope; P1.4 fuzzy haystack; consider splitting file further (`scoped.rs`, `deep.rs`) for maintainability only.

### 5.4 Files — live cache

**Good:** key normalizes `f `/`file `/`folder `; TTL + LRU.

**Improve:** `contains`; `Arc` hits; optional **negative** cache for “walked, 0 hits” (1–2 min) to stop repeat walks on typos.

### 5.5 Calc (`calc/*`)

**Order:** battery → duration → tz → currency → units → datetime → math → natural.

**Good:** battery only after keyword; custom expr eval (no meval); FX via shared store.

**Improve:** cheaper prefilters before tz/units regex for strings that only failed plain-text because of ` in ` / ` to `; keep feature parity.

### 5.6 FX (`fx.rs`)

**Good:** disk load at construct; convert never blocks; single-flight refresh + 15 m backoff.

**Improve:** optional ureq; `save_disk` pretty JSON unnecessary (compact); TTL 12 h OK for ECB.

### 5.7 Translate (`translate.rs` ~1k LOC)

**Good:** kill switch; no curl on UI; pending row; fail cache; disk sweep 500; direction parse tests.

**Improve:** single config snapshot; bound `mem_ok` (e.g. 256 LRU); worker already single-flight in UI — keep it.

---

## 6. Suggested work phases (same spirit as UI)

### Phase 1 — Safe, high ROI (no UX change)

1. **Release index lock before deep WalkDir** (P0.1). ✅  
2. **`LiveCache::contains`** for `should_deep_search` (P0.2). ✅  
3. Fuzzy on **`name_lower`** (P1.4). ✅  
4. **`resolve_id`** without per-app `format!` (P1.6). ✅ (dead-code pass)  
5. Currency **normalize once** (P1.7). ✅  
6. Translate hot checks use **`config.with`** / single `cfg` clone (P1.8). ✅  

**Expected:** less jank when deep runs; slightly lower `iso_files` / retype CPU; no visual change.

### Phase 2 — Config & cache ownership

1. **`ConfigStore::snapshot` → `Arc<BlinkConfig>`** + `with` (P1.1) ✅ — hot paths clone Arc, not the whole tree.  
2. Live cache stores **`Arc<[SearchResult]>`** (P1.2) ✅ — retypes share hits.  
3. **Scoped query one-slot memo** (P1.3) ✅ — engine force-files + deep gate share one index parse.  
4. **`ensure_fresh`** no mount rediscovery on TTL-ok + fingerprint match with cached mounts (P1.9) ✅.  


### Phase 3 — Process model & structure (optional)

1. Replace worker `curl` with lightweight HTTP client (FX + translate).  
2. Bound translate memory cache; optional negative live-cache.  
3. Split `search.rs` for readability.  
4. Nested `.desktop` dir walk if users miss apps.  

**Expected:** battery / offline behavior; not needed for list polish.

---

## 7. What **not** to optimize

- Math expr path already ~2 µs — leave alone.  
- Raising fuzzy caps or deep visit caps “for completeness” — destroys latency.  
- Searching app Comment/Keywords with fuzzy — reintroduces letter-soup ranking bugs (by design D4).  
- Sync deep on UI thread.  
- Measuring UI lightness with `blink --bench` alone.

---

## 8. Quick verification checklist (after any Phase 1+)

```bash
cargo test -q
cargo build --release
# honest engine benches
blink --bench
# concurrent feel (manual): type while deep fills nested file; list should stay responsive
# FX offline: 100 usd to eur → instant from disk, no multi-ms stall
# translate off: zero translate work
```

Compare to post-fix baselines (~2k index, file `doc` ~60 µs, FX ~2 µs with rates on disk) in [`OPTIMIZATION.md`](../OPTIMIZATION.md) / index regression doc.

---

## 9. Priority matrix

| ID | Item | Effort | Impact | Risk |
|----|------|--------|--------|------|
| P0.1 | Drop index lock around deep walk | M | **High** (concurrent UX) | ✅ plan_deep_jobs + run unlocked |
| P0.2 | LiveCache::contains | S | Medium | ✅ |
| P1.1 | Config Arc / with | M | High (alloc) | ✅ snapshot+with |
| P1.2 | Arc live-cache hits | S–M | Medium | ✅ |
| P1.3 | Scoped parse once | S–M | Low–Med | ✅ memo |
| P1.4 | Fuzzy name_lower | S | Low–Med | ✅ |
| P1.6–P1.8 | Small alloc cleanups | S | Low | ✅ resolve_id, currency, translate with |
| P1.9 | ensure_fresh mounts | S | Low (periodic) | ✅ |
| P2 HTTP client | M | Battery / worker | Medium (deps) |
| Dead code cleanup | S | Hygiene | None |

---

## 10. Summary

Providers are in **good shape** after calc split, file search gates, FX non-blocking, and deep-root guards. Remaining wins are less about algorithms and more about **lock scope**, **clone hygiene**, and **not redoing work the engine already did**.

The single most important correctness/UX fix is **not holding the file index lock across live deep walks**. The single most important steady-state efficiency theme is **stop cloning config and full result vectors on every keystroke**.

No mandatory feature loss is required for Phase 1–2.
