//! End-to-end CLI gating for the IPC toggle path (cold-start plan W2).
//!
//! These run the real binary under a private `XDG_RUNTIME_DIR` (and a bogus
//! D-Bus address) so they can never reach the developer's live session or
//! leave state behind in it.

#![cfg(unix)]

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// Result of one isolated `hark` invocation.
struct Run {
    /// `None` when the process had to be killed at the timeout.
    status: Option<ExitStatus>,
    stdout: String,
    elapsed: Duration,
}

struct Lab {
    dir: PathBuf,
}

impl Lab {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "hark-cli-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("create lab dir");
        Self { dir }
    }

    fn socket(&self) -> PathBuf {
        self.dir.join("hark.sock")
    }

    /// Run `hark` with `args` in the isolated env, killing it after `timeout`.
    fn run(&self, args: &[&str], timeout: Duration) -> Run {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_hark"));
        cmd.args(args)
            .env("XDG_RUNTIME_DIR", &self.dir)
            .env("XDG_CACHE_HOME", self.dir.join("cache"))
            .env("XDG_CONFIG_HOME", self.dir.join("config"))
            .env("XDG_STATE_HOME", self.dir.join("state"))
            .env("XDG_DATA_HOME", self.dir.join("data"))
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("DISPLAY")
            // A private (nonexistent) bus: even if the process tried to
            // register, it could not activate the developer's live instance.
            .env(
                "DBUS_SESSION_BUS_ADDRESS",
                "unix:path=/nonexistent-hark-test",
            )
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().expect("spawn hark");

        let mut stdout = child.stdout.take().expect("stdout pipe");
        let out_reader = std::thread::spawn(move || {
            let mut s = String::new();
            stdout.read_to_string(&mut s).ok();
            s
        });
        let mut stderr = child.stderr.take().expect("stderr pipe");
        let err_reader = std::thread::spawn(move || {
            let mut s = String::new();
            stderr.read_to_string(&mut s).ok();
            s
        });

        let t0 = Instant::now();
        let status = loop {
            match child.try_wait().expect("try_wait") {
                Some(status) => break Some(status),
                None if t0.elapsed() >= timeout => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                None => std::thread::sleep(Duration::from_millis(2)),
            }
        };
        let stdout = out_reader.join().expect("stdout reader");
        let _stderr = err_reader.join().expect("stderr reader");
        Run {
            status,
            stdout,
            elapsed: t0.elapsed(),
        }
    }
}

impl Drop for Lab {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// `--help` must print hark's option summary and exit 0 without creating a
/// `hark.sock` (the listener is skipped because GTK exits without unwinding).
#[test]
fn help_never_leaves_a_socket_behind() {
    let lab = Lab::new("help");
    let run = lab.run(&["--help"], Duration::from_secs(10));

    assert_eq!(
        run.status.and_then(|s| s.code()),
        Some(0),
        "help must exit 0 (killed after {:?}?)",
        run.elapsed
    );
    assert!(
        run.stdout.contains("--daemon"),
        "hark's own help section must be printed, got: {}",
        run.stdout
    );
    assert!(
        !lab.socket().exists(),
        "`hark --help` left a stale socket at {}",
        lab.socket().display()
    );
}

/// With a live daemon, `--help` must not deliver a toggle before printing
/// help. Needs AF_UNIX bind (may be denied in sandboxed CI).
#[test]
#[ignore = "unix socket bind"]
fn help_does_not_toggle_a_live_daemon() {
    use std::os::unix::net::UnixListener;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let lab = Lab::new("help-toggle");
    let socket = lab.socket();
    let listener = UnixListener::bind(&socket).expect("bind fake daemon");
    let got_toggle = Arc::new(AtomicBool::new(false));
    let flag = got_toggle.clone();
    std::thread::spawn(move || {
        while let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0u8; 32];
            if let Ok(n) = stream.read(&mut buf) {
                if buf[..n].windows(6).any(|w| w == b"toggle") {
                    flag.store(true, Ordering::SeqCst);
                }
            }
        }
    });

    let run = lab.run(&["--help"], Duration::from_secs(10));
    assert!(
        run.status.is_some(),
        "help must exit on its own (killed after {:?})",
        run.elapsed
    );
    // The toggle (if any) is sent before GTK runs; the process has exited, but
    // the accept thread may still be draining — give it a moment.
    let deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < deadline && !got_toggle.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !got_toggle.load(Ordering::SeqCst),
        "`hark --help` toggled a live daemon"
    );
    assert!(
        socket.exists(),
        "a help run must not disturb the live daemon's socket"
    );
}
