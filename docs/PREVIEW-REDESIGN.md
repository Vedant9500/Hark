# Preview / window redesign — tracked issues

**Status:** first redesign shipped (items 1–5 below); follow-ups §6–§7
**reverted 2026-09-06** — per-frame measure/resize loop felt laggy, gaps
persisted, and it introduced a render glitch (overlapping rows/footer).
Tree is back to the §1–§5 state, which stays.
**Reported:** 2026-09-06 (7 screenshots).
**Code ref:** `src/ui/mod.rs` (`PREVIEW_WINDOW_WIDTH`), `src/ui/preview.rs`
(thumb gate + `ScaleDown`), `src/ui/size_anim.rs` (Euclidean travel).
Prior art: `docs/audit/SECTION_05_PREVIEW_PANEL.md`,
`docs/hyprland-layer-corners.md`, `UI_UX_AUDIT.md`.

Current architecture (facts, verified in code):

- `WINDOW_WIDTH = 720`, `EXPANDED = 480`, `COMPACT = 110` (`src/ui/mod.rs:42-44`).
- Preview root fixed `280×380` (`src/ui/preview.rs:214-216`), image frame fixed
  `248×186` 4:3 (`preview.rs:40-41`, `:297`), CSS mins match
  (`src/theme/css.rs:378-384`).
- Decode target `DECODE_MAX_PX = 496` (2× frame, HiDPI) (`preview.rs:43`).
- `set_size_request` is a *minimum* — natural size can still grow the window.
  `preview.set_visibility_cb` is currently a **no-op** (`mod.rs:242-253`):
  no window widen on preview open, so the 280px panel eats the list column
  (720 → ~440px list).
- `SizeTweener` (`src/ui/size_anim.rs`) only animates compact↔expanded
  *height*. No width / preview-toggle animation exists.
- Decode order: FreeDesktop thumb **first**, then full decode
  (`preview.rs:1377-1402`). Thumb path returns the cached pixels as-is
  (`decode_thumb_or_scaled`, `preview.rs:1405-1438`) — can be 128px `normal`
  for a 2K source. Full path caps long edge at 496 (`decode_image_scaled`,
  `preview.rs:1699-1724`).
- `Picture`: `ContentFit::Contain`, `can_shrink(true)`, `hexpand(true)`,
  fixed 248×186 request (`preview.rs:288-297`). Small textures upscale to fill,
  large ones downscale — same frame regardless of native resolution.

---

## 1. Preview open squeezes the results column

Screenshots: `grass` 128px result, `gemini` 2784px result (imgs 1–2).
List column drops to ~440px, paths/subtitles truncate hard. Looks ugly.

- Cause: fixed 720 window + 280 panel inside same `body` HBox
  (`mod.rs:171-174`, `:255-257`, `preview.rs:214`). Old widen-to-1001 behavior
  from §5.2 was reverted to no-op in `ee891c9` (ghost fix); stale comment at
  `mod.rs:40-41` still claims widening.
- Redesign Q: widen window to ~1000 on preview (Raycast "Search Files" pattern,
  §5 audit) vs overlay/Quick Look vs fixed split? Must keep list ≥ ~600px.
  Watch Hyprland width-ghost (`hyprland-layer-corners.md`, `no_anim` rule).

## 2. All resizing snaps, no smooth animation

Window / panel changes are instant. Want eased resize.

- Cause: `SizeTweener::glide` exists but only called from `apply_body_chrome`
  for compact↔expanded height (`mod.rs:2314-2315`); preview toggle calls
  nothing. `RESIZE_MIN_MS 160` / `MAX 240`, ease-out cubic (`size_anim.rs:17-35`).
- Redesign Q: extend tweener (or new width tweener) to preview show/hide +
  content-driven changes; retarget mid-flight like `scroll_anim` does for
  rapid arrowing. Keep `no_anim` layerrule — motion must stay app-side.

## 3. Preview sharpness is non-deterministic (crisp vs blurry mess)

Screenshot: `resume.pdf` page crisp (img 3) vs pixel-icon cases blurry.

