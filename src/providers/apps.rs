use super::{Action, Provider, ResultKind, SearchResult};
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::RwLock;

const APP_RESULT_LIMIT: usize = 20;

#[derive(Debug, Clone)]
struct DesktopApp {
    id: String,
    name: String,
    name_lower: String,
    comment: String,
    exec: String,
    icon: String,
    terminal: bool,
    no_display: bool,
    /// Absolute path to the `.desktop` file (for drag-and-drop).
    desktop_path: PathBuf,
}

pub struct AppProvider {
    apps: RwLock<Vec<DesktopApp>>,
    matcher: SkimMatcherV2,
}

impl AppProvider {
    pub fn new_empty() -> Self {
        Self {
            apps: RwLock::new(Vec::new()),
            matcher: SkimMatcherV2::default().ignore_case(),
        }
    }

    pub fn len(&self) -> usize {
        self.apps.read().unwrap().len()
    }

    pub fn reload(&self) {
        let mut apps = Vec::new();
        let mut seen = HashSet::new();

        for dir in desktop_dirs() {
            if !dir.exists() {
                continue;
            }
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
                    continue;
                }
                if let Some(app) = parse_desktop_file(&path) {
                    if app.no_display || app.exec.is_empty() || app.name.is_empty() {
                        continue;
                    }
                    if seen.insert(app.id.clone()) {
                        apps.push(app);
                    }
                }
            }
        }

        apps.sort_by(|a, b| a.name_lower.cmp(&b.name_lower));
        *self.apps.write().unwrap() = apps;
    }

    pub fn resolve_id(&self, id: &str) -> Option<SearchResult> {
        let apps = self.apps.read().unwrap();
        apps.iter()
            .find(|a| format!("app:{}", a.id) == id)
            .map(|a| to_result(a, 1000))
    }

    pub fn all_results(&self, limit: usize) -> Vec<SearchResult> {
        let apps = self.apps.read().unwrap();
        apps.iter().take(limit).map(|a| to_result(a, 1000)).collect()
    }

    /// GUI apps suitable for the Settings "Default apps" picker.
    /// Filters out terminal-only entries; keeps NoDisplay=false apps already loaded.
    pub fn list_for_picker(&self) -> Vec<AppPickEntry> {
        let apps = self.apps.read().unwrap();
        apps.iter()
            .filter(|a| !a.terminal && !a.exec.is_empty())
            .map(|a| AppPickEntry {
                desktop_id: desktop_file_id(&a.desktop_path, &a.id),
                name: a.name.clone(),
                icon: a.icon.clone(),
                comment: a.comment.clone(),
            })
            .collect()
    }

    /// Resolve a stored desktop id to a friendly name (uses loaded app list first).
    pub fn display_name_for_desktop_id(&self, desktop_id: &str) -> Option<String> {
        let key = normalize_desktop_id(desktop_id);
        let apps = self.apps.read().unwrap();
        for a in apps.iter() {
            let id = desktop_file_id(&a.desktop_path, &a.id);
            if normalize_desktop_id(&id) == key || normalize_desktop_id(&a.id) == key {
                return Some(a.name.clone());
            }
        }
        crate::providers::files::desktop_id_display_name(desktop_id)
    }
}

/// Lightweight app descriptor for settings UI (no launch plumbing).
#[derive(Debug, Clone)]
pub struct AppPickEntry {
    /// Preferred desktop file id for GDesktopAppInfo (e.g. `org.gnome.Loupe.desktop`).
    pub desktop_id: String,
    pub name: String,
    pub icon: String,
    pub comment: String,
}

fn desktop_file_id(path: &Path, stem: &str) -> String {
    path.file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("{stem}.desktop"))
}

fn normalize_desktop_id(id: &str) -> String {
    let id = id.trim().to_ascii_lowercase();
    id.strip_suffix(".desktop").unwrap_or(&id).to_string()
}

impl Provider for AppProvider {
    fn search(&self, query: &str) -> Vec<SearchResult> {
        let apps = self.apps.read().unwrap();
        let q = query.trim();
        if q.is_empty() {
            return apps
                .iter()
                .take(12)
                .map(|a| to_result(a, 1000))
                .collect();
        }

        let q_lower = q.to_lowercase();
        // Min-heap of top-K scores
        let mut heap: BinaryHeap<Reverse<(i64, usize)>> =
            BinaryHeap::with_capacity(APP_RESULT_LIMIT + 1);

        for (idx, app) in apps.iter().enumerate() {
            // Fast path: prefix / substring on precomputed name_lower
            // Bands must stay aligned with engine.rs (exact 50k, prefix 30k+,
            // contains 15k+). Fuzzy stays well below contains so path exacts win.
            let score = if app.name_lower == q_lower {
                50_000
            } else if app.name_lower.starts_with(&q_lower) {
                30_000 + (q_lower.len() as i64 * 100)
            } else if app.name_lower.contains(&q_lower) {
                15_000 + (q_lower.len() as i64 * 50)
            } else if let Some(s) = self.matcher.fuzzy_match(&app.name_lower, q) {
                // Name-only fuzzy; ignore comment/keywords letter soup.
                if s < 40 {
                    continue;
                }
                s
            } else {
                continue;
            };

            let key = (score, idx);
            if heap.len() < APP_RESULT_LIMIT {
                heap.push(Reverse(key));
            } else if let Some(Reverse(worst)) = heap.peek() {
                if key > *worst {
                    heap.pop();
                    heap.push(Reverse(key));
                }
            }
        }

        let mut scored: Vec<(i64, &DesktopApp)> = heap
            .into_iter()
            .map(|Reverse((score, idx))| (score, &apps[idx]))
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0));
        scored
            .into_iter()
            .map(|(score, app)| to_result(app, score))
            .collect()
    }
}

