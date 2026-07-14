pub mod apps;
pub mod calc;
pub mod files;
pub mod fx;

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
    LaunchApp { exec: String, terminal: bool },
    OpenPath(PathBuf),
    OpenTerminal(PathBuf),
    Copy(String),
    OpenSettings,
}

pub trait Provider: Send + Sync {
    fn search(&self, query: &str) -> Vec<SearchResult>;
}
