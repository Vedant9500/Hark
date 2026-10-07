//! Inline definitions (no LLM, no API key).
//!
//! Fast path (UI thread): pattern parse + **process-memory** cache / fail
//! only (no disk). Network path: `search_network` on a worker thread — may
//! read durable disk cache, then Wikipedia (title search + summary in one
//! request, so case and typos resolve) → DuckDuckGo Instant Answer (never
//! blocks GTK).
//! When `DefineConfig.enabled` is false: zero I/O.
//!
//! A total miss still owns the query with a fail row whose Enter opens a
//! web search for the term — the launcher never dead-ends.

use crate::config::{ConfigStore, DefineConfig};
use crate::providers::web::google_search_url;
use crate::providers::{Action, ResultKind, SearchResult};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) const DEFINE_SCORE: i64 = 100_000;
const DEFINE_PENDING_SCORE: i64 = 95_000;
const DEFINE_FAIL_SCORE: i64 = 80_000;
/// Definitions barely change; keep them a month.
const CACHE_TTL_SECS: u64 = 30 * 24 * 3600;
/// Per-entry stored-text cap (bytes). Long extracts are truncated before
/// they can persist durably.
const CACHE_ENTRY_MAX_BYTES: usize = 2000;
/// Avoid hammering free APIs on repeated misses.
const FAIL_CACHE_SECS: u64 = 90;
/// Offline/timeout misses are transient: retry soon after the network
/// returns, but still don't respawn a worker per keystroke while offline.
const UNREACHABLE_CACHE_SECS: u64 = 10;
const PENDING_PREFIX: &str = "define:pending:";
const OK_PREFIX: &str = "define:";
/// Single-char terms never hit the network: `define b` owns no useful
/// article and each prefix fetch used to head-of-line block the final term.
const MIN_TERM_CHARS: usize = 2;

/// A fetched definition plus optional Wikipedia article media. Image/page
/// URLs ride the durable cache so the preview can render article art
/// without a second API call.
#[derive(Debug, Clone)]
struct DefineHit {
    title: String,
    extract: String,
    source: String,
    image_url: Option<String>,
    page_url: Option<String>,
    description: Option<String>,
    /// Disambiguation page this article was picked from (`Python` →
    /// `Python (programming language)`): drives the "Other meanings" row.
    disambig: Option<String>,
}

/// One meaning listed on a Wikipedia disambiguation page ("did you mean").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Candidate {
    title: String,
    #[serde(default)]
    description: Option<String>,
}

/// Outcome of a successful network lookup.
#[derive(Debug, Clone)]
enum Lookup {
    /// One article / abstract answers the term.
    Hit(DefineHit),
    /// The term is ambiguous: `page` is the disambiguation page, the
    /// candidates its listed meanings (editorial order).
    Ambiguous {
        page: String,
        candidates: Vec<Candidate>,
    },
}

/// UI-safe article media for a resolved define row (mem cache only, no I/O).
#[derive(Debug, Clone)]
pub struct DefineMedia {
    pub image_url: String,
    pub page_url: Option<String>,
    pub description: Option<String>,
}

/// Process-local success cache (disk is the durable layer).
fn mem_ok() -> &'static Mutex<HashMap<String, CacheEntry>> {
    static M: OnceLock<Mutex<HashMap<String, CacheEntry>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Negative cache: key → (miss kind, fetched_at). Misses own the query with
/// a web-search fail row, so remember them briefly without disk writes.
fn mem_fail() -> &'static Mutex<HashMap<String, (MissKind, u64)>> {
    static M: OnceLock<Mutex<HashMap<String, (MissKind, u64)>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(HashMap::new()))
}

pub struct DefineProvider {
    config: Arc<ConfigStore>,
}

impl DefineProvider {
    pub fn new(config: Arc<ConfigStore>) -> Self {
        Self { config }
    }

    pub fn cfg(&self) -> DefineConfig {
        self.config.with(|c| c.define.clone())
    }

    pub fn is_enabled(&self) -> bool {
        self.config.with(|c| c.define.enabled)
    }

    /// Whether define owns this query: enabled, parses, and the term is
    /// within budget (max chars and min length). One gate for every caller
    /// (sync search, worker, debounce, icon, deep-search skip) so ownership
    /// never claims a query it shows no rows for (`define b` falls through
    /// to the web fallback instead of an empty list).
    pub fn should_handle(&self, query: &str) -> bool {
        self.lookup_query(query).is_some()
    }

    /// Parse once + apply every gate (enabled, term budget, min length).
    /// `None` means define does not own this query.
    fn lookup_query(&self, query: &str) -> Option<ParsedQuery> {
        let max_chars = self
            .config
            .with(|c| c.define.enabled.then_some(c.define.max_chars))?;
        let parsed = parse_query(query)?;
        let n = parsed.term.chars().count();
        (MIN_TERM_CHARS..=max_chars).contains(&n).then_some(parsed)
    }

    /// True when UI should spawn a worker: enabled, matches, term within
    /// budget, not already in **memory** cache or recent-fail.
    /// Disk is checked on the worker (`search_network`) so the UI thread
    /// never blocks on FS.
    pub fn needs_network(&self, query: &str) -> bool {
        let Some(p) = self.lookup_query(query) else {
            return false;
        };
        let key = cache_key(&p.term);
        cache_get_mem(&key).is_none() && fail_get(&key).is_none()
    }

    /// Blocking definition fetch (worker thread only). May read disk cache.
    /// Misses return a web-search fail row (never empty — the query is
    /// owned either way, so Enter always does something useful).
    pub fn search_network(&self, query: &str) -> Vec<SearchResult> {
        let Some(p) = self.lookup_query(query) else {
            return Vec::new();
        };
        let key = cache_key(&p.term);
        if let Some(e) = cache_get(&key) {
            return entry_results(&p, &web_query(&p, query), &e);
        }
        if let Some(kind) = fail_get(&key) {
            return vec![fail_result(&p.term, &web_query(&p, query), kind)];
        }
        match fetch_definition(&p.term) {
            Ok(lookup) => {
                // Show exactly what is cached so the first view and every
                // later (cached) view render the same rows.
                let e = cache_put(&key, &p.term, &lookup);
                fail_clear(&key);
                entry_results(&p, &web_query(&p, query), &e)
            }
            Err(kind) => {
                fail_put(&key, kind);
                vec![fail_result(&p.term, &web_query(&p, query), kind)]
            }
        }
    }

    /// UI-thread safe: memory cache hit, recent fail, or a "Defining…"
    /// placeholder. **No disk I/O** — durable cache loads on the worker.
    pub fn search(&self, query: &str) -> Vec<SearchResult> {
        let Some(p) = self.lookup_query(query) else {
            return Vec::new();
        };
        let key = cache_key(&p.term);
        if let Some(e) = cache_get_mem(&key) {
            return entry_results(&p, &web_query(&p, query), &e);
        }
        if let Some(kind) = fail_get(&key) {
            return vec![fail_result(&p.term, &web_query(&p, query), kind)];
        }
        vec![pending_result(&p.term)]
    }
}

/// What the user asked for — decides what Enter does on the answer row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    /// "What does it mean?" (`define X`, `X meaning`, `X full form` …):
    /// Enter copies the definition.
    Define,
    /// "Tell me about this person / place / thing" (`who is X`,
    /// `where is X`, `what is X`, `tell me about X`, `wiki X` …): Enter
    /// opens the article; the definition is still one Ctrl+C away.
    Lookup,
}

/// A parsed define query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedQuery {
    pub term: String,
    /// Explicit prefix (`define X`, `wiki X`): the term is taken verbatim —
    /// no stoplist, no article stripping.
    pub forced: bool,
    pub intent: Intent,
}

/// Parse a query into `(term, forced)`. See [`parse_query`].
pub fn parse_term(query: &str) -> Option<(String, bool)> {
    parse_query(query).map(|p| (p.term, p.forced))
}

/// Parse a define query.
///
/// Forced prefixes: `def `, `define `, `dict `, `dictionary ` (Define);
/// `wiki `, `wikipedia ` and the ` wikipedia` suffix (Lookup).
/// Define questions: `what does X mean`, `what does X stand for`,
/// `X full form`, `full form of X`, `X stands for`, `meaning of X`,
/// `X meaning`, `X means`.
/// Lookup questions: `what is/are X`, `what's X`, `who is/was/are/were X`,
/// `who's X`, `where is/are/was X`, `where's X`, `tell me about X`.
pub fn parse_query(query: &str) -> Option<ParsedQuery> {
    let q = query.trim();
    if q.is_empty() {
        return None;
    }
    // Autocorrect/phone keyboards type `what’s`; fold to ASCII so the
    // apostrophe shapes below match.
    let q = q.replace('\u{2019}', "'");
    // Trailing "?" is punctuation, not part of the term.
    let q = q.trim_end_matches('?').trim();
    if q.is_empty() {
        return None;
    }
    // Forced prefixes first (longest first so `define` wins over `def`).
    for (prefix, intent) in [
        ("dictionary ", Intent::Define),
        ("wikipedia ", Intent::Lookup),
        ("define ", Intent::Define),
        ("dict ", Intent::Define),
        ("wiki ", Intent::Lookup),
        ("def ", Intent::Define),
    ] {
        if q.get(..prefix.len())
            .is_some_and(|s| s.eq_ignore_ascii_case(prefix))
        {
            return forced(&q[prefix.len()..], intent);
        }
    }
    // `X wikipedia`. (No ` wiki` suffix: `arch wiki` / `nixos wiki` name
    // the project wikis, not a Wikipedia article.)
    if let Some(t) = strip_suffix_word(q, " wikipedia") {
        return forced(&t, Intent::Lookup);
    }
    let (term, intent) = parse_question_raw(q)?;
    let term = match intent {
        Intent::Lookup => strip_fact_suffix(&term),
        Intent::Define => term.as_str(),
    };
    let term = strip_indefinite_article(term);
    if is_non_definition_term(term) {
        return None;
    }
    Some(ParsedQuery {
        term: valid_term(term)?,
        forced: false,
        intent,
    })
}

