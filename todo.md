# Blink — TODO / product backlog

**Last updated:** 2026-08-11

Open product work and known gaps, grouped by area. Shipped items are archived in git history and [docs/archive/](docs/archive/). The local `feature.txt` at the repo root is a scratch wishlist; its items are folded in below so they survive for anyone who clones without that file.

**Pre-publish work (codebase cleanup → AUR):** see [docs/RELEASING.md](docs/RELEASING.md) — this is the roadmap for getting the repo clean/optimized and publishing to the AUR.

---

## Actions & system app control

| Priority | Item | Notes |
|----------|------|-------|
| P2 | Per-extension open overrides | Beyond coarse categories (e.g. `.svg` vs `.png`); only if category defaults feel too blunt |
| P2 | One-shot “Open with…” on results | Context or secondary action to pick once without changing the default |
| P2 | Detect system MIME default for display | Show “Loupe (system)” vs “Eye of GNOME (Blink)” so the user knows what’s active |
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

## Raycast feature parity — gaps blink currently lacks

Researched against the Raycast manual (core + power + AI features). Blink already covers: fuzzy app/file search, globs & scoped search, calc/units/FX/timezone/battery math, translate, media preview, drag-and-drop, settings, action panel, theming (Caelestia). Below are the Raycast capabilities with **no blink equivalent** yet.

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
| P2 | **Calculator extras: color conversion** | Type/paste a color (`#FF6B35`, `rgb`, `hsl`, `oklch`) → visual preview + copy in any format (blink math is number-only today) |
| P2 | **Favorites / pinning** | Pin any command/app/item to top of root search; reorder |
| P2 | **Aliases & per-command hotkeys** | Assign short keywords (`gc` → Chrome) and global hotkeys to commands; blink only has a few fixed keys |
| P3 | **Notes** | Lightweight markdown notes + search, accessible via hotkey (big feature, novel app inside app) |
| P3 | **Focus / distraction blocking** | Block or allow-list apps/websites for a timed session; categories |
| P3 | **URL handling** | Type a URL / domain → detect and offer open-in-browser (blink force-prefixes files only) |
| P3 | **Calendar events** | Upcoming events today at top of empty search; search calendar/schedule |
| P3 | **Contacts** | Search contacts and act on them |
| P3 | **Cloud sync / account** | Sync settings, snippets, quicklinks, notes across devices / teams |
| P3 | **Deeplinks** | `blink://command?...` links for shared commands / automation |
| P3 | **Hyper key / modifier chaining** | Use a single key (caps/ctrl) as a modifier across commands |
| P3 | **Hotkeys & aliases for *calculators* (today's calc results)** | Pin a fav math/unit query; broader alias system overlaps the row above |

## AI (optional long-term direction)

| Priority | Item | Notes |
|----------|------|-------|
| P3 | **AI chat / Quick AI** | Natural-language assistant from root search |
| P3 | **AI commands** | Turn prompts into one-press commands; selected-text actions |
| P3 | **Dictation** | Speech-to-text anywhere, cleaned up + pasted |
| P3 | **MCP support** | Connect local MCP servers as tools — greatest leverage; blink already targets a launcher niche that pairs well with agent tooling |
| P3 | **Extensibility platform** | Official extension/secondary-command API — the single biggest differentiator; without it, most `P1–P3` rows above must be hand-built into the binary |

## Calculation results — modern card layout migration

Unifies legacy text rows (`conversion: None` via `result_calc`) onto the Raycast-style `.blink-conv-card` (`conversion: Some`). Already on card: math expressions, unit/currency/timezone conversion, clock time ranges, translate.

| Priority | Item | Notes |
|----------|------|-------|
| P2 | Migrate `datetime.rs` to card | `now`, `utc`, `tomorrow`/`yesterday`, unix/epoch, relative `in N units`, days-until, date parsing, week number, day-of-year |
| P2 | Migrate `battery.rs` to card | `format_result`: `battery`, `power`, `charging`, charge %, ETA |
| P2 | Migrate `math.rs` natural/base to card | `try_natural` (`10% of 2k`, `tip 15% on 2k`) + `base_result` (`0xff`, `0b1010`) |
| P2 | Migrate `duration.rs` unit-duration to card | `try_duration_expr`: plain `10h 30min`, `2h + 30m` (clock range already done) |

## Architecture / hygiene

| Priority | Item | Notes |
|----------|------|-------|
| P1 | Split `config` mounts module | Only if `config.rs` grows again |
| P2 | Virtualized results list | n/a while result cap is 25 (`docs/OPTIMIZATION.md`) |

## Optimization / ops backlog

Kept in [docs/OPTIMIZATION.md](docs/OPTIMIZATION.md) (open items H2 / U1 / P1 / C1) — see also [docs/performance.md](docs/performance.md) for how to measure and bench.
