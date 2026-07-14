use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

pub fn socket_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    dir.join("blink.sock")
}

/// Fast path for hotkey: tell the daemon to toggle. Returns true if delivered.
pub fn request_toggle() -> bool {
    let path = socket_path();
    for _ in 0..5 {
        if let Ok(mut stream) = UnixStream::connect(&path) {
            let _ = stream.set_write_timeout(Some(Duration::from_millis(100)));
            if stream.write_all(b"toggle\n").is_ok() {
                return true;
            }
        }
        thread::sleep(Duration::from_millis(20));
    }
    false
}

/// Listen for toggle requests; callback is invoked on the calling thread's channel.
pub fn spawn_listener(on_toggle: impl Fn() + Send + 'static + Clone) {
    let path = socket_path();
    let _ = std::fs::remove_file(&path);

    let listener = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("blink: ipc bind failed: {e}");
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
            }
        }
    });
}