fn forced(term: &str, intent: Intent) -> Option<ParsedQuery> {
    Some(ParsedQuery {
        term: valid_term(term.trim())?,
        forced: true,
        intent,
    })
}

/// Question shapes → raw term + intent (stoplist/article handling is the
/// caller's).
fn parse_question_raw(q: &str) -> Option<(String, Intent)> {
    let lower = q.to_ascii_lowercase();
    let after_prefix = |prefixes: &[&str]| {
        prefixes
            .iter()
            .find_map(|p| strip_prefix_word(&lower, p))
            .map(|after| q[after..].trim().to_string())
    };
    // Contractions first (no space after the wh-word).
    if let Some(t) = after_prefix(&["what's ", "whats "]) {
        return Some((t, Intent::Lookup));
    }
    if let Some(t) = after_prefix(&["who's ", "whos ", "where's ", "wheres "]) {
        return Some((t, Intent::Lookup));
    }
    if let Some(t) = after_prefix(&[
        "who is ",
        "who was ",
        "who are ",
        "who were ",
        "where is ",
        "where are ",
        "where was ",
        "tell me about ",
    ]) {
        return Some((t, Intent::Lookup));
    }
    // `what does X mean` / `what do X mean` / `what does X stand for` …
    if let Some(rest) = strip_prefix_word(&lower, "what ") {
        let rest_orig = &q[rest..];
        let rest_lower = &lower[rest..];
        for verb in ["does ", "do "] {
            if let Some(after) = strip_prefix_word(rest_lower, verb) {
                let term = rest_orig[after..].trim();
                if let Some(t) = strip_suffix_word(term, " stand for") {
                    return Some((t, Intent::Define));
                }
                if let Some(t) = strip_suffix_word(term, " mean") {
                    return Some((t, Intent::Define));
                }
                // `what does X` with no trailing verb word is not a question.
                return None;
            }
        }
        for verb in ["is ", "are "] {
            if let Some(after) = strip_prefix_word(rest_lower, verb) {
                return Some((rest_orig[after..].trim().to_string(), Intent::Lookup));
            }
        }
        return None;
    }
    if let Some(t) = after_prefix(&["full form of ", "meaning of "]) {
        return Some((t, Intent::Define));
    }
    // Suffix shapes on the whole query (`strip_suffix_word` is
    // case-insensitive already).
    for suffix in [" full form", " stands for", " meaning", " means"] {
        if let Some(t) = strip_suffix_word(q, suffix) {
            return Some((t, Intent::Define));
        }
    }
    None
}

/// `where was napoleon born` → `napoleon`, `where is petra located` →
/// `petra`. The trailing fact word is the question, not the subject — left
/// in, title search fuzzy-matches it into the wrong article
/// (`Napoleon Bonaparte (police officer)`). One word only, and never the
/// whole term.
fn strip_fact_suffix(term: &str) -> &str {
    const FACT_WORDS: &[&str] = &[
        " born",
        " located",
        " situated",
        " based",
        " from",
        " buried",
        " now",
        " today",
    ];
    for w in FACT_WORDS {
        if let Some(head) = term
            .len()
            .checked_sub(w.len())
            .filter(|&i| term.is_char_boundary(i) && term[i..].eq_ignore_ascii_case(w))
            .map(|i| term[..i].trim_end())
        {
            if !head.is_empty() {
                return head;
            }
        }
    }
    term
}

/// `a cpu` → `cpu`. Only indefinite articles: `the` is often part of a real
/// title (`The Who`), and Wikipedia redirects `The X` → `X` where it isn't.
fn strip_indefinite_article(term: &str) -> &str {
    let lower = term.to_ascii_lowercase();
    // Fixed phrases where the leading `a` is part of the term itself.
    const KEEP: &[&str] = &[
        "a priori",
        "a posteriori",
        "a fortiori",
        "a cappella",
        "a capella",
        "a la ",
        "a.k.a",
    ];
    if KEEP.iter().any(|k| lower.starts_with(k)) {
        return term;
    }
    for art in ["a ", "an "] {
        if lower.starts_with(art) && term.len() > art.len() {
            return term[art.len()..].trim_start();
        }
    }
    term
}

/// Question-shaped queries that are not asking for a definition
/// (`what is my ip`, `what's up`, `what is the time in tokyo`). Claiming
/// them would only produce a miss row and hide apps/files/web results.
fn is_non_definition_term(term: &str) -> bool {
    let t = term.trim().to_ascii_lowercase();
    const EXACT: &[&str] = &[
        "up",
        "new",
        "this",
        "that",
        "it",
        "going on",
        "happening",
        "wrong",
        "the time",
        "time",
        "the date",
        "date",
        "today",
        "the day",
        "the weather",
        "weather",
        "by any",
        "by all",
        "by no",
        "you",
        "yourself",
        "me",
        "here",
        "there",
    ];
    const PREFIX: &[&str] = &[
        "my ",
        "your ",
        "new in ",
        "the time in ",
        "time in ",
        "the weather in ",
        "weather in ",
        "the date in ",
    ];
    EXACT.contains(&t.as_str()) || PREFIX.iter().any(|p| t.starts_with(p))
}

/// ASCII case-insensitive prefix strip returning the byte offset of the rest.
/// `get` keeps this panic-free on multi-byte queries.
fn strip_prefix_word(lower: &str, prefix: &str) -> Option<usize> {
    if lower.get(..prefix.len()).is_some_and(|s| s == prefix) {
        Some(prefix.len())
    } else {
        None
    }
}

/// ASCII case-insensitive suffix strip returning the leading part.
fn strip_suffix_word(orig: &str, suffix: &str) -> Option<String> {
    if orig.len() >= suffix.len() && orig[orig.len() - suffix.len()..].eq_ignore_ascii_case(suffix)
    {
        Some(orig[..orig.len() - suffix.len()].to_string())
    } else {
        None
    }
}

/// A term is 1+ letters, bounded, and never a path/glob/URL.
fn valid_term(term: &str) -> Option<String> {
    let t = term.trim();
    if t.is_empty() || t.chars().count() > 80 {
        return None;
    }
    if !t.chars().any(|c| c.is_alphabetic()) {
        return None;
    }
    if t.contains('/') || t.contains('*') || t.contains("://") {
        return None;
    }
    Some(t.to_string())
}

pub fn is_define_query(query: &str, cfg: &DefineConfig) -> bool {
    if !cfg.enabled {
        return false;
    }
    let Some(p) = parse_query(query) else {
        return false;
    };
    let n = p.term.chars().count();
    (MIN_TERM_CHARS..=cfg.max_chars).contains(&n)
}

/// Text the web-search row searches for: the whole question (`what is the
/// time in tokyo`; `define X` lands on the engine's dictionary box), except
/// `wiki X` lookups, where `wiki` is a hark command — search the term
/// (minus a `(disambiguation)` suffix from an "Other meanings" hop).
fn web_query(p: &ParsedQuery, query: &str) -> String {
    if p.forced && p.intent == Intent::Lookup {
        let t = p.term.trim();
        let cut = t.len().saturating_sub(DISAMBIG_SUFFIX.len());
        if t.is_char_boundary(cut) && t[cut..].eq_ignore_ascii_case(DISAMBIG_SUFFIX) {
            return t[..cut].trim().to_string();
        }
        return t.to_string();
    }
    query.to_string()
}

/// Rows for a cached lookup: the article (plus an "Other meanings" row
/// when it was picked from a disambiguation page), or the "did you mean"
/// list for an ambiguous term. `web_q` is what the web row searches.
fn entry_results(p: &ParsedQuery, web_q: &str, e: &CacheEntry) -> Vec<SearchResult> {
    if !e.candidates.is_empty() {
        return candidate_results(&p.term, web_q, &e.title, &e.candidates);
    }
    let mut out = vec![ok_result(&p.term, e, p.intent)];
    if let Some(page) = e.disambig.as_deref() {
        out.push(other_meanings_result(&p.term, page));
    }
    out
}

/// Article row. The full paragraph is the `subtitle` and renders inline in
/// the list; the preview pane shows the Wikipedia image when the summary
/// carries one. Enter copies the paragraph for `Define` questions and opens
/// the article for `Lookup` questions (people, places, things) — Ctrl+C
/// still copies there.
fn ok_result(term: &str, e: &CacheEntry, intent: Intent) -> SearchResult {
    let para = collapse_ws(&e.extract);
    let action = match (intent, e.page_url.as_deref()) {
        (Intent::Lookup, Some(url)) => Action::OpenUrl(url.to_string()),
        _ => Action::Copy(para.clone()),
    };
    SearchResult {
        id: format!("{OK_PREFIX}{}", cache_key(term)),
        title: format!("{} · {}", e.title, e.source),
        subtitle: para,
        kind: ResultKind::Define,
        score: DEFINE_SCORE,
        icon: Some("accessories-dictionary".into()),
        action,
        conversion: None,
        matched: None,
    }
}

/// Query that lists every meaning of a disambiguation page (`wiki Python
/// (disambiguation)`); the lookup forces the "did you mean" list for it.
fn disambiguation_query(page: &str) -> String {
    let page = page.trim();
    if page.to_ascii_lowercase().ends_with(DISAMBIG_SUFFIX) {
        format!("wiki {page}")
    } else {
        format!("wiki {page}{DISAMBIG_SUFFIX}")
    }
}

const DISAMBIG_SUFFIX: &str = " (disambiguation)";

fn other_meanings_result(term: &str, page: &str) -> SearchResult {
    let name = page
        .strip_suffix(DISAMBIG_SUFFIX)
        .unwrap_or(page)
        .trim()
        .to_string();
    SearchResult {
        id: format!("define:more:{:016x}", fnv1a64(&normalized_key(term))),
        title: format!("Other meanings of “{name}”"),
        subtitle: "Enter to list them".into(),
        kind: ResultKind::Define,
        score: DEFINE_SCORE - 1,
        icon: Some("accessories-dictionary".into()),
        action: Action::SetQuery(disambiguation_query(page)),
        conversion: None,
        matched: None,
    }
}

