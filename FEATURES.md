# Blink — Complete Feature List

**Blink** is a Raycast-style launcher for Linux, built for **Hyprland / Wayland**, using **GTK4** (optional `gtk4-layer-shell` overlay).

| | |
|--|--|
| **Version** | 0.1.0 |
| **License** | MIT |
| **Platform** | Linux (x86_64; aarch64 when built for it) |

---

## 1. Core launcher UX

| Feature | Details |
|--------|---------|
| **Resident daemon** | `blink --daemon` keeps the process warm so the hotkey path avoids a cold GTK startup |
| **Instant toggle** | A second `blink` process talks to the daemon over a Unix socket (`$XDG_RUNTIME_DIR/blink.sock`) |
| **Layer-shell overlay** | Optional feature: exclusive keyboard grab, centered near the top of the focused Hyprland monitor via `hyprctl` |
| **Fallback window mode** | Modal window when layer-shell is unsupported |
| **Auto-hide on focus loss** | Closes shortly after losing focus (suppressed during drag-and-drop) |
| **Empty state** | Frecency “recent” items, then fills remaining slots with apps |
| **Merged ranking** | Apps + files + calc + settings command; score-first, kind as tie-break; top **25** results |
| **Usage / frecency** | Boosts apps and files you open often; stored in `~/.local/state/blink/usage.json` |
| **Footer action chips** | Context-aware labels: Copy Result / Open · Drag / Terminal shortcut |

---

## 2. Keyboard shortcuts

| Key | Action |
|-----|--------|
| `↑` / `↓` / `Tab` / `Shift+Tab` | Navigate results |
| `Enter` | Activate (open app/path, copy calc, open settings) |
| `Ctrl+C` | Copy calc/conversion result and hide |
| `Ctrl+Alt+Enter` | Open terminal at folder (or parent of file) |
| `Ctrl+,` | Open Blink Settings |
| `Esc` | Close launcher (or leave settings) |
| Settings: `↑`/`↓` / `j`/`k` / `Home`/`End` | Cycle settings categories |

---

## 3. App search

- Scans FreeDesktop `.desktop` files from standard application directories
- Fuzzy match (Skim) with score bands: **exact → prefix → contains → fuzzy**
- Filters `NoDisplay` entries and empty exec/name
- Launch with terminal flag support (`Terminal=true` desktop entries)
- Drag `.desktop` path for drag-and-drop
- Background reload on start and every **30 minutes**

---

## 4. File / folder search

### Query modes

| Mode | Example |
|------|---------|
| Free text (≥2 chars) | `doc`, `glassbox` |
| File force prefixes | `f query`, `file query`, `folder query` |
| Path browser | `~/Doc`, `/mnt/...`, `./src` |
| Globs | `*.md`, `foo/**/*.rs`, `?at` |
| Extension shorthand | `.md`, `.png` |
| Scoped search | `optimization.md in glassbox`, `*.rs under ~/Projects` (also `within` / `inside`) |

### Indexing

| Behavior | Details |
|----------|---------|
| **Roots** | Home, external mounts (NTFS / exFAT / vfat via `findmnt`), extra folders |
| **Depth** | Configurable 1–6 (default **2**); deep roots always walked to depth **6** |
| **Cap** | **100,000** indexed paths |
| **Disk cache** | `~/.cache/blink/file-index.json` (TTL 30 min, fingerprint invalidation) |
| **Excludes** | Defaults include `.git`, `node_modules`, `target`, caches, Trash, browser/Steam junk, and more |
| **Ranking** | Strong path matches demote weak app fuzzy hits |
| **Async live deep search** | Budgeted walk when the index is weak; live cache keeps retypes instant |
| **Auto-promote** | Opening a deep file can pin a nearby project root (`.git`, `Cargo.toml`, `package.json`, …) |
| **Manual pins** | Pin / unpin deep roots in Settings (cap **32**) |

### Actions

- Open with system handler (`xdg-open`) or a Blink per-category default app from Settings
- Open a terminal at the path
- Drag file/folder (and apps) into other apps (Telegram, Nautilus, browsers, etc.)

---

## 5. Calculator & conversions

### Math

- Expressions: `2+2`, `sqrt(144)`, `sin` / `cos` / `tan`, `log`, `pi` / `π`, `^` / `**`, factorials
- Natural language: `15% of 80`, `tip 20% on 45`
- Bases: `0xFF`, `0b1010` → decimal / hex / binary

### Unit conversion (with partial prediction)

**Categories:** mass, length, volume, temperature, speed, data, time, area

Examples:

- `100 km to mi`
- `32 f to c`
- `1 gb to mb`
- `10kg to pou` → predicts **pounds**

### Currency (live FX)

- ECB-based rates, **12 hour** cache
- Currency symbols: `$`, `€`, `£`, `¥`, `₹`, and more
- Examples: `100 usd to eur`, `$50 to inr`
- Partial target prediction

### Time zones

- `now in tokyo`, `time in london`
- `12pm here in london`, `4pm est to pst`, `16:00 cet to ist`
- City / abbreviation resolve + prediction

### Power / battery (Linux sysfs)

- Queries: `battery`, `power`, `ac power`, `charging`, `on battery`, `plugged in`, …
- Shows **On AC power** vs **On battery**, charge %, status, optional W draw and ETA
- Read only when you search (no background poll)

### Date / time

- `now`, `time`, `date`, `today`, `utc`, `tomorrow`, `yesterday`
- Unix: `unix`, bare epoch, `unix 1710000000`
- Relative: `in 3 days`, `2 hours ago`, `1 week from now`

