mod config;
mod engine;
mod ipc;
mod providers;
mod theme;
mod ui;
mod usage;

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
        run_bench();
        return;
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
    let _hold = app.hold();

    let engine = Arc::new(Engine::new());
    let state: Rc<RefCell<Option<ui::Launcher>>> = Rc::new(RefCell::new(None));
    let first_activate = Rc::new(Cell::new(true));

    {
        let engine = engine.clone();
        let state = state.clone();
        let first_activate = first_activate.clone();
        app.connect_activate(move |app| {
            let mut slot = state.borrow_mut();
            if let Some(launcher) = slot.as_ref() {
                launcher.toggle();
                return;
            }

            let launcher = ui::Launcher::new(app, engine.clone());
            if !(daemon && first_activate.get()) {
                launcher.show();
            }
            first_activate.set(false);
            *slot = Some(launcher);
        });
    }

    // IPC from lightweight `blink` invocations → main loop toggle.
    {
        let state = state.clone();
        let (tx, rx) = async_channel::unbounded::<()>();
        ipc::spawn_listener(move || {
            let _ = tx.send_blocking(());
        });
        glib::spawn_future_local(async move {
            while let Ok(()) = rx.recv().await {
                if let Some(launcher) = state.borrow().as_ref() {
                    launcher.toggle();
                } else {
                    // Window not ready yet — force activate.
                    // Application will create it on next activate.
                }
            }
        });
    }

    app.run_with_args(&args);
}

/// Headless one-shot search for debugging. Usage: `blink --search "query"`
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

