# Translation

Hark can translate pasted or typed text via the translate provider
(`src/providers/translate.rs`). Results appear as a conversion row (copy on Enter).

Network work never runs on the GTK main thread. Empty **API endpoint** races free
Google (unofficial gtx) and MyMemory; a LibreTranslate-compatible URL uses that
backend only.

## Usage

| Form | Example | Behavior |
|------|---------|----------|
| Auto paste | `你好世界` / `Привет` / `नमस्ते` | Detect script → config **target language** |
| Forced prefix | `tr Hello` / `translate Hello` / `译 你好` | Source auto/guessed → config target |
| Explicit pair | `tr en es Hello world` | Source `en`, target `es`, text after the two codes |
| Settings target | Tools → Target language | Default BCP-47 code (e.g. `en`, `es`, `hi`) |

Prefixes: `tr `, `translate `, `译 ` (case-insensitive for ASCII prefixes).

Paths, globs, and file-scoped queries (`*.md`, `foo in bar`) are never treated as
translate jobs. Pure Latin app-style queries (`firefox`, `hello world`) are **not**
auto-detected — use `tr …` for those.

If auto-detect would translate into the same language as the source (e.g. Chinese
paste with target `zh`), Hark flips the target to English (or Chinese if the
source is English). Explicit `tr en en …` is left alone.

## Auto-detect scripts

When **Auto-detect foreign-script paste** is on, these Unicode ranges trigger
translation without a `tr` prefix. Source language is guessed from the dominant script:

| Script | Source code | Notes |
|--------|-------------|--------|
| Han (Chinese characters) | `zh-CN` | Shared with Japanese; kana wins if present |
| Hiragana / Katakana | `ja` | Japanese |
| Hangul | `ko` | Korean |
| Cyrillic | `ru` | Russian and other Cyrillic languages |
| Arabic (incl. presentation forms) | `ar` | Arabic; also covers Persian/Urdu *script* (API may need `fa` / `ur` via `tr`) |
| Devanagari | `hi` | Hindi and related; force `tr sa …` etc. if needed |
| Bengali | `bn` | |
| Tamil | `ta` | |
| Thai | `th` | |
| Hebrew | `he` | |
| Greek | `el` | |

Latin-only text (Spanish, French, German, …) is **not** auto-detected so app
search stays clean. Use an explicit direction:

```text
tr en es Hello
tr es en Hola
tr fr de Bonjour
```

## Language codes

### Canonical codes (recommended)

Use these in Settings → **Target language** and in `tr <src> <tgt> <text>`:

| Code | Language |
|------|----------|
| `en` | English |
| `es` | Spanish |
| `fr` | French |
| `de` | German |
| `pt` | Portuguese |
| `pt-BR` | Portuguese (Brazil) |
| `it` | Italian |
| `nl` | Dutch |
| `pl` | Polish |
| `tr` | Turkish |
| `ru` | Russian |
| `uk` | Ukrainian |
| `el` | Greek |
| `ar` | Arabic |
| `he` | Hebrew |
| `fa` | Persian (Farsi) |
| `ur` | Urdu |
| `hi` | Hindi |
| `bn` | Bengali |
| `ta` | Tamil |
| `th` | Thai |
| `vi` | Vietnamese |
| `id` | Indonesian |
| `zh-CN` | Chinese (Simplified) |
| `zh-TW` | Chinese (Traditional) |
| `ja` | Japanese |
| `ko` | Korean |
| `no` | Norwegian |
| `auto` | Auto-detect source (forced `tr` without two codes; Google/LibreTranslate) |

Any other ISO-ish tag shaped like `xx` or `xx-YY` (2–3 letter primary, optional
region) is accepted and passed through after light normalization. Whether the
**backend** supports that pair depends on Google / MyMemory / your LibreTranslate
instance — unsupported pairs soft-fail in the UI.

### Aliases Hark normalizes

Typed aliases are folded to the canonical form above before caching and HTTP:

| You type | Becomes |
|----------|---------|
| `zh`, `zh-cn`, `zh-hans`, `cn`, `chi`, `chinese` | `zh-CN` |
| `zh-tw`, `zh-hant`, `zh-hk`, `tw` | `zh-TW` |
| `jp`, `jpn`, `japanese` | `ja` |
| `kr`, `kor`, `korean` | `ko` |
| `iw`, `hebrew` | `he` |
| `ua`, `ukr` | `uk` |
| `nb`, `nn`, `nor` | `no` |
| `fa`, `per`, `farsi`, `persian` | `fa` |
| `ur`, `urd` | `ur` |
| `hi`, `hin`, `hindi` | `hi` |
| `bn`, `ben`, `bengali` | `bn` |
| `ta`, `tam`, `tamil` | `ta` |
| `th`, `tha`, `thai` | `th` |
| `vi`, `vie`, `vietnamese` | `vi` |
| `ar`, `ara`, `arabic` | `ar` |
| `ru`, `rus`, `russian` | `ru` |
| `es`, `spa`, `spanish` | `es` |
| `fr`, `fra`, `fre`, `french` | `fr` |
| `de`, `ger`, `deu`, `german` | `de` |
| `pt`, `por`, `portuguese` | `pt` |
| `pt-br`, `br` | `pt-BR` |
| `it`, `ita`, `italian` | `it` |
| `tr`, `tur`, `turkish` | `tr` |
| `pl`, `pol`, `polish` | `pl` |
| `nl`, `dut`, `nld`, `dutch` | `nl` |
| `id`, `ind`, `indonesian` | `id` |
| `el`, `gre`, `ell`, `greek` | `el` |
| `en`, `eng`, `english` | `en` |

Underscores are treated like hyphens (`zh_cn` → same as `zh-cn`).

### How codes are sent to backends

| Backend | Encoding |
|---------|----------|
| LibreTranslate | Primary ISO 639-1 (`zh`, `pt`, `en`) |
| Google gtx | Keeps `zh-CN`, `zh-TW`, `pt-BR`; other tags → primary |
| MyMemory | Same as Google for Chinese / `pt-BR`; primary otherwise; no reliable `auto` (falls back to `en`) |

## Config

Settings → **Tools** → Translation:

| Option | Meaning |
|--------|---------|
| Enable translation | Master switch (off = zero I/O) |
| Auto-detect foreign-script paste | Script table above |
| Target language | Default target code |
| API endpoint | LibreTranslate base URL; empty = free race |
| API key | Optional for self-hosted LibreTranslate |

On-disk: `translate` section of the Hark config (`enabled`, `target_lang`,
`endpoint`, `api_key`, `auto_detect`, `max_chars`).

## Cache

- Process memory + durable `~/.cache/hark/translate/` (or platform cache dir)
- Success TTL ~14 days; fail cache ~90s to avoid hammering free APIs
- Cache key = source + target + whitespace-normalized text

## Source of truth

Language aliases, script ranges, and API encoding live in
`src/providers/translate.rs`. Update this doc when those tables change.
