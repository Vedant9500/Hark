# Blink — Action Panel audit

**Status:** Phase 1 implemented · Phase 2+ pending  
**Last updated:** 2026-07-21 (Phase 1)  
**Related:** user request (Raycast-style actions on selected item); [`feature.txt`](../feature.txt)  
**Code baseline:** `Action` enum in `src/providers/mod.rs`, footer chips in `src/ui/footer.rs` / `src/ui/mod.rs`, execute in `src/engine.rs`

---

## Goal

Add a Raycast-style **action panel** so the selected result can expose secondary operations (copy path, reveal, trash, open with, run elevated, etc.) without stuffing every shortcut into the footer.

Target UX (from product notes): bottom-right becomes **Actions / Options** for the current item; Settings moves elsewhere (e.g. keep `Ctrl+,` and/or a settings entry inside the panel / empty-state).

---

## Progress tracker

### Research

- [x] Survey Raycast action model (primary Enter + `⌘K` panel + pinned shortcuts)
- [x] Survey Windows-oriented actions (run as admin, uninstall) vs Linux reality
- [x] Map Linux tools available on this host (`xdg-open`, `gio`, `pkexec`, `wl-copy`, Dolphin, `hyprctl`)
- [x] Inventory Blink’s current `Action` / footer / preview surface
- [x] Prioritize P0–P3 recommend / defer / skip
- [ ] User sign-off on Phase 1 scope (optional)

### Phase 1 — Panel skeleton + high-value file actions

- [x] `Ctrl+K` / footer **Actions** chip opens panel
- [x] Context-aware action list by `ResultKind` (file / folder / app / calc)
- [x] **Copy path**
- [x] **Copy name**
- [x] **Reveal in file manager** (Dolphin `--select` when available)
- [x] **Move to trash** (`gio trash` + confirm for folders)
- [x] Wire execute + hide launcher where appropriate
- [x] Refresh list / index after destructive ops (trash)

### Phase 2 — Open With + app location

- [ ] **Open with…** (GTK `AppChooser` / GIO MIME apps)
- [ ] Apps: **Reveal .desktop / install location**
- [ ] Apps: **Copy name** / **Copy desktop path**
- [ ] Preview toggle polish (Quick Look-ish; panel already exists)

### Phase 3 — Elevated + optional process control

- [ ] **Run elevated** via `pkexec` (clear label + warning; not default)
- [ ] Optional: **Quit app** via Hyprland (`hyprctl clients`) — fragile, behind flag or DE detect

### Phase 4 — Later / only if still requested

- [ ] Compress (zip/tar)
- [ ] Properties sheet (or richer preview metadata)
- [ ] Paste result into focused window (`wtype` / `ydotool` — Hyprland-fragile)
- [ ] Package-aware **Uninstall** / “open in package manager” (pacman/flatpak/snap only when detected)
- [ ] Rename / move / duplicate (file-manager scope creep)

### Explicitly out of scope (for now)

- [ ] Generic uninstall for curl/AppImage/`/opt` installs
- [ ] Run as different user
- [ ] AirDrop / share sheet
- [ ] Blind Windows “Run as administrator” copy without PolicyKit UX

---

## Raycast model (reference)

| Layer | macOS Raycast | Blink today | Blink target |
|-------|---------------|-------------|--------------|
| Primary | `↵` | Enter + footer primary label | Keep |
| Secondary panel | `⌘K` / Actions button | Missing | `Ctrl+K` + Actions chip |
| Pinned shortcuts | `⌘↵`, `⌘C`, custom | Terminal, Copy, Settings chips | Keep frequent pins; rest in panel |
| Context | Per item / extension | Partial (`update_footer`) | `actions_for(item)` |

Raycast is **not** primarily a Windows admin menu. Core value is: Open, Open With, Show in Finder, Copy Path/Name, Trash, Quick Look, plus extension actions. Admin/uninstall are more PowerToys/Windows-flavored.

---

## Blink baseline (as of 2026-07-21)

### `Action` enum

```text
LaunchApp { exec, terminal, desktop_path }
OpenPath(PathBuf)
OpenTerminal(PathBuf)
Copy(String)
SetQuery(String)
OpenSettings
RevealPath(PathBuf)   # Phase 1
TrashPath(PathBuf)    # Phase 1
```

### Footer

| Chip / key | Behavior |
|------------|----------|
| Primary (Enter) | Open / Copy Result / Use Scope · Drag for file/folder/app |
| Terminal | `Ctrl+Alt+Enter` — folders/files |
| Copy | `Ctrl+C` — calc/conversion + `Action::Copy` |
| Settings | `Ctrl+,` |
| **Actions** | `Ctrl+K` — secondary panel (Phase 1) |
| Copy path | `Ctrl+Shift+C` |
| Reveal | `Ctrl+Shift+R` |
| DnD | Files, folders, apps (desktop path) |

### Already strong

- Open via `xdg-open` / config `open_with`
- Terminal at path
- Clipboard (`wl-copy` path in engine)
- Side preview panel (partial Quick Look)
- Drag-out

### Host tools confirmed

| Tool | Path / note |
|------|-------------|
| `xdg-open` | `/usr/bin/xdg-open` |
| `gio` | trash + file ops |
| `pkexec` | elevated runs |
| `wl-copy` | Wayland clipboard |
| Dolphin | default `inode/directory` handler |
| `hyprctl` | Hyprland window control |

---

## Action matrix

Legend: **Rec** = recommend · **Eff** = effort · **Ph** = phase

### Files & folders

