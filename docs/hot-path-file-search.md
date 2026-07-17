# Hot-path file search — design note

**Date:** 2026-07-17  
**Status:** design; **Batch A implemented 2026-07-17** (hot set + seed scoring; no short-circuit yet)  
**Goal:** make **regular** free-text file searches faster by scanning a small *hot set* of paths the user actually opens, while **cold / rare / structural** queries keep today’s full-index cost and correctness.

Related:

- Core audit (usage, empty state, ranking): [`core-depth-analysis.md`](./core-depth-analysis.md)
- Bench rollup after #1–#12: [`bench/core-audit-2026-07-17-summary.md`](./bench/core-audit-2026-07-17-summary.md)
- Providers / index search path: [`providers-depth-analysis.md`](./providers-depth-analysis.md)
- Index depth / caps: [`performance.md`](./performance.md), [`index-regression-depth-analysis.md`](./index-regression-depth-analysis.md)
- Tracker: [`OPTIMIZATION.md`](../OPTIMIZATION.md)

---

## TL;DR

| Observation | Implication |
|-------------|-------------|
| Index is ~**2k** paths (can grow toward `MAX_INDEX` 100k) | Full free-text scan is already ~**50–70 µs** for `doc` — good, but still **O(n)** every keystroke |
| User regularly opens only **dozens** of files/folders | That signal already exists in **`UsageStore`** as `path:…` ids |
| Today usage only **re-ranks** after a full scan | We never use “I only care about these paths” to **shrink the scan** |
| Empty launcher already uses `usage.top(20)` | Product already thinks “hot first”; search should too |

**Proposal:** two-tier free-text file search — **hot set first** (e.g. top 64–128 opened paths that still resolve in the index); if hits are strong enough, return; otherwise **full index** (same as now). Path/glob/scoped/absolute queries **always** use the full path (unchanged).

**Contract:** common targets get faster; out-of-hot-range targets take **regular** search time — never silently disappear.

---

## 1. Motivation

### 1.1 Workload shape

Typical Raycast-style use:

- Same projects, docs, configs, downloads — **tens of paths**, opened repeatedly.
- Occasional “where is that one-off file?” — **needs the full index**.

Indexing ~2k (or more) paths is correct for **discovery**. Searching all of them on every keystroke for “`readme`” is wasteful when the user always means `~/blink/README.md`.

### 1.2 What Blink already measures (2026-07-17)

From `blink --bench` after core audit (order-of-magnitude, warm index ~2.1k items):

| Case | Median (approx.) |
|------|-----------------:|
| math / unit / fx | 1–4 µs |
| iso_apps | ~1–2 µs |
| **iso_files / file `doc`** | **~50–65 µs** |
| settings merge | ~90–100 µs |

So file free-text is already healthy at 2k. A hot set still helps:

1. **Common queries** feel even snappier (especially under UI + merge).  
2. **Scales** if the index grows (deep roots, more mounts, higher `max_depth`).  
3. Aligns product behavior with empty-state frecency.

### 1.3 What we do *not* want

- Shrinking the **persistent index** to only hot paths (breaks discovery).  
- Hiding cold results when the hot set is weak.  
- New network / telemetry.  
- Changing path browser, globs, or `name in scope` semantics.

---

## 2. Current architecture (relevant bits)

### 2.1 Index

- Built by `FileProvider` / `files/index.rs` from home, mounts, extras, deep roots.  
- On disk: `~/.cache/blink/file-index.json` (compact cache + fingerprint + TTL).  
- Hard cap: **`MAX_INDEX` = 100_000**.  
- Default depth 2; deep roots walked deeper.

### 2.2 Free-text search hot path

Approximate flow for a free-text query (e.g. `doc`):

```
Engine::search
  → files.search_with(q, allow_fuzzy, DeepMode::Skip)   // UI path
       → search_index(&index, …)   // full slice scan, top-K heap (25)
  → usage.boost(&id) on each non-calc result   // rank only
  → dedup / sort / truncate 25
```

Live deep search is separate (`DeepMode::Async` + `LiveCache` for **query → hits**, not path popularity).

**Important:** free-text loops over **`index.iter()`** (full slice), often more than one pass (strong name match, then fuzzy when needed). Top-K heap limits *results*, not *candidates scored*.

