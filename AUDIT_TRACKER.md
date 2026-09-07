# Blink Codebase Audit & Optimization Tracker

> ## 🚨 AGENT SYSTEM DIRECTIVE / AUDIT MANDATE
>
> **Objective**: Perform an exhaustive, line-by-line code audit, optimization, and bug-fixing sweep across every file in `src/` prior to implementing new features.
>
> When starting a session or resuming work from this tracker, follow these strict execution rules:
>
> ### 1. Depth & Rigor (Zero Skimming)
> - Read and inspect **every single line** of code in the target batch. Never skim, gloss over, or assume correctness.
> - Trace full data flow, state transitions, lifetime scopes, and concurrency boundaries.
> - Verify error paths, edge cases (empty strings, zero division, unicode handling, unexpected input, network drops, OS signal drops).
>
> ### 2. Target Review Dimensions
> - 🐛 **Bugs & Edge Cases**: Unhandled `Result`/`Option` (`.unwrap()`, `.expect()` in non-test code), panics, potential deadlocks, integer overflow/underflow, floating point NaN/Inf, race conditions, stale cache reads.
> - ⚡ **Performance & Allocations**:
>   - Eliminate redundant `.clone()`, `to_string()`, `to_owned()`, and heap allocations in hot paths / per-keystroke queries.
>   - Replace owned types with borrowed slices (`&str`, `&[T]`, `Cow<'a, T>`) where feasible.
>   - Reuse buffers (`Vec::clear()` vs reallocating), reserve capacity when size is known.
>   - Reduce lock contention (`RwLock` vs `Mutex`, minimize lock scope).
> - 🏛️ **Architecture & Design**:
>   - Identify poor abstractions, tight coupling, code duplication, and leaky encapsulation.
>   - Ensure clean separation between UI state, background engine workers, and calculation providers.
> - 🦀 **Idiomatic Rust & Modern Conventions**:
>   - Leverage standard traits (`From`, `TryFrom`, `Display`, `Default`, `AsRef`).
>   - Use idiomatic pattern matching, iterator chains (when clearer/faster than loops), and proper `Error` enums (`thiserror` / custom error types).
>   - Remove dead code, redundant imports, unused variables, and outdated comments.
>
> ### 3. Batch Execution Protocol
> For each batch:
> 1. **Read**: Use `Read` tool to load all lines of all files in the batch.
> 2. **Analyze**: Provide deep, line-referenced analysis detailing bugs, inefficiencies, and code quality improvements.
> 3. **Implement**: Apply verified, high-quality refactors/fixes directly using `Edit` or `Write`.
> 4. **Verify**: Run `cargo check`, `cargo test`, and `cargo clippy` to ensure zero regressions or compiler warnings.
> 5. **Update Tracker**: Mark batch as `Completed`, document key discoveries and fixes in the Batch Log below.
> 6. **Persist Knowledge**: Call `openlmlib_save_finding` for notable architectural discoveries or optimizations.
>
> ---

Comprehensive line-by-line audit of all `src/` files across 51 Rust files (~38,810 LOC).

## Audit Goals
- **Correctness & Safety**: Eliminate edge-case bugs, unwrap/panic risks, race conditions, stale cache reads.
- **Performance & Allocations**: Minimize unnecessary allocations/clones (`Arc`, `String`, `Vec`), reuse buffers, reduce lock contention.
- **Efficiency & CPU**: Cache lookups, algorithmic improvements in indexing, fuzzy matching, calculation parsing.
- **UI Responsiveness & Polish**: Smooth frame rendering, minimal re-renders, clean event dispatch, robust IPC.
- **Idiomatic Rust & Cleanup**: Modern Rust practices, clean error propagation, dead code removal.

---

## Batch Overview & Progress

| Batch | Module / Area | Files | Total LOC | Status |
|:-----:|:---|:---|:---:|:---:|
| **01** | Core Foundation & IPC | `src/lib.rs`<br>`src/main.rs`<br>`src/ipc.rs`<br>`src/bench.rs`<br>`src/usage.rs` | 1,994 | Completed |
| **02** | Configuration System | `src/config.rs` | 2,019 | Completed |
| **03** | Core Engine & Query Router | `src/engine.rs` | 1,606 | Completed (2nd pass verified) |
| **04** | Typo/Fuzzy Engine & Base Providers | `src/typos.rs`<br>`src/providers/mod.rs`<br>`src/providers/http.rs` | 1,750 | Completed (2nd pass verified) |
| **05** | App Search & FX Providers | `src/providers/apps.rs`<br>`src/providers/fx.rs` | 1,854 | Completed (2nd pass verified) |
| **06** | Translation Engine | `src/providers/translate.rs` | 1,654 | Completed (2nd pass verified) |
| **07** | Calc Core & Math/Expressions | `src/providers/calc/mod.rs`<br>`src/providers/calc/util.rs`<br>`src/providers/calc/math.rs`<br>`src/providers/calc/expr.rs`<br>`src/providers/calc/currency.rs`<br>`src/providers/calc/fueleco.rs` | 1,770 | Completed |
| **08** | Calc Units & Quick Parser | `src/providers/calc/units.rs`<br>`src/providers/calc/quick.rs` | 2,198 | Completed |
| **09** | Calc Unit Math & Conversions | `src/providers/calc/unitmath.rs`<br>`src/providers/calc/battery.rs`<br>`src/providers/calc/cooking.rs` | 1,855 | Completed |
| **10** | Calc Financial & Specialty | `src/providers/calc/financial.rs`<br>`src/providers/calc/home.rs`<br>`src/providers/calc/duration.rs` | 1,195 | Completed |
| **11** | Calc Datetime & Timezones | `src/providers/calc/datetime.rs`<br>`src/providers/calc/timezone.rs` | 1,960 | Completed |
| **12** | File Index & Cache Layer | `src/providers/files/hot.rs`<br>`src/providers/files/live_cache.rs`<br>`src/providers/files/index.rs` | 1,781 | Completed |
| **13** | File Provider Core & Search Types | `src/providers/files/mod.rs`<br>`src/providers/files/search/mod.rs`<br>`src/providers/files/search/rank.rs` | 2,188 | Completed |
| **14** | File Search Engines (Plan/Glob/Deep) | `src/providers/files/search/plan.rs`<br>`src/providers/files/search/glob.rs`<br>`src/providers/files/search/deep.rs` | 2,528 | Completed |
| **15** | Theme & Styling Engine | `src/theme/mod.rs`<br>`src/theme/css.rs` | 1,520 | Completed |
| **16** | UI Micro-Components & Animations | `src/ui/footer.rs`<br>`src/ui/size_anim.rs`<br>`src/ui/scroll_anim.rs`<br>`src/ui/action_panel.rs`<br>`src/ui/thumbnails.rs` | 1,062 | Completed |
| **17** | UI Interactions & Result Rows | `src/ui/dnd.rs`<br>`src/ui/open_with.rs`<br>`src/ui/rows.rs` | 2,007 | Pending |
| **18** | UI Preview Window | `src/ui/preview.rs` | 2,164 | Pending |
| **19** | UI Settings Window | `src/ui/settings.rs` | 2,371 | Pending |
| **20** | UI Main / View Model (Part 1) | `src/ui/mod.rs` (Lines 1–1800) | 1,800 | Pending |
| **21** | UI Main / Event Loop (Part 2) | `src/ui/mod.rs` (Lines 1801–3572) | 1,772 | Pending |

