pub mod apps;
pub mod calc;
pub mod files;
pub mod fx;
pub mod http;
pub mod translate;

use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultKind {
    App,
    File,
    Folder,
    Calc,
    Conversion,
    Command,
}

/// Raycast-style dual-panel conversion display (time zones, units, etc.)
#[derive(Debug, Clone)]
pub struct ConversionView {
    pub left_title: String,
    pub left_badge: String,
    pub right_title: String,
    pub right_badge: String,
}

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub id: String,
    pub title: String,
    pub subtitle: String,
    pub kind: ResultKind,
    pub score: i64,
    pub icon: Option<String>,
    pub action: Action,
    pub conversion: Option<ConversionView>,
    /// Char indices into `title` that matched the query (Raycast-style
    /// highlight). `None` = the match came from non-title text (keywords,
    /// desktop id, path) or the title is computed (calc/convert) — render
    /// without highlight.
    pub matched: Option<Vec<usize>>,
}

/// Case-insensitive substring positions (char indices) of `needle` in `title`.
///
/// Derived at result-construction time where the query is known, so the UI
/// never re-runs matching. Returns `None` when the needle is absent or when
/// lowercasing the title changes its char count (rare Unicode folds) —
/// indices would not map back onto the original title.
pub fn title_match_indices(title: &str, needle: &str) -> Option<Vec<usize>> {
    if needle.is_empty() {
        return None;
    }
    let needle_lower = needle.to_lowercase();
    let hay = title.to_lowercase();
    if hay.chars().count() != title.chars().count() {
        return None;
    }
    let start = hay.find(&needle_lower)?;
    let start_char = hay[..start].chars().count();
    let len = needle_lower.chars().count();
    Some((0..len).map(|i| start_char + i).collect())
}

#[derive(Debug, Clone)]
pub enum Action {
    /// Launch a `.desktop` app. `desktop_path` is the on-disk entry (for DnD).
    LaunchApp {
        exec: String,
        terminal: bool,
        desktop_path: Option<PathBuf>,
    },
    OpenPath(PathBuf),
    OpenTerminal(PathBuf),
    Copy(String),
    /// Replace the launcher search text (scope folder completions after ` in `).
    SetQuery(String),
    OpenSettings,
    /// Reveal a path in the system file manager (select the item when possible).
    RevealPath(PathBuf),
    /// Move a path to the FreeDesktop trash (`gio trash`).
    TrashPath(PathBuf),
    /// Ask which app should open this path (system Open With dialog).
    OpenWith(PathBuf),
    /// Toggle the media preview side panel (UI-only).
    TogglePreview,
}

/// One entry in the Raycast-style action panel (secondary menu).
#[derive(Debug, Clone)]
pub struct ActionSpec {
    pub id: &'static str,
    pub label: String,
    /// Display-only shortcut hint (e.g. `Ctrl Shift C`).
    pub shortcut: Option<&'static str>,
    pub action: Action,
    pub destructive: bool,
}

impl Action {
    /// Filesystem path that can be dragged to other apps, if any.
    pub fn drag_path(&self) -> Option<&std::path::Path> {
        match self {
            Action::OpenPath(p)
            | Action::OpenTerminal(p)
            | Action::RevealPath(p)
            | Action::TrashPath(p) => Some(p.as_path()),
            Action::LaunchApp { desktop_path, .. } => desktop_path.as_deref(),
            Action::Copy(_)
            | Action::SetQuery(_)
            | Action::OpenSettings
            | Action::OpenWith(_)
            | Action::TogglePreview => None,
        }
    }
}

/// Full equation for a calc/conversion card: `24 * 60 = 1440`, `10 USD = 954.40 INR`.
pub(crate) fn formula_text(item: &SearchResult) -> Option<String> {
    let c = item.conversion.as_ref()?;
    if c.left_title.is_empty() || c.right_title.is_empty() {
        return None;
    }
    Some(format!("{} = {}", c.left_title, c.right_title))
}

