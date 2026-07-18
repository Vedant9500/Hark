mod config;
mod engine;
mod ipc;
mod providers;
mod theme;
mod ui;
mod usage;
mod typos;

#[cfg(feature = "bench")]
mod bench;

use engine::Engine;
use gtk::glib;
use gtk::prelude::*;
use gtk::Application;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

const APP_ID: &str = "dev.blink.launcher";

fn main() {
    let mut args: Vec<String> = std::env::args().collect();
    let daemon = args.iter().any(|a| a == "--daemon");
    let bench = args.iter().any(|a| a == "--bench");
    // Headless one-shot: `blink --search "optimization.md in blink"`
    let search_q = args
        .iter()
        .position(|a| a == "--search")
        .and_then(|i| args.get(i + 1).cloned());
    args.retain(|a| a != "--daemon" && a != "--bench" && a != "--search");
    if let Some(q) = search_q.as_ref() {
        args.retain(|a| a != q);
    }

    if bench {
        #[cfg(feature = "bench")]
        {
            bench::run_bench();
            return;
        }
        #[cfg(not(feature = "bench"))]
        {
            eprintln!(
                "blink: --bench requires a build with `--features bench`\n\
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

    // IPC from lightweight `blink` invocations → main loop toggle.
    {
        let state = state.clone();
        let pending_toggle = pending_toggle.clone();
        let app_weak = app_weak.clone();
        let (tx, rx) = async_channel::unbounded::<()>();
        ipc::spawn_listener(move || {
            let _ = tx.send_blocking(());
        });
        glib::spawn_future_local(async move {
            while let Ok(()) = rx.recv().await {
                if let Some(launcher) = state.borrow().as_ref() {
                    launcher.toggle();
                } else {
                    // Window not ready yet — mark pending and force activate.
                    pending_toggle.set(true);
                    if let Some(app) = app_weak.upgrade() {
                        app.activate();
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
        eprintln!("usage: blink --search \"query\"");
        std::process::exit(2);
    }

    println!("blink --search {q:?}");
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
    println!("index-only search: {} hits in {}ms", results.len(), index_ms);
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

