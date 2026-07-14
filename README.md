# Blink

Raycast-style launcher for Linux, built for **Hyprland / Wayland**.

## Features (v0.1)

| Feature | Usage |
|--------|--------|
| **App search** | Type app name — fuzzy match `.desktop` entries |
| **File / folder search** | Type ≥2 chars, or `f query`, or path like `~/Doc` |
| **Math** | `2+2`, `sqrt(144)`, `15% of 80`, `tip 20% on 45` |
| **Unit conversion** | `100 km to mi`, `32 f to c`, `1 gb to mb` |
| **Bases** | `0xFF`, `0b1010` |

**Keys:** `↑/↓` navigate · `Enter` open/copy · `Ctrl+C` copy calc · `Esc` close

## Build

```bash
# optional but recommended on Hyprland
sudo pacman -S gtk4 gtk4-layer-shell

./scripts/install.sh
# or:
cargo build --release
# with overlay:
cargo build --release --features layer-shell
```

## Hyprland

Blink runs as a **resident daemon** (started on login). **Alt+A** toggles the window instantly.

```lua
-- execs.lua  (preload, no window)
hl.exec_cmd("blink --daemon")

-- keybinds.lua  (toggle via single-instance activate)
hl.bind(vars.kbBlink, hl.dsp.exec_cmd("blink"))  -- kbBlink = ALT + A
```

## Prefixes

- `f <query>` / `file <query>` — files only  
- `~/…` or `/…` — path browser  
- math/conversion queries float to the top automatically  

## Docs

- **[docs/performance.md](docs/performance.md)** — search latency, index depth chart, binary/RAM, how to re-bench  
- **[docs/](docs/)** — performance reference + raw depth benchmark JSON  
- **[OPTIMIZATION.md](OPTIMIZATION.md)** — modularization / optimization worklog  

```bash
blink --bench   # latency + index rebuild + resources
```
