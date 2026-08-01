# Blink Codebase Audit Report

**Project:** `blink` v0.1.0 — Raycast-style launcher for Linux (Hyprland/Wayland)  
**Scope:** Entire repository (`src/`, packaging, CI, docs, config)  
**Date:** 2026-07-25  
**Method:** 15+ mandatory analysis passes with source verification  
**LOC:** ~22,467 lines of Rust under `src/` (binary crate, GTK4)  
**Tests executed:** `cargo test --features layer-shell` → **90 passed, 0 failed, 2 ignored**

---

## Executive Summary & Repository Health Score

Blink is a mature, performance-conscious launcher with a clear architecture: a resident daemon (`--daemon`), Unix-socket IPC toggle, multi-provider search (`apps` / `files` / `calc` / `translate`), and a GTK4 overlay UI with async deep search and network translation.

**Strengths**

- Thoughtful hot-path design: index-only UI search, async deep walks, live-cache, hot-paths, generation counters to drop stale workers.
- Solid domain coverage in calc (math, units, FX, timezone, battery, duration) with unit tests.
- Release profile is production-grade (`lto`, `codegen-units=1`, `strip`, `opt-level=3`).
- Config migrations, exclude defaults, deep-root forbids, and secret-ish filename skips show operational awareness.
- In-tree unit tests are numerous and currently green (90/90 non-ignored).

**Critical concerns**

1. **Shell injection on app launch** — `.desktop` `Exec=` is interpolated into `sh -c` without quoting.
2. **Sensitive directories indexed by design** — `.ssh` and `.gnupg` are explicitly *allowed* into the file index.
3. **Secrets on disk** — LibreTranslate `api_key` stored in plaintext `config.json` without restricted file mode.
4. **No PR CI** — only a release workflow; `clippy`/`fmt`/tests never gate merges.
5. **Concurrent index rebuild race** — `run_build` has no mutual exclusion.

### Health Score: **72 / 100**

| Area | Score | Notes |
|------|------:|-------|
| Architecture | 82 | Clear modules; some mega-files |
| Correctness / domain logic | 78 | Strong calc; a few numeric edges |
| Concurrency | 70 | Good UI gens; index rebuild race |
| Performance | 80 | Heavily optimized; residual allocs |
| Error handling | 68 | Many silent `let _ =`; poison `unwrap`s |
| Security | 55 | Shell injection + secret dirs + API key |
| Tests | 74 | Good unit surface; weak integration/CI |
| Config / CI / packaging | 65 | Release packaging OK; no test CI |
| Docs fidelity | 62 | Broken links; placeholders |
| Style / maintainability | 70 | Inconsistent lock style; huge modules |

---

## Pass Log (15+ mandatory passes)

| Pass | Focus | Outcome |
|-----:|-------|---------|
| 1 | Repository architecture & graph | Mapped modules, deps, orphans |
| 2 | Critical path & logic integrity | Engine search/execute traced |
| 3 | Numerical & domain edge cases | Math/FX/units/datetime audited |
| 4 | Concurrency, async & races | Index rebuild, workers, IPC |
| 5 | Resource management & leaks | Caches, tmp previews, threads |
| 6 | Performance & vectorization | Search/index hot paths |
| 7 | Error boundaries & silent failures | `unwrap`/`let _` inventory |
| 8 | Type safety & validation | Config sanitize, API boundaries |
| 9 | Security & vulnerabilities | Shell, secrets, network, index |
| 10 | Test suite coverage & assertions | 90 tests; gaps noted |
| 11 | Configuration, build & CI/CD | Cargo, release.yml, packaging |
| 12 | Documentation vs implementation | README/docs link & claim audit |
| 13 | Style, formatting & naming | rustfmt, mega-files, lock idioms |
| 14 | Deep stress / cascade bugs | Interaction failures |
| 15 | Final verification & synthesis | Re-checked line-level evidence |
| 16 | Adversarial re-check of “clean” areas | IPC, open_with, trash, preview |
| 17 | Dependency & supply-chain surface | Cargo.toml / ureq / GTK stack |

---

## Critical Bugs & Vulnerabilities

### C1. Shell injection via unquoted `Exec=` in `launch_app` — **CRITICAL**

**File:** `src/providers/apps.rs` lines 372–396  
**Root cause:** App launch builds a shell string and passes it to `sh -c` without quoting `exec` (from `.desktop` `Exec=` after field-code strip). Any metacharacters in a desktop entry become shell syntax.