/// Value without unit suffix / decoration: `22.05 lb` → `22.05`, `1440` → `1440`.
/// Non-numeric right titles (e.g. hex colors) yield None.
pub(crate) fn unformatted_value(item: &SearchResult) -> Option<String> {
    let c = item.conversion.as_ref()?;
    let t = c.right_title.trim();
    let end = t.find(' ').unwrap_or(t.len());
    let num = &t[..end];
    if num.is_empty()
        || !num
            .chars()
            .all(|ch| ch.is_ascii_digit() || matches!(ch, '.' | ',' | '-' | '+'))
    {
        return None;
    }
    Some(num.to_string())
}

/// Secondary actions for the selected result (action panel / `Ctrl+K`).
///
/// Primary Enter still uses `item.action` directly; the panel lists open plus
/// extras (copy path, reveal, trash, …).
pub fn secondary_actions(item: &SearchResult) -> Vec<ActionSpec> {
    let mut out = Vec::new();
    match item.kind {
        ResultKind::File | ResultKind::Folder => {
            let path = match &item.action {
                Action::OpenPath(p) | Action::OpenTerminal(p) => p.clone(),
                _ => return out,
            };
            out.push(ActionSpec {
                id: "open",
                label: "Open".into(),
                shortcut: Some("↵"),
                action: Action::OpenPath(path.clone()),
                destructive: false,
            });
            // Folders can still be opened with a specific app (e.g. an IDE).
            out.push(ActionSpec {
                id: "open_with",
                label: "Open With…".into(),
                shortcut: Some("Ctrl Shift O"),
                action: Action::OpenWith(path.clone()),
                destructive: false,
            });
            out.push(ActionSpec {
                id: "terminal",
                label: "Open Terminal Here".into(),
                shortcut: Some("Ctrl Alt ↵"),
                action: Action::OpenTerminal(path.clone()),
                destructive: false,
            });
            out.push(ActionSpec {
                id: "copy_path",
                label: "Copy Path".into(),
                shortcut: Some("Ctrl Shift C"),
                action: Action::Copy(path.to_string_lossy().into_owned()),
                destructive: false,
            });
            let name = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(item.title.as_str())
                .to_string();
            out.push(ActionSpec {
                id: "copy_name",
                label: "Copy Name".into(),
                shortcut: None,
                action: Action::Copy(name),
                destructive: false,
            });
            out.push(ActionSpec {
                id: "reveal",
                label: "Reveal in File Manager".into(),
                shortcut: Some("Ctrl Shift R"),
                action: Action::RevealPath(path.clone()),
                destructive: false,
            });
            out.push(ActionSpec {
                id: "trash",
                label: "Move to Trash".into(),
                shortcut: None,
                action: Action::TrashPath(path),
                destructive: true,
            });
            if item.kind == ResultKind::File {
                out.push(ActionSpec {
                    id: "toggle_preview",
                    label: "Toggle Preview".into(),
                    shortcut: Some("Ctrl P"),
                    action: Action::TogglePreview,
                    destructive: false,
                });
            }
        }
        ResultKind::App => {
            out.push(ActionSpec {
                id: "open",
                label: "Open".into(),
                shortcut: Some("↵"),
                action: item.action.clone(),
                destructive: false,
            });
            out.push(ActionSpec {
                id: "copy_name",
                label: "Copy Name".into(),
                shortcut: None,
                action: Action::Copy(item.title.clone()),
                destructive: false,
            });
            if let Action::LaunchApp {
                exec, desktop_path, ..
            } = &item.action
            {
                if let Some(bin) = crate::providers::apps::resolve_exec_binary(exec) {
                    out.push(ActionSpec {
                        id: "copy_exec",
                        label: "Copy Executable Path".into(),
                        shortcut: None,
                        action: Action::Copy(bin.to_string_lossy().into_owned()),
                        destructive: false,
                    });
                    out.push(ActionSpec {
                        id: "reveal_install",
                        label: "Reveal Install Location".into(),
                        shortcut: None,
                        action: Action::RevealPath(bin),
                        destructive: false,
                    });
                }
                if let Some(dp) = desktop_path {
                    out.push(ActionSpec {
                        id: "copy_path",
                        label: "Copy Desktop Path".into(),
                        shortcut: Some("Ctrl Shift C"),
                        action: Action::Copy(dp.to_string_lossy().into_owned()),
                        destructive: false,
                    });
                    out.push(ActionSpec {
                        id: "reveal",
                        label: "Reveal Desktop File".into(),
                        shortcut: Some("Ctrl Shift R"),
                        action: Action::RevealPath(dp.clone()),
                        destructive: false,
                    });
                }
            }
        }
        ResultKind::Calc | ResultKind::Conversion => {
            let text = match &item.action {
                Action::Copy(t) => t.clone(),
                _ => item.title.clone(),
            };
            out.push(ActionSpec {
                id: "copy",
                label: "Copy Result".into(),
                shortcut: Some("Ctrl C"),
                action: Action::Copy(text),
                destructive: false,
            });
            // Raycast trio: ⌘↵ unformatted value, ⌘⇧↵ question + answer.
            if let Some(v) = unformatted_value(item) {
                out.push(ActionSpec {
                    id: "copy_value",
                    label: "Copy Unformatted Value".into(),
                    shortcut: Some("Ctrl ↵"),
                    action: Action::Copy(v),
                    destructive: false,
                });
            }
            if let Some(f) = formula_text(item) {
                out.push(ActionSpec {
                    id: "copy_formula",
                    label: "Copy Formula".into(),
                    shortcut: Some("Ctrl Shift ↵"),
                    action: Action::Copy(f),
                    destructive: false,
                });
            }
        }
        ResultKind::Command => {
            out.push(ActionSpec {
                id: "open",
                label: "Open".into(),
                shortcut: Some("↵"),
                action: item.action.clone(),
                destructive: false,
            });
        }
    }
    out
}

