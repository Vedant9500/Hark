# Blink — Release & AUR publishing roadmap

> **Status:** research / planning only. This is a **roadmap**, not a list of finished
> tasks. Each section is designed to be executed as its own session; tick items as
> they land. Last reviewed: 2026-08-11.

Goal: clean up and optimize the codebase, then publish **`blink-launcher` to the AUR**.
(The npm idea was dropped — a native GTK4 Rust app doesn't belong on npm. AUR is the
target.)

The plan is split into **two phases**: **Phase 1 codebase hygiene** (anything facing a
public release should be green and reproducible first) and **Phase 2 AUR publishing**.

---

## Phase 1 — Codebase hygiene & CI (do first)

Run in whatever order works, one session per bullet. Everything here is about making
the repo deterministic, warning-free, and dependency-audited *before* anyone else builds it.

### 1.1 Pin the toolchain

- [x] Add `rust-toolchain.toml` (`channel = "stable"`). Push: `rust-version = "1.70"` set in `Cargo.toml` (gtk4/glib floor).
  - This is the single source of truth for CI + the PKGBUILD's
    `RUSTUP_TOOLCHAIN=stable`. Without it the version can drift between your machine,
    CI, and a user's `makepkg`.
  - Optionally record the MSRV in `Cargo.toml` (`rust-version = "…"`).
  - Reference: [RustProjectPrimer CI][rpp-ci] (toolchain pinning), Effective Rust.

### 1.2 Warning-free everywhere (`-Dwarnings`)

Currently **7 clippy warnings** surface with `--all-features` (CI only gates
`--features layer-shell`). CI should gate the same flag set you lint locally.

- [x] Fix the 7 clippy warnings:
  - `src/providers/apps.rs:43` — public `len` with no `is_empty` → add `is_empty`.
  - `src/bench.rs:114` — `f64 -> f64` redundant cast.
  - `src/config.rs` — `field_reassign_with_default` × 3 (test code).
  - Bonus: `is_multiple_of` (stable 1.87) replaced with `% 250 == 0` to fit MSRV 1.70.
- [x] CI: switch from `cargo clippy --features layer-shell -- -D warnings` to
      `cargo clippy --all-targets --all-features -- -D warnings` so the gate matches
      local dev (see changelog history: this exact mismatch is why 7 slipped through).
- [x] Add `cargo fmt --check` (already present), `cargo test --all-features`.

### 1.3 Dependency audit & supply chain

- [x] Run `cargo audit` → fix known advisories / yanked crates.
      Fixed: `event-listener 5.4.1` (RUSTSEC-2026-0221) → 5.4.2 via `cargo update -p event-listener`.
- [x] Add `cargo deny` (`deny.toml`): license allowlist (MIT/Apache-2.0/Unicode-3.0/
      CDLA-Permissive-2.0 etc.), advisory check (yanked = deny), source restrictions
      (crates.io only), duplicate-version warning.
  - Note: GTK4 bindings chain is heavy; don't force zero-duplicates day one — triage
    into *fix now / defer with expiry / consciously accept*.
  - Triage: only dupes are `windows-*` (dead on Linux target) — acceptable at warn.
- [x] Wire `cargo audit` + `cargo deny` into CI (thorough tier, weekly `schedule` trigger).
      Added `.github/workflows/security.yml` (weekly cron + manual dispatch).
- [x] Check for unused deps (`cargo machete` / `cargo udeps`) — drop anything not used.
      `cargo machete`: no unused deps.

### 1.4 Reproducibility checks

- [x] Keep `Cargo.lock` committed (already tracked — good; Rust apps, not libs, should
      commit it). CI asserts with `cargo fetch --locked`; release build also uses
      `cargo build --release --locked`.
- [x] Decide release tooling. **Decision: keep existing** `./scripts/package-release.sh`
      + `release.yml` — already builds tarball + installer + SHA256SUMS + optional .deb
      on `v*` tags, wired to GitHub Releases. `cargo-dist`/`cargo-release` churn not worth
      it for single-platform Linux binary. Minimum satisfied: tag `v*` builds a release.

### 1.5 Verify before calling it done

- [x] `cargo fmt --check` — clean.
- [x] `cargo clippy --all-targets --all-features -- -D warnings` — clean.
- [x] `cargo test --all-features` — green (128 tests).
- [x] `cargo audit` / `cargo deny` — clean (event-listener bumped; deny all four checks ok).
- [x] CI is green on `main` and gates everything above on PR.

---

## Phase 2 — AUR publishing (the actual goal)

The `packaging/aur/PKGBUILD` already exists and mostly follows the ArchWiki Rust
guidelines (ferent `cargo fetch --locked` / `--frozen --release` pattern). A handful of
concrete things must be fixed before it's submission-ready.

### 2.1 Fix PKGBUILD correctness issues

- [ ] **Replace `sha256sums=('SKIP')` with real checksums.**
  Run `makepkg -g` / `updpkgsums` against the tagged source tarball and commit the real
  hashes. `SKIP` is a red flag in AUR review — reviewers expect a pinned checksum.