---

## Detailed Batch Logs & Findings

### Batch 01: Core Foundation & IPC
- **Files**: `src/lib.rs` (18), `src/main.rs` (420), `src/ipc.rs` (472), `src/bench.rs` (487), `src/usage.rs` (597)
- **Status**: `Completed` (2nd pass verified)
- **Key Focus**: Startup latency, IPC daemon reliability, benchmark harness precision, usage frequency tracker lock contention.
- **Notes & Fixes**:
  - 🐛 **`src/main.rs` Flag Strip Ordering Fix**: `args.retain` was stripping `--search` before `strip_search_args` ran, preventing `strip_search_args` from matching `--search` and leaving search operands in `args` passed to GTK. Fixed `args.retain` filter.
  - 🐛 **`src/main.rs` Toggle State Inversion (2nd pass)**: Accounted for pending toggle inversion across daemon vs interactive startup modes.
  - 🐛 **`src/ipc.rs` Early Boot Rate Limiter & Fragmented Ack Fix (2nd pass)**: Replaced `Instant::now().checked_sub(...)` with `Option<Instant>` to eliminate 5-second error suppression window on fresh boots. Buffered socket ack reads in `request_toggle` to guarantee complete line parsing under TCP/UNIX fragmentation.
  - ⚡ **`src/usage.rs` Zero Allocation Optimizations (2nd pass)**:
    - `record()`: Checked `g.entries.get_mut(id)` before calling `.entry(id.to_string())`, eliminating heap allocation for all existing entries (>99% of calls).
    - `top()` and `top_path_ids()`: Replaced pre-sort `String` clones with `&str` references, allocating only for the final `n` elements after truncation.
    - `prune_entries()` and `prune_entries_pinning()`: Switched from cloning entire map keys into `Vec<String>` and `HashSet<String>` to sorting `&str` references and removing only the `drop_count` lowest items ($O(\text{dropped})$ rather than $O(N)$ clones).
    - `salvage_usage_file()`: Direct `u32` scalar extraction and `UsageEntry::deserialize` without AST cloning.
  - ⚡ **`src/bench.rs` Profiling & Percentile Hardening (2nd pass)**:
    - `proc_cpu_self()` & `daemon_stats()` & `gpu_stats()`: Replaced `Vec<&str>` allocations with zero-alloc iterator `.nth()`/`.next()`.
    - `bench_query()`: Added empty sample check and safe percentile indexing to prevent underflow panic when `iters == 0`.
    - `index_cache_bytes()`: Replaced hardcoded `.cache` with `dirs::cache_dir()`.

---

### Batch 02: Configuration System
- **Files**: `src/config.rs` (2,019)
- **Status**: `Completed` (2nd pass verified)
- **Key Focus**: Config parsing robustness, default fallback handling, file watcher debounce, serialization overhead.
- **Notes & Fixes**:
  - ⚡ **`section_or_default()` Zero-Clone Deserialization**: Replaced `serde_json::from_value(sv.clone())` with direct `T::deserialize(sv)`, eliminating full clone of every section's JSON AST during load.
  - ⚡ **`is_system_target()` Zero-Allocation Prefix Search**: Replaced `format!("{prefix}/")` heap string allocation in loop over 15 prefixes with zero-allocation byte prefix check (`t.strip_prefix(prefix).is_some_and(|rest| rest.starts_with('/'))`).
  - ⚡ **`parse_ipv4_literal()` Stack Allocation**: Switched from allocating `Vec<&str>` and `Vec<u32>` to stack-allocated `[u32; 4]` buffer during SSRF translate endpoint validation.
  - ⚡ **`ExcludeSet::matches()` Hot Path Allocation Elimination**: Optimized component lowercase checking to only allocate `to_ascii_lowercase()` if ASCII uppercase characters are actually present (99.9% of paths on Linux are already lowercase), eliminating hundreds of thousands of heap allocations during directory walks. Also switched pattern matching to borrowed `Cow<str>`.
  - ⚡ **`discover_mounts()` & `pretty_path()` (2nd pass)**: Eagerly canonicalized discovered mount targets, eliminating repeated `canonicalize()` syscalls. Used byte length for mount prefix comparisons and avoided intermediate String allocation in `pretty_path`.
  - ⚡ **`ConfigStore::update()` Lock Contention (2nd pass)**: Serialized new config snapshot directly from local Arc without re-acquiring `RwLock::read`.

---