```rust
// src/providers/apps.rs
pub fn launch_app(exec: &str, terminal: bool) {
    let shell_cmd = if terminal {
        let term = /* TERMINAL or fallback */;
        format!("{term} -e {exec}")   // neither term nor exec is shell-quoted
    } else {
        exec.to_string()
    };
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(format!(
            "setsid -f {shell_cmd} >/dev/null 2>&1 || nohup {shell_cmd} >/dev/null 2>&1 &"
        ));
    let _ = cmd.spawn();
}
```

**Impact:** A malicious or compromised `.desktop` file (user-local `~/.local/share/applications`, flatpak exports, etc.) can execute arbitrary commands when selected in Blink. This is the primary launch path (`Engine::execute` → `LaunchApp`).

**Contrast:** `open_terminal_at` / `spawn_detached` in `src/providers/files/mod.rs` correctly use `shell_quote`.

**Exact fix (preferred — avoid shell):**

```diff
--- a/src/providers/apps.rs
+++ b/src/providers/apps.rs
@@ pub fn launch_app(exec: &str, terminal: bool) {
-    let shell_cmd = if terminal {
-        let term = std::env::var("TERMINAL")
-            .ok()
-            .filter(|t| which(t).is_some())
-            .or_else(|| which("alacritty").map(|_| "alacritty".into()))
-            .or_else(|| which("kitty").map(|_| "kitty".into()))
-            .or_else(|| which("foot").map(|_| "foot".into()))
-            .unwrap_or_else(|| "xterm".into());
-        format!("{term} -e {exec}")
-    } else {
-        exec.to_string()
-    };
-
-    // Detach fully so the app survives after blink hides.
-    let mut cmd = Command::new("sh");
-    cmd.arg("-c")
-        .arg(format!(
-            "setsid -f {shell_cmd} >/dev/null 2>&1 || nohup {shell_cmd} >/dev/null 2>&1 &"
-        ))
-        .stdin(std::process::Stdio::null())
-        .stdout(std::process::Stdio::null())
-        .stderr(std::process::Stdio::null());
-    let _ = cmd.spawn();
+    // Parse Exec into argv; never pass through a shell.
+    let mut argv: Vec<String> = shell_words_split(exec); // or manual whitespace split + quote handling
+    if argv.is_empty() {
+        return;
+    }
+    let mut cmd = if terminal {
+        let term = resolve_terminal(); // known binary only
+        let mut c = Command::new(term);
+        c.arg("-e");
+        c.args(&argv);
+        c
+    } else {
+        let bin = argv.remove(0);
+        let mut c = Command::new(bin);
+        c.args(&argv);
+        c
+    };
+    cmd.stdin(Stdio::null())
+        .stdout(Stdio::null())
+        .stderr(Stdio::null());
+    // Prefer posix_spawn / pre_exec setsid if detach needed; avoid sh -c.
+    let _ = cmd.spawn();
 }
```

**Minimal mitigation if shell detach must stay:**

```diff
-        format!("{term} -e {exec}")
+        format!("{} -e {}", shell_quote_str(&term), shell_quote_str(exec))
...
-            exec.to_string()
+            // Still unsafe for multi-arg Exec; prefer argv form above.
+            shell_quote_str(exec)
```

Even quoted single-string form is wrong for multi-argument `Exec=` lines — **argv is the correct model**.

---

### C2. Private key material can enter the file index — **CRITICAL (privacy)**

**File:** `src/providers/files/index.rs` lines 415–422  

Dot-directories are skipped **except**:

```rust
if name.starts_with('.')
    && path.is_dir()
    && !matches!(name, ".config" | ".local" | ".ssh" | ".gnupg")
{
    return true;
}
```

**Impact:** `~/.ssh` and `~/.gnupg` are walked (within depth limits) and may surface `id_rsa`, `id_ed25519`, `*.key`, trust DBs, etc. in launcher results. Combined with Open / Copy Path / DnD, this increases secret exposure surface.

**Exact fix:**

```diff
--- a/src/providers/files/index.rs
+++ b/src/providers/files/index.rs
-            && !matches!(name, ".config" | ".local" | ".ssh" | ".gnupg")
+            // Never index crypto material dirs. Prefer explicit allowlist only.
+            && !matches!(name, ".config" | ".local")
```

Also add hard name skips:

```diff
+            || name == "id_rsa"
+            || name == "id_ed25519"
+            || name == "id_ecdsa"
+            || name.ends_with(".pem")
+            || name == "private-keys-v1.d"
```

---

### C3. Translate API key stored in plaintext config without mode `0600` — **HIGH**

**Files:**  
- `src/config.rs` — `TranslateConfig.api_key`, `save()` at ~617–627, `config_path()` ~630–634  
- `src/ui/settings.rs` ~2183+ writes key from UI  

