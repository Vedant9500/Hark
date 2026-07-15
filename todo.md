# Blink — TODO

**Last updated:** 2026-07-15

---

## Preview pane gaps (from optimization review)

Source: `src/ui/preview.rs` · tracker: `docs/preview-optimization.md`

| Priority | Item | Why | Status |
|----------|------|-----|--------|
| **P0** | Single in-flight decode job (“latest selection wins”) | Debounce stops *scheduling* storms, but started workers always finish; slow arrowing can stack concurrent `std::thread` decodes with no cancel/pool | **done** — `worker_busy` + `inflight` queue; at most one decode thread; completion pumps only latest settled request |
| **P0** | Cache key includes mtime + size | Path-only LRU can show a stale texture after an in-place edit until eviction | **done** — `FileFp { len, mtime_ns }` on every cache entry; mismatch evicts |
| **P1** | Avoid double open on full decode | `Pixbuf::file_info` + `from_file_at_scale` both open the file; hurts cold HDD/network mounts | **mitigated** — both stay off main; `file_info` is header-only for native dims label; single expensive scaled open. Full single-open would drop native WxH for large images |
| **P1** | Deduplicate LRU insert path | `insert_cache` exists but async completion reimplements map/order eviction | **done** — sole insert path is `PreviewPanel::insert_cache` |
| **P2** | Off-main FreeDesktop thumb load | Thumb path is intentional + cheap, but still sync on UI thread (`canonicalize` + MD5 + `Texture::from_filename`) under rapid select | **done** — thumb resolve + load on worker; main only stats + debounce |
| **P2** | Optional: drop extra pixel→Pixbuf hop | Worker already copies pixels; main rebuilds Pixbuf then Texture — fine at 496px, short-lived ~2× peak | **partial** — `Bytes::from_owned` avoids a second buffer clone on main; Pixbuf hop remains (GTK API) |
| later | Generate missing FreeDesktop thumbs | Better first-hit for images without `~/.cache/thumbnails` entry (doc item 6) | not this pass |
| later | Video first-frame / PDF page | Non-goals of current preview pass; icon-only today | not this pass |

### Done (keep for context)

- [x] LRU texture cache (cap 24)
- [x] 45 ms selection debounce
- [x] Off-main-thread scaled decode (`from_file_at_scale` ≤496px)
- [x] FreeDesktop thumbnail fast path + cache
- [x] Generation token for stale UI apply
- [x] Search/index no regression (`blink --bench`)
- [x] Single in-flight decode (latest wins)
- [x] Cache fingerprint (mtime + size)
- [x] Shared `insert_cache` for all writers
- [x] Off-main FreeDesktop thumb resolve + load

---

## Drag & drop files into other apps

**Ask:** search an image in Blink → drag it into Telegram / WhatsApp / browser / file manager.

**Today:** `src/ui/dnd.rs` + row/preview wiring. Real filesystem path payload (`GdkFileList` / `GFile` / `text/uri-list`); layer-shell focus-loss suppressed + exclusive keyboard released during drag; Hyprland MOVE preferred advertised but never deletes.

| Priority | Item | Notes | Status |
|----------|------|-------|--------|
| **P1** | DnD file URI from result row | `attach_path_drag` on rows via `Action::drag_path()` (`OpenPath` / `.desktop` apps) | **done** — `src/ui/rows.rs` + `dnd.rs` |
| **P1** | DnD from preview picture | `PathDragBinding` on preview panel when showing a file path | **done** — `src/ui/preview.rs` |
| **P1** | Layer-shell / focus-loss safety | Suppress auto-hide; `KeyboardMode::OnDemand` mid-drag; 350 ms post-drop grace | **done** — `DragSession` + Hyprland fix commit |
| **P2** | Multi-select DnD | Only if multi-select lands; single-file first | blocked |
| **P2** | Drag icon / thumbnail hotspot | Theme mime icon always; FreeDesktop thumb when present for images | **done** — `set_drag_icon` prefers `~/.cache/thumbnails` then icon theme |

