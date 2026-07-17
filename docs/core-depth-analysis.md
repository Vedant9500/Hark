# Core depth analysis — `src` (engine, config, main, usage, ipc)

**Date:** 2026-07-17  
**Status:** diagnosis; **#1–#12 applied 2026-07-17** (all prioritized actions; bench under `docs/bench/core-*.md`)  
**Scope:** `src/main.rs`, `src/engine.rs`, `src/config.rs`, `src/usage.rs`, `src/ipc.rs`  
**Skipped (already analyzed):** `src/providers/**`, `src/ui/**`  
**Deprioritized:** `src/theme/**` (not critical for this pass)

**Goal:** find inefficiencies, bugs, dead code, and optimizations so Blink stays **lightweight and efficient** — correct config/usage I/O, cheap empty-state and search merge, reliable daemon toggle — without dropping features.

Related:

- Tracker: [`OPTIMIZATION.md`](../OPTIMIZATION.md)
- Providers analysis: [`providers-depth-analysis.md`](./providers-depth-analysis.md)
- UI analysis: [`ui-depth-analysis.md`](./ui-depth-analysis.md)
- Battery / wakeups: [`battery-optimization.md`](./battery-optimization.md)
- Perf baselines: [`performance.md`](./performance.md)

---

## TL;DR

| Area | Health | Biggest issue |
|------|--------|----------------|
| **Engine merge** | Good | Duplicated `force_files` logic; app `resolve_id` does full fuzzy search; allocs on dedup |
| **Config store** | OK structure | `update` always pretty-saves; default excludes force-merged on every load |
| **Usage / frecency** | Simple & useful | Sync pretty-write every launch; unbounded map; `top` sorts all entries |
| **IPC / daemon** | Good idea | Bind fail is silent; pending toggle no-op while window not ready |
| **main CLI** | Useful | `--bench` ~half of `main.rs` always in release binary; headless still starts periodic threads |
| **Mounts / paths** | Works | EFI/`/boot` skip bug; `pretty_path` / `is_excluded` hot-path cost |

**Top fixes if implementing next:**

1. **Stop re-injecting default excludes on every config load** (user deletes don’t stick).  
2. **`ConfigStore::update` → save only when changed** (auto-promote / no-op settings thrash disk).  
3. **Usage debounce + compact JSON + entry cap** (idle I/O + long-term RAM).  
4. **Direct `apps.resolve_id` + shared `is_force_files_query`** (empty-state + correctness).  
5. **Headless engine / gate bench binary + IPC pending-toggle** (CLI weight + reliability).

Architecture is already sound for a launcher: Arc-swapped config, deferred network, DeepMode skip on UI path, calc/translate short-circuit, release LTO/strip/abort.

---

## 1. Map of core modules

| Path | ~LOC | Role |
|------|-----:|------|
| `src/main.rs` | 638 | CLI (`--daemon`, `--search`, `--bench`), GTK app, IPC → toggle |
| `src/engine.rs` | 599 | Provider merge, rank, execute, deep-root promote, empty state |
| `src/config.rs` | 779 | BlinkConfig, Arc store, mounts, pretty paths, excludes, open-with, UI/translate knobs |
| `src/usage.rs` | 125 | Frecency map + `usage.json` |
| `src/ipc.rs` | 63 | Unix socket toggle (`$XDG_RUNTIME_DIR/blink.sock`) |
| **Total core** | **~2.2k** | |

```
src/
  main.rs      # entry, daemon hold, bench/search tools
  engine.rs    # search merge + execute
  config.rs    # config + mounts + path display + exclude API
  usage.rs     # frecency
  ipc.rs       # single-socket toggle
  providers/   # (out of scope — see providers-depth-analysis.md)
  ui/          # (out of scope — see ui-depth-analysis.md)
  theme/       # deprioritized this pass
```

---

## 2. Severity legend

| Level | Meaning |
|-------|---------|
| **P0** | Correctness / silent data loss / product bugs |
| **P1** | Idle battery, disk I/O, measurable search/open cost |
| **P2** | Dead code, structure, docs drift, micro-opts |
| **P3** | Nice-to-haves for long-term lightness |

---

## 3. P0 — Bugs / correctness

### 3.1 Config excludes you delete get re-added on every load ✅ **fixed 2026-07-17**

```text
// ConfigStore::load
for name in default_excludes() {
    if !cfg.index.exclude.iter().any(|e| e == &name) {
        cfg.index.exclude.push(name);
        changed = true;
    }
}
```