fn to_result(app: &DesktopApp, score: i64) -> SearchResult {
    SearchResult {
        id: format!("app:{}", app.id),
        title: app.name.clone(),
        subtitle: if app.comment.is_empty() {
            "Application".into()
        } else {
            app.comment.clone()
        },
        kind: ResultKind::App,
        score,
        icon: if app.icon.is_empty() {
            None
        } else {
            Some(app.icon.clone())
        },
        action: Action::LaunchApp {
            exec: app.exec.clone(),
            terminal: app.terminal,
            desktop_path: Some(app.desktop_path.clone()),
        },
        conversion: None,
    }
}

fn desktop_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join(".local/share/applications"));
    }
    if let Ok(xdg) = std::env::var("XDG_DATA_DIRS") {
        for part in xdg.split(':') {
            if !part.is_empty() {
                dirs.push(PathBuf::from(part).join("applications"));
            }
        }
    } else {
        dirs.push(PathBuf::from("/usr/share/applications"));
        dirs.push(PathBuf::from("/usr/local/share/applications"));
    }
    dirs.push(PathBuf::from("/var/lib/flatpak/exports/share/applications"));
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join(".local/share/flatpak/exports/share/applications"));
    }
    dirs
}

fn parse_desktop_file(path: &Path) -> Option<DesktopApp> {
    let content = fs::read_to_string(path).ok()?;
    let mut in_desktop = false;
    let mut name = String::new();
    let mut comment = String::new();
    let mut exec = String::new();
    let mut icon = String::new();
    let mut terminal = false;
    let mut no_display = false;
    let mut hidden = false;
    let mut try_exec = String::new();

    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_desktop = line == "[Desktop Entry]";
            continue;
        }
        if !in_desktop || line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key {
            "Name" if name.is_empty() => name = value.to_string(),
            "Comment" if comment.is_empty() => comment = value.to_string(),
            "Exec" if exec.is_empty() => exec = clean_exec(value),
            "Icon" if icon.is_empty() => icon = value.to_string(),
            "Terminal" => terminal = value.eq_ignore_ascii_case("true"),
            "NoDisplay" => no_display = value.eq_ignore_ascii_case("true"),
            "Hidden" => hidden = value.eq_ignore_ascii_case("true"),
            "TryExec" => try_exec = value.to_string(),
            "Type" if value != "Application" => return None,
            _ => {}
        }
    }

    if hidden || name.is_empty() {
        return None;
    }
    if !try_exec.is_empty() && which(&try_exec).is_none() {
        // Keep entries without resolvable TryExec if Exec exists — many are fine
    }

    let id = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("app")
        .to_string();

    let name_lower = name.to_lowercase();
    Some(DesktopApp {
        id,
        name,
        name_lower,
        comment,
        exec,
        icon,
        terminal,
        no_display,
        desktop_path: path.to_path_buf(),
    })
}

fn clean_exec(exec: &str) -> String {
    let mut parts = Vec::new();
    for part in exec.split_whitespace() {
        if part.starts_with('%') {
            continue;
        }
        parts.push(part);
    }
    parts.join(" ")
}

fn which(bin: &str) -> Option<PathBuf> {
    if bin.contains('/') {
        let p = PathBuf::from(bin);
        return if p.exists() { Some(p) } else { None };
    }
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(bin);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

pub fn launch_app(exec: &str, terminal: bool) {
    let shell_cmd = if terminal {
        let term = std::env::var("TERMINAL")
            .ok()
            .filter(|t| which(t).is_some())
            .or_else(|| which("alacritty").map(|_| "alacritty".into()))
            .or_else(|| which("kitty").map(|_| "kitty".into()))
            .or_else(|| which("foot").map(|_| "foot".into()))
            .unwrap_or_else(|| "xterm".into());
        format!("{term} -e {exec}")
    } else {
        exec.to_string()
    };

    // Detach fully so the app survives after blink hides.
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(format!("setsid -f {shell_cmd} >/dev/null 2>&1 || nohup {shell_cmd} >/dev/null 2>&1 &"))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let _ = cmd.spawn();
}
