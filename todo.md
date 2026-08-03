# Blink — TODO / product backlog

**Last updated:** 2026-08-03

Open product work and known gaps, grouped by area. Shipped items are archived in git history and [docs/archive/](docs/archive/). The local `feature.txt` at the repo root is a scratch wishlist; its items are folded in below so they survive for anyone who clones without that file.

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

| Priority | Item | Notes |
|----------|------|-------|
| P3 | Locale-aware default currency / timezone | From `feature.txt`: e.g. `10 usd` converts to the user’s local currency by default, chosen during setup/install |

## Architecture / hygiene

| Priority | Item | Notes |
|----------|------|-------|
| P1 | Split `config` mounts module | Only if `config.rs` grows again |
| P2 | Virtualized results list | n/a while result cap is 25 (`docs/OPTIMIZATION.md`) |

## Optimization / ops backlog

Kept in [docs/OPTIMIZATION.md](docs/OPTIMIZATION.md) (open items H2 / U1 / P1 / C1) — see also [docs/performance.md](docs/performance.md) for how to measure and bench.