**Feasibility:** yes on GTK4/Wayland for **file paths** (not in-memory pixels as the primary payload). Telegram Desktop, browsers, Nautilus, etc. accept `file://` URI / file-list drops. Electron/WhatsApp desktop usually do too when they implement standard file DnD.

**Caveats:**
- Layer-shell: handled (ignore focus-loss + keyboard mode toggle). Still worth retesting after compositor updates.
- Prefer offering a real filesystem path (already true for file results), not a temp copy, unless the source is virtual.
- Image “as bitmap” (`image/png` content) is a separate, less useful path for chat apps — they want the file.
- Drag icon uses FreeDesktop thumbs when available (sync, small file); does not re-decode the full image or borrow the preview LRU (row drag often has no preview texture).

---

## Default apps for file types (settings)

**Ask:** in Settings, pick which app opens each common file kind from Blink (images → image viewer, markdown/txt/pdf → editor/viewer, audio/video → media player), instead of always using the system generic handler (`xdg-open` / MIME default only).

**Today:** `Action::OpenPath` → `open_path_with` + `BlinkConfig.open_with` (per-category desktop id; empty = `xdg-open`). Settings → **Default apps**.

| Priority | Item | Notes | Status |
|----------|------|-------|--------|
| **P1** | Config: per-category default app | `OpenWithConfig` on `BlinkConfig` (`images`, `video`, `audio`, `pdf`, `markdown`, `text`, `documents`, `archives`); desktop-id; empty = system default | **done** |
| **P1** | Settings page/section “Default apps” | Nav page `defaults` — row per category: label + current app + Choose… / System | **done** |
| **P1** | App picker UI | Modal list of installed GUI `.desktop` apps (`AppProvider::list_for_picker`); filter + system default row | **done** |
| **P1** | Open path honors overrides | `FileOpenCategory::from_path` → `launch_with_desktop_id` (`gio::DesktopAppInfo`); fallback `xdg-open` | **done** |
| **P2** | Per-extension overrides | Beyond coarse categories (e.g. `.svg` vs `.png`); only if category defaults feel too blunt | pending |
| **P2** | “Open with…” on result / preview | Context or secondary action to pick once without changing default | pending |
| **P2** | Detect system MIME default for display | Show “Loupe (system)” vs “Eye of GNOME (Blink)” so user knows what’s active | pending |

**Categories (v1 target):**

| Category | Example extensions | Typical app |
|----------|--------------------|-------------|
| Images | png, jpg, webp, gif, svg | image viewer |
| Video | mp4, mkv, webm, mov | video player |
| Audio | mp3, flac, ogg, wav | music player |
| PDF | pdf | PDF viewer |
| Markdown | md, markdown | editor / MD viewer |
| Plain text | txt, log, conf | text editor |
| Documents | odt, docx, rtf | office suite |
| Archives | zip, tar, 7z (optional) | archive manager |

**Implementation sketch:**

1. `config.rs` — serde map `HashMap<String, String>` or typed struct of optional desktop-ids; persist under `~/.config/blink/`.
2. `ui/settings.rs` — new page `"defaults"` in nav + card rows; write through `ConfigStore` like folders/exclusions.
3. `providers/files.rs` (`open_path`) — resolve category from extension (share helpers with `MediaKind` / preview mime); if override set, `gio::AppInfo` / `gtk::gio` launch or `exec path`; else current behavior.
4. Do **not** change global XDG MIME defaults unless user opts in later (“Also set as system default”).

**Caveats:**

- Flatpak/Snap apps need correct desktop-id + portal-friendly launch.
- Terminal apps (`Terminal=true`) should open via terminal wrapper or be filtered out of picker.
- Keep activate path fast: no full desktop rescan on every Enter (cache app list).

---

## Path / glob search

**Ask:** type patterns like `blink/docs/*.md` or `src/**/*.rs` and get matching files (scoped by path segments + extension/glob), not empty results.