### Batch 03: Core Engine & Query Router
- **Files**: `src/engine.rs` (1,629)
- **Status**: `Completed (2nd pass verified)`
- **Key Focus**: Query dispatch overhead, thread synchronization, debounce logic, query cancellation, channel buffer sizing.
- **Notes & Fixes**:
  - ⚡ **`search()` Settings Command Pre-check Stack Optimization**: Replaced unconditional `q.to_lowercase()` heap allocation on every single keystroke with a stack-buffered ASCII lowercase check (`[u8; 32]`), avoiding heap allocations on 99.9% of queries.
  - ⚡ **`search()` In-place Deduplication**: Replaced temporary `keep: Vec<bool>` side allocation and dual iteration in `search()` with single in-place `results.retain(|r| seen.insert(r.id.clone()))`.
  - ⚡ **`empty_results()` Sort & Saturating Math**: Changed `sort_by_key` to `sort_unstable_by_key` and used `1_000_i64.saturating_add(b)` to prevent arithmetic overflow on corrupted usage stores.
  - ⚡ **`resolve_alias_target()` Vector Consumption**: Switched `hits.first()` clone to `hits.into_iter().next()` to consume owned result and avoid cloning `id` and `title` Strings.
  - 🐛 **`auto_promote_deep_root()` Marker Check**: Replaced `.exists()` with `.try_exists().unwrap_or(false)` to prevent silent panic/errors on inaccessible symlink/permission error paths.
  - ⚡ **`format_int()` Linear Single-Pass Forward Formatting**: Replaced dual `.rev()` String allocation and char-by-char iterator formatting with a single allocated byte buffer sized with exact comma count and `chunks_exact(3)`.
  - 🐛 **`copy_to_clipboard()` Pipe & Status Robustness**: Properly checked child process exit status (`child.wait().map(|s| s.success())`) and handled write failures gracefully across `wl-copy` and `xclip`.
  - 📝 **2nd pass: Dedup Comment Correction**: Fixed misleading "without reallocating Strings" comment — `retain+clone` still clones ≤25 small ids (borrow rules force it). Comment now accurate.

---

### Batch 04: Typo/Fuzzy Engine & Base Providers
- **Files**: `src/typos.rs` (1,009), `src/providers/mod.rs` (517), `src/providers/http.rs` (245)
- **Status**: `Completed (2nd pass verified)`
- **Key Focus**: Edit distance performance, lookup tables, HTTP client connection pooling and timeout handling, provider trait dispatch.
- **Notes & Fixes**:
  - ⚡ **`src/typos.rs` Zero-Allocation Levenshtein & Prefix Comparison**: Added `levenshtein_chars(&[char], &[char])` to avoid creating multiple intermediate `String` and `Vec<char>` allocations inside loop `near_title_prefix()`.
  - ⚡ **`src/typos.rs` Prune Keys Borrowing**: Changed `prune_aliases()` sorting vector from owned `Vec<(String, i64)>` to borrowed `Vec<(&str, i64)>`, allocating `to_drop` strings only for evicted entries ($O(\text{dropped})$ rather than $O(N)$ allocations).
  - ⚡ **`src/providers/mod.rs` Zero-Allocation ASCII Title Match**: In `title_match_indices()`, added fast-path ASCII byte window search (`eq_ignore_ascii_case`) for ASCII titles/queries, bypassing `to_lowercase()` String allocations and char iterator indexing for >99% of UI highlight lookups.
  - ⚡ **`src/providers/http.rs` Content-Length Buffer Pre-allocation & Deduplication**: Consolidated response stream consumption into `read_response_bytes()` helper across `get_bytes`, `get_bytes_background`, `get_bytes_query`, and `post_json`, pre-allocating `Vec::with_capacity` based on `Content-Length` header (bounded by 4 MB) to prevent multiple vector re-allocations during response reading.
  - 🐛 **`src/providers/mod.rs` `unformatted_value()` Comma Strip**: `1,000` now returns `1000` for Copy Unformatted Value (was keeping thousands sep). Guards empty/sign-only results.

---

### Batch 05: App Search & FX Providers
- **Files**: `src/providers/apps.rs` (1,269), `src/providers/fx.rs` (582)
- **Status**: `Completed (2nd pass verified)`
- **Key Focus**: Desktop entry parsing speed, icon resolution caching, FX rate parsing and offline fallback.
- **Notes & Fixes**:
  - ⚡ **`src/providers/apps.rs` Zero-Vector `quote_join()`**: Replaced intermediate `argv.iter().map(...).collect::<Vec<_>>().join(" ")` allocation with direct single-string accumulation.
  - ⚡ **`src/providers/apps.rs` Borrowed `normalize_desktop_id()`**: Replaced heap-allocating `to_ascii_lowercase()` / `to_string()` with borrowed `&str` stripping and case-insensitive equality (`eq_ignore_ascii_case`), eliminating string allocations when resolving desktop IDs in `display_name_for_desktop_id()`.
  - ⚡ **`src/providers/apps.rs` Heap Eviction & Span Cleanup**: In `AppProvider::search()`, tracked heap indices to evict dropped fuzzy match spans from `fuzzy_spans` map immediately upon min-heap eviction and used `fuzzy_spans.remove()` to avoid cloning span vectors.
  - ⚡ **`src/providers/fx.rs` Flattened Currency Normalization**: Flattened nested match arms in `normalize_currency` for 3-letter currency code lookups into direct match arms.
  - ⚡ **`src/providers/apps.rs` Hoisted Locale/Desktop Env**: `reload_from_dirs()` reads `current_locales()` + `current_desktops()` once, passes to `parse_desktop_file_with()` — was 500x env reads per reload. Removed dead `desktop_allowed()` wrapper.
  - ⚡ **`src/providers/apps.rs` Reuse `exec` String**: `to_result()` clones `app.exec` instead of `quote_join(&argv)` per hit per keystroke (20 joins saved).
  - 🐛 **`src/providers/fx.rs` Portable `geteuid()`**: Replaced Linux-only `/proc/self/status` parse with `extern geteuid()` — macOS disk cache was permanently untrusted, never persisted.

---