### Duration arithmetic

- Multi-unit expressions: `10h 30min`, `2d + 3h - 15m`

### Conversion UI

- Dual-panel “conversion card” (left → right) for units, FX, time zones, and math

---

## 6. Commands & settings

- Type `settings` / `preferences` / `index` / `config` → **Blink Settings**

### Settings categories

1. **Indexing** — home toggle, mounts, depth, rebuild now, status
2. **Extra folders** — custom search roots
3. **Exclusions** — names always skipped
4. **Default apps** — per-category open apps (images, video, audio, PDF, markdown, text, documents, archives); Blink-only, does not change system MIME
5. **Display** — path style: **Label** vs **Drive** (`~/…` / `Windows C:…` style)
6. **Appearance** — opacity, accent colour, font scale, icon size/style, corner radius

Also:

- Deep-root management (pin / unpin)
- Config file: `~/.config/blink/config.json`

---

## 7. Preview pane

- Side panel (~**280px**) for the selected result
- **Images:** scaled decode off the main thread, FreeDesktop thumbnail fast path, LRU texture cache (cap **24**), mtime + size fingerprint, latest-wins decode; generate missing thumbs into `~/.cache/thumbnails`
- Media typing: image / video / audio / document / archive / code / other
- **Video:** first-frame preview via `ffmpeg` (falls back to icon if tool/file fails)
- **PDF:** first-page preview via `pdftoppm` (poppler); other documents stay icon + metadata
- Audio: icon + metadata
- Drag-and-drop from the preview picture as well as list rows

---

## 8. Drag and drop

- Real filesystem paths (`GdkFileList` / URI list), not in-memory pixels
- Sources: result rows **and** preview
- Wayland / layer-shell aware: suppress auto-hide; release exclusive keyboard during drag
- Actions offered: **COPY | MOVE | ASK** (never deletes on MOVE)

---

## 9. Theming

- Default Tokyo Night–style palette
- Live theme from **Caelestia** `scheme.json` (`~/.local/state/caelestia/scheme.json`)
- **Translate-on-paste:** CJK auto / `tr …` / `tr en zh …` direction → conversion card + copy; Google∥MyMemory or LibreTranslate; `source:auto` when supported; disk+fail cache; async + gen cancel; master kill switch in Settings → Tools
- **Appearance settings:** panel opacity, accent override, font scale, icon size, symbolic icons, corner radius
- Hot-reloads on file change (or 2s poll fallback)
- Custom GTK CSS for launcher chrome

---

## 10. Performance & ops tooling

| CLI | Purpose |
|-----|---------|
| `blink --daemon` | Resident process |
| `blink` | Toggle (or start if no daemon) |
| `blink --search "q"` | Headless search debug (index + optional deep) |
| `blink --bench` | Latency (median / p95), index rebuild, RSS / CPU, optional daemon / GPU stats |

Other internals:

- Background FX warm-up
- App + file reindex every **30 minutes**
- Isolated provider benches (apps / files / calc)
- Related docs: [docs/performance.md](docs/performance.md), [docs/preview-optimization.md](docs/preview-optimization.md), [OPTIMIZATION.md](OPTIMIZATION.md)

---

## 11. Packaging & install

| Package | Audience |
|---------|----------|
| One-line installer | Anyone with curl (GitHub release) |
| Portable `.tar.gz` | Offline / USB share (`scripts/package-release.sh`) |
| Source package | Build yourself (`scripts/package-source.sh`) |
| User install | Under `~/.local` + uninstall script |
| AUR `PKGBUILD` | Arch / Endeavour / Cachy |
| `.deb` | Debian / Ubuntu via `cargo-deb` |

Also ships:

- Desktop entry + SVG icon
- Optional login autostart for the daemon

### Runtime requirements

- **GTK 4**
- **Recommended on Hyprland:** `gtk4-layer-shell` for true overlay mode

---

## 12. Hyprland integration

```conf
exec-once = blink --daemon
bind = ALT, A, exec, blink
```

Layer-shell namespace: `blink`  
Positions near ~20% from the top of the focused monitor.

---

## 13. Architecture

| Module | Role |
|--------|------|
| `engine` | Merge providers, ranking, execute, deep-root promote |
| `providers/apps` | Desktop apps |
| `providers/files` | Index, search, globs, live deep, open |
| `providers/calc/*` | Math, units, FX, TZ, datetime, duration |
| `providers/fx` | Currency rates store |
| `ui/*` | Window, rows, preview, DnD, settings, footer |
| `theme` | Caelestia + CSS |
| `config` | Index settings + mounts |
| `usage` | Frecency |
| `ipc` | Unix socket toggle |

---

## 14. Known gaps / planned

Tracked in [todo.md](todo.md). Not fully shipped yet:

- Multi-select drag-and-drop
- ~~Generate missing FreeDesktop thumbnails~~ **done**
- ~~Video first-frame / PDF page preview~~ **done**
- Per-extension open overrides
- One-shot “Open with…” on results (vs Settings defaults)

Shipped recently:

- Default apps per file type in Settings (`open_with` categories → desktop id; Blink-only, not system MIME)

---

## One-line summary

**Blink is a fast, daemonized Hyprland launcher that fuzzy-searches apps and files (with globs, scoped paths, and deep live walks), does Raycast-style math / units / currency / timezone conversions, previews media, supports file drag-and-drop, and has a built-in settings panel for indexing and default open apps — all in a themed GTK4 overlay.**