**Today:** only absolute/`~/` path *completions* (`path_completions`); free-text is name/fuzzy only — no `*`, no multi-segment path filter. Query `blink/docs/*.md` matches nothing useful.

| Priority | Item | Notes | Status |
|----------|------|-------|--------|
| **P1** | Detect path-like / glob queries | If query contains `/` or `*`/`?` (and isn’t pure calc), route to path-glob search instead of app fuzzy | **done** — `is_path_glob_query` + engine `force_files` |
| **P1** | Segment filter on index | Split on `/`; require each non-glob segment to appear in order in `path_lower` (e.g. `blink` then `docs`) | **done** — `find_path_segment` component-aware |
| **P1** | Glob on final component | Support `*`, `?`, and `*.md` / `*foo*` on file/dir name (and optionally full relative path with `**` later) | **done** — custom `glob_match` (no extra crate) |
| **P1** | Extension shorthand | `*.md`, `*.rs`, `.png` as “any path ending with that ext” when no folder segments | **done** — `.md` → `*.md` |
| **P2** | `**` recursive glob | `src/**/*.ts` across index; keep result cap (25) + strong scoring | pending — `*` already spans; `**` as segment not special-cased |
| **P2** | Absolute/mount globs | `/home/…/*.pdf`, `D:/Glassbox/**/*.md` via index + live dir when under one folder | **partial** — `~/…/*.ext` live `read_dir` + index supplement |
| **P2** | In-folder live glob | When pattern resolves under an existing dir, `read_dir` / walk that tree (fresher than index) | **partial** — absolute/`~/` final-component only; relative still index-only |

**v1 query examples:**

| Query | Meaning |
|-------|---------|
| `blink/docs/*.md` | paths containing `…/blink/…/docs/…` with name matching `*.md` |
| `*.rs` | any indexed file ending in `.rs` |
| `glassbox/src/` | folders/files under path segments glassbox + src |
| `todo.md` | unchanged exact/fuzzy name search |
| `optimization.md in glassbox/docs` | see **Scoped “in” search** below |

**Implementation sketch:**

1. `search.rs` — if `q.contains('/') \|\| q.contains('*') \|\| q.contains('?')`, call `search_glob(...)` before/instead of name-only pass.
2. Parse: strip optional `f `/`file ` prefix; split segments; last segment may be glob pattern (`globset` or small custom `*`/`?` matcher).
3. Score: longer path agreement + shallower depth win; exact segment hits ≫ loose.
4. Engine: treat glob queries like `force_files` (skip apps) so Chrome doesn’t appear for `*.md`.
5. Optional crate: `globset` / `glob` — only if custom matcher gets messy; prefer zero deps first.

**Caveats:**

- Don’t treat `2 * 3` calc as glob (calc already short-circuits when it hits).
- `*` alone is useless — require ≥1 literal char or a path segment.
- Cap work (early exit at FILE_RESULT_LIMIT strong hits) so globs stay instant on big indexes.

---

## Scoped “in” search (filename in folder)

**Ask:** natural scoped queries so you name the file first and the folder second:

- `optimization.md in glassbox/docs`
- `optimization.md in glassbox/`
- `main.rs in blink`
- `*.md in docs/`

**Today:** free-text is a single bag of tokens for fuzzy/name match; the word `in` is not special; path segments aren’t a scope filter.