**Effect:** Settings “remove exclude” is temporary. Next daemon start re-injects the full default list and rewrites config.

**Fix:** One-shot migration (config `version` bump / `excludes_seeded` flag), or only seed defaults when the config file is **missing** — never force-merge on every load.

---

### 3.2 `ConfigStore::update` always clones + pretty-saves even on no-ops ✅ **fixed 2026-07-17**

```text
pub fn update<F: FnOnce(&mut BlinkConfig)>(&self, f: F) {
    // always: clone full config → f → sanitize → Arc::new
    self.save(); // always pretty-serialize + atomic write
}
```

Call sites like `promote_deep_root` / `remove_deep_root` may **do nothing** (already pinned / not found), but still:

- deep-clone full config  
- re-sanitize UI/translate  
- `serde_json::to_string_pretty`  
- write `.tmp` + rename  

Auto-promote on file open hits “already pinned” often → needless disk churn.

**Fix:** Compare before/after (or return `bool` from closure), only swap Arc + `save` when changed. Prefer compact JSON for runtime writes if human editability is secondary.

---

### 3.3 Mount EFI / `/boot` skip is wrong (dead branch) ✅ **fixed 2026-07-17**

```text
if target.contains("EFI") || target == "/boot" {
    if target.contains("EFI") {
        continue;
    }
}
```

`/boot` matches the outer condition but **never** `continue`s. Only EFI targets are skipped.

**Fix:**

```rust
if target.contains("EFI") || target == "/boot" || target.ends_with("/boot") {
    continue;
}
```

---

### 3.4 `force_files` is case-sensitive; settings match is not ✅ **fixed 2026-07-17**

In `Engine::search` and again in `should_deep_search`:

```text
q.starts_with("f ") || q.starts_with("file ") || q.starts_with("folder ") || ...
```

Settings uses `q.to_lowercase()` prefixes; `File foo` / `F doc` do **not** force files. The block is **duplicated** — easy to drift.

**Fix:** Single helper, e.g. `fn is_force_files_query(q: &str) -> bool`, using ASCII-lowercased first token / prefixes. Use it from both call sites (and keep in sync with translate’s path guards if needed).

---

### 3.5 IPC: bind failure is soft-fail; second process can become heavy ✅ **fixed 2026-07-17**

```text
// spawn_listener
UnixListener::bind → Err → eprintln + return
```

If an orphan socket / race exists: `request_toggle` fails → full GTK app path. GTK `application_id` may still single-instance via D-Bus, but IPC remains a sharp edge (stale socket, no readiness handshake, no peer credentials).

**Fix:** On bind fail: try connect+toggle; if connect fails, `remove_file` + rebind once. Optionally reply `ok` so clients don’t race. Clean socket on process exit.

---

### 3.6 Main: IPC toggle while window not built is a no-op ✅ **fixed 2026-07-17**

```text
// main IPC future
if let Some(launcher) = state.borrow().as_ref() {
    launcher.toggle();
} else {
    // comment: force activate — code does nothing
}
```

Early hotkeys after `blink --daemon` (before first activate builds the window) can be dropped.

**Fix:** `app.activate()` and/or a `pending_toggle` flag consumed on first `connect_activate`.

---

## 4. P1 — Efficiency (keep Blink light)

### 4.1 `UsageStore::record` is sync, pretty, unbounded ✅ **fixed 2026-07-17**

| Issue | Detail |
|-------|--------|
| Sync save | Pretty JSON write on **every** launch/open |
| Unbounded map | `entries` grows forever |
| `top(n)` | Sorts **all** entries every empty-query open |
| `boost` | `SystemTime` + `f64::exp` per result on every search |

**Fixes:**

1. Debounce save (1–2s, or flush on hide/exit).  
2. Compact JSON (`to_string` / `to_vec`).  
3. Cap map (e.g. keep top ~500 by frecency; drop cold tails).  
4. Cache `now_secs` once per search for all boosts.

---

### 4.2 `Engine::new` always spawns warm + 45‑minute periodic threads ✅ **fixed 2026-07-17** (CLI uses `new_headless`)

Even for `blink --search` / `blink --bench`:

- bg apps reload + index `ensure_fresh`  
- eternal `loop { sleep(45 * 60); apps.reload(); files.rebuild_index(); }`  

`rebuild_index` → `ensure_fresh` is good (TTL/fingerprint), but apps **always** rescan `.desktop` trees every interval.

**Fixes:**