| Action | Linux approach | Eff | Ph | Rec | Status |
|--------|----------------|-----|----|-----|--------|
| Open | existing `OpenPath` | — | — | done | done |
| Open Terminal Here | existing `OpenTerminal` | — | — | done | done |
| Copy Path | `copy_to_clipboard(path)` | L | 1 | yes | done |
| Copy Name | basename → clipboard | L | 1 | yes | done |
| Reveal in file manager | `dolphin --select` / `xdg-open` parent | L–M | 1 | yes | done |
| Move to Trash | `gio trash` + confirm | L | 1 | yes | done |
| Open With… | GTK AppChooser / GIO | M | 2 | yes | todo |
| Quick Look | existing preview + toggle | L–M | 2 | yes | partial |
| Compress | `zip`/`tar` | M | 4 | later | todo |
| Properties | stat + dialog / preview | M | 4 | later | todo |
| Rename / Move | fs + reindex | M–H | 4 | later | todo |

### Apps

| Action | Linux approach | Eff | Ph | Rec | Status |
|--------|----------------|-----|----|-----|--------|
| Open | existing `LaunchApp` | — | — | done | done |
| Reveal location | parent of `desktop_path` or Exec | L | 2 | yes | done (desktop file) |
| Copy Name | title → clipboard | L | 2 | yes | done |
| Copy desktop path | clipboard | L | 2 | yes | done |
| Run elevated | `pkexec` wrap Exec | M | 3 | optional | todo |
| Quit / Force Quit | `hyprctl` match class | M | 3 | optional | todo |
| Uninstall | pacman/flatpak/snap detect only | H | 4 | defer | todo |

### Calc / conversion / translate

| Action | Linux approach | Eff | Ph | Rec | Status |
|--------|----------------|-----|----|-----|--------|
| Copy Result | existing | — | — | done | done |
| Paste into focused window | `wtype`/`ydotool` | M–H | 4 | later | todo |
| Large Type | overlay label | L | 4 | optional | todo |

### Windows-style (user-mentioned)

| Action | Linux framing | Rec |
|--------|---------------|-----|
| Run as admin | **Run elevated** (`pkexec`), never silent default | Phase 3 optional |
| Uninstall | Package-aware only; no fake one-click for manual installs | Phase 4 / skip v1 |
| Open app location | Reveal `.desktop` / install dir | Phase 2 |

---

## Proposed UX

```text
[ ↵ Open · Drag ]  │  [ Terminal  Ctrl Alt ↵ ]  │  [ Actions  Ctrl K ]  │  …
```

- **Ctrl+K** or Actions chip → popover/list of secondary actions for selection
- ↑/↓ + Enter to run; Esc closes
- Footer Settings chip moves or becomes secondary (product: options on item first)
- Destructive actions (trash) confirm; elevated actions warn

### Suggested API shape (implementation notes)

```text
// engine or providers
fn actions_for(item: &SearchResult) -> Vec<ActionSpec>
// ActionSpec { id, label, shortcut, kind, destructive }

// extend Action or parallel SecondaryAction:
CopyPath | CopyName | RevealPath | TrashPath | OpenWith | LaunchElevated | …
```

Files to touch (expected):

| Area | Files |
|------|--------|
| Action model | `src/providers/mod.rs` |
| Execute | `src/engine.rs`, `src/providers/files/mod.rs`, `src/providers/apps.rs` |
| Footer | `src/ui/footer.rs`, `src/ui/mod.rs` |
| New UI | `src/ui/action_panel.rs` (proposed) |
| Style | `src/ui/style.css` |

---

## Rollout checklist (when implementing)

1. [ ] Design `ActionSpec` + `actions_for` without UI
2. [ ] Action panel widget (list + keyboard)
3. [ ] Hook Ctrl+K + footer chip
4. [ ] Implement Phase 1 executors
5. [ ] Post-trash UI refresh
6. [ ] Phase 2 Open With + app reveal
7. [ ] Manual test matrix (below)
8. [ ] Release build + daemon restart

### Manual test matrix

| Case | Expected |
|------|----------|
| File selected → Ctrl+K | Open, Reveal, Copy path/name, Trash, Terminal |
| Folder selected → Trash | Confirm then gone from list / trash can |
| App (pacman) → Reveal | File manager on `.desktop` or install dir |
| App (manual `/opt`) → Reveal | Still works via desktop path |
| Calc result → panel | Copy result (and little else) |
| Reveal | Dolphin selects the file when possible |
| Copy path | `wl-paste` matches absolute path |
| Elevated (if built) | PolicyKit prompt; cancel is safe |
| Esc / click away | Panel closes; launcher stays usable |

---

## Decision log

| Date | Decision |
|------|----------|
| 2026-07-21 | Research complete; Phase 1 = panel + copy path/name + reveal + trash |
| 2026-07-21 | Uninstall not core v1 (multi-source package problem) |
| 2026-07-21 | “Run as admin” → optional **Run elevated** (`pkexec`), not default open |
| 2026-07-21 | Prefer Dolphin `--select` for reveal on this host; fallback `xdg-open` parent |
| 2026-07-21 | Phase 1 shipped: `Ctrl+K` panel, copy path/name, reveal, trash + confirm |
| 2026-07-21 | Reuse existing preview as Quick Look; don’t rebuild |

---

## Open questions

- [ ] Exact Settings placement after Actions takes bottom-right (empty-state only vs always `Ctrl+,` only vs panel entry)
- [ ] Trash: always confirm, or only directories / non-empty?
- [ ] Should primary footer still show Terminal when Actions panel also lists it?
- [ ] Flatpak/Snap-specific actions in v1 or later?

---

## Changelog (this audit)

| Date | Note |
|------|------|
| 2026-07-21 | Initial audit from Raycast/Linux research + codebase baseline |
| 2026-07-21 | Phase 1 implementation: Action panel + copy/reveal/trash |
