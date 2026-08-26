use super::{title_match_indices, Action, ResultKind, SearchResult};
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};
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
    /// Parsed launch argv (field codes stripped), preserved losslessly.
    /// `exec` is a display/re-parse string that quotes tokens so
    /// `split_exec_args` round-trips it back to exactly this argv.
    argv: Vec<String>,
    icon: String,
    terminal: bool,
    no_display: bool,
    /// Absolute path to the `.desktop` file (for drag-and-drop).
    desktop_path: PathBuf,
}

/// Quote a single argv token (if needed) so `split_exec_args` reproduces it
/// exactly. Desktop Exec quoting is destroyed by parse→join→re-split unless
/// tokens containing whitespace or quote characters are re-quoted here.
fn quote_token(tok: &str) -> String {
    if !tok.is_empty()
        && tok.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(
                    c,
                    '_' | '-' | '/' | '.' | ':' | '=' | '+' | '@' | ',' | '%' | '*'
                )
        })
    {
        return tok.to_string();
    }
    // Double-quote and escape what the double-quote lexer treats specially.
    let mut out = String::with_capacity(tok.len() + 2);
    out.push('"');
    for c in tok.chars() {
        if matches!(c, '"' | '\\' | '$' | '`') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// Join argv into an Exec-style string that survives a re-split unchanged.
fn quote_join(argv: &[String]) -> String {
    argv.iter()
        .map(|t| quote_token(t))
        .collect::<Vec<_>>()
        .join(" ")
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

    #[cfg(feature = "bench")]
    pub fn is_empty(&self) -> bool {
        self.apps
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .is_empty()
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
                argv: vec![id.replace('-', "_")],
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
            .map(|a| to_result(a, 1000, None))
    }

    pub fn all_results(&self, limit: usize) -> Vec<SearchResult> {
        let apps = self.apps.read().unwrap_or_else(|p| p.into_inner());
        apps.iter()
            .take(limit)
            .map(|a| to_result(a, 1000, None))
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
            return apps
                .iter()
                .take(12)
                .map(|a| to_result(a, 1000, None))
                .collect();
        }

        let q_lower = q.to_lowercase();
        // Min-heap of top-K scores
        let mut heap: BinaryHeap<Reverse<(i64, usize)>> =
            BinaryHeap::with_capacity(APP_RESULT_LIMIT + 1);
        // Fuzzy-band char spans per app idx (heap entry keeps only score+idx).
        // Substring bands re-derive their spans deterministically at
        // conversion; fuzzy needs the matcher's own indices here.
        let mut fuzzy_spans: HashMap<usize, Vec<usize>> = HashMap::new();

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
            } else if let Some((s, indices)) = self.matcher.fuzzy_indices(&app.name_lower, q) {
                // Name-only fuzzy; ignore comment/keywords letter soup.
                if s < 40 {
                    continue;
                }
                // Indices map onto the title only when case folding preserved
                // the char count (guards rare Unicode expansions).
                if app.name_lower.chars().count() == app.name.chars().count() {
                    fuzzy_spans.insert(idx, indices);
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

        let mut scored: Vec<(i64, usize)> = heap
            .into_iter()
            .map(|Reverse((score, idx))| (score, idx))
            .collect();
        scored.sort_by_key(|b| std::cmp::Reverse(b.0));
        scored
            .into_iter()
            .map(|(score, idx)| {
                let app = &apps[idx];
                // Fuzzy spans when present; substring bands (exact / prefix /
                // contains) re-derive from the title itself. Token-band hits
                // never contain the query in the name (contains would have
                // matched first), so they correctly get no highlight.
                let matched = fuzzy_spans
                    .get(&idx)
                    .cloned()
                    .or_else(|| title_match_indices(&app.name, &q_lower));
                to_result(app, score, matched)
            })
            .collect()
    }
}

fn to_result(app: &DesktopApp, score: i64, matched: Option<Vec<usize>>) -> SearchResult {
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
            // Always serialized from the parsed argv so the launch path
            // (`split_exec_args`) reproduces it exactly.
            exec: quote_join(&app.argv),
            terminal: app.terminal,
            desktop_path: Some(app.desktop_path.clone()),
        },
        conversion: None,
        matched,
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
    let mut exec_raw = String::new();
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
            "Exec" if exec_raw.is_empty() => exec_raw = value.to_string(),
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
    // Field codes are launch placeholders, not arguments — strip exactly the
    // standalone codes (never tokens that merely start with `%`).
    let argv: Vec<String> = split_exec_args(&exec_raw)
        .into_iter()
        .filter(|part| !is_field_code(part))
        .collect();
    let exec = quote_join(&argv);
    Some(DesktopApp {
        id,
        name,
        name_lower,
        search_blob,
        comment,
        exec,
        argv,
        icon,
        terminal,
        no_display,
        desktop_path: path.to_path_buf(),
    })
}

