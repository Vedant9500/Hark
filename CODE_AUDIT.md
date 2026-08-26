# Code Audit — Bugs, Logic Errors & Inefficiencies

Date: 2026-08-21 · Scope: full `src/` (~31k lines, 48 files)
Method: manual review of core modules + parallel reviewer passes over UI/providers/calc.
Baseline: `cargo clippy --all-targets` clean, `cargo test` 226 passed / 0 failed — all findings below are logic-level issues tooling does not catch.

**Verified 2026-08-22**: every finding re-checked against source by parallel review agents; verdicts and corrections marked inline (`CONFIRMED` / `PARTIAL` / removed false positives). New findings from the verification pass added in their own sections.

Totals after verification: **3 crash bugs · 18 confirmed logic/perf issues (1 false positive removed) · 7 perf · ~15 minor · 16 new findings**

---

## 🔴 Crash bugs (panics on user input)

### 1. `src/providers/translate.rs:241` — UTF-8 slice panic on Unicode whitespace — ✅ CONFIRMED (correction)
`rest[idx..]` panics on a non-char-boundary. `split_whitespace` splits on *Unicode* whitespace, but the byte walk only skips ASCII whitespace (`translate.rs:230-240`), so a U+3000/U+2028 separator between lang codes (`tr zh<ideographic-space>en hello`) lands `idx` mid-UTF-8-sequence and aborts on the GTK main thread. Reachable via forced prefix and auto-detect (`is_translate_query` :490).
**Correction:** single NBSP (2 bytes) between two 2-char codes lands exactly on the boundary — silent mis-parse (second code leaks into text), not panic. Panic needs a separator run ≥3 bytes. Bonus desync: `\x0B` is `char::is_whitespace` but not ASCII-whitespace → garbage output, never panic.
**Fix:** locate codes with `str::find` (returns boundaries) instead of the manual byte walk.

### 2. `src/providers/files/search/glob.rs:156` — UTF-8 slice panic in glob segment scan — ✅ CONFIRMED
`find_path_segment` retries with `i = abs + 1` (:156) after a failed component-boundary check. `abs` is the start of a matched occurrence, so `path_lower[abs] == seg[0]`; when the query segment starts with a multi-byte lead byte (`src/文档/*.rs`, or scoped `report.md in 文档`), `abs+1` is a continuation byte and the next iteration's `&path_lower[i..]` (:147) panics. Reachable from plan.rs segment building (:163,:172,:571), index matches (index.rs:368 lowercases paths), and deep.rs live walk (:543,:565,:699).
**Fix:** advance to next char boundary instead of `abs + 1`.

### 3. `src/ui/settings.rs:296` — capture-phase key controller swallows text input — ✅ CONFIRMED (+ twin)
Root-level `EventControllerKey` in Capture phase intercepts `j/k/↑/↓/Home/End/PageUp/PageDown` before the focused widget; typing "j"/"k" in any Entry inside Settings switches category instead, arrows/Home/End lose cursor movement.
**Addition:** a second interceptor exists at `src/ui/mod.rs:837-880` — fixing settings.rs alone leaves that one live.
**Fix:** skip handling when `root.focus_widget()` downcasts to an editable, in both controllers.

---

## 🟠 Wrong results / logic errors

### 4. `src/providers/calc/timezone.rs:672` (div at :675) — local offset truncated to whole hours — ✅ CONFIRMED
`local_as_tz()` computes `off / 3600` (integer division), mapping IST (+5:30) to `Etc/GMT-5`. Every "here → X" conversion is off by 30 minutes (45 for Nepal).
**Fix:** carry the raw offset through the conversion math.

### 5. `src/providers/calc/timezone.rs:424` — negative UTC offsets never resolve — ✅ CONFIRMED (worse than stated)
`normalize_place_key` replaces `-`→`_` (:428), killing the `"utc-8"` arms (:653-654). Worse: unresolved tokens fall through to `predict_tz` prefix matching (:298), which silently maps bad utc/gmt tokens to plain UTC — user gets an equal-times card instead of an error.
**Fix:** don't mangle `-`; make unmatched utc/gmt tokens error out.

### 6. `src/providers/apps.rs:435` — `clean_exec` destroys argv quoting — ✅ CONFIRMED
Parse→store→launch round trip verified: `Exec=foo --opt="bar baz"` → parse `["foo","--opt=bar baz"]` → store joined `"foo --opt=bar baz"` (apps.rs:435-441, stored :284) → re-split at launch (engine.rs:374 → apps.rs:505) yields `["foo","--opt=bar","baz"]`. Same for `sh -c "echo hello"`.
**Fix:** store the parsed `Vec<String>` argv in `DesktopApp`.

### ~~7. `quick.rs:506` Mb/Kb/Gb treated as bytes~~ — ❌ FALSE POSITIVE (removed)
`ends_with('B')` is case-sensitive; `Mb` ends lowercase `'b'`, never classifies as bytes — bits handling matches the function's own doc. The audit's `55GB at 100Mb` example does not reproduce. **Real inverse bug found → see NEW-N3.**

### 8. `src/providers/calc/quick.rs:639` (seed :645) — predictable password PRNG — ✅ CONFIRMED
xorshift64 seeded `nanos ^ pid·const`; consumers verified: `uuid_v4` (:774-789), `try_password` (:810-836).
**Fix:** seed credential paths from OS CSPRNG; keep xorshift for dice/coin.

### 9. `src/providers/calc/quick.rs:669` — i64 overflow in random range — ✅ CONFIRMED (release behavior worse)
`(hi - lo + 1)` computed in i64 before cast; subtraction overflows first. Debug: panic. Release: full-range wraps span→0 (always `lo`); **partial over-range spans wrap negative → results below `lo`**, not just stuck-at-lo.
**Fix:** compute span in i128 / reject spans > u64.

### 10. `src/providers/calc/expr.rs:161` — unary minus binds tighter than `^` — ✅ CONFIRMED
`parse_pow` base goes through `parse_unary` (:186-188 negates before the `^` loop) → `-2^2 = (-2)^2 = 4`.
**Fix:** recurse unary into pow (`-` → `-parse_pow(...)`).

### 11. `src/ui/mod.rs:1044` — char/byte index mismatch in Right-arrow gate — ✅ CONFIRMED (direction corrected)
`search.position() >= search.text().len() as i32` compares GTK char index vs byte length. Since byte len ≥ char count, the gate is *harder* to satisfy on non-ASCII queries: at-end → Action Panel shortcut silently fails to fire (false negative); it never misfires spuriously.
**Fix:** compare against `chars().count()`.

### 12. `src/theme/css.rs:1-24` — byte slicing of external scheme.json values — ✅ CONFIRMED (vector narrowed)
`is_light_theme`/`rgba` slice `&h[0..2]` directly. Panic vector = caelestia's external `scheme.json` only; the accent entry path is sanitized upstream.
**Fix:** validate ASCII-hex or use `get(0..2)` with fallback; share one sanitize helper (see NEW-N15).

### 13. `src/providers/files/search/glob.rs:265-267` — sibling dirs leak into scoped globs — ✅ CONFIRMED
Clause 3 `starts_with(&dir_lower)` has no `/` boundary and contributes **only** sibling-prefix paths (clauses 1-2 cover children + dir itself) — `/home/u/docs-extra/x.md` outranks real child `/home/u/docs/sub/x.md` under rank.rs's tree-blind boosts (verified mechanically: depth + high_value dominate). Live `read_dir` branch (:217-251) unaffected.
**Fix:** delete clause 3 outright.

### 14. `src/providers/files/index.rs:808` — cache meta stamped despite failed save — ⚠️ PARTIAL (core confirmed, details corrected)
Meta write (:813-816) sits inside `if let Ok(to_vec)` but **outside** `if fs::write(&tmp).is_ok()` (:810): ENOSPC on tmp write → meta stamped fresh anyway. Rename result also ignored (:811) — same freshness-lie class.
**Corrections:** `to_vec` failure does *not* stamp meta (whole block gated); consequence chain overstated — RAM index stays populated, next tick takes the battery path, so persistent cost is a full walk **per process start**, not every tick (timer 45 min, TTL 30 min).
**Fix:** stamp meta only after rename succeeds; check rename result.

### 15. `src/providers/files/mod.rs:293` — inconsistent force-prefix stripping — ⚠️ PARTIAL (divergence real, impact ≈ nil today)
Fallback strip is lowercase-only (:292-297) vs CI `strip_force_files_prefix` (:23-41). But `is_scoped_file_query` runs first (:288) with CI stripping internally, strong-signal scoped queries never reach the fallback, and in fallback territory the prefix word is inert (bare-folder confidence checks scope only; memo key uses the CI strip consistently). Observable divergence only where correct strip empties the name side (`file in docs`).
**Fix:** still worth it for hygiene — reuse `strip_force_files_prefix`.

### 16. `src/ui/thumbnails.rs:33` — thumbnail URI not percent-encoded — ✅ CONFIRMED
`format!("file://{}", canon.display())` used for digest and written `Thumb::URI`; code comment admits it. Spaces/`#`/`%`/non-ASCII → cross-app cache misses, invalid URI chunks shared with other apps.
**Fix:** `gio::File::for_path(canon).uri()`.

### 17. `src/ui/thumbnails.rs:12` — stale thumbnails never invalidated — ✅ CONFIRMED
`Thumb::MTime` write-only (zero read sites repo-wide); `dest.is_file() { return true }` early-return (:72-74); preview skips regen whenever any thumb exists (preview.rs:994).
**Fix:** parse+compare `tEXt::Thumb::MTime` before returning/writing.

### 18. `src/ui/open_with.rs:283` — GObject reference cycle leaks the popover — ✅ CONFIRMED (broader than stated)
Closed-handler captures its own popover strongly (:280-288) → whole widget tree leaks per open. **Additional independent cycles:** every `activate_row` closure captures `popover_c` (:93) attached to row GestureClick controllers (:195-210, :247-262). Fixing the closed-handler alone does **not** fix the leak.
**Fix:** WeakRef/downgrade for all popover captures.

### 19. `src/ui/preview.rs:1280` — predictable temp paths for converter output — ✅ CONFIRMED (severity tempered)
`create_dir_all("/tmp/hark-preview")` with default umask (0755, first creator owns); fully predictable names `{v,p}-{pid}-{name}`; remove_file+subprocess-write+read-back race lets a dir owner feed attacker-controlled pixels to gdk-pixbuf. Requires winning a create race on a multi-user box.
**Fix:** tempfile (O_EXCL) or 0700 dir under XDG cache.

### 20. `src/ui/preview.rs:1319` — no timeout on ffmpeg/pdftoppm — ✅ CONFIRMED
Blocking `.status()` (:1318-1334 video, :1369 pdf), no watchdog anywhere; hang chain traced end-to-end: worker thread blocked → send never runs → `worker_busy` stuck true → all previews dead until restart.
**Fix:** kill after ~10 s, report failure.

### 21. `src/ui/preview.rs:1246` — corrupt thumbnail kills the fallback chain — ⚠️ PARTIAL (reframed)
`.ok()?` does short-circuit `decode_thumb_or_scaled`, leaving its own fall-through (:1262-1275) unreachable for corrupt thumbs — but recovery survives one frame up: caller `decode_preview_media` (:1219-1223) falls through to kind-based decode. Real defect = duplicated/unreachable in-function fallback, not broken recovery.
**Fix:** bind `let Ok(pb) = … else fall-through` and delete the duplicate block.

### 22. `src/providers/fx.rs:150` — zero `to_rate` yields silent 0.00 conversions — ✅ CONFIRMED (+ extension)
`convert_amount` checks `from_rate == 0.0` only (:150-153); `load_disk` (:278-281) applies none of `parse_rates_body`'s validation (:260) → tampered/stale cache gives silent `0.00`. **Extension:** negative rates pass both network and disk paths → sign-flipped output also possible.
**Fix:** reject non-finite/zero/negative in both paths.

---

## 🟡 Performance / responsiveness

### 23. `src/ui/settings.rs:1826,2141` — config written to disk per keystroke — ✅ CONFIRMED (nuance)
Accent/target-language/endpoint/api-key fields run `ConfigStore::update` (clone + serialize + tmp-write/rename/chmod) on every `changed`. Theme reload fires accent-only; an equality guard limits some redundant writes; see NEW-N14 for the set_text cascade that doubles writes anyway.
**Fix:** commit on focus-out/Enter or debounce.

### 24. `src/providers/files/hot.rs:65` — Vec cloned under read lock every keystroke — ✅ CONFIRMED (low severity)
≤512 B clone (cap 64); real smell is the clone happening nested inside the index read lock (files/mod.rs:200-212), widening the window the rebuild writer must wait on. Dirty-gate keeps contention rare.
**Fix:** `Arc<[usize]>`; hoist snapshot out of the outer lock.

### 25. `src/ui/preview.rs:427` — synchronous `fs::metadata` on the main thread — ✅ CONFIRMED
All callers are GTK main-loop handlers (ui/mod.rs ×8 sites); blocking stat stalls UI per selection change on NFS/FUSE.
**Fix:** probe in debounced worker.

### 26. `src/ui/preview.rs:1187` — album art stretched to a square — ✅ CONFIRMED
`scale_simple(DECODE_MAX_PX, DECODE_MAX_PX, Bilinear)` distorts covers (image path preserves aspect at :1393; texture distortion survives `ContentFit::Contain`).
**Fix:** preserve aspect ratio.

### 27. `src/ui/settings.rs:844` — duplicate extra-folder rows — ⚠️ DOWNGRADED to minor
Duplicate config rows are real (add stores raw text, pins normalize), but **double indexing does not occur**: index dedup via `seen` (index.rs:299). Cosmetic/config-hygiene only. Normalize before contains-check anyway.

### 28. `src/ipc.rs:96-110` — listener can block forever on a silent client — ✅ CONFIRMED (self-verified)
Accepted stream read (:100) has no timeout; connect-and-write-nothing parks the listener iteration; subsequent toggles queue behind it. Compounded by NEW-N11 (inline handler execution).
**Fix:** `set_read_timeout`; handle off-thread.

### 29. `src/theme/mod.rs:184-206` — monitor spawns a debounce timer per event — ✅ CONFIRMED (bounded harm)
No coalescing — each event schedules its own 80 ms apply; bursts stack timers. Damage bounded by `apply_gen` skipping stale applications.
**Fix:** keep one pending `SourceId`, remove/re-arm.

---

## 🔵 Minor / cleanup — all ✅ CONFIRMED as claimed

