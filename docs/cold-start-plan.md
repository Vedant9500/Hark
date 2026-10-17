# Hark — cold-start optimization plan

**Status:** M3 lab-measured (W3, 2026-10-17) — Hyprland spot-check pending; next: M4 (W5, optional) · **Created:** 2026-10-17
**Source of numbers:** verified cold-start analysis (5 passes, each independently re-measured; see §8).
**Related:** [performance.md](./performance.md) (§ Cold start) · [OPTIMIZATION.md](./OPTIMIZATION.md) · [bench/](./bench/)

> Line numbers were verified on 2026-10-17. Re-check before editing; the seam map in Appendix A is the index.

---

## 0. TL;DR

| Workstream | What it removes | Est. effect | Risk |
|---|---|---|---|
| **W2** Socket lifecycle + CLI gating | +100 ms press after a stale `hark.sock`; `--help` toggling the daemon | stale press ≈ 10 ms; no leftover socket | Low |
| **W4** Tiny GTK-free hotkey client | ~19–20 ms of loader work on **every** press | press 48–65 → **≈30–45 ms** | Low |
| **W3** Idle ghost warm-up | ~170 ms of first-present + renderer init on the **first** press | first press 300–350 → **≈50–65 ms** | Med |
| **W1** Periodic-refresh truth-up | wrong comments/docs; decision record for the 45-min walk | — | Low |
| **W5** Optional experiments (renderer, `opt-level`) | ≤ a few tens of ms (moot after W3) | optional | Low |

Execution order (each independently shippable): **M1 = W2 + W1/W6 → M2 = W4 → M3 = W3 → M4 = W5.**
Biggest single UX win is W3; W4 is the easiest measurable win; W2 is correctness.

**Out of scope here:** daemon RSS growth (~62 → 213 MB observed over a session — separate investigation), service-mode rearchitecture, PGO/BOLT.

---

## 1. Problem & target budgets

Measured today (verified, §8):

| Moment | Cost |
|---|---|
| Hotkey client exec (every press) | **22–23 ms** (`/bin/true` = 0.6 ms; 114 libs, ~105–113 MB mapped, 53.7k relocs) |
| Warm press (daemon resident) | **48–65 ms** real env / 24–26 ms lab |
| First press, cold daemon | **≈300–350 ms** (docs baseline ≈280 ms) |
| Login CPU burst (`GtkApplication` register) | **~200–230 ms wall / ~309 ms user**, one-time |
| Every 45 min | full index re-walk **≈43–51 ms** + cache I/O |

Targets after this plan:

| Metric | Target | Reached by |
|---|---|---|
| Warm press | **≤ 35 ms** (real env, p50) | W4 |
| First press after boot | **≤ 65 ms** — indistinguishable from warm | W3 (+W4) |
| Stale-socket press overhead | **≤ 10 ms** | W2 |
| Leftover sockets / `--help` side effects | **0** | W2 |
| Login CPU burst | unchanged (inherent to GTK boot) — moved **off press time** | W3 |
| 45-min walk | unchanged cost, **documented**; halved frequency only if we accept stale content | W1 (decision) |

---

## 2. Invariants (must not break)

1. **Single-instance semantics**: a plain `hark` press toggles the resident daemon; `--daemon` first-activate stays hidden; rapid toggles collapse (capacity-1 channel, `main.rs:133–158`).
2. **IPC compatibility**: `"toggle\n"` write counts as delivered even without the `"ok\n"` ack (old daemon compat, `ipc.rs:42–69`).
3. **Keyboard semantics**: layer-shell `KeyboardMode::Exclusive` on real shows; DnD still gets Exclusive (`ui/dnd.rs:253`); the 150 ms `ignore_focus_loss` guard (`ui/mod.rs:1474–1477`) keeps behaving.
4. **UI behaviors**: entrance/hide CSS pops (`hark-anim-in/out`, `no_anim` layer rule), preview deferred clear/cancel, settings state reset on show.
5. **Lifecycle**: `scripts/install.sh` restarts via `pgrep -x hark` + `pkill -x hark`; SIGTERM/SIGINT flush + socket removal (`main.rs:164–186`).
6. **CI stays green**: `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test --all-features` (`.github/workflows/ci.yml`), on layer-shell enabled builds.

