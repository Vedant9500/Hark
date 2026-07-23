# Style Guide Review Tracker (Temporary)

Temporary tracking file for a 5-part style guide review of the Blink codebase.
Scope: `src/` only (~21.7k LOC). Build artifacts, packaging, and docs are out of scope.

| Part | Name | Approx. LOC | Status |
|------|------|-------------|--------|
| 1 | Core & App Shell | ~3,530 | ✅ complete |
| 2 | UI Shell | ~3,790 | ✅ complete |
| 3 | UI Features & Theme | ~4,760 | ✅ complete |
| 4 | Providers (Apps, Calc, FX, HTTP, Translate) | ~5,030 | ✅ complete |
| 5 | Files Provider | ~4,660 | ⬜ pending |

**Overall status:** 4 / 5 complete

**Style reference:** [Rust Style Guide](https://doc.rust-lang.org/style-guide/) (default Rust style; project uses `rustfmt.toml` edition 2021, `max_width = 100`).

---

## Part 1 — Core & App Shell

**Focus:** entrypoint, config, search engine, IPC, usage tracking, typo correction, benchmarks.

| File | LOC | Status |
|------|-----|--------|
| `src/main.rs` | 179 | ✅ |
| `src/config.rs` | 976 | ✅ |
| `src/engine.rs` | 819 | ✅ |
| `src/ipc.rs` | 194 | ✅ |
| `src/usage.rs` | 289 | ✅ |
| `src/typos.rs` | 579 | ✅ |
| `src/bench.rs` | 490 | ✅ |

**Part status:** ✅ complete  
**Reviewed:** 2026-07-23 against https://doc.rust-lang.org/style-guide/

### Verdict

Mostly compliant. No line-width violations (>100), no tabs, no trailing whitespace, no block comments. Naming (`snake_case` / `UpperCamelCase` / `SCREAMING_SNAKE_CASE`) is solid. Main gap is **rustfmt drift** on 5 of 7 files (layout of short vs multi-line expressions), plus a few **double blank lines** and one **module declaration order** nit.

### Findings

#### P1 — rustfmt layout drift (must fix for default style)

`rustfmt --check` fails on: `config.rs`, `engine.rs`, `ipc.rs`, `typos.rs`, `bench.rs`.  
`main.rs` and `usage.rs` are already clean.

Style guide basis: indentation/line width, block indent, small-item single-line form, blank lines (0 or 1 between items).

| File | Issue (rustfmt wants) |
|------|------------------------|
| `bench.rs:50` | Multi-line `if` arms instead of single-line `if deep.is_empty() { … } else { … }` |
| `bench.rs:160–176` | Collapse short `println!` calls onto one line |
| `bench.rs:246` | Remove extra blank line before `pick_bench_app_query` |
| `bench.rs:250` | Multi-line array literal with trailing comma |
| `bench.rs:283` | Collapse `p95` assignment onto one line |
| `config.rs:324` | Double blank line before `LayoutMode` → single blank |
| `config.rs:456` | Collapse `target_lang` chain onto one line |
| `config.rs:778` | Break method chain after `best` (block indent) |
| `config.rs:921–929` | Multi-line `assert_eq!` / `assert!` when message doesn't fit |
| `config.rs:961` | Collapse short array arg onto one line |
| `config.rs:EOF` | Trailing double blank after final `}` |
| `engine.rs:223` | Prefer chain form for `results.iter().any(...)` |
| `engine.rs:361–364` | Single-line `secondary_actions` signature |
| `engine.rs:711–715` | Single-line `if` condition with `\|\|` |
| `engine.rs:738–740` | Single-line `if` condition |
| `engine.rs:801` | Double blank before test module |
| `ipc.rs:114` | Double blank before `#[cfg(test)]` |
| `typos.rs:221–225` | Collapse `sort_by` comparator |
| `typos.rs:258–260` | Different break for `ok_or_else` |
| `typos.rs:402` | Collapse `cur[j] = …` chain |
| `typos.rs:508+` | Collapse short `learn_from_launch` / `assert_eq!` calls |

**Fix:** `cargo fmt` (or `rustfmt` on those files). No semantic change.

#### P2 — blank lines (style guide: 0 or 1 between items)

Double blank lines found at:

- `config.rs:323–324` (before `LayoutMode`)
- `engine.rs:800–801` (before tests)
- `ipc.rs:113–114` (before tests)
- `bench.rs:245–246` (before helper)

#### P3 — module declaration order (`main.rs`)

Style guide: version-sort module declarations. Current order:

```text
config, engine, ipc, providers, theme, ui, usage, typos
```

`typos` should come before `ui` and `usage` alphabetically/version-sort:

```text
config, engine, ipc, providers, theme, typos, ui, usage
```

(Imports after mods look fine; crate uses are appropriately ordered relative to `std`/`gtk`.)

#### P4 — minor / optional (advice, not hard style-guide musts)

| Item | Notes |
|------|--------|
| `PathStyle` manual `Default` | Could be `#[derive(Default)]` + `#[default]` on `Label` (same pattern as `LayoutMode`) — idiomatic, not required by the formatting guide |
| Expression-oriented style | Generally good (`let x = if …`, early returns). No systematic anti-pattern found |
| Comments | Prefer `//` (done). Many are sentence fragments / rationale notes — guide says complete sentences are a *recommendation* only |
| `Cargo.toml` | Present and reasonable; out of Part 1 `src/` scope but edition matches `rustfmt.toml` (2021) |

### What already looks good

- 4-space indent, spaces not tabs
- Max line width ≤ 100 in source (no over-width lines)
- Trailing commas on multi-line lists where already multi-line
- Block indent preferred over visual indent
- Naming conventions followed
- `///` / `//!` docs used appropriately (`typos.rs`, `bench.rs`, public APIs)
- `usage.rs` fully rustfmt-clean

### Suggested next action

```bash
cargo fmt
```

Then re-check Part 1 files; remaining manual nits are only the `mod` sort in `main.rs` if you want strict item-order compliance.

### Fixes applied (2026-07-23)

- [x] `cargo fmt` — all Part 1 files pass `rustfmt --check`
- [x] Double blank lines removed (via fmt)
- [x] `main.rs` module order: `typos` before `ui` / `usage`
- [x] `PathStyle`: `#[derive(Default, Copy)]` + `#[default]` on `Label` (dropped manual `impl Default`)
- [x] `cargo check --features "layer-shell,bench"` succeeds

---

## Part 2 — UI Shell

**Focus:** main window / list UI, row rendering, footer, action panel, drag-and-drop, thumbnails, base CSS.

| File | LOC | Status |
|------|-----|--------|
| `src/ui/mod.rs` | ~2197 | ✅ |
| `src/ui/rows.rs` | ~496 | ✅ |
| `src/ui/dnd.rs` | ~368 | ✅ |
| `src/ui/action_panel.rs` | ~247 | ✅ |
| `src/ui/thumbnails.rs` | 225 | ✅ |
| `src/ui/style.css` | 166 | ✅ (CSS; not rustfmt) |
| `src/ui/footer.rs` | 80 | ✅ |

**Part status:** ✅ complete  
**Reviewed:** 2026-07-23 against https://doc.rust-lang.org/style-guide/

### Verdict

**Clean.** All six Rust files pass `rustfmt --check` with project `rustfmt.toml` (edition 2021, max_width 100). No tabs, no trailing whitespace, no double blank lines, no lines over 100 columns, no block comments in Rust. Naming and module layout match the guide. No mandatory fixes.

*(Already reformatted by the earlier full-crate `cargo fmt` from Part 1.)*

### Findings

#### No P1 issues

| Check | Result |
|--------|--------|
| rustfmt layout | Pass — all 6 `.rs` files |
| Max width 100 | Pass |
| Spaces / no tabs | Pass |
| Trailing whitespace | Pass |
| Blank lines (0 or 1) | Pass |
| Module order (`ui/mod.rs`) | Pass — alpha: `action_panel`, `dnd`, `footer`, `open_with`, `preview`, `rows`, `settings`, `thumbnails` |
| Naming | Pass — `snake_case` / `UpperCamelCase` / `SCREAMING_SNAKE_CASE`; `box_` for reserved `box` (guide-approved) |
| Prefer `//` over `/* */` (Rust) | Pass |
| Block indent | Pass |

#### Informational / out of Rust style-guide scope

| Item | Notes |
|------|--------|
| `src/ui/style.css` | GTK CSS, not Rust. Uses 2-space indent and `/* */` comments (normal for CSS). Not governed by the Rust Style Guide or rustfmt. |
| `footer.rs` module docs | Sibling files use `//!` crate/module docs; `footer.rs` has none. Soft consistency preference only. |
| `#[allow(dead_code)]` | Present on a few helpers/fields (`footer`, `dnd`, `mod.rs`). Not a formatting/style-guide violation. |
| Import grouping | `group_imports` default is Preserve; rustfmt accepts current order. No reorder needed. |

### What looks good

- Strong `//!` module docs on `rows`, `dnd`, `action_panel`, `thumbnails`
- Expression-oriented control flow (`if let`, early returns, `let … = match`)
- Multi-line function args use trailing commas
- Constants properly `SCREAMING_SNAKE_CASE` (`WINDOW_WIDTH`, `SEARCH_DEBOUNCE_MS`, …)
- Large `ui/mod.rs` stays within width and rustfmt layout rules

### Fixes applied

None required for Part 2.

---

## Part 3 — UI Features & Theme

**Focus:** settings UI, preview pane, open-with dialog, theme generation / CSS injection.

| File | LOC | Status |
|------|-----|--------|
| `src/ui/settings.rs` | ~2208 | ✅ |
| `src/ui/preview.rs` | ~1081 | ✅ |
| `src/theme/css.rs` | ~935 | ✅ |
| `src/ui/open_with.rs` | ~343 | ✅ |
| `src/theme/mod.rs` | ~175 | ✅ |

**Part status:** ✅ complete  
**Reviewed:** 2026-07-23 against https://doc.rust-lang.org/style-guide/

### Verdict

**Mostly clean.** All five files already pass `rustfmt --check`. One hard style-guide issue: three UI strings in `settings.rs` exceeded the 100-column max (rustfmt does not reflow string literals). Fixed by wrapping with `\` string continuations. No other mandatory fixes.

### Findings

#### P1 — line width > 100 (settings copy strings) — **fixed**

Style guide: max line width 100. These were single-line string literals:

| Location | Was | Fix |
|----------|-----|-----|
| `settings.rs` Appearance page_shell subtitle | ~128 cols | `\`-continued string |
| `settings.rs` Appearance note label | ~180 cols | `\`-continued string |
| `settings.rs` Tools note label | ~185 cols | `\`-continued string |

String values unchanged (verified with a small rustc join check). `cargo check` still green.

#### No other P1 issues

| Check | Result |
|--------|--------|
| rustfmt layout | Pass (all 5 files) |
| Tabs / trailing space / double blanks | Pass |
| Naming | Pass (`box_` for reserved `box` is guide-approved) |
| Module order (`theme/mod.rs`) | Pass (`mod css;`) |
| Prefer `//` in Rust | Pass — `/* */` inside `theme/css.rs` are **CSS comments in a format string**, not Rust block comments |

#### Informational

| Item | Notes |
|------|--------|
| Module docs | `open_with.rs` has `//!`; `settings.rs` / `preview.rs` / `theme/*` do not — soft consistency only |
| `theme/css.rs` | Large raw CSS template string; indent inside the string is CSS convention (2-space), not Rust block indent |
| `#[allow(dead_code)]` / clippy allows | Present; not a formatting violation |

### Fixes applied (2026-07-23)

- [x] Wrap three long user-facing strings in `src/ui/settings.rs` to ≤100 columns
- [x] Re-check: no Part 3 lines > 100; rustfmt clean; `cargo check --features "layer-shell,bench"` OK

---

## Part 4 — Providers (Apps, Calc, FX, HTTP, Translate)

**Focus:** provider trait / registry, app launcher, calculator submodules, FX rates, HTTP helper, translate.

| File | LOC | Status |
|------|-----|--------|
| `src/providers/mod.rs` | ~322 | ✅ |
| `src/providers/apps.rs` | ~435 | ✅ |
| `src/providers/fx.rs` | ~261 | ✅ |
| `src/providers/http.rs` | 78 | ✅ |
| `src/providers/translate.rs` | ~1016 | ✅ |
| `src/providers/calc/mod.rs` | ~150 | ✅ |
| `src/providers/calc/expr.rs` | ~391 | ✅ |
| `src/providers/calc/math.rs` | ~148 | ✅ |
| `src/providers/calc/units.rs` | ~547 | ✅ |
| `src/providers/calc/timezone.rs` | ~748 | ✅ |
| `src/providers/calc/datetime.rs` | ~174 | ✅ |
| `src/providers/calc/duration.rs` | ~107 | ✅ |
| `src/providers/calc/currency.rs` | ~128 | ✅ |
| `src/providers/calc/battery.rs` | ~444 | ✅ |
| `src/providers/calc/util.rs` | 70 | ✅ |

**Part status:** ✅ complete  
**Reviewed:** 2026-07-23 against https://doc.rust-lang.org/style-guide/

### Verdict

**Mostly clean.** All 15 files already pass `rustfmt --check`. Main issue: **13 lines over 100 columns** — almost all long regex string literals (rustfmt will not reflow them) plus one test desktop-file fixture. Fixed by splitting regexes with `concat!(…)` and wrapping the desktop fixture with `\` string continuations. Module order, naming, tabs, blanks: good.

### Findings

#### P1 — line width > 100 — **fixed**

| File | Lines | Kind | Fix |
|------|-------|------|-----|
| `apps.rs` | 1 | Test `.desktop` fixture string (~284 cols) | Multi-line string with `\n\` |
| `calc/math.rs` | 3 | Magnitude / % / tip regexes | `concat!(r"…", r"…")` |
| `calc/units.rs` | 2 | Convert regexes | `concat!` |
| `calc/timezone.rs` | 5 | TZ query / predict regexes | `concat!` |
| `calc/datetime.rs` | 1 | Relative time regex | `concat!` |
| `calc/duration.rs` | 1 | Duration token regex | `concat!` |

Pattern semantics unchanged (`concat!` joins at compile time). Tests: **71 passed**, 2 ignored.

#### No other P1 issues

| Check | Result |
|--------|--------|
| rustfmt layout | Pass (all 15 files) |
| Tabs / trailing space / double blanks | Pass |
| Module order (`providers/mod.rs`) | Pass — `apps`, `calc`, `files`, `fx`, `http`, `translate` |
| Module order (`calc/mod.rs`) | Pass — alpha: battery…util |
| Naming | Pass |

#### Informational

| Item | Notes |
|------|--------|
| Module docs | `http.rs` has `//!`; most calc modules do not — soft consistency only |
| `files` module | Declared here but reviewed in Part 5 |

### Fixes applied (2026-07-23)

- [x] Wrap over-width regexes via `concat!` in math/units/timezone/datetime/duration
- [x] Wrap desktop-entry test fixture in `apps.rs`
- [x] `cargo fmt`; no Part 4 lines > 100; full test suite green

---

## Part 5 — Files Provider

**Focus:** file index, search, hot paths, live cache, files provider module.

| File | LOC | Status |
|------|-----|--------|
| `src/providers/files/search.rs` | 3082 | ⬜ |
| `src/providers/files/mod.rs` | 646 | ⬜ |
| `src/providers/files/index.rs` | 588 | ⬜ |
| `src/providers/files/live_cache.rs` | 187 | ⬜ |
| `src/providers/files/hot.rs` | 160 | ⬜ |

**Part status:** ⬜ pending  
**Reviewer notes:**

- 
- 

---

## How to use

1. Pick a part (recommended order: 1 → 5).
2. Review each file for style-guide issues (naming, formatting, module layout, comments, error handling patterns, etc.).
3. Flip `⬜` → `✅` per file and for the part when done.
4. Drop findings under **Reviewer notes**.
5. Update the summary table and overall count at the top.
6. Delete this file when the full review is finished.

## Status legend

| Symbol | Meaning |
|--------|---------|
| ⬜ | pending |
| 🔄 | in progress |
| ✅ | complete |
| ⛔ | blocked / skipped |
