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

## Install (share with friends)

Linux doesn’t have one universal “APK / MSI”. Blink ships the closest equivalents:

| What | Who it’s for | How |
|------|----------------|-----|
| **One-line installer** | Anyone with curl | Downloads latest release + installs under `~/.local` |
| **Portable `.tar.gz`** | Offline / USB share | Extract → `./install.sh` |
| **AUR `PKGBUILD`** | Arch / Endeavour / Cachy | `makepkg -si` (or AUR helper once published) |
| **`.deb`** | Debian / Ubuntu | Optional via `cargo-deb` |
| **From source** | Developers | `./scripts/install.sh` |

### One-line (after you publish a GitHub Release)

```bash
curl -fsSL https://github.com/YOUR_GITHUB_USER/blink/releases/latest/download/install.sh | bash

# optional: also enable login autostart of the daemon
curl -fsSL https://github.com/YOUR_GITHUB_USER/blink/releases/latest/download/install.sh | bash -s -- --autostart
```

### Portable binary package (no GitHub needed)

Friend does **not** need Rust — just GTK4.

```bash
# you build once:
./scripts/package-release.sh
# share: dist/blink-0.1.0-x86_64-linux.tar.gz

# friend installs:
tar xzf blink-0.1.0-x86_64-linux.tar.gz
./blink-0.1.0-x86_64-linux/install.sh
```

### Complete source package (no GitHub / no git clone)

Share the full code so they can build/modify it themselves:

```bash
# you run:
./scripts/package-source.sh
# share: dist/blink-0.1.0-source.tar.gz   (small — excludes target/)

# friend:
tar xzf blink-0.1.0-source.tar.gz
cd blink-0.1.0-source
# read BUILD_FROM_SOURCE.txt
./scripts/install.sh          # build + install + restart daemon
# ./scripts/install.sh --no-restart
# ./scripts/install.sh --restart-only
```

Do **not** zip the whole project folder by hand — `target/` alone is multi‑GB of junk.
### Uninstall (user install)

```bash
# from the extracted package, or:
~/.local/…  # or re-run the package’s uninstall.sh
```

`packaging/uninstall-user.sh` removes the binary, desktop entry, icon, and autostart file.

### Requirements (runtime)

- Linux x86_64 (or aarch64 when you build for it)
- **GTK 4** (`gtk4` / `libgtk-4-1`)
- **Recommended on Hyprland:** `gtk4-layer-shell` for true overlay mode

```bash
# Arch
sudo pacman -S gtk4 gtk4-layer-shell

# Debian / Ubuntu (names vary by version)
sudo apt install libgtk-4-1
# layer-shell package name may be libgtk4-layer-shell0
```

## Build from source

```bash
# optional but recommended on Hyprland
sudo pacman -S gtk4 gtk4-layer-shell

./scripts/install.sh
# or:
cargo build --release --features layer-shell
```

### Make a shareable release yourself

```bash
# builds binary + dist/blink-*-linux.tar.gz + dist/install.sh + SHA256SUMS
./scripts/package-release.sh

# optional .deb (Ubuntu/Debian friends)
cargo install cargo-deb
cargo deb --release --features layer-shell
```

Tag + push to let GitHub Actions attach artifacts to a Release:

```bash
git tag v0.1.0
git push origin v0.1.0
```

## Hyprland

Blink runs as a **resident daemon** (started on login). **Alt+A** toggles the window instantly.

```lua
-- execs.lua  (preload, no window)
hl.exec_cmd("blink --daemon")

-- keybinds.lua  (toggle via single-instance activate)
hl.bind(vars.kbBlink, hl.dsp.exec_cmd("blink"))  -- kbBlink = ALT + A
```

Or in `hyprland.conf`:

```conf
exec-once = blink --daemon
bind = ALT, A, exec, blink
```

## Prefixes

- `f <query>` / `file <query>` — files only  
- `~/…` or `/…` — path browser  
- math/conversion queries float to the top automatically  

## Docs

- **[docs/performance.md](docs/performance.md)** — search latency, index depth chart, binary/RAM, how to re-bench  
- **[docs/power_performance.md](docs/power_performance.md)** — Blink vs Rofi: power, memory, CPU, background processes  
- **[docs/battery-optimization.md](docs/battery-optimization.md)** — battery life & low CPU spike plan  
- **[docs/](docs/)** — performance reference + raw depth benchmark JSON  
- **[OPTIMIZATION.md](OPTIMIZATION.md)** — modularization / optimization worklog  
- **[packaging/](packaging/)** — desktop entry, user installer, AUR `PKGBUILD`

```bash
blink --bench   # latency + index rebuild + resources
```
