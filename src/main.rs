use gtk::glib;
use gtk::prelude::*;
use gtk::Application;
use hark::engine::Engine;
use hark::ipc;
use hark::ui;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

const APP_ID: &str = "dev.hark.launcher";

fn main() {
    let mut args: Vec<String> = std::env::args().collect();

    // `hark update` / `hark --update`: rebuild + reinstall from the source
    // checkout. Runs before the IPC toggle path and before GTK arg parsing.
    if let Some(forwarded) = update_invocation(&args) {
        run_update(&forwarded);
    }

    let daemon = args.iter().any(|a| a == "--daemon");
    let bench = args.iter().any(|a| a == "--bench");
    // Headless one-shot: `hark --search "optimization.md in hark"`.
    // A missing operand must be a usage error, not a silent fallthrough
    // into resident GUI mode (hangs scripts/keybinds, exec-once leftovers).
    let search_q = parse_search_arg(&args);
    if matches!(search_q, Some(None)) {
        eprintln!("usage: hark --search \"query\"");
        std::process::exit(2);
    }
    let search_q = search_q.flatten();
    args.retain(|a| a != "--daemon" && a != "--bench" && a != "--search");
    if let Some(q) = search_q.as_ref() {
        args.retain(|a| a != q);
    }

    if bench {
        #[cfg(feature = "bench")]
        {
            hark::bench::run_bench();
            return;
        }
        #[cfg(not(feature = "bench"))]
        {
            eprintln!(
                "hark: --bench requires a build with `--features bench`\n\
                 example: cargo build --release --features \"layer-shell,bench\""
            );
            std::process::exit(2);
        }
    }
    if let Some(q) = search_q {
        run_search_once(&q);
        return;
    }

    // Hotkey path: no GTK startup if daemon is already running.
    if !daemon && ipc::request_toggle() {
        return;
    }

    let app = Application::builder().application_id(APP_ID).build();
    let app_weak = app.downgrade();
    let _hold = app.hold();

    let engine = Arc::new(Engine::new());
    let state: Rc<RefCell<Option<ui::Launcher>>> = Rc::new(RefCell::new(None));
    let first_activate = Rc::new(Cell::new(true));
    // IPC may arrive before the window is built — remember to show once ready.
    let pending_toggle = Rc::new(Cell::new(false));

    {
        let engine = engine.clone();
        let state = state.clone();
        let first_activate = first_activate.clone();
        let pending_toggle = pending_toggle.clone();
        app.connect_activate(move |app| {
            let mut slot = state.borrow_mut();
            if let Some(launcher) = slot.as_ref() {
                launcher.toggle();
                return;
            }

            let launcher = ui::Launcher::new(app, engine.clone());
            // Daemon first activate: stay hidden unless an early IPC asked to show.
            let show = !(daemon && first_activate.get()) || pending_toggle.get();
            if show {
                launcher.show();
                pending_toggle.set(false);
            }
            first_activate.set(false);
            *slot = Some(launcher);
        });
    }

    // IPC from lightweight `hark` invocations → main loop toggle.
    {
        let state = state.clone();
        let pending_toggle = pending_toggle.clone();
        let app_weak = app_weak.clone();
        // Capacity 1 + try_send: a toggle flood (IPC spam, stuck hotkey)
        // collapses into at most one queued event instead of growing an
        // unbounded channel and queuing rapid show/hide churn on the loop.
        // A dropped send means a toggle is already pending — exactly the
        // coalescing semantics we want.
        let (tx, rx) = async_channel::bounded::<()>(1);
        ipc::spawn_listener(move || {
            let _ = tx.try_send(());
        });
        glib::spawn_future_local(async move {
            // One activation request while the window is still being built —
            // a flood must not queue an activate chain; pending_toggle is
            // consumed by the activate handler itself.
            let mut activate_requested = false;
            while let Ok(()) = rx.recv().await {
                if let Some(launcher) = state.borrow().as_ref() {
                    launcher.toggle();
                } else {
                    // Window not ready yet — mark pending and force activate.
                    pending_toggle.set(true);
                    if !activate_requested {
                        activate_requested = true;
                        if let Some(app) = app_weak.upgrade() {
                            app.activate();
                        }
                    }
                }
            }
        });
    }

    app.run_with_args(&args);
}