| Priority | Item | Notes | Status |
|----------|------|-------|--------|
| **P1** | Parse `… in …` | If query matches `^(?P<name>.+?)\s+in\s+(?P<scope>.+)$` (case-insensitive `in`), split into **name/glob** + **scope path**; require non-empty both sides | **done** — `parse_scoped_query` + aliases; disambiguation by ext/glob/path-like/known folder |
| **P1** | Scope = path segments | Scope `glassbox/docs` or `glassbox/` → require those segments in order in `path_lower` (same as path-glob segment filter); trailing `/` allowed | **done** — reuses `search_glob` / `find_path_segment`; `~/`/`/` expand via `expand_user` |
| **P1** | Name = exact / prefix / glob | Left side: exact filename, prefix, or `*.md` / `opt*` — not full-query fuzzy against apps | **done** — name via `name_matches_pat` / glob scoring |
| **P1** | Files-only mode | `in` queries force file/folder provider (skip apps/commands) | **done** — `is_scoped_file_query` / `FileProvider::is_scoped_query` → engine `force_files` |
| **P1** | Live deep walk when scoped | Scope narrows roots a lot → safe to live-walk under matched `glassbox` dirs past index depth (pairs with on-demand deep search) | **done** — `maybe_deep_for_scoped` (abs root / segment roots + pins) |
| **P2** | Aliases | `within`, `under`, `inside` same as `in`; optional `from` | **done** — `in` / `within` / `under` / `inside` (`from` still optional) |
| **P2** | Completions UX | After typing ` in `, suggest top folders from index as soft hints (optional subtitle “scoped to …”) | pending |
| **P3** | Reverse form | `glassbox/docs optimization.md` without `in` only if we can disambiguate; **not** v1 | pending |

**Examples:**

| Query | Behavior |
|-------|----------|
| `optimization.md in glassbox/docs` | name ≈ `optimization.md` AND path has `…/glassbox/…/docs/…` |
| `optimization.md in glassbox/` | name match under any path containing `glassbox` |
| `*.md in glassbox/docs` | glob name under scope |
| `in glassbox` | invalid / ignore parse — fall back to normal search (`in` alone useless) |
| `login in firefox` | careful: may be app intent; only treat as scoped if **right side looks path-like** (has `/`, `~`, or matches an indexed folder name) **or** left side looks like a filename (has `.ext`) |

**Disambiguation (important):**

- Prefer scoped parse when left has a file extension (`.md`, `.rs`, …) **or** right has `/`/`~` **or** right matches a known folder in index.
- Otherwise keep normal multi-provider search (`login in firefox` shouldn’t become a weird path filter).

**Implementation sketch:**

1. Tiny parser in `files/search.rs` (or `query.rs`): `parse_scoped(q) -> Option<(name, scope)>`.
2. Reuse segment filter + name/glob scorer from path-glob work.
3. If index scope miss / weak → live walk only under dirs matching scope segments (cheap).
4. Engine: if scoped parse hits, `force_files` + skip apps.

**Caveats:**

- Don’t steal calc/commands (`convert 10 in to cm` style) — calc already owns unit queries; run calc first; only parse `in` for file search if calc misses.
- Unicode/case: scope and name compared lowercased like the rest of file search.

---

## On-demand deep search (beyond index depth)

**Ask:** default index is shallow (`max_depth` **2**) so startup stays fast. Finding a specific file at depth 3–6 (e.g. `~/dev/glassbox/src/ui/foo.rs`) should still work via a **live walk** when the index can’t answer — not by permanently indexing everything deep.

**Today:** walk stops at `cfg.index.max_depth` (clamped 1..=6, default 2). Search only hits `IndexedPath` + absolute/`~/` path completions. Deep files never enter the index → never match free-text / path-segment queries.

**Why live walk is fine:** one project tree with hard skips is microseconds–low ms. Junk trees are the problem — already skipped at index time and must be skipped live too:

| Always skip (reuse `should_always_skip` / excludes) | Why |
|------------------------------------------------------|-----|
| `node_modules`, `.pnpm-store`, `.npm`, `.yarn` | thousands of irrelevant files |
| `target`, `dist`, `build`, `out`, `.next`, `.nuxt`, `.turbo` | build artifacts |
| `__pycache__`, `.mypy_cache`, `.pytest_cache`, `.tox`, `venv`, `.venv` | Python noise |
| `.git`, `.svn`, `.hg`, `.cache`, `.cargo`, `.rustup` | VCS / tool caches |
| browser profile dirs, recycle bins, etc. | same as index |

