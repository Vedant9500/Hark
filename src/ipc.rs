use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

pub fn socket_path() -> PathBuf {
    if let Some(dir) = dirs::runtime_dir() {
        return dir.join("hark.sock");
    }
    // No XDG_RUNTIME_DIR (rare on non-Wayland sessions): fail closed into a
    // user-private cache dir instead of the shared temp dir, so other local
    // users cannot occupy or race the socket path. `spawn_listener` creates
    // this directory with mode 0700.
    dirs::cache_dir()
        .or_else(dirs::home_dir)
        .map(|d| d.join("hark"))
        .unwrap_or_else(|| std::env::temp_dir().join("hark"))
        .join("hark.sock")
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
                // Optional ack from healthy daemons; older daemons may close without
                // reply. Everything past a successful write counts as delivered —
                // the listener answers "ok\n" or nothing. An unexpected payload must
                // NOT fall through to a rewrite: a second "toggle" would immediately
                // undo the first.
                let mut buf = [0u8; 8];
                let _ = stream.read(&mut buf);
                return true;
            }
            Err(_) => {
                thread::sleep(Duration::from_millis(20));
            }
        }
    }
    false
}

/// Listen for toggle requests; `on_toggle` runs INLINE on the listener thread,
/// so handlers must stay trivial — bounce real work onto the GTK main loop via
/// a channel (see main.rs). Per-connection reads time out after 2s and ack
/// writes after 1s, so a silent client cannot park the listener and wedge
/// later toggles.
pub fn spawn_listener(on_toggle: impl Fn() + Send + 'static + Clone) {
    spawn_listener_at(&socket_path(), on_toggle);
}

/// Bind `path` and serve toggles. Split from [`spawn_listener`] so tests can
/// exercise the accept/handler logic on a scratch socket.
pub fn spawn_listener_at(path: &std::path::Path, on_toggle: impl Fn() + Send + 'static + Clone) {
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

    let listener = match bind_socket(path) {
        Some(l) => l,
        None => {
            eprintln!("hark: ipc bind failed for {}", path.display());
            return;
        }
    };

    // Permissions were locked to 0600 on the temp node before the atomic
    // rename in `bind_socket` — nothing to fix up here.

    thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(stream) = conn else { continue };
            // Handle each client on its own short-lived thread: the accept
            // loop must never block on a slow/silent client, or one stalled
            // connection wedges every later hotkey toggle. The read timeout
            // below still bounds each handler thread's lifetime.
            let on_toggle = on_toggle.clone();
            thread::spawn(move || {
                let mut stream = stream;
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let mut buf = [0u8; 64];
                let n = stream.read(&mut buf).unwrap_or(0);
                if n == 0 {
                    return;
                }
                let msg = std::str::from_utf8(&buf[..n]).unwrap_or("").trim();
                if msg == "toggle" {
                    on_toggle();
                    let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
                    let _ = stream.write_all(b"ok\n");
                }
            });
        }
    });
}

/// Bind the IPC socket. If the path exists but is stale (no live peer), remove
/// and rebind once so a new daemon can take over after a crash.
///
/// `bind` creates the socket with mode `0777 & !umask`. Temporarily forcing
/// umask 077 prevents group/other connects during the bind→chmod window
/// (CWE-367); chmod 0600 is then checked before accepting clients.
fn bind_socket(path: &std::path::Path) -> Option<UnixListener> {
    #[cfg(unix)]
    fn bind_private(path: &std::path::Path) -> Option<UnixListener> {
        use std::os::unix::fs::PermissionsExt;

        unsafe extern "C" {
            fn umask(mask: u32) -> u32;
        }

        let old_umask = unsafe { umask(0o077) };
        let listener = UnixListener::bind(path);
        unsafe {
            umask(old_umask);
        }
        let listener = listener.ok()?;
        if std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).is_err() {
            let _ = std::fs::remove_file(path);
            return None;
        }
        Some(listener)
    }

    #[cfg(not(unix))]
    fn bind_private(path: &std::path::Path) -> Option<UnixListener> {
        UnixListener::bind(path).ok()
    }

    match bind_private(path) {
        Some(l) => Some(l),
        None => {
            // Path busy or leftover socket file.
            if path.exists() {
                // Live daemon already listening → leave it alone.
                if UnixStream::connect(path).is_ok() {
                    eprintln!("hark: ipc already active at {}", path.display());
                    return None;
                }
                // Stale socket (connect fails) — reclaim.
                let _ = std::fs::remove_file(path);
                bind_private(path).or_else(|| {
                    eprintln!("hark: ipc rebind failed at {}", path.display());
                    None
                })
            } else {
                eprintln!("hark: ipc bind failed at {}", path.display());
                None
            }
        }
    }
}

#[cfg(test)]
mod ipc_tests {
    use super::*;

    #[test]
    fn socket_path_uses_runtime_dir_or_tmp() {
        let p = socket_path();
        assert!(p.ends_with("hark.sock"));
    }

    /// Needs AF_UNIX bind (may be denied in sandboxed CI).
    #[test]
    #[ignore = "unix socket bind"]
    fn stale_socket_is_reclaimed() {
        let dir = std::env::temp_dir().join(format!(
            "hark-ipc-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("hark.sock");
        let _ = std::fs::remove_file(&path);
        let reclaimed = bind_socket(&path);
        assert!(reclaimed.is_some(), "should bind clean path");
        drop(reclaimed);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Needs AF_UNIX bind. The socket must be published with mode 0600 from
    /// the start (bind→chmod race fix: temp bind + chmod + atomic rename).
    #[test]
    #[ignore = "unix socket bind"]
    fn bound_socket_is_user_only() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!(
            "hark-ipc-mode-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("hark.sock");
        let _ = std::fs::remove_file(&path);

        let listener = bind_socket(&path).expect("bind");
        let mode = std::fs::metadata(&path)
            .expect("socket node exists")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "socket must be 0600 immediately after bind"
        );
        // And it still accepts connections (rename didn't break the listener).
        assert!(UnixStream::connect(&path).is_ok());
        drop(listener);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[ignore = "unix socket bind"]
    fn toggle_roundtrip_with_ack() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let dir = std::env::temp_dir().join(format!(
            "hark-ipc-rt-{}-{}",
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

    /// A client that connects and stalls must not wedge later toggles
    /// (slowloris fix: per-client handler threads off the accept loop).
    #[test]
    #[ignore = "unix socket bind"]
    fn stalled_client_does_not_wedge_later_toggles() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let dir = std::env::temp_dir().join(format!(
            "hark-ipc-slow-{}-{}",
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
        spawn_listener_at(&path, move || {
            hits_c.fetch_add(1, Ordering::SeqCst);
        });

        // Give the listener a moment to bind.
        let mut connected = false;
        for _ in 0..50 {
            if UnixStream::connect(&path).is_ok() {
                connected = true;
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        assert!(connected, "listener never bound");

        // Slowloris client: connect and send nothing.
        let _staller = UnixStream::connect(&path).expect("stall connect");
        thread::sleep(Duration::from_millis(100));

        // While the staller is silent, a normal toggle must still go through.
        let mut s2 = UnixStream::connect(&path).expect("toggle connect");
        s2.write_all(b"toggle\n").unwrap();
        s2.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut buf = [0u8; 8];
        let n = s2.read(&mut buf).unwrap_or(0);
        assert!(
            std::str::from_utf8(&buf[..n]).unwrap_or("").contains("ok"),
            "toggle wedged behind the stalling client"
        );
        assert_eq!(hits.load(Ordering::SeqCst), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