---

## 3. Method (how work gets done here)

### 3.1 The loop

1. **Measure first (M0)** — lock a baseline with the playbook in §7; store raw logs under `docs/bench/` (local, gitignored — repo convention: `docs/bench/run-*.txt`).
2. **Change one thing** per commit/PR; keep workstreams separate so each is independently revertible.
3. **Verify** — CI matrix + the workstream's acceptance table + a fresh lab pass (§6). For bug fixes: prove the new test catches the bug (see 3.3).
4. **Document** — update `performance.md`/`OPTIMIZATION.md` and append to the outcome log (§Appendix B).

### 3.2 Measurement rules

- **Never disturb the live session or the user's daemon.** All timing runs happen in an isolated lab (§7) with private `XDG_*` dirs, `dbus-run-session`, and a headless compositor.
- **Never `pkill`.** Kill only PIDs you spawned and tracked.
- No `sudo`, no system-wide cache drops; if a truly cold-disk number is needed, say so as UNCERTAIN instead.
- Report medians of ≥5 runs (or ≥30 for micro-timings) plus a range; single runs are anecdotes.

### 3.3 Prove the test catches the bug

For every bug fix in W1/W2: write the regression test, then **temporarily revert the fix (or the guard line), run the test, confirm it FAILS**, restore the fix, confirm it passes. A test that passes both ways is vacuous. Record the command used.

---

## 4. Workstreams

### W1 — Periodic refresh: align code comments, docs, and budget

**Facts (verified):**

- The tick is `recv_timeout(45 min)` (`engine.rs:119–136`); on each tick it reloads apps + calls `rebuild_index()` → `ensure_fresh()`.
- `ensure_fresh` (`index.rs:112–160`) only skips work when the **TTL is fresh** (`index.rs:119`, `:152–154`). At the tick, meta age is 45 min > `INDEX_TTL_SECS` (30 min, `index.rs:16`) → cache is stale → full walk (`:159` → `run_build`), then `save_cache` (~2 ms).
- The "fingerprint" is **config-only** (`index.rs:210–249`; comment at `:243–244`: “content freshness is handled by TTL”). There is **no** content-fingerprint short-circuit. `engine.rs:111–118` documents one that cannot happen — that comment is the bug.
- Net: **a ~43–51 ms metadata walk every 45 min is the intended discovery mechanism**, not a logic error.

**Decision to record (pick one, in a comment + `performance.md`):**

- **(a) Accept (recommended).** 43–51 ms per 45 min ≈ 0.1 % duty cycle. Fix the wrong comment, document cadence (“new files appear within ≤45 min”), fix the TTL row in `performance.md:203/:285`.
- **(b) Halve frequency.** Set `INDEX_TTL_SECS ≥ 45 min` → the tick’s `ensure_fresh` finds the RAM index TTL-fresh and returns **without walking**; then walks on the next tick (≈90 min cadence). *Trade:* new files take up to ~90 min to appear.
- **(c) Two-tier (future, only if the index grows 10×).** Per-directory mtime pruning so only changed subtrees are re-walked. Track as an `OPTIMIZATION.md` open item, not this plan.

**Also:** the walk runs on a background thread — verify it can never block the UI path (it can’t today: `build_lock` + swap; re-check after touching it).

**Decision (2026-10-17): (a) accept.** ≈0.1 % duty cycle; new files surface within ≤45 min. Recorded in `engine.rs` / `index.rs` comments and the `performance.md` TTL/daemon-refresh rows; no behavioral change.

**Acceptance:** comments match behavior; decision written down; `performance.md` TTL/refresh rows corrected; no behavioral change for (a).

### W2 — Socket lifecycle & CLI gating (stale `hark.sock`, `--help`)

**Facts (verified):**

- `--help` falls through to the IPC fast path: with a live daemon it **sends a toggle** (`main.rs:85`) before GTK prints its own help.
- Any non-daemon run that reaches `main.rs:139` spawns the listener thread which **creates `hark.sock`**. GTK consumes `--help` inside `run_with_args` (`main.rs:188`) and the process **exits without unwinding** — only the SIGTERM/SIGINT handler removes the socket (`main.rs:171–185`). Result: a stale socket file.
- Next press without a daemon: `request_toggle` sees `path.exists()` and burns **5 × 20 ms sleeps** (`ipc.rs:23–77`) → **+100 ms** on the press.
- The daemon reclaims stale sockets on bind (`ipc.rs:236–257`), so this only hurts the client path.

