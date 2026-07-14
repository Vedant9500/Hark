use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PathStyle {
    Label,
    Drive,
}

impl Default for PathStyle {
    fn default() -> Self {
        Self::Label
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexConfig {
    #[serde(default = "default_true")]
    pub include_home: bool,
    #[serde(default)]
    pub include_mounts: HashMap<String, bool>,
    #[serde(default)]
    pub extra_roots: Vec<String>,
    /// Folders always walked to depth 6 at index time (pin / promote).
    /// Also preferred as live deep-search roots. Never written by live hits.
    #[serde(default)]
    pub deep_roots: Vec<String>,
    #[serde(default = "default_excludes")]
    pub exclude: Vec<String>,
    #[serde(default = "default_depth")]
    pub max_depth: usize,
    #[serde(default)]
    pub path_style: PathStyle,
}

fn default_true() -> bool {
    true
}

fn default_depth() -> usize {
    // Level 0 = root; walk max_depth components below it.
    // 2 ≈ ~/Projects/foo; clamped 1..=6 in FileProvider.
    2
}

fn default_excludes() -> Vec<String> {
    vec![
        // VCS / package / build
        ".git".into(),
        ".svn".into(),
        ".hg".into(),
        "node_modules".into(),
        "target".into(),
        "dist".into(),
        "build".into(),
        "out".into(),
        // Python / env
        ".venv".into(),
        "venv".into(),
        "env".into(),
        ".env".into(),
        "__pycache__".into(),
        ".mypy_cache".into(),
        ".pytest_cache".into(),
        ".tox".into(),
        // JS / tooling caches
        ".npm".into(),
        ".yarn".into(),
        ".pnpm-store".into(),
        ".turbo".into(),
        ".next".into(),
        ".nuxt".into(),
        // Rust / Java / IDE
        ".cargo".into(),
        ".rustup".into(),
        ".gradle".into(),
        ".m2".into(),
        ".idea".into(),
        // System / browser junk
        ".cache".into(),
        ".thumbnails".into(),
        "Trash".into(),
        "$RECYCLE.BIN".into(),
        "System Volume Information".into(),
        "BraveSoftware".into(),
        ".mozilla".into(),
        ".steam".into(),
        ".pi/agent/sessions".into(),
    ]
}

impl Default for IndexConfig {
    fn default() -> Self {
        Self {
            include_home: true,
            include_mounts: HashMap::new(),
            extra_roots: Vec::new(),
            deep_roots: Vec::new(),
            exclude: default_excludes(),
            max_depth: default_depth(),
            path_style: PathStyle::Label,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BlinkConfig {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub index: IndexConfig,
}

fn default_version() -> u32 {
    1
}

#[derive(Debug, Clone)]
pub struct MountInfo {
    pub target: PathBuf,
    pub label: String,
    pub drive_letter: Option<char>,
}

pub struct ConfigStore {
    inner: RwLock<BlinkConfig>,
    path: PathBuf,
}

impl ConfigStore {
    pub fn load() -> Self {
        let path = config_path();
        let mut cfg = if path.exists() {
            fs::read_to_string(&path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default()
        } else {
            BlinkConfig::default()
        };

        // Seed mount defaults for newly discovered mounts
        let mounts = discover_mounts();
        let mut changed = false;
        // Migrate older deep-scan configs (default used to be 8).
        // FileProvider clamps walk to 1..=6 — allow up to 6 here.
        if cfg.index.max_depth > 6 {
            cfg.index.max_depth = default_depth();
            changed = true;
        }
        // Ensure important ignore names exist even on older configs
        for name in default_excludes() {
            if !cfg.index.exclude.iter().any(|e| e == &name) {
                cfg.index.exclude.push(name);
                changed = true;
            }
        }
        for m in &mounts {
            let key = m.target.to_string_lossy().to_string();
            if !cfg.index.include_mounts.contains_key(&key) {
                // windows_c off by default (large); others on
                let on = !key.contains("windows_c") && !key.contains("windowsEFI");
                cfg.index.include_mounts.insert(key, on);
                changed = true;
            }
        }

        let store = Self {
            inner: RwLock::new(cfg),
            path,
        };
        if changed || !store.path.exists() {
            store.save();
        }
        store
    }

    pub fn get(&self) -> BlinkConfig {
        self.inner.read().unwrap().clone()
    }

    pub fn update<F: FnOnce(&mut BlinkConfig)>(&self, f: F) {
        {
            let mut g = self.inner.write().unwrap();
            f(&mut g);
        }
        self.save();
    }

    pub fn save(&self) {
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

pub fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")))
        .join("blink/config.json")
}

pub fn discover_mounts() -> Vec<MountInfo> {
    let mut mounts = Vec::new();

    // findmnt -J preferred
    if let Ok(out) = Command::new("findmnt")
        .args(["-J", "-t", "ntfs,ntfs3,fuseblk,exfat,vfat"])
        .output()
    {
        if out.status.success() {
            if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&out.stdout) {
                if let Some(arr) = v.get("filesystems").and_then(|x| x.as_array()) {
                    collect_findmnt(arr, &mut mounts);
                }
            }
        }
    }

    if mounts.is_empty() {
        // Fallback: scan common roots
        for base in ["/mnt", "/media", "/run/media"] {
            let base = PathBuf::from(base);
            if let Ok(rd) = fs::read_dir(&base) {
                for e in rd.flatten() {
                    let p = e.path();
                    if p.is_dir() {
                        // /run/media/user/LABEL
                        if base.ends_with("media") {
                            if let Ok(rd2) = fs::read_dir(&p) {
                                for e2 in rd2.flatten() {
                                    let p2 = e2.path();
                                    if p2.is_dir() {
                                        mounts.push(mount_from_path(&p2));
                                    }
                                }
                            }
                        } else {
                            mounts.push(mount_from_path(&p));
                        }
                    }
                }
            }
        }
    }

    // Dedupe by target
    let mut seen = std::collections::HashSet::new();
    mounts.retain(|m| seen.insert(m.target.clone()));
    mounts.sort_by(|a, b| a.target.cmp(&b.target));
    mounts
}

fn collect_findmnt(arr: &[serde_json::Value], out: &mut Vec<MountInfo>) {
    for fs in arr {
        if let Some(target) = fs.get("target").and_then(|t| t.as_str()) {
            if target.starts_with("/mnt")
                || target.starts_with("/media")
                || target.starts_with("/run/media")
            {
                // skip EFI/boot
                if target.contains("EFI") || target == "/boot" {
                    // still allow /mnt/windowsEFI skip
                    if target.contains("EFI") {
                        continue;
                    }
                }
                let label = fs
                    .get("label")
                    .and_then(|l| l.as_str())
                    .unwrap_or("")
                    .to_string();
                let mut info = mount_from_path(Path::new(target));
                if !label.is_empty() {
                    info.label = label;
                }
                out.push(info);
            }
        }
        if let Some(children) = fs.get("children").and_then(|c| c.as_array()) {
            collect_findmnt(children, out);
        }
    }
}

fn mount_from_path(path: &Path) -> MountInfo {
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("disk")
        .to_string();
    let drive_letter = match name.as_str() {
        "windows_c" | "Windows_C" | "c" | "C" => Some('C'),
        "windows_d" | "Windows_D" | "d" | "D" => Some('D'),
        "windows_e" | "e" | "E" => Some('E'),
        "windows_f" | "f" | "F" => Some('F'),
        _ => None,
    };
    let label = if name.starts_with("windows_") {
        match drive_letter {
            Some(c) => format!("Windows {c}"),
            None => name.clone(),
        }
    } else {
        name
    };
    MountInfo {
        target: path.to_path_buf(),
        label,
        drive_letter,
    }
}

/// Format a path for display using config path style + mount table.
pub fn pretty_path(path: &Path, style: &PathStyle, mounts: &[MountInfo]) -> String {
    // Home first
    if let Some(home) = dirs::home_dir() {
        if let Ok(rest) = path.strip_prefix(&home) {
            if rest.as_os_str().is_empty() {
                return "~/".into();
            }
            return format!("~/{}", rest.display());
        }
    }

    // Longest matching mount prefix
    let mut best: Option<&MountInfo> = None;
    for m in mounts {
        if path.starts_with(&m.target) {
            if best.map(|b| m.target.components().count() > b.target.components().count())
                .unwrap_or(true)
            {
                best = Some(m);
            }
        }
    }

    if let Some(m) = best {
        let rest = path
            .strip_prefix(&m.target)
            .map(|r| r.display().to_string())
            .unwrap_or_default();
        let rest = if rest.is_empty() {
            String::new()
        } else {
            format!("/{rest}")
        };
        return match style {
            PathStyle::Drive => {
                if let Some(letter) = m.drive_letter {
                    format!("{letter}:{rest}")
                } else if !m.label.is_empty() {
                    format!("{}:{rest}", m.label)
                } else {
                    format!("{}{rest}", m.target.display())
                }
            }
            PathStyle::Label => {
                let label = if m.label.is_empty() {
                    m.target
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("disk")
                        .to_string()
                } else {
                    m.label.clone()
                };
                format!("{label}:{rest}")
            }
        };
    }

    path.display().to_string()
}

pub fn is_excluded(path: &Path, excludes: &[String]) -> bool {
    let s = path.to_string_lossy();
    path.components().any(|c| {
        let name = c.as_os_str().to_string_lossy();
        excludes.iter().any(|ex| {
            if ex.contains('/') {
                s.contains(ex.as_str())
            } else {
                name == *ex || name.eq_ignore_ascii_case(ex)
            }
        })
    })
}