/// Headless search + resource micro-bench. Usage: `blink --bench`
/// Prints median / p95 µs, RAM, CPU, GPU (if available).
fn run_bench() {
    use std::time::{Duration, Instant};

    println!("blink --bench");
    println!("warming engine (apps + file index)…");

    let mem_before = proc_mem_self();
    let cpu_before = proc_cpu_self();
    let wall0 = Instant::now();

    // Headless: no 45m periodic refresh thread.
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
    let warm_ms = wall0.elapsed().as_millis();
    let p = engine.index_progress();
    let cache_b = engine.index_cache_bytes().unwrap_or(0);
    println!(
        "index: {} items · capped={} · running={} · warm_ms={} · cache_bytes={}",
        p.count, p.capped, p.running, warm_ms, cache_b
    );

    // Wait for desktop apps (loaded on a bg thread in Engine::new).
    let apps_deadline = Instant::now() + Duration::from_secs(5);
    while engine.apps_len() == 0 && Instant::now() < apps_deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    let apps_n = engine.apps_len();
    let cfg = engine.config().snapshot();
    let deep = cfg.index.deep_roots.join(", ");
    let deep = if deep.is_empty() { "(none)".into() } else { deep };
    println!(
        "config: max_depth={} · deep_roots={} · apps={}",
        cfg.index.max_depth.clamp(1, 6),
        deep,
        apps_n
    );
    if apps_n == 0 {
        println!("warning: no desktop apps loaded — iso_apps / app cases may be empty");
    }

    // E1: timed force rebuild (blocking; bench only)
    println!();
    println!("=== index rebuild (blocking) ===");
    let t_rb = Instant::now();
    engine.bench_force_reindex_blocking();
    let rebuild_ms = t_rb.elapsed().as_millis();
    let p2 = engine.index_progress();
    let cache_b2 = engine.index_cache_bytes().unwrap_or(0);
    println!(
        "rebuild_ms={} · items={} · cache_bytes={} · capped={}",
        rebuild_ms, p2.count, cache_b2, p2.capped
    );

    // App query: prefer a name likely present (chrome/firefox/blink). Fallback "a".
    let app_q = pick_bench_app_query(&engine);
    let queries = [
        ("math", "10 + 20"),
        ("unit", "10kg to lb"),
        ("unit_partial", "10kg to pou"),
        ("fx", "100 usd to eur"),
        ("app", app_q.as_str()),
        ("file", "doc"),
        ("file_force", "f doc"),
        ("settings", "settings"),
    ];

    // Isolated provider probes (engine merge can hide provider wins).
    // iso_files uses index-only (DeepMode::Skip) so live-cache cannot fake µs.
    let iso = [
        ("iso_apps", app_q.as_str(), "apps"),
        ("iso_files", "doc", "files"),
        ("iso_calc", "10 + 20", "calc"),
    ];

    const WARMUP: u32 = 8;
    const ITERS: u32 = 40;

    // Burst: hammer all queries to sample CPU during search load
    let cpu_burst0 = proc_cpu_self();
    let wall_burst0 = Instant::now();
    for _ in 0..20 {
        for (_, q) in queries {
            let _ = engine.search(q);
        }
    }
    let burst_wall = wall_burst0.elapsed();
    let cpu_burst1 = proc_cpu_self();
    let burst_cpu_ms = cpu_delta_ms(cpu_burst0, cpu_burst1);
    let burst_util = if burst_wall.as_millis() > 0 {
        (burst_cpu_ms as f64) * 100.0 / (burst_wall.as_millis() as f64)
    } else {
        0.0
    };

    println!();
    println!("=== engine.search (merged) ===");
    println!(
        "{:<14} {:<18} {:>10} {:>10} {:>8}",
        "case", "query", "median_us", "p95_us", "hits"
    );
    println!("{}", "-".repeat(64));

    for (name, q) in queries {
        let (median, p95, hits) = bench_query(WARMUP, ITERS, || engine.search(q));
        println!(
            "{:<14} {:<18} {:>10} {:>10} {:>8}",
            name, q, median, p95, hits
        );
    }

    println!();
    println!("=== isolated providers ===");
    println!(
        "{:<14} {:<18} {:>10} {:>10} {:>8}",
        "case", "query", "median_us", "p95_us", "hits"
    );
    println!("{}", "-".repeat(64));
    for (name, q, kind) in iso {
        let (median, p95, hits) = match kind {
            "apps" => bench_query(WARMUP, ITERS, || engine.search_apps_only(q)),
            "files" => bench_query(WARMUP, ITERS, || engine.search_files_index_only(q)),
            _ => bench_query(WARMUP, ITERS, || engine.search_calc_only(q)),
        };
        println!(
            "{:<14} {:<18} {:>10} {:>10} {:>8}",
            name, q, median, p95, hits
        );
    }

    let mem_after = proc_mem_self();
    let cpu_after = proc_cpu_self();
    let total_cpu_ms = cpu_delta_ms(cpu_before, cpu_after);
    let total_wall = wall0.elapsed();

    println!();
    println!("=== resources (this --bench process) ===");
    println!(
        "rss_kb:        {} → {}  (Δ {})",
        mem_before.rss_kb,
        mem_after.rss_kb,
        mem_after.rss_kb as i64 - mem_before.rss_kb as i64
    );
    println!(
        "hwm_kb:        {}  (peak RSS)",
        mem_after.hwm_kb
    );
    println!(
        "vsz_kb:        {}",
        mem_after.vsz_kb
    );
    println!("threads:       {}", mem_after.threads);
    println!(
        "cpu_user_ms:   {:.1}",
        cpu_after.user_ms - cpu_before.user_ms
    );
    println!(
        "cpu_sys_ms:    {:.1}",
        cpu_after.sys_ms - cpu_before.sys_ms
    );
    println!(
        "cpu_total_ms:  {:.1}  over wall {:.0} ms",
        total_cpu_ms,
        total_wall.as_millis()
    );
    println!(
        "cpu_burst:     {:.1} ms CPU / {:.0} ms wall ≈ {:.0}% of one core",
        burst_cpu_ms,
        burst_wall.as_millis(),
        burst_util
    );

    // Live daemon (if separate process is running)
    if let Some(d) = daemon_stats() {
        println!();
        println!("=== resources (running blink --daemon) ===");
        println!("pid:           {}", d.pid);
        println!("rss_kb:        {}", d.rss_kb);
        println!("hwm_kb:        {}", d.hwm_kb);
        println!("vsz_kb:        {}", d.vsz_kb);
        println!("threads:       {}", d.threads);
        println!("cpu_%:         {:.1}  (instant, from /proc)", d.cpu_pct);
        println!("mem_%:         {:.2}", d.mem_pct);
        println!("etime:         {}", d.etime);
    } else {
        println!();
        println!("=== resources (running blink --daemon) ===");
        println!("(no daemon process found)");
    }

    // GPU (NVIDIA if present; blink is CPU/GTK — usually idle)
    println!();
    println!("=== gpu ===");
    match gpu_stats() {
        Some(g) => {
            println!("driver:        {}", g.driver);
            println!("name:          {}", g.name);
            println!("util_%:        {}", g.util_pct);
            println!("mem_used_mb:   {}", g.mem_used_mb);
            println!("mem_total_mb:  {}", g.mem_total_mb);
            println!("note:          blink is CPU/GTK; GPU util is system-wide sample");
        }
        None => println!("(no nvidia-smi / GPU stats)"),
    }

    // Host memory
    if let Some(s) = host_mem() {
        println!();
        println!("=== host ===");
        println!("mem_total_kb:  {}", s.total_kb);
        println!("mem_avail_kb:  {}", s.avail_kb);
        println!("cpus:          {}", s.cpus);
    }

    if let Ok(meta) = std::fs::metadata(std::env::current_exe().unwrap_or_default()) {
        println!();
        println!("binary_bytes:  {}", meta.len());
    }
    if let Some(sz) = index_cache_bytes() {
        println!("index_bytes:   {}", sz);
    }

    println!();
    println!("done — paste tables into OPTIMIZATION.md Improvement log");
}