- Suspects (both in code):
  a. Thumb-first: a stale/low-res `normal` (128) or `large` (256) thumb is
     served directly for any size source (`preview.rs:1405-1422`,
     `thumbnails.rs:20-32`). No thumb → full 496 decode → crisp. Explains
     "sometimes".
  b. Upscale: sources smaller than the 248 frame (e.g. 128px icons) are
     stretched ~2× by `Contain` → blurry by construction.
- Redesign Q: minimum effective resolution gate? Never serve a thumb smaller
  than the frame (decode full instead)? Downscale-only policy (never upscale,
  center small art at native size on checkerboard)? Consider `Nearest` for
  pixel art vs `Bilinear`.

## 4. No fixed list/preview split — window "dances" while scrolling

Screenshots: two `.py` code selections (imgs 4–5) with visibly different
geometry; same class of complaint for image scroll-through.

- Cause: window follows natural size (`set_size_request` = min). Image view
  (~186 + meta), code view (scroll `min 120 / max 380` + meta,
  `preview.rs:349-357`), and icon view (centered, variable) each impose
  different natural heights; show/hide toggles 280px of natural width.
  `apply_body_chrome` only pins 720×480 / 720×110 (`mod.rs:2308-2313`).
- Redesign Q: lock a constant outer geometry for the whole preview-open
  session + fixed list:preview ratio (e.g. 720 list + 280 preview = 1000, or
  proportional split). All three stack pages (icon/image/code) must share one
  content box size; letterbox inside, never resize chrome. Defer content
  longer than the box to internal scroll.

## 5. Display size ignores native resolution (2K looks small, 128px looks big)

Screenshots: `gemini` 2784×1536 vs `Grass_90176eb4.png` 128×128 (imgs 6–7).
Same frame → normalized appearance, resolution cue lost; small art upscaled
blurry (see #3b).

- Cause: fixed 248×186 frame + `Contain` (`preview.rs:296-297`). By design
  every image fills the same box.
- Redesign Q: downscale-only + native-size centering with cap? Explicit zoom
  affordance? Keep dims label (already shows `label_w × label_h`,
  `preview.rs:1717-1723`) but make pixels honest: small stays small, large
  fits. Decide pixel-art policy separately.

---

## Redesign constraints (do not regress)

- Hyprland layer ghost: compositor anims off (`no_anim`), all motion app-side
  (`size_anim.rs:1-8`, `hyprland-layer-corners.md`). Width tween must step
  window+shell together like height does today.
- Debounce/latest-wins decode (`LOAD_DEBOUNCE 45ms`, single worker,
  `preview.rs:47`, `:1105-1129`) stays — fast arrowing must not stack workers.
- Texture cache cap 24, 2 MiB code gate, converter timeouts/sandbox stay.
- §5 deferred items still open: checkerboard for alpha (#26 note at
  `preview.rs:1340`), structured metadata grid, txt/md render. Fold into
  redesign, don't conflict.

## Follow-up (2026-09-06): text-aware height + stage bezel — REVERTED

> Reverted same day. Kept here as a record of what was tried and why it
> failed: measuring + resizing every bind created a laggy feel, the gaps
> persisted anyway, and a glitch appeared (rows/footer rendering overlapped
> mid-resize). Lesson: per-bind `measure()` + window glides fight GTK's own
> relayout; any future attempt needs to size from row counts, not live
> measure, and never resize inside a settle callback. The §1–§5 redesign
> above is the current shipping state.

Original attempt (for reference): `fit_window_height` measured list/panel
natural heights per bind and glided the window 280–480 (`ui.adaptive_height`
toggle + settings checkbox); list/code scrolls got exact-fit
(vexpand off, dynamic max); decode capped at native dims; `on_content`
settle callback refit on async arrival; stage bg transparent for the bezel.
All removed; `adaptive_height` never shipped in a release.

## Next steps

- Visual pass on `resume` / `gemi` / `.py` + the original 7 queries.
- Trash-removal refit (currently corrects on next refresh).