- [ ] **Drop `--release` from `check()`.**
  Arch guidelines say use `cargo test --frozen` (no `--release`) so overflow checks and
  `debug_assert!` stay on. Tests don't ship, so release-mode testing adds nothing.
  → `cargo test --frozen` (keep `--features layer-shell`, as now).
- [ ] Reconsider `arch=('aarch64')`: fine to keep but you must **also build-test it**
  (cross target) or drop it — don't advertise aarch64 you can't verify.
- [ ] `makedepends=('cargo')` — the guideline names `cargo` (the `rust` meta handles
      rustc); this is already correct.
- [ ] Confirm `depends=('gtk4' 'glib2')` + `optdepends=('gtk4-layer-shell')` match
      runtime needs (layer-shell is optional — correct as optdepends).

### 2.2 Generate and commit `.SRCINFO`

- [ ] The AUR requires a **`.SRCINFO`** file alongside `PKGBUILD` — it is currently
      **absent**. Generate with:
      ```sh
      cd packaging/aur && makepkg --printsrcinfo > .SRCINFO
      ```
- [ ] Commit `.SRCINFO` **every time** `PKGBUILD` metadata changes (pkgver, deps, …) or
      the AUR will reject the push / show stale versions.

### 2.3 Validate locally before submitting

- [ ] `namcap PKGBUILD` and `namcap <built>.pkg.tar.zst` — lint for packaging errors.
- [ ] Build in a clean chroot (`devtools`, e.g. `ccm s` / `extra-x86_64-build`) to catch
      missing deps that only appear without your local packages. This is the #1 missed
      dependency detector.
- [ ] Confirm the tag in `source=` (currently `v$pkgver`) matches a **real pushed GitHub
      tag** — the tarball URL must resolve, or the build fails at download time.

### 2.4 Submit to the AUR

Prereqs (outside the repo, one-time):

- [ ] Create an account at **aur.archlinux.org** (register + enable 2FA + SSH).
- [ ] Create a **dedicated SSH key** (e.g. `ssh-keygen -t ed25519 -f ~/.ssh/id_aur`),
      add the public key to your AUR profile, configure `~/.ssh/config`:
      ```
      Host aur.archlinux.org
          User aur
          IdentityFile ~/.ssh/id_aur
      ```
- [ ] Pre-check: search the AUR for existing `blink` packages — do not submit a
      duplicate; if one exists (left by someone else), adopt or disambiguate the name.

Publishing workflow (either AUR git directly, or `aurpublish` helper which auto-gen
`.SRCINFO`):

```
# direct AUR repo (pkgname is the AUR package name)
git clone ssh://aur@aur.archlinux.org/blink-launcher.git
cp packaging/aur/PKGBUILD .
makepkg --printsrcinfo > .SRCINFO
git add PKGBUILD .SRCINFO
git commit -m "initial: blink-launcher v0.1.0"
git push origin master    # AUR only accepts pushes to master
```
- [ ] The AUR git repo uses **`master`** (not `main`) — rename your branch if needed.

### 2.5 After publishing (maintenance loop)

- [ ] On each new release: bump `pkgver`, `makepkg -g` → update checksums, regen
      `.SRCINFO`, bump `pkgrel=1`, commit, push to AUR.
  - Consider `aurpublish` (hooks auto-regenerate `.SRCINFO`) to remove the manual step.
  - Consider automation: `nvchecker` / GitHub Action that flags new tags for the PKGBUILD.
- [ ] Keep `pkgrel` monotonic within a `pkgver` (reset to 1 only on a new upstream release).

---

## Design decisions / open questions

- **Package name:** repo is `blink`, AUR name is `blink-launcher` (naming clash
  avoidance). Confirm this is intentional and not confusing for users searching "blink".
- **crates.io:** `Cargo.toml` has `version = "0.1.0"` and a clean LICENSE/README but the
  crate is a **binary app**, so `cargo publish` to crates.io is optional. AUR is the
  distribution channel; crates.io only matters if you want `cargo install blink`. Not a
  blocker — flag it in case you want it later.
- **`release.yml`:** already builds `.deb` + tarball on `v*` tags — that's the release
  artifact source the AUR `source=` expects. Make sure the tag it reads matches the
  `pkgver`/tag convention (`v0.1.0`).

---

## References

- ArchWiki — [Rust package guidelines][wiki-rust]
- ArchWiki — [AUR submission guidelines][wiki-aur-submit]
- ArchWiki — [Arch User Repository][wiki-aur]
- [reproducible-builds.org — Rust][repro-rust]
- [Rust Project Primer — CI][rpp-ci]
- Cargo Book — [Continuous Integration][cargo-ci]
- `aurpublish` — [man page][aurpublish]

[wiki-rust]: https://wiki.archlinux.org/title/Rust_package_guidelines
[wiki-aur-submit]: https://wiki.archlinux.org/title/AUR_submission_guidelines
[wiki-aur]: https://wiki.archlinux.org/title/Arch_User_Repository
[repro-rust]: https://reproducible-builds.org/docs/rust/
[rpp-ci]: https://rustprojectprimer.com/ci/index.html
[cargo-ci]: https://doc.rust-lang.org/cargo/guide/continuous-integration.html
[aurpublish]: https://man.archlinux.org/man/extra/aurpublish/aurpublish.1.en