/// Choose a short app query that actually hits installed desktops (bench only).
fn pick_bench_app_query(engine: &Engine) -> String {
    // Prefer classic baselines, then anything installed, else a letter.
    for cand in ["fire", "chrom", "chrome", "blink", "term", "code", "discord"] {
        let hits = engine.search_apps_only(cand);
        if !hits.is_empty() {
            return cand.to_string();
        }
    }
    // Fall back: first loaded app name prefix (2–5 chars).
    let sample = engine.search_apps_only("");
    if let Some(r) = sample.first() {
        let t = r.title.to_lowercase();
        let n = t.chars().take(4).collect::<String>();
        if n.len() >= 2 {
            return n;
        }
    }
    "a".into()
}

fn bench_query<F>(warmup: u32, iters: u32, mut f: F) -> (u64, u64, usize)
where
    F: FnMut() -> Vec<crate::providers::SearchResult>,
{
    use std::time::Instant;
    for _ in 0..warmup {
        let _ = f();
    }
    let mut samples = Vec::with_capacity(iters as usize);
    let mut hits = 0usize;
    for _ in 0..iters {
        let t0 = Instant::now();
        let r = f();
        samples.push(t0.elapsed().as_micros() as u64);
        hits = r.len();
    }
    samples.sort_unstable();
    let median = samples[(samples.len() / 2).min(samples.len() - 1)];
    let p95 =
        samples[(((samples.len() as f64) * 0.95).ceil() as usize - 1).min(samples.len() - 1)];
    (median, p95, hits)
}

#[derive(Clone, Copy, Default)]
struct MemSnap {
    rss_kb: u64,
    hwm_kb: u64,
    vsz_kb: u64,
    threads: u64,
}

#[derive(Clone, Copy, Default)]
struct CpuSnap {
    user_ms: f64,
    sys_ms: f64,
}

struct DaemonSnap {
    pid: i32,
    rss_kb: u64,
    hwm_kb: u64,
    vsz_kb: u64,
    threads: u64,
    cpu_pct: f64,
    mem_pct: f64,
    etime: String,
}

struct GpuSnap {
    driver: String,
    name: String,
    util_pct: String,
    mem_used_mb: String,
    mem_total_mb: String,
}

struct HostSnap {
    total_kb: u64,
    avail_kb: u64,
    cpus: usize,
}

fn proc_mem_self() -> MemSnap {
    proc_mem_pid("self")
}

fn proc_mem_pid(pid: &str) -> MemSnap {
    let Ok(text) = std::fs::read_to_string(format!("/proc/{pid}/status")) else {
        return MemSnap::default();
    };
    let mut s = MemSnap::default();
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("VmRSS:") {
            s.rss_kb = parse_kb(v);
        } else if let Some(v) = line.strip_prefix("VmHWM:") {
            s.hwm_kb = parse_kb(v);
        } else if let Some(v) = line.strip_prefix("VmSize:") {
            s.vsz_kb = parse_kb(v);
        } else if let Some(v) = line.strip_prefix("Threads:") {
            s.threads = v.trim().parse().unwrap_or(0);
        }
    }
    s
}

