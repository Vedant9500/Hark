//! Test-only helpers.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Unique private scratch directory under the system temp dir, **removed on
/// drop**.
///
/// Fixtures used to `create_dir_all` a `hark-…-<pid>-<n>` dir and never
/// delete it. A fresh PID per `cargo test` run meant ~60 empty dirs
/// leaked into `/tmp` every run. The dir is 0700 because
/// `write_private_file` refuses untrusted parents.
///
/// Stores that flush on `Drop` must own their `ScratchDir` as a field (fields
/// drop after the owner's `Drop::drop`). Otherwise the final flush
/// re-creates the directory through `write_private_file`'s
/// `create_dir_all`.
#[derive(Debug)]
pub(crate) struct ScratchDir {
    path: PathBuf,
}

impl ScratchDir {
    pub(crate) fn new(tag: &str) -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("hark-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        let _ = std::fs::create_dir_all(&path);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700));
        }
        Self { path }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scratch_dir_is_private_unique_and_removed_on_drop() {
        let a = ScratchDir::new("scratch-selftest");
        let b = ScratchDir::new("scratch-selftest");
        assert_ne!(a.path(), b.path());
        assert!(a.path().is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(a.path()).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700);
        }
        std::fs::write(a.path().join("f.json"), b"{}").unwrap();
        let (pa, pb) = (a.path().to_path_buf(), b.path().to_path_buf());
        drop(a);
        drop(b);
        assert!(!pa.exists(), "non-empty dir removed too");
        assert!(!pb.exists());
    }
}