`ConfigStore::save` writes pretty JSON via temp+rename but **never** sets file permissions. Default umask may leave `config.json` group/world-readable.

**Exact fix:**

```diff
--- a/src/config.rs
+++ b/src/config.rs
     pub fn save(&self) {
         if let Some(parent) = self.path.parent() {
             let _ = fs::create_dir_all(parent);
         }
         let snap = self.snapshot();
         if let Ok(data) = serde_json::to_string_pretty(snap.as_ref()) {
             let tmp = self.path.with_extension("json.tmp");
             if fs::write(&tmp, data).is_ok() {
+                #[cfg(unix)]
+                {
+                    use std::os::unix::fs::PermissionsExt;
+                    let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600));
+                }
                 let _ = fs::rename(tmp, &self.path);
+                #[cfg(unix)]
+                {
+                    use std::os::unix::fs::PermissionsExt;
+                    let _ = fs::set_permissions(&self.path, fs::Permissions::from_mode(0o600));
+                }
             }
         }
     }
```

Longer-term: store the key in a secret service (libsecret) or a separate `0600` credentials file outside the main config.

---

### C4. Concurrent `run_build` has no mutex — **HIGH (correctness/perf)**

**File:** `src/providers/files/index.rs` — `ensure_fresh`, `force_rebuild`, `run_build` (~86–145)

`indexing: AtomicBool` is informational only. Two threads (boot warm + Settings force reindex, or periodic + force) can both enter `run_build`, walk the filesystem twice, and race on:

```rust
save_cache(&items, &fingerprint);
*self.index.write().unwrap() = items;
```

**Impact:** CPU/disk thrash, torn cache writes (last writer wins), progress flicker, possible empty-index windows under abort-on-panic builds if a panic occurs mid-swap (less likely in safe code, still racy).

**Exact fix:**

```diff
--- a/src/providers/files/index.rs
+++ b/src/providers/files/index.rs
 pub struct IndexState {
     ...
     pub indexing: AtomicBool,
+    build_lock: Mutex<()>,
 }

     fn run_build(&self, fingerprint: String) {
+        let _guard = self.build_lock.lock().unwrap_or_else(|p| p.into_inner());
+        // Optional: if fingerprint already matches and not force, return.
         self.indexing.store(true, Ordering::Relaxed);
         ...
     }
```

Or use `compare_exchange` on `indexing` to reject overlapping builds and queue a single follow-up rebuild.

---

### C5. LibreTranslate endpoint is an open HTTP SSRF gadget — **MEDIUM–HIGH**

**File:** `src/providers/translate.rs` ~736–760

```rust
let url = format!("{}/translate", cfg.endpoint.trim_end_matches('/'));
...
crate::providers::http::post_json(&url, &payload)
```

User-controlled `endpoint` is not restricted to `https://`, host allowlists, or block of link-local/metadata IPs. A crafted config (or future remote settings import) can cause the daemon to POST query text + API key to arbitrary URLs.

**Fix sketch:**

```rust
fn validate_endpoint(url: &str) -> Result<(), String> {
    let u = url::Url::parse(url).map_err(|_| "invalid endpoint")?;
    if u.scheme() != "https" && u.scheme() != "http" { return Err(...); }
    // reject 127.0.0.0/8, 10/8, 169.254/16, metadata hosts unless debug flag
    Ok(())
}
```

Prefer requiring `https` in production defaults.

---

### C6. Poisoned `RwLock`/`Mutex` uses `unwrap` on hot paths + `panic = "abort"` — **MEDIUM**

**Files (examples):**  
- `src/providers/apps.rs` — `self.apps.read().unwrap()` (search path)  
- `src/providers/files/index.rs` — many `.unwrap()`  
- `src/providers/files/live_cache.rs` — `lock().unwrap()`  
- `src/providers/fx.rs` — `cache.read().unwrap()`  

**Contrast:** `ConfigStore` / `TypoStore` / `UsageStore` correctly use `unwrap_or_else(|p| p.into_inner())`.

With `panic = "abort"` in `[profile.release]` (`Cargo.toml`), any poisoned lock aborts the **entire daemon** instead of recovering.

**Fix:** Standardize on poison recovery for all long-lived locks, or use `parking_lot` (no poison).

---

### C7. Custom `TERMINAL` path ignored in `open_terminal_at` — **MEDIUM (logic)**

**File:** `src/providers/files/mod.rs` ~575–610

