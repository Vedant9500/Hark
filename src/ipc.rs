use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

pub fn socket_path() -> PathBuf {
    if let Some(dir) = dirs::runtime_dir() {
        return dir.join("blink.sock");
    }
    // No XDG_RUNTIME_DIR (rare on non-Wayland sessions): fail closed into a
    // user-private cache dir instead of the shared temp dir, so other local
    // users cannot occupy or race the socket path. `spawn_listener` creates
    // this directory with mode 0700.
    dirs::cache_dir()
        .or_else(dirs::home_dir)
        .map(|d| d.join("blink"))
        .unwrap_or_else(|| std::env::temp_dir().join("blink"))
        .join("blink.sock")
}

/// Fast path for hotkey: tell the daemon to toggle. Returns true if delivered.
pub fn request_toggle() -> bool {
    let path = socket_path();
    // No daemon socket at all → nothing to toggle. Skipping the retry loop
    // saves ~100ms of cold-start time (5 × 20ms sleeps) when the connect
    // failure is instant ENOENT. The retries below only matter for the
    // stale-socket race (daemon died, fresh daemon rebinding), which still
    // runs because `exists()` is true for a leftover socket file.
    if !path.exists() {
        return false;
    }
    for _ in 0..5 {
        match UnixStream::connect(&path) {
            Ok(mut stream) => {
                let _ = stream.set_write_timeout(Some(Duration::from_millis(100)));
                let _ = stream.set_read_timeout(Some(Duration::from_millis(100)));
                if stream.write_all(b"toggle\n").is_err() {
                    thread::sleep(Duration::from_millis(20));
                    continue;
                }
                // Optional ack from healthy daemons; older daemons may close without reply.
                let mut buf = [0u8; 8];
                match stream.read(&mut buf) {
                    Ok(n) if n > 0 => {
                        let msg = std::str::from_utf8(&buf[..n]).unwrap_or("").trim();
                        if msg.starts_with("ok") || msg.is_empty() {
                            return true;
                        }
                    }
                    // EOF / timeout after successful write — still treat as delivered
                    // (listener processed toggle without writing ack).
                    Ok(_) | Err(_) => return true,
                }
            }
            Err(_) => {
                thread::sleep(Duration::from_millis(20));
            }
        }
    }
    false
}

/// Listen for toggle requests; `on_toggle` runs on the listener thread
/// (caller should bounce work onto the GTK main loop via a channel).
pub fn spawn_listener(on_toggle: impl Fn() + Send + 'static + Clone) {
    let path = socket_path();

    // Ensure the socket directory exists and is user-private. XDG_RUNTIME_DIR
    // is already 0700 by spec; the cache-dir fallback needs to be created and
    // locked down here.
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
        }
    }

    let listener = match bind_socket(&path) {
        Some(l) => l,
        None => {
            eprintln!("blink: ipc bind failed for {}", path.display());
            return;
        }
    };

    // Restrict to user
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }

    thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut stream) = conn else { continue };
            let mut buf = [0u8; 64];
            let n = stream.read(&mut buf).unwrap_or(0);
            if n == 0 {
                continue;
            }
            let msg = std::str::from_utf8(&buf[..n]).unwrap_or("").trim();
            if msg == "toggle" {
                on_toggle();
                let _ = stream.write_all(b"ok\n");
            }
        }
    });
}

/// Bind the IPC socket. If the path exists but is stale (no live peer), remove
/// and rebind once so a new daemon can take over after a crash.
fn bind_socket(path: &std::path::Path) -> Option<UnixListener> {
    match UnixListener::bind(path) {
        Ok(l) => return Some(l),
        Err(e) => {
            // Path busy or leftover socket file.
            if path.exists() {
                // Live daemon already listening → leave it alone.
                if UnixStream::connect(path).is_ok() {
                    eprintln!("blink: ipc already active at {} ({e})", path.display());
                    return None;
                }
                // Stale socket (connect fails) — reclaim.
                let _ = std::fs::remove_file(path);
                match UnixListener::bind(path) {
                    Ok(l) => return Some(l),
                    Err(e2) => {
                        eprintln!("blink: ipc rebind failed: {e2}");
                        return None;
                    }
                }
            }
            eprintln!("blink: ipc bind failed: {e}");
            None
        }
    }
}

#[cfg(test)]
mod ipc_tests {
    use super::*;

    #[test]
    fn socket_path_uses_runtime_dir_or_tmp() {
        let p = socket_path();
        assert!(p.ends_with("blink.sock"));
    }

    /// Needs AF_UNIX bind (may be denied in sandboxed CI).
    #[test]
    #[ignore = "unix socket bind"]
    fn stale_socket_is_reclaimed() {
        let dir = std::env::temp_dir().join(format!(
            "blink-ipc-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("blink.sock");
        let _ = std::fs::remove_file(&path);
        let reclaimed = bind_socket(&path);
        assert!(reclaimed.is_some(), "should bind clean path");
        drop(reclaimed);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[ignore = "unix socket bind"]
    fn toggle_roundtrip_with_ack() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let dir = std::env::temp_dir().join(format!(
            "blink-ipc-rt-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test.sock");
        let _ = std::fs::remove_file(&path);

        let hits = Arc::new(AtomicUsize::new(0));
        let hits_c = hits.clone();
        let listener = UnixListener::bind(&path).expect("bind");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 64];
                let n = stream.read(&mut buf).unwrap_or(0);
                let msg = std::str::from_utf8(&buf[..n]).unwrap_or("").trim();
                if msg == "toggle" {
                    hits_c.fetch_add(1, Ordering::SeqCst);
                    let _ = stream.write_all(b"ok\n");
                }
            }
        });
        thread::sleep(Duration::from_millis(30));

        let mut stream = UnixStream::connect(&path).expect("connect");
        stream.write_all(b"toggle\n").unwrap();
        let mut buf = [0u8; 8];
        let n = stream.read(&mut buf).unwrap_or(0);
        assert!(std::str::from_utf8(&buf[..n]).unwrap_or("").contains("ok"));
        assert_eq!(hits.load(Ordering::SeqCst), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
