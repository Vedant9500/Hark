# Style Guide Review Tracker (Temporary)

Temporary tracking file for a 5-part style guide review of the Blink codebase.
Scope: `src/` only (~21.7k LOC). Build artifacts, packaging, and docs are out of scope.

| Part | Name | Approx. LOC | Status |
|------|------|-------------|--------|
| 1 | Core & App Shell | ~3,530 | ✅ complete |
| 2 | UI Shell | ~3,790 | ⬜ pending |
| 3 | UI Features & Theme | ~4,760 | ⬜ pending |
| 4 | Providers (Apps, Calc, FX, HTTP, Translate) | ~5,030 | ⬜ pending |
| 5 | Files Provider | ~4,660 | ⬜ pending |

**Overall status:** 1 / 5 complete

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
| `src/ui/mod.rs` | 2219 | ⬜ |
| `src/ui/rows.rs` | 490 | ⬜ |
| `src/ui/dnd.rs` | 356 | ⬜ |
| `src/ui/action_panel.rs` | 249 | ⬜ |
| `src/ui/thumbnails.rs` | 225 | ⬜ |
| `src/ui/style.css` | 166 | ⬜ |
| `src/ui/footer.rs` | 80 | ⬜ |

**Part status:** ⬜ pending  
**Reviewer notes:**

- 
- 

---

## Part 3 — UI Features & Theme

**Focus:** settings UI, preview pane, open-with dialog, theme generation / CSS injection.

| File | LOC | Status |
|------|-----|--------|
| `src/ui/settings.rs` | 2221 | ⬜ |
| `src/ui/preview.rs` | 1084 | ⬜ |
| `src/theme/css.rs` | 936 | ⬜ |
| `src/ui/open_with.rs` | 345 | ⬜ |
| `src/theme/mod.rs` | 177 | ⬜ |

**Part status:** ⬜ pending  
**Reviewer notes:**

- 
- 

---

## Part 4 — Providers (Apps, Calc, FX, HTTP, Translate)

**Focus:** provider trait / registry, app launcher, calculator submodules, FX rates, HTTP helper, translate.

| File | LOC | Status |
|------|-----|--------|
| `src/providers/mod.rs` | 327 | ⬜ |
| `src/providers/apps.rs` | 434 | ⬜ |
| `src/providers/fx.rs` | 264 | ⬜ |
| `src/providers/http.rs` | 78 | ⬜ |
| `src/providers/translate.rs` | 1016 | ⬜ |
| `src/providers/calc/mod.rs` | 148 | ⬜ |
| `src/providers/calc/expr.rs` | 386 | ⬜ |
| `src/providers/calc/math.rs` | 147 | ⬜ |
| `src/providers/calc/units.rs` | 548 | ⬜ |
| `src/providers/calc/timezone.rs` | 743 | ⬜ |
| `src/providers/calc/datetime.rs` | 175 | ⬜ |
| `src/providers/calc/duration.rs` | 111 | ⬜ |
| `src/providers/calc/currency.rs` | 130 | ⬜ |
| `src/providers/calc/battery.rs` | 448 | ⬜ |
| `src/providers/calc/util.rs` | 71 | ⬜ |

**Part status:** ⬜ pending  
**Reviewer notes:**

- 
- 

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