### 2.3 Usage / frecency (already shipped)

| Piece | Role |
|-------|------|
| `UsageStore` | `~/.local/state/blink/usage.json` — count + last-open time |
| `record(id)` | On launch/open (debounced compact write; cap 500 entries) |
| File ids | `path:/absolute/path` |
| App ids | `app:desktop-stem` |
| `boost(id)` | Score bump after provider search |
| `top(n)` | Empty-state recent list |

So: **opens are tracked; search does not yet use opens to limit scan.**

### 2.4 Empty state (already “hot first”)

`Engine::empty_results`:

1. `usage.top(20)` → `resolve_id` (exact app/path resolve after #4).  
2. Fill remaining slots with apps.

That is the right *UX* model for “things I use.” Hot-path search is the same idea for **typed** queries.

### 2.5 Related caches (do not confuse)

| Cache | Key | Purpose |
|-------|-----|---------|
| File index | roots/fingerprint | Persistent path catalog |
| `LiveCache` | query string | Skip re-walk of deep search for same query |
| Usage | result id | Frecency for rank + empty state |
| **Hot set (proposed)** | open history ∩ index | Shrink free-text candidate set |

---

## 3. Design: two-tier free-text search

```
query
  │
  ├─ absolute / ~/ / ./ / drive / glob / scoped (`in`)?
  │     → full index + existing path/glob/scope logic  (NO hot short-circuit)
  │
  └─ free-text name search (incl. after stripping f/file/folder)
        │
        ├─ Phase 1 — HOT SET (e.g. 64–128 IndexedPath still in index)
        │     score with same matchers as today
        │     if strong enough → return top-K   ← fast path
        │
        └─ Phase 2 — FULL INDEX
              today’s search_index body
              when hot empty / weak / miss
```

### 3.1 Query types vs hot set

| Query type | Example | Use hot set? |
|------------|---------|--------------|
| Free-text name | `doc`, `glassbox`, `readme` | **Yes** (phase 1) |
| Force-files prefix | `f doc`, `File doc` | **Yes** after strip (same free-text) |
| Absolute / home / drive | `~/Dev`, `/mnt/…`, `D:/…` | **No** |
| Glob / ext shorthand | `*.md`, `.rs`, `foo/**` | **No** |
| Scoped | `opt.md in blink` | **No** (scope needs index structure) |
| Empty query | (no type) | Existing usage empty-state only |

### 3.2 Contract (product)

| Situation | Behavior |
|-----------|----------|
| Query matches a frequently opened path well | Prefer fast hot path |
| Query only matches cold index paths | Full scan — **same cost/quality as today** |
| Hot set empty (new user / no file opens yet) | Full scan always |
| Hot hit weak, cold hit exact | Full scan must still win (never skip when hot is weak) |

---

## 4. Hot set construction

### 4.1 Source of truth

**Primary:** `UsageStore` entries whose id starts with `path:`.

Sort by existing `frecency(count, last)` (same as `top` / `boost`).

Optional later:

- Session-only opens (before disk frecency matures).  
- “Selected but not opened” search rows (noisier — not MVP).

### 4.2 Membership rules

For each top `path:` id (in frecency order):

1. Strip `path:` prefix.  
2. Resolve against **current** index (or `resolve_path` / exists + still not excluded).  
3. Drop missing, moved, or non-indexable paths.  
4. Stop at **`HOT_CAP`** (suggested default **64**, max ~256).

Store either:

- **Indices** into the main `Vec<IndexedPath>` (no string duplication), or  
- Small `HotEntry { index_idx or path, name_lower, … }` snapshot rebuilt when index or usage changes.

### 4.3 Rebuild triggers

| Event | Action |
|-------|--------|
| Index rebuild / load cache | Rebuild hot (drop stale) |
| `usage.record` for a `path:` id | Mark dirty; rebuild on next search or debounced (same 2s window as usage save is fine) |
| Settings that change roots/excludes | Via reindex path |

### 4.4 Cap and memory

| Knob | Suggested default | Notes |
|------|------------------:|-------|
| `HOT_CAP` | 64 | Dozens of regular files + folders |
| Upper clamp | 256 | Diminishing returns vs full 2k |
| Storage | indices + Arc index | Negligible vs full index |

Usage map itself is already capped at **500** entries (#6).

---

## 5. “Strong enough” to skip full index

Reuse score bands already used by apps/files/engine (exact ~50k, prefix ~30k+, contains ~15k+, fuzzy lower). Align with deep-skip style thresholds (`DEEP_SKIP_IF_INDEX_SCORE` / strong path demotion ~30k).

**Conservative MVP gate (recommended ship gate):**

Skip full index **only if** free-text hot phase produces **at least one** hit with:

```text
score >= 30_000   // prefix / strong path band
```

or (slightly looser, after measurement):

```text
best >= 15_000 && query.len() >= 3 && hot_hits >= 1
```

**Always full-index when:**

- Hot set empty.  
- Query length &lt; 2 (or &lt; 3 if tuning shows early keys need breadth).  
- Best hot score below gate.  
- Force path/glob/scope modes (never enter hot phase).

**Principle:** one extra full scan is cheap; a **missed exact cold file** is a product bug.

### 5.1 Safe rollout sequence

| Step | Behavior | Risk |
|------|----------|------|
| **0** | Build hot set; log size / optional hit rate; **no** search change | None |
| **1** | Phase 1 + **always** phase 2; assert winners match (debug/bench) | None |
| **2** | Skip phase 2 when gate passes | Medium — tune threshold |
| **3** | Bench `file_hot` / `file_cold`; ship | Low if step 1 proved parity |

---

## 6. Integration points (code map)

| Module | Change |
|--------|--------|
| `src/usage.rs` | Optional helper: `top_paths(n) -> Vec<String>` filtering `path:` |
| `src/providers/files/mod.rs` | Own `HotSet` state; rebuild; pass into search |
| `src/providers/files/search.rs` | Free-text branch: score hot slice first; gate; else full loop |
| `src/providers/files/index.rs` | Hook rebuild completion → `hot.rebuild()` |
| `src/engine.rs` | Usually **no** API change if results shape unchanged; boost still applies |
| `src/bench.rs` | Cases `file_hot` / `file_cold` (feature `bench`) |

Keep deep-walk + `LiveCache` as they are; hot set is about **index scan**, not live walk.

---

## 7. Expected performance (honest)

At **~2.1k** indexed items (current machine baseline after audit):

| Scenario | Scan size | Rough free-text cost |
|----------|----------:|----------------------|
| Today full | ~2k | ~50–70 µs (`doc`) |
| Hot hit, skip full | ~64–128 | **~10–25 µs** (order-of-magnitude) |
| Hot miss → full | 64–128 + 2k | ≈ today (+ tiny phase-1 overhead) |

Relative win grows if index → 10k–100k.

**Do not promise** order-of-magnitude UI latency change without measuring: GTK row rebuild still dominates perceived typing cost (see UI analysis). Hot set still reduces **provider** work and scales better.

### 7.1 Bench plan

```bash
cargo build --release --features "layer-shell,bench"
# Seed usage: open a few paths via daemon, or test helper to record path: ids
./target/release/blink --bench
```

Add (when implementing):

| Case | Intent |
|------|--------|
| `file_hot` | Name that resolves only/best via a heavily used path |
| `file_cold` | Name present in index, never/rarely opened |
| `iso_files` | Unchanged full-index baseline |

Success criteria:

- `file_hot` median **meaningfully below** `file_cold` / current `file`.  
- `file_cold` median **within noise** of pre-change `file`.  
- No result-quality regressions on cold exact matches (manual + optional assert in step 1).

---

## 8. Alternatives considered

| Approach | Pros | Cons | Verdict |
|----------|------|------|---------|
| **Hot set first (this doc)** | Real open signal; cold path preserved | Needs gate tuning | **Preferred** |
| Rank-only usage (status quo) | Simple | Still O(n) scan | Keep for ranking; not enough alone |
| Query-string cache only (`LiveCache`) | Great for retype | Not “new query, same file” | Keep separate |
| Index only hot paths | Fast | Breaks discovery | Reject |
| Full inverted index / n-grams | Faster cold too | Heavy for launcher | Out of scope |
| ML / embeddings | — | Complexity, size | Reject |

---

## 9. Pitfalls and edge cases

| Risk | Mitigation |
|------|------------|
| Stale hot path (deleted/moved) | Drop on resolve fail; rebuild after index |
| Weak hot match hides cold exact | Strict gate; always full if below threshold |
| Short prefixes (`d`, `do`) | Min length or require strong hot score |
| Only apps in usage, no files | Hot empty → full scan |
| Hot folders vs files | Include both (opens of either) |
| Concurrent rebuild | RwLock / swap Arc list like index |
| Privacy | In-memory view of existing local usage.json only |
| `f ` / case prefixes | Strip with existing `strip_file_mode_prefix` before hot phase |
| Deep search still scheduled | Unchanged `should_deep_search`; hot only affects index phase |

---

## 10. Settings / knobs (optional product)

| Setting | Default | Notes |
|---------|---------|-------|
| Prefer frequent files | on | Off → always full free-text scan |
| Hot set size | 64 | Advanced; clamp 16–256 |

Not required for MVP (constants + compile-time caps are enough).

---

## 11. MVP implementation checklist

- [x] `UsageStore::top_path_ids(n)` (or equivalent filter on `top`)  
- [x] `HotSet` on `FileProvider`: rebuild from usage ∩ index  
- [x] Rebuild on index ready + usage dirty (next search / reindex)  
- [x] Free-text: score hot first with shared scoring helpers (Batch A; full scan always follows)  
- [ ] Skip full index only if `best_hot >= 30_000` (tunable)  
- [ ] Path/glob/scope never use hot short-circuit  
- [ ] Unit tests: empty hot; strong hot skip; weak hot falls through; stale path dropped  
- [ ] Bench: `file_hot` / `file_cold` + document in `docs/bench/`  
- [ ] Manual: open same file 5×, type unique name fragment → fast path; type rare file name → still found  

**Non-goals for MVP:** session-only layer, selection-without-open, inverted index, UI settings panel.

---

## 12. Relation to completed core audit (#1–#12)

Hot-path search **builds on** recent work:

| Prior fix | Why it helps hot set |
|-----------|----------------------|
| #4 exact `resolve_id` | Empty-state resolve cheap when validating hot paths |
| #6 usage debounce + cap | Stable, bounded open history |
| #10 `ExcludeSet` | Full-index fallback stays efficient |
| #11 cheaper merge/dedup | Hot path still merges cleanly in engine |
| #7 headless engine | Easy to bench without 45m thread |

It does **not** replace those fixes; it is the next **search-path** optimization after core hygiene.

---

## 13. Decision summary

| Question | Answer |
|----------|--------|
| Should we use “few dozen regular files”? | **Yes** |
| How? | Hot set from **usage `path:`** ∩ index |
| Does cold search get slower? | **No** — full index when hot is empty/weak |
| Does rare file disappear? | **No** — if hot is weak, full scan |
| Shrink persistent index? | **No** |
| Ship without measurement? | **No** — hot vs cold bench required |

---

## 14. Next action

When implementing: start at **§11 MVP**, measure with **§7.1**, keep gate **§5** conservative, write results under `docs/bench/hot-path-YYYY-MM-DD.*` and a one-line row in `OPTIMIZATION.md`.

Until then this document is the **design authority** for hot-path file search.

---

## 15. Batch A status (2026-07-17)

**Shipped:**

| Piece | Location |
|-------|----------|
| `UsageStore::top_path_ids` | `src/usage.rs` |
| `HotPaths` / `build_hot_set` | `src/providers/files/hot.rs` |
| `FileProvider` holds `HotPaths` | `new_empty(config, usage)` |
| Rebuild on reindex + `note_usage_changed` | `rebuild_index` / `force_rebuild` / `Engine::record_usage` |
| Free-text seeds heap from hot indices first | `score_free_text_full` in `search.rs` |

**Not yet (Batch B):** skip full index when hot is strong (`score >= 30_000`).

**Parity:** cold paths still always scanned — result quality matches pre-hot-path for free-text.

```bash
cargo test hot_tests
cargo build --release --features "layer-shell,bench"
./target/release/blink --bench   # search medians should stay flat vs core audit end
```