/// "Did you mean" rows for an ambiguous term, in the disambiguation page's
/// editorial order. Enter looks the chosen meaning up (`wiki <title>`); a
/// final row still offers the web search.
fn candidate_results(
    term: &str,
    query: &str,
    page: &str,
    candidates: &[Candidate],
) -> Vec<SearchResult> {
    let name = page.strip_suffix(DISAMBIG_SUFFIX).unwrap_or(page).trim();
    let mut out: Vec<SearchResult> = candidates
        .iter()
        .take(MAX_CANDIDATES)
        .enumerate()
        .map(|(i, c)| SearchResult {
            id: format!("define:dym:{:016x}", fnv1a64(&c.title)),
            title: c.title.clone(),
            subtitle: match c.description.as_deref() {
                Some(d) => format!("“{name}” may refer to · {d}"),
                None => format!("“{name}” may refer to"),
            },
            kind: ResultKind::Define,
            score: DEFINE_SCORE - i as i64,
            icon: Some("accessories-dictionary".into()),
            action: Action::SetQuery(format!("wiki {}", c.title)),
            conversion: None,
            matched: None,
        })
        .collect();
    let mut web = fail_result(term, query, MissKind::NotFound);
    web.title = "Search the web instead".into();
    out.push(web);
    out
}

/// Meanings shown for an ambiguous term (rows stay scannable; the web row
/// covers the long tail).
const MAX_CANDIDATES: usize = 6;

fn pending_result(term: &str) -> SearchResult {
    SearchResult {
        id: format!("{PENDING_PREFIX}{:016x}", fnv1a64(&normalized_key(term))),
        title: "Defining…".into(),
        subtitle: term.to_string(),
        kind: ResultKind::Define,
        score: DEFINE_PENDING_SCORE,
        icon: Some("accessories-dictionary".into()),
        action: Action::Copy(term.to_string()),
        conversion: None,
        matched: None,
    }
}

/// Misses never dead-end: Enter opens a web search for the **whole query**
/// (`what is the time in tokyo`, not just `the time in tokyo`; `define X`
/// also lands on the engine's dictionary box). Offline misses say so
/// instead of claiming the term has no definition.
fn fail_result(term: &str, query: &str, kind: MissKind) -> SearchResult {
    let query = normalized_key(query.trim().trim_end_matches('?'));
    let query = if query.is_empty() {
        term.to_string()
    } else {
        query
    };
    let title = match kind {
        MissKind::NotFound => "No definition found",
        MissKind::Unreachable => "Couldn't reach definition sources",
    };
    SearchResult {
        id: format!("define:fail:{:016x}", fnv1a64(&normalized_key(term))),
        title: title.into(),
        subtitle: format!("Enter to search the web for “{query}”"),
        kind: ResultKind::Command,
        score: DEFINE_FAIL_SCORE,
        icon: Some("applications-internet".into()),
        action: Action::OpenUrl(google_search_url(&query)),
        conversion: None,
        matched: None,
    }
}

pub fn is_pending_result(r: &SearchResult) -> bool {
    r.id.starts_with(PENDING_PREFIX)
}

/// Single-space the extract so rows and the reader show one clean paragraph.
fn collapse_ws(extract: &str) -> String {
    extract.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ── HTTP (worker only) ──────────────────────────────────────────────────────

/// Why a lookup produced no definition. Drives the miss row's wording and
/// how long the miss is remembered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MissKind {
    /// A backend answered, but had nothing usable (404, disambiguation,
    /// empty abstract).
    NotFound,
    /// No backend answered (offline, DNS, timeout, 429/5xx). Transient:
    /// cached only briefly and never reported as "no definition".
    Unreachable,
}

/// Map an `http` helper error string to a miss kind. `HTTP 4xx` (except
/// 429) means the server answered "no such page"; everything else is
/// transport trouble or a temporary server-side failure.
fn classify_http_err(e: &str) -> MissKind {
    match e
        .strip_prefix("HTTP ")
        .and_then(|c| c.trim().parse::<u16>().ok())
    {
        Some(code) if (400..500).contains(&code) && code != 429 => MissKind::NotFound,
        _ => MissKind::Unreachable,
    }
}

/// Wikipedia first, DDG Instant Answer as backup. The miss is
/// `NotFound` when any backend answered, `Unreachable` only when none did.
fn fetch_definition(term: &str) -> Result<Lookup, MissKind> {
    let wiki = match wikipedia_lookup(term) {
        Ok(lookup) => return Ok(lookup),
        Err(k) => k,
    };
    match ddg_abstract(term) {
        Ok(hit) => Ok(Lookup::Hit(hit)),
        Err(MissKind::Unreachable) if wiki == MissKind::Unreachable => Err(MissKind::Unreachable),
        Err(_) => Err(MissKind::NotFound),
    }
}

/// Result of resolving a term against Wikipedia's title search.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Resolved {
    /// Use this article. `via_disambig` names the disambiguation page it
    /// was picked from (`Python` → `Python (programming language)`).
    Article {
        title: String,
        via_disambig: Option<String>,
    },
    /// Ambiguous: list this disambiguation page's meanings.
    Ambiguous(String),
}

/// Resolve the term, then fetch either the article summary or the
/// disambiguation page's meanings ("did you mean").
fn wikipedia_lookup(term: &str) -> Result<Lookup, MissKind> {
    match wikipedia_resolve_title(term)? {
        Resolved::Article {
            title,
            via_disambig,
        } => {
            let mut hit = wikipedia_summary(&title)?;
            hit.disambig = via_disambig;
            Ok(Lookup::Hit(hit))
        }
        Resolved::Ambiguous(page) => {
            let candidates = wikipedia_disambig_candidates(&page)?;
            if candidates.is_empty() {
                return Err(MissKind::NotFound);
            }
            Ok(Lookup::Ambiguous { page, candidates })
        }
    }
}

/// Fetch the REST summary for an exact (already resolved) title.
///
/// The REST `page/summary/{title}` endpoint needs the exact title (only the
/// first letter is case-folded), so lowercase names (`linus sebastian`)
/// and typos 404'd when the raw term was used. Resolving first fixes that;
/// the summary is still fetched from REST because its extract is cleaned
/// (no pronunciation/IPA/birth-date parentheticals), unlike the action
/// API's plain-text extracts.
fn wikipedia_summary(title: &str) -> Result<DefineHit, MissKind> {
    // Spaces → underscores (canonical article form), then path-encode.
    let title_path = crate::providers::web::encode_query(&title.replace(' ', "_"));
    let url = format!("https://en.wikipedia.org/api/rest_v1/page/summary/{title_path}");
    let bytes = crate::providers::http::get_bytes(&url).map_err(|e| classify_http_err(&e))?;
    parse_wikipedia_body(&bytes).map_err(|_| MissKind::NotFound)
}

/// `generator=prefixsearch` is the search box's title completion:
/// case-insensitive, redirect-aware (`cpu` → Central processing unit),
/// typo-tolerant (`linux sebastian` → Linus Sebastian) and ranked. Only
/// rank + disambiguation flags are requested, keeping the reply tiny.
fn wikipedia_resolve_title(term: &str) -> Result<Resolved, MissKind> {
    let bytes = crate::providers::http::get_bytes_query(
        "https://en.wikipedia.org/w/api.php",
        &[
            ("action", "query"),
            ("format", "json"),
            ("formatversion", "2"),
            ("generator", "prefixsearch"),
            ("gpssearch", term),
            ("gpsnamespace", "0"),
            ("gpslimit", WIKI_CANDIDATES),
            ("redirects", "1"),
            ("prop", "pageprops"),
            ("ppprop", "disambiguation"),
        ],
    )
    .map_err(|e| classify_http_err(&e))?;
    let list_all = term.to_ascii_lowercase().ends_with(DISAMBIG_SUFFIX);
    parse_wikipedia_search(&bytes, list_all).map_err(|_| MissKind::NotFound)
}

/// Candidates per lookup: the top match plus room for its qualified senses
/// (`Python (programming language)`) when the top is a disambiguation page.
const WIKI_CANDIDATES: &str = "5";

/// Meanings listed on a disambiguation page, from its wikitext (one small
/// request). Wikitext keeps the editors' order — most common meanings first
/// — and each entry line already carries a short description.
fn wikipedia_disambig_candidates(page: &str) -> Result<Vec<Candidate>, MissKind> {
    let bytes = crate::providers::http::get_bytes_query(
        "https://en.wikipedia.org/w/api.php",
        &[
            ("action", "parse"),
            ("format", "json"),
            ("formatversion", "2"),
            ("prop", "wikitext"),
            ("redirects", "1"),
            ("page", page),
        ],
    )
    .map_err(|e| classify_http_err(&e))?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| MissKind::NotFound)?;
    let text = v
        .get("parse")
        .and_then(|p| p.get("wikitext"))
        .and_then(|w| w.as_str())
        .ok_or(MissKind::NotFound)?;
    Ok(parse_disambig_wikitext(text))
}

/// Entry lines of a disambiguation page → candidates.
///
/// Only top-level bullets that *start* with a link are meanings
/// (`* [[Linus Tech Tips]], a Canadian tech-related YouTube channel`);
/// lines that merely mention a link (`* Softwin AVX, former name of
/// [[Bitdefender]]`) and nested bullets are skipped. Parsing stops at the
/// "See also" section.
fn parse_disambig_wikitext(text: &str) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('=') {
            let heading = line.trim_matches('=').trim().to_ascii_lowercase();
            if heading == "see also" {
                break;
            }
            continue;
        }
        let Some(rest) = line.strip_prefix('*') else {
            continue;
        };
        if rest.starts_with('*') || rest.starts_with(':') {
            continue;
        }
        // Italic/bold wrappers around the link (`''[[Avengers vs. X-Men]]''`).
        let rest = rest.trim_start().trim_start_matches('\'');
        let Some(link) = rest.strip_prefix("[[") else {
            continue;
        };
        let Some(close) = link.find("]]") else {
            continue;
        };
        let inner = &link[..close];
        let target = inner.split('|').next().unwrap_or("");
        let target = target.split('#').next().unwrap_or("").trim();
        // Namespaced links (File:, Category:, wikt:) are not meanings.
        if target.is_empty() || target.contains(':') || target.chars().count() > 120 {
            continue;
        }
        if out.iter().any(|c| c.title.eq_ignore_ascii_case(target)) {
            continue;
        }
        let tail = link[close + 2..].trim_start_matches('\'');
        let description = clean_wikitext(tail)
            .trim_start_matches([',', ';', ':', '-', '–', '—'])
            .trim()
            .to_string();
        let description = (!description.is_empty()).then(|| {
            let d: String = description.chars().take(120).collect();
            d
        });
        out.push(Candidate {
            title: target.to_string(),
            description,
        });
        if out.len() >= MAX_CANDIDATES {
            break;
        }
    }
    out
}