#[cfg(test)]
mod action_panel_tests {
    use super::*;
    use std::path::PathBuf;

    fn file_item(path: &str) -> SearchResult {
        let p = PathBuf::from(path);
        SearchResult {
            id: format!("path:{path}"),
            title: p.file_name().unwrap().to_string_lossy().into(),
            subtitle: path.into(),
            kind: ResultKind::File,
            score: 0,
            icon: None,
            action: Action::OpenPath(p),
            conversion: None,
            matched: None,
        }
    }

    #[test]
    fn file_actions_include_copy_reveal_trash() {
        let acts = secondary_actions(&file_item("/tmp/notes.md"));
        let ids: Vec<_> = acts.iter().map(|a| a.id).collect();
        assert!(ids.contains(&"open_with"));
        assert!(ids.contains(&"copy_path"));
        assert!(ids.contains(&"copy_name"));
        assert!(ids.contains(&"reveal"));
        assert!(ids.contains(&"trash"));
        assert!(ids.contains(&"toggle_preview"));
        assert!(acts.iter().any(|a| a.destructive && a.id == "trash"));
    }

    fn calc_item(conv: Option<ConversionView>, title: &str) -> SearchResult {
        SearchResult {
            id: "calc:t".into(),
            title: title.into(),
            subtitle: String::new(),
            kind: ResultKind::Calc,
            score: 0,
            icon: None,
            action: Action::Copy(title.into()),
            conversion: conv,
            matched: None,
        }
    }

    #[test]
    fn calc_actions_include_copy_trio() {
        let item = calc_item(
            Some(ConversionView {
                left_title: "10 USD".into(),
                left_badge: "USD".into(),
                right_title: "954.40 INR".into(),
                right_badge: "INR · ECB".into(),
            }),
            "954.40 INR",
        );
        let acts = secondary_actions(&item);
        let ids: Vec<_> = acts.iter().map(|a| a.id).collect();
        assert!(ids.contains(&"copy"));
        assert!(ids.contains(&"copy_value"));
        assert!(ids.contains(&"copy_formula"));
        let formula = acts.iter().find(|a| a.id == "copy_formula").unwrap();
        match &formula.action {
            Action::Copy(t) => assert_eq!(t, "10 USD = 954.40 INR"),
            _ => panic!("formula action is not Copy"),
        }
        let value = acts.iter().find(|a| a.id == "copy_value").unwrap();
        match &value.action {
            Action::Copy(t) => assert_eq!(t, "954.40"),
            _ => panic!("value action is not Copy"),
        }
    }