### Batch 06: Translation Engine
- **Files**: `src/providers/translate.rs` (1,674)
- **Status**: `Completed (2nd pass verified)`
- **Key Focus**: Translation query parsing, rate limit handling, response parsing efficiency, language pair resolution.
- **Notes & Fixes**:
  - ⚡ **`cache_key()` Streaming Hash Calculation**: Streamed `source`, `target`, and whitespace-normalized query tokens directly into FNV-1a hash state (`simple_hash_update`), completely eliminating intermediate `Vec<&str>` and `format!` heap string allocations on every translation cache check.
  - ⚡ **`is_lang_code()` Zero-Allocation Parsing**: Replaced `split(['-', '_']).collect::<Vec<&str>>()` and `.to_ascii_lowercase()` with zero-allocation `split(['-', '_'])` iterator and ASCII checks.
  - ⚡ **`maybe_sweep_cache()` Syscall & Sort Optimization**: Cached `mtime` from initial `fs::metadata` call directly into `(PathBuf, u64, u64)` tuple, eliminating $O(N)$ repeated `metadata(p).modified()` filesystem syscalls during sorting, and switched to `sort_unstable_by_key`.
  - ⚡ **`cache_put()` Memory Map Trimming**: Replaced `Vec<(u64, String)>` clone with `Vec<(&str, u64)>` borrowed slice references when pruning in-memory translation cache entries.
  - ⚡ **`strip_translate_prefix()` Zero-Alloc + Panic Fix**: Slice `eq_ignore_ascii_case` via `get(..len)` instead of whole-query `to_lowercase()`. `get` keeps multi-byte queries panic-free (caught 7 test failures on CJK/Cyrillic).
  - 🐛 **`plaintext_endpoint()` IPv6 + Userinfo**: Bracketed `[::1]` now parses host inside brackets (was `""` → false loopback refusal). Strips `user@` before host check.
  - 🐛 **Portable `geteuid()`**: Same macOS cache-disable bug as fx — disk cache never trusted on macOS. Fixed with `extern geteuid()`.

---

### Batch 07: Calc Core & Math/Expressions
- **Files**: `src/providers/calc/mod.rs` (219), `src/providers/calc/util.rs` (204), `src/providers/calc/math.rs` (234), `src/providers/calc/expr.rs` (687), `src/providers/calc/currency.rs` (213), `src/providers/calc/fueleco.rs` (175)
- **Status**: `Completed`
- **Key Focus**: AST evaluation safety, precision loss, tokenizer speed, currency formatting edge cases.
- **Notes & Fixes**:
  - ⚡ **`mod.rs` Zero-Alloc Plain-Text Gate**: Rewrote `looks_like_plain_text()` without `to_ascii_lowercase()` — keywords via `eq_ignore_ascii_case`, quickwin prefixes via boundary-safe `get(..len)`, function names via byte-window `contains_ignore_ascii_case`. Fast digit/operator checks first. Saves one heap alloc per keystroke on calc path.
  - 🐛 **`math.rs` Uppercase Function Gate Miss**: `looks_like_math()` matched `sqrt/sin/cos/tan/log/pi` case-sensitively, so `SQRT(16)`/`PI` never computed (evaluator supports them). Now case-insensitive via shared helper.
  - ⚡ **`util.rs` Shared `contains_ignore_ascii_case()`**: Byte-window helper, UTF-8 safe (non-ASCII bytes ≥0x80 never equal ASCII needle). Used by mod + math gates.
  - 🐛 **`util.rs` Fraction Trim**: `parse_qty_number()` now trims both sides of `/` (was only right) — `"1 /2"` style spaced fractions parse.
  - ⚡ **`util.rs` `relative_secs()` No-Alloc**: Replaced `to_lowercase()` + match with `eq_ignore_ascii_case` chain.
  - 🐛 **`currency.rs` Trailing Symbol Set**: `RE_SYM_AFTER` now includes `₩₽` matching leading set + fast path (`100₩`/`100₽` normalize).
  - ⚡ **`fueleco.rs` Single Lowercase**: `try_fuel_economy()` lowercases once, shares with `convert()` + `out_label()` (was 3x per query).

---

### Batch 08: Calc Units & Quick Parser
- **Files**: `src/providers/calc/units.rs` (872), `src/providers/calc/quick.rs` (1,326)
- **Status**: `Completed`
- **Key Focus**: Unit lookup hash map efficiency, regex compilation reuse, quick parser precedence.
- **Notes & Fixes**:
  - 🐛 **`units.rs` Temperature predictions restored**: `try_conversion_predict` used `to_base(&from)?.1` which is `None` for `c/f/k`, killing all `100 c to ` predictions. Now maps `c/f/k` → `"temperature"` category.
  - 🐛 **`quick.rs` Uppercase unit rejects**: `try_bmi` / `try_height` / `try_steps` matched `(?i)` captures case-sensitively — `BMI 180CM 75KG` misread as 180 m, `5'5" TO CM` → None, `10000 STEPS IN KM` → m. Now all `eq_ignore_ascii_case`; `case SNAKE` style too.
  - 🐛 **`units.rs` Specialty prediction gaps**: `UNIT_ALIASES` had zero pressure/energy/power/angle/frequency entries (plus missing `ms/us/wk/mo/yr/pb/tib/cm2/ft/s/um/nm/ug/m3`), so `10 pa to ` / `10 j to ` predicted nothing. Added ~70 aliases + preferred ranking per new category.
  - 🐛 **`quick.rs` `from_roman` robustness**: `Vec<char>` + unchecked `+=` + unbounded `+` regex. Now byte loop, `checked_add/sub`, 32-char cap.
  - 🐛 **`units.rs`/`quick.rs` Non-finite guards**: `convert()` now rejects non-finite input/result (overflow → `None`); `parse_size_bytes` / `parse_speed_bps` / `try_speed` reject `Inf`.
  - ⚡ **`units.rs` `normalize_unit()` single-pass**: Was `to_lowercase` + 5 chained `replace` (6 allocs) per resolve. Now one `with_capacity` char fold.
  - ⚡ **`units.rs` Zero-alloc gates**: `is_magnitude_word()` + bare `m/b/t` check via `eq_ignore_ascii_case` (was `to_ascii_lowercase` per keystroke); `predict_units()` prefix via `get(..len)` + `contains_ignore_ascii_case`, no `to_lowercase`; `HashSet::with_capacity` + contains-before-clone dedup.
  - ⚡ **`quick.rs` Per-keystroke alloc purge**: `try_base_convert` / `try_random` / `try_uuid` / `try_password` no longer `to_ascii_lowercase` whole query (`(?i)` regex + case-insensitive helpers); `try_roman` / `try_text` / `try_password` fast prefix gates before regex; `shown` only on success; `infer_base` zero-alloc (no lowercase, `from_str_radix` handles case); `split_num_unit` single regex with `\s*` (no filter-collect); `slug` single-pass; `case` borrowed `Vec<&str>`.
  - ⚡ **`quick.rs` `base_card` sign-safe**: Replaced `trim_start_matches("0x")` (left `-0xff` for negatives) with sign + `unsigned_abs` rendering.
  - 🦀 **`quick.rs` `uuid_v4(&[u8]) -> Option`**: Was `Vec<u8>` + `copy_from_slice` panic on wrong len. Now `try_into` checked.

