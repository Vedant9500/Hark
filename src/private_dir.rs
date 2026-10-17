//! User-private directory trust (audit N1 threat model).
//!
//! Shared by the config stores and the IPC listener. Also compiled into the
//! GTK-free `hark-hotkey` stub by path include (together with `ipc.rs`), so
//! this file must stay std-only: no `crate::` references, no GTK.

use std::fs;
use std::path::Path;

/// True when `dir` is owned by this user and locked to 0700 — the same
/// guard fx.rs/translate.rs apply. Any store that can fall back to shared
/// `/tmp` space must refuse to persist through a directory someone else
/// pre-created.
#[cfg(unix)]
pub(crate) fn dir_is_trusted(dir: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Ok(m) = fs::metadata(dir) else {
        return false;
    };
    m.is_dir() && m.mode() & 0o777 == 0o700 && m.uid() == current_euid().unwrap_or(u32::MAX)
}

#[cfg(unix)]
pub(crate) fn current_euid() -> Option<u32> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("Uid:"))?;
    line.split_whitespace().nth(2)?.parse().ok()
}

#[cfg(not(unix))]
pub(crate) fn dir_is_trusted(_dir: &Path) -> bool {
    true
}