/// Plain text from a wikitext fragment: `[[a|b]]` → `b`, `[[a]]` → `a`,
/// templates and HTML tags dropped (tag contents kept), quote marks
/// removed, whitespace collapsed.
fn clean_wikitext(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix("[[") {
            let end = r.find("]]").unwrap_or(r.len());
            let inner = &r[..end];
            out.push_str(inner.rsplit('|').next().unwrap_or(inner));
            rest = r.get(end + 2..).unwrap_or("");
        } else if let Some(r) = rest.strip_prefix("{{") {
            let end = r.find("}}").unwrap_or(r.len());
            rest = r.get(end + 2..).unwrap_or("");
        } else if rest.starts_with('<') {
            let end = rest.find('>').map(|i| i + 1).unwrap_or(rest.len());
            rest = &rest[end..];
        } else if let Some(r) = rest.strip_prefix("''") {
            rest = r.trim_start_matches('\'');
        } else {
            let ch = rest.chars().next().unwrap_or(' ');
            out.push(ch);
            rest = &rest[ch.len_utf8()..];
        }
    }
    collapse_ws(&out)
}

/// True for resolved article rows (excludes `Defining…` pending, miss,
/// "did you mean" and "other meanings" rows). Article ids are
/// `define:<16 hex>`; every other define row has a word segment.
pub fn is_ok_result(r: &SearchResult) -> bool {
    r.kind == ResultKind::Define && r.id.strip_prefix(OK_PREFIX).is_some_and(is_hex_key)
}

fn is_hex_key(key: &str) -> bool {
    key.len() == 16 && key.chars().all(|c| c.is_ascii_hexdigit())
}

/// Mem-cache-only media lookup for a resolved row id (safe on GTK main).
/// Returns `None` for pending/miss rows, cache misses, and non-Wikimedia
/// image hosts (planted cache entries must never turn the preview into a
/// generic URL fetcher).
pub fn lookup_media_by_id(result_id: &str) -> Option<DefineMedia> {
    let key = result_id.strip_prefix(OK_PREFIX)?;
    // Only article ids (fixed 16-hex keys); pending/miss/list rows never match.
    if !is_hex_key(key) {
        return None;
    }
    let hit = cache_get_mem(key)?;
    let image_url = hit.image_url?;
    if !is_allowed_image_url(&image_url) {
        return None;
    }
    Some(DefineMedia {
        image_url,
        page_url: hit.page_url,
        description: hit.description,
    })
}

fn is_allowed_image_url(url: &str) -> bool {
    url.starts_with("https://upload.wikimedia.org/")
        || url.starts_with("https://thumb.wikimedia.org/")
}

/// Pick the best article title from a `generator=prefixsearch` response.
///
/// Pages arrive unordered; `index` is the search rank. The top match wins
/// unless it is a disambiguation page, in which case the best-ranked
/// *qualified sense* of the same name is used (`Python` → `Python
/// (programming language)`, `Mercury` → `Mercury (planet)`). Unrelated
/// lower-ranked titles (`LTT` → `LTT 1445`) are never substituted: the
/// term is `Ambiguous` and its meanings are listed instead. `list_all`
/// (term ends in `(disambiguation)`) always lists.
fn parse_wikipedia_search(bytes: &[u8], list_all: bool) -> Result<Resolved, ()> {
    let v: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| ())?;
    let mut pages: Vec<&serde_json::Value> = v
        .get("query")
        .and_then(|q| q.get("pages"))
        .and_then(|p| p.as_array())
        .ok_or(())?
        .iter()
        .collect();
    pages.sort_by_key(|p| p.get("index").and_then(|i| i.as_u64()).unwrap_or(u64::MAX));
    let top = *pages.first().ok_or(())?;
    let top_title = page_title(top).ok_or(())?;
    if !is_disambiguation(top) {
        return Ok(Resolved::Article {
            title: top_title.to_string(),
            via_disambig: None,
        });
    }
    // `X (disambiguation)` asks for the list itself — never auto-pick.
    if list_all {
        return Ok(Resolved::Ambiguous(top_title.to_string()));
    }
    let qualified = format!("{} (", top_title.to_lowercase());
    let sense = pages
        .iter()
        .skip(1)
        .filter(|p| !is_disambiguation(p))
        .filter_map(|p| page_title(p))
        .find(|t| t.to_lowercase().starts_with(&qualified));
    Ok(match sense {
        Some(t) => Resolved::Article {
            title: t.to_string(),
            via_disambig: Some(top_title.to_string()),
        },
        None => Resolved::Ambiguous(top_title.to_string()),
    })
}

fn page_title(p: &serde_json::Value) -> Option<&str> {
    p.get("title")
        .and_then(|t| t.as_str())
        .map(str::trim)
        .filter(|t| !t.is_empty())
}

/// Disambiguation pages carry no usable definition ("X may refer to:").
fn is_disambiguation(p: &serde_json::Value) -> bool {
    p.get("pageprops")
        .and_then(|pp| pp.get("disambiguation"))
        .is_some()
}

fn parse_wikipedia_body(bytes: &[u8]) -> Result<DefineHit, ()> {
    let v: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| ())?;
    // Disambiguation pages carry no usable definition — let DDG try.
    if v.get("type").and_then(|t| t.as_str()) == Some("disambiguation") {
        return Err(());
    }
    let extract = v.get("extract").and_then(|e| e.as_str()).unwrap_or("");
    if extract.trim().is_empty() {
        return Err(());
    }
    let title = v.get("title").and_then(|t| t.as_str()).unwrap_or("").trim();
    if title.is_empty() {
        return Err(());
    }
    // Prefer the caller-sized thumbnail (330px fits the 280px preview
    // stage); fall back to the original when no thumbnail is listed.
    let image_url = v
        .get("thumbnail")
        .and_then(|t| t.get("source"))
        .and_then(|s| s.as_str())
        .or_else(|| {
            v.get("originalimage")
                .and_then(|t| t.get("source"))
                .and_then(|s| s.as_str())
        })
        .filter(|u| is_allowed_image_url(u))
        .map(|s| s.to_string());
    let page_url = v
        .get("content_urls")
        .and_then(|c| c.get("desktop"))
        .and_then(|d| d.get("page"))
        .and_then(|p| p.as_str())
        .filter(|u| {
            u.starts_with("https://en.wikipedia.org/wiki/")
                || u.starts_with("http://en.wikipedia.org/wiki/")
        })
        .map(|s| s.to_string());
    let description = v
        .get("description")
        .and_then(|d| d.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    Ok(DefineHit {
        title: title.to_string(),
        extract: extract.trim().to_string(),
        source: "Wikipedia".into(),
        image_url,
        page_url,
        description,
        disambig: None,
    })
}

fn ddg_abstract(term: &str) -> Result<DefineHit, MissKind> {
    let bytes = crate::providers::http::get_bytes_query(
        "https://api.duckduckgo.com/",
        &[
            ("q", term),
            ("format", "json"),
            ("no_html", "1"),
            ("skip_disambig", "1"),
        ],
    )
    .map_err(|e| classify_http_err(&e))?;
    parse_ddg_body(&bytes).map_err(|_| MissKind::NotFound)
}

fn parse_ddg_body(bytes: &[u8]) -> Result<DefineHit, ()> {
    let v: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| ())?;
    let text = v
        .get("AbstractText")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .trim();
    if text.is_empty() {
        return Err(());
    }
    let source = v
        .get("AbstractSource")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .trim();
    let source = if source.is_empty() {
        "DuckDuckGo".to_string()
    } else {
        source.to_string()
    };
    // Heading doubles as the display title when present.
    let heading = v
        .get("Heading")
        .and_then(|h| h.as_str())
        .unwrap_or("")
        .trim();
    let title = if heading.is_empty() {
        source.clone()
    } else {
        heading.to_string()
    };
    Ok(DefineHit {
        title,
        extract: text.to_string(),
        source,
        image_url: None,
        page_url: None,
        description: None,
        disambig: None,
    })
}

// ── Cache ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheEntry {
    term: String,
    title: String,
    extract: String,
    source: String,
    fetched_at: u64,
    #[serde(default)]
    image_url: Option<String>,
    #[serde(default)]
    page_url: Option<String>,
    #[serde(default)]
    description: Option<String>,
    /// Disambiguation page the article was picked from ("Other meanings").
    #[serde(default)]
    disambig: Option<String>,
    /// Non-empty for an ambiguous term: its meanings ("did you mean").
    /// `title` is then the disambiguation page and `extract` is empty.
    #[serde(default)]
    candidates: Vec<Candidate>,
}

impl CacheEntry {
    /// An entry answers the term with an article or a meanings list.
    fn is_usable(&self) -> bool {
        !self.extract.trim().is_empty() || !self.candidates.is_empty()
    }
}

fn cache_dir() -> PathBuf {
    // No `/tmp` fallback: a world-writable directory lets any local user
    // plant entries that are later copied to the clipboard.
    dirs::cache_dir()
        .or_else(dirs::home_dir)
        .map(|d| d.join(".cache/hark/define"))
        .unwrap_or_default()
}