fn run_search_once(query: &str) {
    use std::time::{Duration, Instant};

    let q = query.trim();
    if q.is_empty() {
        eprintln!("usage: hark --search \"query\"");
        std::process::exit(2);
    }

    println!("hark --search {q:?}");
    let engine = Engine::new_headless();
    engine.spawn_warm();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let p = engine.index_progress();
        if !p.running && p.count > 0 {
            break;
        }
        if Instant::now() > deadline {
            println!(
                "warning: index not ready (count={}, running={})",
                p.count, p.running
            );
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let p = engine.index_progress();
    println!("index: {} items · running={}", p.count, p.running);

    let t0 = Instant::now();
    let results = engine.search(q);
    let index_ms = t0.elapsed().as_millis();
    println!(
        "index-only search: {} hits in {}ms",
        results.len(),
        index_ms
    );
    for (i, r) in results.iter().take(12).enumerate() {
        println!(
            "  [{i}] score={} kind={:?} title={}  {}",
            r.score, r.kind, r.title, r.subtitle
        );
    }

    let should = engine.should_deep_search(q, &results);
    println!("should_deep_search={should}");
    if should || results.is_empty() {
        let t1 = Instant::now();
        let deep = engine.search_files_deep(q);
        let deep_ms = t1.elapsed().as_millis();
        println!("deep search: {} hits in {}ms", deep.len(), deep_ms);
        for (i, r) in deep.iter().take(12).enumerate() {
            println!(
                "  [{i}] score={} kind={:?} title={}  {}",
                r.score, r.kind, r.title, r.subtitle
            );
        }
    }
}

/// Recognize the update invocation: `hark update` (subcommand, first arg
/// only — `hark --search update` must stay a search) or `hark --update`.
/// Parse the `--search` flag. `Some(None)` = flag present with no operand
/// (usage error), `None` = flag absent, `Some(Some(q))` = headless query.
/// A missing operand must not silently fall through to resident GUI mode.
fn parse_search_arg(args: &[String]) -> Option<Option<String>> {
    let i = args.iter().position(|a| a == "--search")?;
    Some(args.get(i + 1).cloned())
}

/// Returns the args to forward to the install script (its own flags, e.g.
/// `--no-restart`), or None when this is not an update invocation.
fn update_invocation(args: &[String]) -> Option<Vec<String>> {
    if args.get(1).map(String::as_str) == Some("update") {
        return Some(args.iter().skip(2).cloned().collect());
    }
    if args.iter().any(|a| a == "--update") {
        let forwarded: Vec<String> = args
            .iter()
            .skip(1)
            .filter(|a| a.as_str() != "--update")
            .cloned()
            .collect();
        return Some(forwarded);
    }
    None
}

/// Rebuild + reinstall by exec-ing the install script. The process image is
/// replaced, so the script's `pkill -x hark` (daemon stop for ETXTBSY)
/// cannot kill the updater itself, and no shell/child lingers after exit.
fn run_update(forwarded: &[String]) {
    // Source checkout recorded at build time; hark is only ever built from
    // this tree in the dev flow that ships scripts/install.sh.
    const SRC_ROOT: Option<&str> = option_env!("HARK_SRC_ROOT");

    let root = match SRC_ROOT {
        Some(r) if !r.is_empty() => std::path::PathBuf::from(r),
        _ => {
            // Fallback: a dev install lays out bin as <root>/target/release/hark.
            let cur = std::env::current_exe()
                .ok()
                .and_then(|p| p.canonicalize().ok());
            let from_exe = cur
                .as_deref()
                .and_then(|p| p.parent()) // .../target/release
                .and_then(|p| p.parent()) // .../target
                .map(|p| p.to_path_buf());
            match from_exe {
                Some(p) if p.join("scripts/install.sh").is_file() => p,
                _ => {
                    eprintln!(
                        "hark update: source checkout not found.\n\
                         This command expects a dev install (binary under <checkout>/target/release).\n\
                         Run ./scripts/install.sh from the checkout instead."
                    );
                    std::process::exit(1);
                }
            }
        }
    };

    let script = root.join("scripts/install.sh");
    if !script.is_file() {
        eprintln!("hark update: {} not found", script.display());
        std::process::exit(1);
    }

    let script = script.into_os_string();
    let mut cmd = std::process::Command::new("bash");
    // exec(2) replaces this process; on success nothing after this runs.
    use std::os::unix::process::CommandExt;
    let err = cmd.arg(script).args(forwarded).exec();
    eprintln!("hark update: failed to exec bash: {err}");
    std::process::exit(1);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn update_subcommand_detected_in_first_position() {
        assert_eq!(
            update_invocation(&argv(&["hark", "update"])),
            Some(argv(&[]))
        );
        assert_eq!(
            update_invocation(&argv(&["hark", "update", "--no-restart"])),
            Some(argv(&["--no-restart"]))
        );
    }

    #[test]
    fn update_flag_detected_anywhere() {
        assert_eq!(
            update_invocation(&argv(&["hark", "--update"])),
            Some(argv(&[]))
        );
        assert_eq!(
            update_invocation(&argv(&["hark", "--no-restart", "--update"])),
            Some(argv(&["--no-restart"]))
        );
    }

    #[test]
    fn update_not_confused_with_search_query_or_toggle() {
        // A search query "update" must not trigger the updater.
        assert_eq!(
            update_invocation(&argv(&["hark", "--search", "update"])),
            None
        );
        // Plain toggle/daemon invocations unaffected.
        assert_eq!(update_invocation(&argv(&["hark"])), None);
        assert_eq!(update_invocation(&argv(&["hark", "--daemon"])), None);
        // Bare positional that isn't "update" is not ours to handle.
        assert_eq!(update_invocation(&argv(&["hark", "foo"])), None);
    }

    #[test]
    fn search_arg_requires_operand() {
        // Flag absent.
        assert_eq!(parse_search_arg(&argv(&["hark"])), None);
        // Present with value.
        assert_eq!(
            parse_search_arg(&argv(&["hark", "--search", "foo bar"])),
            Some(Some("foo bar".into()))
        );
        // Present with empty string — still an explicit operand.
        assert_eq!(
            parse_search_arg(&argv(&["hark", "--search", ""])),
            Some(Some("".into()))
        );
        // Present with no operand — must be distinguishable so main exits 2
        // instead of silently entering resident GUI mode.
        assert_eq!(parse_search_arg(&argv(&["hark", "--search"])), Some(None));
    }
}