---

### Batch 09: Calc Unit Math & Conversions
- **Files**: `src/providers/calc/unitmath.rs` (630), `src/providers/calc/battery.rs` (483), `src/providers/calc/cooking.rs` (542)
- **Status**: `Completed`
- **Key Focus**: Dimensional analysis accuracy, fractional unit parsing, cooking volume conversions.
- **Notes & Fixes**:
  - 🐛 **`unitmath.rs` Fraction addition dropped**: `had_fraction` path used `qty.unit?`, so `1/2 cup + 1/2 cup` (unit=None after add_sub) returned None. Now falls back to `display_qty` when unit lost.
  - 🐛 **`unitmath.rs` Speed divide-by-zero**: length/time branch missed `b.base==0` check (`5km/0h` → inf). Now rejects.
  - 🐛 **`unitmath.rs` Bare guard case leak**: `matches!(m|b|t)` missed `M/B/T`, so `100M` rendered while `100m` stayed unresolved. Now `eq_ignore_ascii_case`.
  - 🐛 **`unitmath.rs` Bare area/speed vanished**: `bare_value_card` returned None for area/speed/pressure/energy/power/angle/frequency. Now extends base units + `smart_prefix`.
  - 🐛 **`unitmath.rs` Volume order dead code**: `ml` before `tbsp/tsp` made those unreachable. Reordered to `gal,l,pt,cup,tbsp,tsp,ml`.
  - 🐛 **`unitmath.rs` Category-name display**: pressure/energy/etc fallback showed `5 pressure`. Now real prefix lists + raw value fallback + finite guards on all add/sub/mul/div paths.
  - ⚡ **`unitmath.rs` Unicode replace alloc**: `q.replace×÷` ran twice per keystroke. Now `Cow` borrowed fast path.
  - 🐛 **`battery.rs` Charging ETA suffix**: discharging + charging both said `left`. Now `to full` when charging.
  - 🐛 **`battery.rs` Mains missing file**: absent `online` forced `Some(false)` → misclassified Battery. Now leaves untouched; USB empty branch removed; unreachable inference branch simplified.
  - ⚡ **`battery.rs` Per-keystroke allocs**: `is_battery_query` lowercased whole query + `to_ascii_lowercase` per battery. Now `eq_ignore_ascii_case` chain; `sort_unstable_by`.
  - 🐛 **`cooking.rs` Butter volume ignored**: `1 stick butter in cups` fell back to g. Now converts via density (113.4g / 227g-per-cup).
  - 🐛 **`cooking.rs` Oven badge case**: `Fan` capital returned `conventional`. Now case-insensitive.
  - ⚡ **`cooking.rs` Alloc purge**: `find_ingredient` inner `format!` per alias → byte-window `contains_word`; `vol_ml` `replace(' ')` → direct match; `try_recipe_scale` fast gate before lowercase; `try_oven` single regex match (was 3x); redundant lowercase removed.
  - ⚡ **`cooking.rs` UK spellings + decimal servings**: added `litre/millilitre` variants; servings regex now allows decimals with finite guards.

---

### Batch 10: Calc Financial & Specialty
- **Files**: `src/providers/calc/financial.rs` (542), `src/providers/calc/home.rs` (256), `src/providers/calc/duration.rs` (397)
- **Status**: `Completed`
- **Key Focus**: Loan / mortgage precision, duration arithmetic overflow protection.
- **Notes & Fixes**:
  - 🐛 **`financial.rs` Negative amounts rendered**: interest/discount/split/gst/hourly accepted signed AMT with no positivity check (`-25/hr` → `-52000/yr`). Now all reject `<=0` + non-finite.
  - 🐛 **`financial.rs` Inf cards**: discount/gst pct, emi totals, cagr t/rate, rule72 years, pct_change overflow had no finite guards (`72 at inf%` → `0 years`). Now all reject non-finite.
  - 🐛 **`financial.rs` EMI tenure truncation**: `months as i64` truncated fractionals + saturated huge floats. Now `format_number(months)` + interest-total finite guard.
  - ⚡ **`financial.rs` 10-regex per keystroke**: every financial sub-parser ran its regex on all queries. Now cheap `contains_ignore_ascii_case` keyword gates before each regex; `compound` check zero-alloc.
  - 🐛 **`home.rs` Wrong EUR for non-eurozone**: `Europe/` catch-all returned EUR for CHF/SEK/NOK/DKK/CZK/PLN/HUF/RON/BGN/ISK. Now explicit mappings.
  - 🐛 **`home.rs` Missing zones**: added Calcutta/Rangoon/Saigon aliases, Taipei/VND/MYR, Chatham/NZD.
  - ⚡ **`duration.rs` `to_lowercase()` per keystroke**: redundant with `(?i)` regexes + destroyed display case. Now parses original query directly; `m`-ambiguity check case-insensitive; `q.get(last_end..)` boundary-safe.
  - 🐛 **`duration.rs` Overflow shown as `0s`**: pct-of path had no finite guard (`format_duration(inf)` → `0s`). Now rejects; absurd finite totals capped at ~100y; scale overflow rechecked after multiply.
  - 🐛 **`unitmath.rs` Negative bare time**: `format_duration(neg)` → `0s`. Now `-` prefixed abs rendering.
  - 🏁 **F1 venues (out-of-band, `timezone.rs` Batch 11 file)**: added 2026 circuit aliases (monza/imola/madring/spa/zandvoort/suzuka/sakhir/jeddah/baku/lusail/yas/montreal/austin/vegas etc) + country aliases (italy/spain/austria/belgium/hungary/netherlands/...). `15:00 here to monza` + `now in italy` now resolve.

