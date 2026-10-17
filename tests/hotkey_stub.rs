//! `hark-hotkey` stub behavior (cold-start plan W4): the plain press toggles a
//! live daemon over the shared IPC protocol, and everything else `exec`s the
//! full `hark` binary with argv and exit code preserved.
//!
//! All runs use a private `XDG_*` env (plus a bogus D-Bus address) so they can
//! never reach the developer's live session or leave state behind in it.

#![cfg(unix)]

use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// One isolated stub invocation.
struct Run {
    /// `None` when the process had to be killed at the timeout.
    status: Option<ExitStatus>,
    stdout: String,
    stderr: String,
    elapsed: Duration,
}

struct Lab {
    dir: PathBuf,
}

impl Lab {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "hark-stub-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(dir.join("run")).expect("create lab dir");
        Self { dir }
    }

    fn runtime(&self) -> PathBuf {
        self.dir.join("run")
    }

    fn socket(&self) -> PathBuf {
        self.runtime().join("hark.sock")
    }

    /// Run `exe` with `args` in the isolated env, killing it after `timeout`.
    fn run(&self, exe: &Path, args: &[&str], timeout: Duration) -> Run {
        let mut cmd = Command::new(exe);
        cmd.args(args)
            .env("XDG_RUNTIME_DIR", self.runtime())
            .env("XDG_CACHE_HOME", self.dir.join("cache"))
            .env("XDG_CONFIG_HOME", self.dir.join("config"))
            .env("XDG_STATE_HOME", self.dir.join("state"))
            .env("XDG_DATA_HOME", self.dir.join("data"))
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("DISPLAY")
            .env(
                "DBUS_SESSION_BUS_ADDRESS",
                "unix:path=/nonexistent-hark-test",
            )
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().expect("spawn stub");

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
        Run {
            status,
            stdout: out_reader.join().expect("stdout reader"),
            stderr: err_reader.join().expect("stderr reader"),
            elapsed: t0.elapsed(),
        }
    }
}

impl Drop for Lab {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The stub itself, plus a fake `hark` next to it (the stub resolves the full
/// binary as a sibling). Returns `(stub, argv-log)`.
fn stub_with_fake_full(lab: &Lab) -> (PathBuf, PathBuf) {
    let bin_dir = lab.dir.join("bin");
    std::fs::create_dir_all(&bin_dir).expect("bin dir");

    let stub = bin_dir.join("hark-hotkey");
    std::fs::copy(env!("CARGO_BIN_EXE_hark-hotkey"), &stub).expect("copy stub");
    let mut perms = std::fs::metadata(&stub)
        .expect("stub metadata")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&stub, perms).expect("chmod stub");

    let log = lab.dir.join("argv.log");
    let fake = bin_dir.join("hark");
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\nprintf '%s' \"$*\" > '{}'\nexit 7\n",
            log.display()
        ),
    )
    .expect("write fake hark");
    let mut perms = std::fs::metadata(&fake)
        .expect("fake metadata")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&fake, perms).expect("chmod fake");

    (stub, log)
}

/// `--help` is not the stub's job: it must reach the real binary, which prints
/// hark's option summary and exits 0.
#[test]
fn help_is_forwarded_to_the_full_binary() {
    let lab = Lab::new("help");
    let run = lab.run(
        Path::new(env!("CARGO_BIN_EXE_hark-hotkey")),
        &["--help"],
        Duration::from_secs(10),
    );

    assert_eq!(
        run.status.and_then(|s| s.code()),
        Some(0),
        "help must exit 0 (killed after {:?}? stderr: {})",
        run.elapsed,
        run.stderr
    );
    assert!(
        run.stdout.contains("--daemon"),
        "help text must come from the full binary, got: {}",
        run.stdout
    );
}

/// Explicit CLI args are forwarded untouched and the exit code is preserved.
#[test]
fn args_are_forwarded_to_the_full_binary() {
    let lab = Lab::new("args");
    let (stub, log) = stub_with_fake_full(&lab);

    let run = lab.run(&stub, &["--search", "two words"], Duration::from_secs(10));

    assert_eq!(
        run.status.and_then(|s| s.code()),
        Some(7),
        "exit code must be the full binary's (stderr: {})",
        run.stderr
    );
    assert_eq!(
        std::fs::read_to_string(&log).expect("argv log"),
        "--search two words",
        "argv must be forwarded verbatim"
    );
}

/// No daemon: the press falls back to the full binary with no args — the
/// cold-start path stays exactly `hark`'s.
#[test]
fn no_daemon_falls_back_to_the_full_binary() {
    let lab = Lab::new("fallback");
    let (stub, log) = stub_with_fake_full(&lab);
    assert!(!lab.socket().exists());

    let run = lab.run(&stub, &[], Duration::from_secs(10));

    assert_eq!(
        run.status.and_then(|s| s.code()),
        Some(7),
        "fallback must exec the full binary (stderr: {})",
        run.stderr
    );
    assert_eq!(
        std::fs::read_to_string(&log).expect("argv log"),
        "",
        "no-arg press must forward no args"
    );
}

/// Live daemon: the plain press is delivered over the socket and the stub
/// exits without ever reaching the full binary. Needs AF_UNIX bind.
#[test]
#[ignore = "unix socket bind"]
fn plain_press_toggles_a_live_daemon() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let lab = Lab::new("toggle");
    let (stub, log) = stub_with_fake_full(&lab);

    let listener = UnixListener::bind(lab.socket()).expect("bind fake daemon");
    let got_toggle = Arc::new(AtomicBool::new(false));
    let flag = got_toggle.clone();
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0u8; 32];
            if let Ok(n) = stream.read(&mut buf) {
                if buf[..n].windows(6).any(|w| w == b"toggle") {
                    flag.store(true, Ordering::SeqCst);
                }
            }
            let _ = stream.write_all(b"ok\n");
        }
    });

    let run = lab.run(&stub, &[], Duration::from_secs(10));

    assert_eq!(
        run.status.and_then(|s| s.code()),
        Some(0),
        "delivered toggle must exit 0 (killed after {:?}? stderr: {})",
        run.elapsed,
        run.stderr
    );
    assert!(
        got_toggle.load(Ordering::SeqCst),
        "the daemon must receive `toggle`"
    );
    assert!(
        !log.exists(),
        "the full binary must not run when the daemon answered"
    );
}
