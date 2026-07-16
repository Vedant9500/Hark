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
}

impl Action {
    /// Filesystem path that can be dragged to other apps, if any.
    pub fn drag_path(&self) -> Option<&std::path::Path> {
        match self {
            Action::OpenPath(p) | Action::OpenTerminal(p) => Some(p.as_path()),
            Action::LaunchApp { desktop_path, .. } => desktop_path.as_deref(),
            Action::Copy(_) | Action::SetQuery(_) | Action::OpenSettings => None,
        }
    }
}