| Priority | Item | Notes | Status |
|----------|------|-------|--------|
| **P1** | Trigger when index is weak | After index search, if no strong hit (score ≥ 30k) **and** query looks specific → live deep search. Skip bare short words, bare `*.md`/`.rs`, etc. | done |
| **P1** | Scope the walk | (1) absolute/`~/` glob → walk that dir; (2) path segments → roots = index folders matching first/last segments; (3) name+ext → high-value shallow roots only — **never** whole `$HOME`/`/` | done |
| **P1** | Deep walk with same ignores | `WalkDir` max_depth 6; `should_descend`/`should_skip_entry` shared with indexer; visit cap 8k + 40ms wall budget | done |
| **P1** | Merge results | Live hits as `path:…`; score −500 vs equivalent index so index still wins when present; dedupe + top-25 | done |
| **P2** | Async / progressive | If walk may exceed ~16ms, run off UI thread and stream/update results (reuse index progress patterns lightly) so typing stays instant | **done** — UI uses `DeepMode::Skip` on main thread; `search_files_deep` (`DeepMode::Async`, 40k visits / 200ms) on worker; `deep_gen` drops stale applies |
| **P2** | Ephemeral cache | Remember last live-walk hits for a few minutes (query → paths) so retyping doesn’t re-walk; don’t write into main `file-index.json` | **done** — `live_cache.rs`: query-key TTL 5m, LRU 64; deep modes put full set; Skip merges cache hits |
| **P2** | Pin / promote | Optional: “always index this folder deeper” from result or settings (raise depth for one root only) | **done** — `index.deep_roots` always depth-6 at index time + preferred live roots; Settings “Deep roots”; auto-promote project folder on open (git/Cargo/package markers); cap 32 |
| **P3** | Adaptive depth | Settings: global depth 2 default; per-root override; or “deep search on demand only” (recommended) | partial — global depth stays 2; deep roots are the per-root override; pure on-demand remains default |

**Trigger examples:**

| Query | Index (depth 2) | Live walk |
|-------|-----------------|-----------|
| `foo.rs` buried at depth 5 | miss | walk candidate project folders for name `foo.rs` |
| `glassbox/src/main.rs` | maybe only `glassbox/` folder | open `…/glassbox`, walk to `src/main.rs` |
| `blink/docs/*.md` | shallow miss | scoped glob walk under matched `blink` dirs |
| `fire` | many apps / shallow hits | **don’t** live-walk (too broad) |

**Implementation sketch:**

1. Share skip helpers: move `should_always_skip` / `should_skip_entry` to something both `index.rs` and live search call (or `pub(crate)` on index).
2. `search_live(query, roots, max_depth, budget)` in `files/search.rs` (or `files/live.rs`).
3. Engine / file provider: `results = index_search(...); if needs_deep(&results, q) { results.extend(live...) }`.
4. Budget: stop when 25 strong matches **or** visited cap **or** time budget; never block typing for hundreds of ms.
5. Keep global index depth at 2 — deep search is a **query-time** feature, not a bigger default index.

**Caveats:**

- Broad 2–3 letter queries must not trigger full-home deep walks.
- Network / slow mounts: shorter budget or skip non-local roots unless path-forced.
- Don’t index live results into the persistent 100k cache (avoids bloat + churn); ephemeral only.
- Reuse excludes from Settings so user-added skips apply to live walks too.

---

## Ranking / relevance

| Priority | Item | Notes | Status |
|----------|------|-------|--------|
| **P0** | Exact folder/file name below weak apps | `kind_rank` sorted App before Folder always; fuzzy apps (Flatseal, Chrome) beat exact `Glassbox` | **done** — score-first sort; drop weak apps when strong path match; app fuzzy name-only + higher threshold |

---

## Other / product (parking lot)

- Virtualized results list — n/a while cap is 25 (`OPTIMIZATION.md` G1)
- ~~Deeper incremental root walk when index grows large~~ → see **On-demand deep search** (prefer query-time live walk over permanent deep index)
- Settings / mounts split if `config.rs` grows further
