# Translation tool — implementation plan

**Status:** Phase 1 done · Phase 1.1 optional  
**Last updated:** 2026-07-15  
**Goal:** Paste Chinese (or other non-English text) into Blink → see a clear translation → Enter copies it.  
**Constraint:** Stay light for a resident Hyprland daemon — no models on boot, no heavy crates in v1, short timeouts, aggressive cache.

Related code today:

| Piece | Path | Reuse |
|-------|------|--------|
| Provider trait / results | `src/providers/mod.rs` | `SearchResult`, `Action::Copy`, `ConversionView` |
| Calc dual-panel UX | `src/providers/calc/*` | Same card style for ZH ↔ EN |
| Lazy HTTP + disk cache | `src/providers/fx.rs` | `curl` + `~/.cache/blink/…` + TTL |
| Engine merge order | `src/engine.rs` | Insert after calc, before apps/files when confident |
| Search debounce | `src/ui/mod.rs` | Existing 40 ms; translate may need longer settle |
| Footer | `src/ui/footer.rs` | “Copy Result” for `Conversion` |
| Settings pattern | `src/ui/settings.rs` + `BlinkConfig` | New toggles / endpoint |

---

## 1. Product UX

### 1.1 Happy paths

| Input | Behavior |
|-------|----------|
| `你好世界` | Auto-detect CJK → translate to default target (EN) |
| `tr 你好世界` / `translate 你好世界` | Forced translate mode (any script) |
| `tr en zh Hello world` | Optional explicit direction (phase 1.1) |
| Same text again | Instant from disk cache (no network) |
| Enter / activate | `Action::Copy(translated)` — same as calc |
| Offline / timeout | One row: “Translation unavailable” (no hang) |

### 1.2 Presentation

Reuse **conversion card** (`ResultKind::Conversion` + `ConversionView`):

```
┌─────────────────────────────────────────┐
│ Translate · ZH → EN                     │
│  你好世界          │  Hello world        │
│  Chinese           │  English            │
└─────────────────────────────────────────┘
```

- **Title** (list/fallback): translated string  
- **Subtitle:** `ZH → EN · cache` or `ZH → EN · LibreTranslate`  
- **Footer:** Copy Result  
- Do **not** open preview pane (not a file)

### 1.3 Non-goals (v1)

- Full bilingual document editor  
- Clipboard watcher (too invasive for a launcher)  
- Offline NLLB / Ollama / multi-GB models  
- Translating every English multi-word query (would steal app search)  
- Boot-time network or model load  

---

## 2. Architecture

```
Query
  │
  ├─ calc (math / units / FX / tz)     ← unchanged; wins first
  │
  ├─ translate                         ← NEW
  │     is_translate_query?
  │       no  → skip
  │       yes → cache hit? → result
  │              else → async/lazy curl (or sync with 2s cap in worker path)
  │
  └─ apps + files                      ← existing
```

### 2.1 New modules

```
src/providers/translate.rs          # v1 single file OK
# later split if needed:
#   detect.rs  cache.rs  http.rs
```

Export from `src/providers/mod.rs` and wire in `Engine` like `CalcProvider`.

### 2.2 Core types (sketch)

```rust
struct TranslateConfig {
    /// Default target BCP-47 / short code, e.g. "en"
    target_lang: String,
    /// LibreTranslate-compatible base URL; empty = built-in default or disabled
    endpoint: String,
    /// Optional API key header
    api_key: Option<String>,
    /// Auto-run when CJK (etc.) detected without `tr ` prefix
    auto_detect: bool,
    /// Max source chars (e.g. 1000)
    max_chars: usize,
}

struct TranslateStore {
    // disk cache under ~/.cache/blink/translate/
}

struct TranslateProvider {
    store: TranslateStore,
    // config read from ConfigStore on each search or cached snapshot
}
```

Config lives on `BlinkConfig` (serde), e.g.:

