# Claude Usage Widget — Design Spec

- **Date:** 2026-06-08
- **Status:** Approved (design); pending implementation plan
- **Platform:** Windows 11 (build 26200), x86_64
- **Language / UI:** Rust + [gpui](https://github.com/zed-industries/zed) + [gpui-component](https://github.com/longbridge/gpui-component)
- **Author:** oz

---

## 1. Purpose

A small, always-on-top desktop widget that answers one question at a glance: **"How close am I to my Claude usage limit?"** It shows the real Anthropic rate-limit utilization for the **5-hour rolling window** (primary) and the **7-day window** (secondary), color-coded, so the user can pace heavy Claude Code sessions and avoid getting cut off.

This is a personal tool for a single user on a **Claude Max 5×** plan (auto-detected from disk).

## 2. Scope

### In scope (v1)
- Real 5-hour and 7-day utilization %, with reset countdowns.
- Color-coded status (green / amber / red) — **passive alerting only**, no toasts or tray badges.
- Two view modes, toggled at runtime: **bars** and **gauge (rings)**.
- A frosted-glass (Mica) always-on-top, borderless window.
- Secondary local token stats (today's tokens, per-model split, live tokens/min) from JSONL — informational, not the headline.
- Config persistence (window position, scale, view mode, backdrop, color thresholds).

### Out of scope (v1) — deliberately cut (YAGNI)
- USD cost, cache-savings, pay-as-you-go comparisons. *(Max subscription = $0 marginal cost. Kept as an opt-in hook for the API-key case — see §14.)*
- 90-day heatmap, 52-week activity calendar, AI weekly summary, news ticker, webhooks, localhost JSON API, anomaly detection.
- System tray icon and keep-alive plumbing.
- File-watching (polling + an incremental cursor is sufficient).
- Cross-platform support (Windows only; the code should not gratuitously block a future macOS port, but it is not a goal).

## 3. Data sources

The fundamental split: **utilization % is server-side** (the authoritative number), while **token detail is local**. Two sources, reconciled into one snapshot.

### 3.1 Quota / utilization — "both sources, with fallback"

The headline % comes from Anthropic's own rate-limit accounting, fetched two ways with a freshness-based preference:

1. **Status-line cache (preferred when fresh).** A tiny helper, registered as Claude Code's `statusLine.command` in `~/.claude/settings.json`, receives the session JSON on stdin after each assistant message and writes the `rate_limits` block to a small cache file. The widget reads that file. **No network call; updates live while a session runs.**
   - stdin fields consumed: `rate_limits.five_hour.{used_percentage, resets_at}`, `rate_limits.seven_day.{used_percentage, resets_at}`. `resets_at` here is **Unix epoch seconds**.
   - The user's `settings.json` currently has **no** `statusLine`, so registration is a clean insert (no command chaining required). Registration is **optional** — if absent, the widget simply always uses the OAuth path.
   - Cache file: `~/.claude/widget-cache/ratelimits.json` (written atomically: temp file + rename).
   - Requires Claude Code ≥ ~2.1 (user is on 2.1.16x — satisfied).

2. **OAuth usage endpoint (fallback / when idle).** `GET https://api.anthropic.com/api/oauth/usage`.
   - **Headers (all mandatory):** `Authorization: Bearer <token>`, `anthropic-beta: oauth-2025-04-20`, `User-Agent: claude-code/<version>`. **Omitting the User-Agent yields persistent HTTP 429.**
   - Token source: `~/.claude/.credentials.json` → `claudeAiOauth.accessToken` (plus `expiresAt`, `subscriptionType`, `rateLimitTier`). On Windows the token is in this file (macOS would use Keychain — not needed here).
   - **Response JSON:** `five_hour`, `seven_day`, `seven_day_opus` (nullable), `seven_day_sonnet`, each `{ utilization: 0–100, resets_at: ISO-8601 }`; plus `extra_usage { is_enabled, monthly_limit, used_credits, utilization }`. `resets_at` here is **ISO-8601** (≠ the status-line Unix-seconds form — do not conflate).
   - **Poll no more often than every 180 s.** Back off on 429; on token expiry/refresh, re-read the credentials file.

3. **Reconciliation rule:** prefer the status-line cache when its timestamp is **< ~2 minutes old**; otherwise poll the OAuth endpoint (respecting the 180 s floor). Net effect: **zero network calls while actively coding** (the status line feeds fresh data), OAuth only when idle. On hard failure of both, retain the last-good snapshot, mark it `Stale`, and show a dim provenance dot — the widget never blanks or crashes on a fetch error.

> Note: these endpoints/headers are **undocumented and version-tied**; they can change without notice. All quota parsing must degrade gracefully (hide the section, keep last-good) rather than panic.

### 3.2 Token detail — local JSONL (incremental)

For the secondary stats (today's tokens, per-model, live tok/min):
- Glob `~/.claude/projects/*/*.jsonl`. **Exclude any path containing `subagents`** (double-count guard).
- Parse only lines with `type == "assistant"`. Fields: top-level `timestamp` (ISO-8601 Z), `requestId`; `message.id` (= messageId), `message.model`, `message.usage.{input_tokens, output_tokens, cache_read_input_tokens, cache_creation_input_tokens}`, `message.usage.cache_creation.{ephemeral_5m_input_tokens, ephemeral_1h_input_tokens}`, `message.usage.speed`.
- **Dedup is mandatory.** Streaming writes 2–10 rows per request sharing one `requestId` with rising `output_tokens`; key dedup on the `(message.id, requestId)` tuple and keep the final record. Without this, tokens are massively overcounted.
- **Incremental read.** The history is ~107 MB across ~43 files and grows. Maintain a per-file cursor `{path → (last_size, last_mtime)}`. Each tick: stat files, read only appended bytes since `last_size`, parse the new lines, fold into rolling aggregates. The refresh path touches kilobytes, never the full 107 MB. Handle truncation/rotation (size shrank → re-read from 0).
- Project name = parent directory of the jsonl file. Daily boundary = **UTC midnight**. "Today's tokens" headline is **output-token-weighted** (matches ccusage/bozdemir convention); input/cache tracked separately.
- `live_tok_per_min`: derived from records whose `timestamp` is within the last ~90 s.

### 3.3 Rejected sources
- `stats-cache.json` — pre-aggregated but `costUSD`/`contextWindow` are all zero and it lags via `lastComputedDate`. Not a live or cost source. (May be read once at startup for cheap historical seed only.)
- OTEL (`claude_code.token.usage`) — off by default, needs a running collector, delta temporality, not passed to subprocesses. Too heavy for a local widget.

## 4. Core data model

The program produces **one immutable `UsageSnapshot` per tick**; the UI is a pure function of it. This is the contract between the collector and the UI — UI-agnostic, fully unit-tested.

```rust
struct UsageSnapshot {
    five_hour:      Window,             // PRIMARY
    seven_day:      Window,             // secondary
    seven_day_opus: Option<Window>,     // when the API splits it out
    tokens:         TokenStats,         // local detail (nice-to-have)
    source:         Provenance,         // where the quota came from
    fetched_at:     SystemTime,
}

struct Window {
    used_pct:  f32,                     // 0.0..=100.0 (Anthropic's number)
    resets_at: Option<SystemTime>,      // normalized to SystemTime regardless of source units
}

enum Level { Ok, Warn, Critical }       // derived from used_pct via configurable thresholds → drives color

enum Provenance { StatusLine, OAuth, Stale { last_good_at: SystemTime } }

struct TokenStats {
    today_total_output: u64,
    by_model:           Vec<(String, u64)>,     // sorted desc
    live_tok_per_min:   Option<f64>,
    top_projects:       Vec<(String, u64)>,     // optional, popup-only
}
```

The gauge value **is Anthropic's own utilization %**, so plan magnitude (Max 5×) never enters the math — there is no guessed denominator.

## 5. Architecture / module layout

Single self-contained `.exe`. Sources isolated behind small interfaces; the UI never touches files or sockets.

```
claude-usage-widget/
├─ Cargo.toml            # pinned git revs for gpui, gpui_platform, gpui-component
├─ rust-toolchain.toml   # pin stable 1.95.0 (match zed)
├─ build.rs              # embed the .exe icon (embed-resource / winres)
├─ assets/               # tray/app icon(s), embedded via rust-embed
└─ src/
   ├─ main.rs            # entry: gpui_platform::application().run → gpui_component::init(cx)
   │                     #   → open window → wrap root in Root::new → spawn poller
   │                     #   → apply Mica backdrop + HWND_TOPMOST
   ├─ model.rs           # the structs in §4 — the core, fully unit-tested
   ├─ sources/
   │   ├─ mod.rs         # QuotaSource interface + reconcile()
   │   ├─ statusline.rs  # read widget-cache/ratelimits.json (local, no network)
   │   ├─ oauth.rs       # GET /api/oauth/usage (token, mandatory UA header, 180s throttle, backoff)
   │   └─ jsonl.rs       # incremental per-file cursor reader → TokenStats (dedup, exclude subagents)
   ├─ collector.rs       # orchestrates the 3 sources per tick → one UsageSnapshot
   ├─ statusline_cmd.rs  # `claude-usage --statusline`: stdin JSON → write ratelimits.json + print a line
   ├─ ui/
   │   ├─ mod.rs         # Root view, view-mode toggle, right-click menu, drag-to-move
   │   ├─ bars.rs        # ProgressBar layout
   │   ├─ gauge.rs       # ProgressCircle concentric rings
   │   └─ theme.rs       # Level → color (green/amber/red), legibility scrim
   ├─ win/
   │   ├─ topmost.rs     # raw HWND → SetWindowPos(HWND_TOPMOST); re-assert on focus loss
   │   └─ backdrop.rs    # Mica/Acrylic selection + Win10 fallback
   └─ config.rs          # widget-config.json: position, scale, view_mode, backdrop, thresholds, opacity
```

**Linus lens:** the heart is `model.rs` (data) + `sources/*` (isolated, testable I/O) + `collector.rs` (reconcile into one snapshot) + `ui/*` (dumb render). Each unit answers: what it does, how it's used, what it depends on.

## 6. Data flow & refresh

1. A detached task on gpui's executor loops:
   - **Every ~30 s:** refresh the JSONL cursor → `TokenStats`; pick quota per the §3.1 reconciliation rule (status-line cache if fresh, else OAuth respecting its 180 s floor).
   - Build a new `UsageSnapshot`; update the model entity; request repaint.
   - I/O (file reads, HTTP) runs off the UI thread; the UI never blocks. HTTP via a minimal client (`ureq`/`reqwest`); pick the lighter one at plan time.
2. The UI view renders the current snapshot → bars or gauge by `view_mode`; color by `Level`; a small provenance/staleness dot.
3. The **status-line helper** (`claude-usage --statusline`, a separate process invoked by Claude Code) extracts `rate_limits`, writes the cache file atomically, and prints a minimal status line. Zero network.

## 7. UI

### 7.1 Two views (right-click toggles between them)

```
   BARS view                          GAUGE view
 ┌────────────────────────────┐     ┌────────────────────────────┐
 │ Claude · Max 5×       ·OAuth│     │ Claude · Max 5×       ·live │
 │ 5H ███████████░░░░  72% ▲   │     │      ╭─────╮   ╭─────╮      │
 │    resets 2h13m            │     │      │ 72% │   │ 41% │      │
 │ 7D ██████░░░░░░░░  41%      │     │      │ 5H  │   │ 7D  │      │
 │ ── today 1.24M · opus-4.8  │     │      ╰─────╯   ╰─────╯      │
 └────────────────────────────┘     │   2h13m         4d         │
                                     └────────────────────────────┘
```

- **Bars:** `gpui_component` `ProgressBar` — 5H large/primary, 7D smaller. Reset countdowns; a footer line with today's tokens + dominant model.
- **Gauge:** two `ProgressCircle` rings (`ProgressCircle::new(id).value(0..=100).color(..).with_size(..)`), 5H + 7D, dashboard style.
- Both render the same `UsageSnapshot`; only the layout differs.

### 7.2 Color thresholds (passive alerting)
- `used_pct < 70` → **green** (`Ok`)
- `70 ≤ used_pct < 90` → **amber** (`Warn`)
- `used_pct ≥ 90` → **red** (`Critical`)
- Thresholds configurable. No notifications, no tray badge — color shift only.

### 7.3 Frosted glass (first-class)
- Default window background: **`WindowBackgroundAppearance::MicaBackdrop`** (Win11 DWMSBT_MAINWINDOW). Config-selectable alternatives: `MicaAltBackdrop` (tabbed material), `Blurred` (acrylic, heavier blur), `Transparent` (plain alpha), `Opaque` (solid).
- **Win10 fallback:** Mica is Win11-only; detect build and fall back to `Blurred`/`Transparent`, ultimately `Opaque`, if Mica is unavailable.
- **Legibility:** translucent backgrounds can wash out small text and the glyph atlas can look alpha-blended at low DPI. Mitigate with (a) a slightly larger base font, (b) a faint rounded scrim/card (`group_box`) behind the numbers, (c) high-contrast text colors. Validate at 100 % / 150 % / 200 % scaling.

### 7.4 Interactions
- **Left-drag:** move the window (custom caption hit-region, since borderless) — persist position.
- **Right-click:** context menu — Toggle view, Backdrop ▸ (Mica/MicaAlt/Acrylic/Solid), Refresh now, Scale, Opacity, Quit.
- **Scroll:** resize/scale (0.6×–2.0×), persisted.

## 8. Window shell & always-on-top

- **Borderless:** `WindowOptions { titlebar: None, is_resizable: false, .. }` (hides the OS title bar).
- **Backdrop:** set `window_background` to the configured `WindowBackgroundAppearance` (Mica by default); can be changed at runtime via the menu.
- **Always-on-top (the one Win32 thing gpui won't do):** gpui has **no** always-on-top on Windows (`WindowKind::PopUp` sets `WS_EX_TOOLWINDOW` only, never `HWND_TOPMOST`). After `open_window`, obtain the raw `HWND` via `raw-window-handle` / `HasWindowHandle` and call `SetWindowPos(hwnd, HWND_TOPMOST, 0,0,0,0, SWP_NOMOVE | SWP_NOSIZE)`. Re-assert on focus loss. Use the `windows` crate for the Win32 calls; keep all `unsafe` confined to `win/topmost.rs`.
- **Mandatory gpui-component wrappers:** call `gpui_component::init(cx)` once, and wrap the root view in `gpui_component::Root::new(view, window, cx)` — skipping either breaks overlays/popups/tooltips/theming.

## 9. Configuration & persistence

`~/.config/claude-usage/widget-config.json` (or `%APPDATA%`), all optional with sane defaults:
- `position {x,y}`, `scale` (0.6–2.0), `view_mode` ("bars" | "gauge"), `backdrop` ("mica" | "mica_alt" | "acrylic" | "transparent" | "opaque"), `opacity`, `thresholds {warn: 70, critical: 90}`, `refresh_secs` (30), `quota_poll_secs` (180), `statusline_max_age_secs` (120).
- Written on change; tolerant of missing/extra fields (forward-compatible).

## 10. Build & toolchain

- **Rust:** stable, pinned via `rust-toolchain.toml` to `1.95.0` (matches Zed; edition 2024).
- **Windows deps:** MSVC (Visual Studio 2022 "Desktop development with C++") + CMake — required to build gpui from source. Document in README.
- **Dependencies:** git-only, **pin explicit revs** on `gpui`, `gpui_platform` (feature `font-kit`), and `gpui-component` (tracks zed `main`, no upstream pin). Commit `Cargo.lock`. Treat upgrades as deliberate events, never `cargo update`.
- **Distribution:** single `.exe`; assets embedded via `rust-embed`; `.exe` icon via a Windows resource (`embed-resource`/`winres`). Expect ~12 MB minimum binary and a heavy first build.

## 11. Milestones (incremental; risk first)

- **M0 — Spike / go-no-go (de-risk before anything else).** Build a borderless gpui-component window on *this* machine with the **Mica backdrop**, apply `HWND_TOPMOST`, and render one `ProgressBar` bound to a hardcoded value. Proves the riskiest stack (topmost + frosted glass + gpui-on-Windows build) end-to-end. **Gate: if this fights us, reconsider before sinking more in.**
- **M1 — Data core (TDD).** `model.rs` + `sources/oauth.rs` → fetch real 5h/7d % (token read, mandatory UA header) and print the snapshot. Fixture-tested: OAuth JSON parse, timestamp-unit normalization (ISO vs Unix-sec vs ms), threshold→`Level`.
- **M2 — Bars view, live.** Wire the OAuth quota into the bars view; color thresholds; reset countdowns; 3-min poll; graceful-degrade on fetch failure.
- **M3 — JSONL detail.** `sources/jsonl.rs` incremental cursor + dedup (exclude subagents) → today's tokens + per-model + live tok/min into the snapshot. Fixture-tested with a sample jsonl.
- **M4 — Gauge view + chrome.** `ProgressCircle` rings; right-click view toggle; backdrop switcher; drag-to-move; config persistence.
- **M5 — Status-line path.** `--statusline` helper + atomic cache write + reconciliation (prefer fresh local, fallback OAuth); optional `settings.json` registration helper.
- **M6 — Polish.** Backdrop fallbacks (Win10), legibility scrim, DPI testing, single-`.exe` packaging with icon, README (build prereqs + setup).

## 12. Testing strategy

TDD where it pays — pure logic gets fixture-backed unit tests; GUI/Win32 validated manually (M0 spike + visual checks).
- **Unit-tested:** JSONL dedup + incremental cursor (fixture jsonl, including streaming-duplicate rows and rotation), reconciliation (fresh-local vs stale-fallback vs both-failed), threshold→`Level`, OAuth + status-line JSON parsing, timestamp-unit normalization, config load/merge.
- **Manual:** topmost behavior (does it hold over other windows / re-assert on focus loss?), Mica rendering + text legibility at 100/150/200 % DPI, drag-to-move, menu actions, live update while a Claude Code session runs.

## 13. Risks & mitigations

1. **Always-on-top missing on Windows.** → M0 spike proves the raw-`HWND` `SetWindowPos(HWND_TOPMOST)` path day one; re-assert on focus loss.
2. **Frosted-glass legibility / Win10 absence.** → larger fonts + scrim card; Win-build detection with `Blurred`→`Transparent`→`Opaque` fallback chain.
3. **OAuth endpoint fragility (undocumented, 429-prone, token expiry).** → mandatory UA header, 180 s throttle, backoff, status-line fallback, stale-degrade; never hard-fail.
4. **JSONL overcount from streaming.** → dedup on `(message.id, requestId)`; keep the final record. Accept that token totals are ~approximate; the authoritative number is the server utilization %.
5. **Unpinned, fast-moving git deps.** → pin explicit revs on gpui + gpui-component; commit `Cargo.lock`; deliberate upgrades only.
6. **Format/version drift** (`.credentials.json`, `sessions/*.json`, stats-cache schema verified on 2.1.16x only). → feature-detect; if a file/field is absent, hide that section rather than crash. Do not depend on session-liveness for v1.
7. **Build prerequisites** (MSVC + CMake). → document up front; M0 validates the toolchain on the real machine.

## 14. Open questions / future

- **Cost (opt-in):** if ever wanted for API-key usage, embed LiteLLM's `model_prices_and_context_window.json` at build time with a `models.dev` fallback. Pricing must be **model-version-aware** (Opus 4.5–4.8 = $5/$25 per MTok; Opus 4/4.1 = $15/$75) with alias + 8-digit date-suffix matching, **four cache rates** (base, read ×0.1, 5m-write ×1.25, 1h-write ×2 — apply the 1h ×2 yourself; LiteLLM only carries the 5m rate), and fast-mode multipliers. Gated behind a config flag; off by default.
- **Charts (future):** gpui-component's native `LineChart`/`AreaChart` enable a usage-over-time sparkline in a later click-through popup — a reason this stack was chosen.
- **macOS port (non-goal):** the data/source/collector layer is portable; only `win/` and the backdrop differ (macOS `PopUp` gives always-on-top for free).

## Appendix A — Exact on-disk references (verified 2026-06-08, Claude Code 2.1.16x)

- Plan: `~/.claude/.credentials.json` → `claudeAiOauth.subscriptionType = "max"`, `rateLimitTier = "default_claude_max_5x"`.
- Token: same file → `claudeAiOauth.accessToken` (+ `expiresAt`, `refreshToken`).
- Live tokens: `~/.claude/projects/<slug>/<sessionId>.jsonl`, assistant lines, `message.usage.*` (see §3.2).
- Aggregates (rejected as live/cost source): `~/.claude/stats-cache.json`.
- Sessions registry (not used in v1): `~/.claude/sessions/*.json` (`pid`, `sessionId`, `cwd`, `status`, `version`).
- Status-line settings target: `~/.claude/settings.json` (currently has no `statusLine` → clean insert).
- Data volume at design time: ~43 jsonl files, ~107.5 MB, largest ~5.6 MB → incremental parsing is mandatory.