`TERMINAL` is resolved via `which_bin`, but `match term.as_str()` only special-cases bare names (`alacritty`, `kitty`, …). Values like `/usr/bin/alacritty` or unknown terminals fall through to the `xterm` branch — **not** the user’s `TERMINAL`.

```rust
_ => format!("xterm -e sh -c 'cd {} && exec $SHELL'", shell_quote(&dir)),
```

**Fix:** Default arm should invoke `term` with a documented working-directory flag, or use `Command::new(term).current_dir(dir)`.

---

### C8. FX conversion divides by rate without zero/NaN guard — **LOW–MEDIUM**

**File:** `src/providers/fx.rs` ~64–66

```rust
let out = (amount / from_rate) * to_rate;
```

Corrupt or partial disk cache could store `0.0` / non-finite rates → `inf`/`NaN` titles.

**Fix:**

```rust
rate_vs_base(cache, &from).and_then(|from_rate| {
    rate_vs_base(cache, &to).and_then(|to_rate| {
        if !from_rate.is_finite() || !to_rate.is_finite() || from_rate == 0.0 {
            return None;
        }
        let out = (amount / from_rate) * to_rate;
        if !out.is_finite() { return None; }
        Some((out, meta))
    })
})
```

---

### C9. Math modulo by zero is implicit (NaN path) — **LOW**

**File:** `src/providers/calc/expr.rs` ~131–153

`/` checks `right == 0.0`; `%` does not. IEEE `% 0.0` → NaN, then `eval_str` rejects non-finite — safe but inconsistent and untested.

```diff
-            '%' => left % right,
+            '%' => {
+                if right == 0.0 {
+                    return None;
+                }
+                left % right
+            }
```

Add test: `assert!(eval_str("5%0").is_none());`

---

### C10. Relative datetime `f64 as i64` millisecond cast — **LOW**

**File:** `src/providers/calc/datetime.rs` ~80–82

```rust
Duration::milliseconds((secs * 1000.0) as i64)
```

Huge inputs (e.g. `1e20 years`) saturate/overflow silently → wrong dates rather than rejection.

**Fix:** Clamp `secs` to a sane range (e.g. ±100 years) before cast.

---

### C11. IPC listener is best-effort only; no authentication beyond socket mode — **LOW (accepted risk)**

**File:** `src/ipc.rs`

- Socket mode `0600` is set (good).  
- Any local process running as the same user can toggle the overlay (expected for a launcher).  
- `request_toggle` treats write-success + read timeout as delivered (good for older daemons).  
- Fallback to `std::env::temp_dir()` when `XDG_RUNTIME_DIR` is missing can place the socket in a shared temp dir — **ensure `0600` still applied** (it is after bind). Prefer failing closed if runtime dir is absent on multi-user systems.

---

## Performance & Memory Bottlenecks

### P1. `search.rs` is a 3.1k-line allocation hotbed

**File:** `src/providers/files/search.rs` (3116 lines)

Observations:

- Repeated `results.iter().map(|r| r.id.clone()).collect::<HashSet<_>>()` when merging deep hits (lines ~126, 1145, 1241, 1272, 1353, 1417, …).
- Frequent `to_lowercase()` / `format!` on ranking paths.
- Deep merge clones full `SearchResult` vectors into live cache (mitigated somewhat by `Arc<[SearchResult]>` in `live_cache.rs` — good).

**Suggestions:**

1. Keep a `HashSet` of ids on the result builder instead of rebuilding per job.
2. Store precomputed `id` as `Arc<str>` in `SearchResult` if clone volume remains high after profiling.
3. Split `search.rs` into `plan.rs` / `deep.rs` / `glob.rs` / `rank.rs` for compiler + human locality (may help LLVM optimize less megafunctions).

**Benchmark hook already exists:** `blink --bench` with `--features bench` — re-run after changes; compare to `docs/bench/hot-path-*.txt`.

### P2. Live-cache key normalization is case-sensitive for prefixes

**File:** `src/providers/files/live_cache.rs` ~36–45

```rust
.strip_prefix("f ")
.or_else(|| raw.strip_prefix("file "))
.or_else(|| raw.strip_prefix("folder "))
```

Engine force-files prefixes are **ASCII case-insensitive** (`strip_force_files_prefix` in `engine.rs`). Cache keys diverge for `File foo` vs `file foo` → duplicate deep walks.

**Fix:** Reuse the same case-insensitive strip helper.

### P3. Live-cache LRU eviction is O(n) per insert when over cap

**File:** `src/providers/files/live_cache.rs` ~110–120

```rust
while map.len() > MAX_ENTRIES {
    let victim = map.iter().min_by_key(|(_, e)| e.last_used)...
}
```

