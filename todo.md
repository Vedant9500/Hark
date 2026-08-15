# Hark — TODO / product backlog

**Last updated:** 2026-08-11

Open product work and known gaps, grouped by area. Shipped items are archived in git history and [docs/archive/](docs/archive/). The local `feature.txt` at the repo root is a scratch wishlist; its items are folded in below so they survive for anyone who clones without that file.

**Pre-publish work (codebase cleanup → AUR):** see [docs/RELEASING.md](docs/RELEASING.md) — this is the roadmap for getting the repo clean/optimized and publishing to the AUR.

---

## Actions & system app control

| Priority | Item | Notes |
|----------|------|-------|
| P2 | Per-extension open overrides | Beyond coarse categories (e.g. `.svg` vs `.png`); only if category defaults feel too blunt |
| P2 | One-shot “Open with…” on results | Context or secondary action to pick once without changing the default |
| P2 | Detect system MIME default for display | Show “Loupe (system)” vs “Eye of GNOME (Hark)” so the user knows what’s active |
| P3 | System app control | From `feature.txt`: remove apps, check for updates |
| P3 | Richer per-result options menu | From `feature.txt`: uninstall / open app location / run-as-admin style options on the selected item, moving the settings entry elsewhere |

## File search

| Priority | Item | Notes |
|----------|------|-------|
| P2 | Multi-select drag-and-drop | Single-file DnD shipped; multi-select blocked until a multi-select UX lands |
| P3 | Reverse scoped form | `glassbox/docs optimization.md` without `in` — only if disambiguation is reliable |

## Localization

Location/locale-aware defaults so short queries assume the user's region (e.g. an Indian user typing `10 usd` gets `₹` automatically). Root feature user wants this "by default based on my location" — ideally inferred from system locale/`$LANG`/`geoclue`, configurable (auto vs explicit pick in settings).

| Priority | Item | Notes |
|----------|------|-------|
| P2 | Location-aware default currency | `10 usd` with no target → convert to local currency (e.g. INR for India); picked from locale, overridable in settings |
| P2 | Location-aware default timezone in datetime | `now`, `tomorrow`, relative `in N units`, unix→local → resolve to the user's timezone; predict/echo local zone |
| P2 | Location-aware default target timezone | Ambiguous `9pm` style timezone queries default to local zone; multi-zone where sensible |
| P2 | Location-aware date/locale formatting | Date/number formatting (DD-MM vs MM-DD, ₹/₹ separator, 24h vs 12h) follows region |
| P3 | Solid geo detection + settings override | Detect once (system locale + `geoclue`) and cache; explicit override in settings so the UX is deterministic |

## Raycast feature parity — gaps hark currently lacks

Researched against the Raycast manual (core + power + AI features). Hark already covers: fuzzy app/file search, globs & scoped search, calc/units/FX/timezone/battery math, translate, media preview, drag-and-drop, settings, action panel, theming (Caelestia). Below are the Raycast capabilities with **no hark equivalent** yet.