fn parse_kb(v: &str) -> u64 {
    v.split_whitespace()
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

fn proc_cpu_self() -> CpuSnap {
    // /proc/self/stat fields 14=utime, 15=stime (jiffies)
    let Ok(text) = std::fs::read_to_string("/proc/self/stat") else {
        return CpuSnap::default();
    };
    // comm can contain spaces/parens — find last ')' then split rest
    let Some(rest) = text.rfind(')').map(|i| &text[i + 2..]) else {
        return CpuSnap::default();
    };
    let parts: Vec<&str> = rest.split_whitespace().collect();
    // after ')': state is [0], utime is [11], stime is [12] (0-based from after comm)
    if parts.len() < 13 {
        return CpuSnap::default();
    }
    let utime: u64 = parts[11].parse().unwrap_or(0);
    let stime: u64 = parts[12].parse().unwrap_or(0);
    let hz = sysconf_clk_tck();
    CpuSnap {
        user_ms: (utime as f64) * 1000.0 / hz,
        sys_ms: (stime as f64) * 1000.0 / hz,
    }
}

fn sysconf_clk_tck() -> f64 {
    // Linux USER_HZ is almost always 100 on desktop
    100.0
}

fn cpu_delta_ms(a: CpuSnap, b: CpuSnap) -> f64 {
    (b.user_ms + b.sys_ms) - (a.user_ms + a.sys_ms)
}

fn daemon_stats() -> Option<DaemonSnap> {
    use std::process::Command;
    let out = Command::new("ps")
        .args([
            "-C",
            "blink",
            "-o",
            "pid=,rss=,%cpu=,%mem=,etime=",
            "--no-headers",
        ])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let self_pid = std::process::id();
    for line in text.lines() {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 5 {
            continue;
        }
        let pid: i32 = cols[0].parse().ok()?;
        if pid as u32 == self_pid {
            continue;
        }
        // check cmdline is --daemon
        let cmd = std::fs::read_to_string(format!("/proc/{pid}/cmdline")).unwrap_or_default();
        if !cmd.contains("--daemon") {
            continue;
        }
        let mem = proc_mem_pid(&pid.to_string());
        return Some(DaemonSnap {
            pid,
            rss_kb: mem.rss_kb.max(cols[1].parse().unwrap_or(0)),
            hwm_kb: mem.hwm_kb,
            vsz_kb: mem.vsz_kb,
            threads: mem.threads,
            cpu_pct: cols[2].parse().unwrap_or(0.0),
            mem_pct: cols[3].parse().unwrap_or(0.0),
            etime: cols[4].to_string(),
        });
    }
    None
}

fn gpu_stats() -> Option<GpuSnap> {
    use std::process::Command;
    let out = Command::new("nvidia-smi")
        .args([
            "--query-gpu=name,utilization.gpu,memory.used,memory.total,driver_version",
            "--format=csv,noheader,nounits",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let line = String::from_utf8_lossy(&out.stdout);
    let line = line.lines().next()?.trim();
    if line.is_empty() {
        return None;
    }
    let parts: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
    if parts.len() < 5 {
        return None;
    }
    Some(GpuSnap {
        name: parts[0].to_string(),
        util_pct: parts[1].to_string(),
        mem_used_mb: parts[2].to_string(),
        mem_total_mb: parts[3].to_string(),
        driver: parts[4].to_string(),
    })
}

fn host_mem() -> Option<HostSnap> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    let mut total = 0u64;
    let mut avail = 0u64;
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("MemTotal:") {
            total = parse_kb(v);
        } else if let Some(v) = line.strip_prefix("MemAvailable:") {
            avail = parse_kb(v);
        }
    }
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    Some(HostSnap {
        total_kb: total,
        avail_kb: avail,
        cpus,
    })
}

fn index_cache_bytes() -> Option<u64> {
    let home = dirs::home_dir()?;
    let p = home.join(".cache/blink/file-index.json");
    std::fs::metadata(p).ok().map(|m| m.len())
}