**Changes:**

1. **Hoist `help_requested`** from the inline check at `main.rs:30–36`; gate the fast path:
   `if !daemon && !help_requested && ipc::request_toggle() { return; }`
2. **Skip listener creation when `help_requested`** (we are about to exit inside GTK; nothing can be cleaned up later). No socket is left behind.
3. **RAII socket guard.** Move `bind_socket` out of the listener thread (synchronous bind in `spawn_listener_at`), keep the accept loop in the thread, and return `Option<SocketGuard>`; `main` holds it in scope. Normal exits (`app.run_with_args` returns, window close, `app.quit()`) drop the guard → socket removed. SIGKILL stays unrecoverable (daemon reclaim covers it). Update the existing tests that call `spawn_listener_at` (they can ignore the return).
4. **Retry budget for dead sockets.** Replace the fixed `5 × 20 ms` with ≤2 attempts and one ~10 ms sleep (constants `RETRY_ATTEMPTS`, `RETRY_SLEEP`), returning `false` quickly on `ConnectionRefused`. Worst-case stale-socket overhead: **≤ ~10 ms**. Rationale: a genuine “daemon rebinding” race needs ~200 ms+ (GTK boot), so 100 ms of client retries never helps today.

**Tests (with 3.3 discipline):**

- Pure predicate `should_offer_ipc(args) -> bool` (false for `--help`/`-h`/`--help-all`, `--search`, `update`) + table test.
- `SocketGuard` drop removes the file; dropping twice is harmless (idempotent remove).
- Retry schedule is a pure function (`[10 ms]` max); no wall-clock sleeps asserted in unit tests.
- Integration (lab): stale socket file + no daemon → `hark` press ≤ ~40 ms wall total (vs ~140 ms today), measured with the harness.

**Revert-proof (3.3, 2026-10-17; each test FAILED with the fix reverted, PASSED after restore):**

| Guard reverted | Command | Failure without the fix |
|---|---|---|
| `should_offer_ipc` help term | `cargo test --all-features --bin hark help_invocations_never_offer_ipc` | FAIL — “--help must not offer the IPC toggle path” |
| listener skip on `--help` | `cargo test --all-features --test cli_gating help_never_leaves_a_socket_behind` | FAIL — stale socket left behind |
| retry constants + `ECONNREFUSED` fast-fail | `cargo test --all-features --lib retry_schedule_is_one_short_sleep_at_most` · `cargo test --all-features --lib -- --ignored stale_socket_press_fails_fast` | FAIL — 4×20 ms schedule; stale press 80.6 ms (budget 60 ms) |
| `SocketGuard::drop` unlink | `cargo test --all-features --lib socket_guard_drop_removes_file_and_is_idempotent` | FAIL — “guard drop must remove the socket” |

**Acceptance:** help never toggles; `ls $XDG_RUNTIME_DIR/hark.sock` absent after `--help` and after a closed GUI run; stale press ≤10 ms overhead; SIGTERM behaviour unchanged; CI green.

### W3 — Idle ghost warm-up (the first-press fix)

**Goal:** pre-pay the cold show path — `present()` 102 ms + renderer init (Vulkan init measured 91 ms, inside present) + first frame (~71 ms after present) — during idle, so the first real press behaves like a warm show.

**Design (seams verified in source):**

- Add a visibility state machine to `Launcher`: `Hidden → Warming → Shown` (and back), stored as `Cell<Vis>`; extract transitions as a **pure function** so they are unit-testable without GTK:
  `next(state, event) -> (state, action)`; events = `press`, `warm_timer_elapsed`, `show`, `hide`.