Fine at small `MAX_ENTRIES`, but if raised, switch to an ordered structure.

### P4. Periodic refresh always calls `rebuild_index` / apps reload

**File:** `src/engine.rs` ~70–78

```rust
thread::spawn(move || loop {
    thread::sleep(Duration::from_secs(45 * 60));
    apps_periodic.reload();
    files_periodic.rebuild_index(); // ensure_fresh — OK if fingerprint stable
});
```

`ensure_fresh` short-circuits when fingerprint+TTL match (good). Still wakes the process every 45m — acceptable for a daemon; document battery impact.

**No shutdown signal:** thread lives until process exit — OK for daemon, but complicates tests if `Engine::new()` is constructed in-process.

### P5. Translate free-backend race leaves orphan worker threads

**File:** `src/providers/translate.rs` ~692–735

First successful backend returns while the other thread may still block up to `http::TOTAL` (4s). Threads are not joined/cancelled.

**Impact:** Under rapid translate queries, short-lived threads accumulate until timeouts complete. Bounded by debounce + `needs_network`, but still a cascade under paste storms.

**Fix:** `ureq` doesn’t cancel easily; use a shared `AtomicBool` cancel + ignore late results (partially done at UI gen layer), and avoid spawning a second backend until the first fails for low-end machines — or use a global semaphore (max 2 translate workers).

### P6. Preview `unsafe` pixbuf pixel copy

**File:** `src/ui/preview.rs` ~905–908

```rust
let pixels = unsafe {
    let slice = pixbuf.pixels();
    slice.to_vec()
};
```

Required by gtk-rs APIs; copy is correct if pixbuf is not mutated concurrently. Ensure decode stays on the worker and only the `Vec` crosses threads (appears true). No fix required beyond documenting the invariant.

### P7. Index cap 100k + deep roots depth 6

**File:** `src/providers/files/index.rs` — `MAX_INDEX = 100_000`

Large homes with deep pins can hit the cap silently (`capped` flag). UI surfaces this via `format_index_status` — good. Consider warning when deep_roots cause early cap.

---

## Architectural & Documentation Inconsistencies

### A1. Mega-modules hurt maintainability

`src/providers/files/search/` split into `mod.rs` (hub) + `glob.rs` / `plan.rs` / `deep.rs` / `rank.rs`. Largest remaining modules:

| File | LOC | Note |
|------|----:|------|
| `src/ui/mod.rs` | 2331 | Window, keys, debounce, deep/translate glue |
| `src/ui/settings.rs` | 2216 | Entire settings surface |
| `src/providers/translate.rs` | 1315 | Provider + HTTP backends + cache + tests |
| `src/ui/preview.rs` | 1112 | Media preview pipeline |

**Recommendation:** Extract pure functions (already partially done) into submodules without behavior change.

### A2. Binary-only crate (no `lib` target)

`cargo test --lib` fails with “no library targets”. All tests hang off `src/main.rs`. This blocks reusing `Engine` from integration harnesses / fuzz targets cleanly.

**Recommendation:**

```toml
[lib]
name = "blink"
path = "src/lib.rs"

[[bin]]
name = "blink"
path = "src/main.rs"
```

### A3. Documentation broken links

| Reference | Status |
|-----------|--------|
| `docs/README.md` → `../FEATURES.md` | **Missing** |
| `docs/README.md` → `../todo.md` | **Missing** |
| `docs/OPTIMIZATION.md` → `FEATURES.md` §14 | **Missing** |
| `Cargo.toml` / `README.md` `YOUR_GITHUB_USER` | Placeholder |
| `feature.txt` product wishlist | Not linked from docs; gitignored |

### A4. README feature table omits major shipped features

Implemented but under-documented in the main README table:

- Online translate (`tr `, auto script detect) — only in `docs/TRANSLATE.md`
- Action panel (`Ctrl+K`) — implemented (`src/ui/action_panel.rs`)
- Currency FX (Frankfurter/ECB cache)
- Battery/power queries
- Settings UI / open-with overrides / theme

### A5. Temporary tracker left in tree

`STYLE_GUIDE_REVIEW_TRACKER.md` (~55 KB) is marked temporary but lives at repo root. Either archive under `docs/archive/` or remove after actionable items are filed.

### A6. Orphan / redundant surfaces

- No dead Rust modules found; `#[allow(dead_code)]` is used sparingly for bench/API completeness.
- `feature.txt` is a product backlog scrap (gitignored) — not code debt.
- Dual install scripts: `scripts/install.sh`, `packaging/install-user.sh`, `dist/install.sh` — intentional packaging layers, but easy to confuse; document which is canonical.