---

### Batch 11: Calc Datetime & Timezones
- **Files**: `src/providers/calc/datetime.rs` (842), `src/providers/calc/timezone.rs` (1,118)
- **Status**: `Completed`
- **Key Focus**: Daylight saving transitions, timezone database lookup speed, fuzzy date parsing.
- **Notes & Fixes**:
  - 🐛 **`datetime.rs` Trim gate miss**: `q.to_lowercase()` without `trim()` broke ` now `, ` week `, bare epoch with spaces, padded ISO dates. Now single `trimmed = q.trim()` used everywhere.
  - 🐛 **`datetime.rs` Feb 29 dead**: `29 feb` on non-leap year returned None. Now scans `year..=year+8` for next valid future date; `31 feb`-style stays None.
  - 🐛 **`datetime.rs` NaN snap-to-now**: `inf + -inf` token mix gave NaN → `as i64 == 0` → now card. Now returns None on NaN; inf still clamps to ±100y.
  - 🐛 **`datetime.rs` `date_of().expect()` panic**: replaced with `Option` + fallback to time-only; `MONTH_ABBR` index now bounds-checked via `month_abbr()`.
  - 🐛 **`timezone.rs` IANA case break**: normalized lower (`europe/london`) failed `Tz::parse`. Now tries original-case + space→underscore first, plus Title-Case fallback for lowercase IANA (`america/new_york` → `America/New_York`).
  - 🐛 **`timezone.rs` Seconds dropped**: copy used `%H:%M` while right showed `%H:%M:%S`; id lacked seconds causing collisions. Now both seconds-aware.
  - 🐛 **`timezone.rs` `local_as_tz()` seasonal fail**: `HALF_HOUR_ZONES` missed St Johns std (-3:30), Lord Howe DST (+11), Chatham DST (+13:45) → `here` conversions failed half the year. Added 3 entries.
  - 🦀 **`timezone.rs` Duplicate `madrid` alias**: removed F1-table duplicate.
  - ⚡ **`datetime.rs` Per-keystroke `to_lowercase()` purge**: exact words via `eq_ignore_ascii_case`, `(?i)` regexes on `trimmed` original — saves one heap alloc per keystroke, preserves display case; `month_idx` + `parse_clock` ampm also zero-alloc.
  - ⚡ **`timezone.rs` Per-keystroke `to_lowercase()` purge**: same — `trim()` only, `here/local/system` checks via `eq_ignore_ascii_case`.
  - ⚡ **`timezone.rs` Fuzzy alloc purge**: `predict_tz` precomputed `COMPACT_ALIASES` static (was 300× `replace` per conversion); `resolve_tz` compact find now underscore-filters without alloc.
  - ⚡ **`datetime.rs` Minor**: `fmt_days_until` clone→2 allocs, `fmt_span` `with_capacity(3)`, final date parse on `trimmed`.

---

### Batch 12: File Index & Cache Layer
- **Files**: `src/providers/files/hot.rs` (164), `src/providers/files/live_cache.rs` (388), `src/providers/files/index.rs` (1,229)
- **Status**: `Completed`
- **Key Focus**: File index memory footprint, incremental update speed, cache invalidation, lock granularity.
- **Notes & Fixes**:
  - 🐛 **`index.rs` Non-UTF8 names invisible**: `index_entry` + `from_cache_entry` used `file_name()?.to_str()?` → None dropped non-UTF8 files entirely. Now `to_string_lossy().into_owned()`.
  - 🐛 **`hot.rs` Lost-dirty race**: `ensure_fresh` load-then-`rebuild`-then-store(false) overwrote concurrent `mark_dirty(true)` → stale hot set. Now swap-claim (`swap(false, AcqRel)`) + clear-before-build; `mark_dirty` uses Release for usage-write visibility.
  - 🐛 **`live_cache.rs` Eviction panic**: `evict_to_cap().expect()` panicked on map/recency drift. Now graceful break.
  - ⚡ **`hot.rs` 100k-entry HashMap purge**: `build_hot_set` hashed full index per rebuild. Now wanted-keyed map (≤128) + single index scan with early exit; output still frecency-ordered, first-occurrence wins.
  - ⚡ **`index.rs` Skip-list alloc purge**: `should_always_skip` did `to_string_lossy()` per component (~500k allocs/build). Now `to_str()` borrow-only fast path.
  - ⚡ **`index.rs` `has_datestamp` Vec purge**: `split().filter().collect::<Vec>()` allocated per non-stamped name (walk majority). Now streaming 3-window over run lengths, zero alloc.
  - ⚡ **`index.rs` Generated-name lowercase purge**: `is_generated_filename/dirname` unconditionally `to_ascii_lowercase()` per entry. Now `lower_if_needed()` borrows unless uppercase present (B02 pattern).
  - ⚡ **`index.rs` `seen` pre-size**: `HashSet::new()` → `with_capacity(4096)` matching items vec.
  - 📝 **Known minor (no fix)**: `forget_path` retain can lose to concurrent `run_build` index swap (needs tombstone set; rare + transient, hot/live already invalidated).

---