```json
"translate": {
  "enabled": true,
  "target_lang": "en",
  "endpoint": "",
  "api_key": null,
  "auto_detect": true,
  "max_chars": 1000
}
```

### 2.3 Detection (`is_translate_query`)

**Always true** when query (after trim) matches:

- Prefix: `tr `, `translate `, `译 ` (case-insensitive prefix)  
- Strip prefix → non-empty remainder  

**Auto (if `auto_detect`)** when **no** force-files / path-glob / confident calc:

- Count letters: if CJK Unified Ideographs / Hiragana / Katakana / Hangul ≥ threshold  
  - e.g. ≥ 1 CJK char and ≥ 30% of non-space chars are CJK, **or** ≥ 2 CJK chars  
- Length in range `[2, max_chars]`  
- Reject: looks like path (`/`, `~/`, `*.ext`), pure math, currency patterns  

**Always false** for:

- Empty / whitespace  
- `is_path_glob_query` / `is_scoped_file_query`  
- Single ASCII token that matches apps better (optional: only auto when any CJK present)

### 2.4 Language guess (v1, no network)

| Script signal | Source | Default target |
|---------------|--------|----------------|
| Han (CJK) | `zh` | config `target_lang` (default `en`) |
| Hiragana/Katakana | `ja` | `en` |
| Hangul | `ko` | `en` |
| Explicit `tr ` + Latin only | `en` (or `auto`) | config or `zh` if user often wants EN→ZH later |

v1.1: optional API `/detect` or LibreTranslate `source: "auto"`.

### 2.5 HTTP backend (v1)

**Protocol:** LibreTranslate-compatible JSON:

```http
POST {endpoint}/translate
Content-Type: application/json

{
  "q": "<text>",
  "source": "zh",
  "target": "en",
  "format": "text"
}
```

Optional header: `Authorization: Bearer <api_key>` if configured.

**Transport:** same as FX — `Command::new("curl")` with:

- `--max-time 2`  
- `--connect-timeout 1`  
- `-fsSL`  
- POST body via `--data-binary @-` or `-d`  

**No** `reqwest` in v1 (keeps binary/deps small). Fail soft if `curl` missing.

**Default endpoint policy (decide at implement time):**

1. Empty config → try a documented public instance **or** show “Configure endpoint in Settings”  
2. Prefer user self-host for privacy (document in Settings hint)  
3. Do not scrape unofficial Google as the only path (ToS / breakage)

### 2.6 Cache

```
~/.cache/blink/translate/<sha256(src|source|target|normalized_text)>.json

{
  "source": "zh",
  "target": "en",
  "q": "你好",
  "translated": "Hello",
  "fetched_at": 1710000000
}
```

- **TTL:** 14 days (constant)  
- **Normalize key:** trim, collapse internal whitespace lightly for key only; display original  
- **Cap files:** optional LRU sweep if dir &gt; N entries (e.g. 500)  
- Cache **hits never touch network**  

### 2.7 Sync vs async

| Path | Behavior |
|------|----------|
| Cache hit | Sync in `search()` — instant row |
| Cache miss | **Preferred:** return nothing or “Translating…” stub + async fill (generation token like deep files / preview) |
| v1 MVP shortcut | Sync curl with 2s max only when prefix `tr ` or query length jumped (paste); still never block boot |

**Recommendation:** MVP = sync curl on translate-confident queries only (rare vs typing), then upgrade to async if jank appears.

Engine short-circuit: if translate returns a strong `Conversion` hit, treat like calc (`has_calc`-style) so weak apps don’t dominate.


## 2.9 Master kill switch (`enabled: false`)

When **Enable translation** is off (`BlinkConfig.translate.enabled = false`):

| Layer | Behavior |
|-------|----------|
| `TranslateProvider::is_enabled` | `false` |
| `Engine::search` | Does **not** call `should_handle` / `search` on translate |
| HTTP / curl | Never started |
| Disk cache | Never read or written |
| Background threads | None spawned for translate |
| Auto-detect / prefix | Ignored entirely |

