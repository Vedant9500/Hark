# Performance & Effective Rust Review Tracker (Temporary)

Temporary tracking file for a 5-part review of the Blink codebase against:

1. **[The Rust Performance Book](https://nnethercote.github.io/perf-book/)** — latency, allocations, I/O, caching
2. **[Effective Rust](https://www.effective-rust.com/)** — types, panics, shared state, over-optimization, tooling

**Scope:** `src/` only (~21.7k LOC). Build artifacts, packaging, and non-Rust docs are out of scope
except where the Performance Book’s **build configuration** chapter applies (`Cargo.toml` profiles / features).

| Part | Name | Approx. LOC | Status |
|------|------|-------------|--------|
| 1 | Core & App Shell | ~3,500 | ✅ complete |
| 2 | UI Shell | ~3,780 | ✅ complete |
| 3 | UI Features & Theme | ~4,750 | ✅ complete |
| 4 | Providers (Apps, Calc, FX, HTTP, Translate) | ~5,030 | ⬜ pending |
| 5 | Files Provider | ~4,620 | ✅ complete |

**Overall status:** 4 / 5 complete

**References:**

| Guide | URL | Used for |
|-------|-----|----------|
| Rust Performance Book | https://nnethercote.github.io/perf-book/ | Hot-path cost, hashing, allocs, I/O, caching |
| Effective Rust | https://www.effective-rust.com/ | Types, `Option`/`Result`, panics, shared state, over-opt |

**Project notes (pre-review):**

- Release profile already sets `lto = true`, `codegen-units = 1`, `opt-level = 3`, `strip = true`,
  `panic = "abort"` — strong defaults per the build-configuration chapter; re-verify during Part 1.
- Optional `bench` feature + `src/bench.rs` micro-bench CLI (`blink --bench`) — exercise under
  the benchmarking chapter rather than only Criterion (crate has no Criterion harness today).
- Hot paths to weight more heavily: search/index (`providers/files/*`), result ranking
  (`engine.rs`), app launch path, typo correction, UI debounce / row rebuilds.

---

## Performance Book checklist (B#)

| # | Chapter / topic | What to look for |
|---|-----------------|------------------|
| B1 | Build configuration | Debug vs release assumptions; feature flags that bloat the daemon; unnecessary deps on hot paths |
| B2 | Profiling | Known hot functions instrumentable? CPU vs allocation vs I/O bound? |
| B3 | Benchmarking | Micro-benches / CLI benches exist for regressions? Stable inputs? |
| B4 | Hashing | Default hasher vs `FxHash` / `ahash` for non-adversarial in-memory maps/sets |
| B5 | Heap allocations | Avoidable `clone` / `to_string` / `to_owned` / `format!` on hot paths; preallocation (`with_capacity`); reuse buffers |
| B6 | Collections | Right structure (`Vec` vs `HashMap` vs `BTree*`); repeated linear scans; unnecessary intermediate `collect`s |
| B7 | Iterators | Extra allocations from intermediate collections; opportunities for fused/chain vs manual loops |
| B8 | Bounds checks | Hot indexed loops that could use iterators / `get_unchecked` only where proven safe & measured |
| B9 | Inlining / monomorphization | Tiny hot helpers not inlined; heavy generic blow-up |
| B10 | Type sizes / layout | Large structs moved a lot; boxing large rarely-used variants; padding waste in hot arrays |
| B11 | Logging / debug | Hot-path `println!` / `dbg!` / expensive `format!` in logging |
| B12 | I/O | Sync blocking on UI thread; repeated open/read; lack of buffering; process spawn vs library |
| B13 | Parallelism / concurrency | Over-sync; lock contention; work that could be off-thread; channel churn |
| B14 | Caching | Recomputation of pure results; stale vs thrash; invalidation cost |

## Effective Rust checklist (E#)

Map of items most relevant to app code. Full list: https://www.effective-rust.com/

| # | Item | What to look for |
|---|------|------------------|
| E1 | Item 1 — express data with types | Enums/structs model domain; avoid stringly APIs on hot paths |
| E3 | Item 3 — `Option`/`Result` transforms | Prefer `?` / combinators over nested `match` where clearer |
| E4 | Item 4 — idiomatic errors | Typed errors vs `String`/`unwrap` for fallible APIs |
| E8 | Item 8 — references & pointers | `Arc`/`&`/`Cow` choices; avoid owned copies when a borrow suffices |
| E9 | Item 9 — iterator transforms | Prefer iterators when they replace alloc-heavy intermediate loops |
| E16 | Item 16 — avoid `unsafe` | No `unsafe` unless measured + justified |
| E17 | Item 17 — shared-state parallelism | Lock scope, poisoning, deadlock risk, prefer message-passing where fit |
| E18 | Item 18 — don't panic | `unwrap`/`expect` on lock/parse paths that can fail in prod |
| E20 | Item 20 — avoid over-optimizing | Measure first; don't micro-opt cold paths or obscure code |
| E27 | Item 27 — document public APIs | `//!` / `///` on provider surface |
| E29 | Item 29 — Clippy | No systematic `allow` without reason |
| E30 | Item 30 — tests beyond unit | Integration / property / bench coverage for regressions |

**Severity labels for findings:**

| Label | Meaning |
|-------|---------|
| **P1** | Likely user-visible latency / reliability issue on a hot path — fix or measure + fix |
| **P2** | Real waste or idiomatic gap; cold path or moderate factor — fix when touching the area |
| **P3** | Micro nit / style; only if measured or zero-risk cleanup |
| **Info** | Observation, already good, or needs a benchmark before judging |

---

## Part 1 — Core & App Shell

**Focus:** entrypoint, config load, search engine / ranking, IPC, usage tracking, typo correction,
micro-bench CLI. Highest leverage after Files search for end-to-end query latency.

| File | LOC | Status |
|------|-----|--------|
| `src/main.rs` | ~182 | ✅ |
| `src/config.rs` | ~969 | ✅ |
| `src/engine.rs` | ~809 | ✅ |
| `src/ipc.rs` | ~193 | ✅ |
| `src/usage.rs` | ~289 | ✅ |
| `src/typos.rs` | ~565 | ✅ |
| `src/bench.rs` | ~485 | ✅ |

**Also in scope for this part:** `Cargo.toml` `[profile.release]`, `[features]` (`bench`, `layer-shell`).

**Part status:** ✅ complete  
**Reviewed:** 2026-07-23  
**Against:**

- [The Rust Performance Book](https://nnethercote.github.io/perf-book/)
- [Effective Rust](https://www.effective-rust.com/)

### Reviewer notes

#### Verdict

**Orchestration is in good shape; no Part-5-scale hot-path leaks.** After the Files provider
work, Core is mostly a careful merge layer: early outs for calc/translate/force-files, apps-before-
files ranking, async deep only when needed, Arc config, debounced usage I/O, and a real
`blink --bench` harness.

There is **no hard P1** confined to this part. Remaining issues are moderate: per-open FS walks
for deep-root auto-promote, lock `.unwrap()` / poison policy, pretty JSON for typos (vs compact
usage), and micro costs on empty-query resolve. Effective Rust Item 20 applies — do not churn
this layer without measurement; biggest search cost still lives in Files (Part 5).

#### Findings

##### P1 — none in Core alone

Keystroke path is `Engine::search` → providers. Local work in engine is small (≤25 results,
sort, boost, typo lookup). Any remaining **user-visible** free-text latency is dominated by
`FileProvider` (already reviewed/fixed in Part 5). No Core-only change is expected to move p95
as much as Files did.

##### P2 — auto-promote deep root walks the FS on every file open (**B12**, **E20**)

`maybe_auto_promote_deep_root` (called from `Action::OpenPath` in `execute`):

- Up to 6 parent levels × several marker probes (`.git`, `Cargo.toml`, `package.json`, …)
- Each probe is `Path::exists()` / join — fine on local SSD, painful on slow / network mounts
- Runs on the **UI thread** via `execute`

**Fix ideas (measure first):**

- Skip if parent is already under a pinned deep root
- Cap to one `metadata` / stop early when path is under `$HOME` depth-N without markers
- Defer promote to a background thread (config update is rare)

Not every open needs this; only files opened deeper than index depth benefit.

##### P2 — lock `.unwrap()` on config / usage / typos (**E17**, **E18**)

Same pattern as Part 5: `RwLock` / `Mutex` with `.unwrap()` on poison. Release `panic = "abort"`
means poison → process death. Poison only follows a prior panic under the lock.

| Store | Locks |
|-------|--------|
| `ConfigStore` | `RwLock<Arc<BlinkConfig>>` |
| `UsageStore` | `RwLock` + `Mutex` (last_save) |
| `TypoStore` | `RwLock` + `Mutex` |

**Recommendation:** same as Files — `parking_lot` or recover via `into_inner` on internal maps
when next touching this code. Not a latency fix.

##### P2 — `TypoStore` saves pretty JSON; `UsageStore` already compact (**B5**, **B12**)

```rust
// typos.rs maybe_save
serde_json::to_string_pretty(&data)

// usage.rs save
serde_json::to_vec(&*g)  // compact
```

Aliases are capped (300) so pretty cost is small, but it is pure overhead (humans rarely hand-edit
typos.json). Align with usage: compact `to_vec` + atomic rename (typos currently writes directly
without `.tmp` rename — weaker crash safety than usage/config).

##### P2 — empty-query path resolves usage ids with FS checks (**B12**)

`empty_results` → `usage.top(20)` → `resolve_id` → `files.resolve_path` (`path.exists()`,
`is_dir()`). Opening the launcher with an empty query can touch the disk up to ~20 times.
Usually warm page cache; still a footgun on slow storage.

**Fix ideas:** cache last-resolved SearchResults for top ids; skip `exists` and let open fail
later; resolve apps first (no FS).

##### P3 — per-result `usage.boost` takes a read lock each time (**B13**)

```rust
for r in &mut results {
    r.score += self.usage.boost(&r.id);  // N ≤ 25
}
```

Correct and cheap at N=25. Optional: one `read()` and score all ids under a single guard if
profiling ever shows lock noise (unlikely).

##### P3 — std `HashMap` for usage / typo maps (**B4**)

Maps stay ≤500 / ≤300 entries. SipHash is fine; FxHash would be micro-gain only. **E20**: skip
unless measured.

##### P3 — typo learn path allocates for Levenshtein (**B5**)

`levenshtein` / `near_title_prefix` build `Vec<char>` and temporary prefix `String`s. Only on
**learn** (launch), not every keystroke. O(n·m) on short strings is fine. Optional later:
stack buffers / byte paths for ASCII aliases.

##### P3 / Effective Rust — types & APIs

| Topic | Assessment |
|-------|------------|
| **E1** `ExecuteOutcome`, `DeepMode` (files), config enums | Good domain modeling |
| **E4** `add_typo_alias` → `Result<String, String>` | UI-facing; acceptable |
| **E8** `ConfigStore::snapshot` → `Arc` | Exemplary; docs push hot paths off `get()` |
| **E8** Engine holds `Arc` providers | Share across threads cleanly |
| **E16** | No `unsafe` in Part 1 |
| **E17** | Warm/reindex on bg threads; IPC listener → channel → GTK main |
| **E18** | Lock unwraps; IPC uses `unwrap_or` on read (good) |
| **E20** | Provider gating (skip files when app prefix strong, skip deep on translate/calc) is purposeful |
| **E27** | `typos` / `bench` have `//!`; `engine` / `config` / `ipc` thinner |
| **E30** | Unit tests: force-files, deep-root, usage debounce, typo learn, IPC path; bench CLI is real tooling |

##### Info — architecture already aligned with the books

| Pattern | Where | Book lens |
|---------|--------|-----------|
| Release: LTO, `codegen-units=1`, `opt-level=3`, `strip`, `panic=abort` | `Cargo.toml` | **B1** |
| `bench` feature keeps daemon lean | `main` + `Cargo.toml` | **B1**, **E26**-ish |
| Hotkey: IPC toggle, no second GTK process when daemon up | `main` + `ipc` | **B12**, startup |
| Index / apps warm + reindex off UI thread | `Engine::spawn_*` | **B13** |
| No network FX at boot (deferred) | `new_headless` comment | **B12**, battery |
| Calc / translate own query → skip apps/files/deep | `search` / `should_deep_search` | **E20**, **B14** |
| Dedup by `id` without cloning `String` keys | `Engine::search` | **B5** |
| Usage save debounce + atomic rename + compact JSON | `usage.rs` | **B12**, **B14** |
| Config Arc swap; skip save when unchanged | `config.rs` | **B5**, **B12** |
| `with()` for borrow without full clone | `ConfigStore` | **E8** |
| Scoped-query memo lives in Files (engine reuses) | cross-part | **B14** |
| `blink --bench` isolated provider probes + p95 | `bench.rs` | **B3** |

#### Checklist snapshot (Part 1)

| Lens | Result |
|------|--------|
| B1 Build config | Strong release profile; optional features correct |
| B3 Benchmarking | Real CLI harness; use it before “optimizing” engine |
| B4 Hashing | Small maps; ignore |
| B5 Allocations | Fine for ≤25 merge; typos pretty JSON is the odd one out |
| B12 I/O | Auto-promote + empty resolve are the only FS nits |
| B13 / E17 | Warm/deep/IPC threading is sound |
| B14 Caching | Usage/typo debounce; config Arc; deep/live in Files |
| E16 unsafe | Pass |
| E18 panics | Lock unwraps only |
| E20 over-opt | Restraint good; no Core rewrites without bench |

#### What already looks good

- `Engine::search` ranking policy is explicit and documented in comments (apps vs files bands)
- Translate / calc short-circuits prevent deep-walk stutters (comment in `should_deep_search`)
- Dedup keeps first occurrence without `HashSet<String>` of owned ids
- Config: Arc snapshot + equality-gated save (no thrash on no-op updates)
- Usage: compact JSON, debounce, prune cap, `Drop` flush
- IPC: stale socket reclaim, 0600 perms, short timeouts, optional ack
- Headless engine for CLI/bench without eternal 45m thread
- Feature-gated bench module so default builds stay small

#### Suggested measurements

```bash
cargo build --release --features "layer-shell,bench"
./target/release/blink --bench

# Empty-open / open-file latency if chasing P2s
# samply record ./target/release/blink --daemon
```

| Experiment | Success signal |
|------------|----------------|
| `iso_files` / `file` cases in `--bench` | Regression guard after any engine merge change |
| Open file under deep tree | Wall time of `execute(OpenPath)` before/after promote deferral |
| Empty query open | Time to first paint / first `empty_results` |

#### Priority order for fixes (optional)

1. **Defer or gate `maybe_auto_promote_deep_root`** if open feels slow on large/network trees  
2. **Typo save: compact JSON + atomic rename** (parity with usage)  
3. **Empty-state resolve:** fewer `exists` calls  
4. Lock policy / `parking_lot` when touching concurrency  
5. Do **not** micro-opt `usage.boost` or Levenshtein without numbers  

#### Fixes applied (2026-07-23)

- [x] **Deep-root auto-promote** — skip if path already under a pin; FS marker walk + promote on a **background thread** (no UI-thread `exists` storm); shared `promote_deep_root_arcs` for worker
- [x] **Typos save** — compact `serde_json::to_vec` + atomic `.json.tmp` rename (parity with usage); test `save_is_compact_json`
- [x] **Empty results** — cap path resolves to 8; `resolve_path` uses one `symlink_metadata` (not `exists` + `is_dir`)
- [x] **Lock poison recovery** — `unwrap_or_else(|p| p.into_inner())` on config / usage / typo / mounts read used by resolve
- [x] Tests: **74 passed**, 2 ignored

---



## Part 2 — UI Shell

**Focus:** main window / list UI, row rendering, footer, action panel, drag-and-drop, thumbnails.
Watch for per-keystroke work, full list rebuilds, icon/thumbnail I/O on the GTK thread.

| File | LOC | Status |
|------|-----|--------|
| `src/ui/mod.rs` | ~2197 | ✅ |
| `src/ui/rows.rs` | ~496 | ✅ |
| `src/ui/dnd.rs` | ~368 | ✅ |
| `src/ui/action_panel.rs` | ~247 | ✅ |
| `src/ui/thumbnails.rs` | ~225 | ✅ |
| `src/ui/style.css` | 166 | ✅ (CSS; paint cost only) |
| `src/ui/footer.rs` | 80 | ✅ |

**Part status:** ✅ complete  
**Reviewed:** 2026-07-23  
**Against:**

- [The Rust Performance Book](https://nnethercote.github.io/perf-book/)
- [Effective Rust](https://www.effective-rust.com/)

### Reviewer notes

#### Verdict

**Strong interactive architecture; remaining cost is mostly engine/providers, not widget churn.**
The shell already applies the right Performance Book / Effective Rust lessons for a GTK launcher:
keystroke debounce, row **pool** rebind (no per-search widget trees), generation tokens to drop
stale async deep/translate, single-flight worker queues, DnD guard against mid-drag rebind, and
cached UI knobs (`ui_icon_size` / symbolic / compact) so search does not clone full config.

No Part-2 **P1** that is clearly user-visible after Parts 1 and 5. Optional P2/P3: icon theme
`has_icon` on first bind of a name, main-thread FreeDesktop thumb I/O on drag begin, heavy
`Rc` clone fan-out in `refresh_results` / closures (idiomatic GTK, not free), and action-panel
rebuild of all buttons on each open.

Preview/settings/theme are **Part 3** — not fully scored here even where `mod.rs` calls them.

#### Findings

##### P1 — none confined to UI Shell

Keystroke path: debounce → `engine.search` → `ResultRowPool::apply` (≤25 binds) → optional
async deep/translate. Widget work is bounded; search cost is still dominated by providers
(Files/apps). Do not rewrite the shell without `blink --bench` + typing latency numbers (**E20**).

##### P2 — icon resolve can hit `IconTheme::has_icon` on cache miss (**B5**, **B12**)

`rows.rs` `resolve_row_icon` / `resolve_row_icon_uncached`:

- Thread-local cache (cap 512, then clear) — good (**B14**)
- On miss: up to two `theme.has_icon` calls (symbolic candidate + base) on the **GTK main thread**
- Cache key rebuilds with `format!` on insert and again when re-inserting after clear

First time a given icon name appears in a session can stall a frame slightly (theme lookup).
Steady typing after warm cache is fine.

**Fix ideas:** pre-warm common icons at startup; LRU instead of full clear at 512; store
`&'static str` / interned names where possible; avoid double `format!` when re-inserting after clear.

##### P2 — drag thumbnail path does sync FS on main thread (**B12**)

`dnd.rs` `drag_thumbnail_icon` → `freedesktop_thumbnail`:

- `canonicalize`, up to 3 `is_file` probes under `~/.cache/thumbnails/…`
- Only for image extensions; comment notes thumbs are small and already on disk
- Drag begin is infrequent vs keystrokes — acceptable, but network home / cold cache can hitch

**Fix ideas:** memoize last path→thumb; skip canonicalize when path is already absolute and known;
load texture async only if first hit is slow (probably overkill — **E20**).

##### P2 — `refresh_results` clones full result vec before deep gate (**B5**)

```rust
let current = results.borrow().clone();
if !engine.should_deep_search(&q, &current) { return; }
```

`should_deep_search` only needs scores/kinds/ids. Cloning ≤25 `SearchResult`s is small but
unnecessary if the API accepted a slice of the borrowed vec (`results.borrow()` +
`should_deep_search(&q, &results.borrow())` with restructure, or a lighter view type).

Also: many `Rc`/`Arc`/widget clones when scheduling translate + deep futures — normal for
glib closures; not free, but not the bottleneck (**E8**).

##### P2 — action panel rebuilds entire button list every open (**B5**)

`ActionPanel::open_for` removes all children and builds new `Button` trees per open. Specs are
few (usually &lt; 10). Fine for now; a tiny pool would only matter if profiling shows open lag.

##### P3 — icon cache clear thrash (**B14**)

At 513 entries the whole map is cleared. Bursty unique icons (many different file types) can
oscillate. Prefer random eviction / simple LRU ring of 512.

##### P3 — custom MD5 in `thumbnails.rs` (**E20**, deps)

Hand-rolled MD5 for FreeDesktop names — correct and dependency-free. Not a hot keystroke path.
Leave unless you already pull a hash crate for something else.

##### P3 — CSS (`style.css`)

Not Rust. Fixed-width shell, no per-frame style injection in this part (theme inject is Part 3).
No performance action.

##### P3 / Effective Rust

| Topic | Assessment |
|-------|------------|
| **E1** `DragSession`, `ActionPanel`, pool modes | Clear domain split |
| **E8** `Rc`/`Cell`/`RefCell` for GTK single-thread | Correct model; large clone fan-out is the tax |
| **E16** | No `unsafe` in Part 2 sources reviewed |
| **E17** | Deep/translate workers + gen tokens + single-flight queues — good shared-state discipline |
| **E18** | Few unwraps in shell; worker queues still `lock().unwrap()` (same poison note as Part 1) |
| **E20** | Debounce 40ms / translate 180ms, compact idle skip search, drag rebind guard — purposeful |
| **E27** | `rows`, `dnd`, `action_panel`, `thumbnails` have `//!`; `footer` thinner |
| **E30** | Tab-complete unit tests in `mod.rs`; row pool behavior is integration-hard without GTK |

##### Info — architecture already aligned with the books

| Pattern | Where | Book lens |
|---------|--------|-----------|
| Search debounce 40ms; translate 180ms | `mod.rs` | **B5**, typing CPU |
| Compact idle: skip `engine.search` when body hidden | `refresh_results` | **E20**, **B14** |
| `ResultRowPool` rebind; remove unused rows (height) | `rows.rs` | **B5**, GTK layout |
| Icon resolve cache (TLS HashMap) | `rows.rs` | **B14** |
| UI knobs in `Cell` (no config clone per search) | `Launcher` | **B5**, **E8** |
| `deep_gen` invalidates stale async applies | `mod.rs` | **B13**, races |
| Single-flight deep + translate workers | `schedule_*_job` | **B13**, **E17** |
| Skip row rebind while DnD active | `refresh_results` | correctness + perf |
| Network translate never on main | comment + worker | **B12** |
| FreeDesktop thumb read for drag (no full decode) | `dnd.rs` | **B12** |

#### Checklist snapshot (Part 2)

| Lens | Result |
|------|--------|
| B5 Allocations | Pool good; result clone before deep gate; Rc fan-out |
| B12 I/O | Icon theme + drag thumb on main (bounded) |
| B13 / E17 | Workers + gen + single-flight solid |
| B14 Caching | Icon cache; debounce; compact idle |
| E16 unsafe | Pass |
| E18 panics | Worker mutex unwrap only |
| E20 over-opt | Shell is restrained; don't micro-opt without typing traces |

#### What already looks good

- Explicit comments on why ListBox rows are **removed** not hidden (height)
- Conversion rows swap child instead of Stack double-height
- Path drag binding reused on pooled rows (`PathDragBinding`)
- Action panel uses Buttons (not flaky ListBox-in-Popover under layer-shell)
- Footer is trivial; no per-keystroke work beyond label text
- Layer-shell keyboard mode toggles for DnD carefully documented

#### Suggested measurements

```bash
cargo build --release --features "layer-shell,bench"
# Typing: samply/perf on blink daemon while hammering the search entry
# Compare: cold icon set vs warm; drag-start of image file vs folder
```

| Experiment | Success signal |
|------------|----------------|
| Keystroke → list update | p95 under debounce + search budget |
| First appearance of rare mime icons | No multi-frame hitch after optional prewarm |
| Drag image with/without FD thumb | Drag begin &lt; few ms |

#### Priority order for fixes (optional)

1. **Avoid full `results` clone** for `should_deep_search` (borrow / thinner check)  
2. **Icon cache:** LRU or no double-format on reinsert; optional prewarm  
3. **Drag thumb path memo** if drag begin shows up in profiles  
4. Action-panel button pool — only if open feels slow  
5. Do **not** remove debounce or pool “for cleanliness”

#### Fixes applied (2026-07-23)

- [x] **Deep gate** — `should_deep_search` uses `results.borrow().as_slice()` (no full vec clone)
- [x] **Icon cache** — FIFO eviction at 512 (no full clear thrash); single insert path (no double `format!`)
- [x] **Drag thumbnail memo** — last path → texture in TLS so repeated drag-begin skips FD probes
- [x] Tests: **74 passed**, 2 ignored

---



## Part 3 — UI Features & Theme

**Focus:** settings UI, preview pane, open-with dialog, theme / CSS generation and injection.
Mostly cold paths (settings, theme apply) but preview can sit on the interactive path.

| File | LOC | Status |
|------|-----|--------|
| `src/ui/settings.rs` | ~2213 | ✅ |
| `src/ui/preview.rs` | ~1081 | ✅ |
| `src/theme/css.rs` | ~935 | ✅ |
| `src/ui/open_with.rs` | ~343 | ✅ |
| `src/theme/mod.rs` | ~175 | ✅ |

**Part status:** ✅ complete  
**Reviewed:** 2026-07-23  
**Against:**

- [The Rust Performance Book](https://nnethercote.github.io/perf-book/)
- [Effective Rust](https://www.effective-rust.com/)

### Reviewer notes

#### Verdict

**Preview is the only interactive hot path; it is already well designed.** Debounce (45 ms),
single-flight decode worker, gen tokens, FreeDesktop thumb first, LRU texture cache (24) with
mtime/size fingerprint, and process spawns (`ffmpeg` / `pdftoppm`) off the GTK main loop —
this is the right Performance Book shape for a media side-panel.

Settings + theme are **cold / user-driven**: full settings UI is built once at launcher start
(all pages eagerly), and appearance steppers re-read `scheme.json` and inject ~19 KB of CSS on
every click. That can hitch while scrubbing opacity/radius, but it is not on the keystroke or
result-list path. Open With is infrequent and does MIME + GIO app enumeration on the main thread
when the popover opens — acceptable for a modal-ish picker (**E20**).

**No Part-3 P1.** Remaining items are main-thread FS on selection, full CSS regenerate per
tweak, eager settings construction, and small idiomatic nits.

#### Findings

##### P1 — none confined to UI Features & Theme

Selection → `PreviewPanel::update` either paints a cache hit or schedules a debounced worker.
Heavy decode never runs on the main loop. Settings/theme/open-with are off the search hot path.
Do not rewrite CSS generation or settings structure without measuring (**E20**).

##### P2 — selection update does multiple main-thread FS probes (**B12**, **B5**) — **fixed**

Was: `is_dir` + `file_meta` metadata + `FileFp::of` metadata (same inode thrice) and
`media_kind` allocated a lowercased extension `String`.

**Applied:** one `std::fs::metadata` in `update`; derive dir-ness, `FileFp`, and labels via
`file_meta_from`; pass `fp` into `queue_image_load` (no second probe). `media_kind` uses
`eq_ignore_ascii_case` against static lists (no alloc).

##### P2 — every appearance tweak reloads scheme + regenerates full CSS (**B5**, **B12**, **B14**) — **fixed**

Was: each stepper `theme.reload()` → re-read `scheme.json` + full CSS inject.

**Applied:**

- `ThemeManager` caches last `Theme` after `apply()` (startup + scheme monitor)
- `reload()` is **UI-only** (`apply_ui_only`): uses cached colours + current `UiThemeConfig`
- `reload()` debounced 60 ms; cancelled / gen-bumped when full `apply()` runs so stale
  debounce callbacks do not fight scheme reloads

Still regenerates ~19 KB CSS on inject (necessary for GTK) but skips scheme disk I/O on
appearance steppers.

##### P2 — settings builds all pages eagerly at app start (**B5**, **B10**) — **deferred (E20)**

Still builds all eight pages in `SettingsPanel::new`. Lazy stack children is a larger
refactor; skip until startup profiles show settings as a meaningful slice.

##### P2 — Open With enumerates apps on the GTK main thread (**B12**) — **partial fix**

Still runs GIO `recommended` / `all` / `default` on the main thread (cold path).

**Applied:** single `content_type_for_path` shared by subtitle + `apps_for_content_type`
(was probing content type twice). Full MIME→apps reuse via apps provider deferred (**E20**).

##### P3 — `normalize_hex` both branches identical (**E3**) — **fixed**

Collapsed to `format!("#{v}")` after trim.

##### P3 — preview texture cache uses std `HashMap` + linear LRU vec (**B4**, **B6**)

Cap 24 — std hasher is fine (**E20**). `touch_cache` / eviction scan `Vec` of 24 `PathBuf`s
linearly — trivial. Same pattern as icon cache; no change required unless cache grows a lot.

##### P3 — `resolve_icon_name` in preview hits `IconTheme::has_icon` (**B12**)

Same class of cost as Part 2 row icons, only for icon-mode preview (audio / non-PDF docs).
Infrequent vs list binds. Optional: share Part 2’s resolve cache.

##### P3 — `Display::default().expect("display")` at theme init (**E18**)

Startup-only; GTK without a display is fatal anyway. Acceptable.

##### Info — CSS is one giant compile-time template (**B5**, **E20**)

`css::render` interpolates theme tokens into a fixed ~19 KB stylesheet. No per-frame work.
Alternative (load static CSS + GTK CSS variables) would be a design change, not a quick win.
Leave as-is.

##### Info — `pixbuf.pixels()` copy uses `unsafe` (**E16**)

Documented need: GObject pixel view must be copied before send across threads. Scoped and
justified; keep.

#### What already looks good

| Area | Why |
|------|-----|
| Preview debounce + gen | Latest selection wins; arrow spam does not stack decodes |
| Single-flight worker | At most one decode thread; queue via `inflight` |
| Texture LRU + `FileFp` | mtime/size invalidate; no stale pixels after overwrite |
| FreeDesktop thumb first | Avoid re-decode when thumbnails already exist |
| Video/PDF off-main | `ffmpeg` / `pdftoppm` only on worker thread |
| Theme file monitor | Debounced 80 ms; **no** busy poll (battery-aware comment) |
| Open With layer-shell | In-window popover avoids broken external portals |
| Settings open-with picker | Uses engine app list, not full GIO rescan for defaults UI |
| Soft-fail external tools | Missing ffmpeg/pdftoppm → “Could not load preview”, no panic |

#### Effective Rust map (Part 3)

| Item | Notes |
|------|-------|
| **E8** | Preview paths clone `PathBuf` for workers/closures — necessary for `'static` threads |
| **E16** | One justified `unsafe` pixel copy in `pixbuf_to_pixels` |
| **E17** | No shared locks in this part; `Rc`/`RefCell` GTK main-thread model |
| **E18** | No production `.unwrap()` on fallible I/O; `expect("display")` only at theme boot |
| **E20** | Do not over-optimize settings page build or CSS template without profiles |
| **E27** | `open_with` has `//!`; preview/settings/theme thinner module docs |

#### Suggested measurements

- Arrow through 50 large images with preview open: main-thread time in `update` vs worker
- Hold opacity −/+ in Settings: time in `Theme::load` + `load_from_string` per click
- Open With on a MIME type with many handlers: time to `popover.popup()`
- Cold start: time spent in `SettingsPanel::new` vs rest of window setup

#### Optional fixes — applied 2026-07-23

| # | Fix | Status |
|---|-----|--------|
| 1 | Single `metadata` in preview `update` / `queue_image_load` | ✅ |
| 2 | `ThemeManager` cached `Theme` + UI-only CSS path | ✅ |
| 3 | Collapse `normalize_hex` | ✅ |
| 4 | Debounce appearance `theme.reload` (60 ms) | ✅ |
| 5a | Open With: one content-type probe | ✅ |
| 5b | Lazy settings pages / Open With apps-provider reuse | ⬜ deferred (**E20**) |

#### Part checklist (B/E coverage)

| Check | Result |
|-------|--------|
| B5 Heap | CSS still regen on inject; `media_kind` no longer allocs; settings still eager |
| B12 I/O | One metadata per selection; scheme disk only on apply/monitor; Open With GIO once CT |
| B13 Concurrency | Preview single-flight good; no lock contention here |
| B14 Caching | Texture LRU + cached theme colours for UI reloads |
| E16 unsafe | Justified pixel copy |
| E18 panic | Clean except display expect |
| E20 over-opt | Lazy settings / MIME app cache left until measured |
- Preview content load: size limits, cancellation when selection changes quickly.
- Settings open cost (one-shot is fine if bounded).

#### Fixes applied

- [ ] …

---

## Part 4 — Providers (Apps, Calc, FX, HTTP, Translate)

**Focus:** provider trait / registry, app launcher, calculator submodules, FX rates, HTTP helper,
translate. Mix of pure CPU (calc, fuzzy apps) and network (FX, translate).

| File | LOC | Status |
|------|-----|--------|
| `src/providers/mod.rs` | ~322 | ⬜ |
| `src/providers/apps.rs` | ~442 | ⬜ |
| `src/providers/fx.rs` | ~261 | ⬜ |
| `src/providers/http.rs` | 78 | ⬜ |
| `src/providers/translate.rs` | ~1016 | ⬜ |
| `src/providers/calc/mod.rs` | ~150 | ⬜ |
| `src/providers/calc/expr.rs` | ~391 | ⬜ |
| `src/providers/calc/math.rs` | ~160 | ⬜ |
| `src/providers/calc/units.rs` | ~553 | ⬜ |
| `src/providers/calc/timezone.rs` | ~757 | ⬜ |
| `src/providers/calc/datetime.rs` | ~177 | ⬜ |
| `src/providers/calc/duration.rs` | ~108 | ⬜ |
| `src/providers/calc/currency.rs` | ~128 | ⬜ |
| `src/providers/calc/battery.rs` | ~444 | ⬜ |
| `src/providers/calc/util.rs` | 70 | ⬜ |

**Part status:** ⬜ pending  
**Reviewed:** —  
**Against:** https://nnethercote.github.io/perf-book/

### Reviewer notes

_(fill during review)_

#### Verdict

_(one paragraph)_

#### Findings

##### P1 —

##### P2 —

##### P3 / Info —

#### What already looks good

- 

#### Suggested measurements

- App provider: desktop-file scan + match latency; cache invalidation.
- Calc: regex compile once (`once_cell` / `Lazy`) vs per query; expression parse cost.
- FX / translate: request coalescing, timeouts, no UI-thread blocking (confirm worker design).
- Provider fan-out: sequential vs parallel; early cancel when query changes.

#### Fixes applied

- [ ] …

---

## Part 5 — Files Provider

**Focus:** file index, search, hot paths, live cache, files provider module. Largest performance
surface in the app.

| File | LOC | Status |
|------|-----|--------|
| `src/providers/files/search.rs` | ~3074 | ✅ |
| `src/providers/files/mod.rs` | ~627 | ✅ |
| `src/providers/files/index.rs` | ~578 | ✅ |
| `src/providers/files/live_cache.rs` | 187 | ✅ |
| `src/providers/files/hot.rs` | ~158 | ✅ |

**Part status:** ✅ complete  
**Reviewed:** 2026-07-23  
**Against:**

- [The Rust Performance Book](https://nnethercote.github.io/perf-book/)
- [Effective Rust](https://www.effective-rust.com/)

### Reviewer notes

#### Verdict

**Solid architecture, a few measurable hot-path leaks.** The design already follows several
Performance Book and Effective Rust lessons well: short index lock scopes, top-K heaps, hot-set
short-circuit, live-cache with `Arc<[SearchResult]>`, budgeted deep walks off the main lock,
pre-lowercased index fields, and `DeepMode` as a real enum rather than stringly flags.

The main gaps are **per-score heap allocations in free-text ranking** (`format!` in
`apply_path_boosts`), **O(n) string-key maps rebuilt on every hot refresh**, **JSON index cache
I/O cost**, and **lock `.unwrap()` panics** on shared state. Effective Rust Item 20 applies: fix
only what measurement confirms; the rest are good opportunistic cleanups.

No `unsafe`. No systematic over-engineering. Biggest wins are cheap to try and easy to bench.

#### Findings

##### P1 — hot-path allocations in free-text scoring (**B5**, **E20**)

`score_name_only` → `apply_path_boosts` runs for **every** index entry on a full scan (up to
`MAX_INDEX` = 100k). Inside the hot loop:

```rust
// search.rs — apply_path_boosts
if item.path_lower.contains(&format!("/{q_lower}")) || item.path_lower.ends_with(q_lower) {
    score += 2_000;
}
```

That allocates a new `String` per scored item for a path-segment check. Also, each result materializes
via `indexed_to_result`:

- `format!("path:{}", …)` for `id`
- `pretty_path` (home lookup + `format!`) for subtitle
- `path.clone()` into `Action::OpenPath`
- `icon: Some(… .into())` → heap `String` even though `icon_for_path` returns `&'static str`

Top-K is only 25, so result construction is smaller than the score loop, but the `format!` inside
`apply_path_boosts` is **O(index size)** per keystroke on non-hot-skip queries.

**Fix (low risk):**

1. Replace `format!("/{q_lower}")` with a stack buffer or two-slice check
   (`path_lower.ends_with(q_lower)` already exists; for mid-path use
   `path_lower.contains` with a prebuilt `/{q}` once **outside** the loop, or
   `split`/`windows` without alloc).
2. Precompute `needle = format!("/{q_lower}")` once in `score_free_text_*` and pass `&str`.
3. Optionally keep icon as `Option<&'static str>` or `Cow<'static, str>` on `SearchResult`
   (cross-cutting; Part 1/engine may need to agree).

**Measure first** with `blink --bench` / heaptrack on a full free-text query against a full index.

##### P1 — hot-set rebuild clones entire index path map (**B5**, **B6**, **B4**)

`hot.rs` `build_hot_set`:

```rust
let mut by_path: HashMap<String, usize> = HashMap::with_capacity(index.len());
for (idx, item) in index.iter().enumerate() {
    by_path.entry(item.path_lower.clone()).or_insert(idx);
}
```

On every dirty rebuild this **clones every `path_lower`** (up to 100k strings) into a
`std` `HashMap` (SipHash). Hot set cap is only 64 — the reverse lookup should not cost a full
index string clone.

**Fix options (prefer measured):**

| Option | Notes |
|--------|--------|
| A. Build `HashMap<&str, usize>` from `index` | Lifetime tied to index read lock — already held by caller in `ensure_fresh` / `rebuild` |
| B. Keep a persistent `path_lower → idx` beside the index | Invalidate on rebuild; avoid per-hot rebuild |
| C. Linear scan for ≤128 wanted paths | 64 × 100k strcmp may still beat alloc of 100k keys — measure |
| D. `FxHashMap` / `hashbrown` | Secondary; only after eliminating clones (**B4**) |

Also `snapshot_indices` clones the small `Vec<usize>` each search — fine (≤64).

##### P2 — index on-disk cache is JSON + full string rewrite (**B12**, **B5**)

`index.rs` `save_cache` / `load_cache`:

- Full `serde_json::to_vec` of up to 100k path strings
- Atomic rename (good)
- Compact `CacheEntry` (only path / is_dir / depth) — good design
- `load_cache` re-derives `name_lower` / `path_lower` / flags via `make_indexed` — correct but CPU-heavy on cold start

Fingerprint uses `DefaultHasher` (not cryptographic need — OK for local cache key; **B4** is about
in-memory maps, not this).

**Opportunistic improvements:**

- `bincode` / `rkyv` / `postcard` for binary cache (faster load; version already gated)
- `Vec::with_capacity` on walk is 4096 — fine; could ramp or reserve previous size
- `is_high_value_path` calls `dirs::home_dir()` and `format!("{home_s}/")` **per entry** during
  index build — cache home prefix once outside the loop (**B5**)

##### P2 — live deep walk allocates aggressively (**B5**, **B12**)

`live_deep_under_roots` (async path, budgeted — good):

- Per visit: `name.to_lowercase()`, `path.to_string_lossy().to_lowercase()`, `format!("path:…")`,
  `make_indexed` (more lowercasing + path clone), then another `path.to_path_buf()` into top-K
- Top-K uses linear min scan on a ≤25 vec (fine)
- `existing: HashSet<String>` of result ids rebuilt from clones in `run_deep_jobs`

Budgets (`40ms` sync / `200ms` async, visit caps) already limit worst case — aligns with **E20**
(don't unbounded-walk). Still, reducing per-node allocs improves how deep you get within budget.

**Fix ideas:** reuse a `String` buffer for `path_lower`; score without full `IndexedPath`;
`HashSet` of path indices or `PathBuf` only where needed.

##### P2 — result merge / cache put clones full `SearchResult` graphs (**B5**, **E8**)

`mod.rs`:

```rust
self.live_cache.put(query, results.clone());  // deep mode
// …
merge_cached: HashSet of id clones + r.clone() per cached hit
```

`LiveCache` already stores `Arc<[SearchResult]>` and returns Arc clones on get — good (**E8**).
But `put` takes `Vec` after a full clone of results, and `get` for deep full-hit path does
`cached.to_vec()` (clones every `SearchResult` out of the Arc).

**Fix:** `put` can take ownership of `results` without clone when you're done with them; full-hit
return path could return `Arc` or write into a shared buffer if the engine allows. Cross-module
API change — coordinate with engine/UI.

##### P2 — lock poisoning via `.unwrap()` (**E17**, **E18**)

Widespread:

| Location | Pattern |
|----------|---------|
| `index` / `mounts` / `fingerprint` | `read().unwrap()` / `write().unwrap()` |
| `hot.set` | `write().unwrap()` / `read().unwrap()` |
| `live_cache.inner` | `lock().unwrap()` |
| `scoped_memo` | `lock().unwrap()` |

With `panic = "abort"` in release, a poisoned lock aborts the process. Poisoning only happens after
a panic while holding the lock — so this is secondary to not panicking elsewhere. Still, Effective
Rust Item 18 prefers recovering or using `parking_lot` (no poison) for internal caches.

**Recommendation:** `parking_lot::{Mutex,RwLock}` for internal non-poison maps, or
`.unwrap_or_else(|e| e.into_inner())` for “keep going” recovery on caches. Not a latency P1.

##### P2 — mounts / config snapshot cloning each search (**B5**, **E8**)

```rust
let cfg = self.state.config.snapshot(); // Arc — good
let mounts = self.state.mounts.read().unwrap().clone(); // full Vec clone per search
```

Mount lists are small; OK. If `pretty_path` / scoring only need `&[MountInfo]`, hold the read guard
for the index phase only (already structured that way for index). Cloning mounts is P3 unless
profiling shows otherwise.

##### P3 — hashing defaults on in-memory maps (**B4**)

`HashMap` / `HashSet` for:

- hot path rebuild (`path_lower` keys)
- live cache query keys
- `seen` sets during walks / merges
- index `seen: HashSet<PathBuf>` during build

None of these are adversarial user-controlled high-QPS hash-DoS surfaces in the same way as a
public HTTP API, but free-text search is interactive: **FxHash / ahash** can shave map ops on
large sets. Only worth it after removing the clone storm in `build_hot_set`.

##### P3 — sort comparators re-lowercase titles (**B5**)

```rust
a.title.to_lowercase().cmp(&b.title.to_lowercase())
```

in `merge_cached` / `merge_live` / absolute glob — only on ≤25 items. Prefer `eq_ignore_ascii_case`
or store `title_lower` if you touch this code; not worth a dedicated change (**E20**).

##### P3 / Effective Rust — types & APIs

| Topic | Assessment |
|-------|------------|
| **E1** `DeepMode`, `IndexedPath`, `CacheEntry` | Good domain modeling |
| **E1** result `id` as `String` (`path:…`) | Stringly; works; parsing elsewhere by prefix |
| **E4** `trash_path` → `Result<(), String>` | Acceptable for UI errors; not hot |
| **E8** `LiveCache` `Arc<[T]>` | Exemplary shared ownership |
| **E8** `ConfigStore` Arc snapshot | Exemplary |
| **E9** scoring | Explicit loops + heap — correct for top-K; iterators wouldn't help much |
| **E16** | No `unsafe` in this part |
| **E20** | Hot skip, visit caps, strong-score early outs — optimization is purposeful, not cargo-cult |
| **E27** | `hot`, `live_cache` have `//!`; `search.rs` / `index.rs` sparse module docs |
| **E30** | Strong unit tests in `search` / `hot` / `live_cache`; worth keeping benches in `bench` feature |

##### Info — architecture already aligned with the books

| Pattern | Where | Book lens |
|---------|--------|-----------|
| Index read lock dropped before WalkDir | `FileProvider::search_with` | **B13**, **E17** |
| Top-K `BinaryHeap` instead of full sort | `score_free_text_*` | **B6**, **B5** |
| Hot short-circuit (`HOT_SKIP_FULL_SCORE`) | `score_free_text_full` | **B14**, **E20** |
| Fuzzy budget (`fuzzy_left = 500`) | `finish_free_text_fuzzy` | **E20** |
| Precomputed `name_lower` / `path_lower` | `IndexedPath` | **B5** |
| Compact disk cache + version + TTL | `index.rs` | **B12**, **B14** |
| Negative TTL for empty deep results | `live_cache` | **B14** |
| LRU + cap (64) on live cache | `live_cache` | **B14** |
| Scoped query memo | `scoped_memo` | **B14** |
| `icon_for_path` → static str names | `mod.rs` | good; only the `String` wrap costs |
| Release profile LTO / abort | `Cargo.toml` | **B1** (crate-wide) |

#### Checklist snapshot (Part 5)

| Lens | Result |
|------|--------|
| B4 Hashing | P3 — std hasher; clone cost dominates |
| B5 Allocations | **P1** — `apply_path_boosts` format; hot rebuild clones; result strings |
| B6 Collections | Good top-K heap; hot map oversized |
| B10 Type sizes | `IndexedPath` holds 3 strings + PathBuf — intentional for search speed |
| B12 I/O | JSON cache cold-start; deep walk budgeted |
| B13 / E17 Concurrency | Good lock discipline; unwrap on poison |
| B14 Caching | Live + disk + hot + scoped memo — strong |
| E16 unsafe | Pass (none) |
| E18 panics | Lock unwraps; test-only expects elsewhere |
| E20 over-opt | Restraint is good; fix measured leaks only |

#### What already looks good

- Phase split: index under short `RwLock`, deep walk after release
- Free-text: name-only pass first, fuzzy only if weak top-K
- Hot set + min query length avoids short-prefix false short-circuits
- Live cache Arc sharing + negative caching
- Disk cache fingerprint ignores content mtimes (avoids thrash) with TTL freshness
- Exclude / low-value / high-value scoring reduces junk in top-K
- Substantial unit coverage for globs, scoped `in`, hot build, cache keys

#### Suggested measurements (do these before large rewrites)

```bash
cargo build --release --features "layer-shell,bench"
./target/release/blink --bench   # file / search cases if present

# Allocation focus on free-text against a warm full index
# heaptrack ./target/release/blink …
# or dhat / samply for CPU
```

| Experiment | Success signal |
|------------|----------------|
| Remove per-item `format!("/{q}")` in scoring | Fewer allocs/query; lower p95 free-text |
| Hot rebuild without cloning all `path_lower` | Faster post-open / post-reindex; less spike RSS |
| Binary index cache | Faster cold start / ensure_fresh from disk |
| Cache home string in `is_high_value_path` | Faster `build_index` only |

#### Priority order for fixes

1. **Precompute path needle** in `apply_path_boosts` call chain (or stop allocating inside it)
2. **Hot set lookup without full-index string clone**
3. **Avoid `results.clone()` before `live_cache.put`** (ownership)
4. **Home-prefix once** in index build high-value check
5. Binary cache / FxHash only if 1–4 aren't enough after measurement
6. Lock poison policy / `parking_lot` when touching concurrency next

#### Fixes applied (2026-07-23)

- [x] `apply_path_boosts`: zero-alloc `path_contains_slash_prefixed` replaces per-item `format!("/{q}")`
- [x] `build_hot_set`: `HashMap<&str, usize>` borrows `path_lower` (no full-index string clone)
- [x] `live_cache.put_returning`: move into `Arc` once; drop double-owned-Vec pattern
- [x] `is_high_value_path` / index load: `OnceLock` home prefix (`home_prefix_lower`)
- [x] Unit test for slash-prefixed path match; full suite **72 passed**, 2 ignored
- [ ] (optional, not done) binary disk cache
- [ ] (optional, not done) `parking_lot` for internal locks

---

## How to use

1. Pick a part (recommended order: **5 → 1 → 4 → 2 → 3** — hottest paths first; or 1 → 5 structural).
2. For each file, walk the **Performance (B#)** and **Effective Rust (E#)** checklists.
3. Prefer measurement over intuition for P1s; always judge hot-path changes in **`--release`**.
4. Flip `⬜` → `🔄` → `✅` per file and for the part when done.
5. Record findings with severity and guide tag (`B5`, `E18`, …).
6. Update the summary table and overall count at the top.
7. Delete this file when the full review is finished (or archive it).

## Status legend

| Symbol | Meaning |
|--------|---------|
| ⬜ | pending |
| 🔄 | in progress |
| ✅ | complete |
| ⛔ | blocked / skipped |

## Quick command palette

```bash
cargo fmt --check
cargo check --features "layer-shell,bench"
cargo test --features "layer-shell,bench"

# Performance-oriented
cargo build --release --features "layer-shell,bench"
./target/release/blink --bench
```
)