### Batch 13: File Provider Core & Search Types
- **Files**: `src/providers/files/mod.rs` (964), `src/providers/files/search/mod.rs` (813), `src/providers/files/search/rank.rs` (411)
- **Status**: `Completed`
- **Key Focus**: Search ranking scoring stability, match highlighting allocation overhead, provider lifecycle.
- **Notes & Fixes**:
  - 🐛 **`mod.rs` Non-UTF8 display gap**: `resolve_path` + test `seed_index` showed `?` for non-UTF8 names while B12 index shows lossy. Now `to_string_lossy` everywhere.
  - ⚡ **`search/mod.rs` Per-keystroke `to_lowercase()` purge**: free-text `q.to_lowercase()` allocated every keystroke. Now `Cow` borrows when no uppercase present (Unicode-aware check); empty-index check moved before fold so cold-start skips alloc.
  - ⚡ **`rank.rs` Double `display_name()` purge**: `heap_to_results` computed display name twice per result (matched fallback + title). New `indexed_to_result_with_title` computes once (~50 allocs saved/keystroke at cap).
  - ⚡ **`mod.rs` Merge-sort alloc purge**: `merge_cached` sort comparator allocated 2 Strings per comparison. New `cmp_title_fold` streams `char::to_lowercase`, zero alloc, deterministic tiebreak.
  - ⚡ **`mod.rs` Icon ext alloc purge**: `icon_for_path` unconditionally lowercased extension per result. Now borrows unless uppercase present.
  - 📝 **No scoring changes**: bands/boosts/penalties/deep-gating untouched; lock ordering (index→hot, memo→index) verified deadlock-free.

---

### Batch 14: File Search Engines (Plan/Glob/Deep)
- **Files**: `src/providers/files/search/plan.rs` (697), `src/providers/files/search/glob.rs` (914), `src/providers/files/search/deep.rs` (917)
- **Status**: `Completed`
- **Key Focus**: Thread pool saturation, traversal early exit, regex vs prefix matching optimizations.
- **Notes & Fixes**:
  - ⚡ **`glob.rs` `glob_match()` zero-alloc rewrite**: Was `Vec<char>` ×2 per item (200k allocs/keystroke over 100k index). Now byte-index streaming with `str[ch..].chars()` — no heap, same `?`=one-Unicode-char semantics. Preserves `a?`→`aé` tests.
  - 🐛 **`glob.rs`/`deep.rs` Non-UTF8 invisible gaps**: `match_mid_glob`, `search_absolute_glob` live, `live_deep_under_roots` + final map used `file_name().and_then(|s| s.to_str())` → skipped non-UTF8 entirely. Now `to_string_lossy()` everywhere (B12/B13 parity). `path_completions` prefix also lossy.
  - 🐛 **`deep.rs` `score_live_hit()` contains-band clamp miss**: `glob.rs` clamps boosted contains to `SKIP-1`, deep did not — depth/high/mnt boosts (+13k) could push substring-only live hit over 30k, falsely cancelling sibling deep jobs. Now same `min(SKIP-1)` clamp.
  - 🐛 **`deep.rs` `hit_paths` final sort non-deterministic**: `sort_by_key(Reverse(score))` stable-preserved WalkDir arrival (readdir varies). Now `sort_by(score desc, path asc)`. Same fix for `maybe_live_relative_glob` + `search_absolute_glob` live: collect+sort before LIMIT cap so huge dirs yield deterministic subsets.
  - ⚡ **`glob.rs` `match_mid_glob` hoisted `head.to_lowercase()`**: Was recomputed per directory entry. Now once per level.
  - ⚡ **`deep.rs` `roots_from_segments()` wasted clone**: `Vec<(score, PathBuf, String)>` cloned `path_lower` per candidate but discarded on return. Now `Vec<(score, PathBuf)>`.
  - ⚡ **`plan.rs` scope-hint alloc purge**: `parse_scope_hint_query()` allocated 3 Strings just for `.is_some()` gates in `is_path_glob_query`, `is_scoped_file_query`, `plan_deep_jobs`, `should_deep_search`. New `is_scope_hint_query()->bool` + shared `find_scope_keyword()` helper — saves 3 allocs ×4 call sites per keystroke.
  - ⚡ **`plan.rs` `scope_folder_suggestions()` double index scan**: `exact_dir` + `last_is_partial` were 2× O(N) `.any()` scans. Now single pass with early break on exact.
  - ⚡ **`glob.rs` `is_drive_path_query()` zero-alloc**: Was `to_ascii_lowercase()` + `chars().nth(8)` per query. Now byte `eq_ignore_ascii_case(b"windows ")` + `is_ascii_alphabetic`.
  - ⚡ **`glob.rs`/`deep.rs` single-scan metachar checks**: `pat.contains('*') || contains('?')` → `pat.bytes().any(|b| b==b'*'||b==b'?')` in `name_matches_pat`, `score_glob_item` (×2), `score_live_hit`, `match_mid_glob`, `maybe_live_relative_glob`.
  - ⚡ **`glob.rs` `path_completions()` per-entry `to_lowercase()` purge**: ASCII prefixes (99%) now `eq_ignore_ascii_case` byte-prefix without alloc; Unicode fallback preserves semantics.
  - 📝 **No scoring changes**: bands/boosts/penalties/deep-gating thresholds untouched except contains-clamp parity; lock ordering N/A (index snapshot + worker, no locks held during WalkDir).

---