/// Split a desktop `Exec=` value into argv, respecting simple shell-style quotes.
/// Never feed the result through a shell — tokens are passed as argv only.
///
/// Lenient on unterminated quotes: the rest of the line joins the current
/// token rather than erroring.
fn split_exec_args(exec: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut cur = String::new();
    let mut chars = exec.chars().peekable();
    let mut in_single = false;
    let mut in_double = false;

    while let Some(c) = chars.next() {
        match c {
            // Desktop Entry spec: outside quotes, `\` escapes the next char
            // (`bar\ baz` is one token, `\\` is a literal backslash).
            '\\' if !in_single && !in_double => {
                // Drop the backslash; the escaped char keeps its literal value
                // but loses any special meaning (whitespace stays in-token).
                if let Some(escaped) = chars.next() {
                    cur.push(escaped);
                }
            }
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
                }
                // Trailing `\` inside an unterminated double quote escapes
                // nothing — drop it instead of leaking a stray backslash.
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

/// Standalone desktop-entry field codes (`%f`, `%F`, `%%`-style placeholders).
/// Per spec these are exact standalone tokens — `--opt=%s` is a normal argument.
fn is_field_code(part: &str) -> bool {
    matches!(
        part,
        "%f" | "%u" | "%F" | "%U" | "%d" | "%D" | "%n" | "%i" | "%c" | "%k" | "%v" | "%m"
    )
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

    // Prefer `setsid -f program args...` so the child survives hark exit without a shell.
    let mut cmd = Command::new("setsid");
    cmd.arg("-f").args(argv);
    if crate::providers::files::spawn_and_reap(cmd).is_ok() {
        return Ok(());
    }

    // Fallback: direct spawn (may die with the parent session on some setups).
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    crate::providers::files::spawn_and_reap(cmd)
        .map_err(|err| format!("could not launch {}: {err}", argv[0]))
}

/// Resolve the primary binary from a desktop `Exec=` line (field codes stripped).
pub fn resolve_exec_binary(exec: &str) -> Option<PathBuf> {
    let first = split_exec_args(exec)
        .into_iter()
        .find(|part| !is_field_code(part) && !part.is_empty())?;
    which(&first)
}

pub fn launch_app(exec: &str, terminal: bool) -> Result<(), String> {
    let mut argv: Vec<String> = split_exec_args(exec)
        .into_iter()
        .filter(|part| !is_field_code(part))
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
        let dir = std::env::temp_dir().join(format!("hark-app-test-{}", std::process::id()));
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
    fn matched_spans_follow_scoring_bands() {
        let provider = AppProvider::new_empty();
        provider.inject(&[
            ("org.alacritty.Alacritty", "Alacritty"),
            ("firefox", "Firefox"),
            ("org.gnome.eog", "Image Viewer"),
        ]);

        // Prefix band: highlight the first three chars of the title.
        let hits = provider.search("ala");
        assert_eq!(hits[0].title, "Alacritty");
        assert_eq!(hits[0].matched.as_deref(), Some(&[0usize, 1, 2][..]));

        // Exact band — whole title highlighted.
        let hits = provider.search("firefox");
        assert_eq!(hits[0].title, "Firefox");
        assert_eq!(hits[0].matched, Some((0..7).collect::<Vec<usize>>()));

        // Token-band hit (desktop id `eog`): matched via search_blob, not the
        // title — no highlight, which is honest.
        let hits = provider.search("eog");
        assert!(hits.iter().any(|h| h.title == "Image Viewer"));
        assert!(hits
            .iter()
            .all(|h| h.title != "Image Viewer" || h.matched.is_none()));

        // Fuzzy band: sparse matcher indices (a…c…t of Alacritty).
        let hits = provider.search("act");
        let alac = hits.iter().find(|h| h.title == "Alacritty").unwrap();
        let spans = alac.matched.as_deref().unwrap();
        assert_eq!(spans.len(), 3, "fuzzy span count: {spans:?}");
        assert_eq!(spans[0], 0, "first fuzzy span anchors at 'A': {spans:?}");
        assert!(spans[2] < alac.title.chars().count());
    }

    #[test]
    fn split_exec_args_handles_quotes_and_field_codes() {
        // Desktop Entry spec: unquoted backslash escapes the next char.
        assert_eq!(
            split_exec_args(r"/usr/bin/foo bar\ baz"),
            vec!["/usr/bin/foo".to_string(), "bar baz".to_string()]
        );
        assert_eq!(
            split_exec_args(r"/usr/bin/foo a\\b"),
            vec!["/usr/bin/foo".to_string(), "a\\b".to_string()]
        );
        assert_eq!(
            split_exec_args(r"/bin/echo 'single \ kept'"),
            vec!["/bin/echo".to_string(), "single \\ kept".to_string()]
        );
        assert_eq!(
            split_exec_args(r#"/usr/bin/foo --opt="bar baz" %F"#),
            vec![
                "/usr/bin/foo".to_string(),
                "--opt=bar baz".to_string(),
                "%F".to_string()
            ]
        );
        // Field codes strip as exact standalone tokens only.
        assert!(is_field_code("%F"));
        assert!(!is_field_code("--opt=%s"));
        assert_eq!(
            split_exec_args(r#"/usr/bin/foo --opt=%s %F"#),
            vec![
                "/usr/bin/foo".to_string(),
                "--opt=%s".to_string(),
                "%F".to_string()
            ]
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
        // Unterminated double quote: lenient merge, no dangling escape leak.
        assert_eq!(
            split_exec_args(r#"/bin/echo "hi\"#),
            vec!["/bin/echo".to_string(), "hi".to_string()]
        );

        // Quoting survives storage: re-splitting the stored Exec string
        // reproduces the original argv exactly.
        let argvs: Vec<Vec<String>> = vec![
            vec!["foo".into(), "--opt=bar baz".into()],
            vec!["sh".into(), "-c".into(), "echo hello".into()],
            vec!["/usr/bin/subl".into(), "--launch-group".into()],
            vec!["weird;$(reboot)".into(), "a'b".into()],
        ];
        for argv in &argvs {
            let joined = quote_join(argv);
            assert_eq!(split_exec_args(&joined), *argv, "round trip of {joined:?}");
        }
    }

    #[test]
    fn parsed_desktop_argv_survives_launch_round_trip() {
        let dir = std::env::temp_dir().join(format!("hark-app-argv-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("quoted.desktop");
        let mut f = fs::File::create(&path).unwrap();
        write!(
            f,
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=Quoted\n\
             Exec=sh -c \"echo hello world\" --opt=%s %F\n\
             Icon=x\n"
        )
        .unwrap();

        let app = parse_desktop_file(&path).expect("parse");
        assert_eq!(
            app.argv,
            vec![
                "sh".to_string(),
                "-c".to_string(),
                "echo hello world".to_string(),
                "--opt=%s".to_string()
            ]
        );
        // What launch_app consumes must equal what was parsed at scan time.
        assert_eq!(split_exec_args(&app.exec), app.argv);

        let _ = fs::remove_dir_all(&dir);
    }
}