| Priority | Item | Notes |
|----------|------|-------|
| P1 | **Clipboard history** | Searchable record of copied text / images / links / colors; pin, edit, paste-as-plain-text, bulk-delete, search by type |
| P1 | **Snippets** | Reusable text templates with a keyword expansion; insert anywhere; dynamic placeholders (date, clipboard, `{calculator}`); import/export (Espanso, TextExpander) |
| P1 | **Quicklinks** | Saved shortcuts to URLs / files / folders / deeplinks, searchable in root; dynamic placeholders; browser-tab autofill |
| P1 | **Command palette / extension commands** | Searchable commands beyond apps/files (not just built-ins) — foundation Raycast is built on; see Extensions below |
| P1 | **Window management** | Resize/move focused window from keyboard (left/right half, custom sizes/positions, window layouts), with hotkeys |
| P2 | **System commands** | One-shot system actions: lock, sleep, restart, logout, volume up/down/mute, toggle light/dark, empty trash, show desktop, quit all apps, dismiss notifications |
| P2 | **Emoji / symbol picker** | Search emoji by name/meaning, paste into active input, custom keywords, skin tone, copy unicode, grid/pin favorites |
| P2 | **Script commands** | Bring-your-own Bash/Python/Node scripts as searchable commands or hotkey-bound; add a script directory |
| P2 | **Calculator extras: color conversion** | Type/paste a color (`#FF6B35`, `rgb`, `hsl`, `oklch`) → visual preview + copy in any format (hark math is number-only today) |
| P2 | **Favorites / pinning** | Pin any command/app/item to top of root search; reorder |
| P2 | **Aliases & per-command hotkeys** | Assign short keywords (`gc` → Chrome) and global hotkeys to commands; hark only has a few fixed keys |
| P3 | **Notes** | Lightweight markdown notes + search, accessible via hotkey (big feature, novel app inside app) |
| P3 | **Focus / distraction blocking** | Block or allow-list apps/websites for a timed session; categories |
| P3 | **URL handling** | Type a URL / domain → detect and offer open-in-browser (hark force-prefixes files only) |
| P3 | **Calendar events** | Upcoming events today at top of empty search; search calendar/schedule |
| P3 | **Contacts** | Search contacts and act on them |
| P3 | **Cloud sync / account** | Sync settings, snippets, quicklinks, notes across devices / teams |
| P3 | **Deeplinks** | `hark://command?...` links for shared commands / automation |
| P3 | **Hyper key / modifier chaining** | Use a single key (caps/ctrl) as a modifier across commands |
| P3 | **Hotkeys & aliases for *calculators* (today's calc results)** | Pin a fav math/unit query; broader alias system overlaps the row above |

## AI (optional long-term direction)

| Priority | Item | Notes |
|----------|------|-------|
| P3 | **AI chat / Quick AI** | Natural-language assistant from root search |
| P3 | **AI commands** | Turn prompts into one-press commands; selected-text actions |
| P3 | **Dictation** | Speech-to-text anywhere, cleaned up + pasted |
| P3 | **MCP support** | Connect local MCP servers as tools — greatest leverage; hark already targets a launcher niche that pairs well with agent tooling |
| P3 | **Extensibility platform** | Official extension/secondary-command API — the single biggest differentiator; without it, most `P1–P3` rows above must be hand-built into the binary |

## Calculation results — modern card layout migration

Unifies legacy text rows (`conversion: None` via `result_calc`) onto the Raycast-style `.hark-conv-card` (`conversion: Some`). Already on card: math expressions, unit/currency/timezone conversion, clock time ranges, translate.

| Priority | Item | Notes |
|----------|------|-------|
| P2 | Migrate `datetime.rs` to card | `now`, `utc`, `tomorrow`/`yesterday`, unix/epoch, relative `in N units`, days-until, date parsing, week number, day-of-year |
| P2 | Migrate `battery.rs` to card | `format_result`: `battery`, `power`, `charging`, charge %, ETA |
| P2 | Migrate `math.rs` natural/base to card | `try_natural` (`10% of 2k`, `tip 15% on 2k`) + `base_result` (`0xff`, `0b1010`) |
| P2 | Migrate `duration.rs` unit-duration to card | `try_duration_expr`: plain `10h 30min`, `2h + 30m` (clock range already done) |

## Master priority order

Single consolidated priority across the unit-math, cooking, financial, and quick-win sections below. Rationale: the unit-math engine is a shared foundation that gates several cooking/financial items, and the correctness bugs must land first because they return plausible-but-wrong answers. Execution is not strictly top-to-bottom — Tier 2 items are independent providers and parallelize while Tier 1 work proceeds.

| Tier | Item | Gated by |
|------|------|----------|
| T0 | `m`/`b`/`t` magnitude vs meter/byte/tonne collision | — |
| T0 | Duration provider steal (`50% of 1h 30min`, `in 1h 30min`) | — |
| T0 | `m` = minutes vs meters routing | — |
| T1 | Unit engine: × / ÷ by number (`200mb * 10`, `2km/5`) | T0 |
| T1 | Duration × / ÷ number (`2min 16 sec * 5`) | T0 |
| T1 | Fraction parsing (`1/2 cup`) | T0 |
| T1 | Same-dimension add/sub (`2m + 30cm`) | unit engine |
| T1 | `% of units` (`15% of 2km`) | unit engine |
| T1 | Compound units (`5km/2h`, fuel economy) | unit engine |
| T2 | Multi-unit relative datetime (`1h 30 min from now`) | — |
| T2 | Financial P1: interest, discount, split, GST | — |
| T2 | Financial P2: EMI, CAGR, rule-72, % change, hourly↔annual | — |
| T2 | Cooking density table + butter sticks | fractions |
| T2 | Cooking recipe scaling | unit engine |
| T2 | Cooking oven fan offset | — |
| T2 | Quick wins: base-conversion output, roman, BMI, steps→km | — |
| T2 | Quick wins P3: random, uuid, text utils, date-diff | — |

## Unit math & duration arithmetic gaps

Empirically probed against `CalcProvider::search` (2026-08-15). "NONE" = no result. Some queries **silently return wrong answers** (P0 rows) — worse than a missing result. The root cause: `expr.rs` resolves identifiers to `pi`/`e` only (`const_val`), so unit tokens abort the parse; `duration.rs` token regex supports only `+`/`-`; `datetime.rs` relative regex takes a single number+unit; `calc/mod.rs` routing order (duration → datetime → math) lets early providers steal queries.

Closes the `todo.txt` wishlist items: `200mb * 10`, `1h 30 min from now`, `2min 16 sec * 5`.

| Priority | Item | Notes |
|----------|------|-------|
| P0 | Fix single-letter magnitude/unit collision | `m` magnitude = million collides with meters: `100m / 2` → `50000000` (should be `50m`), `1m * 3` → `3000000`. Same for `b`=billion vs byte, `t`=trillion vs tonne |
| P0 | Stop duration provider stealing non-duration queries | `50% of 1h 30min` → "1h 30min" (should be `45min`); `in 1h 30min` returns a duration card, not a future timestamp (inconsistent with `in 2h`). Duration regex ignores leading junk and the bare `in ` prefix |
| P0 | `m` = minutes vs meters routing conflict | `100m` → "100 m from now" timestamp; `100m + 5m` → "1h 45min" duration. Meters get reinterpreted as minutes |
| P1 | Unit × number / ÷ number | `200mb * 10`, `2km / 5`, `1kg * 4`, `500g / 2`, `2km×3`, `2km ÷ 5`, `2km/5`. Output smart prefix (`2km/5` → `400m`) |
| P1 | Duration × number / ÷ number | `2min 16 sec * 5`, `1h 30min * 2`, `30min / 2`, `1h / 2`, `1.5h * 2` |
| P1 | Multi-unit relative datetime | `1h 30 min from now`, `1h 30 min ago`, `1h 30 min later`, `2 hours 30 minutes from now` (datetime handles single unit only today) |
| P2 | Same-dimension add/sub with mixed prefixes | `2m + 30cm`, `1km + 500m`, `5km + 2km`, `2km - 500m`, `200mb + 100mb`, `1gb - 512mb`. Convert both to base, then smart-prefix the result |
| P2 | Percentage of units | `15% of 2km`, `10% of 200mb`, `50% of 2h`, `tip 10% on 500g` |
| P2 | Bare unit values | `5km`, `500g`, `2kg` → show base (or common-target) value instead of no result |
| P3 | Compound units | `5km / 2h` (speed), `60km/h * 2`, `2km² / 2`, `4m2 * 3` |

## Cooking tools

Volume↔volume, temperature, and mass cooking conversions already work (`2 tbsp to tsp` → 6 tsp, `1 cup to ml`, `250 c to f`). Missing: density-based weight↔volume, fractions, and scaling. Verified gaps (probe 2026-08-15).

| Priority | Item | Notes |
|----------|------|-------|
| P1 | Ingredient weight↔volume | `100g flour in cups`, `2 cups sugar in g` — static density table (flour, sugar, butter, rice, oats, honey, milk, oil), same shape as `UNIT_ALIASES` |
| P1 | Fraction quantities | `1/2 cup to ml`, `1/3 cup sugar in g` — units/expr regex takes plain integers only today |
| P2 | Recipe scaling | `double 2 cups flour`, `scale 1.5x`, `4 servings to 8` |
| P2 | Butter sticks | `1 stick butter` → 113 g |
| P3 | Oven fan↔conventional | `fan 200c to conventional` (~15-20 °C lower) |

## Financial tools

New pattern provider in `calc/` (same shape as `math.rs`/`try_natural`), reusing `format_number`. Currency/FX and `tip` already exist.

| Priority | Item | Notes |
|----------|------|-------|
| P1 | Simple + compound interest | `interest 1000 at 5% for 3 years` → total + interest earned |
| P1 | Discount | `20% off 500` → 400 (only `tip 20% on 45` exists today) |
| P1 | Bill split | `split 45 4` → per person |
| P1 | GST / tax add | `gst 18% on 1000` → ₹1180 + GST amount (₹-friendly, matches existing lakh/crore support) |
| P2 | EMI | `emi 500000 8% 5 years` → monthly payment |
| P2 | CAGR / returns | `cagr 10000 to 20000 3 years` |
| P2 | Rule of 72 | `72 at 8%` → years to double |
| P2 | Percent change | `100 to 150` → +50% |
| P2 | Hourly↔annual | `25/hr to annual`, `60000/yr to hourly` |
| P3 | Inflation-adjusted value | `10000 in 2020 to now` |

## Other calculator quick wins

All pattern-based, cheap to add. Base conversion today only accepts `0x`/`0b` input (no output direction).

| Priority | Item | Notes |
|----------|------|-------|
| P1 | Base conversion output | `255 to hex`, `ff to dec`, `1010 to bin`, `o` octal |
| P2 | Roman numerals | `roman 1984` → MCMLXXXIV |
| P2 | BMI | `bmi 180cm 75kg` → 23.1 |
| P2 | Fuel economy | `12 km/l to mpg`, `30 mpg to l/100km` |
| P2 | Steps→distance | `10000 steps in km` (stride-length assumption) |
| P3 | Random | `dice`, `roll d20`, `coin` |
| P3 | UUID / password | `uuid`, `password 16` |
| P3 | Text utils | word count, slugify, case convert (`case snake Hello World`) |
| P3 | Date diff / age | `1998-03-15 to now`, `age 1998-03-15` |

## Architecture / hygiene

| Priority | Item | Notes |
|----------|------|-------|
| P1 | Split `config` mounts module | Only if `config.rs` grows again |
| P2 | Virtualized results list | n/a while result cap is 25 (`docs/OPTIMIZATION.md`) |

## Optimization / ops backlog

Kept in [docs/OPTIMIZATION.md](docs/OPTIMIZATION.md) (open items H2 / U1 / P1 / C1) — see also [docs/performance.md](docs/performance.md) for how to measure and bench.