| Location | Issue | Verdict note |
|---|---|---|
| `engine.rs:530` | Dead `let _ = q;` + unused lowercase var | exact |
| `datetime.rs:555` | Dead `let _ = now.hour();` keep-alive | exact |
| `unitmath.rs:179` | Dead `_b` field in parser struct | exact |
| `apps.rs:481` | Spawned children never waited → zombies until exit | :481+:491 |
| `apps.rs:426` | Unquoted `\` not escaped per Desktop Entry spec (`bar\ baz` splits) | spec deviation |
| `settings.rs:502,520` | Depth ± calls force_reindex even when clamped | cost = rebuild thread, not disk |
| `settings.rs:2050` | Restore-defaults leaves symbolic-icons checkbox stale | exact |
| `open_with.rs:103` | xdg-open spawn error discarded; window hidden regardless | exact |
| `thumbnails.rs:15` | Probe order large→normal→x-large — x-large unreachable | exact |
| `preview.rs:775` | Stale GtkSourceView language kept when guess_language → None | exact |
| `preview.rs:956` | Dead `meta` parameter | exact |
| `files/mod.rs:324` | Hardcoded `truncate(25)` instead of FILE_RESULT_LIMIT | constant lives in deep.rs |
| `rank.rs:122` | Hot short-circuit → highlight-style flip; fuzzy-only candidates vanish on strong queries | reframed: hot path never ran fuzzy, nothing dropped mid-flight |
| `rank.rs:218` | Budget decremented before scoring — failed scorings burn budget | corrected: prefilter skips do NOT burn (their `continue`s precede decrement) |
| `quick.rs:20,51` | `hexa` arm unreachable (regexes :85/:91 never produce it) | exact |
| `math.rs:127`/`quick.rs:45` | >u64 hex/binary silently yields no result | exact |
| `config.rs:1210` | ExcludeSet::matches double hash lookup | raw lookup redundant |
| `usage.rs`/`typos.rs` | Dirty-flag race record↔save (one extra delayed write worst case) | usage.rs:169-178 shape |

## ❓ Needs confirmation — resolved

| Location | Concern | Verdict |
|---|---|---|
| `expr.rs:110` | Parser depth cap missing | **Real risk**: 6-frame cycle per `(`; right-assoc `^` chain adds 1 frame/token |
| `preview.rs:370` | RefCell borrow across visibility callback | Latent only — current callback is an empty closure; hazard pattern real |
| `preview.rs:385` | Un-hide doesn't re-show panel until next update | Mitigated today — both call sites call `update()` which re-shows; fragile contract |
| `settings.rs:280,330,355` | Overlay guard borrow held across cb() | **FALSE POSITIVE** — borrow_mut is one-shot before signals fire; no reentry path |
| `live_cache.rs:51` | Duplicate recency stamps hide LRU entries | **FALSE POSITIVE** — monotonic `seq` makes stamps unique; covered by test |
| `thumbnails.rs:26` | canonicalize diverges symlink cache keys | Confirmed behavior — differs from other apps' hashing |
| `thumbnails.rs:77` | Pixbuf::from_bytes abort on inconsistent rowstride | Latent — current callers internally consistent |
| `open_with.rs:36` | Sync enumeration janks popover open | Confirmed — content-type + recommended queries on main thread pre-popup |
| `datetime.rs:112` | ymd_between leap-day year count off by one | **Confirmed bug, mechanism corrected**: chrono `with_year`/`with_month` return None (no Feb-28 clamp) — Feb-29 anchor kills the year walk instantly: 2020-02-29→2024-02-29 reports "48 months"; day 29/30/31 anchors also break month walk |

---

## 🆕 New findings (verification pass)

### Security

**N1. `src/providers/fx.rs:271-291` — `/tmp/hark` rate-cache fallback allows arbitrary-file overwrite.**
This one needs plain words because it is the most serious new item: when the XDG cache dir can't be resolved, currency-rate persistence falls back to `/tmp/hark/fx-rates.json`. `save_disk` does `create_dir_all` + `fs::write`, which follows symlinks and creates world-readable paths with default umask. A local attacker who pre-creates `/tmp/hark` (or symlinks the target file) before the victim's first run gets the victim process to overwrite attacker-chosen files with attacker-chosen JSON content. Fix mirrors what `ipc.rs` already does: create the fallback dir with mode 0700 and refuse pre-existing dirs not owned by the current user, or drop the `/tmp` fallback entirely.

### Logic / wrong answers

**N2. `timezone.rs:284-310` + `:420-421` — bogus utc/gmt tokens silently resolve to plain UTC.** `gmt+3` etc. fail resolve_tz arms then prefix-match in predict_tz → equal-times answer presented as success. Related structural trap: any future resolve_tz arm literal containing `-` is dead on arrival because the normalizer (:428) rewrites `-`→`_` (arm table and normalizer disagree on alphabet).

**N3. `quick.rs:502-507` — inverse of removed F7: case-sensitive byte detection vs lowercased magnitude table.** `Byte`/`Bytes`/`Kilobyte/s`/`MB/S`/`MBPS` contain no capital-B trigger forms → classified BITS → download ETA 8× too slow.

**N4. `glob.rs:172-196` — `?` matches one BYTE, not one character.** `a?` fails to match filename `aé` (false negative on multibyte names).

**N5. `glob.rs:217-251` — absolute-glob live branch skips `should_skip_entry`.** Excluded directories leak through absolute globs while relative globs apply excludes (glob.rs:445,:456).

**N6. `quick.rs:669-670` — f64 precision in rng_int.** Spans ≥ 2⁵³ collapse adjacent integers → unreachable values/non-uniform distribution even without i64 overflow.

### Robustness

**N7. `ipc.rs:44-54` — unrecognized ack retries toggle up to 5×.** Non-"ok" reply falls through without returning → loop rewrites `toggle`; EOF already counted as delivered.

**N8. `ipc.rs:96-110` — inline sequential handling wedges listener.** Slow/hung `on_toggle` parks all subsequent toggles despite the doc comment (:64-65) saying callers must bounce to the main loop (nothing enforces).

**N9. `files/mod.rs:435,634,716,725` — zombie spawns.** Same unwaited-Child class as apps.rs minor (trash_path is the correct contrast: blocking `.status()`).

**N10. `apps.rs` field-code filter over-matches.** Any token starting `%` is dropped in clean_exec (:438) and launch (:507); spec says standalone field codes with `%%` literal.

**N11. `apps.rs:402-432` — unterminated quote merges command tail into one token.** Malformed Exec launches wrong argv silently.

**N12. `quick.rs:635-646` — thread-local xorshift reseeds from nanos^pid.** Threads started in the same millisecond get correlated streams (micro).

### UI / consistency

**N13. `dnd.rs:335` — drag icon loads freedesktop thumb without mtime validation.** Stale drag icons share root cause with #17.

**N14. `settings.rs:1866-1875` — preset click double-writes config.** `set_text` fires the changed handler (write + theme.reload) then the preset body writes again + reloads again: 2 writes + 2 CSS injects per click; reset's `set_active` cascades similarly.

**N15. `theme/mod.rs:120` — unsanitized scheme colour flows into Pango markup** (rows.rs:73-81,:108). Benign today (GTK parse-fallback), same external-input class as #12 — route through the future shared sanitizer.

**N16. Fixed temp filenames race.** `thumbnails.rs:110` (`.{digest}.hart-tmp.png`), `usage.rs:176` (`usage.json.tmp`): concurrent writers can rename torn files into place. Low likelihood (single-daemon design).

---

## Suggested fix order (revised)

1. Crash pair (#1, #2) — user-triggerable main-thread aborts.
2. Settings keyboard swallow (#3) — both interceptors (settings.rs + ui/mod.rs:837).
3. **fx `/tmp/hark` symlink overwrite (N1)** — only actual security boundary issue found.
4. Timezone cluster (#4, #5+N2) — silently wrong answers for ~half the world.
5. Password PRNG (#8) + argv quoting (#6) — security/correctness of outputs.
6. Preview hang (#20) + IPC wedge (#28+N8) — permanent-degradation class.
7. Config-per-keystroke (#23+N14) + meta stamping (#14) — disk churn / cold-start walks.

---

## 📋 Tracker

All findings, sorted by priority then file. IDs map to sections above. Mark `☐` → `☑` when fixed; `Status` values: `open` / `fixed` / `wontfix`.

| ID | Pri | Location | Issue | Status |
|---|---|---|---|---|
| #1 | P0 | `providers/translate.rs:241` | UTF-8 slice panic on ≥3-byte Unicode whitespace between lang codes | fixed |
| #2 | P0 | `files/search/glob.rs:156` | UTF-8 slice panic: retry `abs+1` lands on continuation byte for multibyte segments | fixed |
| #3 | P0 | `ui/settings.rs:296` + `ui/mod.rs:837` | Capture-phase key controllers swallow text input in Entries (two interceptors) | fixed v2: guard downcasts gtk::Editable (focus may be Entry internal Text) |
| N1 | P0 | `providers/fx.rs:271-291` | SECURITY: `/tmp/hark` fallback follows symlinks → arbitrary-file overwrite as victim | fixed |
| #4 | P1 | `calc/timezone.rs:672` | Local offset truncated to whole hours (IST off 30m, Nepal 45m) | fixed |
| #5 | P1 | `calc/timezone.rs:424` | Negative UTC offsets never resolve (`-`→`_` normalization kills arms) | fixed |
| N2 | P1 | `calc/timezone.rs:284-310` | Bogus utc/gmt tokens silently resolve to plain UTC; `-` arms DOA structurally | fixed |
| #6 | P1 | `providers/apps.rs:435` | `clean_exec` re-join destroys argv quoting at launch | fixed |
| #8 | P1 | `calc/quick.rs:639` | Passwords/UUIDs from xorshift seeded nanos^pid — enumerable credentials | fixed |
| #9 | P1 | `calc/quick.rs:669` | i64 overflow in random range; release wraps negative span → results below lo | fixed |
| N6 | P1 | `calc/quick.rs:669-670` | f64 precision: spans ≥2⁵³ collapse adjacent random ints | fixed |
| #10 | P1 | `calc/expr.rs:161` | Unary minus binds tighter than `^`: `-2^2 = 4`, conventional −4 | fixed |
| #11 | P1 | `ui/mod.rs:1044` | Char index vs byte len gate — Action Panel shortcut dead on non-ASCII queries | fixed |
| #12 | P1 | `theme/css.rs:1-24` | Byte slicing external scheme.json hex panics instead of fallback | fixed |
| #13 | P1 | `files/search/glob.rs:265` | Sibling-prefix dirs leak into scoped globs (delete clause 3) | fixed |
| #14 | P1 | `files/index.rs:808` | Meta stamped despite failed tmp write/rename → freshness lie per process start | fixed |
| #15 | P2 | `files/mod.rs:293` | Force-prefix fallback strips lowercase-only vs CI elsewhere (hygiene) | fixed |
| #16 | P1 | `ui/thumbnails.rs:33` | Thumbnail URI not percent-encoded → cross-app cache misses, invalid URI shared | fixed |
| #17 | P1 | `ui/thumbnails.rs:12` | Stale thumbs never invalidated — MTime write-only, never compared | fixed |
| #18 | P1 | `ui/open_with.rs:283` | GObject cycles leak popover tree — closed-handler AND activate_row captures | fixed |
| #19 | P1 | `ui/preview.rs:1280` | Predictable `/tmp/hark-preview` paths; dir-owner can feed pixels to decoder | fixed |
| #20 | P1 | `ui/preview.rs:1319` | No timeout on ffmpeg/pdftoppm — one hang kills all previews until restart | fixed |
| #21 | P2 | `ui/preview.rs:1246` | `.ok()?` makes in-function thumb fallback unreachable (dead code) | fixed |
| #22 | P1 | `providers/fx.rs:150` | Zero to_rate → silent 0.00; negatives pass network AND disk validation | fixed |
| N3 | P1 | `calc/quick.rs:502-507` | Inverse case bug: `Bytes`/`MBPS`/`MB/S` classified bits → ETA 8× too slow | fixed |
| N4 | P1 | `files/search/glob.rs:172-196` | Glob `?` matches one byte not one char — false negatives on multibyte names | fixed |
| N5 | P1 | `files/search/glob.rs:217-251` | Absolute-glob live branch skips should_skip_entry — excluded dirs leak | fixed |
| NC-dt | P1 | `calc/datetime.rs:112` | ymd_between Feb-29 anchor → chrono None kills walk; "48 months" for 4 years | fixed |
| N7 | P1 | `ipc.rs:44-54` | Unrecognized ack retries toggle up to 5× | fixed |
| N8 | P1 | `ipc.rs:96-110` | Inline handler wedges listener thread (pairs with #28) | fixed |
| #28 | P1 | `ipc.rs:100` | No read timeout on accepted stream — silent client parks listener forever | fixed |
| N13 | P2 | `ui/dnd.rs:335` | Drag icon loads freedesktop thumb without mtime check (same root as #17) | fixed |
| #23 | P2 | `ui/settings.rs:1826,2141` | Config written per keystroke on main thread (+ theme reload) | fixed |
| N14 | P2 | `ui/settings.rs:1866-1875` | Preset click double-writes config + double CSS inject (set_text cascade) | fixed |
| #24 | P2 | `files/hot.rs:65` | Vec cloned under read lock nested inside index lock every keystroke | fixed |
| #25 | P2 | `ui/preview.rs:427` | Sync fs::metadata on main thread stalls UI on NFS/FUSE | fixed |
| #26 | P2 | `ui/preview.rs:1187` | Album art stretched square via scale_simple | fixed |
| #29 | P2 | `theme/mod.rs:184-206` | Debounce timer spawned per event, no coalescing | fixed |
| #27 | P3 | `ui/settings.rs:844` | Duplicate extra-folder rows in config (index dedup prevents double work) | fixed |
| N9 | P2 | `files/mod.rs:435,634,716,725` | Unwaited spawns → zombies (same class as apps.rs:481) | fixed |
| N10 | P3 | `providers/apps.rs:438,507` | Field-code filter drops any `%token`; `%%` literal mishandled | fixed |
| N11 | P3 | `providers/apps.rs:402-432` | Unterminated quote merges command tail silently | fixed |
| apps-z | P3 | `providers/apps.rs:481` | Spawned children never waited → zombies | fixed |
| apps-bs | P3 | `providers/apps.rs:426` | Unquoted `\` not escaped per Desktop Entry spec | fixed |
| N15 | P3 | `theme/mod.rs:120` | Raw scheme colour into Pango markup — route through shared sanitizer w/ #12 | fixed |
| N16 | P3 | `thumbnails.rs:110`, `usage.rs:176` | Fixed temp filenames race → torn file renamed into place | fixed |
| eng-q | P4 | `engine.rs:530` | Dead `let _ = q;` + unused lowercase var | fixed |
| dt-hr | P4 | `calc/datetime.rs:555` | Dead keep-alive line | fixed |
| um-b | P4 | `calc/unitmath.rs:179` | Dead `_b` parser field | fixed |
| set-depth | P4 | `ui/settings.rs:502,520` | Depth ± force_reindex even when clamped | fixed |
| set-sym | P4 | `ui/settings.rs:2050` | Restore-defaults leaves symbolic-icons checkbox stale | fixed |
| ow-spawn | P4 | `ui/open_with.rs:103` | xdg-open spawn error discarded; window hidden regardless | fixed |
| th-order | P4 | `ui/thumbnails.rs:15` | Probe order large→normal→x-large — x-large unreachable | fixed |
| pv-lang | P4 | `ui/preview.rs:775` | Stale GtkSourceView language kept when guess_language → None | fixed |
| pv-meta | P4 | `ui/preview.rs:956` | Dead `meta` parameter | fixed |
| fm-trunc | P4 | `files/mod.rs:324` | Hardcoded truncate(25) instead of FILE_RESULT_LIMIT | fixed |
| rank-hot | P4 | `files/search/rank.rs:122` | Hot short-circuit flips highlight style; fuzzy-only candidates vanish | fixed |
| rank-budget | P4 | `files/search/rank.rs:218` | Fuzzy budget burned by failed scorings (prefilter skips don't burn) | fixed |
| q-hexa | P4 | `calc/quick.rs:20,51` | `hexa` arm unreachable via regexes | fixed |
| m-u64 | P4 | `calc/math.rs:127`, `quick.rs:45` | >u64 hex/binary silently yields nothing | fixed |
| cfg-hash | P4 | `config.rs:1210` | ExcludeSet::matches double hash lookup | fixed |
| usage-race | P4 | `usage.rs:169-178`, `typos.rs:206-220` | Dirty-flag race record↔save (one delayed write worst case) | fixed |

### Latent hazards (fix opportunistically, no current trigger)

| ID | Location | Hazard | Status |
|---|---|---|---|
| pv-refcell | `ui/preview.rs:370` | Borrow held across visibility callback — panics if callback gains a body touching same RefCell | fixed | — fixed
| th-bytes | `ui/thumbnails.rs:77` | Pixbuf::from_bytes over-reads if caller-supplied rowstride/pixels ever inconsistent | fixed | — fixed
| th-canon | `ui/thumbnails.rs:26` | canonicalize before hashing diverges symlinked cache keys from other apps | fixed | — fixed
| ow-sync | `ui/open_with.rs:36` | Sync app enumeration janks popover open on cold app DB | fixed | — fixed
| rng-reseed | `calc/quick.rs:635-646` | Same-millisecond thread starts get correlated xorshift streams (micro) | fixed |

**Counts:** P0 ×4 · P1 ×25 · P2 ×10 · P3 ×7 · P4 ×16 · latent ×5 = **67 items**. Fixed so far: 34 items (4×P0 · 18×P1 + 5 bonus · empty-state layout) + install speedup.

---

## 🧭 Pass 3 architecture & dependency verification (2026-08-25)

### Architecture and discovery delta (verified)

- `src/main.rs` dispatch order is `--daemon` / `--bench` gate / `--search` one-shot / IPC fast-path / GTK application. The daemon and client are the same binary, as documented.
- Startup-to-launch trace: `main` → `Engine::new` → warm/periodic worker threads → `Launcher::new` → IPC `spawn_listener` → unbounded async channel → `glib::spawn_future_local` → `Launcher::toggle/show` → search debounce → `Engine::search/execute` → `spawn_detached_argv` (`apps.rs`) or `open_path_with`/`spawn_detached` (`files/mod.rs`). This confirms the single-main-loop architecture and why worker callbacks must remain non-blocking.
- Source census: 53 Rust files (including `bench.rs` behind the `bench` feature). Only one `unsafe` block exists (`src/ui/preview.rs:1509`), and it is a safe copy: the call is `pixbuf.pixels().to_vec()` inside the pixbuf's lifetime; no raw pointer is retained after the statement. This passes the unsafe invariant census.
- External process/exec surfaces verified: `setsid`, `sh -c` (file-manager detach fallback), `xdg-open`, `gio trash`, `gvfs-trash`, `findmnt`, terminals, `wl-copy`, `xclip`, `ffmpeg`, `pdftoppm`, `hyprctl`. All user-controlled file arguments are passed as argv, except the intentionally shell-quoted `spawn_detached` path, which routes every argument through `shell_quote` (`files/mod.rs:616-635`).
- Environment/external-path inputs: `XDG_RUNTIME_DIR`, `XDG_STATE_HOME`, `XDG_CACHE_HOME`/home fallbacks, `TERMINAL`, `PATH`. Config, cache, usage, typo, FX, file-index, and IPC paths were inspected; no world-writable shared `/tmp` state path remains. Atomic writes use fixed sibling `.tmp` files, which is race-prone under concurrent writers (already tracked as N16), but not attacker-writable path selection.

### Verification matrix

| Check | Result | Evidence |
|---|---|---|
| `cargo fmt` | Applied; now clean | `cargo fmt` run; only pre-existing formatting drift changed |
| `cargo clippy --all-targets --all-features -- -D warnings` | ✅ clean | Exit 0 after formatting |
| `cargo test --all-features` | ✅ 253 passed, 0 failed, 2 ignored (`#[ignore = "unix socket bind"]`) | Full unit-test run |
| `cargo build --locked` (default) | ✅ | Exit 0 |
| `cargo build --locked --features layer-shell,bench` | ✅ | Exit 0 |
| `cargo-deny --all-features check` | ❌ advisories | `RUSTSEC-2024-0436` — `paste v1.0.15` unmaintained (transitive via `lofty v0.25.1`) |
| deny bans/licenses/sources | ✅ | warnings only (duplicate versions, unencountered allow-list entries, missing `sourceview5` license metadata) |
| MSRV floor 1.70 | ⚠️ not verified in this environment | No `rustup`/1.70 toolchain available; this is evidence-gated, not claimed as passing |

### New verified findings

#### P1 — LibreTranslate API-key redirect exfiltration surface (`src/providers/http.rs:78-90`, `src/providers/translate.rs:704-730`, `src/config.rs:499-543`)

- **Root cause:** `validate_translate_endpoint` checks the initially configured host, but `http::post_json` uses `ureq`'s default agent, which follows up to five redirects. The user's LibreTranslate API key is serialized into the JSON request body (`translate.rs:722-724`). For HTTP 307/308, `ureq` preserves the request body across redirects (upstream `agent.rs` redirect handling); a configured plain-HTTP endpoint (explicitly allowed for local LibreTranslate) can therefore redirect the exact key-bearing body to an arbitrary URL, including another network-reachable host.
- **Failure/security pathway:** attacker controls or compromises an allowed endpoint URL → responds `307/308` to `https://collector.example/translate` → ureq replays `{q, source, target, format, api_key}` → key and pasted source text are exfiltrated. This is a cross-module bypass of the carefully implemented initial-host SSRF deny-list.
- **Classification:** SSRF-adjacent credential/data disclosure (OWASP *Unvalidated Redirects and Forwards* / CWE-601, with CWE-200 for secret disclosure).
- **Verified constraints:** only the custom LibreTranslate branch carries the API key; free Google/MyMemory requests contain no secret. The endpoint deny-list does block direct cloud-metadata/link-local first hops, so the issue is specifically redirect-time target validation, not the initial parse.
- **Remediation (fully audited):** make `post_json` accept no redirects (or use a dedicated translate agent with `redirects(0)`) and perform an explicit allow-once redirect resolver that revalidates every hop with the existing `validate_translate_endpoint`. Simpler safe patch: build the translate request agent with `.redirects(0)`, surface HTTP 3xx as an error, and require the user to update the endpoint. Add a regression test with a local 307 listener (ignored by default like the current socket tests) asserting no second request/body replay occurs.
- **Status 2026-08-26:** fixed in `src/providers/http.rs` by routing secret-bearing `post_json` through a dedicated `.redirects(0)` agent. Added `post_json_agent_has_redirects_disabled`, which starts local source/target listeners and asserts the redirect target is not contacted.

#### P2 — daemon has no graceful process-lifecycle cleanup (`src/main.rs:16-110`, `src/ipc.rs:54-119`)

- **Root cause:** no `SIGINT`/`SIGTERM` handler, `glib` shutdown hook, or UnixListener cleanup is installed. `Engine::shutdown_periodic_refresh` exists and runs only on in-process `Drop`; process termination never reaches it.
- **Failure pathway:** SIGTERM → GTK/runtime default termination → stale `hark.sock` remains. The next start reclaims it through `bind_socket`'s connect-and-remove logic, so this is recoverability-by-rebind rather than a deadlock. Because socket parents are runtime/cache with mode 0700 and the socket itself is 0600, no cross-user takeover is exposed. Practical impact is minor stale-file residue and a small race window where a second daemon may observe the first as alive.
- **Classification:** availability/lifecycle hygiene (CWE-404 premature resource release inverse: resource not released on exit).
- **Remediation:** install a Unix signal watch on the GTK main loop, send a stop message to the listener thread (or hold an acquired `Listener` and close/drop it during shutdown), call `shutdown_periodic_refresh`, and remove the socket only after the listener has stopped accepting. Also remove the socket path in a `panic` hook if keeping `panic = "abort"` in release.
- **Existing mitigation:** stale socket rebind logic at `ipc.rs:123-142` must remain regardless; it covers crashes and SIGKILL.

#### P2 — daemon refresh thread can keep the process alive after UI teardown (`src/engine.rs:86-107`, `src/main.rs:57-108`)

- **Root cause:** `Launcher` is retained forever in `Rc<RefCell<Option<Launcher>>>`; the periodic-refresh thread owns clones of `FileProvider` and `AppProvider` and loops for 45-minute intervals. GTK application shutdown does not route through `Engine::drop` because `Arc<Engine>` is captured by UI closures and the app-hold keeps the process alive by design.
- **Failure pathway:** on application close, the intended daemon model is to continue, so this is only a lifecycle mismatch if a future caller drops the window expecting process exit; currently no user-facing defect was reproducible. Recorded as a hazard, not a crash.
- **Remediation:** add an explicit `connect_shutdown` handler on `Gtk::Application` that stops IPC and periodic refresh, and document that `--daemon` intentionally keeps `hold()` until signal shutdown.

#### P3 — stale-race/dead-code audit corrections

- `theme/mod.rs:139-206` already coalesces `reload()` with a stored `SourceId`; the uncoalesced timer is only the *file-monitor* fallback at lines 200-204. Thus existing #29 should be narrowed: rapid appearance settings are debounced, but a chatty scheme writer can still schedule one 80 ms timer per monitor event.
- `schedule_deep_job`/`schedule_translate_job` (`ui/mod.rs:2640-2745`) have a narrow re-arm race: after taking no job, a writer can store a job between the empty check and `BUSY.store(false)`. The subsequent recheck covers this writer, so the implementation is correct; the `lock().unwrap()` calls are on poisoning-only mutexes and never contend long enough to affect the GTK loop because all critical sections are short. No finding.
- `Application::activate` on an existing launcher toggles it (`main.rs:72-75`). Therefore a second bare `hark` invocation can toggle via GApplication activation even if the IPC socket is missing. This is intentional overlap, but it means IPC failure does not fully prevent duplicate toggle paths. No security impact; document as behavior.
- `Engine` deliberately spawns the periodic refresh only for the daemon and stops it on `Drop`; the `--search` headless path has a bounded 30 s wait and exits. No thread leak was verified there.

### Dependency triage

- **RUSTSEC-2024-0436 / `paste v1.0.15`** is the sole deny failure. It enters only through `lofty v0.25.1` (audio metadata). Impact is maintenance status, not a known memory-safety exploit. Options: (a) wait for a lofty release replacing `paste`; (b) temporarily add a scoped, dated, commented ignore in `deny.toml` only if CI must be green while upstream moves; (c) remove/fork the lofty feature. Do not silently mark it resolved.
- Duplicate-version warnings are transitive GTK/Windows-target duplication and are warnings by policy. No duplicate crate with a known conflicting runtime semantic was identified.
- `sourceview5` does not declare a license in its manifest. Its crate license metadata should be checked before tightening `deny.toml` beyond warning.

### Continuous audit execution log

- Pass 3A: repository map, daemon/client dispatch, startup/IPC/provider/launch trace, env/external-path inventory, unsafe census — complete.
- Pass 3B: IPC framing, permissions, stale-socket reclaim, read/write timeouts, callback handoff via async channel — complete; no new IPC finding beyond lifecycle cleanup.
- Pass 3C: HTTP/FX/translate body caps, timeouts, strict parsing, SSRF first-hop validation, background-agent behavior — complete; one new redirect finding above.
- Pass 3D: filesystem traversal (`follow_links(false)`, depth caps, `MAX_INDEX`, excludes, symlink classification), hot set, cache atomicity/meta ordering — complete; no new traversal bug beyond tracked N14/N16.
- Pass 3E: exec surfaces, desktop argv parsing/quoting, terminal launch, DND source paths — complete; no shell-injection bypass found in current arguments.
- Pass 3F: build/clippy/tests/fmt/deny/default and layer-shell+bench matrix — complete; MSRV unresolved due missing toolchain.
- Passes 4–12 recorded per-pass sections above; Pass 13 (2026-08-26): state stores/config, UI helper/animation/drag layer, calc-provider fuzz sweep — 24 new findings (4 P2, 20 P3); see Pass 13 section.
- Pass 14 (2026-08-26): engine scoring/merge depth, IPC flood/interrupt/auth, settings/open_with re-sweep — 21 new findings (6 P2, 15 P3); see Pass 14 section.
- Pass 15 (2026-08-26): ui/mod.rs depth, apps.rs/index.rs re-sweep, theme/bench/lib/packaging/docs first sweep — 25 new findings (4 P2, 21 P3); see Pass 15 section.
- Pass 16 (2026-08-26): search planner/deep-walk first unit sweep, HTTP providers depth re-verification, preview re-sweep — 21 new findings (1 P1, 4 P2, 16 P3); see Pass 16 section.
- Pass 17 (2026-08-26): fixed-status re-verification (34/34 clean, zero discrepancies), files provider surface, rows.rs depth — 12 new findings (2 P2, 10 P3); see Pass 17 section.
- Pass 18 (2026-08-26): calc depth, typo-learning algorithms, Phase-4 composite chains — 15 new findings (1 P1 chain, 5 P2, 9 P3); 4 chains verified, 4 refuted; `panic=abort` confirmed upgrading two findings' release impact. See Pass 18 section.
- Pass 19 (2026-08-26): saturation re-sweeps (mod.rs/rank.rs SATURATED; glob.rs P2 filter bypass) + manual dep-drift review — 5 new findings (1 P2, 4 P3). See Pass 19 section.
- Pass 20 (2026-08-26): expr parser depth (empirical P1 stack overflow), Action dispatch completeness, packaging pipeline — 4 new findings (1 P1, 3 P3). See Pass 20 section.
- Pass 21 (2026-08-26): config migration/sanitize depth, scoring-pipeline trace (SATURATED), allocation churn — 11 new findings (2 P2, 9 P3). See Pass 21 section.
- **AUDIT CLOSED by user instruction after Pass 21** (pass-yield was declining and remaining surfaces near-saturated). Termination condition (two consecutive zero-finding passes) was NOT formally met; remaining candidates for any future resume are listed in the Pass 21 close-out.

---

## 🧪 Pass 4 adversarial and edge-case audit (2026-08-25)

### New verified findings

#### P2 — `hark --search` without a value silently enters resident/GUI mode (`src/main.rs:18-43`)

- **Root cause:** `args.iter().position(|a| a == "--search").and_then(|i| args.get(i + 1).cloned())` maps a missing operand to `None`, exactly like no `--search` at all. There is no sentinel distinguishing “option present, value absent”.
- **Verified failure path:** `./target/debug/hark --search` produced no stdout/stderr and kept running until interrupted; the empty-query branch that correctly emits usage/exit 2 is reached only when an explicit empty string is supplied.
- **Impact:** scripts and keybindings hang instead of returning an immediate POSIX usage error. Under systemd/hyprland `exec-once`, the misconfigured invocation can leave an unintended resident process.
- **CWE/classification:** CLI argument validation defect (CWE-20 / CWE-1284 depending on exposure); local availability/behavior issue, not privilege escalation.
- **Remediation:** retain an `Option<Option<String>>` (or explicit `--search` marker), reject a missing operand with the existing usage message and exit code 2 before `Engine::new_headless`, and add tests for present-with-value, present-empty-value, and present-with-next-flag cases.

#### P3 — custom CLI flags are undocumented by `--help` and can conflict semantically (`src/main.rs:14-41`)

Runtime verification: `--help` shows only GTK/GApplication options; `--help-all` likewise does not mention `--daemon`, `--search`, or `--bench`. `--bench` wins over a simultaneously supplied `--daemon`, and `--search --daemon` would consume `--daemon` as its query before GTK parsing. Registering the options with GApplication (or parsing before GTK) would provide accurate help, conflict checks, and standard exit codes.

#### P3 — `.desktop` localization keys ignored (`src/providers/apps.rs:360-403`)

Only unlocalized keys are accepted. Compliant entries still work, but user-locale `Name[...]`/`Icon[...]` values are ignored and malicious entries cannot bypass parsing through a localized key because those keys are never consumed for execution. Full remediation is locale-aware key selection; no security impact was proven.

### False positives/corrections from this pass

- Earlier suspicion that `parse_direction_and_text` could still slice on a non-boundary is obsolete: current code uses `str::find`, which returns char boundaries; tests include Unicode whitespace.
- Scoped-query keyword slicing uses `to_lowercase()` then indexes the original only at `find`-returned boundaries plus ASCII keyword lengths; no multibyte panic path was found in the current implementation.
- `MAX_CODE_BYTES` bounds source previews before worker read; no unbounded preview read found.
- Runtime `--help`/`--help-all` verification is recorded rather than inferred from GTK defaults.

### Pass 4 verification log

- CLI runtime probes: `--help`, `--help-all`, `--search` (missing operand, interrupted), prior `--search ""` behavior source-verified.
- Keyboard/controller path review: settings/root capture, action panel, Escape lifecycle, Enter modifier matrix, Right-arrow gate.
- Desktop-entry parser review: section gating, type filtering, localization coverage, argv/field-code tests.
- Source-preview bounds and worker behavior reviewed.
- Regression matrix after documentation-only changes: `cargo clippy --all-targets --all-features -- -D warnings` clean; `cargo test --all-features` green.
- This pass produced two new CLI findings and one low/medium localization limitation; it is **not** a zero-finding clean pass.

---

## 🧾 Pass 5 subsystem, packaging, and CI audit (2026-08-25)

### New verified findings

#### P2 — usage/typo learning can be lost on daemon termination (`src/usage.rs:151-183`, `src/typos.rs:192-219`, `src/main.rs`)

- **Root cause:** both stores debounce writes for 2 seconds and rely on `Drop::flush`. In the daemon, `Arc<UsageStore>`/`Arc<TypoStore>` are captured by `Arc<Engine>` and GTK closures and the process normally terminates by signal/exit, so destructors do not run. This compounds the missing signal-shutdown path documented in Pass 3.
- **Failure pathway:** launch two aliases/results less than 2 s apart → final mutation sets dirty and schedules no timer → SIGTERM/exit → last record is never persisted. Earlier writes within the debounce interval are likewise lost.
- **Impact:** silent loss of user personalization, not security corruption. Atomic rename prevents partial-file damage when a write is in progress.
- **Remediation:** explicit shutdown hook must call `UsageStore::flush()` and `TypoStore::flush()` after stopping GTK; alternatively make `record` arm a one-shot `glib::timeout_add_local_once(SAVE_DEBOUNCE)` so the latest dirty state is written even without process shutdown. Keep the existing atomic `.tmp` rename.
- **Related existing tracker:** usage-race already captures the one-delayed-write race; this finding covers the distinct no-flush-on-signal loss.

#### P3 — translate durable-cache entries can exceed configured query limits (`src/providers/translate.rs:924-1063`)

- **Root cause:** `cache_put` stores the full `q` and `translated` strings without a length cap. Users can raise/lower `translate.max_chars` (100–5000), but every previously accepted request remains on disk for 14 days; 500 entries × 5 KB source + response can persist roughly 5 MB. `maybe_sweep_cache` bounds count, not bytes or per-entry size.
- **Failure pathway:** paste many 5000-char strings → 500-entry cache cap is reached only after writes; disk grows before sweep; lowering `max_chars` does not shrink existing entries.
- **Impact:** cache growth/privacy retention of past pasted text beyond the user's current preference; no memory-safety issue.
- **Remediation:** enforce a per-entry byte cap on read/write, store a hash-only key plus limited metadata if retaining source text is unnecessary, and make the sweep evict by total directory bytes as well as entry count.

#### P3 — bench daemon detection can inspect the wrong process (`src/bench.rs:386-423`)

- **Root cause:** `/proc/<pid>/cmdline` is NUL-separated. `cmd.contains("--daemon")` accepts a process with any argument equal to `--daemon` (including an unrelated program launched as `hark --daemon something`) and cannot distinguish an executable named `hark` from another binary whose argv includes this token. The `ps -C hark` prefilter bounds the executable-name mistake but not argument-position confusion.
- **Impact:** diagnostic-only incorrect attribution; no daemon or user data is modified.
- **Remediation:** split `cmdline` on NUL and require one argument exactly equal to `--daemon` (or ends_with semantics chosen deliberately); continue using `/proc` as authoritative rather than `ps` formatting.

#### P3 — release repo fallback can target the wrong GitHub repository (`scripts/package-release.sh:28-44`)

- **Root cause:** the fallback tests `[[ -z "$GITHUB_REPO" || "$GITHUB_REPO" == *"github.com"* ]]`, but after the preceding sed normalization an origin URL has already had `github.com` removed. Therefore a normalized non-empty remote from a fork is kept only when it does not contain `github.com`; the condition works for that case, but if `git remote get-url` emits a URL that sed cannot normalize (for example `ssh://git@github.com/user/repo.git`, where the host is not the prefix), the value still contains `github.com` and is silently replaced by the maintainer repository. A build from such a checkout emits an installer pointing at Vedant9500/Hark.
- **Failure pathway:** fork cloned through an SSH URL form not handled by the two anchored patterns → release script builds successfully → generated `install.sh` downloads from the upstream repository, potentially a different version than the packaged artifacts.
- **Impact:** incorrect update/download origin for locally produced release artifacts. Not code execution by itself; the downloaded tarball is not checksum-verified by the generated installer.
- **Remediation:** normalize SSH URLs with a robust expression or fail loudly when origin cannot be parsed; allow explicit `HARK_GITHUB_REPO` only as an override, never silently substitute the upstream repo. Include `SHA256SUMS` in generated installer verification before extraction.

#### P3 — generated online installer performs no checksum/signature verification (`scripts/package-release.sh:91-135`)

- **Root cause:** the generated installer downloads a tarball into `mktemp -d` and immediately extracts/executes `install.sh`; `SHA256SUMS` is generated for release publication but never consulted. TLS protects the transport, but GitHub release asset substitution/compromise and mirror/proxy environments are not covered.
- **Impact:** supply-chain execution of unverified downloaded shell code (CWE-494; OWASP A08:2021 Software and Data Integrity Failions category concept applies).
- **Remediation:** embed the expected version/arch SHA-256 at package generation, verify before extraction, or download and verify a detached checksum/signature. At minimum verify the exact asset digest and fail closed.

#### P3 — working tree contains 2.3 GB of untracked AUR build products under `packaging/aur/`

- **Verified evidence:** `git ls-files packaging/aur` contains only `PKGBUILD` and `.SRCINFO`, while the directory contains built packages, debug symbols, extracted source, package roots, logs, and a source tarball (`du -sh packaging/aur` = 2.3 GB). They are not tracked by git.
- **Impact:** repository/workspace bloat, accidental release contamination if packaging scripts ever copy broad paths, and stale audited copies that can mislead source-based review. The Pass 1 map initially surfaced duplicate source under this path.
- **Remediation:** delete local build artifacts or move them outside the worktree; add `packaging/aur/pkg/`, `packaging/aur/src/`, `*.pkg.tar.zst`, `*.tar.gz`, and build logs to `.gitignore`; never search extracted packaging source as authoritative current code.

### CI/packaging verification

- `bash -n` passes for every tracked shell script (`packaging/*.sh`, `packaging/hyprland/*.sh`, `scripts/*.sh`).
- CI gates the same local matrix: lockfile fetch, fmt, all-target/all-feature clippy, and all-feature tests. Release builds use `--locked`; AUR uses `cargo fetch --locked` then `--frozen`.
- Weekly security workflow runs both cargo-audit and cargo-deny. Current local cargo-audit result: **0 vulnerabilities**, one informational warning for `paste v1.0.15` (RUSTSEC-2024-0436); this independently corroborates the cargo-deny result.
- MSRV could not be verified: the environment has only Rust 1.97.1 and no rustup/1.70 toolchain/container runtime. This remains explicitly unproven rather than inferred from metadata.
- No shell injection was found in install scripts: variable interpolation writes user-controlled XDG paths into generated desktop files, but quoted redirection/`install` paths are used and scripts do not `eval` downloaded content. The integrity gap above remains the principal release-script issue.

### Store/cache clean-pass notes

- `UsageStore` and `TypoStore` cap entries, prune by frecency, serialize under locks, and atomically rename sibling temp files; no unbounded growth or lock-across-I/O issue was found.
- Typo Levenshtein is bounded to normalized aliases ≤24 chars and title comparisons, so pathological quadratic behavior is not reachable from arbitrary long queries.
- Translate success/failure memory maps are capped (256/64), TTL checks are saturating, disk sweep caps count, and FNV is used only as a non-security cache key. No denial or collision-security claim was violated.
- `Engine::search_calc_only` is correctly gated behind `bench`; no accidental production dependency on the feature was found.
- Bench resource parsing is otherwise defensive (`unwrap_or` fallbacks); its hard-coded `USER_HZ=100` can misreport CPU on unusual kernels but is diagnostic-only and already low-impact.

### Pass 5 verification log

- Audited `usage.rs` and `typos.rs` end-to-end, including lock scope, caps, atomic writes, frecency math, normalization, and learning eligibility.
- Audited translate memory/durable cache, TTL, sweep, key normalization, and parsers for cache integrity behavior.
- Audited `bench.rs` subprocess/proc parsing and feature gating.
- Audited tracked PKGBUILD/.SRCINFO, user install/uninstall, Hyprland helper, dev installer, source/release packaging, generated installer, and all three workflows.
- Ran shell syntax validation across all tracked scripts; shellcheck is not installed locally.
- Ran cargo-audit independently; result recorded above.
- Inspected git tracking versus filesystem to identify untracked build-product bloat.
- This pass produced five new findings; it is **not** a zero-finding clean pass.

---

## 🔁 Pass 6 UI helper & calculation-provider audit (2026-08-25)

### New verified findings

#### P2 — conversion-card rapid wheel navigation creates one-shot timer pile-up (`src/ui/rows.rs:478-489`)

- **Root cause:** every animated conversion swap removes/re-adds `hark-conv-swap` and unconditionally schedules another 220 ms `timeout_add_local_once` to remove the class. No timer source is stored or cancelled.
- **Failure pathway:** hold ↓/↑ through a prediction set → each swap schedules another one-shot while the preceding ones remain live → transient timer count grows (bounded only by input rate and one 220 ms lifetime each). Each callback retains a strong `conv_root` clone, so repeated wheeling briefly retains row widgets until their timers fire. Functionally, the newest class removal can occur before the newest animation duration in pathological ordering, cutting the pop short.
- **Impact:** UI resource churn and potentially inconsistent animation termination under rapid input; no permanent leak was proven because every timer is one-shot.
- **Remediation:** store a `RefCell<Option<glib::SourceId>>` per pooled row (or use an animation generation counter); on retrigger remove the prior source before scheduling, mirror the debounce pattern used elsewhere in the UI.

#### P3 — thumbnail writer does not enforce FreeDesktop privacy mode (`src/ui/thumbnails.rs:88-173`)

- **Root cause:** Hark creates `~/.cache/thumbnails/{large,normal}` when absent and writes `.{digest}.hark-tmp.png`/destination with inherited umask (verified default environment yields 0644 files; parent directories are already 0700 on this host). The thumbnail spec expects restricted modes when the cache is private, and other producers commonly create these directories/files 0700/0600.
- **Failure pathway:** first run on a host without an existing thumbnail cache → directories/files may be group/world-readable, exposing scaled previews of user files to local users if the parent cache directory itself is later created permissively or already exists permissively.
- **Impact:** local confidentiality issue only; on the audited host existing `large`/`normal` modes were 0700, so current files were not exposed. This is hardening rather than a demonstrated breach.
- **Remediation:** explicitly create/write directories and destination with 0700/0600 Unix modes; after `savev`, apply mode 0600 to the temp file before atomic rename.

#### P3 — unit-prediction table exposes an incomplete catalog (`src/providers/calc/units.rs:123-323,400-527`)

- **Root cause:** `to_base` implements many later-added categories (pressure, energy, power, angle, frequency), but `UNIT_ALIASES` only covers mass/length/volume/time/data/area/temperature. `predict_units` iterates only `UNIT_ALIASES`; additionally, aliases exist for units without `to_base` entries (notably `"t"` → tonne mass, which lacks a base-table arm) and several supported units (`hp`, `deg`, pressure/energy/frequency aliases) are absent from prediction. For an empty target, only hardcoded categories (`mass`, `length`, `volume`, `temperature`, `speed`, `data`, `time`, `area`) get preferred suggestions.
- **Verified consequence:** `try_conversion_predict("10 kg to ")` can still work via mass aliases, but tonne itself is not predicted despite being a listed mass alias because `to_base("t")` returns `None`; similarly, exact aliases that map to unsupported base entries never predict. Users see inconsistent prediction coverage across categories that exact conversion supports.
- **Impact:** wrong/incomplete suggestions/UX inconsistency, no arithmetic corruption in exact conversions.
- **Remediation:** generate prediction from one unit table containing aliases, canonical names, factors, and categories; add missing `to_base` units (including tonne and pressure/energy/power/angle/frequency aliases); derive preferred lists from category metadata rather than a duplicate hardcoded map.

#### P3 — fuel-economy converter overbroadly claims any query containing “ to ” (`src/providers/calc/fueleco.rs:53-100`) — **fixed 2026-08-26**

- **Root cause:** gating is only `lower.contains(" to ")`; conversion then splits the left side with `splitn(2, whitespace)` and requires the second token to parse as a fuel unit. A query such as `5 ton to lb` is not consumed (verified by the “fuel unit” gate), so most unit conversions remain safe. However, when fuel matching succeeds, `out_label` re-splits by the first `" to "` even if another ` to ` appears later; numeric parsing accepts `inf`/`NaN` through Rust `f64::parse` before the `<= 0.0` filter (NaN bypasses that comparison and propagates into arithmetic).
- **Verified consequence:** `NaN mpg to l/100km` proceeds through parsing; fuel conversion computes `L100_PER_MPG / NaN`, yielding NaN and formatting a nonsensical card instead of rejecting the input. Existing tests do not cover non-finite fuel input.
- **Fix:** `convert` now rejects non-finite input and non-finite output; regression test `rejects_non_finite_values` fails when the guard is removed.
- **Impact:** invalid calculator card (CWE-20 numeric input validation); no crash or panic.
- **Remediation:** require `value.is_finite() && value > 0.0`, and reject non-finite outputs before constructing the card. Add regression tests for `inf`, `NaN`, negative, and zero values.

### Clean areas verified in this pass

- `ResultRowPool` correctly attaches/detaches only the required 25 slots and clears drag paths on detach.
- `highlight_markup` escapes each Pango segment, handles multibyte char→byte mapping, and safely ignores duplicate/out-of-range indices.
- `ActionPanel` resets selection/firing on each open, has double-fire protection, and replaces all child buttons without retaining old callbacks.
- Scroll and size tweeners use weak `Rc` state plus cancellable tick IDs; no reference cycle or repeated frame-clock source was found.
- `ensure_row_visible` clamps target offsets, handles zero-height/unlaid-out rows, and cancels stale glides before fallback.
- FreeDesktop thumbnail URI is percent-encoded through `gio::File::uri`; mtime parsing uses checked PNG chunk arithmetic and `checked_add`; the MD5 implementation is used only for non-security cache naming.
- Action-panel focus-loss suppression is reset on failed open and by the destructive-alert completion callback; the popover close callback also clears open/firing state.
- Exact unit conversion arithmetic rejects cross-category conversions and temperature is handled outside the linear factor path.

### Pass 6 verification log

- Reviewed all remaining UI helper modules: `rows.rs`, `action_panel.rs`, `footer.rs`, `scroll_anim.rs`, `size_anim.rs`, `thumbnails.rs`, plus their call sites in `ui/mod.rs`.
- Reviewed remaining calculation providers, with line-level tracing of `units.rs` prediction/exact conversion and `fueleco.rs` parsing/arithmetic.
- Verified thumbnail file modes against the live user cache and current umask behavior.
- No source changes were made in this pass; findings are audit-only pending remediation.
- This pass produced four new findings; it is **not** a zero-finding clean pass.

---

## ➗ Pass 7 numeric/parser edge audit (2026-08-25)

### New verified findings

#### P3 — compound-interest card can present `inf`/`NaN` instead of rejecting the query (`src/providers/calc/financial.rs:34-55`) — **fixed 2026-08-26**

- **Root cause:** `amt()` delegates to `expr::eval_str`, which correctly rejects non-finite final values, but the separately captured rate and term are parsed directly as `f64` (`:43-44`). Validation only checks `rate <= 0.0 || t_years < 0.0`; NaN passes both comparisons. Extremely large compound terms also overflow `powf`. Unlike EMI (`:199`), the resulting `total` has no `is_finite()` guard.
- **Failure pathway:** `interest 1 crore at 5% for 1e308 years compounded` → `powf` overflows to infinity; `format_number` intentionally stringifies non-finite values rather than rejecting them (`util.rs:17-20`), producing an “Infinity” calculator card. A NaN rate/term from a future parser change would likewise pass all existing comparisons.
- **Fix:** interest now rejects non-finite rate/term and non-finite totals; regression test `interest_rejects_non_finite_results` fails when the guard is removed.
- **Impact:** invalid calculator answer (CWE-20 numeric validation); no panic or memory-safety issue. `format_number`’s non-finite passthrough is intentional formatting, not validation.
- **Remediation:** require `rate.is_finite()`, `t.is_finite()`, `p.is_finite()`, and reject `!total.is_finite() && !interest_amt.is_finite()` before constructing the card. Add regression tests for overflow and NaN inputs, mirroring the EMI guard.

#### P3 — financial arithmetic lacks domain-range guards beyond sign checks (`src/providers/calc/financial.rs:89-160,267-365`)

- **Root cause:** discount/GST accept any finite positive percentage, rule-72 accepts arbitrarily small positive rates, and percentage-change/hourly conversion only guard explicit zero divisors. Very large inputs can overflow and be displayed via `format_number`, while semantically invalid cases (e.g. `200% off`, producing a negative discounted total; `72 at 1e-300%`, yielding an astronomical but finite “years to double”) remain valid according to no documented domain constraints.
- **Verified distinction:** no non-finite input can enter these paths because `amt()` uses `eval_str`, and ordinary literals are regex-bounded decimal strings; their risk is overflow after multiplication or nonsensical domain output, not NaN injection.
- **Impact:** low-severity wrong/unhelpful answers, not crashes.
- **Remediation:** define and enforce provider-specific bounds (e.g. percentage 0–100 where “off/GST” semantically implies it, reasonable rate/term ranges) and reject non-finite derived outputs at one shared boundary before `card_result`.

#### P3 — currency/unit conversion value parsing rejects magnitude-suffixed amounts (`src/providers/calc/units.rs:7-42`, `src/providers/calc/currency.rs:58-85`)

- **Root cause:** `RE_CONVERT`/`RE_CONVERT_PARTIAL` capture a bare decimal number and parse it with `str::parse::<f64>()`; unlike financial amounts, they do not call `expr::eval_str` or accept `k/mil/crore` suffixes.
- **Verified pathway:** `10k kg to lb` does not match the unit grammar because `10k` is not a valid bare decimal token; plain `10 kg to lb` works. Currency likewise supports `100 usd to inr` but not `1.5k usd to inr`, while the same magnitude spelling works in finance.
- **Impact:** inconsistent natural-language coverage across calculator providers; no incorrect arithmetic when a query does match.
- **Remediation:** share one amount grammar/parser (fraction + decimal + magnitude suffix) across unit, currency, and finance providers, preserving unit-letter collisions (`m` as meters) through whitespace requirements where already intended.

#### P3 — fraction parser accepts non-finite numerator/denominator literals (`src/providers/calc/util.rs:5-15`) — **fixed 2026-08-26**

- **Root cause:** `parse_qty_number` parses each fraction half with Rust float parsing and only checks `b == 0.0`; `NaN/1`, `1/NaN`, `inf/2`, and `2/inf` are accepted. This helper is used by unit conversion quantity parsing, so malformed literals can become non-finite conversion factors before reaching `format_number`.
- **Impact:** the regex layer currently limits ordinary decimal inputs and therefore prevents these strings from reaching it in the main unit path, making this latent rather than directly user-triggered today. It is still an unsafe shared helper for future call sites.
- **Fix:** `parse_qty_number` now rejects non-finite parts and non-finite results for fractions and plain values; regression test `parse_qty_number_rejects_non_finite` fails when the guard is removed.
- **Remediation:** require both numerator and denominator finite and denominator nonzero; add direct unit tests for `NaN`, `inf`, and negative denominators.

### Clean areas verified in this pass

- `expr::eval_str` rejects non-finite results, zero divisors/modulo, incomplete token consumption, unknown identifiers/functions, and factorial outside the finite 0–170 integer domain. Its precedence printer mirrors the parser grammar.
- Number tokenization stays on byte boundaries, accepts scientific notation only when syntactically complete, and leaves unknown alphabetic suffixes untouched for later parsing.
- Date relative-time calculations clamp to ±100 years before converting to chrono milliseconds, avoiding silent huge-cast date corruption.
- `ymd_between` uses checked chrono date construction, month-end clamping, and cannot loop past `b`; leap-day spans are regression-tested.
- Battery sysfs parsing uses unsigned integers/saturating multiplication and rejects non-finite, zero, or >48 h estimates.
- Clock-range arithmetic is bounded by regex width and hour/minute parsing, preventing ordinary integer overflow.
- Currency conversion’s FX arithmetic is downstream of `FxStore` finite/positive rate validation and no bypass was found.
- `format_number` explicitly handles non-finite values, making any “Infinity”/“NaN” card a parser-validation bug rather than a formatting panic.

### Pass 7 verification log

- Reviewed `expr.rs`, `math.rs`, `financial.rs`, `units.rs`, `currency.rs`, `util.rs`, `datetime.rs`, `duration.rs`, `battery.rs`, and `fueleco.rs` arithmetic/validation boundaries.
- Traced shared parsers (`eval_str`, `amt`, `parse_qty_number`, `relative_secs`, `format_number`) and compared their validation guarantees.
- Cross-checked regex grammars against their post-parse numeric validation and existing tests.
- No source changes were made in this pass.
- This pass produced four new findings; it is **not** a zero-finding clean pass.

---

## 🎲 Pass 8 quickwin/timezone audit (2026-08-25)

### New verified findings

#### P2 — invalid Roman numeral strings are accepted and silently converted (`src/providers/calc/quick.rs:160-186`)

- **Root cause:** `from_roman` only walks characters right-to-left, subtracts a smaller value after a larger one, and rejects zero/overflow. It does not validate canonical Roman-numeral syntax or the standard 1–3999 range on reverse conversion. `to_roman` enforces the range, but reverse conversion does not.
- **Mechanical examples verified against the implementation:** `IIII` → 4, `IVIV` → 8, `IXIX` → 18, `VIVI` → 10, `VV` → 10, `LL` → 100, `DD` → 1000, `IM` → 999, `IC` → 99, `XD` → 490, `XM` → 990, `MIM` → 1999. All are non-canonical, and subtractive prefixes such as `IM`/`IC`/`XD`/`XM` were never legal Roman numerals.
- **Impact:** authoritative-looking wrong calculator answers (`roman IM` presents “999” as a conversion). No panic or security impact.
- **Remediation:** parse with a strict canonical-form regex (`^M{0,3}(CM|CD|D?C{0,3})(XC|XL|L?X{0,3})(IX|IV|V?I{0,3})$`) and reject empty/zero matches, or generate every canonical numeral 1–3999 into a lookup map. Add regression cases for every malformed family above.

#### P3 — ambiguous timezone prediction can map unrelated prefixes to a city (`src/providers/calc/timezone.rs:288-330`)

- **Root cause:** `predict_tz` includes matches when `p.starts_with(alias)` **or** `alias.starts_with(p)`, then sorts by score but gives every “other” match 100 and falls back to lexical alias order. Short ambiguous prefixes can therefore resolve to a seemingly arbitrary city rather than surfacing ambiguity.
- **Verified logic path:** a prefix that is not an exact/startswith alias but is a superstring of an alias (for example a phrase beginning with `la`, `ny`, or `sf`) is accepted for the contained alias; a short prefix matching multiple aliases chooses lexicographically first among equal 100 scores. No single deterministic wrong city is asserted here without runtime GTK-independent evidence, so the confirmed defect is ambiguity acceptance/arbitrary tie-breaking, not a specific pair.
- **Impact:** plausible wrong timezone answer for ambiguous input.
- **Remediation:** when more than one distinct timezone matches, return an ambiguity/error result (or produce prediction rows as the unit picker does); require the alias-prefix direction for prediction and reserve superstring matching for exact normalized lookup.

#### P3 — timezone conversions silently resolve nonexistent local times to no result during DST gaps (`src/providers/calc/timezone.rs:501-543`)

- **Root cause:** `and_local_timezone(from_tz).single()?` intentionally rejects ambiguous/nonexistent local timestamps. Rejecting a nonexistent spring-forward time is safe, but because the provider returns `Option<SearchResult>`, users receive no explanation—the query falls through to other providers or empty state.
- **Impact:** usability/error-feedback defect, not an incorrect conversion. Ambiguous fall-back times are also silently discarded rather than showing both possibilities.
- **Remediation:** distinguish `None` from a conflict using `LocalResult::None`/`Ambiguous`, and produce an explicit error card (“02:30 does not exist in Europe/Berlin on this date”) or choose/present both offsets explicitly.

### Clean areas verified in this pass

- `to_roman` correctly enforces 1–3999 and canonical output construction.
- Password/UUID generation uses OS CSPRNG when `/dev/urandom` is available; password rejection sampling (`b < 248`) is unbiased over the 62-character set; random integer sampling is span-safe and modulo-bias-free.
- Height parsing rejects nonpositive heights, validates feet/inch semantics after decimal-shift normalization, and height conversion rounds inches with carry into feet.
- BMI rejects nonpositive height/weight and categorizes finite BMI values.
- Steps conversion arithmetic is bounded by regex digit counts and uses fixed factors.
- Cooking paths require positive quantities/ingredient matches, reject non-ingredient butter-stick queries, and scale factors are positive.
- Oven conversion accepts explicit Fahrenheit/Celsius and performs fixed ±20 °C fan adjustment.
- Clock parsing bounds hour/minute/second and correctly normalizes AM/PM.
- Offset timezone lookup validates current half-hour zone offsets and rejects unmatched fractional offsets rather than truncating.
- `build_tz_conversion` uses checked chrono constructors and correct timezone conversion; no arithmetic bug was found beyond DST conflict feedback.

### Pass 8 verification log

- Reviewed quickwin Roman, BMI, height, steps, random, UUID, and password paths.
- Reviewed timezone alias resolution, prediction, clock parsing, offset mapping, and conversion construction.
- Emulated `from_roman` across malformed canonical-form families to verify each claimed wrong result.
- Reviewed cooking quantity, density, recipe scaling, and oven-conversion arithmetic.
- No source changes were made in this pass.
- This pass produced three new findings; it is **not** a zero-finding clean pass.

---

## 🔗 Pass 9 engine integration & state audit (2026-08-25)

### New verified findings

#### P2 — hard-coded minimum query length silently suppresses file search (`src/engine.rs:251-266`)

- **Root cause:** apps and files are only searched when `q.len() >= 2`. `q.len()` is UTF-8 byte length, but the practical behavior is broader: even valid ASCII single-character file-name queries (`f a`, `f *.c`, or an indexed single-character filename) are excluded unless they satisfy `force_files`.
- **Verified logic:** `force_files` does rescue explicit path/glob/scoped queries, but a bare one-character query with an app prefix that does not classify as path-shaped reaches the `q.len() >= 2` gate and gets neither app nor file results. Empty-results recents are unrelated and only shown for an entirely empty query.
- **Impact:** intentional anti-noise behavior may suppress legitimate exact one-character indexed names; the condition also uses bytes rather than chars, though all query syntax recognized here is ASCII.
- **Remediation:** use `q.chars().count()` for clarity and define the intended policy explicitly. If single-character noise must be suppressed, apply it only to fuzzy app/file matching and allow exact indexed-name or glob matches through.

#### P3 — settings-generated config writes use a shared fixed temp filename (`src/config.rs:820-843`)

- **Root cause:** every `ConfigStore::save()` writes `config.json.tmp`, chmods it, then renames. `update()` serializes mutations under a write lock, but `save()` itself takes only a read snapshot and does not serialize the write/rename sequence. Two threads or UI callbacks saving concurrently can truncate each other’s temp file or rename a partially written payload.
- **Current call-site context:** settings handlers run on the GTK main loop, so normal GUI use is single-threaded. Background/config-loading paths are not concurrent writers today. This is a latent concurrency hazard and generalizes existing finding N16 from usage/typos to config.
- **Impact:** torn config write if a non-main-thread save path is added; no current user-visible trigger verified.
- **Remediation:** hold a dedicated save `Mutex` across tmp-write/chmod/rename, or use a unique temporary name plus atomic rename. Preserve 0600 permissions and fsync the file and parent directory for crash consistency.

#### P3 — calculator providers are queried in fixed priority order rather than specificity order (`src/providers/calc/mod.rs:44-114`)

- **Root cause:** provider dispatch is an ordered `if let Some` chain. Earlier broad parsers can own a query before later providers that may be more specific. Examples verified structurally: `try_cooking` runs before general `try_conversion`; `try_currency` runs before `try_conversion`; timezone prediction runs before both currency and unit conversion.
- **Impact:** ambiguous natural-language queries can be classified by provider order rather than semantic specificity. No specific wrong arithmetic case was proven in this pass because regex gates mostly disambiguate, so this is a design robustness finding rather than a confirmed wrong answer.
- **Remediation:** score candidate results across providers or group parsers into mutually ambiguous classes (money, units, cooking, timezone) and choose by grammar specificity/full-consumption confidence.

#### P3 — settings UI displays unsanitized local state after clamp (`src/ui/settings.rs:1717-1746,1774-1808`)

- **Root cause:** stepper callbacks calculate `next` by mutating config, then `ConfigStore::update` sanitizes/clamps it. If the callback’s locally computed `next` is already within bounds the label is correct, but the callback’s initial fallback value and local arithmetic duplicate sanitization. At a boundary, sanitization could differ from the local computation if ranges drift. This is a maintainability/UI-state drift hazard rather than a currently reproduced mismatch.
- **Impact:** potential stale/incorrect UI label after future range changes; persisted config remains sanitized.
- **Remediation:** return the post-sanitization value from `ConfigStore::update`, or snapshot config after update and update all labels from the authoritative snapshot.

### Clean areas verified in this pass

- Engine result dedup retains the first provider occurrence before score-based sorting; ordering is deterministic through score, kind rank, and title.
- Usage boosts and typo-alias boosts use saturating arithmetic and cannot wrap scores.
- `apply_typo_alias` boosts an existing target or resolves/injects it with a bounded score; no unbounded result injection was found.
- Empty-state recents cap path resolutions at eight and overall results at fifteen.
- FX refresh is coalesced with compare-exchange and 15-minute backoff; the worker swaps the cache under a write lock and always clears inflight. No worker storm or deadlock was found.
- FX network and disk rate validation reject zero/negative/non-finite rates; conversion rejects non-finite amount/rates/output and has regression coverage.
- FX cache persistence uses directory ownership/mode checks and `O_NOFOLLOW`; no symlink-follow overwrite path was found on Linux.
- Config snapshots use Arc swap; provider reads are lock-bounded and hot paths use `with` rather than cloning full config trees.
- Theme settings reload through the coalesced theme manager debounce; appearance-state synchronization on launcher show was verified.

### Pass 9 verification log

- Reviewed `Engine::search` provider gating, score boosting, typo injection, dedup, sort, truncation, empty-state recents, and alias-target resolution.
- Reviewed `CalcProvider` dispatch ordering and all direct provider handoffs.
- Reviewed FX memory/disk/network cache lifecycle, concurrency, validation, and persistence security.
- Reviewed ConfigStore update/sanitize/save and representative settings propagation handlers.
- No source changes were made in this pass.
- This pass produced four new findings; it is **not** a zero-finding clean pass.

---

## 🗂 Pass 10 file-search internals audit (2026-08-25)

### New verified findings

#### P3 — live deep-cache misses are only visible to one thread (`src/providers/files/live_cache.rs:111-145`, `files/mod.rs:180-248`)

- **Root cause:** `contains()` returns whether an entry exists but does not communicate a negative miss to `search_with()`. The UI’s deep scheduler first calls `contains()`; if no entry exists it launches a worker. Meanwhile another `search_with(DeepMode::Async)` call can also miss and launch a walk because there is no “pending” state. The worker layer (`ui/mod.rs`) is separately single-flighted by generation/latest-job, but `FileProvider::search_with` itself permits duplicate synchronous/deep walks before the first result is cached.
- **Impact:** redundant filesystem walks in callers that do not use the UI’s single-flight wrapper; no stale result or corruption. Current daemon UI path is mitigated by `schedule_deep_job`.
- **Remediation:** add a pending marker to `LiveCache` (or return a `Lookup::{Hit, Pending, Miss}`), register pending on first request, and let only the owner walk; time out stale pending entries.

#### P3 — live cache key normalization discards leading query semantics beyond force-files prefixes (`src/providers/files/live_cache.rs:100-108`)

- **Root cause:** `key_for()` strips `f`/`file`/`folder` prefixes and lowercases, but does not distinguish a bare query from one with path/glob/scoping syntax whose search semantics can change with index/config state. Cached results are immutable for 5 minutes even after excludes, roots, mounts, or index fingerprints change; `clear_live_cache` is called only after trash/rebuild paths.
- **Verified pathways:** modifying excludes or extra roots triggers a reindex, but no automatic `LiveCache::clear`; stale positive hits for a newly excluded directory can still merge into UI results for up to five minutes. Mount style changes likewise do not invalidate cached result subtitles.
- **Impact:** stale results/labels after settings changes for a bounded TTL; no security issue.
- **Remediation:** include a config/index fingerprint (already available to `IndexState`) in cache entries, or clear the live cache whenever the index fingerprint/config roots/excludes/path style changes.

#### P3 — merged index/live results are not rescored against current usage state (`src/providers/files/mod.rs:244-320`)

- **Root cause:** index-only search computes scores at query time. Cached deep hits were scored when first walked, then `merge_cached` combines them with current index hits but does not reapply the engine’s usage boost (`Engine::search` applies boosts only to its initially collected provider results, before cached results may be merged inside `files.search_with`). Since files provider applies boosts internally only through index scoring, previously cached deep hits keep their historical scores.
- **Impact:** ordering can differ between the first deep result and a cached retyped result if usage changed in between. No invalid/crashing result.
- **Remediation:** store a score base in cached entries and apply current usage boosts during merge, or normalize all file scores after merging.

### Clean areas verified in this pass

- `LiveCache` LRU recency insertion/removal is internally consistent, including reinsertion and touch; regression tests cover both.
- Empty deep results are negative-cached with a shorter TTL and still distinguish “walked, no hits” from absent.
- Cache values are shared via `Arc<[SearchResult]>`; `get` clones only the Arc and UI conversion is explicit.
- Heap ranking is bounded to 25 results, ties prefer shallower paths, and duplicate index entries are filtered before heap insertion.
- Hot-path early exit only fires for ≥4-char queries with a strong exact/prefix hit; otherwise a full scan and bounded fuzzy pass run.
- Fuzzy spans are dropped when case folding changed char count, and title substring fallback is used safely.
- Path boost arithmetic is bounded by depth and fixed constants; low-value paths require explicit name containment.
- Deep jobs are planned under the index read lock but walks occur after lock release, avoiding lock-across-filesystem-I/O.
- Trash clears the live cache before refresh.
- Auto-promotion walks a bounded ancestor chain, rejects forbidden roots, canonicalizes before comparison, caps deep roots at 32, and rechecks configuration before mutation.

### Pass 10 verification log

- Reviewed `live_cache.rs`, `rank.rs`, and their integration in `FileProvider::search_with`.
- Reviewed deep-cache lifecycle, index/deep result merging, ranking heap bounds, hot-path short circuit, and fuzzy budget.
- Reviewed open-path configuration propagation and auto deep-root promotion.
- No source changes were made in this pass.
- This pass produced three new findings; it is **not** a zero-finding clean pass.

---

## 🧩 Pass 11 settings & search planner audit (2026-08-25)

### New verified findings

#### P3 — Open With uses an unstable row-index → app mapping (`src/ui/open_with.rs:92-136`)

- **Root cause:** app rows store `gio::AppInfo` in one `apps_rc` vector and activation reads `row.index()`. The extra “System default” row is appended after app rows, but the mapping assumes app row indexes exactly equal positions in `apps_rc`. It currently does. However, any future separator, filtering, hidden row, or reorder breaks this indirect positional contract. The special row is recognized by widget name while app rows are identified only by mutable list position.
- **Impact:** maintainability/latent incorrect app activation; no current user-visible mismatch verified.
- **Remediation:** set each app row’s widget name to a stable app id and resolve activation by id (or store a map from row name to `AppInfo`), eliminating reliance on `ListBoxRow::index()`.

#### P3 — Open With performs synchronous MIME/app enumeration on the GTK main loop (`src/ui/open_with.rs:34-37`, `305-353`)

- **Root cause:** `show_open_with_picker` calls `content_type_for_path` (GIO `query_info`), `apps_for_content_type` (recommended/all/default app enumeration), and `content_type_get_description` before constructing widgets. All run synchronously in the GTK callback. This is already tracked as latent hazard `ow-sync`; this pass verifies that no async path or cache has since been added.
- **Impact:** UI jank on cold MIME caches, network-backed content types, or large application registries.
- **Remediation:** enumerate off-thread, then popup on the main loop with a generation token so a stale picker does not appear.

#### P3 — deep-search specificity thresholds use UTF-8 byte lengths (`src/providers/files/search/deep.rs:411-445`)

- **Root cause:** `looks_specific_for_deep` gates on `q.len() < 3` and `q.len() >= 5`, which are byte lengths, not character counts. A five-character CJK filename is 15 bytes and passes as “specific”; a two-character CJK name (6 bytes) also passes despite being the intended short-noise case. Conversely, combining-mark-heavy scripts can distort the threshold. The q lowering itself preserves Unicode case folding.
- **Impact:** inconsistent deep-walk cost classification across scripts, potentially triggering broad walks for very short non-Latin names. No panic or wrong result.
- **Remediation:** use `q.chars().count()` consistently, and consider script-aware minimum widths.

#### P3 — scoped-query confidence accepts any `.ext`-like token without ext whitelist (`src/providers/files/search/plan.rs:210-231`)

- **Root cause:** `name_looks_like_file` accepts any nonempty stem plus ≤8 alphanumeric characters after a dot. Strings such as `version.1` or `node.20` are treated as filename-like and can force a scoped deep walk even when the user meant an app/version phrase. Existing disambiguation prevents absolute-path theft, but confidence remains broad.
- **Impact:** unnecessary live filesystem walks and files-mode ownership for ambiguous phrases; no security issue.
- **Remediation:** require known/common extension families for bare confidence, or combine the dot heuristic with additional context (scope path-likeness, index hit, glob char).

### Clean areas verified in this pass

- Open With uses weak popover captures in activation and close handlers, avoiding the previous popover reference cycle; close unp parents the popover.
- Double-fire protection resets on failed launch and the system-default path does not leave `firing` stale because the popover is dismissed.
- App enumeration deduplicates by app id/name and caps at 40 entries.
- Settings default-app rows update labels/reset sensitivity after config mutation and use stable category objects.
- App picker replaces any existing picker page, resets the open flag, closes cleanly via Back/Escape/selection, and filters by tooltip/id.
- Settings list refills remove all prior children before rebuilding, preventing duplicate rows in these paths.
- Scoped parsing lowercases names/segments, preserves absolute roots, strips empty components, and uses `find`-returned boundaries for keyword slicing.
- Deep planning correctly avoids broad extension-only globs, absolute non-glob completions, incomplete scope hints, and empty queries.
- Strong index results suppress redundant deep walks using file/folder-specific scores rather than mixed app scores.

### Pass 11 verification log

- Reviewed `open_with.rs` end-to-end, including app mapping, launch paths, lifecycle, MIME lookup, and app enumeration.
- Reviewed settings default-app category rows, app picker construction/filtering/selection, and list refill behavior.
- Reviewed scoped-query confidence, segment normalization, and deep-search specificity gates.
- No source changes were made in this pass.
- This pass produced four new findings; it is **not** a zero-finding clean pass.

---

## 🖼 Pass 12 preview/theme/glob audit (2026-08-25)

### New verified findings

#### P2 — audio previews have no file-size or worker single-flight bound (`src/ui/preview.rs:808-929`, `1115-1172`)

- **Root cause:** image/video/PDF previews pass through the preview panel’s single `worker_busy`/`inflight` scheduler, but `queue_audio_load` directly spawns a new `std::thread` on every debounce callback. There is no `MAX_AUDIO_BYTES` gate; `_fp` is accepted but unused. Each queued audio path can therefore open and parse an arbitrarily large media file off-thread, and rapid successive selections can create multiple concurrent lofty parsers even though only the latest generation renders.
- **Failure pathway:** rapidly select several multi-hundred-MB audio files → each debounce fires (latest generation changes, but old threads already spawned) → concurrent tag/picture reads consume memory and I/O; stale threads finish and their bounded-channel receiver is dropped, but parsing work still completes.
- **Impact:** resource exhaustion/UI slowdown (CWE-400); no GTK-thread blocking because work is off-main.
- **Remediation:** route audio loads through the same single-flight worker scheduler, impose a size cap before parsing, and pass `_fp` into any cache/dedup decision. Add a cancellation-aware parser if lofty supports progressive reads; otherwise bound concurrent stale workers to one.

#### P3 — preview panel’s synchronous metadata probe remains on the GTK main loop (`src/ui/preview.rs:429-452`)

- **Root cause:** `PreviewPanel::update` calls `std::fs::metadata(path)` synchronously to determine directory status, size, mtime, and previewability before queueing work. This is existing tracked finding #25; Pass 12 verifies it is still present and additionally that it occurs before any debounce, so every selection change pays the probe immediately.
- **Impact:** UI stall on NFS/FUSE/removable media paths.
- **Remediation:** derive previewability from the indexed path and item kind, then perform metadata/fingerprint probing in the worker and reconcile with a generation token.

#### P3 — CSS accepts 3/4/8-digit hex where RGB byte parsing requires six digits (`src/theme/mod.rs:226-232`, `theme/css.rs:3-28`)

- **Root cause:** `sanitize_hex` deliberately permits `#fff`, `#ffff`, and `#RRGGBBAA`, but `is_light_theme` and `rgba` strip only `#` and then test only `len < 6`. A three-digit `#fff` reaches slicing `h[0..6]`, which is short, and therefore falls into the fallback path; four- and eight-digit forms similarly either fall back or use only the first RGB bytes without honoring alpha. The behavior is safe (no panic; fallback color), but inconsistent with the sanitizer’s advertised accepted formats.
- **Impact:** valid shorthand/RGBA scheme colors are silently replaced by fallback RGB; no CSS injection because all forms remain ASCII hex.
- **Remediation:** normalize 3/4-digit shorthand and 8-digit RGBA to the actual values CSS should use, or restrict `sanitize_hex` to six ASCII hex digits if shorthand is not intended.

#### P3 — literal glob candidates can score lower than glob matches (`src/providers/files/search/glob.rs:100-117`)

- **Root cause:** literal name patterns without `*`/`?` score 50,000 on exact match, but a mere prefix gets 40,000 and substring gets 32,000. A wildcard match receives a base 38,000 plus segment boosts and an extension-glob bonus. Therefore `readme` (literal prefix, no scope) can rank below `*.md` or a pattern with a path segment even when the literal prefix is a more confident user intent.
- **Impact:** ordering inconsistency between literal and wildcard glob results in mixed queries; no incorrect inclusion/exclusion.
- **Remediation:** rank literal prefix/substring above wildcard matches by default, or fold query type into a single confidence score before scope boosts.

### Clean areas verified in this pass

- Preview image/video/PDF loads use path + mtime/size fingerprints, bounded image size, 2 GiB video/PDF cap, debounce, generation checks, and one worker slot.
- FreeDesktop thumbnail decode prefers the shared cache before source decode.
- Converter scratch directories are forced 0700, output files are `O_CREAT|O_EXCL` 0600, names contain OS entropy, and ffmpeg/pdftoppm are deadline-killed and reaped.
- Audio worker results are generation/path gated before rendering, with icon fallback when metadata parsing fails.
- `sanitize_hex` prevents non-ASCII or malformed scheme values from entering CSS; theme values are never interpolated outside rgba/foreground constructions.
- Theme reload uses CSS provider parsing rather than Pango markup injection.
- Glob matching operates on Unicode chars, handles `*`/`?` correctly including trailing stars, and path component scans advance on UTF-8 boundaries.
- Absolute glob searches deduplicate live/index hits and skip excluded entries in the live path.

### Pass 12 verification log

- Reviewed preview worker lifecycle, generation guards, cache fingerprinting, media size gates, converter sandboxing/timeouts, and audio parsing.
- Reviewed theme scheme loading, sanitization, CSS construction, and provider injection.
- Reviewed indexed and absolute glob matching/scoring semantics.
- No source changes were made in this pass.
- This pass produced four new findings; it is **not** a zero-finding clean pass.

---

## 🗃 Pass 13 state stores, config, UI helper & calc-provider depth audit (2026-08-26)

Areas targeted for increased depth beyond Passes 1–12: the persistent state stores (`typos.rs`, `usage.rs`) and `config.rs` (load/save/durability/mount discovery), the UI helper widgets and animation/drag layers (`ui/thumbnails.rs`, `ui/rows.rs`, `ui/footer.rs`, `ui/action_panel.rs`, `ui/scroll_anim.rs`, `ui/size_anim.rs`, `ui/dnd.rs`), and a fuzz-style sweep of the remaining calc providers (`cooking.rs`, `duration.rs`, `unitmath.rs`, `fueleco.rs`, `datetime.rs`, `battery.rs`). All line numbers below verified against current source.

### New verified findings

#### P2 — malformed store JSON is silently wiped with no backup (`src/typos.rs:76-81`, `src/usage.rs:44-49`)

- **Root cause:** both stores load with `fs::read_to_string(&path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()`. Any parse failure — truncated file, empty file, or a single wrong-typed field (`"count": "2"` fails the whole `TypoFile`/`UsageFile` struct) — falls through to the empty default with no log and no backup, unlike `config.rs` which does `backup_invalid_config` (`config.rs:856-862`). The next debounced save then atomically overwrites the user's entire learned-alias and usage history with the empty default, making the loss unrecoverable.
- **Failure pathway:** crash/power loss between `fs::write` and durable rename (compounded by the no-fsync issue below), disk corruption, or hand-editing → load silently wipes → save persists the wipe.
- **Impact:** silent destruction of user learning data (CWE-754 / CWE-755).
- **Remediation:** mirror the config.rs pattern — on parse error, `fs::copy` to `*.json.invalid`, log, and optionally salvage per-entry via `serde_json::Value` → per-key `from_value` with error skip.

#### P2 — empty exclude pattern panics via `windows(0)` on first indexed walk (`src/config.rs:1185-1195`, `1221-1227`)

- **Root cause:** `ExcludeSet::from_list` splits each pattern on `/` and filters out empty components; an `index.exclude` entry of `"/"` or `"//"` therefore pushes an **empty** pattern `Vec`. `matches()` then calls `comps.windows(pattern.len())`, and `slice::windows` panics when the window length is 0. `load()` never sanitizes the exclude list, so `{"index": {"exclude": ["/"]}}` survives load and aborts the first indexing walk.
- **Impact:** user-config-triggered daemon panic at startup/first index (CWE-20; panic, not memory unsafety).
- **Remediation:** skip patterns that filter to empty (`if !p.is_empty() { patterns.push(p) }`) in `from_list`, plus a regression test for the `"/"` and `"//"` exclude values.

#### P2 — stale drag-end timer fires mid-drag, hiding the launcher during an active drag (`src/ui/dnd.rs:178-184`, `192-225`)

- **Root cause:** `end_session` arms a one-shot `timeout_add_local_once(1200 ms)` that clears `ignore_focus_loss`, re-grabs exclusive keyboard, and hides every visible non-active window. `begin_session` re-arms the session flags for a new drag but never cancels a pending timer from a previous drag (no `SourceId` is stored). If drag #2 begins within 1.2 s of drag #1 ending, the stale timer fires mid-drag: it clears the auto-hide guard while focus is on the drop target, re-grabs the exclusive keyboard the module deliberately avoids during drags (comment at `dnd.rs:6-8`), and — because Hark is typically not the active surface during a drag — executes `w.set_visible(false)` on the launcher surface mid-drag, cancelling the Wayland data source. The module's own comment (`dnd.rs:188-191`) warns that hiding too early makes "the drop silently do nothing"; this reintroduces exactly that, and `drag-end` for the second drag may never arrive, leaving `session.active == true` (which blocks list rebuilds).
- **Impact:** dropped/cancelled drags and a wedged session when two drags start within 1.2 s (common when dragging several files in sequence). CWE-362 / CWE-404.
- **Remediation:** store the pending `glib::SourceId` in `DragSession`; `begin_session` removes any pending timer before arming flags; additionally make the timer body a no-op when `session.active.get()` is true.

#### P2 — NaN/inf fuel-economy values pass the `<= 0.0` guard (`src/providers/calc/fueleco.rs:59-67`)

- **Root cause:** the value is parsed with bare `str::parse::<f64>()`, which accepts `"nan"`, `"inf"`, `"infinity"` spellings; the only guard is `value <= 0.0`, which is false for NaN, and `inf > 0` also passes. `nan km/l to mpg` produces a `NaN mpg` card (`format_number` stringifies non-finite values by design, `util.rs:16-18`); `nan mpg to l/100km` divides by NaN.
- **Impact:** garbage calculator card (CWE-20 numeric validation).
- **Remediation:** add `!value.is_finite()` to the rejection guard at `fueleco.rs:66`, mirroring the EMI/financial guards.

#### P3 — atomic store writes are not fsynced; crash can leave a truncated store (`src/typos.rs:215-219`, `src/usage.rs:175-179`, `config.rs:826-843`)

- **Root cause:** all three stores write via `fs::write(tmp)` + `fs::rename(tmp, path)`. `fs::write` performs no `fsync`; on power loss the rename can be journaled while the data blocks are not, leaving a zero-length or truncated file on next boot — which the silent-wipe issue above then destroys. The orphaned `*.json.tmp` is also never cleaned up on rename failure.
- **Impact:** durability gap (CWE-755); interacts with the wipe finding to destroy data after a crash.
- **Remediation:** `File::create` + `write_all` + `sync_all` on the tmp before rename (optionally sync the parent dir), and remove the tmp on rename failure.

#### P3 — `resolve_icon_name` indexes an empty candidate slice (`src/ui/rows.rs:727-729`)

- **Root cause:** `let fallback = candidates[candidates.len() - 1];` runs before any length check. Called from mode-bar icon resolution (`ui/mod.rs:2271-2300`) inside GTK callbacks, so a future empty call site panics on the daemon's single main loop. All current call sites pass non-empty literals — latent, not live (CWE-125-style index panic).
- **Remediation:** `candidates.last().copied().unwrap_or("text-x-generic")`.

#### P3 — stacked 220 ms swap-class cleanup timers truncate a newer swap animation (`src/ui/rows.rs:479-487`)

- **Root cause:** each animated conversion swap removes+re-adds `hark-conv-swap` and arms an independent one-shot timer that removes the class. Two swaps within 220 ms: the older timer fires mid-flight and strips the class, truncating the newer keyframe. Each timer also strongly clones `conv_root`, pinning the widget subtree up to 220 ms past teardown — bounded and self-terminating, so cosmetic.
- **Remediation:** store a `RefCell<Option<glib::SourceId>>` per row; remove the previous timer before arming the new one.

#### P3 — mount skip uses raw substring `contains("EFI")`/`contains("efi/")` (`src/config.rs:1042-1048`)

- **Root cause:** any mount whose path merely contains those substrings is skipped from indexing: `/mnt/KEFIR` (contains `"EFI"`), `/media/u/Defi/` (contains `"efi/"`), `/run/media/u/Reference`. The user's volume silently never appears in the index.
- **Remediation:** match case-insensitively on whole path components (`target.split('/').any(|c| c.eq_ignore_ascii_case("efi"))`).

#### P3 — `discover_mounts()` re-executed per `deep_roots` entry at load (`src/config.rs:738-742` → `86-89` → `76-78`)

- **Root cause:** `load()`'s `deep_roots.retain(|s| !is_overbroad_deep_root(s))` walks the closure chain down to `discover_mounts()`, which re-reads `/proc/self/mounts` **and** rescans `/mnt`, `/media`, `/run/media` (`config.rs:1005-1035`) once per retained root. `load()` already computed the mount table nearby.
- **Remediation:** hoist the mount table once at load and pass it into the check.

#### P3 — alias conflict switch contradicts its comment; strong alias at `count == 2` silently retargeted (`src/typos.rs:164-176`)

- **Root cause:** the comment says "only switch after the new id wins once more often — replace if counts were low, else keep", but the code switches when `e.count <= 2`, and `STRONG_COUNT == 2` — so a twice-confirmed strong alias is reset to `count = 1` and retargeted by a single conflicting launch. There is no win tally at all.
- **Remediation:** require the new id to be observed at least as often as the current count before switching, or switch only when `e.count < STRONG_COUNT`.

#### P3 — clock rollback freezes decay/recency at maximum (`src/typos.rs:468-472`, `src/usage.rs:224-228`)

- **Root cause:** `now_secs()` returns 0 on pre-epoch clock rollback (no panic); `age = now.saturating_sub(last)` = 0, so every entry scores full recency (`500`/`5000`) with `decay = 1.0` — rankings become count-only and pruning/top-N churn arbitrarily until the clock recovers. Entries recorded during rollback get `last = 0`.
- **Remediation:** clamp `last` monotonic per store, or treat `now < last` as "use previous now".

#### P3 — `top()` / `top_path_ids()` tie-break is HashMap-iteration-random (`src/usage.rs:118`, `148`)

- **Root cause:** `items.sort_by_key(|b| Reverse(b.1))` is stable, but the input order is `HashMap` iteration (randomized per process), so equal-frecency entries (e.g. hundreds of zero-use ids) yield different top-N sets across restarts; the file-search hot set churns between runs.
- **Remediation:** tie-break on `(score, count, id)`.

#### P3 — first-use entry can be evicted by its own `record` call (`src/usage.rs:87-94`)

- **Root cause:** a brand-new id scores `1*1000 + 5000 = 6000`; a day-old entry with `count ≥ 2` scores ≥ 7000. With the map at `MAX_ENTRIES`, the just-launched id is the coldest and is pruned inside the same `record` that inserted it — the write is a no-op but still marks dirty and triggers a disk rewrite. Prune math itself is correct.
- **Remediation:** pin the recorded id during prune.

#### P3 — duration scale accepts negative multipliers (`src/providers/calc/duration.rs:53-59`, `71`)

- **Root cause:** `RE_SCALE` captures `[+-]?\d+(?:\.\d+)?`; the guard only rejects `s == 0.0`. `1h 30min * -2` yields a `-3h` title via the explicit negative-formatting branch.
- **Remediation:** reject `s < 0.0` unless negative durations are a documented feature.

#### P3 — unbounded duration digits overflow to `inf`, saturating cast prints absurd values (`src/providers/calc/duration.rs:216-220`)

- **Root cause:** `parse_duration_tokens` caps neither token count nor digit length; a ≥309-digit count parses to `f64::INFINITY`, `inf < 0.001` is false, and `inf.round() as i64` saturates to `i64::MAX`, producing `106751991167300d 15h 30min 8s`-class titles. No crash (saturating cast), purely wrong output (CWE-190).
- **Remediation:** early-return in `format_duration` for non-finite input and cap token digit length in the parser.

#### P3 — cooking ingredient match is substring-based, assigning wrong densities (`src/providers/calc/cooking.rs:74-79`)

- **Root cause:** `find_ingredient` matches the first table alias **contained** in the tail string. `1 cup milk chocolate` matches `"milk"` (244 g/cup) instead of a chocolate density (~40% error); `2 cups sugar free pudding mix` matches `"sugar"`; `100g flour tortilla in cups` matches `"flour"`.
- **Remediation:** require whole-word (whitespace-boundary) alias matching.

#### P3 — oven conversion accepts physically impossible temperatures (`src/providers/calc/cooking.rs:378-391`)

- **Root cause:** the fan/conventional regex accepts `[+-]?\d+` with no range check; `fan -260 c to conventional` renders `-280 c`, below absolute zero.
- **Remediation:** reject `v_c < -273.15` (or clamp to a sane cooking range) after unit conversion.

#### P3 — battery capacity `u8` accepts firmware values above 100 (`src/providers/calc/battery.rs:173-174`)

- **Root cause:** `capacity` parses into `Option<u8>` (0–255); some firmware momentarily reports >100 while charging, rendering `battery 101%`. Cosmetic/environmental, no crash.
- **Remediation:** clamp to `c.min(100)` after parse.

#### P3 — store tmp files briefly world-readable before chmod (`src/config.rs:826-836`; same pattern in typos/usage)

- **Root cause:** `fs::write` creates `*.json.tmp` with default umask (typically 0644); the `translate` API key inside config is on disk group/world-readable for the window between write and `set_permissions(0o600)`. CWE-732.
- **Remediation:** create with `OpenOptions::new().mode(0o600)` before writing.

#### P3 — unknown config keys are silently dropped on next save (`src/config.rs:645-657`)

- **Root cause:** `HarkConfig` and nested structs have no `deny_unknown_fields` and no preserved-extra map; unknown keys parse as defaults and are permanently erased on the next `save()`. A typo'd key reads as default rather than erroring. Related: `backup_invalid_config` overwrites `config.json.invalid` on each successive failure, retaining only the latest corrupt version.
- **Remediation:** `deny_unknown_fields` (the backup path already exists) or a `#[serde(flatten)] extra: Map` preserved on rewrite.

#### P3 — decimal/hex-IP and DNS-resolvable SSRF literals pass the translate-endpoint blocklist (`src/config.rs:585-600`)

- **Root cause:** `parse_ipv4_literal` accepts only dotted-decimal, so `http://2130706173` (decimal 127.0.0.1) is treated as a hostname and passes; any DNS name resolving to link-local/metadata IPs also passes. The code comment claims SSRF protection ("block cloud-metadata / link-local targets"). Low severity: the endpoint is user-configured local settings, not remote input; the blocklist is defense-in-depth only.
- **Remediation:** resolve the host at request time and re-check the resolved IP, or document the blocklist as literal-only.

#### P3 — drag-thumbnail memo pins one `gdk::Texture` for process lifetime (`src/ui/dnd.rs:324-338`)

- **Root cause:** the thread-local single-entry memo is never cleared, so the last dragged image's texture (GPU memory) outlives window destruction. One ≤256 px texture — negligible.
- **Remediation:** clear the memo on window hide, or accept as-is.

#### P3 — thumbnail size-slot store/read mismatch (`src/ui/thumbnails.rs:19-24`, `118-121`)

- **Root cause:** `store_freedesktop_thumbnail` writes only `large`/`normal`, while the reader probes `large`, `normal`, `x-large`. Staleness is correctly rejected by mtime checks, so no stale entry is ever served — the cost is only needless regeneration/cosmetic sizing.
- **Remediation:** none required; optionally skip the `x-large` probe or write `x-large` for large sources.

#### P3 (theoretical) — `Instant::now() - SAVE_DEBOUNCE` startup subtraction panics if monotonic clock < 2 s (`src/typos.rs:70,88`, `src/usage.rs:67,76`)

- **Root cause:** `Instant - Duration` panics on overflow; on Linux `CLOCK_MONOTONIC` starts at boot, so this requires the daemon to start within 2 s of boot — effectively unreachable, but it is the only arithmetic panic in these files.
- **Remediation:** `Instant::now().checked_sub(SAVE_DEBOUNCE).unwrap_or_else(Instant::now)`.

### Clean areas verified in this pass

- Pruning math in both stores (`typos.rs:440-451`, `usage.rs:190-203`) is off-by-one-free; caps are enforced on load and every insert.
- Levenshtein/Wagner–Fischer in `typos.rs:415-438` is correct and range-safe; `RwLock`/`Mutex` poisoning is handled via `into_inner()`; float→int casts in frecency saturate (no UB); rename-based saves are same-directory atomic; a racing `ConfigStore::update` cannot write a stale snapshot because `save()` re-snapshots.
- No repeating glib event sources in the audited UI files: the only timers are the two one-shot sites noted above; `mod.rs` debounce timers are removed on re-arm (`ui/mod.rs:392-395`).
- `scroll_anim.rs` / `size_anim.rs` tweeners use `Rc::downgrade` tick closures, cancel the previous ticker on every start, and `ControlFlow::Break` on completion — no post-completion ticking, no strong-ref cycles, no concurrent tickers.
- `highlight_markup` slices only at `char_indices` boundaries and escapes every segment (including the trailing run) via `glib::markup_escape_text`; footer/action_panel use `set_text` exclusively — no markup injection surface.
- `thumbnails.rs` hand-rolled MD5 matches the FreeDesktop spec and is used only as a cache filename; PNG chunk parsing uses checked ranges with `?`; rows.rs recycling diffing/clearing prevents stale badge/drag state across rebinds; the icon cache is FIFO-capped at 512.
- `fueleco.rs` conversion constants verified correct (2.35215 L·gal/km·mi round-trip); `battery.rs` µWh/µW energy math is unit-correct with saturating multiplies and full non-finite/zero/>48 h rejection (`battery.rs:381-399`); `unitmath.rs` parser rejects unicode digits, `"nan"`/`"inf"` spellings, and guards division-by-zero at both sites; `duration.rs` clock parsing bounds hours/minutes/seconds (leap-second `:60` falls through to no-result rather than a wrong answer); `datetime.rs` ±100-year clamp and Option-chained chrono construction exclude overflow/infinite-loop paths.

### Pass 13 verification log

- Three parallel subagent sweeps: (a) `typos.rs`/`usage.rs`/`config.rs` stores + config; (b) `ui/` helper widgets, animations, drag layer, thumbnails, rows, footer, action panel; (c) fuzz-style calc-provider sweep of `cooking.rs`, `duration.rs`, `unitmath.rs`, `fueleco.rs`, `datetime.rs`, `battery.rs` at increased depth over the Pass 7 numeric audit.
- Every finding's cited `file:line` was re-read and confirmed against current source by the main auditor before recording (spot-checks of all 24 findings).
- `cargo clippy --all-targets` — clean (0 warnings); output captured to scratch (`clippy.log`).
- `cargo test` — 253 passed / 0 failed / 2 ignored; output captured to scratch (`test.log`).
- `cargo deny check` — **not run**: `cargo-deny` is not installed in this environment (recorded honestly per plan criterion 4; Pass 3F's deny results remain the last available).
- MSRV 1.70 rebuild — still not verifiable here (no 1.70 toolchain installed; unchanged from Pass 3F).
- No source changes were made in this pass.
- This pass produced **24 new findings** (4 P2, 20 P3); it is **not** a zero-finding clean pass.

### Termination condition status

The audit termination condition (all 5 phases done at least once **plus** two consecutive zero-finding passes) is **not met**: Pass 13 produced 24 new verified findings. Phases remain nominally covered since Pass 3, but areas surfaced for a Pass 14 include: remediation-side re-verification of the Pass 13 wipe/durability interaction, `engine.rs` scoring/merge paths at increased depth, `ipc.rs` flood/interrupt behavior, and a focused sweep of `ui/settings.rs` + `ui/open_with.rs` (last covered in Pass 11).

---

## 🔁 Pass 14 engine scoring, IPC flood & settings/open_with depth audit (2026-08-26)

Areas targeted per the Pass 13 close-out: `engine.rs` merge/scoring at depth, `ipc.rs` flood/interrupt behavior (plus `main.rs` dispatch), and an increased-depth re-sweep of `ui/settings.rs` + `ui/open_with.rs` (last covered Pass 11). All line numbers verified against current source.

### New verified findings

#### P2 — serialized IPC accept loop: slowloris clients wedge all hotkey presses (`src/ipc.rs:96-113`)

- **Root cause:** connections are handled inline on one thread; each can cost up to 2 s of blocking read (connect, send nothing) plus 1 s of blocking `ok\n` write (send `toggle`, never read). No per-connection dispatch. 200 silent clients ≈ 6.7 min during which every real `request_toggle` burns its full 5×(20 ms+100 ms) retry budget and returns `false`, so `main.rs:48-50` falls through and spawns a full GTK Application instead of toggling. CWE-400.
- **Remediation:** spawn a bounded detached thread per connection, or non-blocking accept loop; at minimum skip the ack write (client already treats no-reply as success, comment at `ipc.rs:41-47`).

#### P2 — `accept` error tight-loops at 100% CPU under fd exhaustion (`src/ipc.rs:99-100`)

- **Root cause:** `let Ok(mut stream) = conn else { continue };` retries instantly on any accept error; under `EMFILE`/`ENFILE` accept fails instantly and forever → busy loop burning a core with no recovery and no log. CWE-772.
- **Remediation:** match on the accept error; sleep 10–50 ms on `EMFILE`/`ENFILE`/`ENOMEM`, log others, break on `EINVAL`.

#### P2 — bind→chmod race leaves the socket briefly world-connectable (`src/ipc.rs:121`, `:90-94`)

- **Root cause:** `UnixListener::bind` creates the node with mode `0777 & !umask` (typically 0755); between bind and the later `set_permissions(0o600)` any local user may `connect()` and send `toggle` — Unix sockets don't re-check mode after the connection is queued. A foreign toggle can open/close the victim's overlay; the chmod result is also discarded (`let _ =`). CWE-367.
- **Remediation:** set `umask(0o077)` around bind and restore, or bind to a temp name, chmod 0600, then `rename()` into place atomically; check the chmod result.

#### P2 — unbounded toggle channel: flood clients grow the queue and churn the overlay (`src/main.rs:92-103`)

- **Root cause:** every accepted `toggle` enqueues one unit into an **unbounded** `async_channel`, and each drained unit runs a full `launcher.toggle()` on the main loop — no coalescing, bound, or rate limit. A local flood client (7-byte write per reconnect, >50k/s) grows memory unboundedly and queues thousands of show/hide cycles replaying long after the flood stops. CWE-400.
- **Remediation:** coalesce (`if tx.is_empty() { tx.send_blocking(()) }`), or bounded channel with `try_send`, or collapse even-count pending toggles before invoking the UI.

#### P2 — source/exclusion toggles never trigger reindex; index silently stale (`src/ui/settings.rs:543-547`, `563-568`, `846-857`, `947-955`)

- **Root cause:** these handlers only call `engine.config().update(...)`; contrast the depth steppers (`settings.rs:509`, `529`) and `promote_deep_root` (`engine.rs:630`) which call `engine.force_reindex()`. After unchecking a mount or adding an exclusion, the running index still contains those files until the 30-minute TTL rebuild (`settings.rs:481`) or a manual Rebuild — no UI hint. The index fingerprint does change (`index.rs:210-224` hashes these fields), so staleness persists exactly until the next rebuild cycle. CWE-1251.
- **Remediation:** call `engine.force_reindex()` after these mutations, or at minimum show an "index stale — rebuild required" badge.

#### P2 — "Reset appearance" desynchronizes the symbolic-icons checkbox (`src/ui/settings.rs:2057-2067`)

- **Root cause:** the reset handler sets `c.ui = UiThemeConfig::default()` (which resets `symbolic_icons` to `false`, `config.rs:392`) and refreshes every other control — accent, opacity, radius, font, icon size, layout — but never `sym_cb.set_active(false)`. GTK only emits `toggled` on actual change, so the UI shows the option on while config says off. The four subtitle hint labels also keep stale text. CWE-1251.
- **Remediation:** add `sym_cb.set_active(false);` and refresh the hint labels in the reset handler.

#### P3 — non-saturating usage-boost add overflows on corrupt `usage.json` (`src/engine.rs:283`, `:333`)

- **Root cause:** `r.score += self.usage.boost(&r.id)` — `boost` derives from a `u64 count` deserialized with no clamp (`usage.rs:47`); a hand-edited `count: 99999999999999999999` saturates `frecency`'s `as i64` cast to `i64::MAX`, and `50_000 + i64::MAX` panics in debug builds / wraps negative in release (result vanishes). The typo path already uses `saturating_add` (`engine.rs:559-562`). CWE-190.
- **Remediation:** `r.score = r.score.saturating_add(self.usage.boost(&r.id))` at both sites, or clamp `count` on deserialization.

#### P3 — hot-set short-circuit drops strictly better index results (`src/providers/files/search/rank.rs:110-125`)

- **Root cause:** when any hot path (≤64 entries) reaches the ≥30,000 prefix band, the full index scan is skipped entirely (documented Batch-B tradeoff). If a new file exactly matching the query name (50,000 band) is created after a frequently-opened prefix-matching file became hot, the exact match is never scanned for queries ≥4 chars.
- **Remediation:** short-circuit only at the ≥50,000 exact band, or always run a cheap exact-name `HashMap` lookup before skipping.

#### P3 — spinner counter can drift upward on text-changed-without-refresh paths (`src/ui/mod.rs:2374-2381`, `2555-2558`, `2611-2620`)

- **Root cause:** `async_pending.set(0)` resets only inside `refresh_results`; if entry text changes without a refresh (programmatic `SetQuery` paths), the pending future passes the gen check but fails the text-equality bail at `2620` and returns without its decrement — the counter was never reset, so it drifts up and the spinner stays lit.
- **Remediation:** decrement before the text-equality bail-out, or recompute pending from live job tokens.

#### P3 — 5-minute live-cache TTL suppresses discovery of newly created files (`src/providers/files/live_cache.rs:17-18`, `files/mod.rs:246-259`)

- **Root cause:** after a deep walk caches results (negative-cached 90 s when empty), external file creation is invisible until TTL expiry; `clear_live_cache()` is only invoked on trash (`engine.rs:417`). A design tradeoff, recorded as a finding because the negative-TTL window silently returns no results for a file that now exists.
- **Remediation:** shorten `NEGATIVE_TTL`, or watch deep roots for invalidation.

#### P3 — per-result usage lock acquisition per keystroke on the main loop (`src/engine.rs:281-284`)

- **Root cause:** `usage.boost(&r.id)` takes a fresh `RwLock` read guard once per result (up to ~45 × per keystroke on the GTK main loop); `top(20)` on every empty-state render also clones 20 `String`s. Uncontended cost is small but repeated hot-path work.
- **Remediation:** batch API (`boost_snapshot`) or hold one read guard across the boost loop.

#### P3 — IPC fallback socket path: silent mkdir/chmod failures and symlink-able `/tmp/hark` (`src/ipc.rs:14-19`, `68-77`)

- **Root cause:** in the rare no-XDG/no-cache/no-home environment, the socket lands under `/tmp/hark`; all errors are ignored (`let _ = create_dir_all` succeeds through an attacker symlinked dir, `let _ = set_permissions`), so an attacker-controlled parent can host a supplanted socket the stale-reclaim path will `remove_file`/rebind. CWE-59.
- **Remediation:** `create_dir` with no-follow semantics, verify parent `uid == geteuid()` and mode 0700, abort IPC on failure.

#### P3 — single `read()` silently drops split-stream messages (`src/ipc.rs:103-105`)

- **Root cause:** exactly one `read` syscall per connection; a client sending `tog` + `gle\n` in separate writes reads only `tog`, the `== "toggle"` compare fails, and the toggle is silently lost. Standard clients' 7-byte writes are atomic on unix sockets, so limited to nonstandard clients.
- **Remediation:** loop on `read` until `\n` or buffer full within the timeout window.

#### P3 — `args.retain` removes every arg equal to the search query (`src/main.rs:26-28`)

- **Root cause:** `hark --search gimp gimp` strips both `gimp` tokens (including a program-path collision) instead of only the flag+operand pair.
- **Remediation:** remove by index (`args.drain(i..=i+1)`) before any retain pass.

#### P3 — `request_toggle` reports success without confirmed processing (`src/ipc.rs:44-49`)

- **Root cause:** the ack read result is ignored; `true` is returned after write success even if the listener died between write and dispatch, so `main.rs:48-50` skips spawning an instance and the keypress silently does nothing.
- **Remediation:** require `ok\n` within the timeout for the `true` return, or log acked vs unacked.

#### P3 — settings row-removal closures form strong reference cycles leaking row subtrees (`src/ui/settings.rs:788-818`, `1577-1627`)

- **Root cause:** the remove button's `connect_clicked` closure strongly captures `row`, and `row.append(&rm)` puts the button inside the row — a cycle that keeps the subtree (labels, handlers, `Arc<Engine>` clones) alive for the process lifetime after detach/refill. Every alias/extra-folder/exclusion/deep-root removal and every list refill leaks one subtree in the long-lived daemon. CWE-401. The weak-capture pattern already exists in the codebase (`open_with.rs:88-91`).
- **Remediation:** capture `row.downgrade()` and `upgrade()` inside the handler.

#### P3 — text entries persist config to disk on every keystroke (`src/ui/settings.rs:1836-1845`, `2149-2155`, `2173-2182`, `2201-2210`)

- **Root cause:** all use `connect_changed`; `ConfigStore::update` ends with `save()` — a full JSON serialize + tmp write + rename per character. A 30-char endpoint = 30 disk writes, and each intermediate value is momentarily the live config consumed by the running translation path (sanitization at `config.rs:467-487` prevents bad values, but partial URLs go live).
- **Remediation:** debounce ~500 ms or persist on focus-loss/`activate`.

#### P3 — settings j/k/arrow navigation dead-keys on filtered rows (`src/ui/settings.rs:313-328`)

- **Root cause:** `row_at_index` counts hidden rows; when the target row is filtered out, the handler neither scans for the next visible row nor propagates the key — it still returns `Propagation::Stop`. With a filter leaving rows 2 and 5 visible, ↓ from row 2 silently does nothing.
- **Remediation:** scan forward/backward to the next visible row, or don't `Stop` when no move happened.

#### P3 — `xdg-open` spawn failure ignored while the launcher still closes (`src/ui/open_with.rs:108`)

- **Root cause:** `let _ = Command::new("xdg-open").spawn();` is followed unconditionally by popdown/hide — on minimal-WM setups without `xdg-open`, nothing opens and there is no feedback (contrast the AppInfo branch at `119-131` which logs and keeps the popover open). CWE-754.
- **Remediation:** match on the spawn result; on `Err`, log, reset `firing`, keep the popover open.

#### P3 — control characters in filenames vertically expand the Open With popover (`src/ui/open_with.rs:60-67`)

- **Root cause:** the filename is embedded verbatim in the header label; `set_ellipsize(Middle)` bounds width but embedded `\n` (legal in Linux filenames) forces multi-line height expansion.
- **Remediation:** strip control chars (`chars().filter(|c| !c.is_control())`) before display.

#### P3 — Open With app list ordering is desktop-scan order, unsorted (`src/ui/open_with.rs:342-352`)

- **Root cause:** `recommended_for_type` then `all_for_type` appended in GIO iteration order with only dedup + truncate; the 40-app cap can select a machine-dependent subset. Recommended-first ordering (the main intent) is preserved.
- **Remediation:** sort the post-recommended remainder by `app.name()` before truncation.

### Clean areas verified in this pass

- Engine merge determinism: `engine.rs:308-313` sorts `score desc → kind_rank → title` (total order); files/apps heap tie-breaks are total orders; per-provider pre-merge truncation cannot lose cross-provider results (files keep their own best 25, apps 20); `merge_cached`'s different tie-break is inert because `Engine::search` re-sorts.
- Scores are `i64` end-to-end — no float ordering or NaN pitfalls in the search path; negative scores cannot leak into heaps (`rank.rs:279-281` filters `score <= 0`); all narrowing casts verified lossless.
- Hot-set/index generation safety: hot indices are snapshot and consumed under the same `index.read()` guard, so index swaps cannot desynchronize hot pointers; `seen` HashSet prevents hot/index double-counting; lock ordering is strictly `index → hot/usage` — no inversion.
- Search cancellation: gen tokens bump per refresh and are checked before applying async deep/translate hits with a text-equality double-check, synchronous within one main-loop continuation — late results cannot override newer ones (modulo the spinner-accounting finding above). The `schedule_deep_job` double-worker window (re-check + re-CAS after `BUSY.store(false)`) was traced through every interleaving: no duplicate processing, no lost wakeup, no BUSY leak.
- Dedup and truncation limits agree end-to-end (engine 25, files 25, merge_cached 25, UI 25); `empty_results`'s path-resolve cap and fill-to-15 arithmetic verified consistent; `live_cache.rs` LRU internals verified including reinsert recency (test-covered).
- IPC: no per-connection thread spawn; fixed 64-byte stack buffer bounds per-connection memory; zero-length reads, invalid UTF-8, EOF-vs-error, and partial-write-then-disconnect all handled without panic; no length-prefix framing exists so length-bounds attacks are structurally impossible; no locks held across I/O; main-loop state is `Rc`/`RefCell`-confined to the main context; exit codes consistent (2 for usage errors).
- Settings numeric inputs are all clamped steppers re-clamped by `sanitize` — no numeric text-entry parse panics; stale label lookups are Option-guarded; the settings panel is created once in a `Stack` (no per-switch source churn); no byte-index slicing on user text in either file; no `unwrap`/`expect` on fallible values reachable from callbacks; Open With argv passes paths as single args (no shell interpolation at this layer); MIME fallback degrades directory → query_info → content_type_guess without panicking; the Open With popover uses weak captures (no cycle).

### Pass 14 verification log

- Three parallel subagent sweeps: (a) `engine.rs` + `providers/mod.rs` + `files/hot.rs`/`live_cache.rs` scoring/merge depth; (b) `ipc.rs` + `main.rs` flood/interrupt/auth; (c) increased-depth re-sweep of `ui/settings.rs` + `ui/open_with.rs`. The IPC agent's initial run returned only a summary; it was resumed to emit the full per-finding report before recording.
- Every finding's cited `file:line` was re-read and confirmed against current source by the main auditor before recording (all 21 findings spot-checked).
- `cargo clippy --all-targets` — clean; `cargo test` — 253 passed / 0 failed / 2 ignored; captured to scratch (`clippy_pass14.log`, `test_pass14.log`). `cargo deny check` still unavailable in this environment (unchanged from Pass 13); MSRV 1.70 rebuild still not verifiable (no 1.70 toolchain).
- One infrastructure interruption lost the first three subagent runs; all three were relaunched and completed.
- No source changes were made in this pass.
- This pass produced **21 new findings** (6 P2, 15 P3); it is **not** a zero-finding clean pass.

### Termination condition status (Pass 14)

Termination condition still **not met** (no consecutive clean passes; Pass 14 found 21 new issues). Named areas for Pass 15: `ui/mod.rs` main window/keybind/tab-completion at depth (largest file, only partially covered via other passes' citations), `providers/apps.rs` + `providers/files/index.rs` re-sweep at depth, and `theme/` + `bench.rs`/`lib.rs` never swept directly.

---

## 🧭 Pass 15 overlay window, app/index provider & theme/packaging first sweep (2026-08-26)

Areas targeted per the Pass 14 close-out: `ui/mod.rs` at depth (keybinds, tab completion, selection, window lifecycle, glib sources), increased-depth re-sweep of `providers/apps.rs` + `providers/files/index.rs`, and the first direct sweep of `theme/mod.rs` + `theme/css.rs` + `bench.rs` + `lib.rs` + Cargo/packaging/docs assets. All line numbers verified against current source.

### New verified findings

#### P2 — right-click on a conversion set opens the action panel for a different item (`src/ui/mod.rs:688-699`)

- **Root cause:** the right-click handler computes `idx = row.index()`, calls `list_rc.select_row(Some(&row))` — which synchronously fires the row-selected handler; for a conversion set that handler calls `conv_swap_to_front(&mut results, idx)` (`mod.rs:2262-2266`, rotating the vec) — and only then invokes `open_action_panel()`, which re-reads `results.borrow().get(selected)` against the now-rotated vec. The panel opens for whatever landed at the original index after rotation, not the right-clicked item (e.g. right-click "stone" on `100 kg in lb` → panel shows the row rotated into slot 2).
- **Remediation:** snapshot `results.borrow().get(idx).cloned()` before `select_row`, or resolve the panel target by row identity rather than re-reading `selected` after the rotation.

#### P2 — Tab on a conversion set replaces the query with the answer title (`src/ui/mod.rs:1437-1476`, `1494-1502`)

- **Root cause:** every conversion row's action is `Action::Copy`, and `completion_text_for` returns `Some(item.title.clone())` for it; Tab therefore replaces `100 kg in lb` with e.g. `220.462 lb` — which itself parses as a new conversion query, silently destroying the user's input and churning the result set.
- **Remediation:** skip Tab when `is_conv_set(&results)`, or return `None` from `completion_text_for` for `ResultKind::Conversion`.

#### P2 — untracked `preview.clear()` timer fires after re-show blanks a freshly populated preview (`src/ui/mod.rs:1388-1391`)

- **Root cause:** `hide()` arms a one-shot `HIDE_FADE_MS` timer to clear the preview; unlike `hide_delay` (cancelled by `show()` at `1300-1302`), this source is not tracked anywhere. Escape followed by a summon within the fade window: `show()` cancels the fade, the user types, a preview populates — then the orphaned timer fires `preview.clear()` and blanks it until the next selection change. CWE-362 / CWE-404.
- **Remediation:** track this source in a slot cancelled by `show()`, or guard the callback with `if !window.is_visible()`.

#### P2 — PKGBUILD hard-links `gtk4-layer-shell` when built with the feature but lists it only as `optdepends` (`packaging/aur/PKGBUILD:12-14, 29-33`)

- **Root cause:** `build()` enables `--features layer-shell` when `pkg-config gtk4-layer-shell-0` exists on the *builder's* machine; the resulting binary then requires `libgtk4-layer-shell.so.0`, but an `optdepends` entry creates no dependency edge. Users who install the package without the optdep get "error while loading shared libraries" at startup. CWE-1104-class packaging defect.
- **Remediation:** enable the feature unconditionally and move `gtk4-layer-shell` to `depends`, or ship a `hark-layer-shell` split package.

#### P3 — closing settings resets selection to row 0 unconditionally (`src/ui/mod.rs:540-555`)

- **Root cause:** `close_settings` always runs a full `refresh_results` (which resets `selected` to 0), even when the query is unchanged and the user had a later row selected before opening settings; the selection-preserving `rebind_results_from_cache` path exists but is not used here.
- **Remediation:** snapshot/restore `selected` around the refresh, or rebind from cache when the query is unchanged.

#### P3 — `truncate(25)` in async merges can evict the selected row, resetting to the hero card (`src/ui/mod.rs:2844-2876`)

- **Root cause:** `apply_deep_hits`/`apply_translate_hits` preserve selection by id but fall back to `unwrap_or(0)` when deep hits push the selected row past the 25-item cap — landing on the conversion hero card and silently changing the displayed value without user action.
- **Remediation:** clamp to `merged.len().saturating_sub(1)` and prefer re-asserting the same conversion id at its new position, or skip a merge that would evict the selected row.

#### P3 — trash flow leaves `selected` pointing at the shifted-in neighbor (`src/ui/mod.rs:1907-1929`)

- **Root cause:** rows are removed from `results` by id but `selected` keeps the pre-removal index; when the trashed row was at or before the selection, the index now names a different item. Clamped (`min(len-1)`), so no panic — unconsidered selection semantics only.
- **Remediation:** decrement `selected` when the removed position was `< selected`, or resolve the post-trash selection by neighbor id.

#### P3 — `%%` field code never decoded to literal `%` (`src/providers/apps.rs:489-493`, `425-427`)

- **Root cause:** the spec defines `%%` as an escaped literal percent; `is_field_code` omits it and no post-pass collapses it, so `Exec=foo --msg=100%%` launches with a literal `--msg=100%%` argument. CWE-1284.
- **Remediation:** after stripping field codes, `.map(|p| p.replace("%%", "%"))` in both `parse_desktop_file` and `resolve_exec_binary`.

#### P3 — `OnlyShowIn`/`NotShowIn`/`TryExec` keys have no parsing arm at all (`src/providers/apps.rs:383-399`)

- **Root cause:** the key match covers only Name/GenericName/Keywords/Comment/Exec/Icon/Terminal/NoDisplay/Hidden/Type; there is no `XDG_CURRENT_DESKTOP` read in the file. A `.desktop` with `OnlyShowIn=KDE;` is listed and launchable under GNOME/Hyprland, where it misbehaves or fails.
- **Remediation:** parse both keys, split on `;`, compare against `XDG_CURRENT_DESKTOP`, and filter in `reload()` alongside the `no_display` filter; implement `TryExec` (binary missing ⇒ skip) at the same site.

#### P3 — duplicate-key precedence inconsistent: strings first-wins, booleans/Type last-wins (`src/providers/apps.rs:384-398`)

- **Root cause:** string keys latch on first occurrence (`if field.is_empty()` guards); `Terminal`/`NoDisplay`/`Hidden` unconditionally overwrite — a trailing `NoDisplay=false` un-hides an entry the vendor hid, and a trailing `Terminal=true` force-wraps a GUI app.
- **Remediation:** give every key a first-wins latch, consistent with the spec's "later duplicate keys are invalid".

#### P3 — NoDisplay apps unfindable even by exact id (`src/providers/apps.rs:138`)

- **Root cause:** `if app.no_display || … { continue; }` drops the entry entirely, so `resolve_id` and `display_name_for_desktop_id` cannot resolve stored `app:<id>` actions or default-app references to NoDisplay helpers (spec: hidden from menus but launchable/associatable).
- **Remediation:** retain NoDisplay entries in a side map consulted by `resolve_id`/`display_name_for_desktop_id`, filtering them only out of `search`.

#### P3 — panic during index build leaves `indexing=true` permanently (`src/providers/files/index.rs:168-181`)

- **Root cause:** after `indexing.store(true)` there is no `catch_unwind` and no Drop guard resetting the flag; any panic inside `build_index` leaves the UI spinning "indexing…" until a later build happens to complete. CWE-754.
- **Remediation:** guard struct whose `Drop` stores `false`, or `catch_unwind` around the build body.

#### P3 — `MAX_INDEX` eviction is silent walk-order truncation, home-biased (`src/providers/files/index.rs:237-258`, `307-311`)

- **Root cause:** roots are pushed home-first; hitting the 100,000 cap just stops the walk mid-root, so a large home dir starves mounts and `extra_roots` entirely — files on mounted drives become unsearchable, with only the `capped` flag as warning.
- **Remediation:** interleave roots round-robin, or walk everything and rank/truncate afterward (the `low_value`/`high_value` fields already support ranking).

#### P3 — rename-failed index tmp file never cleaned (`src/providers/files/index.rs:810-819`)

- **Root cause:** if the tmp write succeeds but the rename fails, the tmp file (potentially tens of MB) is never deleted; `clear_cache` removes it only on explicit force-rebuild. Accumulates across failures.
- **Remediation:** `let _ = fs::remove_file(&tmp);` on rename failure.

#### P3 — unreadable meta file disables the RAM fast path forever (`src/providers/files/index.rs:757-777`)

- **Root cause:** `cache_ttl_stale` returns true on any meta read failure; if the cache file landed but the meta write did not (read-only/full cache dir), every 45-minute `ensure_fresh` performs a full rebuild walk instead of the no-I/O fast path.
- **Remediation:** fall back to the cache file's own mtime for the TTL decision when the meta is unreadable.

#### P3 — depth fallback fabricates absolute-path depth (`src/providers/files/index.rs:354-357`)

- **Root cause:** on `strip_prefix` failure (path with `..` components), depth becomes the absolute-path component count, feeding `is_high_value_path`'s `depth <= 2` check — such entries lose mount-top ranking. The `as u16` cast itself is safe (walk depth clamped ≤6).
- **Remediation:** return `None` (drop the entry) on `strip_prefix` failure instead of fabricating a depth.

#### P3 — theme watch "debounce" schedules one timer per monitor event (`src/theme/mod.rs:197-203`)

- **Root cause:** unlike `reload()` (`136-147`), which removes a pending source id before re-arming, the FileMonitor callback does not track prior timers — N events in a burst produce N full `apply()` cycles (disk read + JSON parse + CSS re-inject). Idempotent but wasted work; the "Debounce" comment is not implemented.
- **Remediation:** store the pending `SourceId` like `reload_debounce`, remove-and-re-arm each event.

#### P3 — `scheme_path()` accepts a relative `XDG_STATE_HOME` (`src/theme/mod.rs:210-216`)

- **Root cause:** the XDG spec requires the variable to be absolute; a relative value here produces a CWD-dependent scheme path that silently falls back to the built-in palette when the daemon's CWD differs.
- **Remediation:** only honor `state.starts_with('/')`.

#### P3 — `install-user.sh` sed replacement injects `$BIN_DIR` unescaped (`packaging/install-user.sh:38-44`)

- **Root cause:** `&`/`|`/`\` in an `XDG_BIN_HOME` path expand inside the sed replacement text; a path with spaces also yields an unquoted `Exec=` that breaks desktop-entry parsing. CWE-78-adjacent, user-self-inflicted.
- **Remediation:** escape specials before substitution and quote the exec value.

#### P3 — `install-user.sh` has no ETXTBSY handling; upgrading over a running daemon aborts the install mid-script (`packaging/install-user.sh:32`)

- **Root cause:** `install -Dm755` hits `open(O_TRUNC)` on the running executable → "Text file busy" → `set -euo pipefail` aborts, skipping the desktop entry/icon/autostart steps. `scripts/install.sh:91` documents this exact hazard but the logic wasn't carried over.
- **Remediation:** write to a temp name + `mv` (rename over a busy binary succeeds), or kill the daemon first.

#### P3 — `bench.rs` `daemon_stats()` returns `None` on any unparseable pid line (`src/bench.rs:400-406`)

- **Root cause:** `cols[0].parse().ok()?` uses the function-level `Option`, so one non-pid line aborts the whole scan — "(no daemon process found)" while a daemon is running.
- **Remediation:** `let Ok(pid) = cols[0].parse() else { continue; };`

#### P3 — bench timing confounded by concurrent index warm-up threads (`src/bench.rs:100-134`)

- **Root cause:** `spawn_warm()` starts the file-index walker concurrently with the timed `bench_query` loop; CPU contention from the walker inflates `median_us`/`p95_us` non-deterministically on low-core machines.
- **Remediation:** wait until `!index_progress().running` before the timed section, or note the confounder in the output header.

#### P3 (docs) — FEATURES.md: "2s poll fallback" for theme reload does not exist (`FEATURES.md:205` vs `src/theme/mod.rs:174-179`)

- **Root cause:** the code comment explicitly says "Do **not** poll every few seconds"; the fallback is a single apply-once. Users on NFS/sandboxes wait for a documented pick-up that never comes.
- **Remediation:** update FEATURES.md to "single apply-once fallback if monitoring is unavailable".

#### P3 (docs) — FEATURES.md lists one-shot "Open with…" both as not shipped (line 285) and shipped (line 292) (`FEATURES.md:285,292`)

- **Root cause:** `open_with.rs` exists and is wired to Ctrl+Shift+O (`ui/mod.rs:1185`) — the "known gaps" entry is stale.
- **Remediation:** delete the line-285 entry.

#### P3 (docs) — FEATURES.md claim "Tab completion" verified accurate; PageUp/PageDown unbound in the overlay noted as an enhancement gap (`src/ui/mod.rs:1229`)

- **Root cause:** the capture controller falls through to `Proceed` for PageUp/PageDown; focus sits on the Entry (where they do nothing), so the keys are dead in the overlay. Not a defect; recorded per the interface-completeness scope.
- **Remediation:** bind PageUp/PageDown to page the results list.

### Clean areas verified in this pass

- `ui/mod.rs` keybind guards are all distinct (Ctrl+K excludes Shift/Alt to avoid Ctrl+Shift+K); empty-list keys guarded by `len > 0`; wrap-around modulo with no underflow; `dismiss` re-entry guarded; `search_debounce`/`empty_delay` removed in `hide()`; all `Mutex::lock().unwrap()` sites sit on non-panicking critical sections (poisoning unreachable); slicing guarded (`rs[1..]` behind `first()` match); tab path completion prefix boundary prevents `/home/userx` matching `/home/user`; monitor-cache 5 s TTL self-heals after unplug.
- apps.rs: quoting/backslash per Desktop Entry Spec verified including nested quotes and test round-trip; first-wins desktop-id dedup across XDG dirs matches spec; `Hidden`/empty-Name semantics correct; no shell interpolation on launch (`Command::arg` only, injection test); non-UTF8 files skipped; locks poison-recovering.
- index.rs: corrupt/truncated cache degrades to clean rebuild; version mismatch rejected; single-assignment in-memory swap (readers see old-or-new); tmp+rename with meta stamped only after rename; `build_lock` + recheck prevents duplicate walks; fingerprint inputs sorted (no HashMap order leak); build/query exclusion consistent via shared `should_skip_entry`.
- theme/css.rs: every externally sourced CSS value passes `sanitize_hex`/clamps — no font/layout/icon name is ever interpolated from external input; `rgba` uses `u8::from_str_radix` (no overflow); exactly one CssProvider per ThemeManager with `load_from_string` replacing (no accumulation); `apply_gen` guard drops stale UI injects.
- bench.rs /proc parsing handles comm with spaces/parens, all parses `unwrap_or`, p95 index in-bounds; `lib.rs` has no dead code; Cargo.toml features match every `#[cfg]`; `default = []` matches PKGBUILD/docs; release profile self-consistent; `rust-toolchain.toml`/`deny.toml` consistent; PKGBUILD sha256 pinned; doc claims spot-checked accurate (top-25, 45-min reindex, 100k cap, socket/state/config/cache paths, 20%-from-top margin).
- Packaging build artifacts on disk are untracked local dirt (only PKGBUILD/.SRCINFO in git) — not a repo finding.

### Pass 15 verification log

- Three parallel subagent sweeps: (a) `ui/mod.rs` deep audit; (b) `apps.rs`/`index.rs` increased-depth re-sweep; (c) first sweep of `theme/`, `bench.rs`, `lib.rs`, Cargo/packaging/docs assets. The apps/index agent's first run returned only a summary; it was resumed to emit the full report before recording. An earlier attempt at all three sweeps failed with API 405 infrastructure errors and was relaunched.
- Every finding's cited `file:line` was re-read and confirmed against current source by the main auditor before recording (all 25 findings spot-checked).
- `cargo clippy --all-targets` — clean; `cargo test` — 253 passed / 0 failed / 2 ignored; captured to scratch (`clippy_pass15.log`, `test_pass15.log`). `cargo deny check` and MSRV 1.70 rebuild still unavailable in this environment (unchanged).
- No source changes were made in this pass.
- This pass produced **25 new findings** (4 P2, 21 P3); it is **not** a zero-finding clean pass.

### Termination condition status (Pass 15)

Termination condition still **not met** (no clean pass yet; Passes 13–15 yielded 24, 21, and 25 new findings respectively). Named areas for Pass 16: `providers/files/search/plan.rs` + `deep.rs` + `search/mod.rs` (planner/deep-walk logic never directly swept as a unit), `providers/fx.rs`/`http.rs`/`translate.rs` re-verification at increased depth, and `ui/preview.rs` re-sweep beyond Pass 12's audio/metadata findings.

---

## 🔬 Pass 16 search planner, HTTP providers & preview depth audit (2026-08-26)

Areas targeted per the Pass 15 close-out: `files/search/plan.rs`/`deep.rs`/`search/mod.rs` as a unit (first direct sweep), `http.rs`/`fx.rs`/`translate.rs` re-verification at increased depth, and `ui/preview.rs` beyond Pass 12's findings. All line numbers verified against current source.

### New verified findings

#### P1 — `to_lowercase()` byte-offset mismatch panics slicing the original query (`src/providers/files/search/plan.rs:101-114`, `257-286`)

- **Root cause:** both `parse_scoped_query` and `parse_scope_hint_query` locate the scope keyword (` in `, ` within `, … — pure ASCII) in a lowercased copy (`lower.find(kw)`) and then slice the **original** string with those byte offsets (`q[..kw_start]`, `q[kw_start + kw_len..]`). `str::to_lowercase()` is not length-preserving: `İ` (U+0130, 2 bytes) lowercases to 3 bytes; `ẞ` (3 bytes) to 2 bytes. Any length-changing character before the keyword shifts the offset so the slice lands mid-codepoint → panic on the GTK main thread; even when it lands on a boundary, the split is silently wrong (`İ in docs` yields scope `"ocs"`).
- **Failure pathway:** query `İ in 文档` → `parse_scope_hint_query` finds `" in "` at byte 3 of the lowercased string, but `文` starts at byte 6 of the original → `q[7..]` is not a char boundary → daemon crash. Note Pass 4's "scoped-query keyword slicing uses find-returned boundaries" clearance covered a different (since-rewritten) code path; this parser predates or escaped that review.
- **CWE-20** (panic = local DoS via a typed query).
- **Remediation:** find the keyword with an ASCII-case-insensitive match on the original string (`match_indices` + `eq_ignore_ascii_case` — exact, since keywords are ASCII), or map offsets through `char_indices`. Add regression tests with `İ`/`ẞ` before a scope keyword.

#### P2 — nonexistent absolute scope silently walks `/` for the full deep budget (`src/providers/files/search/deep.rs:285-290`)

- **Root cause:** for `abs_root` scopes, `abs.is_dir()` false → `abs.parent()`, whose parent of `/nonxistent` is `/` (of `~/Dcouments` is `$HOME`). The walk then burns `DEEP_VISIT_CAP_ASYNC` (40,000 entries) / 200 ms per keystroke across the root filesystem, yields zero hits, and never surfaces "directory doesn't exist". CWE-20/CWE-755.
- **Remediation:** walk up at most one level and abort if the resulting root is `/` for a non-`/` scope; or emit a "scope not found" result.

#### P2 — deep-scoped literal names match by substring (`src/providers/files/search/glob.rs:161-166`, consumed at `deep.rs:715-717`)

- **Root cause:** `name_matches_pat` for a non-glob pattern accepts `contains`; `main.rs under ~/dev` therefore returns `main.rs.bak`, `main.rs.orig`, `xmain.rsy` at the 32,000 "contains" band. Wrong results presented as scoped search.
- **Remediation:** for literal patterns in the deep/scoped path, require exact or stem-exact matching, or at minimum rank contains-band hits below `DEEP_SKIP_IF_INDEX_SCORE`.

#### P2 — translate disk cache is unauthenticated and can live in world-writable `/tmp` (`src/providers/translate.rs:928-933`, `948-960`)

- **Root cause:** `cache_get` deserializes a `CacheEntry` and serves `translated` verbatim without comparing the entry's `q`/`source`/`target` to the requested job; the cache key is a trivially computable FNV-64. The `cache_dir()` fallback chain can land in `/tmp/hark/translate` (world-writable, no O_NOFOLLOW, no ownership check — contrast the hardened `fx.rs:329-357`). A local attacker pre-creates a cache file whose key matches a text they expect the user to translate, and the launcher displays and copies the attacker's string; also wrong-text hash collisions. CWE-349/CWE-345.
- **Remediation:** verify `e.q`/`e.source`/`e.target` after deserialize; replicate the fx.rs 0700-ownership + O_NOFOLLOW guards; reject the `/tmp` fallback.

#### P2 — code preview of a ≤2 MiB single-line file freezes the main loop (`src/ui/preview.rs:775-779`)

- **Root cause:** the only gate is byte size (`MAX_CODE_BYTES = 2 MiB`); `set_text` + `set_language` (full-buffer re-highlight) + char-wrap of a ~2M-char minified line run on the GTK main thread — seconds to minutes of unresponsiveness, repeatable by keyboard navigation onto any minified asset. CWE-400.
- **Remediation:** cap line count (plain label past ~20k lines) and longest single line (skip highlight or truncate past ~5,000 chars), measured while streaming in the worker.

#### P3 — `index_is_strong` threshold sits below the deep "contains" band (`src/providers/files/search/mod.rs:64-65`, `deep.rs:136-140`, `813`)

- **Root cause:** `DEEP_SKIP_IF_INDEX_SCORE = 30_000` (documented "Exact/prefix band"), but a single contains-band live hit scores 32,000+boosts and cancels all remaining deep jobs — a `todo.md` query whose first job finds only `todo.md.bak`-style substring hits never walks the other roots.
- **Remediation:** raise the gate above the contains band (~34,000) or make it band-aware (exact ≥49,500 / prefix ≥39,500).

#### P3 — drive-prefix detection swallows words like `e:mail` into empty path completion (`src/providers/files/search/glob.rs:347-353`, `search/mod.rs:93-98`)

- **Root cause:** any `<alpha>:` token is routed to `path_completions`; with no matching mount the result is silently empty, and free-text search never runs.
- **Remediation:** require the drive prefix to be followed by `/`/`\`/end-of-string, or verify a mount exists before diverting.

#### P3 — deep-walk tie-breaks are arrival-order dependent (`src/providers/files/search/deep.rs:742-752`, `495-499`)

- **Root cause:** eviction `min_by_key` keeps the first equal-score entry; `sort_unstable_by` on `(score, title_lower)` leaves equal-score same-name different-directory hits in readdir order. Identical queries reorder across runs.
- **Remediation:** add path as the final tie-break in `merge_live` and the eviction comparison.

#### P3 — dead branch in `parse_glob_query` (`src/providers/files/search/plan.rs:615-622`)

- **Root cause:** the single-segment re-check of `contains('*')||contains('?')` duplicates the fully-handled earlier branch; the `Some` arm is unreachable. Behavior coincidentally correct; dead check invites mis-edits.
- **Remediation:** delete the dead arm.

#### P3 — ureq honors proxy env vars unscoped (`src/providers/http.rs:16-21`)

- **Root cause:** no `.proxy()` configuration; ureq 2.x default features read `HTTP_PROXY`/`HTTPS_PROXY`/`ALL_PROXY`. A hostile session env reroutes all provider traffic (including pasted translate text and the LibreTranslate api_key destination) through the attacker's proxy; TLS protects content but metadata/availability are exposed. CWE-441.
- **Remediation:** configure proxy explicitly from user settings only, or document/validate env proxy use.

#### P3 — MyMemory in-band error strings other than the two blocklisted render as translations and are cached (`src/providers/translate.rs:840-855`)

- **Root cause:** MyMemory returns HTTP 200 with status text inside `responseData.translatedText`; only two literal strings are rejected. The documented quota response (`MYMEMORY WARNING: YOU USED ALL AVAILABLE FREE TRANSLATIONS…`) becomes the displayed "translation", is copied by `Action::Copy`, and is disk-cached for 14 days. CWE-754.
- **Remediation:** parse `responseStatus` and reject non-200; blocklist `MYMEMORY WARNING`.

#### P3 — fx.rs accepts any `base`/`date` from network or disk (`src/providers/fx.rs:265-281`)

- **Root cause:** only rates are sanity-checked. A tampered cache or compromised response with `"base":"USD"` produces silently wrong conversions with a legitimate-looking badge; `date` is never format- or freshness-checked, so a 1999 table freshly cached displays as current. CWE-20.
- **Remediation:** require `base == "EUR"` (URL is EUR-based), validate `date` as ISO within N days of `fetched_at`, bound rates to a plausible range.

#### P3 — LibreTranslate api_key sent plaintext to user-allowed `http://` endpoints (`src/providers/translate.rs:718-728`, `config.rs:499-535`)

- **Root cause:** endpoint validation explicitly allows plain HTTP (loopback test at `config.rs:1430-1432`); a LAN endpoint sends the key in cleartext. Disk storage is already mode-0600 (acceptable); the transport is the gap.
- **Remediation:** warn or refuse `api_key` + non-https endpoints unless the host is loopback.

#### P3 — no HTTP retry/backoff; single-shot requests (`src/providers/http.rs`)

- **Root cause:** `get_bytes`/`get_bytes_query`/`post_json` each issue exactly one call; a transient TLS/DNS blip surfaces as "unreachable". translate partially compensates with backend racing and the fail-cache; fx with refresh backoff.
- **Remediation:** one idempotent-GET retry with jitter; keep POSTs single-shot.

#### P3 — stat-vs-read TOCTOU bypasses the 2 MiB code gate (`src/ui/preview.rs:724` vs `755`)

- **Root cause:** the size gate uses the stat captured in `update()`; the worker read 45 ms later happily loads a file grown/replaced past the cap, feeding it to main-thread `set_text` (compounding the freeze finding). CWE-367.
- **Remediation:** re-stat or `Read::take(MAX_CODE_BYTES + 1)` in the worker.

#### P3 — non-UTF-8 code files report "Could not load file" (`src/ui/preview.rs:755-766`)

- **Root cause:** `read_to_string` fails on any invalid byte; UTF-16/Latin-1 sources are conflated with unreadable files. CWE-755.
- **Remediation:** `fs::read` + `String::from_utf8_lossy` in the worker; reserve the error for real I/O failures.

#### P3 — ffmpeg/pdftoppm resolved via inherited PATH with full environment (`src/ui/preview.rs:1384-1414`, `1413`, `1442`)

- **Root cause:** `Command::new("ffmpeg")` does a PATH search with inherited env; a writable PATH directory yields execution of a planted binary with session tokens in env. Defense-in-depth on a single-user desktop (CWE-426/CWE-911), hence P3.
- **Remediation:** resolve absolute converter paths at startup from a config/`which`, and build a minimal child env.

#### P3 — EXIF orientation never applied to JPEG previews or cached thumbnails (`src/ui/preview.rs:1484-1489`, `1181-1188`, `1249`)

- **Root cause:** `Pixbuf::from_file_at_scale` ignores the EXIF Orientation tag; phone photos preview sideways and poison the shared FreeDesktop thumbnail cache with unrotated images.
- **Remediation:** read orientation and `rotate_simple` before caching.

#### P3 — converter scratch files leak on crash; no GC (`src/ui/preview.rs:1284-1293`, removal only at `1374`, `1468`)

- **Root cause:** temp PNG removal is not in a Drop guard; SIGKILL/OOM mid-conversion leaves files in `~/.cache/hark/preview/` forever with no startup sweep.
- **Remediation:** RAII deletion guard and a startup sweep of entries older than ~1 hour.

#### P3 — `clear()` leaves prior file's text in the code buffer (`src/ui/preview.rs:396-406`)

- **Root cause:** `clear()` resets picture/stack/path/gen but never the sourceview buffer; up to 2 MiB of previously previewed source stays resident after hide (no visible flash today since all paths set chrome first — retained memory only).
- **Remediation:** `buffer().set_text("")` in `clear()`.

### Clean areas verified in this pass

- No regex usage in plan/deep/search-mod — the hand-rolled two-pointer glob matcher is O(|pat|·|name|) on short bounded inputs, not catastrophic-backtracking class (no ReDoS). The only non-test `unwrap` (`deep.rs:577`) is guarded by `segments.len() > 1`.
- Walk bounds enforced three ways (visit cap 40k, 200 ms budget, depth ≤6) with checks at root start, `filter_entry`, and per-entry; permission-denied errors are skipped before consuming the visit cap; deep ids (`path:` prefix) dedupe against live glob listings and across jobs via a shared `existing` set.
- Tokenization edges clean: all-meta glob queries rejected; repeated slashes filtered; `~` mid-query goes free-text; plain `foo.rs` not misrouted to glob; `strip_file_mode_prefix` order-safe for `firefox`/`filename`.
- TLS: no `danger_accept_invalid*` anywhere in `src/`; body caps wrap the decompressed stream (`take(4 MiB)`, bounding gzip bombs); Content-Length never trusted; connect vs total timeouts split per agent; zero `unwrap`/`expect` on fallible paths in all three providers; translate UI path touches only in-memory mutexes (no FS/HTTP on main loop); mem caches LRU-capped (256/64) with disk sweep at 500; fx `convert_amount` rejects non-finite/overflow with tests; Google/LibreTranslate parsers reject empty results; language codes validated before sending and query length capped pre-network.
- Preview converter hygiene: children reaped on all four exit paths with `Stdio::null` (no pipe deadlock); no shell, paths passed as single args; temp names from `/dev/urandom` with `create_new` 0600 in a 0700 dir (no pre-plant race); `guess_language` is a table lookup with no panic path; every async continuation is gen- and `last_path`-gated; worker threads own only `Send` data; `pixbuf_to_pixels` validates dims/bit-depth/channels before the pixel copy; GIF decode uses static first-frame APIs; killed-converter partial PNGs removed.

### Pass 16 verification log

- Three parallel subagent sweeps: (a) `files/search/plan.rs`+`deep.rs`+`search/mod.rs` as a unit; (b) `http.rs`/`fx.rs`/`translate.rs` increased-depth re-verification; (c) `ui/preview.rs` re-sweep. All three emitted full per-finding reports.
- Every finding's cited `file:line` was re-read and confirmed against current source by the main auditor, including line-by-line confirmation of the P1 lowercasing mismatch at `plan.rs:101-114`/`257-286` (the Pass 4 clearance explicitly re-checked and superseded: it covered the rewritten `str::find` path in translate.rs, not this planner).
- `cargo clippy --all-targets` — clean; `cargo test` — 253 passed / 0 failed / 2 ignored; captured to scratch (`clippy_pass16.log`, `test_pass16.log`). cargo-deny/MSRV still unavailable (unchanged).
- No source changes were made in this pass.
- This pass produced **21 new findings** (1 P1, 4 P2, 16 P3); it is **not** a zero-finding clean pass.

### Termination condition status (Pass 16)

Termination condition still **not met** (Passes 13–16 yielded 24, 21, 25, 21 new findings). Pass 16 found the audit's first new P1 since the original review. Named areas for Pass 17: cross-cutting remediation re-verification of prior findings' fixed statuses (re-confirm the "fixed" entries in the findings table at the top of this file), `providers/files/hot.rs` + `mod.rs` file-provider surface, and `ui/rows.rs` pool/lifecycle at increased depth beyond Pass 13's findings.

---

## 🧫 Pass 17 fixed-status re-verification, files provider surface & row-pool depth audit (2026-08-26)

Areas targeted per the Pass 16 close-out: full re-confirmation of every tracker item marked "fixed" against current source, `providers/files/mod.rs` + `hot.rs` provider surface, and `ui/rows.rs` at increased depth. All line numbers verified against current source.

### Fixed-status re-verification result (clean)

All 34 tracker items marked "fixed" were re-checked against the current source; **every fix is live** — including the three original crash bugs (#1 translate `str::find`, #2 glob boundary advance, #3 key-capture guard via `and_downcast::<gtk::Editable>` at both interceptors). Two findings were fixed by valid alternative implementations rather than the suggested patch: #5/#N2 (timezone offsets intercepted by explicit shape check at `timezone.rs:293-299` rather than changing `normalize_place_key`) and #8/#9 (urandom-seeded xorshift + i128/u128 rejection sampling at `quick.rs:670-673`, `701-708`, rather than a full CSPRNG swap). Open items spot-checked (#21, #23, #24, #26, #29, eng-q, dt-hr, apps-z) remain genuinely open — none was silently fixed. No tracker corrections needed; this sub-sweep produced **zero new findings**.

### New verified findings

#### P2 — reveal failure is invisible: `Launched` returned unconditionally (`src/providers/files/mod.rs:506-519`, `engine.rs:411-414`)

- **Root cause:** `reveal_in_file_manager` returns `()`; missing paths and total failure (no DBus, no file managers) surface only as `eprintln!`, while the engine maps the call to `ExecuteOutcome::Launched` — the UI then hides the window (`ui/mod.rs:1893`) and nothing opens, with zero user feedback. CWE-754-class error handling.
- **Remediation:** return `Result<(), String>` mirroring `trash_path`; map failure to `ExecuteOutcome::Failed` (keeps the window open) or surface a toast.

#### P2 — trash failure builds a good error string but never shows it (`src/engine.rs:421-424`, `ui/mod.rs:1931-1933`)

- **Root cause:** `trash_path` produces clear messages ("Path no longer exists", gio exit codes) but the engine only `eprintln!`s them; `ExecuteOutcome::Failed` in the UI merely refocuses the entry. A read-only mount or foreign-owned file yields a closed modal and a silently un-trashed file — stderr is not a user surface for a GTK app.
- **Remediation:** show the error string in an AlertDialog/toast on `Failed`, at least for destructive actions.

#### P3 — trashed hot/index entries stay searchable until TTL rebuild (`src/engine.rs:415-425`, `files/mod.rs:140-144`)

- **Root cause:** trash clears only the live cache; the in-memory index still contains the path (phase-1 scan matches it) and the hot set keeps its index position until the next rebuild intersected with usage. Retyping a prefix of a just-trashed hot file re-surfaces it; Enter opens a missing file.
- **Remediation:** on successful trash, remove the entry from the in-memory index under its write lock (covers both index and hot paths).

#### P3 — hot-set eviction nondeterministic on frecency ties (`src/usage.rs:124-142`, `hot.rs:96-112`)

- **Root cause:** bucketed frecency yields many ties; stable sort keeps HashMap iteration order (randomized per process), so the 64-of-128 cut admits a random subset per session — top results for tied paths change across restarts. (Related to the Pass 13 `top()` tie-break finding; this is the hot-set consumer path.)
- **Remediation:** deterministic tie-break (count, last, path id) in `top_path_ids`.

#### P3 — `scoped_memo` single-slot cache never invalidated on index rebuild (`src/providers/files/mod.rs:272-299`)

- **Root cause:** the memoized `is_scoped_query` verdict is computed against the index state at query time and survives rebuilds. A scoped query evaluated while the background build is still populating the index memoizes `false`, and the wrong verdict steers `should_deep_search` until a different query evicts the slot.
- **Remediation:** clear `scoped_memo` in `rebuild_index()`/`force_rebuild()` or store an index generation counter with the memo.

#### P3 — xdg-open spawn success treated as open success; child stdio inherited (`src/providers/files/mod.rs:429-434`)

- **Root cause:** `.spawn()` returns before handler lookup; a missing default app exits non-zero after the engine already returned `Launched` and hid the window. Unlike every other spawn in the file, no `.stdout/.stderr(Stdio::null())`.
- **Remediation:** detach stdio; optionally waitpid on a worker thread and surface a late toast on non-zero exit.

#### P3 — trash pre-check TOCTOU degrades the error message only (`src/providers/files/mod.rs:583-589`)

- **Root cause:** classic exists-check-then-act; a file deleted in between yields the generic gio-exit message instead of "no longer exists". Not exploitable (argv-only, `--` separator, no shell). CWE-367 (cosmetic here).
- **Remediation:** drop the pre-check; map gio's exit status/stderr to the friendly message.

#### P3 — icon resolve cache never invalidated on icon-theme change (`src/ui/rows.rs:76-78`, `664-676`)

- **Root cause:** the cache is cleared only by the symbolic-icons toggle; nothing connects `IconTheme::changed`. After installing a theme where a previously-missing icon name now resolves, rows keep the generic fallback for up to 512 entries indefinitely.
- **Remediation:** `IconTheme::for_display(...).connect_changed(...)` → `clear_icon_resolve_cache()`.

#### P3 — `icon_file_path` stats the filesystem per row per keystroke on the main thread (`src/ui/rows.rs:606-610`, called at `570`)

- **Root cause:** the file-path branch short-circuits before the resolve cache; `Icon=/path/app.png`-style entries incur a synchronous `stat()` for every bound row on every keystroke — on a cold/hung NFS home this freezes input handling.
- **Remediation:** cache the `(requested_path) → Option<PathBuf>` result or resolve asynchronously.

#### P3 — conversion flip during an in-progress transition primes the outgoing panel (`src/ui/rows.rs:449-477`)

- **Root cause:** `cur` is read from `visible_child_name()`, which GTK updates immediately while the 160–220 ms transition still runs; a second wheel press within the window writes the new value into the half-hidden panel and flips back to it — visible stutter. (Distinct from the Pass 13 stacked-timer finding; this is the Stack state machine itself.)
- **Remediation:** track the side in a `Cell<bool>` on `PooledRow`, or coalesce if a transition is still running.

#### P3 — `highlight_markup` is O(n²) in title length (`src/ui/rows.rs:118-119`)

- **Root cause:** `title[..byte].chars().count()` re-scans from byte 0 per character; a 200-char CJK title costs ~40k iterations per row per keystroke × up to 25 rows.
- **Remediation:** `for (pos, (byte, ch)) in title.char_indices().enumerate()` — the enumerate index is already the char index.

#### P3 — wheel-navigate rebinds all 25 rows instead of the swapped two (`src/ui/rows.rs:211-220`, `ui/mod.rs:752-758`)

- **Root cause:** `conv_swap_to_front` only permutes order, but `apply` has no diffing — every arrow press on a conversion set redoes icon resolution (amplifying the stat-per-row finding), markup rebuild, and label writes for the whole list inside a `row_selected` callback.
- **Remediation:** bind only slots 0 and the swapped item's old position, or short-circuit `bind` by item id.

### Clean areas verified in this pass

- Fixed-status sweep: 34/34 fixed items live; open items genuinely open (detailed list in the sub-sweep output captured in the pass log below).
- files/mod.rs + hot.rs: trash delegates to `gio trash --`/`gvfs-trash` (FreeDesktop spec implemented by gvfs — no custom trash logic to be wrong); `spawn_detached` quoting escapes embedded quotes and bins are hardcoded literals (no injection); action-id format `path:{display}` consistent across all four producers and the UI trash-removal; locks poison-recovering only; hot vs index scored by the same `score_name_only` with `seen` dedup; `merge_cached` id-dedup and sort deterministic; `which_bin` uses `split_paths`; `resolve_path` single metadata call.
- rows.rs: pool shrink/grow detaches exactly the trailing slots (no stale-visible widgets); `row_at` bounded by `attached` and every caller handles `None`; highlight indices are char indices aligned with engine producers (which bail when lowercasing changes char count — verified including fuzzy-matcher 0.3.7 semantics); accent interpolation reaches only `sanitize_hex`-validated strings; icon cache keys use NUL separators with kind/symbolic disambiguation; `RefCell` borrows non-overlapping; `suppress_select` guards the select-row re-entry; mode switches idempotent.

### Pass 17 verification log

- Three parallel subagent sweeps: (a) fixed-status re-verification of the tracker; (b) `files/mod.rs`+`hot.rs` provider surface; (c) `rows.rs` depth audit. The first launch of all three failed with API 405 infrastructure errors and was relaunched.
- Every finding's cited `file:line` was re-read and confirmed against current source by the main auditor before recording (all 12 findings spot-checked).
- `cargo clippy --all-targets` — clean; `cargo test` — 253 passed / 0 failed / 2 ignored; captured to scratch (`clippy_pass17.log`, `test_pass17.log`). cargo-deny/MSRV still unavailable (unchanged).
- No source changes were made in this pass.
- This pass produced **12 new findings** (2 P2, 10 P3); it is **not** a zero-finding clean pass. The fixed-status sub-sweep itself was clean (zero tracker discrepancies).

### Termination condition status (Pass 17)

Termination condition still **not met** (Passes 13–17 yielded 24, 21, 25, 21, 12 new findings). The rate is declining (25→21→21→12), consistent with saturation. Named areas for Pass 18: `providers/calc/timezone.rs` + `quick.rs` at increased depth (both large and only partially swept), `src/typos.rs` learning algorithms beyond Pass 13's store-level findings, and a cross-module chain analysis tying the P1/P2s of Passes 13–17 into exploit/failure chains (Phase-4 composite analysis).

---

## ⛓ Pass 18 calc depth, typo-learning algorithms & Phase-4 composite chain audit (2026-08-26)

Areas targeted per the Pass 17 close-out: `calc/timezone.rs` + `quick.rs` at increased depth, `typos.rs` learning algorithms (beyond Pass 13's store I/O findings), and Phase-4 composite chain analysis of the Passes 13–17 P1/P2 catalog. All line numbers verified against current source.

### New verified findings

#### P2 — DST-gap/ambiguous local times silently produce no card (`src/providers/calc/timezone.rs:510-516`)

- **Root cause:** `and_local_timezone(from_tz).single()?` returns `None` when a wall-clock time falls in a DST gap (spring-forward) or is ambiguous (fall-back). `01:30 in new york` on a fall-back date yields nothing at all — no card, no error, the query just falls through.
- **Remediation:** use `.earliest()`/`.latest()` with a disambiguation hint (or render both candidates in the card) instead of `.single()?`.

#### P2 — password/uuid silently fall back to predictable PRNG when `/dev/urandom` is unreadable (`src/providers/calc/quick.rs:652-678`)

- **Root cause:** `os_random_bytes` falls back to the xorshift stream whose seed floor is `nanos ^ (pid × golden-ratio)` — predictable to anyone who can guess start-time and pid (CWE-338). The fallback is silent: the card presents the result as a password with no warning. (Phase-4 chain 8 correctly refuted the *constant-seed* hypothesis — time+pid is the floor, urandom mixes in per-generation when available — but the degraded path remains an unflagged weak generator.)
- **Remediation:** refuse to generate (error card) when urandom is unavailable, or label the output as non-cryptographic in that case.

#### P2 — manual pins are structurally indistinguishable from learned aliases; one conflicting launch silently overwrites a pin (`src/typos.rs:289-296`, `168-171`)

- **Root cause:** `AliasEntry` has no `manual` flag; `set_manual` writes `count = STRONG_COUNT.max(2) = 2`, which is exactly the `e.count <= 2` condition the auto-learner treats as "unconfirmed and replaceable". One accidental near-title launch of the same key retargets and resets the user's explicit pin. CWE-1259-class automatic-override defect.
- **Remediation:** add `manual: bool` (`#[serde(default)]`) to `AliasEntry`; skip the conflict-switch arm when `e.manual`.

#### P2 — alias boost never decays: `lookup` uses only `count`; the 21-day decay is dead code for ranking (`src/typos.rs:96-107` vs `455-464`)

- **Root cause:** `alias_frecency`'s exponential decay feeds only `list()` and `prune_aliases()`; the runtime ranking path (`lookup` → `apply_typo_alias`, `engine.rs:553-566`) keys purely on `count`, which only grows. A two-year-stale alias still yields `BOOST_STRONG = 18_000` (injection floor 22_000 + 18_000 = 40_000), outranking every prefix hit.
- **Remediation:** gate the boost on `e.last` recency or scale it by the same decay in `lookup`.

#### P2 — self-reinforcement loop with no escape: alias-driven launches feed the same alias (`src/typos.rs:110-152`, `engine.rs:557-565`, `ui/mod.rs:2058-2061`)

- **Root cause:** injection puts the alias target at ≥30_000 (top rows); activating the top row re-runs `learn_from_launch` with the same query, incrementing the alias `count` — monotone entrenchment with no decrement path and no penalty for the correct spelling. One accidental Enter can pin a wrong target at BOOST_STRONG permanently (compounded by the non-decay finding).
- **Remediation:** pass a `via_alias: bool` flag from search to activation and don't count launches the alias itself produced.

#### P3 — composite chain: IPC slowloris wedge → duplicate GTK Application spawn (Phase-4 chain 1, VERIFIED, P2)

- **Trace:** serial listener (`ipc.rs:96-113`) wedged by silent clients → `request_toggle` (`ipc.rs:33-59`) burns its retry budget and returns `false` → `main.rs:44-48` falls through and spawns a **full second GTK Application + `Engine::new()`** per hotkey press while the daemon is wedged; `bind_socket` no-ops for the zombie, and any connection that does land feeds the unbounded toggle channel (Pass 14 finding). Local unprivileged DoS. CWE-400.
- **Remediation:** as per the Pass 14 IPC findings (per-connection dispatch or non-blocking accept).

#### P3 — composite chain: translate cache poison → attacker-controlled clipboard (Phase-4 chain 2, VERIFIED, P2 default / P1 in HOME-less env)

- **Trace:** `cache_dir()` `/tmp` fallback (`translate.rs:923-927`) reachable when both XDG cache and HOME are unset → key is plain computable FNV-64 (`:1064-1077`) → `cache_get` (`:947-960`) verifies neither q/source/target nor ownership (no O_NOFOLLOW, contrast `fx.rs:329-357`) → `ok_result` sets `Action::Copy(attacker_string)` → Enter copies it (`engine.rs:402-408`) → the poisoned entry is also **promoted into process memory** (`:955-958`), surviving file removal until TTL. CWE-349/345. In default environments (0700 user cache dir) the chain requires prior file control, hence P2 there.
- **Remediation:** as per the Pass 16 finding (verify entry fields, replicate fx.rs guards, reject the `/tmp` fallback).

#### P2 — composite chain: corrupt-but-parseable `usage.json` count → release-build daemon abort (Phase-4 chain 3, VERIFIED — worse than the Pass 14 finding)

- **Trace:** serde accepts `count: u64::MAX` (no clamp, `usage.rs:16-18`) → `frecency`'s float→int cast saturates to `i64::MAX` (`:205-220`) → non-saturating `r.score += boost` (`engine.rs:283`, `:333`) → the Pass 14 finding said "wraps negative in release", but `Cargo.toml:58` sets **`panic = "abort"`**, so the overflow panic kills the whole daemon in release builds too, on every keystroke matching the poisoned id. CWE-190 + CWE-754.
- **Remediation:** `saturating_add` at both sites (one-line fix) and/or clamp `count` on load.

#### P1 — composite chain: exclude-`"/"` config + missing-path scoped query → deterministic daemon abort (Phase-4 chain 5, VERIFIED)

- **Trace:** `index.exclude = ["/"]` survives load (no sanitize) → `ExcludeSet::from_list` pushes an empty pattern (`config.rs:1185-1195`) → any scoped query at a missing absolute path (`report.md in /nonxistent`) falls back to walking `/` (Pass 16 finding, `deep.rs:285-290`) → the walk's first entry hits `should_skip_entry(root="/")` (`deep.rs:646`) → `ExcludeSet::matches` calls `comps.windows(0)` (`config.rs:1220-1234`) → **panic** — and the deep walk thread (`ui/mod.rs:2676`) has no `catch_unwind`, with `panic = "abort"` the entire daemon dies. Deterministic, repeatable, user-input-triggered. CWE-20 + CWE-754. (Compounds two individually-known findings into a P1 chain; the `windows(0)` empty-pattern fix and the `/`-fallback fix each break the chain independently.)
- **Remediation:** fix either leg (skip empty exclude patterns; or stop the `/` fallback) — and consider `catch_unwind` in the deep-walk worker as defense-in-depth.

#### P3 — predict-tz short-alias scoring misroutes ambiguous city prefixes (`src/providers/calc/timezone.rs`)

- **Root cause:** `starts_with(alias)` matching routes `time in s` → San Francisco and `15:00 in las` → Los Angeles; `15:00 in b` → BST/London; no diacritic folding makes `são paulo` unresolvable. CET/EST abbreviation labels contradict the DST math in summer.
- **Remediation:** require ≥3-char aliases, fold diacritics, and relabel fixed-offset abbreviations during DST.

#### P3 — quickwin numeric/roman edge cases (`src/providers/calc/quick.rs`)

- Hex-only English words parse as numbers (`decade to dec` becomes base conversion); `from_roman` accepts non-canonical numerals (`roman ic` → 99); reversed `random a b` bounds always return `a` while the badge claims `a..=b`; `format_number` collapses sub-5e-9 values to `0`.
- **Remediation:** require `0x` prefix or digit-only tokens for hex; validate canonical Roman form; swap/correct reversed-bounds badge; format tiny magnitudes in scientific notation.

#### P3 — full-title alias branch loosens the edit budget (`src/typos.rs:399-411`)

- **Root cause:** the prefix window uses `max_edit_distance(ql)` but the full-title branch passes `ql.max(tl)` — every 4-char alias gets a doubled error budget (2 edits) against 5–7-char titles (`cdde` matches `codes` despite the declared 1-edit policy for 4-char keys).
- **Remediation:** use `max_edit_distance(ql)` in both branches.

#### P3 — v2 session sweep learns abandoned dead-end tokens (`src/typos.rs:134-146`)

- **Root cause:** all raw last-12 keystroke snapshots are evaluated as candidates against the launched title; `wat`→`wats`→`watss`→`whatsapp` mints `wats` and `watss` as independent injected (30_000) aliases — backspace dead-ends the user retreated from still hijack results.
- **Remediation:** learn only queries within edit distance 1–2 of the *final* query (typo reformulations), not of the title alone.

#### P3 — prune can evict aged manual pins; equal-frecency eviction is process-random (`src/typos.rs:440-452`)

- **Root cause:** same missing `manual` flag — a 3-week-old pin (decayed ≈736) loses to a 3-day-old count=1 alias (≈1360) at the 300-cap; HashMap order randomizes tie eviction.
- **Remediation:** exempt manual entries; tie-break on `(frecency, alias)`.

#### P3 — `set_manual` accepts structurally invalid target ids (`src/typos.rs:279-282`)

- **Root cause:** prefix-only validation lets `"app:"` and `"path:not a path!?"` persist as strong-count zombie aliases (the key side is validated; the id side isn't).
- **Remediation:** require non-empty remainder after the prefix and engine-resolvability before insert.

#### P3 — alias + usage boosts stack and can outrank an exact-name match (`src/engine.rs:283`, `557-565`)

- **Root cause:** an alias-target launch increments both stores; prefix 30_000 + 18_000 alias + ~2_400 usage ≈ 51_200 > 50_000 exact score — installing an app literally named `chrom` cannot reclaim the top slot from an old `chrom`→Chrome alias.
- **Remediation:** cap the combined personal boost below the exact-match score, or apply `max(alias, usage)` for alias targets.

#### P3 — dead `path:` alias targets stat the filesystem per keystroke on the UI thread forever (`src/engine.rs:361-364`, `562`)

- **Root cause:** `resolve_id` → `resolve_path` runs on every search for the matching query; a renamed/unmounted target means a repeated metadata syscall with no timeout, and the alias never self-cleans (compounds the non-decay finding; on network mounts this is a UI stall).
- **Remediation:** drop/demote the alias after N consecutive resolve failures.

### Phase-4 chain analysis verdicts (chains 4, 6, 7, 8 refuted)

- **Chain 4 (.desktop icon → crash/exec): REFUTED** — icon rendering delegates entirely to `gio::FileIcon` + GDK; Hark performs no manual image parsing on that path. Worst outcome is spoofing (attacker-chosen image displayed in a row).
- **Chain 6 (preview freeze + clear timer compounding): REFUTED** — both bugs are real but never compound: the main loop can't run the timer mid-freeze, and `preview.clear()` never touches the sourceview buffer.
- **Chain 7 (config drop + keystroke persistence): REFUTED** — both legs operate on the same typed struct; one save drops all unknown keys at once, no progressive erosion.
- **Chain 8 (predictable password seed): REFUTED as posed** — no constant/time-only seed; urandom mixes in per-generation, time+pid is only the floor (the degraded-path finding above remains, downgraded accordingly).

### Clean areas verified in this pass

- Timezone: AM/PM midnight/noon, IST +5:30 round-trips (HALF_HOUR_ZONES), UTC+14/Etc sign inversion, deterministic list ordering, `here` detection, password byte rejection sampling (248), UUID v4 version/variant bits — all clean; calc results use `set_text` (no markup injection); no color-conversion provider exists.
- Typos: Levenshtein correctness (Wagner–Fischer, empty fast paths); prefix-window slicing on `Vec<char>` cannot split codepoints and empty ranges are safe; candidate generation never scans the corpus (≤13 candidates per launch); Copy actions do not learn; learning excludes calc/conversion/command kinds; alias ids restricted to `app:`/`path:` at both ingestion paths; score arithmetic saturating; `normalize_alias` unicode-safe length check; lock scoping deadlock-free.
- Chains 1/2/3/5 verified end-to-end with file:line at every step; refuted chains 4/6/7/8 each have an identified blocking step.

### Pass 18 verification log

- Three parallel subagent sweeps: (a) `calc/timezone.rs`+`quick.rs` depth; (b) `typos.rs` learning algorithms; (c) Phase-4 composite chain analysis of the Passes 13–17 P1/P2 catalog. One 405 infrastructure failure on the chain agent was relaunched. The timezone/quick agent returned a summary-plus-detail format; all its findings' citations were verified by the main auditor.
- Every finding's cited `file:line` was re-read and confirmed against current source, including `Cargo.toml:58` `panic = "abort"` (which upgrades two prior findings' release-build impact) and the chain-5 trace (`config.rs:1185-1195` → `deep.rs:285-290` → `deep.rs:646` → `config.rs:1220-1234` → `ui/mod.rs:2676`).
- `cargo clippy --all-targets` — clean; `cargo test` — 253 passed / 0 failed / 2 ignored; captured to scratch (`clippy_pass18.log`, `test_pass18.log`). cargo-deny/MSRV still unavailable (unchanged).
- No source changes were made in this pass.
- This pass produced **15 new findings** (1 P1 chain, 5 P2, 9 P3); it is **not** a zero-finding clean pass. Two prior findings were re-assessed (usage-overflow now a release-abort; the chain-5 composite elevates two P2s to a P1 chain).

### Termination condition status (Pass 18)

Termination condition still **not met**. Named areas for Pass 19: remediation-priority triage of the full 13–18 catalog into a fix roadmap (analysis deliverable), then re-sweeps of the two areas that have produced findings at every depth increase (`ui/mod.rs`, `files/search/`) to test for saturation, and `Cargo.lock` dependency version drift review (cargo-deny unavailable — manual triage).

---

## 🧯 Pass 19 saturation re-sweeps & dependency drift review (2026-08-26)

Areas targeted per the Pass 18 close-out: final-depth saturation re-sweeps of `ui/mod.rs` and `files/search/` (mod/rank/glob), plus a manual `Cargo.lock` drift review (cargo-deny and cargo-outdated both unavailable in this environment). All line numbers verified against current source.

### New verified findings

#### P2 — `path_completions` bypasses the secrets/artifact exclude filter (`src/providers/files/search/glob.rs:541-569`)

- **Root cause:** the completion loop consumes `read_dir` entries with only a hidden-file and prefix check — it never receives or calls `should_skip_entry`, unlike both other live-listing paths (`search_absolute_glob` at `glob.rs:231`, `maybe_live_relative_glob` at `:462`). That filter is what hides `.ssh`, `.gnupg`, key material, `.env*`, `credentials.json` (verified by the `skips_ssh_gnupg_and_key_material` test at `index.rs:838`).
- **Failure pathway:** typing `/home/u/.ssh/` routes to completions (`starts_with('/')`) and lists `id_ed25519`, `known_hosts`, etc. — the index-era confidentiality exclusion is trivially bypassed by the simplest path-typing UI. CWE-200 / CWE-552.
- **Remediation:** thread `excludes` into `path_completions` (already a parameter of `search_index`) and `continue` on `should_skip_entry(&path, excludes)`.

#### P3 — mixed-coordinate-space monitor top margin (`src/ui/mod.rs:3011-3017`)

- **Root cause:** the Hyprland path derives the top margin from logical-space `geom.height / 5` while the GDK fallback uses a hardcoded physical-space `80`, and `.max(80)` clamps short logical panels; `gdk_monitor_for_hypr` containment compares Hypr logical coords against GDK physical geometry — on scaled multi-monitor setups the launcher sits at different heights between compositors and can bind to the wrong output. CWE-682.
- **Remediation:** scale Hypr logical geometry by the GDK `scale_factor()` before comparison; derive the margin from the selected GDK monitor uniformly.

#### P3 — `learn_typos` gating diverges between primary Enter and the secondary action panel (`src/ui/mod.rs:2052-2063` vs `1886-1906`)

- **Root cause:** two hand-maintained predicates: primary Enter excludes Calc/Conversion/Command by kind; the secondary panel filters by `spec.id`, and its `"reveal"`/`"terminal"` actions are offered on Command rows — firing `record_usage` for `cmd:` ids that primary Enter never records. Usage stats become activation-path-dependent. CWE-1077.
- **Remediation:** extract one `should_learn(kind, spec_id)` used by both call sites.

#### P3 — failed clipboard copy is silent; Ctrl+Enter closes the launcher with the clipboard untouched (`src/ui/mod.rs:975-989`, `engine.rs:402-408`)

- **Root cause:** `copy_to_clipboard` shells out to `wl-copy`/`xclip`; on systems with neither, `Action::Copy` returns `Failed` (only `eprintln!`), but the Ctrl+Enter fast path ignores the outcome and unconditionally hides the window — the user believes the result was copied. CWE-754.
- **Remediation:** check the `ExecuteOutcome`; on `Failed` keep the window open and surface an inline error.

#### P3 — `split_glob_path` silently drops mid-path wildcard components (`src/providers/files/search/glob.rs:329-344`)

- **Root cause:** for `/home/u/*/docs`, the parent is derived from the *first* metacharacter but the pattern from the *last* component — the live listing returns only the literal child `/home/u/docs`, inconsistent with the index supplement (which matches `docs` anywhere). Silent degradation with no user-facing signal. CWE-636.
- **Remediation:** reject mid-component globs explicitly with a hint, or collect all post-metachar components into a per-segment match.

### Dependency drift review (manual, cargo-deny/cargo-outdated unavailable)

- Lockfile: 204 crates. Key direct dependencies are current within their minor series: gtk4 0.9.7, glib 0.20.12, gtk4-layer-shell 0.5.0, ureq 2.12.1, walkdir (via tree), regex 1.13.0, serde_json 1.0.150, chrono 0.4.45, chrono-tz 0.10.4, lofty 0.25.1, sourceview5 0.9.1, fuzzy-matcher 0.3.7, async-channel 2.5.0. No obviously stale major-pin or known-CVE-carrying version identified by manual inspection; full advisory coverage still requires `cargo deny check` (unavailable here — recorded honestly, unchanged since Pass 3F). Captured to scratch (`dep_drift_pass19.log`).

### Saturation assessment

- `files/search/mod.rs` and `rank.rs`: **SATURATED** — band arithmetic, boost/penalty application, fuzzy-span keys, depth tie-breaks, `**` handling, case-folding, dedup, and unwrap/panic grep all re-verified clean with zero new findings.
- `files/search/glob.rs`: not saturated (two new findings this pass, including the P2 filter bypass).
- `ui/mod.rs`: near-saturated — three new findings remain (monitor coordinate space, learn-gating divergence, silent copy failure); settings/stack switching, `activate_result` branches, `session_queries` bounds, widget recycling, unwraps, and Hypr cache were re-verified clean.

### Pass 19 verification log

- Two parallel subagent saturation sweeps (ui/mod.rs final depth; files/search mod/rank/glob) plus an in-main-thread dependency drift review.
- Every finding's cited `file:line` re-read and confirmed against current source by the main auditor.
- `cargo clippy --all-targets` — clean; `cargo test` — 253 passed / 0 failed / 2 ignored; captured to scratch (`clippy_pass19.log`, `test_pass19.log`).
- No source changes were made in this pass.
- This pass produced **5 new findings** (1 P2, 4 P3); it is **not** a zero-finding clean pass — but two of the four swept files/subsystems came back saturated.

### Termination condition status (Pass 19)

Termination condition still **not met** (Passes 13–19: 24, 21, 25, 21, 12, 15, 5 new findings — declining toward saturation). Remaining unsaturated areas for Pass 20: `providers/calc/expr.rs` + `math.rs` re-verification at increased depth (last covered Pass 7), `engine.rs` action execution table (`Action` enum dispatch completeness), and the packaging assets not yet examined in Pass 15's partial sweep (scripts/install.sh vs install-user.sh divergence).

---

## 🧮 Pass 20 expression parser depth, Action dispatch & packaging pipeline audit (2026-08-26)

Areas targeted per the Pass 19 close-out: `calc/expr.rs`+`math.rs` at increased depth, full `Action` enum dispatch verification, and the packaging/release pipeline scripts beyond Pass 15's partial sweep. All line numbers verified against current source. Note: the dispatch sweep's reveal-false-success and sed-metacharacter items duplicate Pass 17 F1 and Pass 15 findings respectively and are not re-counted.

### New verified findings

#### P1 — stack overflow abort via deeply nested parentheses in the expression parser (`src/providers/calc/expr.rs:238-241`, recursion cycle `:196-244`)

- **Root cause:** each `(` token recurses through the full `parse_primary → parse_expr → parse_add → parse_mul → parse_unary → parse_pow → parse_primary` cycle (`expr.rs:132-244`) with no depth counter anywhere. `looks_like_math` (`math.rs:20-35`) passes for `"(".repeat(15000) + "1+1" + ")".repeat(15000)`, and the tokenizer happily emits the tokens.
- **Empirical proof** (standalone harness compiled from the exact `expr.rs`, project untouched): on a default 2 MiB spawned thread (the background search thread's configuration), **d = 15,000 nested parens (30 KB query) aborts with "stack overflow"**; d = 10,000 survives. Stack overflow is a runtime abort, not a catchable `Option` — with `panic = "abort"` this kills the daemon. Paste-only (not typeable), but a typed/pasted query crashing the daemon. CWE-787/CWE-674.
- **Remediation:** thread a `depth: usize` through the parser family and bail at ~200; the `explain_str` precedence printer (`expr.rs:310-446`) has the same unbounded cycle and needs the same bound.

#### P3 — online installer downloads the tarball with no integrity check despite publishing SHA256SUMS (`dist/install.sh:31-34`)

- **Root cause:** `curl -fsSL "$URL_VERSIONED" -o "$TMP/pkg.tar.gz"` then straight to `tar -xzf`; TLS is the only protection while `dist/SHA256SUMS` exists. CWE-494.
- **Remediation:** fetch and `sha256sum -c` the checksums before extraction.

#### P3 — PATH hint ignores `XDG_BIN_HOME` (`packaging/install-user.sh:95-98`)

- **Root cause:** the install honors `${XDG_BIN_HOME:-…}` (line 7) but the hint always prints `$HOME/.local/bin`. Users with custom XDG paths get misleading advice.
- **Remediation:** print `"$BIN_DIR"`.

#### P3 — release-pipeline rename skew breaks the online installer (`scripts/package-release.sh:27,30` vs `dist/install.sh:20`)

- **Root cause:** `package-release.sh` still stages `hark-${VERSION}-…` packages (and its documented default repo is the Hark repo), while the installer fetches `blink-${VERSION}-…` — a freshly packaged release produces an asset the installer can never find (404 on both URL variants).
- **Remediation:** complete the rename in `package-release.sh` (PKG_NAME, repo fallback) or parameterize both.

#### P3 — dev install script restarts the wrong process name (`scripts/install.sh:46-66,92-94`)

- **Root cause:** `pgrep -x hark`/`pkill -x hark` target the old binary name; on machines running the blink-named release, restart kills/starts nothing while reporting success or errors confusingly. Self-consistent for dev use only.
- **Remediation:** match the shipped binary name (or accept it as a dev-only script and document).

### Clean areas verified in this pass

- Expression evaluator re-verified at depth: tokenizer edges (`1e`, `1e+`, `.5`, `5.`, `.`, unicode minus/digits, long identifiers, empty parens, magnitude-suffix rescan), no quadratic behavior, every loop advances; numeric semantics (`-2^2=-4`, right-assoc power, `-0.0` divisor rejected, f64 modulo sign convention, factorial 0–170 with near-integer tolerance, `0.1+0.2` rounding, `9^99` f64 display fidelity limits — inherent, informational only); all domain errors (sqrt(-1), log(0), asin(2)) rejected by the finite guard; math.rs regexes `^…$`-anchored linear; no unwraps on user data.
- Action dispatch: all 10 enum variants have exactly one exhaustive match arm in `execute`; every variant is constructed (no dead variants); `SetQuery`/`TrashPath`/`OpenWith`/`TogglePreview`/`Failed` outcome/window semantics verified consistent; `resolve_id`'s `app:`/`path:`-only coverage is provably safe (usage/typos persistence is gated to those prefixes at all ingestion sites); every UI affordance maps to a real variant.
- Packaging: all four scripts carry `set -euo pipefail` with quoted expansions and honored `XDG_*` vars; `dist/install.sh` uses `mktemp -d` + trap cleanup; autostart/desktop entries use real flags (`--daemon` verified in `main.rs:15`); the uninstaller removes exactly the four created paths with no over-rm.

### Pass 20 verification log

- Two parallel subagent sweeps: (a) expr/math increased-depth re-verification (with an empirical standalone stack-overflow reproduction — the strongest verification method used so far in this audit); (b) Action dispatch completeness + packaging pipeline.
- Every finding's cited `file:line` re-read and confirmed against current source by the main auditor; duplicates of prior-pass findings (reveal false-success, sed metacharacters) excluded from the count.
- `cargo clippy --all-targets` — clean; `cargo test` — 253 passed / 0 failed / 2 ignored; captured to scratch (`clippy_pass20.log`, `test_pass20.log`). cargo-deny/MSRV still unavailable (unchanged).
- No source changes were made in this pass.
- This pass produced **4 new findings** (1 P1, 3 P3); it is **not** a zero-finding clean pass.

### Termination condition status (Pass 20)

Termination condition still **not met** (Passes 13–20: 24, 21, 25, 21, 12, 15, 5, 4 new findings). Areas remaining unsaturated for Pass 21: `config.rs` migration/sanitize paths beyond Pass 13's load/save findings, `usage.rs`/`typos.rs` interaction with the engine boost paths at trace level, and a final cross-cutting grep sweep for patterns never systematically checked (`.clone()` in hot loops, `String` allocation churn in per-keystroke paths).

---

## 🧱 Pass 21 config migration depth, scoring-pipeline trace & allocation-churn audit (2026-08-26)

Areas targeted per the Pass 20 close-out. All line numbers verified against current source.

### New verified findings

#### P2 — `IndexConfig` has no `sanitize()`; `extra_roots` accepts unvalidated relative and `~otheruser` paths (`src/config.rs:806-813`, `src/ui/settings.rs:852-855`, `files/index.rs:251-255`, `720-731`)

- **Root cause:** `update` sanitizes only `ui` and `translate`; Settings pushes raw trimmed text into `extra_roots`. `expand_user` handles only `~/` and exact `~`, so `~otheruser/x` becomes a literal relative path; both it and bare relative paths fail `is_dir()` and are **silently ignored** — the Settings list shows them as active while the index never walks them. If the daemon cwd happens to contain a matching directory, an unintended tree is indexed. CWE-20/CWE-22.
- **Remediation:** add `IndexConfig::sanitize()` that absolutizes/canonicalizes `extra_roots` (rejecting non-`~/` tilde and relative paths), called from `load` and `update`; validate in the Settings add handler like `promote_deep_root` does.

#### P2 — `save()` race: shared tmp filename + lock dropped before save lets two writers corrupt `config.json` (`src/config.rs:806-818`, `822-845`, concurrent writer at `engine.rs:745-827`)

- **Root cause:** `update` drops the write lock before `save()`, which uses a **fixed** tmp path. `auto_promote_deep_root` runs on a worker thread and calls `config.update` concurrently with GTK Settings edits: two `fs::write`s interleave on the same tmp, then a rename can publish partial content. Next launch the parse fails → the whole config is backed up and reset to defaults — user loses all settings from a benign concurrent pin. CWE-362/CWE-367.
- **Remediation:** hold the write lock (or a save `Mutex`) across tmp-write→rename; unique tmp names; fsync before rename.

#### P3 — `max_depth` out-of-range repair resets to the default (2) instead of clamping to the boundary (`src/config.rs:736-739`)

- **Root cause:** `if cfg.index.max_depth > 6 { cfg.index.max_depth = default_depth(); }` — a hand-edited `7`/`8` silently becomes **2**, and the cache rebuild (which hashes `max_depth`) drops most of the user's tree. Also `max_depth: 0` bypasses this guard entirely (only `> 6` is repaired), persisted verbatim and relying on five independent consumer sites to re-clamp. CWE-20.
- **Remediation:** normalize to `1..=6` once in `load` (`min(6).max(1)`).

#### P3 — `save()` swallows every error; UI shows settings as applied while nothing was persisted (`src/config.rs:822-845`)

- **Root cause:** every fallible step (`create_dir_all`, `to_string_pretty`, `fs::write`, `fs::rename`, both chmods) discards its error with no log. Disk-full/read-only configs silently lose all Settings changes; also masks the race above. CWE-754.
- **Remediation:** return `Result`/log; surface a UI toast on failure (load failures are already logged this way).

#### P3 — per-keystroke translate-field writes persist cleared endpoint state mid-typing (`src/ui/settings.rs:2151-2206`, `config.rs:485-488`)

- **Root cause:** `connect_changed` fires per character; intermediate invalid endpoints are cleared by `TranslateConfig::sanitize`, so a daemon exit mid-typing persists an empty endpoint — translate silently falls back to free public backends. Every keystroke of the secret `api_key` also rewrites it to disk (amplifying the Pass 13 tmp-window exposure). CWE-20/CWE-524 aspect.
- **Remediation:** commit on `activate`/focus-leave or debounce; show validation feedback instead of silent clearing. (Compounds Pass 14's per-keystroke persistence finding with the sanitization interaction.)

#### P3 — exclude add is case-sensitive while matching is case-insensitive → permanent duplicates (`src/ui/settings.rs:951-954` vs `config.rs:1205-1208`)

- **Root cause:** byte-equality `contains` check against a lowercased-at-match-time set; `Node_Modules` alongside `node_modules` is accepted and persisted forever (the default-merge path is also case-sensitive). CWE-20.
- **Remediation:** case-insensitive duplicate check; lowercase-normalize in `IndexConfig::sanitize`.

#### P3 — any single field type error discards the entire config to defaults (`src/config.rs:698-714`)

- **Root cause:** one whole-tree `from_str::<HarkConfig>`; a single wrong-typed field (`max_depth: "3"`) resets **all** sections — index roots, mounts, open_with, UI — to defaults (mitigated by, but distinct from, the Pass 13 backup/unknown-key findings: field-level serde defaults never engage because the struct aborts). CWE-754/CWE-20.
- **Remediation:** per-section deserialization or field-level `Result` wrappers so a bad field degrades only that field.

#### P3 — redundant query lowercasing: up to 5× per keystroke (`engine.rs:206`, `apps.rs:230`, `search/mod.rs:122`, `typos.rs:338`, `live_cache.rs:102` via `files/mod.rs:274`)

- **Root cause:** each layer independently normalizes because `Engine::search` passes raw `&str` down; five `to_lowercase()` allocations per keystroke for the same short string. P3 perf.
- **Remediation:** compute `ql` once and thread `&str`/`Cow` into the providers, typos lookup, and cache key.

#### P3 — mounts `Vec` deep-cloned on every search (`src/providers/files/mod.rs:190-195`)

- **Root cause:** `self.state.mounts.read()...clone()` copies every `MountInfo` (2 heap allocations each) per keystroke though mounts change only on mount events. The clearest reusable-allocation miss in the hot path. P3 perf.
- **Remediation:** store `Arc<[MountInfo]>` and clone the Arc.

#### P3 — full first-result struct cloned per keystroke for the preview (`src/ui/mod.rs:2510-2518`)

- **Root cause:** `results.borrow().first().cloned()` clones an entire `SearchResult` (title/subtitle/id/action/`PathBuf`) per refresh just to pass `&SearchResult` into `preview.update`, which does not retain it. P3 perf.
- **Remediation:** scoped borrow passing `item.first()` directly.

#### P3 — 25-row results Vec fully cloned per deep merge (`src/ui/mod.rs:2833-2839`)

- **Root cause:** `let mut merged = results.borrow().clone();` per async deep batch (not per keystroke). P3 perf.
- **Remediation:** clone ids only into the dedup set; `push` into the original via `mem::take`.

### Clean areas verified in this pass

- Scoring pipeline trace (Part A of the boost audit): **SATURATED** — every score modification site enumerated and arithmetic verified exact; no double-application of usage/alias boosts on any path; injected rows cannot exceed 22k+18k+usage, and outranking an exact match only at ≥6 fresh launches is intended personalization; `apply_typo_alias` gating has no bypass (normalize_alias filters even ungated cases); the Pass 14 non-saturating adds at `:283`/`:336`/`:347` are the only such sites and are overflow-benign short of count ≈9.2×10¹⁵.
- Config: v1→v2 migration idempotent with correct version write-back; migration before sanitize before the single conditional save; fresh-vs-migrated defaults converge; `UiThemeConfig`/`TranslateConfig` clamp math correct; `update()` in-memory swap loses no concurrent mutation; deep-root promote validates and canonicalizes; `ExcludeSet` matching verified including the regression test; no panics/integer defects in non-test config code.
- Allocation: row-bind path borrows; engine result containers pre-sized; icon cache thread-local and reused; live-cache `put` single-move; apps `to_result` clones bounded by top-K.

### Pass 21 verification log

- Two parallel subagent sweeps: (a) config migration/sanitization depth; (b) full scoring-pipeline trace + allocation-churn grep. All citations re-read and confirmed by the main auditor.
- `cargo clippy --all-targets` — clean; `cargo test` — 253 passed / 0 failed / 2 ignored; captured to scratch (`clippy_pass21.log`, `test_pass21.log`). cargo-deny/MSRV still unavailable (unchanged).
- No source changes were made in this pass.
- This pass produced **11 new findings** (2 P2, 9 P3); it is **not** a zero-finding clean pass. The scoring-pipeline angle is now saturated.

### Termination condition status (Pass 21)

Termination condition still **not met** (Passes 13–21: 24, 21, 25, 21, 12, 15, 5, 4, 11 new findings — the increase comes from the previously-unswept config-migration angle, while engine scoring saturated). Remaining for Pass 22: `providers/calc/` remaining unswept corners (datetime.rs parse surfaces at final depth, currency.rs FX display paths), `theme/css.rs` + `theme/mod.rs` beyond Pass 15's findings, and `lib.rs`/`main.rs` wiring final check — the last never-directly-swept surfaces.

---

## 📋 Passes 13–21 consolidated index (by category, sorted by severity)

138 findings from Passes 13–21 reorganized into categories. Sev: P1 (crash/abort), P2 (logic error, leak, security, silent data loss), P3 (minor). Multi-category entries appear once under their dominant category. Search by the Pass N + heading text to find the full entry with root cause, pathway, and remediation.

### 🔴 Crash / Panic (P1 ×3) — **ALL FIXED 2026-08-26** (fixes + regression tests; each test verified to fail with its fix reverted)

| Sev | Finding | Location | Pass |
|---|---|---|---|
| P1 | `to_lowercase()` byte-offset mismatch panics slicing the original query (İ/ẞ before scope keyword) — **fixed**: `to_ascii_lowercase()` (length-preserving) in both parsers; test `scoped_query_multibyte_no_panic` | `files/search/plan.rs:101-114`, `257-286` | 16 |
| P1 | Composite chain: exclude-`"/"` config + missing-path scoped query → walk `/` → `windows(0)` panic → `panic=abort` kills daemon — **fixed**: `ExcludeSet::from_list` skips empty/blank/empty-after-split patterns; test `slash_only_pattern_does_not_panic` | `config.rs:1185-1227` + `deep.rs:285-290,646` | 18 |
| P1 | Stack overflow abort via ~15k nested parentheses in expression parser (empirically reproduced; paste-only) — **fixed**: `MAX_DEPTH = 200` threaded through parse_* and print_* families (incl. unary sign chains); test `deep_nesting_bounded_not_stack_overflow` | `calc/expr.rs:238-241` | 20 |

### 🔒 Security / Trust Boundaries

| Sev | Finding | Location | Pass |
|---|---|---|---|
| P2 | Translate disk cache unauthenticated + world-writable `/tmp` fallback → attacker-controlled clipboard | `translate.rs:928-960` | 16 |
| P2 | `path_completions` bypasses secrets/artifact exclude filter (`.ssh` listing) — **fixed 2026-08-26**: completions now receive `ExcludeSet` and call `should_skip_entry`; test `path_completions_skip_secret_dirs` | `files/search/glob.rs:541-569` | 19 |
| P2 | Serialized IPC accept loop: slowloris clients wedge all hotkey presses | `ipc.rs:96-113` | 14 |
| P2 | bind→chmod race leaves socket briefly world-connectable — **fixed 2026-08-26**: `bind_socket` temporarily forces umask 077 during bind, then checks chmod 0600; ignored test `bound_socket_is_user_only` | `ipc.rs:121`, `:90-94` | 14 |
| P2 | Unbounded toggle channel: flood grows memory + overlay churn | `main.rs:92-103` | 14 |
| P2 | IPC flood → duplicate GTK Application spawn chain | `ipc.rs:96-113` + `main.rs:44-48` | 18 |
| P2 | Stale drag-end timer fires mid-drag, hides launcher, cancels Wayland drop | `ui/dnd.rs:178-225` | 13 |
| P2 | Corrupt-but-parseable usage count → release daemon abort (panic=abort) | `usage.rs:16-18` + `engine.rs:283,333` | 14/18 |
| P3 | ureq honors proxy env vars unscoped | `http.rs:16-21` | 16 |
| P3 | MyMemory in-band errors rendered as translations and cached 14 days | `translate.rs:840-855` | 16 |
| P3 | fx accepts any base/date (tampered cache → wrong conversions) | `fx.rs:265-281` | 16 |
| P3 | api_key sent plaintext to user-allowed http:// endpoints | `translate.rs:718-728` | 16 |
| P3 | IPC fallback socket path: silent mkdir/chmod failure, symlink-able `/tmp/hark` | `ipc.rs:14-19,68-77` | 14 |
| P3 | Store tmp files briefly world-readable before chmod (CWE-732) | `config.rs:826-836` | 13 |
| P3 | Decimal/hex-IP SSRF literals pass translate-endpoint blocklist | `config.rs:585-600` | 13 |
| P3 | Online installer: no tarball integrity check despite published SHA256SUMS | `dist/install.sh:31-34` | 20 |
| P3 | PKGBUILD hard-links layer-shell but lists it only as optdepends | `packaging/aur/PKGBUILD:12-33` | 15 |
| P3 | ffmpeg/pdftoppm resolved via inherited PATH with full env | `ui/preview.rs:1384-1442` | 16 |
| P3 | `set_manual` accepts structurally invalid target ids (zombie aliases) | `typos.rs:279-282` | 18 |

### 💾 Data Integrity / Persistence

| Sev | Finding | Location | Pass |
|---|---|---|---|
| P2 | Malformed store JSON silently wiped, no backup, then overwritten | `typos.rs:76-81`, `usage.rs:44-49` | 13 |
| P2 | `save()` race: shared tmp + lock dropped early → config.json corruption → all settings lost | `config.rs:806-845` | 21 |
| P2 | Non-fsync store writes: crash leaves truncated file (feeds the wipe) | `typos.rs:215-219`, `usage.rs:175-179` | 13 |
| P2 | Store JSON save races: same-shaped bug in typos/usage stores | `typos.rs`, `usage.rs` save paths | 21* |
| P2 | Corrupt-but-parseable usage count → daemon abort (data-poisoning leg) | see Security table | 18 |
| P3 | Unknown config keys silently dropped on next save | `config.rs:645-657` | 13 |
| P3 | `save()` swallows all errors; settings appear saved but aren't | `config.rs:822-845` | 21 |
| P3 | `max_depth > 6` reset to default 2 instead of clamping; `0` bypasses | `config.rs:736-739` | 21 |
| P3 | Single field type error resets the entire config to defaults | `config.rs:698-714` | 21 |
| P3 | Per-keystroke translate writes persist cleared endpoint mid-typing | `settings.rs:2151-2206` | 21 |
| P3 | Rename-failed index tmp files accumulate; no cleanup | `files/index.rs:810-819` | 15 |
| P3 | Converter scratch files leak on crash; no GC sweep | `ui/preview.rs:1284-1293` | 16 |
| P3 | `IndexConfig` unsanitized; `extra_roots` accepts relative/`~otheruser` (silently ignored) | `config.rs`, `settings.rs:852-855` | 21 |

### ⚡ Performance

| Sev | Finding | Location | Pass |
|---|---|---|---|
| P2 | Code preview of ≤2 MiB single-line file freezes main loop (minified assets) | `ui/preview.rs:775-779` | 16 |
| P2 | Nonexistent absolute scope walks `/` for full deep budget per keystroke | `files/search/deep.rs:285-290` | 16 |
| P3 | `icon_file_path` stats FS per row per keystroke (NFS freeze) | `ui/rows.rs:606-610` | 17 |
| P3 | `highlight_markup` O(n²) in title length | `ui/rows.rs:118-119` | 17 |
| P3 | Mounts Vec deep-cloned every search | `files/mod.rs:190-195` | 21 |
| P3 | Query lowercased up to 5× per keystroke | `engine.rs:206` + 4 sites | 21 |
| P3 | First-result struct cloned per keystroke for preview | `ui/mod.rs:2510-2518` | 21 |
| P3 | 25-row Vec fully cloned per deep merge | `ui/mod.rs:2833-2839` | 21 |
| P3 | Wheel-navigate rebinds all 25 rows (amplifies stat-per-row) | `ui/rows.rs:211-220` | 17 |
| P3 | Per-result usage lock acquisition per keystroke; top(20) clones | `engine.rs:281-284` | 14 |
| P3 | `discover_mounts()` re-executed per deep_roots entry at load | `config.rs:738-742` | 13 |
| P3 | Settings text entries persist config per keystroke (30 writes per URL) | `settings.rs:1836-2210` | 14 |
| P3 | Theme monitor schedules one 80 ms apply() per event (no debounce) | `theme/mod.rs:197-203` | 15 |
| P3 | `accept` error tight-loops at 100% CPU under fd exhaustion | `ipc.rs:99-100` | 14 |
| P3 | Bench timing confounded by concurrent index warm-up | `bench.rs:100-134` | 15 |
| P3 | Thumbnail size-slot store/read mismatch (needless regeneration) | `ui/thumbnails.rs:19-121` | 13 |
| P3 | Icon resolve cache never invalidated on icon-theme change | `ui/rows.rs:76-78` | 17 |
| P3 | 5-min live-cache TTL hides newly created files (negative cache 90 s) | `live_cache.rs:17-18` | 14 |
| P3 | Unreadable meta file forces full rebuild every cycle | `files/index.rs:757-777` | 15 |
| P3 | Memory duplication: `path_lower`+`name_lower` full copies per index entry | `files/index.rs:364-382` | 15 |

### 🐛 Logic / Correctness

| Sev | Finding | Location | Pass |
|---|---|---|---|
| P2 | Right-click on conversion set opens action panel for a different item | `ui/mod.rs:688-699` | 15 |
| P2 | Tab on conversion set destroys query with answer title | `ui/mod.rs:1437-1502` | 15 |
| P2 | Deep-scoped literal names match by substring (`main.rs.bak` for `main.rs`) | `glob.rs:161-166` | 16 |
| P2 | Source/exclusion toggles never reindex; stale results up to 30 min | `settings.rs:543-955` | 14 |
| P2 | "Reset appearance" desyncs symbolic-icons checkbox | `settings.rs:2057-2067` | 14 |
| P2 | `index_is_strong` gate below contains-band cancels remaining deep jobs | `search/mod.rs:64-65` | 16 |
| P2 | Hot-set short-circuit can drop strictly better index results | `rank.rs:110-125` | 14 |
| P2 | Reveal failure invisible; `Launched` returned unconditionally | `files/mod.rs:506-519` | 17 |
| P2 | Trash failure string built but never shown to user | `engine.rs:421-424` | 17 |
| P2 | Empty exclude pattern → `windows(0)` panic (crash leg; see P1 chain) | `config.rs:1185-1227` | 13 |
| P3 | `truncate(25)` merge can evict selected row → hero reset | `ui/mod.rs:2844-2876` | 15 |
| P3 | Trash flow leaves `selected` on shifted-in neighbor | `ui/mod.rs:1907-1929` | 15 |
| P3 | Settings close resets selection to row 0 | `ui/mod.rs:540-555` | 15 |
| P3 | Drive-prefix swallows words (`e:mail`) into empty completion | `glob.rs:347-353` | 16 |
| P3 | `split_glob_path` drops mid-path wildcard components | `glob.rs:329-344` | 19 |
| P3 | Deep-walk tie-breaks arrival-order dependent | `deep.rs:742-752` | 16 |
| P3 | Hot-set eviction nondeterministic on frecency ties | `usage.rs:124-142` | 17 |
| P3 | `scoped_memo` stale across index rebuilds | `files/mod.rs:272-299` | 17 |
| P3 | Trashed paths stay in index/hot set until TTL | `engine.rs:415-425` | 17 |
| P3 | xdg-open spawn≠open; failure invisible | `files/mod.rs:429-434` | 17 |
| P3 | Clock rollback freezes decay/recency at maximum | `typos.rs:468-472` | 13 |
| P3 | Mount skip substring `contains("EFI")` prunes `/mnt/KEFIR` | `config.rs:1042-1048` | 13 |
| P3 | `%%`/reserved field codes not decoded/stripped per spec | `apps.rs:489-493` | 15 |
| P3 | `OnlyShowIn`/`NotShowIn`/`TryExec` ignored | `apps.rs:383-399` | 15 |
| P3 | Duplicate-key precedence: strings first-wins, booleans last-wins | `apps.rs:384-398` | 15 |
| P3 | NoDisplay apps unfindable even by exact id | `apps.rs:138` | 15 |
| P3 | Panic during index build leaves `indexing=true` | `files/index.rs:168-181` | 15 |
| P3 | MAX_INDEX eviction home-biased walk-order truncation | `files/index.rs:237-311` | 15 |
| P3 | Depth fallback fabricates absolute-path depth | `files/index.rs:354-357` | 15 |
| P3 | Failed clipboard copy silent; Ctrl+Enter closes anyway | `ui/mod.rs:975-989` | 19 |
| P3 | learn_typos gating diverges primary vs secondary panel | `ui/mod.rs:2052-2063` | 19 |
| P3 | Mixed-coordinate monitor margin (logical vs physical px) | `ui/mod.rs:3011-3017` | 19 |
| P3 | `args.retain` removes every arg equal to the query | `main.rs:26-28` | 14 |
| P3 | `request_toggle` reports success without ack | `ipc.rs:44-49` | 14 |
| P3 | Single IPC read drops split-stream messages | `ipc.rs:103-105` | 14 |
| P3 | EXIF orientation never applied (preview + shared thumb cache) | `ui/preview.rs:1484-1489` | 16 |
| P3 | Alias + usage boosts stack above exact-match score | `engine.rs:283,557-565` | 18 |
| P3 | Wheel flip primes outgoing panel mid-transition | `ui/rows.rs:449-477` | 17 |
| P3 | Trash pre-check TOCTOU degrades error message | `files/mod.rs:583-589` | 17 |

### 🧮 Mathematical / Numeric

| Sev | Finding | Location | Pass |
|---|---|---|---|
| P2 | NaN/inf fuel-economy values pass the `<= 0.0` guard | `calc/fueleco.rs:59-67` | 13 |
| P2 | DST-gap/ambiguous times silently produce no card | `calc/timezone.rs:510-516` | 18 |
| P3 | Compound-interest inf/NaN passthrough (financial) | `calc/financial.rs:34-55` | 7-family |
| P3 | Non-finite fraction literals accepted (`NaN/1`) | `calc/util.rs:5-15` | 7-family |
| P3 | Magnitude-suffixed amounts rejected in units/currency (`10k kg`) | `calc/units.rs`, `currency.rs` | 7-family |
| P3 | Duration scale accepts negative multipliers (`1h * -2` → `-3h`) | `calc/duration.rs:53-71` | 13 |
| P3 | Unbounded duration digits → inf → saturating cast absurd output | `calc/duration.rs:216-220` | 13 |
| P3 | Cooking ingredient substring match assigns wrong density | `calc/cooking.rs:74-79` | 13 |
| P3 | Oven conversion accepts below-absolute-zero temps | `calc/cooking.rs:378-391` | 13 |
| P3 | Battery capacity u8 accepts >100 firmware values | `calc/battery.rs:173-174` | 13 |
| P3 | Predict-tz short-alias misrouting (`s`→SF, `b`→BST); no diacritic folding | `calc/timezone.rs` | 18 |
| P3 | Hex-words parse as numbers; non-canonical roman; reversed random bounds | `calc/quick.rs` | 18 |
| P3 | `format_number` collapses sub-5e-9 values to `0` | `calc/quick.rs` | 18 |

### 🧠 Learning / Ranking Behavior

| Sev | Finding | Location | Pass |
|---|---|---|---|
| P2 | Manual pins clobbered by one conflicting launch (no manual flag) | `typos.rs:289-296,168-171` | 18 |
| P2 | Alias boost never decays; 21-day decay dead code for ranking | `typos.rs:96-107` | 18 |
| P2 | Self-reinforcement loop: alias-driven launches feed the alias | `typos.rs:110-152` | 18 |
| P2 | Conflict-switch contradicts comment; strong alias retargeted at count==2 | `typos.rs:164-176` | 13 |
| P2 | urandom-fallback passwords/uuids silently predictable (CWE-338) | `calc/quick.rs:652-678` | 18 |
| P3 | Full-title alias branch doubles edit budget (4-char vs 5-char titles) | `typos.rs:399-411` | 18 |
| P3 | Session sweep learns abandoned dead-end tokens | `typos.rs:134-146` | 18 |
| P3 | Prune can evict aged manual pins; tie eviction process-random | `typos.rs:440-452` | 18 |
| P3 | Dead `path:` alias targets stat FS per keystroke forever | `engine.rs:361-364` | 18 |
| P3 | First-use usage entry evicted by its own record | `usage.rs:87-94` | 13 |

### 🖼 UI / UX Polish

| Sev | Finding | Location | Pass |
|---|---|---|---|
| P2 | Untracked preview.clear timer blanks freshly populated preview | `ui/mod.rs:1388-1391` | 15 |
| P3 | Stacked 220 ms swap-class timers truncate newer animation | `ui/rows.rs:479-487` | 13 |
| P3 | `resolve_icon_name` empty-slice index (latent) | `ui/rows.rs:727-729` | 13 |
| P3 | Control chars in filenames expand Open With popover | `ui/open_with.rs:60-67` | 14 |
| P3 | Open With app list unsorted (scan-order) | `ui/open_with.rs:342-352` | 14 |
| P3 | Settings j/k navigation dead-keys on filtered rows | `settings.rs:313-328` | 14 |
| P3 | Row-removal closures form strong ref cycles (widget leak) | `settings.rs:788-818,1577-1627` | 14 |
| P3 | `clear()` leaves prior file text in code buffer (retained memory) | `ui/preview.rs:396-406` | 16 |
| P3 | Drag-thumbnail memo pins one texture forever | `ui/dnd.rs:324-338` | 13 |
| P3 | CSS hex shorthand (`#fff`/`#RRGGBBAA`) silently falls back | `theme/mod.rs:226-232` | 12-family |
| P3 | Literal glob candidates score below wildcard matches | `glob.rs:100-117` | 12-family |

### 🛠 Robustness / Error Handling

| Sev | Finding | Location | Pass |
|---|---|---|---|
| P3 | `Instant::now() - SAVE_DEBOUNCE` startup panic (<2 s monotonic clock, theoretical) | `typos.rs:70,88`, `usage.rs:67,76` | 13 |
| P3 | Panic in index build leaves indexing=true (also listed under Logic) | `files/index.rs:168-181` | 15 |
| P3 | Non-UTF-8 code files report "Could not load file" | `ui/preview.rs:755-766` | 16 |
| P3 | Stat-vs-read TOCTOU bypasses 2 MiB code gate | `ui/preview.rs:724,755` | 16 |
| P3 | `bench.rs` `daemon_stats()` aborts on one unparseable line | `bench.rs:400-406` | 15 |
| P3 | install-user.sh: sed replacement metachar injection + ETXTBSY abort | `install-user.sh:32-44` | 15 |
| P3 | Release-pipeline rename skew breaks online installer asset lookup | `package-release.sh:27-30` | 20 |
| P3 | Dev install script restarts wrong process name (hark vs blink) | `scripts/install.sh:46-66` | 20 |
| P3 | PATH hint ignores XDG_BIN_HOME | `install-user.sh:95-98` | 20 |
| P3 | No HTTP retry/backoff; single-shot requests | `http.rs` | 16 |

### 📄 Documentation

| Sev | Finding | Location | Pass |
|---|---|---|---|
| P3 | FEATURES.md "2s poll fallback" does not exist | `FEATURES.md:205` | 15 |
| P3 | FEATURES.md Open With listed as both not-shipped and shipped | `FEATURES.md:285,292` | 15 |
| P3 | PageUp/PageDown unbound in overlay (completeness gap) | `ui/mod.rs:1229` | 15 |

\* The Pass 21 `save()`-race entry covers the config store; the typos/usage stores share the fixed-tmp-name pattern without the same concurrent-writer exposure (single-threaded debounced saves), noted for completeness.

**Category totals:** Crash 3 (all P1) · Security 19 (8 P2, 11 P3) · Data integrity 12 (4 P2, 8 P3) · Performance 21 (2 P2, 19 P3) · Logic 40 (10 P2, 30 P3) · Math 15 (2 P2, 13 P3) · Learning 10 (5 P2, 5 P3) · UI/UX 11 (1 P2, 10 P3) · Robustness 10 (all P3) · Docs 3 (all P3).

---

## 📋 Supplement: pre-existing findings (original audit + Passes 3–12) in the same category/severity form

The table above covers only Passes 13–21. Below are the 67 tracker items from the original audit and verification pass (the Tracker section above), reorganized by category and sorted by severity, with fix status. Passes 4–12's per-pass findings are not duplicated here — the still-open ones among them already appear in the Passes 13–21 index where later passes re-verified them (marked "12-family"/"7-family"), and the remainder live in their own sections.

### 🔴 Crash (original P0/P1 crash class)

| Sev | ID | Finding | Location | Status |
|---|---|---|---|---|
| P0 | #1 | UTF-8 slice panic on Unicode whitespace between lang codes | `translate.rs:241` | fixed |
| P0 | #2 | UTF-8 slice panic: glob retry `abs+1` on continuation byte | `glob.rs:156` | fixed |
| P1 | #3 | Capture-phase key controllers swallow text input (both interceptors) | `settings.rs:296`, `ui/mod.rs:837` | fixed |
| P1 | #12 | Byte-slicing scheme.json hex panics instead of fallback | `theme/css.rs:1-24` | fixed |
| P1 | #11 | Char-vs-byte gate: Action Panel shortcut dead on non-ASCII | `ui/mod.rs:1044` | fixed |

### 🔒 Security (original)

| Sev | ID | Finding | Location | Status |
|---|---|---|---|---|
| P0 | N1 | `/tmp/hark` fallback follows symlinks → arbitrary-file overwrite | `fx.rs:271-291` | fixed |
| P1 | #8 | Passwords/UUIDs from predictable xorshift (nanos^pid) | `quick.rs:639` | fixed |
| P1 | #19 | Predictable `/tmp/hark-preview` paths; dir owner feeds decoder | `preview.rs:1280` | fixed |
| P3 | #21 | `.ok()?` makes in-function thumb fallback unreachable (dead code) | `preview.rs:1246` | fixed |
| P3 | apps-bs | Unquoted `\` not escaped per Desktop Entry spec | `apps.rs:426` | fixed |

### 🧮 Mathematical (original)

| Sev | ID | Finding | Location | Status |
|---|---|---|---|---|
| P1 | #4 | Local offset truncated to whole hours (IST off 30 m) | `timezone.rs:672` | fixed |
| P1 | #5+N2 | Negative UTC offsets never resolve; bogus tokens → silent UTC | `timezone.rs:424,284-310` | fixed |
| P1 | #9+N6 | Random-range i64 overflow; spans ≥2⁵³ collapse | `quick.rs:669-670` | fixed |
| P1 | #10 | Unary minus binds tighter than `^` (`-2^2 = 4`) | `expr.rs:161` | fixed |
| P1 | N3 | Case-inverse bits/bytes classification → ETA 8× off | `quick.rs:502-507` | fixed |
| P1 | NC-dt | Feb-29 anchor kills ymd_between walk ("48 months" for 4 years) | `datetime.rs:112` | fixed |
| P1 | #22 | Zero/negative FX rates pass validation → silent 0.00 / sign flip | `fx.rs:150` | fixed |
| P3 | m-u64 | >u64 hex/binary silently yields nothing | `math.rs:127`, `quick.rs:45` | fixed |
| P3 | q-hexa | `hexa` arm unreachable via regexes (dead code) | `quick.rs:20,51` | fixed |

### 🐛 Logic / Correctness (original)

| Sev | ID | Finding | Location | Status |
|---|---|---|---|---|
| P1 | #6 | `clean_exec` re-join destroys argv quoting at launch | `apps.rs:435` | fixed |
| P1 | N4 | Glob `?` matches one byte not one char | `glob.rs:172-196` | fixed |
| P1 | N5 | Absolute-glob live branch skips excludes — excluded dirs leak | `glob.rs:217-251` | fixed |
| P1 | #13 | Sibling-prefix dirs leak into scoped globs | `glob.rs:265` | fixed |
| P1 | N7 | Unrecognized IPC ack retried → toggle ×5 | `ipc.rs:44-54` | fixed |
| P1 | #28+N8 | No read timeout: silent client parks listener forever | `ipc.rs:96-110` | fixed |
| P2 | #27 | Duplicate extra-folder rows in config | `settings.rs:844` | fixed |
| P3 | N10 | Field-code filter drops any `%token`; `%%` mishandled | `apps.rs:438,507` | fixed |
| P3 | N11 | Unterminated quote merges command tail silently | `apps.rs:402-432` | fixed |
| P3 | rank-hot | Hot short-circuit flips highlight style; fuzzy candidates vanish | `rank.rs:122` | fixed |
| P3 | rank-budget | Fuzzy budget burned by failed scorings | `rank.rs:218` | fixed |
| P4 | eng-q / dt-hr / um-b / set-depth / fm-trunc / cfg-hash | Dead code & minor hygiene batch | various | fixed |

### ⚡ Performance (original)

| Sev | ID | Finding | Location | Status |
|---|---|---|---|---|
| P1 | #17+N13 | Stale thumbs never invalidated; drag icon same root | `thumbnails.rs:12`, `dnd.rs:335` | fixed |
| P1 | #20 | No timeout on ffmpeg/pdftoppm — one hang kills all previews | `preview.rs:1319` | fixed |
| P2 | #23 | Config written per keystroke on main thread | `settings.rs:1826,2141` | fixed |
| P2 | N14 | Preset click double-writes config + double CSS inject | `settings.rs:1866-1875` | fixed |
| P2 | #24 | Hot Vec cloned under nested locks every keystroke | `hot.rs:65` | fixed |
| P2 | #25 | Sync fs::metadata on main thread stalls UI on NFS/FUSE | `preview.rs:427` | fixed |
| P2 | #26 | Album art stretched square via scale_simple | `preview.rs:1187` | fixed |
| P2 | #29 | Theme debounce timer per event, no coalescing | `theme/mod.rs:184-206` | fixed |
| P2 | N9+apps-z | Unwaited spawns → zombies (files + apps) | `files/mod.rs`, `apps.rs:481` | fixed |
| P3 | th-order | Thumbnail probe order makes x-large unreachable | `thumbnails.rs:15` | fixed |
| P3 | ow-sync | Sync app enumeration janks popover on cold DB | `open_with.rs:36` | fixed |
| P4 | usage-race | Dirty-flag race record↔save (one delayed write) | `usage.rs`, `typos.rs` | fixed |

### 🖼 UI/UX (original)

| Sev | ID | Finding | Location | Status |
|---|---|---|---|---|
| P1 | #16 | Thumbnail URI not percent-encoded → cross-app cache misses | `thumbnails.rs:33` | fixed |
| P1 | #18 | GObject cycles leak Open With popover tree | `open_with.rs:283` | fixed |
| P2 | #15 | Force-prefix fallback case-hygiene mismatch | `files/mod.rs:293` | fixed |
| P3 | N15 | Raw scheme colour into Pango markup | `theme/mod.rs:120` | fixed |
| P4 | set-sym / ow-spawn / pv-lang / pv-meta | Reset-desync checkbox, discarded xdg-open errors, stale language, dead param | various | fixed |

### 💾 Data Integrity (original)

| Sev | ID | Finding | Location | Status |
|---|---|---|---|---|
| P1 | #14 | Meta stamped despite failed write → freshness lie per start | `files/index.rs:808` | fixed |
| P3 | N16 | Fixed temp filenames race → torn file renamed into place | `thumbnails.rs:110`, `usage.rs:176` | fixed |

### Latent hazards (no current trigger — fix opportunistically)

| ID | Finding | Location |
|---|---|---|
| pv-refcell | Borrow held across visibility callback — panics if callback gains a body | `preview.rs:370` | — fixed
| th-bytes | Pixbuf::from_bytes over-reads on inconsistent rowstride/pixels | `thumbnails.rs:77` | — fixed
| th-canon | canonicalize before hashing diverges symlinked cache keys | `thumbnails.rs:26` | — fixed
| ow-sync | (also listed under Performance) | `open_with.rs:36` | — fixed
| rng-reseed | Same-millisecond correlated xorshift streams | `quick.rs:635-646` — fixed |

**Pre-existing totals:** 67 items — 4 P0 · 25 P1 · 10 P2 · 7 P3 · 16 P4 · 5 latent. 34 fixed (re-confirmed live by the Pass 17 fixed-status sweep), 33 open (the still-relevant open ones are cross-referenced above where Passes 12–15 re-verified them). Combined with Passes 13–21, the full audit catalog stands at **~205 distinct findings**.
