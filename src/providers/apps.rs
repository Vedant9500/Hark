use super::{Action, ResultKind, SearchResult};
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
    /// Lowercased Name + GenericName + Keywords + desktop id for matching.
    search_blob: String,
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

    #[cfg(feature = "bench")]
    pub fn len(&self) -> usize {
        self.apps.read().unwrap_or_else(|p| p.into_inner()).len()
    }

    /// Test-only: inject apps directly (no filesystem `.desktop` scan).
    /// `id`, `name` are used as-is; a minimal search blob is derived from name + id.
    #[cfg(test)]
    pub(crate) fn inject(&self, apps: &[(&str, &str)]) {
        let mut list = Vec::with_capacity(apps.len());
        for (id, name) in apps {
            let id_tokens = id.replace(['-', '_', '.'], " ").to_ascii_lowercase();
            list.push(DesktopApp {
                id: (*id).to_string(),
                name: (*name).to_string(),
                name_lower: name.to_lowercase(),
                search_blob: format!("{} {} {id_tokens}", name.to_lowercase(), id_tokens),
                comment: String::new(),
                exec: format!("{} %U", id.replace('-', "_")),
                icon: String::new(),
                terminal: false,
                no_display: false,
                desktop_path: PathBuf::new(),
            });
        }
        list.sort_by(|a, b| a.name_lower.cmp(&b.name_lower));
        *self.apps.write().unwrap_or_else(|p| p.into_inner()) = list;
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
        *self.apps.write().unwrap_or_else(|p| p.into_inner()) = apps;
    }

    pub fn resolve_id(&self, id: &str) -> Option<SearchResult> {
        let key = id.strip_prefix("app:").unwrap_or(id);
        let apps = self.apps.read().unwrap_or_else(|p| p.into_inner());
        apps.iter()
            .find(|a| a.id == key)
            .map(|a| to_result(a, 1000))
    }

    pub fn all_results(&self, limit: usize) -> Vec<SearchResult> {
        let apps = self.apps.read().unwrap_or_else(|p| p.into_inner());
        apps.iter()
            .take(limit)
            .map(|a| to_result(a, 1000))
            .collect()
    }

    /// GUI apps suitable for the Settings "Default apps" picker.
    /// Filters out terminal-only entries; keeps NoDisplay=false apps already loaded.
    pub fn list_for_picker(&self) -> Vec<AppPickEntry> {
        let apps = self.apps.read().unwrap_or_else(|p| p.into_inner());
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
        let apps = self.apps.read().unwrap_or_else(|p| p.into_inner());
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

impl AppProvider {
    pub fn search(&self, query: &str) -> Vec<SearchResult> {
        let apps = self.apps.read().unwrap_or_else(|p| p.into_inner());
        let q = query.trim();
        if q.is_empty() {
            return apps.iter().take(12).map(|a| to_result(a, 1000)).collect();
        }

        let q_lower = q.to_lowercase();
        // Min-heap of top-K scores
        let mut heap: BinaryHeap<Reverse<(i64, usize)>> =
            BinaryHeap::with_capacity(APP_RESULT_LIMIT + 1);

        for (idx, app) in apps.iter().enumerate() {
            // Fast path: prefix / substring on precomputed name_lower
            // Bands must stay aligned with engine.rs (exact 50k, prefix 30k+,
            // contains 15k+). Fuzzy stays well below contains so path exacts win.
            // Keywords / GenericName / desktop id live in search_blob at a
            // weaker band so they surface apps without beating real name hits.
            let score = if app.name_lower == q_lower {
                50_000
            } else if app.name_lower.starts_with(&q_lower) {
                30_000 + (q_lower.len() as i64 * 100)
            } else if app.name_lower.contains(&q_lower) {
                15_000 + (q_lower.len() as i64 * 50)
            } else if app
                .search_blob
                .split_whitespace()
                .any(|tok| tok == q_lower || tok.starts_with(&q_lower))
            {
                // Keyword / desktop-id token (e.g. Keywords=zed, id sublime_text).
                // Keep below name-contains (15k) so real title hits still win.
                14_000 + (q_lower.len() as i64 * 50)
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
        scored.sort_by_key(|b| std::cmp::Reverse(b.0));
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
    // Flatpak / Snap export locations (not always present in XDG_DATA_DIRS).
    dirs.push(PathBuf::from("/var/lib/flatpak/exports/share/applications"));
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join(".local/share/flatpak/exports/share/applications"));
    }
    dirs.push(PathBuf::from("/var/lib/snapd/desktop/applications"));
    dirs
}

fn parse_desktop_file(path: &Path) -> Option<DesktopApp> {
    let content = fs::read_to_string(path).ok()?;
    let mut in_desktop = false;
    let mut name = String::new();
    let mut generic_name = String::new();
    let mut keywords = String::new();
    let mut comment = String::new();
    let mut exec = String::new();
    let mut icon = String::new();
    let mut terminal = false;
    let mut no_display = false;
    let mut hidden = false;

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
            "GenericName" if generic_name.is_empty() => generic_name = value.to_string(),
            // Keywords use semicolon separators (trailing `;` common).
            "Keywords" if keywords.is_empty() => {
                keywords = value.replace(';', " ").to_string();
            }
            "Comment" if comment.is_empty() => comment = value.to_string(),
            "Exec" if exec.is_empty() => exec = clean_exec(value),
            "Icon" if icon.is_empty() => icon = value.to_string(),
            "Terminal" => terminal = value.eq_ignore_ascii_case("true"),
            "NoDisplay" => no_display = value.eq_ignore_ascii_case("true"),
            "Hidden" => hidden = value.eq_ignore_ascii_case("true"),
            "Type" if value != "Application" => return None,
            _ => {}
        }
    }

    if hidden || name.is_empty() {
        return None;
    }

    let id = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("app")
        .to_string();

    let name_lower = name.to_lowercase();
    // Include desktop id with separators normalized so `sublime_text` matches `subli`.
    // Parts are lowercased once; desktop ids are typically already ASCII-lower.
    let id_tokens = id.replace(['-', '_', '.'], " ").to_ascii_lowercase();
    let search_blob = format!(
        "{name_lower} {} {} {id_tokens}",
        generic_name.to_lowercase(),
        keywords.to_lowercase(),
    );
    Some(DesktopApp {
        id,
        name,
        name_lower,
        search_blob,
        comment,
        exec,
        icon,
        terminal,
        no_display,
        desktop_path: path.to_path_buf(),
    })
}