- `Engine::new_headless()` without periodic thread for CLI, **or**  
- `start_background_jobs()` only from the daemon path in `main`.  
- Align docs: code is **45 min**; `FEATURES.md` still says **30 min**.

---

### 4.3 `resolve_id` for apps does a full fuzzy search first ✅ **fixed 2026-07-17**

```text
// Engine::resolve_id
apps.search(stem).find(|r| r.id == id)  // O(n) fuzzy
  .or_else(|| apps.resolve_id(id))
```

Empty-state frecency resolves up to ~20 IDs; each app id can fuzzy-scan the whole catalog before a linear exact resolve.

**Fix:** Call `self.apps.resolve_id(id)` only (`id` is exact `app:{stem}`).

---

### 4.4 Dedup / ranking allocations every keystroke ✅ **fixed 2026-07-17**

```text
seen.insert(r.id.clone());  // clone every id string
results.sort_by(... title.cmp ...);
results.truncate(25);
```

`should_deep_search` also builds a cloned `Vec<SearchResult>` of file/folder rows just to pass a slice.

**Fixes:**

- Dedup with `HashSet<&str>` (or retain-by-index) before truncate.  
- Pass filtered views/indices into files deep-gate without cloning rows.  
- Prefer `sort_unstable_by` / `sort_by_key` where order of equals doesn’t matter.

---

### 4.5 `is_excluded` is O(components × excludes × strings) per walk entry ✅ **fixed 2026-07-17** (`ExcludeSet`)

```text
pub fn is_excluded(path: &Path, excludes: &[String]) -> bool
```

Default exclude list is large (~40 names). Called heavily during index/deep walk and **overlaps** hard-coded `should_always_skip` names in the files index → double work.

**Fixes (API used by files):**

- Split excludes once per rebuild: `HashSet` of simple names vs path-substring patterns.  
- Component check = set lookup; keep substring patterns rare.

---

### 4.6 `pretty_path` calls `dirs::home_dir()` per result

Every file row subtitle can re-resolve home. Home rarely changes.

**Fix:** Cache home (and mounts already snapshotted in the file provider) for a search/index build — e.g. `OnceLock` or pass-in snapshot.

---

### 4.7 `discover_mounts` spawns `findmnt` + parses full JSON

Used on: config load, index refresh paths, settings UI. `ensure_fresh` already tries to avoid rediscovery when RAM is fresh — good.

Still: **config load always** runs mounts even when only UI/theme changed.

**Fix:** Lazy mount discovery; or process-local short TTL cache.

---

### 4.8 Auto-promote does up to 6×N `exists()` syscalls per file open

Walks up to 6 parents × project markers (`.git`, `Cargo.toml`, …), plus `canonicalize` in promote / forbidden checks.

**Fixes:** Only when depth suggests the global index wouldn’t cover the path; skip if parent already under a deep root; batch/stat smarter.

---

### 4.9 `copy_to_clipboard` blocks and always tries `wl-copy` then `xclip`

Fine for rare execute; still two process strategies. Prefer one from env (`WAYLAND_DISPLAY` → wl-copy only).

---

### 4.10 `main.rs` bench helpers bloat the release binary ✅ **fixed 2026-07-17** (`--features bench`)

~ half of `main.rs` is `--bench` / resource sampling (`ps`, `nvidia-smi`, `/proc`). Always linked into the daemon binary.

**Fix:** `#[cfg(feature = "bench")]` or a separate `blink-bench` bin.

Also **duplicate** cache size helpers:

| Helper | Path logic |
|--------|------------|
| `Engine::index_cache_bytes` | via `dirs::cache_dir()` (correct XDG) |
| `main::index_cache_bytes` | hardcodes `~/.cache/blink/...` |

Can disagree when `XDG_CACHE_HOME` is set — bench should only use the engine helper.

---

## 5. P2 — Dead code / duplication / hygiene

| Item | Notes |
|------|--------|
| `ConfigStore::get` | `#[allow(dead_code)]` — unused; hot paths already use `snapshot` / `with` |
| `Engine::translate_should_handle` | Dead; UI uses other translate helpers |
| `is_forbidden_deep_root` vs `is_overbroad_deep_root` | ✅ shared `config::is_forbidden_deep_root` (2026-07-17) |
| Docs drift | FEATURES refresh **45m** aligned; LOC map still slightly stale |
| Clippy (core) | Derive `Default` for `PathStyle`; HashMap `entry` for mounts; `sort_by_key` |
| Settings query | `"config"` is exact-only; `settings`/`preferences`/`index` are prefix — inconsistent UX |
| `once_cell` | Not used in core; providers use it (optional long-term `std::sync::LazyLock`) |