### A7. `ExcludeSet` path patterns use substring match

**File:** `src/config.rs` ~858–861

```rust
if self.patterns.iter().any(|p| s.contains(p.as_str())) {
```

A pattern `foo/bar` excludes **any** path containing that substring, not only path components. Can over-exclude (e.g. `.../food/bartender/...` if pattern were `foo/bar` — edgey) and is not glob-aware.

Prefer component-boundary matching or `globset`.

---

## Error Boundaries & Silent Failures (Pass 7 detail)

| Pattern | Approx. count | Risk |
|---------|--------------:|------|
| `let _ =` | ~86 | Hides spawn/IO failures (launch, clipboard, chmod) |
| `unwrap`/`expect` | ~174 (incl. tests) | Hot-path locks; production panic=abort |
| Corrupt config JSON | silent default | User settings vanish without warning |
| Failed `xdg-open` / `gio trash` | partial | Trash surfaces `Failed`; open often silent |

**Recommendations:**

1. Log launch failures at `eprintln!` or a small ring buffer shown in footer.
2. On config parse failure, back up the bad file to `config.json.invalid` and notify once.
3. Align lock poison policy (C6).

---

## Type Safety & Validation (Pass 8 detail)

**Good**

- `TranslateConfig::sanitize` clamps `max_chars`, filters lang tags.
- `UiThemeConfig` sanitization on update.
- Forbidden deep roots shared between config load and engine promote.
- Strong enums: `ResultKind`, `Action`, `DeepMode`, `PathStyle`.

**Gaps**

- `endpoint` URL not schema-validated (C5).
- `OpenWithConfig` desktop ids not verified until launch time.
- `Exec` strings treated as opaque shell (C1) — type system cannot save this; needs argv parsing.

---

## Test Suite Coverage & Assertion Quality (Pass 10)

**Inventory:** 22 `#[cfg(test)]` modules, **92** tests declared, **90** run, **2** ignored (IPC bind).

**Well covered**

- Expression evaluator, duration ranges, timezone parsing  
- File glob/scope/deep gates, live cache, hot paths  
- Translate detection/parsing (offline)  
- Config migration / exclude set  
- Typo learning / usage debounce  

**Gaps / weak spots**

| Gap | Why it matters |
|-----|----------------|
| No `Engine::search` integration tests | Ranking interactions (calc vs apps vs files vs translate) untested |
| No test for `launch_app` argv safety | C1 unguarded |
| No test that `.ssh` is excluded | C2 unguarded |
| IPC tests `#[ignore]` | No CI signal for socket reclaim |
| No UI / GTK tests | Keybindings, debounce, gen cancellation |
| No network mock tests for FX/translate HTTP | Backend parse tests exist for MyMemory only |
| No `clippy -D warnings` in CI | Style/correctness regressions |

**Assertion quality:** Generally behavior-based (good). Some tests use temp dirs with PID — good hygiene. Prefer property tests for `eval_str` edge cases (`%0`, `0**0`, huge factorial already handled).

---

## Configuration, Build & CI/CD (Pass 11)

| Item | Status |
|------|--------|
| `Cargo.toml` release profile | Excellent |
| Optional `layer-shell`, `bench` features | Good |
| `.github/workflows/release.yml` | Builds packages on tags / dispatch |
| PR workflow (test/fmt/clippy) | **Missing** |
| `rustfmt.toml` | Present |
| `cargo-deb` metadata | Present |
| Dependency pins | `Cargo.lock` committed — good |
| `ureq` HTTP | Process-free workers — good vs curl spawn |
| `panic = "abort"` | Smaller binary; harsher failure mode with unwrap |

**Recommended CI job (sketch):**

```yaml
# .github/workflows/ci.yml
on: [push, pull_request]
jobs:
  test:
    runs-on: ubuntu-22.04
    steps:
      - uses: actions/checkout@v4
      - run: sudo apt-get install -y libgtk-4-dev ... # same as release
      - uses: dtolnay/rust-toolchain@stable
        with: { components: rustfmt, clippy }
      - run: cargo fmt --check
      - run: cargo clippy --features layer-shell -- -D warnings
      - run: cargo test --features layer-shell
```

---

## Style, Formatting & Naming (Pass 13)

- Edition 2021, rustfmt `max_width = 100` — consistent.
- Naming is generally clear (`DeepMode`, `SearchResult`, `force_files`).
- Inconsistent lock idioms (`unwrap` vs `unwrap_or_else` poison recovery) — see C6.
- `once_cell::Lazy` used for regexes — fine; could migrate to `std::sync::LazyLock` on newer MSRV.
- Comments are high quality on hot paths (battery/index notes).