- `pub fn warm_up(&self)` (`ui/mod.rs`, new; only from `Hidden`, only when `gtk4_layer_shell::is_supported()`):
  1. `window.set_opacity(0.0)` — never visible to the user.
  2. Run the existing show body **minus** `grab_focus` (`:1471`) — reusing `show()` via an internal `show_impl(Warm)` keeps all reset logic in one place. Keep `center_on_active_monitor` (`:1472`) — it also warms the hyprctl topology cache.
  3. Keyboard mode: currently `Exclusive` is set at construct (`setup_window_chrome`, `ui/mod.rs:3546`). **Defer it**: set `None` at construct, set `Exclusive` at the start of real shows (and leave `dnd.rs:253` as-is). The ghost must never take focus.
  4. Schedule `finish_warm()` after ~350 ms (one-shot timeout, stored like `hide_delay` so it can be cancelled): unmap (`set_visible(false)`), restore `opacity = 1.0`, `Vis::Hidden`. No CSS fade needed.
- `toggle()` (`:1398–1404`) gains the middle state: **`Warming` → promote**: cancel the timer, restore opacity, set `Exclusive`, `grab_focus`, `Vis::Shown`. A press during warm must never hide.
- **Trigger** (`main.rs` daemon-first-activate, `:112–121`): when the daemon boots hidden, schedule `launcher.warm_up()` after an idle delay (suggest 1.5–3 s) to avoid competing with the login burst; skip if the window was shown in the meantime (user pressed first).
- **Kill switch:** config `ui.ghost_warmup` (default true) + env `HARK_NO_GHOST=1` override for debugging.

**Edge cases to handle and test:**

| Case | Expected |
|---|---|
| Press during warm | Promote (never hide), ≤ ~30 ms to visible |
| Press before warm starts (idle window) | Normal real show; timer must skip |
| Rapid press→hide during promote | Normal toggle semantics |
| DnD started during warm | Exclusive as usual (dnd.rs:253) |
| Multi-monitor | Ghost centers on active monitor; real show re-centers anyway |
| Non-layer-shell (X11) | No ghost (guard) |
| Theme/settings change during warm | No special case — next show re-reads config |

**Risks & mitigations:** keyboard-mode semantics on wlroots (verify focus with `hyprctl activewindow` in lab + manual check); opacity-0 present on Vulkan (pixel-diff via `grim`); re-map cost after a quick warm hide (measure 2nd/3rd show — must stay at warm levels).

**Acceptance (lab unless noted):**

- First real press after daemon boot: daemon-side present→frame ≤ ~30 ms; end-to-end ≤ ~65 ms with W4 (real env check by user).
- Zero user-visible flash: `grim` diff before/during warm is empty; `hyprctl activewindow` unchanged during warm.
- Promote path ≤ ~30 ms, keyboard works immediately (typed query lands in the search field).
- `HARK_NO_GHOST=1` fully disables; `cargo test --all-features` + layer-shell lab pass.

**Measured (2026-10-17, lab `run-w3.sh`, instrumented build; independent from-scratch pass confirms):**

- Ghost warm (invisible): present **157 ms** · first frame **227–233 ms** after warm start — paid off-press, mapped ~2.07 s after daemon start, unmapped at +350 ms.
- First press after settle: present **0 ms** · press→frame **8 ms** (independent **9 ms** median, n=3; client stub ≈2 ms → ≈11 ms lab end-to-end). Cold without the ghost: present 153 ms · frame 230 ms (independent 222 ms median).
- Press during warm: promote **4 ms** daemon-side (independent 4–5 ms); settle timer cancelled — never unmaps a shown window, no doubled unmap.
- Race press-before-warm: the warm due is a no-op while shown (0 warm starts); later hide/show returns to warm levels (0 ms / 7 ms).
- Flash: `grim` 0 changed px in 3 shots + 12–13 burst frames; a control real show changed 79,244 px, proving the check is not vacuous.
- Kill switch: `HARK_NO_GHOST=1` → zero `ui/warm: start` lines; first press takes the pre-W3 path.
- Compositor focus state unchanged across warm (sway `get_tree`: same focused workspace; layer surface 0→1→0). Device-level keyboard focus is not observable in the headless lab — the Hyprland spot-check covers it.
- Revert-proofs (3.3): breaking `Warming+Press → Promote` or the `WarmElapsed`-after-promote guard fails 3 machine tests (recorded 2026-10-17).
- Raw (local, gitignored): `bench/cold-start-w3-2026-10-17.md`.

### W4 — Tiny GTK-free hotkey client