---

## 6. P3 — Structural (align with OPTIMIZATION.md)

1. **`config/` split:** `mod.rs` (store) + `mounts.rs` (`discover_mounts`, `pretty_path`, `is_excluded`) — planned as B2.  
2. **`main` split:** `cli_bench.rs` / `cli_search.rs` so daemon stays small to read and compile.  
3. **Shared query classification** used by engine (+ translate/files): force-files / path / scoped — kills duplication and case bugs.  
4. **Shared atomic write helper** for config + usage (tmp + rename), compact encoding, dirty flag.

---

## 7. What’s already good (don’t “optimize” away)

- **Arc-swapped config** on hot reads (`snapshot` / `with`)  
- **No network at boot** for FX (deferred until convert)  
- **DeepMode::Skip** on UI search path; async deep gated by `should_deep_search`  
- **Calc / translate short-circuit** before apps/files noise  
- **Strong path matches demote weak app fuzzy**  
- **Release profile:** LTO, `codegen-units = 1`, strip, `panic = "abort"`  
- **IPC fast path** before GTK when daemon is up  
- **Usage frecency model** is simple and effective (needs cap/debounce only)  
- **Deep-root guards** refuse `$HOME` / `/` / overbroad roots (recent regression fix)

---

## 8. Prioritized action list

| # | Change | Impact | Effort |
|---|--------|--------|--------|
| 1 | Stop force-merging default excludes on load | Correctness | **done** |
| 2 | `ConfigStore::update` save-only-if-changed | Disk/CPU idle | **done** |
| 3 | Fix EFI/`/boot` skip | Correctness | **done** |
| 4 | `resolve_id` → direct app resolve | Empty-state latency | **done** |
| 5 | Extract `is_force_files_query` + case-insensitivity | Correctness + DRY | **done** |
| 6 | Usage: debounce + compact + cap entries | I/O + RAM long-term | **done** |
| 7 | Headless engine without periodic threads | CLI weight | **done** |
| 8 | IPC pending-toggle + stale-socket recovery | Reliability | **done** |
| 9 | Gate `--bench` behind feature / separate bin | Binary size | **done** |
| 10 | `is_excluded` → HashSet name lookup | Index/deep speed | **done** |
| 11 | Dedup without `id.clone()`; avoid fileish clone | Search allocs | **done** |
| 12 | Docs: refresh interval + shared deep-root helper | Hygiene | **done** |

---

## 9. Suggested “lightweight” definition of done

After items **1–8** especially:

- [x] Removing an exclude **stays** removed after restart  *(v2 one-shot seed)*  
- [x] Opening an already-pinned project **does not** rewrite `config.json`  *(`update` PartialEq)*  
- [ ] Empty launcher open does **not** fuzzy-scan apps ~20×  
- [x] Usage file doesn’t grow without bound; saves are batched  *(cap 500 + 2s debounce)*  
- [x] Daemon idle: no surprise **config** writes from no-op updates  *(usage debounce still open)*  
- [x] `blink --search` doesn’t arm a 45‑minute background loop  *(`new_headless`)*  
- [x] Hotkey during early daemon start still toggles once UI is ready  *(`pending_toggle`)*  

**Measure:** `blink --bench` before/after for search medians (should stay flat or improve slightly). Watch RSS / disk writes under `inotifywait` on `~/.config/blink` and `~/.local/state/blink` during open/promote/settings.

---

## 10. Out of scope (pointer)

Largest remaining wins still live outside this pass:

| Area | Doc |
|------|-----|
| Files index lock during deep walk, live-cache clones | [`providers-depth-analysis.md`](./providers-depth-analysis.md) |
| ListBox full rebuild, settings built at boot | [`ui-depth-analysis.md`](./ui-depth-analysis.md) |
| Preview decode / thumbs | [`preview-optimization.md`](./preview-optimization.md) |

Core fixes above are still worth doing first: they touch **every** path (config, usage, empty state, daemon reliability) with small, low-risk patches.

---

## 11. Implementation notes (when coding)

- Prefer **mechanical** fixes; no behavior change beyond the listed bugs.  
- Each slice: compile + daemon restart + open empty query + open a pinned project twice (config mtime should not change second time).  
- Do not re-merge default excludes without an explicit migration story.  
- Keep `snapshot`/`with` as the only hot config APIs; don’t resurrect full `get()` clones on search paths.