---

## Cascade / Interaction Bugs (Pass 14)

| Interaction | Failure mode |
|-------------|--------------|
| Malicious `.desktop` + Enter | Shell injection (C1) |
| Index includes `~/.ssh` + DnD/copy | Secret exfil via normal UX (C2) |
| Poisoned lock + `panic=abort` | Full daemon death mid-keystroke (C6) |
| Concurrent reindex + search | Readers see swapping index; possible empty results blip (C4) |
| Translate race threads + rapid paste | Thread pile-up until 4s timeouts (P5) |
| Stale live cache after external delete | Partially mitigated for trash; not for external `rm` |
| `force_translate` suppresses apps/files | Correct by design; may surprise if auto-detect misfires on mixed strings |
| Corrupt FX cache + convert | Inf/NaN money strings (C8) |
| Config parse fail + silent default | User loses settings and API key without notice |

---

## Final Verification Notes (Pass 15–17)

Re-verified against source (not memory):

- [x] `launch_app` still unquoted (`apps.rs` 372–396)  
- [x] `.ssh`/`.gnupg` allowlist (`index.rs` 420)  
- [x] `api_key` in config JSON, save without mode bits  
- [x] `run_build` without mutex  
- [x] Tests: 90 pass / 2 ignored  
- [x] Missing `FEATURES.md` / `todo.md`  
- [x] Placeholder GitHub user strings  
- [x] Only `release.yml` under `.github/workflows/`  
- [x] `shell_quote` used in files open/terminal but not apps launch  
- [x] No hardcoded cloud API secrets in repo (FX/translate use public endpoints)  
- [x] Google gtx / MyMemory send user text off-machine by design — privacy disclosure needed in UI/README  

**False positives filtered**

- Modulo-by-zero does **not** panic (NaN rejected) — downgraded to consistency issue.  
- `unsafe` in preview is scoped pixel copy — not memory unsafety by itself.  
- IPC “no auth” is normal for same-user launcher sockets when mode is `0600`.

---

## Prioritized Remediation Roadmap

### Immediate (security / data loss) — target: days

1. **Rewrite `launch_app` to argv / GIO launch** — eliminate `sh -c` for Exec (C1).  
2. **Stop indexing `.ssh` / `.gnupg`** and common private key filenames (C2).  
3. **`chmod 0600` config (and any key file)** on save (C3).  
4. **Serialize index rebuilds** with a mutex or single-flight flag (C4).  
5. **Add CI** `fmt` + `clippy -D warnings` + `cargo test` on PR (Pass 11).

### Short-term (correctness / hygiene) — target: 1–2 weeks

6. Validate translate endpoint scheme/host (C5).  
7. Standardize lock poison handling (C6).  
8. Fix custom `TERMINAL` handling (C7).  
9. FX rate finite checks (C8); math `% 0` explicit reject (C9).  
10. Config parse failure backup + user-visible warning.  
11. Case-insensitive live-cache keys (P2).  
12. Fix docs links; replace `YOUR_GITHUB_USER`; document translate + Ctrl+K in README.  
13. Unit tests for C1/C2 regressions.

### Refactoring phase — target: next milestone

14. Split `search.rs` / `ui/mod.rs` / `settings.rs`.  
15. Extract `lib.rs` for testability and fuzzing.  
16. Global limit on translate/FX worker threads; optional backend preference.  
17. Consider libsecret for API keys.  
18. Integration tests for `Engine::search` ranking matrix.  
19. Archive or delete `STYLE_GUIDE_REVIEW_TRACKER.md`.  
20. Product items in `feature.txt` (default currency by locale, richer app actions) — track in real issues, not only a local file.

---

## Suggested Ownership Checklist

| ID | Severity | Effort | Owner hint |
|----|----------|--------|------------|
| C1 | Critical | M | apps / execute path |
| C2 | Critical | S | files index |
| C3 | High | S | config |
| C4 | High | S | files index |
| C5 | Medium | S | translate |
| C6 | Medium | M | cross-cutting |
| CI | High (process) | S | repo tooling |
| Docs | Low | S | docs |

---

## Appendix A — Module Graph (Pass 1)

```
main.rs
├── ipc          (Unix socket toggle)
├── engine       (search orchestration, execute, typos/usage)
│   ├── providers::apps
│   ├── providers::files::{index, search, hot, live_cache}
│   ├── providers::calc::{expr, math, units, currency→fx, timezone, ...}
│   ├── providers::translate → http
│   ├── config
│   ├── usage
│   └── typos
├── ui::{mod, rows, preview, settings, action_panel, dnd, ...}
│   └── theme::{mod, css}
└── bench (feature = "bench")
```