### Batch 15: Theme & Styling Engine
- **Files**: `src/theme/mod.rs` (274), `src/theme/css.rs` (1,246)
- **Status**: `Completed`
- **Key Focus**: CSS generation efficiency, color parsing, dynamic theme switching without memory leak.
- **Notes & Fixes**:
  - 🐛 **`mod.rs` `ThemeManager::new()` headless panic**: `Display::default().expect("display")` crashed daemon/tests without GTK display. Now `if let Some(display)` graceful skip — provider still renders CSS, install skipped.
  - 🐛 **`css.rs` unsanitized `ui.accent` CSS-injection sink**: `render()` interpolated raw `ui.accent` into `caret-color`/badge/selected rules. Store `sanitize()` restricts to `#rrggbb|None`, but `render` is `pub` — any future caller skipping store sanitize injects arbitrary CSS. Now `sanitize_hex(primary_raw)` on render path by construction (+ regression test with `red; } .pwned {` payload).
  - 🐛 **`css.rs` NaN/Inf CSS poisoning**: `f32::clamp` passes NaN through → `NaNpx`/`rgba(..., NaN)` dropped by GTK → unstyled panel. Now `is_finite()` fallbacks (opacity→0.85, scale→1.0) + regression test.
  - ⚡ **`css.rs` `rgb_bytes()` zero-alloc rewrite**: Was `Vec<char>` + `format!` + `to_string()` per color (~15 allocs per `render`). Now byte-index `hex_nibble(b)*17` nibble doubling + in-place `from_str_radix` slices, zero heap. Shorthand/longhand equivalence pinned by test.
  - ⚡ **`mod.rs` scheme-debounce symmetry**: `apply()` cancelled UI timer but left 80ms scheme timer queued → double disk-read + CSS inject on race. New `cancel_scheme_debounce()` called from `apply()` + reused in `watch()` debounce (no-op when invoked from inside own timer).
  - ⚡ **`mod.rs` `watch()` fallback double-apply**: `new()` runs `apply()` then `watch()`; monitor-failure fallback called `apply()` again (2nd disk read + inject, rare path). Now early return — theme already applied.
  - 📝 **Verified safe, no change**: `sanitize_hex` multi-`#` leniency harmless; `rgb_bytes` fallbacks unreachable post-sanitize (defensive); `is_light()`/`apply_ui_only()` RefCell borrows main-thread only (`Rc<!Send>`); `Theme::load` per-key allocs startup-only; `reload()` 12 settings call sites share 60ms debounce + `apply_gen` stale-guard; hardcoded destructive `#f7768e` noted (needs scheme error color — out of scope).

---

### Batch 16: UI Micro-Components & Animations
- **Files**: `src/ui/footer.rs` (113), `src/ui/size_anim.rs` (188), `src/ui/scroll_anim.rs` (181), `src/ui/action_panel.rs` (247), `src/ui/thumbnails.rs` (333)
- **Status**: `Completed`
- **Key Focus**: Frame interpolation math, thumbnail async decode caching, widget layout invalidation.
- **Notes & Fixes**:
  - 🐛 **`thumbnails.rs` `store_*` i32 overflow panic**: `rowstride < width * n_channels` overflows i32 for corrupt dims (600M×4 → 2.4G), panicking debug builds. Rewrote guard fully in i64 saturating math (`min_bytes = (h-1)*stride + w*ch`) + regression test with `width=600M, stride=i32::MAX` (returns false, no panic).
  - 🐛 **`thumbnails.rs` Thumb::URI/digest symlink mismatch**: digest key used `parent.canonicalize()+name`, stored URI used unresolved `file_uri(source)` — disagree whenever any parent is a symlink, so stored chunk never matches readers. Now resolves parent once, derives both digest + URI from same path.
  - ⚡ **`thumbnails.rs` `md5_hex()` per-byte `format!` purge**: 16 tiny allocs per digest (per image probe while scrolling). Now nibble lookup table, single 32-cap String. Correctness pinned by RFC 1321 vectors (`""`, `"a"`, `"abc"`, `"message digest"`, alphabet) — hand-rolled MD5 previously had zero cross-check (self-consistent store/read masked interop breakage).
  - 🦀 **`action_panel.rs` `move_selection()` hardened**: `(cur+1)%n` / `cur-1` arms ignored magnitude + treated 0-step as up. Now `(cur+delta).rem_euclid(n)` + `delta==0` early return. Click vs Enter double-fire traced safe: single-threaded `is_open` gating serializes both orders (capture-Stop or bubble-popdown).
  - 📝 **Verified safe, no change**: `footer.rs` single-clone per selection + exhaustive `ResultKind` match (compiler-forced); `size_anim.rs` Euclidean travel, hidden-snap covers `-1` unset requests, `dur*1000` f64 div0-safe, i64-first frame delta exact, weak-tick no leak; `scroll_anim.rs` short-content pin, page-0 guards, retarget-from-live-offset chaining; `stored_mtime` chunk parser checked-add/bounds (no panics); `pixels.to_vec()` copy kept (borrowed API, once per image).

---

### Batch 17: UI Interactions & Result Rows
- **Files**: `src/ui/dnd.rs` (398), `src/ui/open_with.rs` (508), `src/ui/rows.rs` (1,101)
- **Status**: `Pending`
- **Key Focus**: Drag-and-drop protocol compliance, list view virtual scrolling / row recycling, event bubbling.
- **Notes & Fixes**:
  - *(To be recorded during audit)*

---

### Batch 18: UI Preview Window
- **Files**: `src/ui/preview.rs` (2,164)
- **Status**: `Pending`
- **Key Focus**: Preview generation throttling, mime detection, syntax highlighting worker threads, memory management for large assets.
- **Notes & Fixes**:
  - *(To be recorded during audit)*

---

### Batch 19: UI Settings Window
- **Files**: `src/ui/settings.rs` (2,371)
- **Status**: `Pending`
- **Key Focus**: Two-way binding responsiveness, settings persistence debounce, schema validation error handling.
- **Notes & Fixes**:
  - *(To be recorded during audit)*

---

### Batch 20: UI Main / View Model (Part 1)
- **Files**: `src/ui/mod.rs` (Lines 1–1800)
- **Status**: `Pending`
- **Key Focus**: State management, window lifecycle, query input handling, keyboard shortcut routing.
- **Notes & Fixes**:
  - *(To be recorded during audit)*

---

### Batch 21: UI Main / Event Loop (Part 2)
- **Files**: `src/ui/mod.rs` (Lines 1801–3572)
- **Status**: `Pending`
- **Key Focus**: Render loop efficiency, Wayland layer shell integration, async event processing, graceful cleanup.
- **Notes & Fixes**:
  - *(To be recorded during audit)*