Turning the feature back **on** only re-enables detection (Phase 0) / network (Phase 1) on the **next** query — no resident translate worker exists.

### 2.8 Result builder

```rust
SearchResult {
  id: format!("translate:{hash}"),
  title: translated.clone(),
  subtitle: format!("{src} → {tgt} · {backend}"),
  kind: ResultKind::Conversion,
  score: 100_000, // above normal apps
  icon: Some("preferences-desktop-locale".into()), // or similar
  action: Action::Copy(translated),
  conversion: Some(ConversionView {
    left_title: source_text,
    left_badge: src.to_uppercase(),
    right_title: translated,
    right_badge: tgt.to_uppercase(),
  }),
}
```

---

## 3. Engine integration

### 3.1 `Engine` fields

```rust
translate: Arc<TranslateProvider>,
```

Construct in `Engine::new` with `config.clone()` (like files).

### 3.2 `search` order

```text
1. settings command
2. calc.search
3. if translate enabled && is_translate_query(q):
      translate.search → extend results
      if strong translate hit: skip apps (and maybe skip files)  // force_translate
4. force_files / apps / files as today
```

`force_translate` when:

- Prefix `tr `/`translate `, or  
- Auto CJK and translate score high  

### 3.3 Usage / frecency

Do **not** record translate copy into file/app usage (or use a separate id prefix ignored by empty-state recents).

---

## 4. Settings UI

New nav item **Tools** or subsection under existing page:

| Control | Default | Notes |
|---------|---------|--------|
| Enable translation | on | Master switch |
| Auto-detect CJK paste | on | Off → only `tr ` prefix |
| Target language | `en` | Short list: en, zh, ja, ko, hi, es, fr, de… |
| API endpoint | empty | Placeholder LibreTranslate URL |
| API key | empty | Optional, password-style entry |
| Max characters | 1000 | Clamp 100–5000 |

Persist under `BlinkConfig.translate`. Sanitize on load/update like `UiThemeConfig`.

Privacy hint under the form:

> Text is sent to the configured translation endpoint. Use a local LibreTranslate for privacy.

---

## 5. Phased delivery

### Phase 0 — Scaffold (½ day)

- [x] `TranslateConfig` on `BlinkConfig` + defaults + sanitize  
- [x] `providers/translate.rs` stub: `is_translate_query`, `search` returns empty  
- [x] Wire into `Engine` (no network yet)  
- [x] Unit tests for detection heuristics  
- [x] **Master toggle** `translate.enabled` — when off, engine skips provider entirely (no I/O, no background)  
- [x] Settings → Tools: Enable translation + Auto-detect CJK  

### Phase 1 — MVP online translate (1–2 days)

- [x] Prefix `tr ` / `translate `  
- [x] CJK auto-detect  
- [x] LibreTranslate-compatible `curl` POST (custom endpoint)  
- [x] Free MyMemory fallback when endpoint empty  
- [x] Disk cache + TTL (14d) + sweep  
- [x] `ConversionView` + `Action::Copy`  
- [x] Engine short-circuit when translate owns the query  
- [x] Settings: enable, target lang, endpoint, api key, auto_detect  
- [x] Soft failure row  
- [x] Master kill switch still hard-gates all I/O  
- [x] Docs: this file status; FEATURES.md bullet  

**Done criteria:**

1. Paste `你好` → English translation card within ~2s (cold) / &lt;50 ms (cache)  
2. `tr hello` with endpoint configured works  
3. Typing `firefox` does not call translate  
4. `blink --daemon` start does not open network for translate  
5. `cargo test` detection tests green; release install still fine  

### Phase 1.1 — Direction & polish (optional)

