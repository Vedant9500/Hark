use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::RwLock;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct UsageEntry {
    count: u64,
    last: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct UsageFile {
    #[serde(default = "default_version")]
    version: u32,
    #[serde(default)]
    entries: HashMap<String, UsageEntry>,
}

fn default_version() -> u32 {
    1
}

pub struct UsageStore {
    inner: RwLock<UsageFile>,
    path: PathBuf,
}

impl UsageStore {
    pub fn load() -> Self {
        let path = usage_path();
        let data = if path.exists() {
            fs::read_to_string(&path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default()
        } else {
            UsageFile::default()
        };
        Self {
            inner: RwLock::new(data),
            path,
        }
    }

    pub fn record(&self, id: &str) {
        if id.is_empty() {
            return;
        }
        {
            let mut g = self.inner.write().unwrap();
            let e = g.entries.entry(id.to_string()).or_default();
            e.count = e.count.saturating_add(1);
            e.last = now_secs();
        }
        self.save();
    }

    pub fn boost(&self, id: &str) -> i64 {
        let g = self.inner.read().unwrap();
        let Some(e) = g.entries.get(id) else {
            return 0;
        };
        frecency(e.count, e.last)
    }

    /// Top ids by frecency, highest first.
    pub fn top(&self, n: usize) -> Vec<(String, i64)> {
        let g = self.inner.read().unwrap();
        let mut items: Vec<(String, i64)> = g
            .entries
            .iter()
            .map(|(id, e)| (id.clone(), frecency(e.count, e.last)))
            .collect();
        items.sort_by(|a, b| b.1.cmp(&a.1));
        items.truncate(n);
        items
    }

    fn save(&self) {
        if let Some(parent) = self.path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(data) = serde_json::to_string_pretty(&*self.inner.read().unwrap()) {
            let tmp = self.path.with_extension("json.tmp");
            if fs::write(&tmp, data).is_ok() {
                let _ = fs::rename(tmp, &self.path);
            }
        }
    }
}

fn frecency(count: u64, last: u64) -> i64 {
    let now = now_secs();
    let age = now.saturating_sub(last);
    let recency = if age < 86_400 {
        5_000
    } else if age < 86_400 * 7 {
        2_000
    } else if age < 86_400 * 30 {
        500
    } else {
        0
    };
    // Soft decay over ~2 weeks
    let days = age as f64 / 86_400.0;
    let decay = (-days / 14.0).exp();
    ((count as f64) * 1000.0 * decay) as i64 + recency
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn usage_path() -> PathBuf {
    dirs::state_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join(".local/state")))
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("blink/usage.json")
}
