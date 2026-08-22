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
| #21 | P2 | `ui/preview.rs:1246` | `.ok()?` makes in-function thumb fallback unreachable (dead code) | open |
| #22 | P1 | `providers/fx.rs:150` | Zero to_rate → silent 0.00; negatives pass network AND disk validation | fixed |
| N3 | P1 | `calc/quick.rs:502-507` | Inverse case bug: `Bytes`/`MBPS`/`MB/S` classified bits → ETA 8× too slow | fixed |
| N4 | P1 | `files/search/glob.rs:172-196` | Glob `?` matches one byte not one char — false negatives on multibyte names | fixed |
| N5 | P1 | `files/search/glob.rs:217-251` | Absolute-glob live branch skips should_skip_entry — excluded dirs leak | fixed |
| NC-dt | P1 | `calc/datetime.rs:112` | ymd_between Feb-29 anchor → chrono None kills walk; "48 months" for 4 years | fixed |
| N7 | P1 | `ipc.rs:44-54` | Unrecognized ack retries toggle up to 5× | fixed |
| N8 | P1 | `ipc.rs:96-110` | Inline handler wedges listener thread (pairs with #28) | fixed |
| #28 | P1 | `ipc.rs:100` | No read timeout on accepted stream — silent client parks listener forever | fixed |
| N13 | P2 | `ui/dnd.rs:335` | Drag icon loads freedesktop thumb without mtime check (same root as #17) | fixed |
| #23 | P2 | `ui/settings.rs:1826,2141` | Config written per keystroke on main thread (+ theme reload) | open |
| N14 | P2 | `ui/settings.rs:1866-1875` | Preset click double-writes config + double CSS inject (set_text cascade) | open |
| #24 | P2 | `files/hot.rs:65` | Vec cloned under read lock nested inside index lock every keystroke | open |
| #25 | P2 | `ui/preview.rs:427` | Sync fs::metadata on main thread stalls UI on NFS/FUSE | open |
| #26 | P2 | `ui/preview.rs:1187` | Album art stretched square via scale_simple | open |
| #29 | P2 | `theme/mod.rs:184-206` | Debounce timer spawned per event, no coalescing | open |
| #27 | P3 | `ui/settings.rs:844` | Duplicate extra-folder rows in config (index dedup prevents double work) | open |
| N9 | P2 | `files/mod.rs:435,634,716,725` | Unwaited spawns → zombies (same class as apps.rs:481) | open |
| N10 | P3 | `providers/apps.rs:438,507` | Field-code filter drops any `%token`; `%%` literal mishandled | fixed |
| N11 | P3 | `providers/apps.rs:402-432` | Unterminated quote merges command tail silently | fixed |
| apps-z | P3 | `providers/apps.rs:481` | Spawned children never waited → zombies | open |
| apps-bs | P3 | `providers/apps.rs:426` | Unquoted `\` not escaped per Desktop Entry spec | open |
| N15 | P3 | `theme/mod.rs:120` | Raw scheme colour into Pango markup — route through shared sanitizer w/ #12 | fixed |
| N16 | P3 | `thumbnails.rs:110`, `usage.rs:176` | Fixed temp filenames race → torn file renamed into place | open |
| eng-q | P4 | `engine.rs:530` | Dead `let _ = q;` + unused lowercase var | open |
| dt-hr | P4 | `calc/datetime.rs:555` | Dead keep-alive line | open |
| um-b | P4 | `calc/unitmath.rs:179` | Dead `_b` parser field | open |
| set-depth | P4 | `ui/settings.rs:502,520` | Depth ± force_reindex even when clamped | open |
| set-sym | P4 | `ui/settings.rs:2050` | Restore-defaults leaves symbolic-icons checkbox stale | open |
| ow-spawn | P4 | `ui/open_with.rs:103` | xdg-open spawn error discarded; window hidden regardless | open |
| th-order | P4 | `ui/thumbnails.rs:15` | Probe order large→normal→x-large — x-large unreachable | open |
| pv-lang | P4 | `ui/preview.rs:775` | Stale GtkSourceView language kept when guess_language → None | open |
| pv-meta | P4 | `ui/preview.rs:956` | Dead `meta` parameter | open |
| fm-trunc | P4 | `files/mod.rs:324` | Hardcoded truncate(25) instead of FILE_RESULT_LIMIT | open |
| rank-hot | P4 | `files/search/rank.rs:122` | Hot short-circuit flips highlight style; fuzzy-only candidates vanish | open |
| rank-budget | P4 | `files/search/rank.rs:218` | Fuzzy budget burned by failed scorings (prefilter skips don't burn) | open |
| q-hexa | P4 | `calc/quick.rs:20,51` | `hexa` arm unreachable via regexes | open |
| m-u64 | P4 | `calc/math.rs:127`, `quick.rs:45` | >u64 hex/binary silently yields nothing | open |
| cfg-hash | P4 | `config.rs:1210` | ExcludeSet::matches double hash lookup | open |
| usage-race | P4 | `usage.rs:169-178`, `typos.rs:206-220` | Dirty-flag race record↔save (one delayed write worst case) | open |

### Latent hazards (fix opportunistically, no current trigger)

| ID | Location | Hazard | Status |
|---|---|---|---|
| pv-refcell | `ui/preview.rs:370` | Borrow held across visibility callback — panics if callback gains a body touching same RefCell | open |
| th-bytes | `ui/thumbnails.rs:77` | Pixbuf::from_bytes over-reads if caller-supplied rowstride/pixels ever inconsistent | open |
| th-canon | `ui/thumbnails.rs:26` | canonicalize before hashing diverges symlinked cache keys from other apps | open |
| ow-sync | `ui/open_with.rs:36` | Sync app enumeration janks popover open on cold app DB | open |
| rng-reseed | `calc/quick.rs:635-646` | Same-millisecond thread starts get correlated xorshift streams (micro) | fixed |

**Counts:** P0 ×4 · P1 ×25 · P2 ×10 · P3 ×7 · P4 ×16 · latent ×5 = **67 items**. Fixed so far: 34 items (4×P0 · 18×P1 + 5 bonus · empty-state layout) + install speedup.