**Goal:** every press becomes a ~3–4 ms stub instead of a 22–23 ms GTK-linked exec (114 libs, 53.7k relocs).

**Design:**

- New bin target `hark-hotkey` (`src/bin/hark-hotkey.rs`); **do not reference the `hark` lib** (that would risk pulling GTK into the link). Share the protocol via path-includes of std-only modules:
  `#[path = "../ipc.rs"] mod ipc;` + `#[path = "../private_dir.rs"] mod private_dir;`. (As shipped, `ipc.rs` needed one non-std symbol — the N1 trust check — so `dir_is_trusted`/`current_euid` were extracted from `config.rs` into `private_dir.rs`; `config.rs` re-exports them for existing callers.)
- Behaviour: `ipc::request_toggle()`; on `false` (no daemon), **exec** the full binary from `current_exe().parent()` (arg-forwarding via `std::os::unix::process::CommandExt::exec`) so cold start and all flags (`--search`, `--daemon`, `update`) behave exactly like today. The stub fast-paths only the no-arg toggle; everything else is forwarded.
- Keep the existing fast path in `hark` itself (compat; users can switch back instantly by changing one keybind).

**Packaging / rollout:**

| File | Change |
|---|---|
| `scripts/install.sh` (`:99`) | install `target/release/hark-hotkey` → `~/.local/bin/hark-hotkey` |
| `packaging/aur/PKGBUILD` (`:42`) + `.SRCINFO` | install both binaries |
| `Cargo.toml` deb assets (`:75–79`) | add `hark-hotkey` |
| `scripts/package-release.sh` (`:78`) / `package-source.sh` | include the stub in the tarball |
| Hyprland bind (user dotfiles + docs) | `bind = ..., exec, hark-hotkey` |
| docs (`README`, `OPTIMIZATION.md`) | document the stub and the one-line bind change |

*Alternative (not now):* make the stub take over the `hark` name and rename the full binary `hark-full` (zero config change, but breaks `pgrep -x hark` in `install.sh`, desktop entries, and AUR muscle memory). Revisit at AUR-release time, not here.

**Acceptance:**

- `readelf -d target/release/hark-hotkey | grep NEEDED` contains **no** `libgtk-4`, `libgdk`, `libglib`, `libpango` (projection: ≤ ~10 libs).
- Wall per press ≤ ~5 ms p50 (30 runs, harness, hot cache) vs 22–23 ms today.
- With no daemon: `hark-hotkey` opens the launcher identically to `hark` (args forwarded, exit code preserved via exec).
- Old daemon/new client and new daemon/old client both interoperate (protocol unchanged).

**Measured (2026-10-17, lab `run-w4.sh`, daemon resident, hot cache):**

- NEEDED = `libgcc_s`, `libc` only (`ldd` 4 lines); 351,544 B vs 7,775,480 B for `hark`.
- 30 runs each, same harness: `hark` **27.8 ms p50** (25.2–39.3) → `hark-hotkey` **2.7 ms p50** (2.5–3.8).
- **Independent pass** (§6.3, fresh agent with its own from-scratch lab): `hark-hotkey` **2.22 ms p50** (1.53–3.37) vs `hark` 28.9 ms; `/bin/true` harness floor 1.58 ms; delivery re-proven (`toggle\n` bytes + ack, 60/60 exit 0); no discrepancies.
- Forwarding/fallback: `tests/hotkey_stub.rs` 4/4 locally — help reaches the real binary; argv + exit code (7) preserved via a fake sibling `hark`; no-daemon fallback; live toggle ack (one test `#[ignore]`d for sandboxed CI, run with `--ignored`).
- Packaging smoke: `packaging/install-user.sh` installed both binaries into a temp prefix.
- Raw (local, gitignored): `bench/cold-start-w4-2026-10-17.md`.

### W5 — Optional experiments (only after W3/W4, only with numbers)

| Experiment | Method | Keep only if |
|---|---|---|
| `GSK_RENDERER=gl`/`cairo` for the daemon | lab first-present timing | >10 % off the (remaining) cold path and zero rendering regressions — moot once W3 lands |
| `opt-level = "s"` → `3` (`Cargo.toml:56–61`) | `--bench` medians + client wall + binary size | consistent >3 % search/startup win for ≤ ~0.5 MB size |
| PGO/BOLT | realistic 2–7 % (research) | last resort after everything else; effort is high — leave dormant |