**External process touchpoints:** `sh`/`setsid`, `xdg-open`, `gio trash`, `wl-copy`/`xclip`, `findmnt`, `ffmpeg`, `pdftoppm`, optional terminals.

## Appendix B — Test command used

```bash
cargo test --features layer-shell
# 90 passed; 0 failed; 2 ignored
```

## Appendix C — Pass completeness affirmation

This audit executed **17 distinct passes** (15 required + 2 adversarial/supply-chain). Findings were re-checked against the current tree under `/home/vedant/blink` on 2026-07-25. The task is complete only with this report written to `AUDIT_REPORT.md` at the repository root.

---

## Remediation Status Tracker

Track progress against audit findings. Update the **Status** column as work lands.

**Legend:** `Done` · `Open` · `Accepted` (won't fix / intentional) · `Partial`

| ID | Area | Severity | Status | Notes |
|----|------|----------|--------|-------|
| C1 | Shell injection in `launch_app` | Critical | Done | Argv + `setsid -f`; no `sh -c` for Exec |
| C2 | `.ssh` / `.gnupg` / key files indexed | Critical | Done | Removed allowlist; hard-skip key names |
| C3 | Config API key file mode | High | Done | `config.json` saved as `0600` |
| C4 | Concurrent `run_build` race | High | Done | `build_lock` + single-flight skip |
| C5 | Translate endpoint SSRF | Med–High | Done | Scheme/host validation; metadata blocked |
| C6 | Poisoned lock `unwrap` + abort | Medium | Done | Poison recovery on provider hot paths |
| C7 | Custom `TERMINAL` path ignored | Medium | Done | Basename match + argv spawn; unknown terms use cwd |
| C8 | FX zero/NaN rate division | Low–Med | Done | `convert_amount` rejects zero/NaN/inf |
| C9 | Math `% 0` implicit NaN | Low | Done | `%` rejects zero divisor like `/` |
| C10 | Datetime `f64 as i64` overflow | Low | Done | Relative deltas clamped to ±100 y; test added |
| C11 | IPC same-user only | Low | Done | No shared `/tmp` fallback; user-private cache dir `0700` |
| P1 | `search.rs` size / allocs | Perf | Done | One id HashSet hoisted across deep jobs/merges; merge rank precomputes lowercase key |
| P2 | Live-cache prefix case | Perf | Done | Shared `strip_force_files_prefix`; case-insensitive cache keys |
| P3 | Live-cache LRU O(n) | Perf | Done | Recency `BTreeMap`; O(log n) evict; LRU + reinsert tests |
| P4 | Periodic 45m refresh wake | Perf | Done | `recv_timeout` wake; fingerprint short-circuit documented; stop signal + `Drop` join |
| P5 | Translate orphan workers | Perf | Open | Semaphore or cancel flag |
| P6 | Preview unsafe pixbuf copy | Perf | Accepted | Required by gtk-rs; document invariant |
| P7 | Index 100k cap + deep roots | Perf | Done | Surface warning when capped early |
| A1 | Mega-modules | Arch | Done | Split search into glob / plan / deep / rank |
| A2 | Binary-only crate | Arch | Open | Extract `lib.rs` |
| A3 | Broken docs links | Docs | Open | FEATURES.md / todo / placeholders |
| A4 | README missing features | Docs | Open | Translate, Ctrl+K, FX, settings |
| A5 | Style tracker in tree | Docs | Open | Archive or delete tracker file |
| A6 | Install script confusion | Docs | Open | Document canonical install path |
| A7 | ExcludeSet substring patterns | Arch | Open | Component-boundary or glob match |
| CI | No PR test/fmt/clippy gate | Process | Open | Add `.github/workflows/ci.yml` |
| E1 | Config parse failure silent | Correctness | Open | Backup invalid JSON + notify |
| E2 | Silent launch/IO `let _ =` | Correctness | Open | Surface critical spawn failures |
| T1 | Engine integration tests | Tests | Open | Ranking matrix across providers |
| T2 | Network mock FX/translate | Tests | Open | HTTP parse paths beyond MyMemory |

### Summary

| Status | Count |
|--------|------:|
| Done | 17 |
| Open | 11 |
| Accepted | 2 |
| **Total tracked** | **30** |

*Last updated: 2026-07-26 (C1–C9 fixed; P1–P2 fixed). C10–C11 fixed 2026-08-01. P3–P4 fixed 2026-08-01. P7 + A1 (search split) fixed 2026-08-01.*

---

*End of audit report.*