/// Split a desktop `Exec=` value into argv, respecting simple shell-style quotes.
/// Never feed the result through a shell — tokens are passed as argv only.
fn split_exec_args(exec: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut cur = String::new();
    let mut chars = exec.chars().peekable();
    let mut in_single = false;
    let mut in_double = false;

    while let Some(c) = chars.next() {
        match c {
            '\'' if !in_double => {
                in_single = !in_single;
            }
            '"' if !in_single => {
                in_double = !in_double;
            }
            '\\' if in_double => {
                if let Some(&next) = chars.peek() {
                    if matches!(next, '"' | '\\' | '$' | '`') {
                        cur.push(chars.next().unwrap());
                    } else {
                        cur.push('\\');
                    }
                } else {
                    cur.push('\\');
                }
            }
            c if c.is_whitespace() && !in_single && !in_double => {
                if !cur.is_empty() {
                    args.push(std::mem::take(&mut cur));
                }
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        args.push(cur);
    }
    args
}

fn clean_exec(exec: &str) -> String {
    split_exec_args(exec)
        .into_iter()
        .filter(|part| !part.starts_with('%'))
        .collect::<Vec<_>>()
        .join(" ")
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

fn resolve_terminal() -> String {
    std::env::var("TERMINAL")
        .ok()
        .filter(|t| which(t).is_some())
        .or_else(|| which("alacritty").map(|_| "alacritty".into()))
        .or_else(|| which("kitty").map(|_| "kitty".into()))
        .or_else(|| which("foot").map(|_| "foot".into()))
        .unwrap_or_else(|| "xterm".into())
}

/// Detach with argv only — never interpolate into `sh -c`.
fn spawn_detached_argv(argv: &[String]) -> Result<(), String> {
    if argv.is_empty() {
        return Ok(());
    }

    // Prefer `setsid -f program args...` so the child survives blink exit without a shell.
    let mut cmd = Command::new("setsid");
    cmd.arg("-f")
        .args(argv)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    if cmd.spawn().is_ok() {
        return Ok(());
    }

    // Fallback: direct spawn (may die with the parent session on some setups).
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    cmd.spawn()
        .map(|_| ())
        .map_err(|err| format!("could not launch {}: {err}", argv[0]))
}

/// Resolve the primary binary from a desktop `Exec=` line (field codes stripped).
pub fn resolve_exec_binary(exec: &str) -> Option<PathBuf> {
    let first = split_exec_args(exec)
        .into_iter()
        .find(|part| !part.starts_with('%') && !part.is_empty())?;
    which(&first)
}

pub fn launch_app(exec: &str, terminal: bool) -> Result<(), String> {
    let mut argv: Vec<String> = split_exec_args(exec)
        .into_iter()
        .filter(|part| !part.starts_with('%'))
        .collect();
    if argv.is_empty() {
        return Ok(());
    }

    if terminal {
        let term = resolve_terminal();
        let mut full = Vec::with_capacity(argv.len() + 2);
        full.push(term);
        full.push("-e".into());
        full.append(&mut argv);
        spawn_detached_argv(&full)
    } else {
        spawn_detached_argv(&argv)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parses_keywords_and_matches_desktop_id() {
        let dir = std::env::temp_dir().join(format!("blink-app-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sublime_text.desktop");
        let mut f = fs::File::create(&path).unwrap();
        write!(
            f,
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=Sublime Text\n\
             GenericName=Text Editor\n\
             Comment=Sophisticated text editor\n\
             Exec=/usr/bin/subl %F\n\
             Icon=sublime-text\n\
             Keywords=subl;editor;\n"
        )
        .unwrap();

        let app = parse_desktop_file(&path).expect("parse");
        assert_eq!(app.name, "Sublime Text");
        assert!(app.search_blob.contains("subl"));
        assert!(app.search_blob.contains("sublime"));

        let provider = AppProvider::new_empty();
        {
            let mut apps = provider.apps.write().unwrap_or_else(|p| p.into_inner());
            apps.push(app);
        }
        let hits = provider.search("subli");
        assert!(
            hits.iter().any(|h| h.title == "Sublime Text"),
            "expected Sublime Text in {:?}",
            hits.iter().map(|h| h.title.clone()).collect::<Vec<_>>()
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn split_exec_args_handles_quotes_and_field_codes() {
        assert_eq!(
            split_exec_args(r#"/usr/bin/foo --opt="bar baz" %F"#),
            vec![
                "/usr/bin/foo".to_string(),
                "--opt=bar baz".to_string(),
                "%F".to_string()
            ]
        );
        assert_eq!(
            clean_exec(r#"/usr/bin/foo --opt="bar baz" %U"#),
            "/usr/bin/foo --opt=bar baz"
        );
        // Metacharacters must remain a single argv token, not shell syntax.
        assert_eq!(
            split_exec_args(r#"evil;rm -rf /"#),
            vec!["evil;rm".to_string(), "-rf".to_string(), "/".to_string()]
        );
        assert_eq!(
            split_exec_args(r#"/bin/echo '$(reboot)'"#),
            vec!["/bin/echo".to_string(), "$(reboot)".to_string()]
        );
    }
}