### W6 — Docs & baseline upkeep

- `performance.md` corrections (verified drift): `opt-level=3` claim (`:220` → actual `"s"` until W5 decides), cache **v6 → v9** (`:31/:64/:199–211/:285`; actual `CACHE_VERSION = 9`, `index.rs:20`), TTL row semantics after W1.
- `OPTIMIZATION.md`: row **E** wording (fingerprint+TTL), Notes (`:71` v6 → v9), link this plan, keep bench logs in `bench/`.
- After each milestone: append measured before/after to the outcome log (Appendix B) and drop the raw run into `docs/bench/`.

---

## 5. Milestones

| Milestone | Contents | Definition of done |
|---|---|---|
| **M0 Baseline** | Re-run §7 harness on this machine, store `docs/bench/cold-start-*.txt` | Numbers reproduce §8 within noise; stored under `docs/bench/` (local, gitignored) |
| **M1** | W2 (+W1 decision +W6 doc fixes) | Acceptance tables pass; revert-test proofs recorded |
| **M2** | W4 client + packaging | Stub ldd clean; ≤5 ms press; bind documented |
| **M3** | W3 ghost warm-up (config-gated, default on) | Lab acceptance passes; user spot-check on Hyprland; kill switch works |
| **M4** | W5 experiments (optional) | Each kept change has numbers in `bench/` |

Dependencies: none between W2/W4. W3 is independent but is measured end-to-end best after W4 (client ~4 ms constant). M1 first because correctness fixes are cheap.

---

## 6. Verification protocol (per workstream)

1. CI: `fmt` + `clippy -D warnings` + `test --all-features` (matches `.github/workflows/ci.yml`).
2. Lab re-measure: repeat §7 for the metrics the workstream touches; compare against M0 baseline.
3. **Independent pass**: for W3/W4, have someone (or a fresh agent/subagent) re-run the lab from scratch and re-derive the numbers — do not trust the author's logs.
4. Revert-test proof for every bug fix (3.3).
5. Rollback plan: W2/W4 = revert the commit (stub removal is inert; rebind keybind back to `hark`); W3 = `HARK_NO_GHOST=1` immediately, config `ui.ghost_warmup=false` to disable durably, then revert.

---

## 7. Measurement playbook

**Repo micro-bench (search/index only):**

```bash
cargo build --release --features "layer-shell,bench"
./target/release/hark --bench | tee docs/bench/run-$(date +%F).txt
```

**Lab (never touches the live session).** Reference implementation from the analysis: `/tmp/opencode/hark-lab/drive.sh` + `measure.py` (local, may not persist — re-create from this recipe):

```bash
LAB=/tmp/hark-lab; mkdir -p "$LAB"/{run,config,cache,state,data,logs}
export XDG_RUNTIME_DIR=$LAB/run XDG_CONFIG_HOME=$LAB/config XDG_CACHE_HOME=$LAB/cache \
       XDG_STATE_HOME=$LAB/state XDG_DATA_HOME=$LAB/data

# 1) headless compositor (fully invisible), private socket
WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 \
  sway -c /dev/null --socket "$LAB/run/wayland-lab" &
SWAY_PID=$!
export WAYLAND_DISPLAY=$LAB/run/wayland-lab   # via private XDG_RUNTIME_DIR on wayland-1

# 2) daemon under a private bus; track the PID, kill only that PID later
dbus-run-session -- hark --daemon >"$LAB/logs/daemon.log" 2>&1 &
DPID=$!
for _ in $(seq 1 400); do [ -S "$XDG_RUNTIME_DIR/hark.sock" ] && break; sleep 0.01; done

# 3) client wall + CPU (30 runs), e.g. with the analysis' monitor:
python3 measure.py -- cmd  # see /tmp/opencode/hark-lab/measure.py
# or: /usr/bin/time -v hark   (wall + user/sys + minflt)

# 4) toggle → first frame: run an instrumented build with trace points behind
#    an env var (local patch only, never shipped), OR pixel-diff with grim:
grim "$LAB/a.png"; hark; grim "$LAB/b.png"   # frame probe quantum applies

kill $DPID $SWAY_PID   # tracked PIDs only — never pkill
```

