//! `hark-hotkey` — tiny GTK-free hotkey client (cold-start plan W4).
//!
//! Linking the full GTK stack just to write seven bytes to a Unix socket
//! costs ~19–20 ms of loader work on *every* press. This stub serves the
//! plain toggle from a small binary that links only libc, and hands
//! everything else — cold start, `--search`, `--daemon`, `update`, `--help`,
//! custom GTK flags — to the real `hark` binary via `exec`, so behavior and
//! exit codes are identical to invoking `hark` itself.
//!
//! Bind it instead of `hark`:
//!   bind = ALT, A, exec, hark-hotkey
//!
//! Must NOT reference the `hark` lib (that would link GTK): the IPC client
//! and the trust check are shared by path include and stay std-only.

// The shared modules carry the whole IPC surface, but the stub only uses the
// client half (`request_toggle`); the listener half is intentionally dead here.
#[allow(dead_code)]
#[path = "../ipc.rs"]
mod ipc;
#[allow(dead_code)]
#[path = "../private_dir.rs"]
mod private_dir;

use std::ffi::OsString;
use std::path::PathBuf;

fn main() {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();

    // Only the no-argument toggle is served here. Any argument is hark's
    // real CLI surface (parsed in main.rs): forward it untouched.
    if args.is_empty() && ipc::request_toggle() {
        return;
    }

    exec_full(&args);
}

/// Hand over to the full `hark` binary, preserving argv and (by replacing
/// this process) its exit code. The binary is expected next to this stub —
/// true for `target/release`, `~/.local/bin`, `/usr/bin`, and tarball
/// layouts.
fn exec_full(args: &[OsString]) -> ! {
    use std::os::unix::process::CommandExt;

    let exe = full_binary();
    let err = std::process::Command::new(&exe).args(args).exec();
    // `exec` only returns on failure (missing binary, EACCES, …).
    eprintln!("hark-hotkey: cannot execute {}: {err}", exe.display());
    std::process::exit(127);
}

/// `.../bin/hark` next to the running stub (`.../bin/hark-hotkey`). A bare
/// `hark` (PATH lookup) is the last resort if `current_exe` fails.
fn full_binary() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|dir| dir.join("hark")))
        .unwrap_or_else(|| PathBuf::from("hark"))
}