/// True when the cache directory is owned by this user and locked to 0700
/// (mirrors the fx.rs / translate.rs disk guards).
#[cfg(unix)]
fn cache_dir_trusted(dir: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Ok(m) = fs::metadata(dir) else {
        return false;
    };
    m.is_dir() && m.mode() & 0o777 == 0o700 && m.uid() == current_euid().unwrap_or(u32::MAX)
}

#[cfg(unix)]
fn current_euid() -> Option<u32> {
    extern "C" {
        fn geteuid() -> u32;
    }
    // SAFETY: geteuid takes no args, always succeeds, no side effects.
    Some(unsafe { geteuid() })
}

#[cfg(not(unix))]
fn cache_dir_trusted(_dir: &Path) -> bool {
    true
}

fn cache_path(key: &str) -> PathBuf {
    cache_dir().join(format!("{key}.json"))
}

/// Process-memory only — safe on the GTK main thread (no FS).
fn cache_get_mem(key: &str) -> Option<CacheEntry> {
    let Ok(g) = mem_ok().lock() else {
        return None;
    };
    let e = g.get(key)?;
    if now_secs().saturating_sub(e.fetched_at) > CACHE_TTL_SECS || !e.is_usable() {
        return None;
    }
    Some(e.clone())
}

/// Mem first, then durable disk (promotes into mem). **Worker thread only.**
#[cfg(unix)]
fn read_cache_file(path: &Path) -> Option<String> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    #[cfg(target_os = "linux")]
    const O_NOFOLLOW: i32 = 0o400_000;
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    const O_NOFOLLOW: i32 = 0o400;
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "ios")))]
    const O_NOFOLLOW: i32 = 0;
    let mut f = fs::OpenOptions::new()
        .read(true)
        .custom_flags(O_NOFOLLOW)
        .open(path)
        .ok()?;
    let mut data = String::new();
    f.read_to_string(&mut data).ok()?;
    Some(data)
}

#[cfg(not(unix))]
fn read_cache_file(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok()
}

fn cache_get(key: &str) -> Option<CacheEntry> {
    if let Some(e) = cache_get_mem(key) {
        return Some(e);
    }
    let path = cache_path(key);
    if !cache_dir_trusted(path.parent()?) {
        return None;
    }
    let data = read_cache_file(&path)?;
    let e: CacheEntry = serde_json::from_str(&data).ok()?;
    // The key is a hash of the normalized term; recomputing it from the
    // entry must reproduce the same key or the entry is planted.
    if cache_key(&e.term) != key {
        return None;
    }
    if now_secs().saturating_sub(e.fetched_at) > CACHE_TTL_SECS {
        let _ = fs::remove_file(&path);
        return None;
    }
    if e.term.len() > CACHE_ENTRY_MAX_BYTES || e.extract.len() > CACHE_ENTRY_MAX_BYTES {
        let _ = fs::remove_file(&path);
        return None;
    }
    // Planted cache files must not turn the preview into a generic URL
    // fetcher: drop oversized or off-host media, keep the text.
    let mut e = e;
    sanitize_entry(&mut e);
    if !e.is_usable() {
        return None;
    }
    if let Ok(mut g) = mem_ok().lock() {
        g.insert(key.to_string(), e.clone());
    }
    Some(e)
}

/// Longest prefix of `s` that fits in `max` bytes without splitting a
/// UTF-8 char. Char-count truncation is not enough: 2000 chars of Hindi or
/// accented text is well over 2000 bytes.
fn truncate_bytes(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Store a hit in the memory cache and (when the dir is trusted) on disk.
/// Returns the stored entry — callers render it so the first view matches
/// later cached views (same capped extract).
/// Metadata caps shared by fresh fetches and disk reads. Media must stay
/// on Wikimedia hosts (the preview fetches `image_url`; a planted entry
/// must never turn it into a generic URL fetcher), and list titles feed
/// the search box via `SetQuery`, so they are bounded single-line text.
fn sanitize_entry(e: &mut CacheEntry) {
    if e.image_url
        .as_deref()
        .is_some_and(|u| u.len() > 512 || !is_allowed_image_url(u))
    {
        e.image_url = None;
    }
    if e.page_url.as_deref().is_some_and(|u| {
        u.len() > 512
            || !(u.starts_with("https://en.wikipedia.org/wiki/")
                || u.starts_with("http://en.wikipedia.org/wiki/"))
    }) {
        e.page_url = None;
    }
    e.description = e
        .description
        .as_deref()
        .map(|d| truncate_bytes(d.trim(), 256).to_string())
        .filter(|d| !d.is_empty());
    if e.disambig.as_deref().is_some_and(|t| !is_sane_title(t)) {
        e.disambig = None;
    }
    e.candidates.retain(|c| is_sane_title(&c.title));
    e.candidates.truncate(MAX_CANDIDATES);
    for c in &mut e.candidates {
        c.description = c
            .description
            .as_deref()
            .filter(|d| !d.chars().any(char::is_control))
            .map(|d| truncate_bytes(d.trim(), 256).to_string())
            .filter(|d| !d.is_empty());
    }
}

fn is_sane_title(t: &str) -> bool {
    !t.trim().is_empty() && t.len() <= 256 && !t.chars().any(char::is_control)
}

fn cache_put(key: &str, term: &str, lookup: &Lookup) -> CacheEntry {
    let mut e = match lookup {
        Lookup::Hit(hit) => CacheEntry {
            term: term.into(),
            title: hit.title.clone(),
            extract: hit.extract.clone(),
            source: hit.source.clone(),
            fetched_at: now_secs(),
            image_url: hit.image_url.clone(),
            page_url: hit.page_url.clone(),
            description: hit.description.clone(),
            disambig: hit.disambig.clone(),
            candidates: Vec::new(),
        },
        Lookup::Ambiguous { page, candidates } => CacheEntry {
            term: term.into(),
            title: page.clone(),
            extract: String::new(),
            source: "Wikipedia".into(),
            fetched_at: now_secs(),
            image_url: None,
            page_url: None,
            description: None,
            disambig: None,
            candidates: candidates.clone(),
        },
    };
    // Truncate the extract before storing: oversized text must not linger,
    // and the byte cap must hold for non-ASCII text too.
    e.extract = truncate_bytes(&e.extract, CACHE_ENTRY_MAX_BYTES).to_string();
    sanitize_entry(&mut e);
    if let Ok(mut g) = mem_ok().lock() {
        g.insert(key.to_string(), e.clone());
        const MAX_MEM: usize = 256;
        if g.len() > MAX_MEM {
            let mut keys: Vec<(&str, u64)> =
                g.iter().map(|(k, v)| (k.as_str(), v.fetched_at)).collect();
            keys.sort_unstable_by_key(|(_, ts)| *ts);
            let remove_n = g.len() - MAX_MEM;
            let to_remove: Vec<String> = keys
                .into_iter()
                .take(remove_n)
                .map(|(k, _)| k.to_string())
                .collect();
            for k in to_remove {
                g.remove(&k);
            }
        }
    }
    cache_write_disk(key, &e);
    e
}

/// Durable layer: only into an owned 0700 dir (never a shared/planted one).
fn cache_write_disk(key: &str, e: &CacheEntry) {
    let dir = cache_dir();
    if dir.as_os_str().is_empty() {
        return;
    }
    let _ = fs::create_dir_all(&dir);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
    }
    if !cache_dir_trusted(&dir) {
        return;
    }
    if let Ok(data) = serde_json::to_string(e) {
        let path = cache_path(key);
        let tmp = path.with_extension("json.tmp");
        if fs::write(&tmp, data).is_ok() {
            let _ = fs::rename(&tmp, path);
        } else {
            let _ = fs::remove_file(&tmp);
        }
    }
    maybe_sweep_cache(&dir);
}

fn fail_get(key: &str) -> Option<MissKind> {
    let Ok(mut g) = mem_fail().lock() else {
        return None;
    };
    let (kind, at) = *g.get(key)?;
    let ttl = match kind {
        MissKind::NotFound => FAIL_CACHE_SECS,
        MissKind::Unreachable => UNREACHABLE_CACHE_SECS,
    };
    if now_secs().saturating_sub(at) > ttl {
        g.remove(key);
        return None;
    }
    Some(kind)
}

fn fail_put(key: &str, kind: MissKind) {
    if let Ok(mut g) = mem_fail().lock() {
        let now = now_secs();
        g.insert(key.to_string(), (kind, now));
        const MAX_FAIL: usize = 64;
        if g.len() > MAX_FAIL {
            let mut keys: Vec<(u64, String)> =
                g.iter().map(|(k, (_, at))| (*at, k.clone())).collect();
            keys.sort_by_key(|(ts, _)| *ts);
            let remove_n = g.len() - MAX_FAIL;
            for (_, k) in keys.into_iter().take(remove_n) {
                g.remove(&k);
            }
        }
    }
}

fn fail_clear(key: &str) {
    if let Ok(mut g) = mem_fail().lock() {
        g.remove(key);
    }
}

fn maybe_sweep_cache(dir: &Path) {
    let Ok(rd) = fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(std::path::PathBuf, u64, u64)> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("json"))
        .map(|p| {
            let meta = fs::metadata(&p).ok();
            let sz = meta.as_ref().map(|m| m.len()).unwrap_or(0);
            let mtime = meta
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            (p, sz, mtime)
        })
        .collect();
    const MAX: usize = 500;
    const MAX_TOTAL_BYTES: u64 = 2 * 1024 * 1024;
    let total: u64 = files.iter().map(|(_, s, _)| *s).sum();
    if files.len() <= MAX && total <= MAX_TOTAL_BYTES {
        return;
    }
    files.sort_unstable_by_key(|(_, _, mtime)| *mtime);
    let mut remove_n = files.len().saturating_sub(MAX);
    let mut bytes_after = total;
    if total > MAX_TOTAL_BYTES {
        for (i, (_, sz, _)) in files.iter().enumerate() {
            if bytes_after <= MAX_TOTAL_BYTES {
                break;
            }
            remove_n = remove_n.max(i + 1);
            bytes_after = bytes_after.saturating_sub(*sz);
        }
    }
    for (p, _, _) in files.into_iter().take(remove_n) {
        let _ = fs::remove_file(p);
    }
}