Metrics to record per run (and their sources): client wall/user/sys/minflt (`time -v` or `measure.py`), daemon register wall/user (GTK probe or trace), first-present and present→first-frame (trace or grim quantum), warm press end-to-end, index walk ms (`--bench` + trace), stale-socket press wall.

**Rule of thumb:** if a change doesn't move a number in this table, it doesn't ship.

---

## 8. Baseline (verified 2026-10-17)

| Stage | Value | Notes |
|---|---|---|
| Client exec (per press) | 22–23 ms wall · 14.5 user · 8.3 sys · 1760 minflt | 114 libs, ~105–113 MB mapped, 53,741 relocs; exactly 1 socket connect; `/bin/true` = 0.60 ms |
| `gtk_init` | 6.5 ms warm portal · 157.6 ms fresh bus · 2.6 ms no-services | fresh-bus = one-per-session portal activation |
| `GtkApplication` register | 200–230 ms wall · ~309 ms user | CPU-bound font/style/renderer init, gated on icon-theme resolution (29 ms when none resolves) |
| `Launcher::new` | 20–23 ms (range 19–26) | SettingsPanel eager 8 pages = 10–12 ms |
| Cold present | 102 ms [78–127] | renderer init inside present: cairo 13 / GL 59 / **Vulkan 91 ms** (default GSK renderer is Vulkan) |
| First frame | +71 ms after present (175 ms after show begins) | |
| Warm show | present 0 ms · frame 7 ms [6–11] | reload 2 ms · theme refresh 0 ms (46/46) |
| Engine | construct 0–1 ms; apps 1–2 ms; cache load 4–5 ms/6730 items; walk 43–51 ms/6854 items/13.6k syscalls; save 2 ms | 774 ms first-ever cold walk ⚠️ UNCERTAIN (couldn't drop caches safely) |
| First press / warm press (end-to-end, real env) | ≈300–350 ms / 48–65 ms | warm lab = 24–26 ms |
| Periodic tick | ~50–60 ms every 45 min | by design; W1 decides policy |

> Re-measured 2026-10-17 (M0, post-W2, same lab): warm press p50 28.1 ms
> (30 runs, the harness W4 will use), daemon socket-ready 37.1 ms,
> stale-socket overhead 0.1 ms, `--help` leaves no socket 3/3.
> Raw (local, gitignored): `bench/cold-start-2026-10-17.md`.

---

## 9. Don't chase (already refuted — do not re-invest)

| Idea | Why not |
|---|---|
| `LD_BIND_NOW` | Δ −1.4 %, verifier −0.3 ms — relocs are eager relative; no effect |
| Early `gtk-icon-theme-name` override / tiered themes | Shell game: cost relocates from register into the property set (175.7 + 7.7 = control total); "icon-file scanning" attribution was wrong (≈40 icon syscalls; it's font/style/renderer init gated by theme resolution) |
| Pre-starting the session portal | 155 ms fresh-bus cost is one-time per session, not per app |
| `cairo`/`gl` renderer | Cuts init inside present (13/59 ms) but **moot after W3**; keep only as an experiment |
| PGO/BOLT | 2–7 % realistic; high tooling cost; dormant until W3/W4/W5 are done |
| Walking-short-circuit via the config fingerprint | It doesn't finger-print content (`index.rs:243–244`); content freshness *is* the TTL walk |

---

## Appendix A — Seam map (verify before editing)

| Area | Location | What |
|---|---|---|
| Help gate | `src/main.rs:35` | `--help` detection (`help_flag_present`, W2) |
| IPC fast path | `src/main.rs:89` | `should_offer_ipc` gate + `request_toggle` before GTK |
| Listener spawn | `src/main.rs:153–190` | socket creation + `SocketGuard` held (W2) |
| Signal cleanup | `src/main.rs:196–213` | removes socket on SIGTERM/SIGINT |
| `run_with_args` | `src/main.rs:215` | GTK parses `--help` and exits here |
| Ghost warm-up (W3) | `src/ui/mod.rs:126` (`next`) · `:1580` (`warm_up`) · `:1628` (`promote`); trigger `src/main.rs:16`, `:127–141` | vis machine, idle warm, promote |
| `request_toggle` | `src/ipc.rs:40–113` | retry budget: 2 attempts / 10 ms (W2) |
| Listener / bind | `src/ipc.rs:115–260`, `:267` | `SocketGuard` (`:127`) + sync bind + stale reclaim (W2) |
| Hotkey stub | `src/bin/hark-hotkey.rs`; `tests/hotkey_stub.rs` | GTK-free client + exec fallback (W4) |
| Toggle / show / hide | `src/ui/mod.rs:1553–1789`, `dismiss` `:1791` | `toggle`/`show`/`hide`/`warm_up`/`promote`; Real shows `grab_focus`, warm skips it (W3) |
| Keyboard mode | `src/ui/mod.rs:3803–3809` (construct `None`) · `:3831–3862` (helpers); `src/ui/dnd.rs:253` | Exclusive at real show/promote (W3 deferral) |
| Settings eager build | `src/ui/settings.rs:271–295`; call `src/ui/mod.rs:502` | 10–12 ms at construct (future option) |
| Tick | `src/engine.rs:127–142` | 45 min periodic refresh (W1) |
| TTL / freshness | `src/providers/files/index.rs:21`, `:117–165`, `:894` | TTL decision + ensure_fresh (W1) |
| Fingerprint | `src/providers/files/index.rs:215–254`, `:248` | config-only, includes `CACHE_VERSION` (v9, `:25`) |
| Cache write | `src/providers/files/index.rs:945` | fsync ~9 µs |
| Profile | `Cargo.toml:62–67` | `opt-level = "s"` (W5) |
| Install | `scripts/install.sh:100`; `packaging/aur/PKGBUILD:43`; `Cargo.toml:81–87` | binary install paths (W4) |
| Docs (W6) | `docs/performance.md` · `docs/OPTIMIZATION.md` | v6→v9, opt-level `"s"`, TTL semantics — fixed 2026-10-17 |

## Appendix B — Acceptance summary & outcome log

| WS | Must pass | Status |
|---|---|---|
| W1 | comments/docs truthful; decision recorded | ☑ 2026-10-17 |
| W2 | help no toggle; no leftover socket; stale press ≤10 ms; tests + revert-proof | ☑ 2026-10-17 |
| W4 | stub ldd has no GTK; ≤5 ms press; fallback exec; packaging updated | ☑ 2026-10-17 |
| W3 | first press ≤65 ms; no flash; no focus steal; promote ≤30 ms; kill switch | ☑ lab 2026-10-17 · independent confirmed (0 ms present / 8–9 ms frame; promote 4 ms; flash 0 px) · ☐ Hyprland spot-check |

| Date | Milestone | Result (measured) | Evidence |
|---|---|---|---|
| 2026-10-17 | W2 landed | stale press 126→27 ms; `--help` socket present→absent 3/3; revert-proofs ×4 recorded | commit `980ceb9` · §W2 |
| 2026-10-17 | M0 baseline (post-W2) | warm press p50 28.1 ms (30 runs) · stale overhead 0.1 ms · daemon socket-ready 37.1 ms · help socket absent 3/3 | `bench/cold-start-2026-10-17.md` (local) |
| 2026-10-17 | W1 + W6 | engine/index comments truthful; `performance.md` v9/opt-level/TTL rows; `OPTIMIZATION.md` E + notes; decision (a) recorded | this doc · `docs/performance.md` |
| 2026-10-17 | M2: W4 | `hark-hotkey` stub: press p50 **2.7 ms** vs `hark` 27.8 ms (30 runs each; independent pass 2.22 vs 28.9); NEEDED = libc+libgcc only; packaging + tests + docs updated | `bench/cold-start-w4-2026-10-17.md` (local) |
| 2026-10-17 | M3: W3 | ghost warm present 157 ms off-press; first press **0 ms present · 8 ms to frame** (cold without ghost 153/230); promote **4 ms**; grim before=during=after 0 px (control show 79k px); `HARK_NO_GHOST=1` → 0 warm starts; **independent pass: 9 / 4 / 222 ms** | `bench/cold-start-w3-2026-10-17.md` (local) |