    #[test]
    fn formula_and_unformatted_edge_cases() {
        // Bare math row: value equals the result, no unit to strip.
        let math = calc_item(
            Some(ConversionView {
                left_title: "24 * 60".into(),
                left_badge: "expression".into(),
                right_title: "1440".into(),
                right_badge: "result".into(),
            }),
            "1440",
        );
        assert_eq!(formula_text(&math).as_deref(), Some("24 * 60 = 1440"));
        assert_eq!(unformatted_value(&math).as_deref(), Some("1440"));

        // Non-numeric right side (hex color): no unformatted value.
        let hex = calc_item(
            Some(ConversionView {
                left_title: "#ff5500".into(),
                left_badge: "hex".into(),
                right_title: "FF5500".into(),
                right_badge: "hex".into(),
            }),
            "FF5500",
        );
        assert_eq!(unformatted_value(&hex), None);

        // No card → no formula/value actions at all.
        let plain = calc_item(None, "1440");
        let ids: Vec<_> = secondary_actions(&plain).iter().map(|a| a.id).collect();
        assert!(!ids.contains(&"copy_value"));
        assert!(!ids.contains(&"copy_formula"));
    }

    #[test]
    fn app_actions_include_install_reveal_when_exec_resolves() {
        // `sh` is always on PATH.
        let item = SearchResult {
            id: "app:test".into(),
            title: "Shell".into(),
            subtitle: "test".into(),
            kind: ResultKind::App,
            score: 0,
            icon: None,
            action: Action::LaunchApp {
                exec: "sh".into(),
                terminal: false,
                desktop_path: Some(PathBuf::from("/usr/share/applications/x.desktop")),
            },
            conversion: None,
            matched: None,
        };
        let ids: Vec<_> = secondary_actions(&item).iter().map(|a| a.id).collect();
        assert!(ids.contains(&"reveal_install"));
        assert!(ids.contains(&"copy_exec"));
        assert!(ids.contains(&"reveal"));
        assert!(ids.contains(&"copy_path"));
    }

    #[test]
    fn calc_only_copy() {
        let item = SearchResult {
            id: "calc:1".into(),
            title: "42".into(),
            subtitle: "calc".into(),
            kind: ResultKind::Calc,
            score: 0,
            icon: None,
            action: Action::Copy("42".into()),
            conversion: None,
            matched: None,
        };
        let acts = secondary_actions(&item);
        assert_eq!(acts.len(), 1);
        assert_eq!(acts[0].id, "copy");
    }
}

#[cfg(test)]
mod match_index_tests {
    use super::title_match_indices;

    #[test]
    fn prefix_and_contains_offsets() {
        assert_eq!(title_match_indices("Alacritty", "ala"), Some(vec![0, 1, 2]));
        // Contains: offset counts chars, not bytes.
        assert_eq!(
            title_match_indices("héllo wörld", "wörl"),
            Some(vec![6, 7, 8, 9])
        );
    }

    #[test]
    fn absent_or_empty_needle_is_none() {
        assert_eq!(title_match_indices("Firefox", "chrome"), None);
        assert_eq!(title_match_indices("Firefox", ""), None);
    }

    #[test]
    fn case_insensitive_both_ways() {
        assert_eq!(
            title_match_indices("VSCodium", "codium"),
            Some(vec![2, 3, 4, 5, 6, 7])
        );
        assert_eq!(
            title_match_indices("vscode", "VSCode"),
            Some(vec![0, 1, 2, 3, 4, 5])
        );
    }
}