- [ ] `tr en zh …` / `tr zh en …` parsing  
- [ ] `source: "auto"` when API supports it  
- [ ] Longer debounce for auto mode only  
- [ ] Async “Translating…” + generation cancel  
- [ ] Cache sweep / size cap  

### Phase 2 — Offline (later, optional feature)

- [ ] Feature flag `offline-translate` or Settings “Local only”  
- [ ] Bergamot / Argos CLI if installed — never ship multi-GB by default  
- [ ] Models under `~/.local/share/blink/models/` on demand  

---

## 6. Efficiency checklist

| Rule | Implementation |
|------|----------------|
| No boot network | No translate in `Engine::new` / daemon start |
| Fast reject | Script + prefix checks only; no curl on apps/files queries |
| Short timeout | curl 2s / connect 1s |
| Cache first | Hash key; skip HTTP on hit |
| Cap size | `max_chars`; refuse huge pastes |
| Cancel stale | Async phase: gen token drop |
| Small binary | curl CLI, no reqwest/ONNX in v1 |
| Battery | Event-driven only; no poll loop |

---

## 7. Testing plan

### Unit

- `is_translate_query("你好")` true (auto on)  
- `is_translate_query("firefox")` false  
- `is_translate_query("tr 你好")` true  
- `is_translate_query("*.md")` false  
- `is_translate_query("100 usd to eur")` false (calc wins first anyway)  
- Cache key stable for same input  
- Sanitize endpoint / max_chars  

### Manual

1. Cold daemon: paste Chinese → translation  
2. Repeat paste → instant cache  
3. Disconnect network → failure message, UI still responsive  
4. Settings disable → no translate rows  
5. Wrong endpoint → soft fail  

### Regression

- `blink --bench` apps/files/calc unchanged  
- No idle curl in `ps`/network when idle  

---

## 8. Security & privacy

- Show endpoint host in subtitle when not cache  
- Don’t log full translated text to stdout in release  
- API key stored in config file (mode 0600 if we touch permissions later)  
- Prefer user-controlled LibreTranslate for sensitive text  

---

## 9. File / touch list (Phase 1)

| File | Change |
|------|--------|
| `src/config.rs` | `TranslateConfig` + `BlinkConfig.translate` |
| `src/providers/translate.rs` | **new** provider |
| `src/providers/mod.rs` | `mod translate` |
| `src/engine.rs` | construct + search order + short-circuit |
| `src/ui/settings.rs` | Tools / Translate page or section |
| `FEATURES.md` | Feature bullet when done |
| `todo.txt` | item 8 → in progress / done |
| `docs/README.md` | link this plan |
| `translation.md` | this plan; tick boxes as shipped |

---

## 10. Open decisions (resolve at Phase 1 start)

1. **Default endpoint:** empty (configure required) vs bundled public LibreTranslate URL  
2. **Auto CJK without prefix:** on by default (recommended) vs prefix-only  
3. **EN→ZH:** only via `tr en zh` in 1.1, or auto when target is `zh` and source is Latin  
4. **Async vs sync curl** for cache miss in v1  

**Recommended defaults:** auto CJK on; empty endpoint with clear Settings placeholder + docs example; sync curl MVP; EN→ZH in 1.1.

---

## 11. Implementation order (checklist for the implementer)

1. Config types + defaults  
2. Detection + unit tests  
3. Cache get/put  
4. curl translate function  
5. `TranslateProvider::search` → `SearchResult`  
6. Engine wire-up + short-circuit  
7. Settings UI  
8. Manual paste test + cache test  
9. Mark Phase 1 done in this file and `todo.txt`  

---

## 12. Status log

| Date | Note |
|------|------|
| 2026-07-15 | Plan written from research; not implemented yet |
| 2026-07-15 | Phase 0 scaffold: config, detection, engine gate, Settings toggle |
| 2026-07-15 | Phase 1: cache + LibreTranslate/MyMemory curl, Settings fields, conversion card |
