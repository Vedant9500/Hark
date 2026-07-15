# Blink vs Rofi — power, memory, CPU, and process model

**Last updated:** 2026-07-15  
**Scope:** battery-relevant idle cost, open-window spikes, background processes, binary/deps footprint.  
**Machine (Blink numbers):** Linux / Hyprland · 16 CPUs · ~15.5 GB RAM · RTX 4060 Laptop  

> **Rofi is not installed on this machine** (`pacman -Si rofi` only). Rofi figures below are from package metadata + typical launcher architecture — not side-by-side wattmeter runs. Re-run the [measurement recipe](#how-to-measure-yourself) after installing rofi for apples-to-apples numbers.

Related: [`performance.md`](./performance.md) (search latency / index), [`battery-optimization.md`](./battery-optimization.md) (how to tune for battery), [`OPTIMIZATION.md`](../OPTIMIZATION.md) (improvement log).

---

## TL;DR

| Question | Winner | Why |
|----------|--------|-----|
| **Lowest always-on RAM / idle power** | **Rofi** | No resident process; ~0 MB when closed |
| **Lowest open-window RAM** | **Rofi** | Cairo/XCB-ish stack, ~1 MB package; typically tens of MB open |
| **Fastest hotkey → UI (warm)** | **Blink** | Daemon already holds GTK + indexes; toggle via Unix socket |
| **Cold start cost** | **Rofi** (cheaper) / **Blink** (heavier without daemon) | GTK4 init + index warm is expensive |
| **Search / calc / files in one box** | **Blink** | Built-in providers; rofi needs scripts/modes |
| **Battery if you open launcher often** | **Tie / depends** | Blink pays continuous ~60 MB; rofi pays cold-start CPU each open |
| **Battery if launcher is rare** | **Rofi** | Zero background cost |
| **Hyprland / Raycast-style daily driver** | **Blink** | Overlay + resident toggle + rich features |

**Bottom line:** Rofi wins pure efficiency. Blink trades **~60–65 MB idle RSS + a few threads** for instant toggle and a fuller feature set. For a laptop that opens a launcher dozens of times a day, that trade is often worth it; for minimal/battery-max setups, rofi (or fuzzel) is leaner.

---

## Process model (background processes)

### Blink

| Process | When | Role |
|---------|------|------|
| `blink --daemon` | Login (`exec-once` / autostart) | Resident GTK app; owns window, engine, file index, IPC listener |
| `blink` (hotkey) | Each Alt+A (etc.) | **Short-lived:** connects to `$XDG_RUNTIME_DIR/blink.sock`, writes `toggle\n`, exits. No second GTK if daemon is up |
| Optional bg thread | Inside daemon | File index rebuild (walk + cache write); not a separate binary |
| Unix socket | While daemon up | `blink.sock` mode `0600` |

**Startup path (warm):**

```
hotkey → blink (ms) → UnixStream → daemon toggle → show/hide overlay
```

**Cold path (no daemon):**

```
hotkey → blink starts GTK Application → build UI + Engine → show
```

Code: `src/main.rs` (daemon / toggle), `src/ipc.rs` (socket).

### Rofi

| Process | When | Role |
|---------|------|------|
| `rofi -show drun` (etc.) | Each keybind | Full process: load config, scan desktop entries, paint UI, exit on selection/Esc |
| No required daemon | — | Classic design: **zero background** when closed |
| Optional scripts/modes | Per invocation | File search, calc, etc. usually shell out or use plugins — extra cost only while open |

**Typical Hyprland bind:**

```conf
bind = $mod, D, exec, rofi -show drun
```

No `exec-once` for the launcher itself.

### Comparison

| Aspect | Blink | Rofi |
|--------|------:|-----:|
| Background binary required | **Yes** (`--daemon`) for best UX | **No** |
| Idle processes | 1 (daemon) | 0 |
| Hotkey child process | Tiny IPC client | Full launcher binary |
| Persistent state | In-memory apps + file index + GTK | Mostly re-read each run (desktop cache may help) |
| Socket / IPC | Yes (`blink.sock`) | Not for basic use |

---

## Memory

### Blink (measured on this project)

| State | RSS (typical) | Source |
|-------|-------------:|--------|
| Idle daemon | **~60–65 MB** | `docs/performance.md`, OPTIMIZATION baseline |
| Idle daemon (logged) | **60.7 MB** (62,116 KB) | OPTIMIZATION.md 2026-07-13 |
| Panel open (inflated sample) | **~289–296 MB** | UI open / GTK caches — compare same session carefully |
| Headless `blink --bench` | **~27 → 33 MB** | Live 2026-07-15 run |
| Bench peak HWM | **~38 MB** | same run |
| File index on disk | **~114 KB – 710 KB** | depth / roots (this machine ~710 KB @ 8k items) |
| Config | **~1 KB** | `~/.config/blink/config.json` |
| Threads (idle daemon) | **~7–8** | performance.md |
| VSZ (daemon) | **~640 MB** | virtual; not physical |

**Why ~60 MB idle?** Almost entirely **GTK4** (and linked stack: pango, cairo, gdk-pixbuf, gstreamer helpers via gtk, wayland, vulkan loader paths, etc.). Rust heap / index at depth 2 is small vs toolkit.

**Live bench snapshot (2026-07-15):**

```
index: 8026 items · warm_ms=616 · cache_bytes=710589
bench rss_kb: 27328 → 33416  (Δ 6088)
hwm_kb:       38380
threads:      2   (bench process, not daemon)
binary_bytes: 4818160 (~4.6 MB)
daemon:       (not running during this sample)
```

Documented reference (depth 2, ~1.8k items) is snappier and still **~60–64 MB daemon RSS**.

### Rofi (package + typical behavior)

| Metric | Value | Notes |
|--------|------:|-------|
| Arch package install size | **~1.1 MB** | `pacman -Si rofi` 2.0.0 (includes Wayland) |
| Download size | **~600 KB** | same |
| Dependencies | cairo, pango, glib, libxcb / wayland, gdk-pixbuf, … | **No GTK4** |
| Idle (closed) | **0 MB** | no process |
| Open window RSS | **typically ~15–40 MB** (community / similar tools) | Not measured here — re-check with `/usr/bin/time -v rofi -show drun` |

Rofi’s stack is lighter than GTK4. Expect **lower open-window RSS** and **no always-on RSS**.

### Memory verdict

| Metric | Blink | Rofi | Better for RAM |
|--------|------:|-----:|----------------|
| Always-on | ~60–65 MB | **0** | **Rofi** |
| While open | tens–hundreds MB (GTK) | typically lower | **Rofi** |
| Disk cache | file index hundreds of KB | small desktop caches | Rofi slightly |
| Binary | ~4.3–4.6 MB stripped | package ~1 MB total | **Rofi** |

---

## CPU spikes

### Blink

| Event | Cost | Notes |
|-------|-----:|-------|
| Idle daemon | **&lt;1%** CPU (samples ~0.3%) | sleeps on GTK main loop + IPC |
| Hotkey toggle (warm) | **very small** | IPC write + show/hide; no process spawn of GTK |
| Keystroke search | **µs–ms** | math ~2–10 µs; apps/files tens–hundreds µs at depth 2; see performance.md |
| Index rebuild (blocking walk) | **~15 ms** @ ~1.8k items · **~155 ms** @ ~8k items (live) | bg-friendly design; still CPU when rebuilding |
| Index warm from cache | **~50–600 ms** | depends on cache size / cold page cache |
| Image preview decode | **~15–50 ms** full-res risk | tracked in `preview-optimization.md`; UI-thread care |
| GPU | **0%** CUDA | no dedicated GPU path; compositor may use GPU for surfaces |

**Search CPU (reference depth 2):**

| Case | Median | Notes |
|------|-------:|-------|
| Math / units | ~2 µs | short-circuit; almost free |
| iso_apps `fire` | ~34–37 µs | |
| iso_files `doc` | ~49 µs | |
| Merged app/file | ~35–70 µs | |

Heavier indexes (depth 3–4) scale roughly with item count (see performance depth chart).

### Rofi

| Event | Cost | Notes |
|-------|-----:|-------|
| Idle | **0** | no process |
| Open | **process start + desktop scan + paint** | cold start every time |
| Filter keystrokes | usually light | fuzzy/filter over loaded list |
| Exit | free all | no long-term leak if well behaved |

**CPU-spike shape:**

- **Rofi:** sharp spike **on every open** (startup + I/O), then quiet until next open.  
- **Blink:** one-time (or periodic) index cost in daemon; **open is cheap**; keystrokes stay sub-ms for normal indexes.

If you spam the launcher, Blink’s amortized CPU is often better. If you open it twice a day, Rofi’s total CPU·time is tiny and idle is zero.

---

## Power draw (battery)

Direct watt numbers need `powertop`, `powerstat`, or RAPL/`amd_pstate` sampling. From process model:

| Scenario | Dominant cost | Likely lower power |
|----------|---------------|--------------------|
| Lid open, launcher unused for hours | Blink’s **~60 MB RSS** + occasional timers/index TTL | **Rofi** (nothing running) |
| Many launches / hour | Rofi **cold starts**; Blink **IPC toggle** | **Blink** (amortized) |
| Typing heavy queries | Both small vs compositor/browser | roughly equal; Blink calc/files are very cheap |
| File index rebuild on Blink | short CPU burst every TTL / config change | Rofi avoids this (unless custom scripts walk trees) |

**Rule of thumb:**

- **Always-on memory ≈ always-on power** on laptops (DRAM + background wakeups). Blink pays a fixed tax.  
- **Spiky CPU ≈ short power bumps.** Rofi pays per open; Blink pays on index rebuild and first GTK show.

Neither tool should compete with a browser tab or unthrottled GPU for battery impact — both are small utilities.

---

## Startup latency (perceived)

| Path | Blink | Rofi |
|------|-------|------|
| First ever start | GTK + engine warm + maybe index rebuild | binary + drun scan |
| With daemon already up | **Near-instant show** (design goal) | N/A |
| Without daemon | Full cold GTK start | Normal rofi start |
| After Esc | process stays (daemon) | process exits |

Blink’s whole point of `--daemon` is to avoid paying GTK cold start on every hotkey.

---

## Features vs resource cost

| Capability | Blink | Rofi | Resource note |
|------------|:-----:|:----:|---------------|
| App launch (`.desktop`) | yes | yes (`drun`) | both fine |
| Fuzzy app filter | yes | yes | |
| File / folder index | **built-in** | scripts / plugins | Blink: RAM + disk cache + rebuild CPU |
| Math / units / FX | **built-in** | scripts | Blink: near-zero CPU when short-circuited |
| Settings UI | yes | config files | Blink: more GTK surface area |
| Layer-shell overlay | optional feature | depends on build | both OK on Wayland modern builds |
| Window switcher / dmenu | not primary | **strong** | rofi’s original niche |
| Resident daemon | **required for best UX** | optional/unused | main power difference |

Blink is a **productized multi-provider launcher**. Rofi is a **minimal, composable picker**. Comparing only power without features is incomplete.

---

## Dependencies / binary footprint

### Blink

- Binary: **~4.3–4.6 MB** stripped release (`layer-shell`)
- Runtime: **GTK 4**, optional `gtk4-layer-shell`, glib/gio, wayland, full GTK dep tree (pango, cairo, graphene, often gstreamer-related libs via gtk, etc.)
- Cache: `~/.cache/blink/` (file index, FX rates)

### Rofi (Arch `rofi` 2.0)

- Installed size: **~1.1 MB**
- Depends: bash, cairo, gdk-pixbuf2, glib2, pango, wayland, libxcb stack, xkbcommon, …  
- **No GTK4** → smaller runtime surface and typically lower open RSS

On this system, **fuzzel** is installed (light Wayland launcher, ~314 KB package) and is an even thinner alternative if the goal is minimal power without Blink’s features.

---

## Which is “better”?

Depends on the goal:

### Choose **Rofi** if you care most about

1. **Zero background RAM/power**  
2. Minimal deps and install size  
3. Classic dmenu/scripting workflows  
4. Occasional app launching only  

### Choose **Blink** if you care most about

1. **Instant toggle** after login (daemon)  
2. Apps + files + calc/units in one overlay (Raycast-style)  
3. Hyprland-first layer-shell UX and in-app settings  
4. Accepting **~60 MB idle** as the cost of warmth  

### Hybrid note

Some setups use **fuzzel/rofi for pure app launch** and something else for files — lowest power, more keybinds. Blink intentionally consolidates that into one daemon.

---

## How to measure yourself

### Blink (built-in)

```bash
# Idle daemon resources (start once)
blink --daemon &
sleep 2
blink --bench          # latency + RSS + daemon section if daemon found

# Live process
ps -o pid,rss,vsz,%cpu,etime,cmd -C blink
cat /proc/$(pgrep -f 'blink --daemon' | head -1)/status | grep -E 'VmRSS|VmHWM|Threads'
```

### Rofi (after install)

```bash
sudo pacman -S rofi   # or distro equivalent

# Peak RSS of one open (close with Esc quickly after measure, or use -dump)
/usr/bin/time -v rofi -show drun -e '' 2>&1 | grep -E 'Maximum resident|User time|System time|Elapsed'
# or:
rofi -show drun &  sleep 0.5; ps -o rss,vsz,%cpu,cmd -C rofi; pkill rofi
```

### Power (either)

```bash
# Example: sample package power while idle vs while opening launcher N times
# (needs privileges / RAPL / powerstat — machine-specific)
sudo powerstat -d 0 60
# or powertop --html=power.html
```

Compare:

1. Idle 5 min with only Blink daemon vs only rofi-not-running  
2. 50 open/close cycles of each  
3. Typing the same query length  

---

## Scorecard (this project’s data + fair architecture)

| Metric | Blink | Rofi | Edge |
|--------|------:|-----:|------|
| Idle RSS | ~60–65 MB | **0** | Rofi |
| Idle CPU | ~0–0.5% | **0** | Rofi |
| Open RSS | higher (GTK) | lower | Rofi |
| Warm open latency | **best** | cold every time | Blink |
| Search latency (in-process) | excellent (µs–ms) | depends on mode | Blink for multi-provider |
| Binary / package size | ~4.5 MB | **~1 MB** | Rofi |
| Background processes | **1 daemon** | **0** | Rofi |
| Features / UX density | **high** | minimal + scripts | Blink |
| Battery (rare use) | tax of daemon | **best** | Rofi |
| Battery (frequent use) | amortized warm path | repeated startups | Blink often |

**Practical recommendation for this Blink repo’s target (Hyprland daily driver):**  
Blink is the better **launcher product**; Rofi is the better **low-power picker**. If battery is critical and you only need apps, use rofi/fuzzel. If you want Raycast-like density and instant Alt+A, keep Blink’s daemon and treat ~60 MB as intentional.

---

## Changelog

| Date | Note |
|------|------|
| 2026-07-15 | Initial doc from `performance.md` / OPTIMIZATION numbers + live `--bench`; rofi not installed for side-by-side watt/RSS capture |