fn normalized_key(term: &str) -> String {
    term.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn cache_key(term: &str) -> String {
    // Case-insensitive: `SIMD` and `simd` are one article.
    // `v2` namespace: the image upgrade refetches pre-image cache entries
    // once (their recompute check below fails and the file is dropped).
    let norm = format!("v2:{}", normalized_key(term).to_lowercase());
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in norm.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn fnv1a64(s: &str) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DefineConfig;

    fn cfg() -> DefineConfig {
        DefineConfig::default()
    }

    fn term_of(q: &str) -> Option<String> {
        parse_term(q).map(|(t, _)| t)
    }

    #[test]
    fn question_shapes_parse() {
        assert_eq!(term_of("what does SIMD mean").as_deref(), Some("SIMD"));
        assert_eq!(term_of("What Does AVX Mean").as_deref(), Some("AVX"));
        assert_eq!(term_of("what do GPUs mean").as_deref(), Some("GPUs"));
        assert_eq!(term_of("what is SIMD").as_deref(), Some("SIMD"));
        assert_eq!(term_of("what are GPUs").as_deref(), Some("GPUs"));
        assert_eq!(term_of("what's SIMD?").as_deref(), Some("SIMD"));
        assert_eq!(term_of("whats AVX").as_deref(), Some("AVX"));
        assert_eq!(term_of("SIMD full form").as_deref(), Some("SIMD"));
        assert_eq!(term_of("full form of AVX").as_deref(), Some("AVX"));
        assert_eq!(term_of("SIMD stands for").as_deref(), Some("SIMD"));
        assert_eq!(term_of("what does CPU stand for").as_deref(), Some("CPU"));
        assert_eq!(term_of("meaning of SIMD").as_deref(), Some("SIMD"));
        assert_eq!(term_of("SIMD meaning").as_deref(), Some("SIMD"));
        assert_eq!(term_of("SIMD means").as_deref(), Some("SIMD"));
        assert_eq!(term_of("define SIMD").as_deref(), Some("SIMD"));
    }

    #[test]
    fn forced_prefixes_flagged() {
        let (t, forced) = parse_term("def SIMD").expect("term");
        assert_eq!(t, "SIMD");
        assert!(forced);
        let (t, forced) = parse_term("dict gyroscope").expect("term");
        assert_eq!(t, "gyroscope");
        assert!(forced);
        let (_, forced) = parse_term("what does SIMD mean").expect("term");
        assert!(!forced);
    }

    #[test]
    fn incomplete_and_junk_rejected() {
        for q in [
            "",
            "?",
            "what",
            "what does",
            "what does  mean",
            "what is",
            "define",
            "def",
            "meaning",
            "means",
            "full form",
            "full form of",
            "firefox",
            "2+2",
            "100 km to mi",
            "*.md",
            "/usr/bin",
            "notes in glassbox",
        ] {
            assert!(parse_term(q).is_none(), "must reject {q:?}");
            assert!(!is_define_query(q, &cfg()), "must not handle {q:?}");
        }
    }

    #[test]
    fn curly_apostrophe_and_articles() {
        assert_eq!(term_of("what’s SIMD").as_deref(), Some("SIMD"));
        assert_eq!(term_of("what is a cpu").as_deref(), Some("cpu"));
        assert_eq!(term_of("what is an API?").as_deref(), Some("API"));
        assert_eq!(
            term_of("what is a black hole").as_deref(),
            Some("black hole")
        );
        // Fixed phrases keep their leading `a`.
        assert_eq!(term_of("what is a priori").as_deref(), Some("a priori"));
        assert_eq!(term_of("what is a cappella").as_deref(), Some("a cappella"));
        // `the` is kept: often part of the title (`The Who`).
        assert_eq!(
            term_of("what is the internet").as_deref(),
            Some("the internet")
        );
        // Lone words that merely start with a/an survive.
        assert_eq!(term_of("what is apple").as_deref(), Some("apple"));
        assert_eq!(term_of("what is a").as_deref(), Some("a"));
        // Forced prefixes keep the term verbatim.
        assert_eq!(term_of("define a priori").as_deref(), Some("a priori"));
    }

    fn parsed(q: &str) -> Option<(String, bool, Intent)> {
        parse_query(q).map(|p| (p.term, p.forced, p.intent))
    }

    #[test]
    fn lookup_questions_parse_with_lookup_intent() {
        for (q, term) in [
            ("who is linus sebastian", "linus sebastian"),
            ("Who was Ada Lovelace?", "Ada Lovelace"),
            ("who are daft punk", "daft punk"),
            ("who were the beatles", "the beatles"),
            ("who's elon musk", "elon musk"),
            ("who’s elon musk", "elon musk"),
            ("where is delhi", "delhi"),
            ("where are the andes", "the andes"),
            ("where was napoleon born", "napoleon"),
            ("where is petra located?", "petra"),
            ("who is linus torvalds now", "linus torvalds"),
            ("where's machu picchu", "machu picchu"),
            ("tell me about black holes", "black holes"),
            ("tell me about a black hole", "black hole"),
            ("what is delhi", "delhi"),
            ("what's linux", "linux"),
        ] {
            assert_eq!(
                parsed(q),
                Some((term.into(), false, Intent::Lookup)),
                "{q:?}"
            );
        }
    }

    #[test]
    fn fact_suffix_only_trimmed_for_lookups() {
        // The fact word alone is never stripped down to nothing.
        assert_eq!(parsed("who is born").map(|p| p.0).as_deref(), Some("born"));
        // Define questions keep their term (`what does based mean`).
        assert_eq!(
            parsed("what does based mean").map(|p| p.0).as_deref(),
            Some("based")
        );
        // Forced lookups are verbatim.
        assert_eq!(
            parsed("wiki Born from").map(|p| p.0).as_deref(),
            Some("Born from")
        );
    }

    #[test]
    fn web_row_text_depends_on_query_shape() {
        let q = |s: &str| web_query(&parse_query(s).unwrap(), s);
        assert_eq!(q("what is the capital"), "what is the capital");
        assert_eq!(q("define gregarious"), "define gregarious");
        assert_eq!(q("wiki ltt"), "ltt");
        assert_eq!(q("wiki Python (disambiguation)"), "Python");
        assert_eq!(q("taj mahal wikipedia"), "taj mahal");
    }

    #[test]
    fn wiki_prefix_and_suffix_are_forced_lookups() {
        assert_eq!(
            parsed("wiki linus sebastian"),
            Some(("linus sebastian".into(), true, Intent::Lookup))
        );
        assert_eq!(
            parsed("Wikipedia Taj Mahal"),
            Some(("Taj Mahal".into(), true, Intent::Lookup))
        );
        assert_eq!(
            parsed("taj mahal wikipedia"),
            Some(("taj mahal".into(), true, Intent::Lookup))
        );
        // Forced: verbatim term, no stoplist / article stripping.
        assert_eq!(
            parsed("wiki a priori"),
            Some(("a priori".into(), true, Intent::Lookup))
        );
        assert_eq!(parsed("wiki up"), Some(("up".into(), true, Intent::Lookup)));
        // `X wiki` names project wikis (`arch wiki`), never claimed.
        assert!(parse_query("arch wiki").is_none());
        // Bare / prefix-only words are not lookups.
        for q in ["wiki", "wikipedia", "wiki ", "wikis", "wikipedia's"] {
            assert!(parse_query(q).is_none(), "{q:?}");
        }
    }

    #[test]
    fn define_questions_keep_define_intent() {
        for q in [
            "define gregarious",
            "def SIMD",
            "dict gyroscope",
            "what does SIMD mean",
            "what does CPU stand for",
            "meaning of SIMD",
            "SIMD meaning",
            "SIMD full form",
            "full form of AVX",
            "SIMD stands for",
        ] {
            assert_eq!(
                parse_query(q).map(|p| p.intent),
                Some(Intent::Define),
                "{q:?}"
            );
        }
    }

    #[test]
    fn personal_lookup_questions_not_claimed() {
        for q in [
            "who is it",
            "who are you",
            "where is my phone",
            "where is it",
            "tell me about yourself",
            "where is here",
        ] {
            assert!(parse_query(q).is_none(), "must not claim {q:?}");
        }
    }

    #[test]
    fn disambig_wikitext_lists_entry_lines_in_order() {
        // Real `AVX` page shape: headings, nested bullets, mid-line links,
        // italic links, templates, `See also`.
        let text = "'''AVX''' may refer to:\n{{TOCright}}\n==Computing==\n\
            * [[Advanced Vector Extensions]], an instruction set extension in the x86 architecture\n\
            ** [[AVX2]], an expansion of the AVX instruction set\n\
            * Softwin AVX (AntiVirus eXpert), former name of [[Bitdefender]]\n\n\
            ==Transportation==\n\
            * [[Catalina Airport]] (IATA airport code <code>AVX</code>), Avalon, [[California|CA]]\n\
            ==Other uses==\n\
            * ''[[Avengers vs. X-Men]]'', a comic book event\n\
            * [[AVX Corporation|AVX Corp.]], a manufacturer {{cn}} of parts\n\
            * [[File:Logo.png]] image line\n\
            * [[advanced vector extensions#AVX]] duplicate\n\
            ==See also==\n\
            * [[UltraAVX]]\n{{disambiguation}}";
        let c = parse_disambig_wikitext(text);
        let titles: Vec<&str> = c.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(
            titles,
            [
                "Advanced Vector Extensions",
                "Catalina Airport",
                "Avengers vs. X-Men",
                "AVX Corporation"
            ]
        );
        assert_eq!(
            c[0].description.as_deref(),
            Some("an instruction set extension in the x86 architecture")
        );
        assert_eq!(
            c[1].description.as_deref(),
            Some("(IATA airport code AVX), Avalon, CA")
        );
        assert_eq!(c[2].description.as_deref(), Some("a comic book event"));
        assert_eq!(c[3].description.as_deref(), Some("a manufacturer of parts"));
    }

    #[test]
    fn disambig_wikitext_caps_and_handles_bare_links() {
        let text = (0..10)
            .map(|i| format!("* [[Meaning {i}]]"))
            .collect::<Vec<_>>()
            .join("\n");
        let c = parse_disambig_wikitext(&text);
        assert_eq!(c.len(), MAX_CANDIDATES);
        assert_eq!(c[0].title, "Meaning 0");
        assert_eq!(c[0].description, None);
        assert!(parse_disambig_wikitext("no bullets here").is_empty());
    }

    #[test]
    fn ambiguous_entry_lists_meanings_then_web() {
        let mut e = entry("ltt", "LTT", "", None);
        e.candidates = vec![
            Candidate {
                title: "Linus Tech Tips".into(),
                description: Some("a Canadian tech-related YouTube channel".into()),
            },
            Candidate {
                title: "Light the Torch".into(),
                description: None,
            },
        ];
        let p = parse_query("who is ltt").unwrap();
        let rows = entry_results(&p, "who is ltt", &e);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].title, "Linus Tech Tips");
        assert!(rows[0]
            .subtitle
            .contains("a Canadian tech-related YouTube channel"));
        assert!(rows[0].subtitle.contains("“LTT” may refer to"));
        assert!(matches!(&rows[0].action, Action::SetQuery(q) if q == "wiki Linus Tech Tips"));
        assert!(rows[0].score > rows[1].score, "editorial order kept");
        // List rows are never article rows (no inline body, no media).
        assert!(!is_ok_result(&rows[0]));
        assert!(lookup_media_by_id(&rows[0].id).is_none());
        // Last row: web search for the whole question.
        assert_eq!(rows[2].kind, ResultKind::Command);
        assert!(matches!(&rows[2].action, Action::OpenUrl(u) if u.ends_with("who%20is%20ltt")));
    }

    #[test]
    fn article_from_disambiguation_offers_other_meanings() {
        let mut e = entry(
            "python",
            "Python (programming language)",
            "Python is a language.",
            Some("https://en.wikipedia.org/wiki/Python_(programming_language)"),
        );
        e.disambig = Some("Python".into());
        let p = parse_query("what is python").unwrap();
        let rows = entry_results(&p, "what is python", &e);
        assert_eq!(rows.len(), 2);
        assert!(is_ok_result(&rows[0]));
        assert_eq!(rows[1].title, "Other meanings of “Python”");
        assert!(!is_ok_result(&rows[1]));
        assert!(rows[0].score > rows[1].score);
        assert!(
            matches!(&rows[1].action, Action::SetQuery(q) if q == "wiki Python (disambiguation)")
        );
        // That query lists meanings (forced, ends with the suffix).
        let p = parse_query("wiki Python (disambiguation)").unwrap();
        assert!(p.term.to_ascii_lowercase().ends_with(DISAMBIG_SUFFIX));
        // A page already named `… (disambiguation)` is not suffixed twice.
        assert_eq!(
            disambiguation_query("Delhi (disambiguation)"),
            "wiki Delhi (disambiguation)"
        );
        // Plain articles get no extra row.
        e.disambig = None;
        assert_eq!(entry_results(&p, "q", &e).len(), 1);
    }

    #[test]
    fn ambiguous_lookup_caches_and_sanitizes() {
        let term = "hark-test-ambiguous";
        let key = cache_key(term);
        let e = cache_put(
            &key,
            term,
            &Lookup::Ambiguous {
                page: "LTT".into(),
                candidates: vec![
                    Candidate {
                        title: "Linus Tech Tips".into(),
                        description: Some("channel".into()),
                    },
                    Candidate {
                        title: "bad\ntitle".into(),
                        description: None,
                    },
                ],
            },
        );
        assert!(e.is_usable(), "a meanings list is a usable entry");
        assert_eq!(e.candidates.len(), 1, "control chars dropped");
        let mem = cache_get_mem(&key).expect("cached");
        assert_eq!(mem.candidates[0].title, "Linus Tech Tips");
        assert!(mem.extract.is_empty());
    }

    #[test]
    fn non_definition_questions_not_claimed() {
        for q in [
            "what is my ip",
            "what's my ip address",
            "what’s up",
            "whats up",
            "what is up",
            "what's new in firefox",
            "what is the time in tokyo",
            "what is the weather",
            "what is today",
            "by any means",
            "by all means",
        ] {
            assert!(parse_term(q).is_none(), "must not claim {q:?}");
        }
        // Real terms that share words with the stoplist still parse.
        assert_eq!(term_of("what is new york").as_deref(), Some("new york"));
        assert_eq!(
            term_of("what is time dilation").as_deref(),
            Some("time dilation")
        );
        assert_eq!(term_of("define up").as_deref(), Some("up"));
    }

    #[test]
    fn does_not_steal_paths_or_math() {
        assert!(parse_term("what is /usr/bin").is_none());
        assert!(parse_term("define a/b").is_none());
        assert!(parse_term("what is 2*3").is_none());
    }

    #[test]
    fn max_chars_gates_long_terms() {
        let mut c = cfg();
        c.max_chars = 3;
        assert!(!is_define_query("what does SIMD mean", &c));
        assert!(is_define_query("what does AVX mean", &c));
    }

    #[test]
    fn cache_key_case_insensitive_and_stable() {
        assert_eq!(cache_key("SIMD"), cache_key("simd"));
        assert_eq!(cache_key("a  b"), cache_key("a b"));
        assert_ne!(cache_key("SIMD"), cache_key("AVX"));
    }

    fn entry(term: &str, title: &str, extract: &str, page_url: Option<&str>) -> CacheEntry {
        CacheEntry {
            term: term.into(),
            title: title.into(),
            extract: extract.into(),
            source: "Wikipedia".into(),
            fetched_at: now_secs(),
            image_url: None,
            page_url: page_url.map(str::to_string),
            description: None,
            disambig: None,
            candidates: Vec::new(),
        }
    }

    fn hit(title: &str, extract: &str) -> DefineHit {
        DefineHit {
            title: title.into(),
            extract: extract.into(),
            source: "Wikipedia".into(),
            image_url: None,
            page_url: None,
            description: None,
            disambig: None,
        }
    }

    #[test]
    fn define_intent_row_copies_full_paragraph() {
        let extract = "The International Space Station is a big   station\norbiting Earth.";
        let e = entry(
            "iss",
            "International Space Station",
            extract,
            Some("https://en.wikipedia.org/wiki/International_Space_Station"),
        );
        let r = ok_result("iss", &e, Intent::Define);
        assert_eq!(r.kind, ResultKind::Define);
        assert!(r.conversion.is_none(), "define never renders a card");
        assert!(is_ok_result(&r));
        assert_eq!(r.title, "International Space Station · Wikipedia");
        // Full paragraph inline (subtitle), single-spaced.
        assert_eq!(r.subtitle, collapse_ws(extract));
        match &r.action {
            Action::Copy(t) => assert_eq!(t, &collapse_ws(extract), "{t}"),
            _ => panic!("define intent must copy the full extract"),
        }
    }

    #[test]
    fn lookup_intent_row_opens_article() {
        let url = "https://en.wikipedia.org/wiki/Linus_Sebastian";
        let e = entry(
            "linus sebastian",
            "Linus Sebastian",
            "A YouTuber.",
            Some(url),
        );
        let r = ok_result("linus sebastian", &e, Intent::Lookup);
        assert!(is_ok_result(&r));
        assert_eq!(r.subtitle, "A YouTuber.");
        match &r.action {
            Action::OpenUrl(u) => assert_eq!(u, url),
            _ => panic!("lookup intent must open the article"),
        }
        // No article URL (DDG abstract): falls back to copying.
        let e = entry("x", "X", "Text.", None);
        assert!(matches!(
            ok_result("x", &e, Intent::Lookup).action,
            Action::Copy(_)
        ));
    }

    #[test]
    fn pending_row_is_define_kind() {
        let r = pending_result("iss");
        assert_eq!(r.kind, ResultKind::Define);
        assert!(r.conversion.is_none());
        assert!(is_pending_result(&r));
    }

    #[test]
    fn wikipedia_search_picks_top_match() {
        // Shape of a real `generator=prefixsearch` reply for `linux sebastian`
        // (typo-corrected top match), pages deliberately out of rank order.
        let body = br#"{"batchcomplete":true,"query":{"pages":[
            {"pageid":2,"ns":0,"title":"Sebastian Lind","index":2},
            {"pageid":1,"ns":0,"title":"Linus Sebastian","index":1}
        ]}}"#;
        assert_eq!(
            parse_wikipedia_search(body, false).unwrap(),
            Resolved::Article {
                title: "Linus Sebastian".into(),
                via_disambig: None
            }
        );
    }

    #[test]
    fn wikipedia_search_disambiguation_uses_qualified_sense_only() {
        // `python`: top is a disambiguation page → the best-ranked
        // `Python (…)` sense wins over unrelated lower-ranked titles.
        let python = br#"{"query":{"pages":[
            {"title":"Pythonidae","index":2},
            {"title":"Python","index":1,"pageprops":{"disambiguation":""}},
            {"title":"Python (missile)","index":4},
            {"title":"Python (programming language)","index":3}
        ]}}"#;
        assert_eq!(
            parse_wikipedia_search(python, false).unwrap(),
            Resolved::Article {
                title: "Python (programming language)".into(),
                via_disambig: Some("Python".into())
            }
        );
        // `Python (disambiguation)` asks for the list itself.
        assert_eq!(
            parse_wikipedia_search(python, true).unwrap(),
            Resolved::Ambiguous("Python".into())
        );
        // `ltt`: no qualified sense → list its meanings, never the
        // unrelated `LTT 1445` star.
        let ltt = br#"{"query":{"pages":[
            {"title":"LTT","index":1,"pageprops":{"disambiguation":""}},
            {"title":"LTT 1445","index":2}
        ]}}"#;
        assert_eq!(
            parse_wikipedia_search(ltt, false).unwrap(),
            Resolved::Ambiguous("LTT".into())
        );
        // Qualified senses that are themselves disambiguations are skipped.
        let nested = br#"{"query":{"pages":[
            {"title":"Mercury","index":1,"pageprops":{"disambiguation":""}},
            {"title":"Mercury (mythology)","index":2,"pageprops":{"disambiguation":""}},
            {"title":"Mercury (planet)","index":3}
        ]}}"#;
        assert_eq!(
            parse_wikipedia_search(nested, false).unwrap(),
            Resolved::Article {
                title: "Mercury (planet)".into(),
                via_disambig: Some("Mercury".into())
            }
        );
        // `list_all` never changes a plain article answer.
        let article = br#"{"query":{"pages":[{"title":"Delhi","index":1}]}}"#;
        assert!(matches!(
            parse_wikipedia_search(article, true).unwrap(),
            Resolved::Article { .. }
        ));
    }

    #[test]
    fn wikipedia_search_no_match_misses() {
        // No search hits: the API omits `query` entirely.
        assert!(parse_wikipedia_search(br#"{"batchcomplete":true}"#, false).is_err());
        assert!(parse_wikipedia_search(br#"{"query":{"pages":[]}}"#, false).is_err());
        assert!(
            parse_wikipedia_search(br#"{"query":{"pages":[{"index":1,"title":" "}]}}"#, false)
                .is_err()
        );
        assert!(parse_wikipedia_search(b"not json", false).is_err());
    }

    #[test]
    fn wikipedia_summary_parses_and_rejects_disambiguation() {
        let body = br#"{"type":"standard","title":"SIMD","extract":"Single instruction, multiple data (SIMD) is a type of parallel processing.","thumbnail":{"source":"https://upload.wikimedia.org/wikipedia/commons/thumb/a/ab/SIMD.svg/330px-SIMD.svg.png"},"originalimage":{"source":"https://upload.wikimedia.org/wikipedia/commons/a/ab/SIMD.svg"},"description":"Parallel computing term","content_urls":{"desktop":{"page":"https://en.wikipedia.org/wiki/SIMD"}}}"#;
        let hit = parse_wikipedia_body(body).expect("parse");
        assert_eq!(hit.title, "SIMD");
        assert!(hit.extract.contains("parallel processing"));
        assert_eq!(hit.source, "Wikipedia");
        assert_eq!(
            hit.image_url.as_deref(),
            Some("https://upload.wikimedia.org/wikipedia/commons/thumb/a/ab/SIMD.svg/330px-SIMD.svg.png")
        );
        assert_eq!(
            hit.page_url.as_deref(),
            Some("https://en.wikipedia.org/wiki/SIMD")
        );
        assert_eq!(hit.description.as_deref(), Some("Parallel computing term"));
        // Off-host thumbnails are dropped, text still parses.
        let evil = br#"{"type":"standard","title":"SIMD","extract":"text here","thumbnail":{"source":"https://evil.example/x.png"},"content_urls":{"desktop":{"page":"https://en.wikipedia.org/wiki/SIMD"}}}"#;
        let hit = parse_wikipedia_body(evil).expect("parse");
        assert!(hit.image_url.is_none());
        let dis = br#"{"type":"disambiguation","title":"AVX","extract":"AVX may refer to:"}"#;
        assert!(parse_wikipedia_body(dis).is_err());
        let empty = br#"{"type":"standard","title":"X"}"#;
        assert!(parse_wikipedia_body(empty).is_err());
    }

    #[test]
    fn ddg_parses_abstract() {
        let body = br#"{"Heading":"SIMD","AbstractText":"SIMD is parallel processing.","AbstractSource":"Wikipedia"}"#;
        let hit = parse_ddg_body(body).expect("parse");
        assert_eq!(hit.title, "SIMD");
        assert!(hit.extract.contains("parallel"));
        assert_eq!(hit.source, "Wikipedia");
        assert!(hit.image_url.is_none());
        assert!(parse_ddg_body(br#"{"AbstractText":""}"#).is_err());
    }

    #[test]
    fn fail_row_opens_web_search() {
        let r = fail_result("SIMD", "define SIMD", MissKind::NotFound);
        assert_eq!(r.kind, ResultKind::Command);
        assert_eq!(r.title, "No definition found");
        match &r.action {
            Action::OpenUrl(u) => assert!(u.contains("SIMD"), "{u}"),
            _ => panic!("fail must carry OpenUrl"),
        }
    }

    #[test]
    fn fail_row_searches_whole_question() {
        let r = fail_result(
            "the time in tokyo",
            "what is the time in  tokyo?",
            MissKind::NotFound,
        );
        assert!(
            r.subtitle.contains("“what is the time in tokyo”"),
            "{}",
            r.subtitle
        );
        match &r.action {
            Action::OpenUrl(u) => {
                assert!(u.ends_with("what%20is%20the%20time%20in%20tokyo"), "{u}")
            }
            _ => panic!("fail must carry OpenUrl"),
        }
    }

    #[test]
    fn offline_miss_is_not_reported_as_no_definition() {
        let r = fail_result("SIMD", "define SIMD", MissKind::Unreachable);
        assert_ne!(r.title, "No definition found");
        assert!(matches!(r.action, Action::OpenUrl(_)));
    }

    #[test]
    fn http_errors_classify_miss_kind() {
        assert_eq!(classify_http_err("HTTP 404"), MissKind::NotFound);
        assert_eq!(classify_http_err("HTTP 400"), MissKind::NotFound);
        assert_eq!(classify_http_err("HTTP 429"), MissKind::Unreachable);
        assert_eq!(classify_http_err("HTTP 503"), MissKind::Unreachable);
        assert_eq!(classify_http_err("timed out"), MissKind::Unreachable);
        assert_eq!(classify_http_err("unreachable"), MissKind::Unreachable);
    }

    #[test]
    fn unreachable_misses_expire_faster() {
        let key = cache_key("hark-test-offline-term");
        fail_put(&key, MissKind::Unreachable);
        assert_eq!(fail_get(&key), Some(MissKind::Unreachable));
        // Age the entry past the offline TTL but inside the not-found TTL.
        mem_fail().lock().unwrap().get_mut(&key).unwrap().1 -= UNREACHABLE_CACHE_SECS + 1;
        assert_eq!(fail_get(&key), None);
        fail_put(&key, MissKind::NotFound);
        mem_fail().lock().unwrap().get_mut(&key).unwrap().1 -= UNREACHABLE_CACHE_SECS + 1;
        assert_eq!(fail_get(&key), Some(MissKind::NotFound));
        fail_clear(&key);
    }

    #[test]
    fn truncate_bytes_respects_char_boundaries() {
        assert_eq!(truncate_bytes("short", 10), "short");
        // "दि" is 6 bytes; a 4-byte cap must not split the second char.
        assert_eq!(truncate_bytes("दि", 4), "द");
        let long = "दिल्ली ".repeat(400);
        let t = truncate_bytes(&long, CACHE_ENTRY_MAX_BYTES);
        assert!(t.len() <= CACHE_ENTRY_MAX_BYTES);
        assert!(t.len() > CACHE_ENTRY_MAX_BYTES - 4);
    }

    #[test]
    fn non_ascii_long_extract_is_cached() {
        // 2000 *chars* of Devanagari is ~6000 bytes: the old char-count
        // truncation failed the byte check and skipped caching entirely.
        let term = "hark-test-delhi-long";
        let key = cache_key(term);
        let extract = "दिल्ली ".repeat(400);
        let e = cache_put(&key, term, &Lookup::Hit(hit("Delhi", &extract)));
        assert!(e.extract.len() <= CACHE_ENTRY_MAX_BYTES);
        assert!(extract.starts_with(&e.extract));
        let mem = cache_get_mem(&key).expect("must be cached in memory");
        assert_eq!(mem.extract, e.extract);
    }

    #[test]
    fn pending_id() {
        assert!(is_pending_result(&pending_result("SIMD")));
        assert_eq!(pending_result("SIMD").kind, ResultKind::Define);
    }

    #[test]
    fn single_char_terms_skip_network_and_rows() {
        // `define b` must not own the query: no row, no worker.
        // In-memory config: the test never touches disk or /tmp.
        let provider = DefineProvider::new(std::sync::Arc::new(
            crate::config::ConfigStore::in_memory({
                let mut c = crate::config::HarkConfig::default();
                c.define.enabled = true;
                c
            }),
        ));
        assert!(provider.search("define b").is_empty());
        assert!(!provider.needs_network("define b"));
        assert!(provider.needs_network("define AI"));
        // …and not owned either: the web fallback still shows.
        assert!(!provider.should_handle("define b"));
        assert!(!is_define_query(
            "define b",
            &crate::config::DefineConfig::default()
        ));
        assert!(provider.should_handle("define AI"));
    }

    #[test]
    fn ok_lookup_round_trips_media() {
        let key = cache_key("Black hole");
        let e = cache_put(
            &key,
            "Black hole",
            &Lookup::Hit(DefineHit {
                image_url: Some(
                    "https://upload.wikimedia.org/wikipedia/commons/thumb/x.jpg".into(),
                ),
                page_url: Some("https://en.wikipedia.org/wiki/Black_hole".into()),
                description: Some("Compact astronomical body".into()),
                ..hit("Black hole", "A black hole is compact.")
            }),
        );
        let id = format!("define:{key}");
        let r = ok_result("Black hole", &e, Intent::Define);
        assert_eq!(r.id, id);
        assert!(is_ok_result(&r));
        assert!(!is_ok_result(&pending_result("Black hole")));
        let media = lookup_media_by_id(&id).expect("media");
        assert!(media.image_url.contains("upload.wikimedia.org"));
        assert_eq!(
            media.page_url.as_deref(),
            Some("https://en.wikipedia.org/wiki/Black_hole")
        );
        // Pending / malformed ids never resolve.
        assert!(lookup_media_by_id("define:pending:deadbeef").is_none());
        assert!(lookup_media_by_id("define:xyz").is_none());
    }
}
