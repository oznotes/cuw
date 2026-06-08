# Claude Usage Widget Implementation Plan

> **⚠️ Implementation note (2026-06-08):** The `usage-core` portion of this plan
> (M1, M3, M5 logic, config, sources, collector) is **already implemented and
> passing 51 tests** — see `crates/usage-core/`. Two deliberate refinements were
> made vs. this document: (1) a **two-crate split** (`usage-core` lib + `usage-widget`
> bin) so the core builds/tests without gpui/CMake; (2) the collector test seam is
> `Collector::with_deps(config, projects_root, statusline_cache, oauth_fetch)`
> rather than `new_with_sources`. The M0/M2/M4/M6 GUI tasks are still TODO and
> remain accurate guidance. Adjust file paths (`src/…` → `crates/usage-core/src/…`)
> when reading the completed milestones.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a frosted-glass, always-on-top Windows desktop widget in Rust (gpui + gpui-component) that shows real Claude 5h/7d rate-limit utilization, color-coded, reading the OAuth usage endpoint + status-line cache for quota and incremental JSONL parsing for local token detail.

**Architecture:** A background poller produces one immutable UsageSnapshot per tick from three isolated sources (oauth, statusline, jsonl), reconciled in a collector; the gpui UI is a pure render of that snapshot. All unsafe/Win32 (always-on-top, Mica backdrop) is confined to src/win/.

**Tech Stack:** Rust (edition 2024, stable 1.95.0), gpui + gpui_platform + gpui-component (git, pinned), ureq, serde/serde_json, chrono, anyhow, windows crate, rust-embed.

---

## Shared Contracts (canonical — all tasks conform)

These are the authoritative type/signature definitions. Every task references these exact names; no task redefines them (the file that owns each is noted).

### `model.rs` — pure data types (owned by Task M1-1)

```rust
use std::time::SystemTime;

#[derive(Clone, Debug, PartialEq)]
pub struct Window { pub used_pct: f32, pub resets_at: Option<SystemTime> }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level { Ok, Warn, Critical }

impl Level {
    /// pct in 0..=100; warn/critical are the lower bounds (e.g. 70.0, 90.0).
    pub fn from_pct(pct: f32, warn: f32, critical: f32) -> Level {
        if pct >= critical { Level::Critical } else if pct >= warn { Level::Warn } else { Level::Ok }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Provenance { StatusLine, OAuth, Stale { last_good_at: SystemTime } }

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TokenStats {
    pub today_total_output: u64,
    pub by_model: Vec<(String, u64)>,      // sorted desc by tokens
    pub live_tok_per_min: Option<f64>,
    pub top_projects: Vec<(String, u64)>,  // sorted desc; popup-only
}

#[derive(Clone, Debug)]
pub struct UsageSnapshot {
    pub five_hour: Window,
    pub seven_day: Window,
    pub seven_day_opus: Option<Window>,
    pub tokens: TokenStats,
    pub source: Provenance,
    pub fetched_at: SystemTime,
}
```

### `sources/mod.rs` — quota source contract (`QuotaReading` + `reconcile`)

```rust
use crate::model::{Window, Provenance};
use std::time::SystemTime;

#[derive(Clone, Debug)]
pub struct QuotaReading {
    pub five_hour: Window,
    pub seven_day: Window,
    pub seven_day_opus: Option<Window>,
    pub source: Provenance,        // StatusLine or OAuth (never Stale here)
    pub observed_at: SystemTime,
}

/// Prefer a fresh status-line reading, else oauth, else degrade last_good to Stale.
pub fn reconcile(
    statusline: Option<QuotaReading>,
    oauth: Option<QuotaReading>,
    last_good: Option<QuotaReading>,
    now: SystemTime,
    statusline_max_age: std::time::Duration,
) -> Option<(QuotaReading, Provenance)>;   // (reading_to_show, effective_provenance)
```

Source function signatures (all `anyhow::Result` unless noted):
- `oauth::parse_usage_json(body: &str, observed_at: SystemTime) -> anyhow::Result<QuotaReading>`  // pure, unit-tested
- `oauth::fetch(token: &str, cc_version: &str) -> anyhow::Result<QuotaReading>`  // sends the 3 mandatory headers
- `statusline::cache_path() -> std::path::PathBuf`  // ~/.claude/widget-cache/ratelimits.json
- `statusline::read_cache(path: &std::path::Path) -> Option<QuotaReading>`  // None on missing/corrupt
- `statusline::write_cache_from_stdin(stdin_json: &str, path: &std::path::Path) -> anyhow::Result<()>`  // atomic temp+rename

### `sources/jsonl.rs` — JSONL contract (owned by Task M3-1..M3-5)

```rust
use std::time::SystemTime;
#[derive(Clone, Debug, PartialEq)]
pub struct AssistantRecord {
    pub message_id: String,
    pub request_id: String,
    pub model: String,
    pub output_tokens: u64,
    pub timestamp: SystemTime,
    pub project: String,   // parent dir name of the jsonl file
}
/// Parse one JSONL line; Ok(None) if not an assistant usage line. Pure, unit-tested.
pub fn parse_line(line: &str, project: &str) -> anyhow::Result<Option<AssistantRecord>>;

pub struct TokenLedger { /* seen:(msg,req) set; utc day bucket; by_model/by_project today; recent ring */ }
impl TokenLedger {
    pub fn new() -> Self;
    pub fn ingest(&mut self, rec: &AssistantRecord, now: SystemTime);          // dedup on (msg,req); UTC-day roll
    pub fn stats(&self, now: SystemTime) -> crate::model::TokenStats;          // live_tok_per_min from records < 90s old
}

pub struct Cursor { /* path -> (last_size, last_mtime) */ }
impl Cursor {
    pub fn new() -> Self;
    /// Glob ~/.claude/projects/*/*.jsonl (EXCLUDE "subagents"); read appended bytes since
    /// last_size (re-read from 0 if shrank); parse + ingest. Returns count of new records.
    pub fn update(&mut self, projects_root: &std::path::Path, ledger: &mut TokenLedger, now: SystemTime) -> anyhow::Result<usize>;
}
```

### `collector.rs` — Collector contract (owned by Task M2-3, extended in M3-6/M5-4)

```rust
pub struct Collector { /* cursor, ledger, last_good: Option<QuotaReading>, last_oauth_at: Option<SystemTime>, config: Config */ }
impl Collector {
    pub fn new(config: crate::config::Config) -> Self;
    /// One refresh: update jsonl → TokenStats; read statusline cache; maybe poll oauth
    /// (only if >= quota_poll_secs since last_oauth_at AND statusline not fresh); reconcile; build snapshot.
    pub fn tick(&mut self, now: SystemTime) -> crate::model::UsageSnapshot;
}
```

### `config.rs` — Config contract (owned by Task M1-3, extended in M4-2)

```rust
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Config {
    pub position: Option<(f32, f32)>,
    pub scale: f32,                 // 0.6..=2.0, default 1.0
    pub view_mode: ViewMode,        // default Bars
    pub backdrop: Backdrop,         // default Mica
    pub opacity: f32,               // default 1.0
    pub warn_threshold: f32,        // default 70.0
    pub critical_threshold: f32,    // default 90.0
    pub refresh_secs: u64,          // default 30
    pub quota_poll_secs: u64,       // default 180
    pub statusline_max_age_secs: u64, // default 120
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ViewMode { Bars, Gauge }
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Backdrop { Mica, MicaAlt, Acrylic, Transparent, Opaque }
impl Default for Config { /* the defaults above */ }
impl Config {
    pub fn path() -> std::path::PathBuf;          // %APPDATA%/claude-usage/widget-config.json (dirs::config_dir)
    pub fn load() -> Config;                       // defaults on missing/corrupt; never errors
    pub fn save(&self) -> anyhow::Result<()>;
}
```

### `Cargo.toml` dependency block (use verbatim; revs pinned in Task M0-2)

```toml
[package]
name = "claude-usage-widget"
version = "0.1.0"
edition = "2024"

[dependencies]
gpui            = { git = "https://github.com/zed-industries/zed" }            # rev pinned in M0
gpui_platform   = { git = "https://github.com/zed-industries/zed", features = ["font-kit"] }
gpui-component  = { git = "https://github.com/longbridge/gpui-component" }     # rev pinned in M0
anyhow          = "1"
serde           = { version = "1", features = ["derive"] }
serde_json      = "1"
ureq            = { version = "2", features = ["json", "tls"] }
chrono          = { version = "0.4", default-features = false, features = ["clock", "std"] }
dirs            = "5"
raw-window-handle = "0.6"
windows         = { version = "0.58", features = ["Win32_Foundation", "Win32_UI_WindowsAndMessaging", "Win32_Graphics_Dwm"] }
rust-embed      = "8"

[build-dependencies]
embed-resource  = "2"

[profile.release]
lto = true
strip = true
```

(Exact gpui/gpui-component/windows crate versions are confirmed against the pinned example in M0; if a name differs, the M0 task fixes the block and all later code refers back to THIS block.)

---

## File Structure

One responsibility per module (under `C:\Users\oz\Desktop\claude-usage\src\`):

| File | Responsibility |
|------|----------------|
| `main.rs` | Entry; arg dispatch (`--statusline` → statusline_cmd; `--register-statusline` → registration; else GUI); app bootstrap; spawn poller; apply backdrop + topmost. |
| `model.rs` | Pure data types + `Level::from_pct`. No I/O. Fully unit-tested. |
| `timeutil.rs` | Timestamp normalization (ISO-8601, Unix-sec, ms → SystemTime; UTC-day; reset countdown). Unit-tested. |
| `config.rs` | `Config` struct + load/save/merge + mutators. Unit-tested. |
| `sources/mod.rs` | `QuotaReading` type + `reconcile()`; re-exports submodules. |
| `sources/oauth.rs` | OAuth usage endpoint client → `QuotaReading` (`parse_usage_json` pure + `fetch`). |
| `sources/statusline.rs` | Read/write the rate-limit cache file. |
| `sources/jsonl.rs` | Incremental `Cursor` + dedup + `TokenLedger` → `TokenStats`. |
| `collector.rs` | `Collector`: holds state (Cursor, ledger, last_good, last_oauth_at); `tick()` → `UsageSnapshot`. |
| `statusline_cmd.rs` | `run_statusline(stdin, path)` → write cache + return a line. |
| `statusline_register.rs` | Optional `~/.claude/settings.json` registration helper. |
| `ui/mod.rs` | Root view entity holding latest `UsageSnapshot`; view-mode toggle; menu; drag. |
| `ui/bars.rs` | Render bars view from a `UsageSnapshot`. |
| `ui/gauge.rs` | Render gauge (ProgressCircle rings) view. |
| `ui/theme.rs` | `Level` → gpui color; legibility scrim helper. |
| `win/topmost.rs` | `apply_topmost(hwnd)` via `SetWindowPos(HWND_TOPMOST)`. |
| `win/backdrop.rs` | Choose `WindowBackgroundAppearance` from Config + Win build; Win10 fallback. |

---

## Milestone M0: Setup & spike

**Goal:** Prove the riskiest part of the stack end-to-end on *this* Windows 11 machine — build gpui + gpui-component from source, open a borderless frosted-glass (Mica) window, force it always-on-top via raw `HWND` + `SetWindowPos(HWND_TOPMOST)`, and render one hardcoded `gpui_component::ProgressBar`. This is the **go/no-go gate**: if the build or the topmost+Mica combination fights us here, stop and reconsider before sinking more effort in.

**Verification kind: manual-verify.** There are no unit tests in M0. Each task is verified by building/running the program and visually observing the window on screen (GUI + Win32 cannot be unit-tested). Every task still ends with a commit.

> Prerequisite (one-time, off-plan): MSVC toolchain (Visual Studio 2022, "Desktop development with C++" workload) and CMake must be installed and on `PATH`, or gpui will fail to build from source. If `cargo build` later errors on a missing C++ compiler or `cmake`, install these first. This is documented properly in M6's README; here we just need them present.

---

### Task M0-1: Repo skeleton, toolchain pin, and an empty crate that builds

**Files:**
- Create: `C:\Users\oz\Desktop\claude-usage\rust-toolchain.toml`
- Create: `C:\Users\oz\Desktop\claude-usage\.gitignore`
- Create: `C:\Users\oz\Desktop\claude-usage\Cargo.toml`
- Create: `C:\Users\oz\Desktop\claude-usage\src\main.rs`

- [ ] **Step 1: Initialize the git repo.** From `C:\Users\oz\Desktop\claude-usage\`, run `git init`. (The working directory already exists and contains `docs/`; we add crate files alongside it.)

- [ ] **Step 2: Pin the toolchain.** Create `rust-toolchain.toml`:
  ```toml
  [toolchain]
  channel = "1.95.0"
  components = ["rustfmt", "clippy"]
  targets = ["x86_64-pc-windows-msvc"]
  profile = "minimal"
  ```

- [ ] **Step 3: Add a `.gitignore`:**
  ```gitignore
  /target
  **/*.rs.bk
  *.pdb
  ```
  Note: we do **not** ignore `Cargo.lock` — the spine requires committing it (git-only, pinned deps; deliberate upgrades only).

- [ ] **Step 4: Create a minimal `Cargo.toml` (deps added in M0-2):**
  ```toml
  [package]
  name = "claude-usage-widget"
  version = "0.1.0"
  edition = "2024"

  [dependencies]

  [profile.release]
  lto = true
  strip = true
  ```

- [ ] **Step 5: Create a trivial `src\main.rs`:**
  ```rust
  fn main() {
      println!("claude-usage-widget: skeleton builds");
  }
  ```

- [ ] **Step 6: Run + observe — toolchain works.** Run `cargo run`. **Look for:** Cargo reports `rustc 1.95.0`, the build succeeds, and the terminal prints exactly `claude-usage-widget: skeleton builds`. If rustup says toolchain `1.95.0` is not installed, let it auto-install (or run `rustup toolchain install 1.95.0`) and re-run.

- [ ] **Step 7: Commit.**
  ```
  git add rust-toolchain.toml .gitignore Cargo.toml src/main.rs
  git commit -m "chore: scaffold claude-usage-widget crate, pin toolchain 1.95.0"
  ```

---

### Task M0-2: Add and pin the gpui / gpui-component / windows dependency stack

**Files:**
- Modify: `C:\Users\oz\Desktop\claude-usage\Cargo.toml`

- [ ] **Step 1: Replace the dependency block with the authoritative Shared Contracts block.** Overwrite `Cargo.toml` with the verbatim dependency block from the **Shared Contracts** section above (this supersedes the empty `[dependencies]` from M0-1).

- [ ] **Step 2: Fetch and resolve the dependency graph.** Run `cargo fetch`. This clones zed and gpui-component (large; long first fetch). **Look for:** it completes without a resolver error. If the resolver reports that crate name `gpui_platform` or feature `font-kit` does not exist at upstream `HEAD`, that is the *one* place the spine permits a name fix — proceed to Step 3.

- [ ] **Step 3: Confirm crate names + features against the pinned gpui-component example (authoritative reconciliation point).** Open the gpui-component repo's window example (its `examples/` and root `Cargo.toml`) and confirm the exact dependency entries for `gpui`, the gpui platform/font crate, and `gpui-component`:
  - Is the platform crate named `gpui_platform` with feature `font-kit`, or does the example depend only on `gpui` (with `gpui_platform` re-exported)? Several gpui revisions expose `gpui::Application` directly.
  - Confirm the `windows` crate major version gpui pulls (we declared `0.58`); if gpui's transitive `windows` differs and causes a duplicate-version conflict, align our direct dep.

  **If and only if a name/feature differs**, edit `Cargo.toml` to match the example and note the corrected names in a comment. All later milestones reference *this* corrected block. If everything resolves as written, leave it verbatim.

- [ ] **Step 4: Pin explicit git revs.** After a successful resolve, read the resolved SHAs from `Cargo.lock` (the `git+https://github.com/...#<sha>` source lines). Edit the three git deps to add `rev = "<sha>"`:
  ```toml
  gpui            = { git = "https://github.com/zed-industries/zed", rev = "<zed-sha-from-Cargo.lock>" }
  gpui_platform   = { git = "https://github.com/zed-industries/zed", rev = "<zed-sha-from-Cargo.lock>", features = ["font-kit"] }
  gpui-component  = { git = "https://github.com/longbridge/gpui-component", rev = "<gpui-component-sha-from-Cargo.lock>" }
  ```
  Use the same zed SHA for both zed-sourced crates.

- [ ] **Step 5: Re-resolve with the pins.** Run `cargo update -p gpui -p gpui-component --dry-run`. **Look for:** nothing to change. Per the spine, after this we treat upgrades as deliberate events and never run a blanket `cargo update`.

- [ ] **Step 6: Compile the dependency graph (no app code yet).** Run `cargo build`. **Look for:** gpui, gpui_platform, gpui-component, and the `windows` crate all compile from source (heavy first build — minutes, possibly 10+). If it fails on a missing C++ compiler/CMake, install the MSVC prerequisite and rebuild.

- [ ] **Step 7: Commit (including the lockfile).**
  ```
  git add Cargo.toml Cargo.lock
  git commit -m "build: add and pin gpui/gpui-component/windows deps; commit lockfile"
  ```

---

### Task M0-3: `build.rs` + embedded `.exe` icon

**Files:**
- Create: `C:\Users\oz\Desktop\claude-usage\build.rs`
- Create: `C:\Users\oz\Desktop\claude-usage\assets\icon.ico`
- Create: `C:\Users\oz\Desktop\claude-usage\app.rc`
- Modify: `C:\Users\oz\Desktop\claude-usage\Cargo.toml` (already has `embed-resource` in `[build-dependencies]` from M0-2)

> Naming standard (used everywhere, incl. M6-4): the icon asset is `assets/icon.ico` and the resource script is `app.rc`.

- [ ] **Step 1: Add an icon asset.** Place a real multi-resolution `.ico` (16/32/48/256 px) at `assets\icon.ico`. A placeholder is fine for the spike; it must be a valid `.ico`. From a PNG: `magick convert logo.png -define icon:auto-resize=256,48,32,16 assets\icon.ico`. The final designed icon is a M6 concern.

- [ ] **Step 2: Create the Windows resource script.** Create `app.rc`:
  ```
  1 ICON "assets/icon.ico"
  ```

- [ ] **Step 3: Create `build.rs`:**
  ```rust
  fn main() {
      // Embed the Windows .exe icon on Windows builds; no-op elsewhere.
      #[cfg(target_os = "windows")]
      {
          embed_resource::compile("app.rc", embed_resource::NONE);
      }
      println!("cargo:rerun-if-changed=app.rc");
      println!("cargo:rerun-if-changed=assets/icon.ico");
  }
  ```
  > Note: confirm the `embed-resource` 2.x surface against its docs. If the pinned 2.x point release exposes the older single-arg `compile("app.rc")`, drop the second argument; this is the only line affected.

- [ ] **Step 4: Build + observe — resource compiles.** Run `cargo build`. **Look for:** the build succeeds and `build.rs` runs without a resource-compiler error (it invokes MSVC `rc.exe`/llvm-rc).

- [ ] **Step 5: Observe the icon on the binary.** In Explorer, open `target\debug\` and find `claude-usage-widget.exe`. **Look for:** it uses your `icon.ico` artwork (not the generic blank exe icon).

- [ ] **Step 6: Commit.**
  ```
  git add build.rs app.rc assets/icon.ico Cargo.toml
  git commit -m "build: embed .exe icon via embed-resource build script"
  ```

---

### Task M0-4: Borderless gpui-component window (Mica backdrop) with a hardcoded ProgressBar

**Files:**
- Modify: `C:\Users\oz\Desktop\claude-usage\src\main.rs`

- [ ] **Step 1: Replace `main.rs` with a minimal gpui-component app.** Opens one borderless Mica window, runs `gpui_component::init(cx)`, wraps the root in `gpui_component::Root::new(...)`, renders a hardcoded `ProgressBar` at 72%. Overwrite `src\main.rs`:
  ```rust
  use gpui::{
      div, px, size, Bounds, IntoElement, ParentElement, Render, Styled,
      WindowBackgroundAppearance, WindowBounds, WindowOptions,
  };
  use gpui_component::progress::ProgressBar;
  use gpui_component::Root;

  struct SpikeRoot;

  impl Render for SpikeRoot {
      fn render(
          &mut self,
          _window: &mut gpui::Window,
          _cx: &mut gpui::Context<Self>,
      ) -> impl IntoElement {
          div()
              .flex()
              .flex_col()
              .gap_2()
              .p_4()
              .child("Claude · Max 5x")
              .child("5H  72%")
              .child(ProgressBar::new().value(72.0))
      }
  }

  fn main() {
      gpui_platform::application().run(|cx| {
          gpui_component::init(cx);

          let bounds = Bounds::centered(None, size(px(320.0), px(160.0)), cx);

          let options = WindowOptions {
              titlebar: None,
              is_resizable: false,
              window_background: WindowBackgroundAppearance::MicaBackdrop,
              window_bounds: Some(WindowBounds::Windowed(bounds)),
              ..Default::default()
          };

          cx.open_window(options, |window, cx| {
              let view = cx.new(|_cx| SpikeRoot);
              cx.new(|cx| Root::new(view.into(), window, cx))
          })
          .expect("failed to open window");
      });
  }
  ```
  > Reconciliation note (confirm against the pinned gpui-component example from M0-2): the *exact* spellings of (a) the application entry — `gpui_platform::application().run(..)` vs `gpui::Application::new().run(..)`, (b) the `ProgressBar` constructor — `ProgressBar::new()` vs `ProgressBar::new(id)` and whether `.value(..)` takes `f32` or `0..=100`, (c) the `Root::new(view, window, cx)` argument types (often `AnyView`, hence `view.into()`), and (d) `Bounds::centered`, can drift between revs. Keep `init` + `Root::new` + `MicaBackdrop` + `titlebar: None` intact; adjust only call spellings to whatever compiles.

- [ ] **Step 2: Build.** Run `cargo build`. **Look for:** a clean compile. If it fails, the error is almost certainly one of the four call-spelling drifts above — fix against the example and rebuild.

- [ ] **Step 3: Run + observe — frosted borderless window with a bar.** Run `cargo run`. **Look for:** a ~320×160 window with **no title bar/border**; a **frosted/translucent Mica** background (wallpaper bleeds through with blur — drag another window behind it to confirm); the labels; and a **ProgressBar filled to ~72%**. If the window is solid/opaque, Mica may be unavailable (full Win10 fallback is M6); for M0 the gate is satisfied as long as it runs and renders — note whether Mica engaged.

- [ ] **Step 4: Commit.**
  ```
  git add src/main.rs
  git commit -m "feat: borderless Mica window rendering a hardcoded ProgressBar"
  ```

---

### Task M0-5: Force the window always-on-top via raw `HWND` + `SetWindowPos(HWND_TOPMOST)`

**Files:**
- Create: `C:\Users\oz\Desktop\claude-usage\src\win\mod.rs`
- Create: `C:\Users\oz\Desktop\claude-usage\src\win\topmost.rs`
- Modify: `C:\Users\oz\Desktop\claude-usage\src\main.rs`

- [ ] **Step 1: Create the `win` module declaration.** Create `src\win\mod.rs`:
  ```rust
  pub mod topmost;
  // backdrop added in M6.
  ```

- [ ] **Step 2: Implement `apply_topmost(hwnd)` — the only `unsafe` in the spike.** Create `src\win\topmost.rs`:
  ```rust
  use windows::Win32::Foundation::HWND;
  use windows::Win32::UI::WindowsAndMessaging::{
      SetWindowPos, HWND_TOPMOST, SWP_NOMOVE, SWP_NOSIZE,
  };

  /// Force the given top-level window to the always-on-top band.
  /// gpui has no always-on-top on Windows, so we call SetWindowPos directly.
  /// `hwnd_raw` is the platform window handle obtained via raw-window-handle.
  pub fn apply_topmost(hwnd_raw: isize) -> anyhow::Result<()> {
      let hwnd = HWND(hwnd_raw as *mut core::ffi::c_void);
      // SAFETY: hwnd is a live top-level window handle owned by our process;
      // SetWindowPos with NOMOVE|NOSIZE only changes the Z-order band.
      unsafe {
          SetWindowPos(
              hwnd,
              Some(HWND_TOPMOST),
              0,
              0,
              0,
              0,
              SWP_NOMOVE | SWP_NOSIZE,
          )?;
      }
      Ok(())
  }
  ```
  > Note (windows 0.58 surface): `HWND` wraps `*mut c_void`; `SetWindowPos` returns `windows::core::Result<()>` with the relative-window arg as `Option<HWND>` (hence `Some(HWND_TOPMOST)`); `?` converts `windows::core::Error` into `anyhow::Error`. If the resolved `windows` patch differs, adjust the two constructions; confirm against `cargo doc -p windows`.

- [ ] **Step 3: Wire the module + call it after the window opens.** Replace `src\main.rs`:
  ```rust
  mod win;

  use gpui::{
      div, px, size, Bounds, IntoElement, ParentElement, Render, Styled,
      WindowBackgroundAppearance, WindowBounds, WindowOptions,
  };
  use gpui_component::progress::ProgressBar;
  use gpui_component::Root;
  use raw_window_handle::{HasWindowHandle, RawWindowHandle};

  struct SpikeRoot;

  impl Render for SpikeRoot {
      fn render(
          &mut self,
          _window: &mut gpui::Window,
          _cx: &mut gpui::Context<Self>,
      ) -> impl IntoElement {
          div()
              .flex()
              .flex_col()
              .gap_2()
              .p_4()
              .child("Claude · Max 5x")
              .child("5H  72%")
              .child(ProgressBar::new().value(72.0))
      }
  }

  /// Pull the Win32 HWND out of a gpui window's raw-window-handle.
  fn hwnd_of(window: &gpui::Window) -> Option<isize> {
      let handle = window.window_handle().ok()?;
      match handle.as_raw() {
          RawWindowHandle::Win32(h) => Some(isize::from(h.hwnd)),
          _ => None,
      }
  }

  fn main() {
      gpui_platform::application().run(|cx| {
          gpui_component::init(cx);

          let bounds = Bounds::centered(None, size(px(320.0), px(160.0)), cx);

          let options = WindowOptions {
              titlebar: None,
              is_resizable: false,
              window_background: WindowBackgroundAppearance::MicaBackdrop,
              window_bounds: Some(WindowBounds::Windowed(bounds)),
              ..Default::default()
          };

          cx.open_window(options, |window, cx| {
              if let Some(hwnd) = hwnd_of(window) {
                  if let Err(e) = win::topmost::apply_topmost(hwnd) {
                      eprintln!("apply_topmost failed: {e:#}");
                  }
              } else {
                  eprintln!("could not obtain Win32 HWND for topmost");
              }

              let view = cx.new(|_cx| SpikeRoot);
              cx.new(|cx| Root::new(view.into(), window, cx))
          })
          .expect("failed to open window");
      });
  }
  ```
  > Reconciliation note: confirm how the raw handle is reached from a gpui `Window` against the pinned rev. The portable path is `gpui::Window` implementing `raw_window_handle::HasWindowHandle`. If the locked gpui exposes it differently (a `window.raw_window_handle()` method, or the call must run later via a window-created/activate hook), move `apply_topmost` to wherever the live `HWND` is available — the `win::topmost` function does not change. Also confirm `raw-window-handle` 0.6's `Win32WindowHandle::hwnd` type (`NonZeroIsize`) and adjust the `isize::from(..)` conversion if needed.

- [ ] **Step 4: Build.** Run `cargo build`. **Look for:** clean compile; the `unsafe` block and `windows` crate API calls resolve.

- [ ] **Step 5: Run + observe — the window stays on top.** Run `cargo run`. Open another app, click into it, and try to cover the widget. **Look for:** the frosted borderless widget **stays visibly above** the other (focused) window; the `72%` bar and labels remain readable; bringing various apps to the foreground does not hide it. (Re-asserting topmost on focus loss is a later refinement; for M0 the static `HWND_TOPMOST` holding above a focused window is the gate.)

- [ ] **Step 6: Commit.**
  ```
  git add src/win/mod.rs src/win/topmost.rs src/main.rs
  git commit -m "feat: force always-on-top via SetWindowPos(HWND_TOPMOST) on raw HWND"
  ```

---

### Exit criteria for M0 (go/no-go gate — all must be observably true)

- `cargo build` and `cargo run` succeed on this Windows 11 (build 26200) machine with pinned toolchain `1.95.0`, after gpui + gpui-component compiled from source.
- The three git deps (`gpui`, `gpui_platform`, `gpui-component`) are pinned to explicit revs in `Cargo.toml`, and `Cargo.lock` is committed.
- A **borderless** window opens and renders one hardcoded `gpui_component::ProgressBar` at ~72% plus the label text.
- The window shows the **Mica frosted-glass backdrop**. If Mica did not engage, that is noted as a risk for M6's fallback chain — but the run still succeeded.
- The window is **always-on-top**: it stays visibly above another, focused application window.
- All `unsafe`/Win32 code is confined to `src/win/topmost.rs`; the `.exe` carries the embedded icon from `assets/icon.ico`.
- Each task was committed. If any criterion fails and cannot be remedied with a call-spelling fix against the pinned example, **stop and reconsider the stack before proceeding to M1**.

---

## Milestone M1: Data core

**Goal:** Build the pure, I/O-free data layer — `model.rs` (types + `Level::from_pct`), `timeutil.rs` (timestamp normalization), `config.rs` (load/save/merge), and the OAuth quota client `sources/oauth.rs` (`parse_usage_json` pure + `fetch` over `ureq`) — so a `UsageSnapshot`'s quota half can be produced and printed from real Anthropic data.

**Verification kind: TDD** — every behavior is written test-first: failing test → `cargo test` FAIL → minimal impl → `cargo test` PASS → commit. All unit tests live inline in `#[cfg(test)] mod tests` in the file under test. No GUI, no Win32, no live network in tests (`fetch` is exercised manually at the end).

> Assumes M0 is complete: the crate exists with the exact `Cargo.toml` dependency block, `rust-toolchain.toml` pinning 1.95.0, edition 2024, and `src/main.rs` opening a window. M1 adds modules alongside `main.rs`; declare each new module in `main.rs` as it is created.
>
> Note on `cargo test --lib`: this crate is a single binary, so inline tests run under the bin target. If `--lib` reports "no library targets", use `cargo test --bin claude-usage-widget <filter>` instead. This applies to every `cargo test --lib` command in this plan.

---

### Task M1-1: `model.rs` — core types + `Level::from_pct`

**Files:**
- Create: `C:\Users\oz\Desktop\claude-usage\src\model.rs`
- Modify: `C:\Users\oz\Desktop\claude-usage\src\main.rs` (add `mod model;`)

- [ ] **Step 1: Declare the module in `main.rs`.** Add `mod model;` near the top of `src\main.rs`, after any existing `use` lines and before `fn main`.

- [ ] **Step 2: Write the failing test (full file with types + tests).** Create `src\model.rs` with the canonical types from the Shared Contracts AND the test module, but stub `Level::from_pct` to `unimplemented!()` so the test fails for the right reason:
  ```rust
  use std::time::SystemTime;

  #[derive(Clone, Debug, PartialEq)]
  pub struct Window {
      pub used_pct: f32,
      pub resets_at: Option<SystemTime>,
  }

  #[derive(Clone, Copy, Debug, PartialEq, Eq)]
  pub enum Level {
      Ok,
      Warn,
      Critical,
  }

  impl Level {
      /// pct in 0..=100; warn/critical are the lower bounds (e.g. 70.0, 90.0).
      pub fn from_pct(_pct: f32, _warn: f32, _critical: f32) -> Level {
          unimplemented!()
      }
  }

  #[derive(Clone, Debug, PartialEq)]
  pub enum Provenance {
      StatusLine,
      OAuth,
      Stale { last_good_at: SystemTime },
  }

  #[derive(Clone, Debug, Default, PartialEq)]
  pub struct TokenStats {
      pub today_total_output: u64,
      pub by_model: Vec<(String, u64)>,
      pub live_tok_per_min: Option<f64>,
      pub top_projects: Vec<(String, u64)>,
  }

  #[derive(Clone, Debug)]
  pub struct UsageSnapshot {
      pub five_hour: Window,
      pub seven_day: Window,
      pub seven_day_opus: Option<Window>,
      pub tokens: TokenStats,
      pub source: Provenance,
      pub fetched_at: SystemTime,
  }

  #[cfg(test)]
  mod tests {
      use super::*;

      #[test]
      fn level_ok_below_warn() {
          assert_eq!(Level::from_pct(0.0, 70.0, 90.0), Level::Ok);
          assert_eq!(Level::from_pct(69.9, 70.0, 90.0), Level::Ok);
      }

      #[test]
      fn level_warn_at_and_above_warn_below_critical() {
          assert_eq!(Level::from_pct(70.0, 70.0, 90.0), Level::Warn);
          assert_eq!(Level::from_pct(89.9, 70.0, 90.0), Level::Warn);
      }

      #[test]
      fn level_critical_at_and_above_critical() {
          assert_eq!(Level::from_pct(90.0, 70.0, 90.0), Level::Critical);
          assert_eq!(Level::from_pct(100.0, 70.0, 90.0), Level::Critical);
          assert_eq!(Level::from_pct(150.0, 70.0, 90.0), Level::Critical);
      }

      #[test]
      fn token_stats_default_is_empty() {
          let s = TokenStats::default();
          assert_eq!(s.today_total_output, 0);
          assert!(s.by_model.is_empty());
          assert!(s.live_tok_per_min.is_none());
          assert!(s.top_projects.is_empty());
      }
  }
  ```

- [ ] **Step 3: Run the test, expect FAIL.** `cargo test --lib model::tests`. Expected: the three `level_*` tests panic with `not implemented`; `token_stats_default_is_empty` passes. Overall: FAILED.

- [ ] **Step 4: Minimal implementation.** Replace the stubbed `from_pct` body with the canonical logic:
  ```rust
  impl Level {
      pub fn from_pct(pct: f32, warn: f32, critical: f32) -> Level {
          if pct >= critical {
              Level::Critical
          } else if pct >= warn {
              Level::Warn
          } else {
              Level::Ok
          }
      }
  }
  ```

- [ ] **Step 5: Run the test, expect PASS.** `cargo test --lib model::tests` → `4 passed; 0 failed`.

- [ ] **Step 6: Commit.**
  ```
  git add src/model.rs src/main.rs
  git commit -m "feat(model): add core UsageSnapshot types and Level::from_pct"
  ```

---

### Task M1-2: `timeutil.rs` — timestamp normalization

The quota sources speak different time units: OAuth uses ISO-8601, the status-line uses Unix epoch **seconds**, JSONL timestamps are ISO-8601 Z. We also need UTC-day helpers. This module normalizes all to `std::time::SystemTime` (spine: "convert to `SystemTime` at module boundaries"). Pure and fully unit-tested.

**Files:**
- Create: `C:\Users\oz\Desktop\claude-usage\src\timeutil.rs`
- Modify: `C:\Users\oz\Desktop\claude-usage\src\main.rs` (add `mod timeutil;`)

> Exports used by later milestones: `parse_iso8601`, `from_unix_secs`, `from_unix_millis`, `utc_day` (i64 day-count, M1 tests), and **`utc_day_start` (SystemTime at UTC midnight)** — consumed by `TokenLedger` in M3. Both `utc_day` helpers are defined here so M3 never redefines time logic.

- [ ] **Step 1: Declare the module.** Add `mod timeutil;` to `src\main.rs`.

- [ ] **Step 2: Write the failing test (full file with stubs + tests).** Create `src\timeutil.rs`. Define the helpers as stubs returning `unimplemented!()`, plus the full test module. Known instants: `2021-01-01T00:00:00Z` = Unix `1_609_459_200`; `2026-06-08T12:34:56Z` = Unix `1_780_403_696`.
  ```rust
  use anyhow::{Context, Result};
  use std::time::{Duration, SystemTime, UNIX_EPOCH};

  /// Parse an ISO-8601 / RFC-3339 timestamp (e.g. "2026-06-08T12:34:56Z") into SystemTime.
  pub fn parse_iso8601(_s: &str) -> Result<SystemTime> {
      unimplemented!()
  }

  /// Unix epoch SECONDS (status-line `resets_at` form) → SystemTime.
  pub fn from_unix_secs(_secs: i64) -> SystemTime {
      unimplemented!()
  }

  /// Unix epoch MILLISECONDS → SystemTime.
  pub fn from_unix_millis(_ms: i64) -> SystemTime {
      unimplemented!()
  }

  /// Whole days since the Unix epoch (UTC midnight boundary). Two instants share
  /// a "today" iff this is equal.
  pub fn utc_day(_t: SystemTime) -> i64 {
      unimplemented!()
  }

  /// The UTC-midnight instant that begins the day containing `t`.
  pub fn utc_day_start(_t: SystemTime) -> SystemTime {
      unimplemented!()
  }

  #[cfg(test)]
  mod tests {
      use super::*;

      const EPOCH_2021: i64 = 1_609_459_200; // 2021-01-01T00:00:00Z
      const T_2026: i64 = 1_780_403_696;     // 2026-06-08T12:34:56Z

      #[test]
      fn iso8601_z_parses_to_expected_unix_secs() {
          let t = parse_iso8601("2021-01-01T00:00:00Z").unwrap();
          let d = t.duration_since(UNIX_EPOCH).unwrap();
          assert_eq!(d.as_secs(), EPOCH_2021 as u64);
      }

      #[test]
      fn iso8601_with_offset_parses() {
          let t = parse_iso8601("2026-06-08T14:34:56+02:00").unwrap();
          let d = t.duration_since(UNIX_EPOCH).unwrap();
          assert_eq!(d.as_secs(), T_2026 as u64);
      }

      #[test]
      fn iso8601_with_fractional_seconds_parses() {
          let t = parse_iso8601("2026-06-08T12:34:56.500Z").unwrap();
          let d = t.duration_since(UNIX_EPOCH).unwrap();
          assert_eq!(d.as_secs(), T_2026 as u64);
          assert_eq!(d.subsec_millis(), 500);
      }

      #[test]
      fn iso8601_garbage_errors() {
          assert!(parse_iso8601("not a date").is_err());
      }

      #[test]
      fn unix_secs_roundtrips() {
          let t = from_unix_secs(EPOCH_2021);
          let d = t.duration_since(UNIX_EPOCH).unwrap();
          assert_eq!(d.as_secs(), EPOCH_2021 as u64);
          assert_eq!(d.subsec_nanos(), 0);
      }

      #[test]
      fn unix_millis_keeps_sub_second() {
          let t = from_unix_millis(EPOCH_2021 * 1000 + 250);
          let d = t.duration_since(UNIX_EPOCH).unwrap();
          assert_eq!(d.as_secs(), EPOCH_2021 as u64);
          assert_eq!(d.subsec_millis(), 250);
      }

      #[test]
      fn iso_and_unix_secs_agree() {
          assert_eq!(
              parse_iso8601("2021-01-01T00:00:00Z").unwrap(),
              from_unix_secs(EPOCH_2021)
          );
      }

      #[test]
      fn utc_day_is_stable_within_a_day_and_rolls_at_midnight() {
          let start = parse_iso8601("2026-06-08T00:00:00Z").unwrap();
          let late = parse_iso8601("2026-06-08T23:59:59Z").unwrap();
          let next = parse_iso8601("2026-06-09T00:00:00Z").unwrap();
          assert_eq!(utc_day(start), utc_day(late));
          assert_eq!(utc_day(next), utc_day(start) + 1);
      }

      #[test]
      fn utc_day_matches_known_epoch_day_count() {
          let t = from_unix_secs(EPOCH_2021);
          assert_eq!(utc_day(t), 18628); // 1_609_459_200 / 86_400
      }

      #[test]
      fn utc_day_start_is_midnight_of_the_day() {
          let mid = parse_iso8601("2026-06-08T00:00:00Z").unwrap();
          let late = parse_iso8601("2026-06-08T23:58:00Z").unwrap();
          assert_eq!(utc_day_start(late), mid);
          assert_eq!(utc_day_start(mid), mid);
      }
  }
  ```

- [ ] **Step 3: Run the test, expect FAIL.** `cargo test --lib timeutil::tests`. Expected: every test calling a helper panics with `not implemented`. Overall: FAILED.

- [ ] **Step 4: Minimal implementation.** Replace the stub bodies using `chrono`:
  ```rust
  use chrono::{DateTime, Utc};

  pub fn parse_iso8601(s: &str) -> Result<SystemTime> {
      let dt: DateTime<Utc> = DateTime::parse_from_rfc3339(s.trim())
          .with_context(|| format!("invalid ISO-8601 timestamp: {s:?}"))?
          .with_timezone(&Utc);
      let secs = dt.timestamp();
      let nanos = dt.timestamp_subsec_nanos();
      let base = if secs >= 0 {
          UNIX_EPOCH + Duration::new(secs as u64, nanos)
      } else {
          UNIX_EPOCH - Duration::new((-secs) as u64, 0) + Duration::from_nanos(nanos as u64)
      };
      Ok(base)
  }

  pub fn from_unix_secs(secs: i64) -> SystemTime {
      if secs >= 0 {
          UNIX_EPOCH + Duration::from_secs(secs as u64)
      } else {
          UNIX_EPOCH - Duration::from_secs((-secs) as u64)
      }
  }

  pub fn from_unix_millis(ms: i64) -> SystemTime {
      if ms >= 0 {
          UNIX_EPOCH + Duration::from_millis(ms as u64)
      } else {
          UNIX_EPOCH - Duration::from_millis((-ms) as u64)
      }
  }

  pub fn utc_day(t: SystemTime) -> i64 {
      let secs = match t.duration_since(UNIX_EPOCH) {
          Ok(d) => d.as_secs() as i64,
          Err(e) => -(e.duration().as_secs() as i64),
      };
      secs.div_euclid(86_400)
  }

  pub fn utc_day_start(t: SystemTime) -> SystemTime {
      from_unix_secs(utc_day(t) * 86_400)
  }
  ```
  > Put `use chrono::{DateTime, Utc};` at the top with the other imports (replace the stub `use` line). Keep `anyhow::{Context, Result}` and the `std::time` imports. Trim any unused imports to avoid `unused_import` warnings.

- [ ] **Step 5: Run the test, expect PASS.** `cargo test --lib timeutil::tests` → `11 passed; 0 failed`.

- [ ] **Step 6: Commit.**
  ```
  git add src/timeutil.rs src/main.rs
  git commit -m "feat(timeutil): normalize ISO-8601 / Unix-secs / millis to SystemTime + utc_day helpers"
  ```

---

### Task M1-3: `config.rs` — `Config` struct + load/save/merge

The persisted settings, with `#[serde(default)]` so missing/extra fields are forward-compatible (spec §9). `load()` never errors; `save()` returns `anyhow::Result`. Tests cover defaults, partial-JSON merge, corrupt-file tolerance, and a round-trip, using private `load_from`/`save_to` seams so tests never touch the real `%APPDATA%` path.

**Files:**
- Create: `C:\Users\oz\Desktop\claude-usage\src\config.rs`
- Modify: `C:\Users\oz\Desktop\claude-usage\src\main.rs` (add `mod config;`)

- [ ] **Step 1: Declare the module.** Add `mod config;` to `src\main.rs`.

- [ ] **Step 2: Write the failing test (full file with stubs + tests).** Create `src\config.rs` with the exact Shared Contracts `Config` shape, `Default`, the public `path`/`load`/`save`, private `load_from`/`save_to` seams (stubbed `unimplemented!()`), and the full test module:
  ```rust
  use anyhow::{Context, Result};
  use std::path::{Path, PathBuf};

  #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
  #[serde(default)]
  pub struct Config {
      pub position: Option<(f32, f32)>,
      pub scale: f32,
      pub view_mode: ViewMode,
      pub backdrop: Backdrop,
      pub opacity: f32,
      pub warn_threshold: f32,
      pub critical_threshold: f32,
      pub refresh_secs: u64,
      pub quota_poll_secs: u64,
      pub statusline_max_age_secs: u64,
  }

  #[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
  pub enum ViewMode { Bars, Gauge }

  #[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
  pub enum Backdrop { Mica, MicaAlt, Acrylic, Transparent, Opaque }

  impl Default for Config {
      fn default() -> Self {
          Config {
              position: None,
              scale: 1.0,
              view_mode: ViewMode::Bars,
              backdrop: Backdrop::Mica,
              opacity: 1.0,
              warn_threshold: 70.0,
              critical_threshold: 90.0,
              refresh_secs: 30,
              quota_poll_secs: 180,
              statusline_max_age_secs: 120,
          }
      }
  }

  impl Config {
      /// %APPDATA%/claude-usage/widget-config.json (dirs::config_dir).
      pub fn path() -> PathBuf {
          unimplemented!()
      }

      pub fn load() -> Config {
          Self::load_from(&Self::path())
      }

      pub fn save(&self) -> Result<()> {
          self.save_to(&Self::path())
      }

      fn load_from(_path: &Path) -> Config {
          unimplemented!()
      }

      fn save_to(&self, _path: &Path) -> Result<()> {
          unimplemented!()
      }
  }

  #[cfg(test)]
  mod tests {
      use super::*;
      use std::env;

      fn temp_path(name: &str) -> PathBuf {
          let mut p = env::temp_dir();
          let unique = format!(
              "claude-usage-test-{}-{}-{}",
              std::process::id(),
              name,
              std::time::SystemTime::now()
                  .duration_since(std::time::UNIX_EPOCH)
                  .unwrap()
                  .as_nanos()
          );
          p.push(unique);
          p.push("widget-config.json");
          p
      }

      #[test]
      fn defaults_are_the_canonical_values() {
          let c = Config::default();
          assert_eq!(c.position, None);
          assert_eq!(c.scale, 1.0);
          assert_eq!(c.view_mode, ViewMode::Bars);
          assert_eq!(c.backdrop, Backdrop::Mica);
          assert_eq!(c.opacity, 1.0);
          assert_eq!(c.warn_threshold, 70.0);
          assert_eq!(c.critical_threshold, 90.0);
          assert_eq!(c.refresh_secs, 30);
          assert_eq!(c.quota_poll_secs, 180);
          assert_eq!(c.statusline_max_age_secs, 120);
      }

      #[test]
      fn load_from_missing_file_returns_defaults() {
          let p = temp_path("missing");
          assert!(!p.exists());
          let c = Config::load_from(&p);
          assert_eq!(c.view_mode, ViewMode::Bars);
          assert_eq!(c.scale, 1.0);
      }

      #[test]
      fn load_from_corrupt_file_returns_defaults() {
          let p = temp_path("corrupt");
          std::fs::create_dir_all(p.parent().unwrap()).unwrap();
          std::fs::write(&p, b"{ this is not valid json ").unwrap();
          let c = Config::load_from(&p);
          assert_eq!(c.warn_threshold, 70.0);
          assert_eq!(c.backdrop, Backdrop::Mica);
      }

      #[test]
      fn partial_json_merges_over_defaults() {
          let p = temp_path("partial");
          std::fs::create_dir_all(p.parent().unwrap()).unwrap();
          std::fs::write(&p, br#"{ "view_mode": "Gauge", "warn_threshold": 55.0 }"#).unwrap();
          let c = Config::load_from(&p);
          assert_eq!(c.view_mode, ViewMode::Gauge);
          assert_eq!(c.warn_threshold, 55.0);
          assert_eq!(c.critical_threshold, 90.0);
          assert_eq!(c.refresh_secs, 30);
      }

      #[test]
      fn save_then_load_roundtrips() {
          let p = temp_path("roundtrip");
          let mut c = Config::default();
          c.scale = 1.5;
          c.view_mode = ViewMode::Gauge;
          c.backdrop = Backdrop::Acrylic;
          c.position = Some((100.0, 200.0));
          c.critical_threshold = 95.0;

          c.save_to(&p).unwrap();
          assert!(p.exists());

          let loaded = Config::load_from(&p);
          assert_eq!(loaded.scale, 1.5);
          assert_eq!(loaded.view_mode, ViewMode::Gauge);
          assert_eq!(loaded.backdrop, Backdrop::Acrylic);
          assert_eq!(loaded.position, Some((100.0, 200.0)));
          assert_eq!(loaded.critical_threshold, 95.0);
      }

      #[test]
      fn save_creates_missing_parent_dirs() {
          let p = temp_path("nested-parent");
          assert!(!p.parent().unwrap().exists());
          Config::default().save_to(&p).unwrap();
          assert!(p.exists());
      }
  }
  ```

- [ ] **Step 3: Run the test, expect FAIL.** `cargo test --lib config::tests`. Expected: `defaults_are_the_canonical_values` passes; every test calling `load_from`/`save_to` panics with `not implemented`. Overall: FAILED.

- [ ] **Step 4: Minimal implementation.** Replace the three stub bodies:
  ```rust
  impl Config {
      pub fn path() -> PathBuf {
          let mut dir = dirs::config_dir().unwrap_or_else(std::env::temp_dir);
          dir.push("claude-usage");
          dir.push("widget-config.json");
          dir
      }

      pub fn load() -> Config {
          Self::load_from(&Self::path())
      }

      pub fn save(&self) -> Result<()> {
          self.save_to(&Self::path())
      }

      fn load_from(path: &Path) -> Config {
          match std::fs::read_to_string(path) {
              Ok(body) => serde_json::from_str(&body).unwrap_or_default(),
              Err(_) => Config::default(),
          }
      }

      fn save_to(&self, path: &Path) -> Result<()> {
          if let Some(parent) = path.parent() {
              std::fs::create_dir_all(parent)
                  .with_context(|| format!("creating config dir {}", parent.display()))?;
          }
          let body = serde_json::to_string_pretty(self).context("serializing config")?;
          std::fs::write(path, body)
              .with_context(|| format!("writing config to {}", path.display()))?;
          Ok(())
      }
  }
  ```

- [ ] **Step 5: Run the test, expect PASS.** `cargo test --lib config::tests` → `6 passed; 0 failed`.

- [ ] **Step 6: Commit.**
  ```
  git add src/config.rs src/main.rs
  git commit -m "feat(config): Config with serde(default) load/save/merge and forward-compat tests"
  ```

---

### Task M1-4: `sources/mod.rs` — `QuotaReading` type + module wiring

Before the OAuth client can return anything, the shared `QuotaReading` type must exist. This task adds the type, the `reconcile` signature as `todo!()` (M2 implements the body), and the submodule declarations. A trivial construction smoke test.

**Files:**
- Create: `C:\Users\oz\Desktop\claude-usage\src\sources\mod.rs`
- Create: `C:\Users\oz\Desktop\claude-usage\src\sources\oauth.rs` (placeholder)
- Modify: `C:\Users\oz\Desktop\claude-usage\src\main.rs` (add `mod sources;`)

- [ ] **Step 1: Declare the module.** Add `mod sources;` to `src\main.rs`.

- [ ] **Step 2: Write the failing test (full file).** Create `src\sources\mod.rs` with the canonical `QuotaReading`, the `reconcile` signature as `todo!()`, the submodule declarations, and a construction smoke test:
  ```rust
  use crate::model::{Provenance, Window};
  use std::time::{Duration, SystemTime};

  pub mod oauth;
  pub mod statusline;
  pub mod jsonl;

  #[derive(Clone, Debug)]
  pub struct QuotaReading {
      pub five_hour: Window,
      pub seven_day: Window,
      pub seven_day_opus: Option<Window>,
      pub source: Provenance, // StatusLine or OAuth (never Stale here)
      pub observed_at: SystemTime,
  }

  /// Prefer a fresh status-line reading, else oauth, else degrade last_good to Stale.
  /// NOTE: implemented in milestone M2; declared here so the crate compiles during M1.
  pub fn reconcile(
      _statusline: Option<QuotaReading>,
      _oauth: Option<QuotaReading>,
      _last_good: Option<QuotaReading>,
      _now: SystemTime,
      _statusline_max_age: Duration,
  ) -> Option<(QuotaReading, Provenance)> {
      todo!("M2 implements reconcile")
  }

  #[cfg(test)]
  mod tests {
      use super::*;

      #[test]
      fn quota_reading_constructs_and_clones() {
          let r = QuotaReading {
              five_hour: Window { used_pct: 72.0, resets_at: None },
              seven_day: Window { used_pct: 41.0, resets_at: None },
              seven_day_opus: None,
              source: Provenance::OAuth,
              observed_at: SystemTime::UNIX_EPOCH,
          };
          let r2 = r.clone();
          assert_eq!(r2.five_hour.used_pct, 72.0);
          assert_eq!(r2.source, Provenance::OAuth);
      }
  }
  ```
  > `oauth.rs`, `statusline.rs`, and `jsonl.rs` are declared here. To compile this task, create placeholder files for all three (single-line comments). M1-5 fills `oauth.rs`; M5 fills `statusline.rs`; M3 fills `jsonl.rs`.

- [ ] **Step 3: Run before placeholders exist, expect FAIL (compile error).** `cargo test --lib sources::tests::quota_reading_constructs_and_clones` → fails to compile (`file not found for module oauth`). That compile failure is the red state.

- [ ] **Step 4: Minimal implementation.** Create the three placeholder files:
  - `src\sources\oauth.rs`: `// placeholder — implemented in Task M1-5`
  - `src\sources\statusline.rs`: `// placeholder — implemented in milestone M5`
  - `src\sources\jsonl.rs`: `// placeholder — implemented in milestone M3`

- [ ] **Step 5: Run the test, expect PASS.** `cargo test --lib sources::tests::quota_reading_constructs_and_clones` → `1 passed; 0 failed`.

- [ ] **Step 6: Commit.**
  ```
  git add src/sources/mod.rs src/sources/oauth.rs src/sources/statusline.rs src/sources/jsonl.rs src/main.rs
  git commit -m "feat(sources): add QuotaReading type and reconcile signature (impl deferred to M2)"
  ```

---

### Task M1-5: `sources/oauth.rs` — `parse_usage_json` (pure, fixture-tested) + `fetch`

Turn the real OAuth `/api/oauth/usage` response body into a `QuotaReading`. `parse_usage_json` is pure and fully unit-tested against a fixture matching spec §3 / Appendix A: `five_hour`, `seven_day`, nullable `seven_day_opus`, `seven_day_sonnet`, each `{ utilization: 0–100, resets_at: ISO-8601 }`. `resets_at` is ISO-8601 here (NOT Unix seconds — that's the status-line form).

**Files:**
- Modify: `C:\Users\oz\Desktop\claude-usage\src\sources\oauth.rs` (replace placeholder)
- Create: `C:\Users\oz\Desktop\claude-usage\tests\fixtures\oauth_usage.json`

- [ ] **Step 1: Create the fixture file.** Create `tests\fixtures\oauth_usage.json`:
  ```json
  {
    "five_hour": { "utilization": 72.5, "resets_at": "2026-06-08T16:00:00Z" },
    "seven_day": { "utilization": 41.0, "resets_at": "2026-06-12T00:00:00Z" },
    "seven_day_opus": { "utilization": 18.25, "resets_at": "2026-06-12T00:00:00Z" },
    "seven_day_sonnet": { "utilization": 33.0, "resets_at": "2026-06-12T00:00:00Z" },
    "extra_usage": {
      "is_enabled": false,
      "monthly_limit": 0,
      "used_credits": 0,
      "utilization": 0
    }
  }
  ```

- [ ] **Step 2: Write the failing test (full file with stubs + tests).** Replace the placeholder `src\sources\oauth.rs` with deserialize structs, the `parse_usage_json` + `fetch` signatures (stubbed `unimplemented!()`), and the test module (`fetch` is NOT unit-tested — exercised manually in Step 7):
  ```rust
  use crate::model::{Provenance, Window};
  use crate::sources::QuotaReading;
  use crate::timeutil;
  use anyhow::{Context, Result};
  use serde::Deserialize;
  use std::time::SystemTime;

  const OAUTH_USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";

  #[derive(Deserialize)]
  struct RawWindow {
      utilization: f32,
      resets_at: Option<String>, // ISO-8601 in the OAuth response
  }

  #[derive(Deserialize)]
  struct RawUsage {
      five_hour: RawWindow,
      seven_day: RawWindow,
      #[serde(default)]
      seven_day_opus: Option<RawWindow>,
  }

  /// Pure: parse the OAuth usage JSON body into a QuotaReading. Unit-tested.
  pub fn parse_usage_json(_body: &str, _observed_at: SystemTime) -> Result<QuotaReading> {
      unimplemented!()
  }

  /// Live: GET /api/oauth/usage with the 3 mandatory headers, then parse.
  /// Not unit-tested (real network); exercised manually.
  pub fn fetch(_token: &str, _cc_version: &str) -> Result<QuotaReading> {
      unimplemented!()
  }

  #[cfg(test)]
  mod tests {
      use super::*;

      const FIXTURE: &str = include_str!("../../tests/fixtures/oauth_usage.json");

      #[test]
      fn parses_fixture_percentages() {
          let observed = SystemTime::UNIX_EPOCH;
          let r = parse_usage_json(FIXTURE, observed).unwrap();
          assert_eq!(r.five_hour.used_pct, 72.5);
          assert_eq!(r.seven_day.used_pct, 41.0);
          assert_eq!(r.source, Provenance::OAuth);
          assert_eq!(r.observed_at, observed);
      }

      #[test]
      fn parses_iso8601_resets_at_not_unix_seconds() {
          let r = parse_usage_json(FIXTURE, SystemTime::UNIX_EPOCH).unwrap();
          let expected = timeutil::from_unix_secs(1_780_416_000); // 2026-06-08T16:00:00Z
          assert_eq!(r.five_hour.resets_at, Some(expected));
      }

      #[test]
      fn parses_opus_split_when_present() {
          let r = parse_usage_json(FIXTURE, SystemTime::UNIX_EPOCH).unwrap();
          let opus = r.seven_day_opus.expect("fixture has opus split");
          assert_eq!(opus.used_pct, 18.25);
      }

      #[test]
      fn missing_opus_yields_none() {
          let body = r#"{
              "five_hour": { "utilization": 10.0, "resets_at": "2026-06-08T16:00:00Z" },
              "seven_day": { "utilization": 5.0, "resets_at": "2026-06-12T00:00:00Z" },
              "seven_day_opus": null,
              "seven_day_sonnet": { "utilization": 2.0, "resets_at": "2026-06-12T00:00:00Z" }
          }"#;
          let r = parse_usage_json(body, SystemTime::UNIX_EPOCH).unwrap();
          assert!(r.seven_day_opus.is_none());
      }

      #[test]
      fn null_resets_at_yields_none_window_reset() {
          let body = r#"{
              "five_hour": { "utilization": 10.0, "resets_at": null },
              "seven_day": { "utilization": 5.0, "resets_at": null }
          }"#;
          let r = parse_usage_json(body, SystemTime::UNIX_EPOCH).unwrap();
          assert_eq!(r.five_hour.resets_at, None);
          assert_eq!(r.seven_day.resets_at, None);
      }

      #[test]
      fn garbage_body_errors_not_panics() {
          assert!(parse_usage_json("not json at all", SystemTime::UNIX_EPOCH).is_err());
      }
  }
  ```

- [ ] **Step 3: Run the test, expect FAIL.** `cargo test --lib sources::oauth::tests`. Expected: the five parsing tests panic with `not implemented`; `garbage_body_errors_not_panics` also panics (stub never returns `Err`). Overall: FAILED.

- [ ] **Step 4: Minimal implementation.** Replace both stub bodies:
  ```rust
  fn to_window(raw: &RawWindow) -> Result<Window> {
      let resets_at = match raw.resets_at.as_deref() {
          Some(s) if !s.is_empty() => Some(timeutil::parse_iso8601(s)?),
          _ => None,
      };
      Ok(Window { used_pct: raw.utilization, resets_at })
  }

  pub fn parse_usage_json(body: &str, observed_at: SystemTime) -> Result<QuotaReading> {
      let raw: RawUsage =
          serde_json::from_str(body).context("parsing OAuth /usage JSON body")?;
      let five_hour = to_window(&raw.five_hour).context("five_hour window")?;
      let seven_day = to_window(&raw.seven_day).context("seven_day window")?;
      let seven_day_opus = match &raw.seven_day_opus {
          Some(w) => Some(to_window(w).context("seven_day_opus window")?),
          None => None,
      };
      Ok(QuotaReading {
          five_hour,
          seven_day,
          seven_day_opus,
          source: Provenance::OAuth,
          observed_at,
      })
  }

  pub fn fetch(token: &str, cc_version: &str) -> Result<QuotaReading> {
      let user_agent = format!("claude-code/{cc_version}");
      let resp = ureq::get(OAUTH_USAGE_URL)
          .set("Authorization", &format!("Bearer {token}"))
          .set("anthropic-beta", "oauth-2025-04-20")
          .set("User-Agent", &user_agent) // MANDATORY: omitting yields persistent HTTP 429
          .call()
          .context("GET /api/oauth/usage failed")?;
      let observed_at = SystemTime::now();
      let body = resp.into_string().context("reading OAuth /usage response body")?;
      parse_usage_json(&body, observed_at)
  }
  ```
  > `ureq` 2.x: `ureq::get(url).set(header, value).call()?` returns a `Response`; `.into_string()` reads the body. If the pinned `ureq` exposes a different builder method, adjust the chain — the three header names/values are fixed by the spec and must not change. Note: `cc_version` here is the bare version (e.g. `"2.1.16"`); `fetch` prepends `claude-code/`. The collector's `read_oauth_creds` (M5-4) must therefore pass the bare version, not a pre-formatted UA.

- [ ] **Step 5: Run the test, expect PASS.** `cargo test --lib sources::oauth::tests` → `6 passed; 0 failed`.

- [ ] **Step 6: Commit.**
  ```
  git add src/sources/oauth.rs tests/fixtures/oauth_usage.json
  git commit -m "feat(oauth): parse_usage_json (fixture-tested) + fetch with mandatory headers"
  ```

- [ ] **Step 7: Manual live smoke check (no automated test, observe only).** Confirm `fetch` works against the real endpoint using the on-disk token (Appendix A: `~/.claude/.credentials.json` → `claudeAiOauth.accessToken`). Add an `#[ignore]`d test at the bottom of `oauth.rs`'s test module:
  ```rust
  #[test]
  #[ignore = "live network; run manually with a valid token"]
  fn live_fetch_prints_snapshot() {
      let home = dirs::home_dir().unwrap();
      let cred = std::fs::read_to_string(home.join(".claude").join(".credentials.json")).unwrap();
      let v: serde_json::Value = serde_json::from_str(&cred).unwrap();
      let token = v["claudeAiOauth"]["accessToken"].as_str().unwrap();
      let r = super::fetch(token, "2.1.16").unwrap();
      println!("five_hour {:?}  seven_day {:?}", r.five_hour, r.seven_day);
  }
  ```
  Run explicitly: `cargo test --lib sources::oauth::tests::live_fetch_prints_snapshot -- --ignored --nocapture`. Observe: it prints two `Window` values whose `used_pct` are plausible 0–100 numbers and does NOT return HTTP 429 (a 429 means the `User-Agent` header was dropped). Leave the `#[ignore]` attribute in place so CI never hits the network. Commit it with the test module if you keep it:
  ```
  git add src/sources/oauth.rs
  git commit -m "test(oauth): add ignored live fetch smoke test"
  ```

---

### Exit criteria for M1

- [ ] `cargo test` passes green across `model`, `timeutil`, `config`, and `sources::oauth`; the single live `fetch` test stays `#[ignore]`d.
- [ ] `model.rs` exposes the canonical types and `Level::from_pct` returns `Ok`/`Warn`/`Critical` at the documented boundaries (70/90 inclusive lower bounds).
- [ ] `timeutil.rs` normalizes ISO-8601 (with `Z`, numeric offsets, fractional seconds), Unix seconds, and Unix milliseconds to `SystemTime`; `utc_day` rolls exactly at UTC midnight; `utc_day_start` returns the UTC-midnight instant.
- [ ] `config.rs` loads defaults on missing/corrupt files (never errors), merges partial JSON over defaults via `#[serde(default)]`, and round-trips; defaults match the contract exactly.
- [ ] `sources/mod.rs` defines `QuotaReading` and the `reconcile` signature (body deferred to M2); the crate compiles with all three submodule placeholders.
- [ ] `sources/oauth.rs::parse_usage_json` produces a correct `QuotaReading` from the fixture (ISO-8601 resets, nullable opus, garbage → `Err`), and `fetch` sends all three mandatory headers.
- [ ] A manual live `fetch` returns plausible 0–100 percentages with no HTTP 429.
- [ ] Each task is committed; `Cargo.lock` remains committed and untouched (no `cargo update`).

---

## Milestone M2: Bars live

**Goal:** Reconcile a quota reading per tick, drive it through the `Collector` into a `UsageSnapshot`, color it by `Level`, render it as a live bars view, and wire the background poller so the window updates on a timer.

**Verification kind: mixed** — `reconcile`, `Collector::tick`, and `Level → color` are TDD (test-first, pure logic with injected `now`/sources); `ui/bars.rs` + the poller wiring in `ui/mod.rs`/`main.rs` are manual-verify.

> Prerequisites M0 and M1 are complete and committed. M2 builds directly on those types.
>
> **Forward note:** M2 builds the first working `Collector` with a test seam. The collector's token half is `TokenStats::default()` here (placeholder); **M3-6** swaps in the real JSONL `Cursor`/`TokenLedger`, and **M5-4** finalizes the constructor/seam shape and the OAuth-skip logic. Each milestone rewrites `collector.rs` as a whole; the public `Collector::new(config) -> Self` and `tick(now) -> UsageSnapshot` signatures never change.

---

### Task M2-1: `reconcile()` in `sources/mod.rs` (TDD)

The freshness-based quota chooser from spec §3.1: prefer a fresh status-line reading, else the OAuth reading, else degrade `last_good` to `Stale`. Pure function, injected `now`.

> **Note:** M5-3 re-runs and extends these `reconcile` tests with boundary cases; this task delivers the working implementation. The body written here is final — M5-3 only adds tests around it.

**Files:**
- Modify: `C:\Users\oz\Desktop\claude-usage\src\sources\mod.rs`

- [ ] **Step 1: Write the failing test.** Append a `#[cfg(test)] mod m2_reconcile_tests` to `src\sources\mod.rs` covering the four branches: fresh status-line wins; stale status-line falls through to OAuth; both absent degrades last-good to `Stale`; everything absent yields `None`.
  ```rust
  #[cfg(test)]
  mod m2_reconcile_tests {
      use super::*;
      use crate::model::{Provenance, Window};
      use std::time::{Duration, SystemTime};

      fn reading(used: f32, source: Provenance, observed_at: SystemTime) -> QuotaReading {
          QuotaReading {
              five_hour: Window { used_pct: used, resets_at: None },
              seven_day: Window { used_pct: used / 2.0, resets_at: None },
              seven_day_opus: None,
              source,
              observed_at,
          }
      }

      fn now() -> SystemTime {
          SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000)
      }

      #[test]
      fn fresh_statusline_is_preferred_over_oauth() {
          let n = now();
          let sl = reading(72.0, Provenance::StatusLine, n - Duration::from_secs(30));
          let oa = reading(50.0, Provenance::OAuth, n - Duration::from_secs(5));
          let (chosen, prov) = reconcile(Some(sl), Some(oa), None, n, Duration::from_secs(120)).unwrap();
          assert_eq!(chosen.five_hour.used_pct, 72.0);
          assert_eq!(prov, Provenance::StatusLine);
      }

      #[test]
      fn stale_statusline_falls_through_to_oauth() {
          let n = now();
          let sl = reading(72.0, Provenance::StatusLine, n - Duration::from_secs(300));
          let oa = reading(50.0, Provenance::OAuth, n - Duration::from_secs(5));
          let (chosen, prov) = reconcile(Some(sl), Some(oa), None, n, Duration::from_secs(120)).unwrap();
          assert_eq!(chosen.five_hour.used_pct, 50.0);
          assert_eq!(prov, Provenance::OAuth);
      }

      #[test]
      fn no_fresh_sources_degrades_last_good_to_stale() {
          let n = now();
          let last = reading(63.0, Provenance::OAuth, n - Duration::from_secs(600));
          let (chosen, prov) = reconcile(None, None, Some(last.clone()), n, Duration::from_secs(120)).unwrap();
          assert_eq!(chosen.five_hour.used_pct, 63.0);
          match prov {
              Provenance::Stale { last_good_at } => assert_eq!(last_good_at, last.observed_at),
              other => panic!("expected Stale, got {other:?}"),
          }
      }

      #[test]
      fn nothing_available_returns_none() {
          let n = now();
          assert!(reconcile(None, None, None, n, Duration::from_secs(120)).is_none());
      }

      #[test]
      fn stale_statusline_and_no_oauth_degrades_to_stale() {
          let n = now();
          let sl = reading(72.0, Provenance::StatusLine, n - Duration::from_secs(300));
          let (chosen, prov) = reconcile(Some(sl.clone()), None, Some(sl.clone()), n, Duration::from_secs(120)).unwrap();
          assert_eq!(chosen.five_hour.used_pct, 72.0);
          match prov {
              Provenance::Stale { .. } => {}
              other => panic!("expected Stale, got {other:?}"),
          }
      }
  }
  ```

- [ ] **Step 2: Run the test, expect FAIL.** `cargo test --lib sources::m2_reconcile_tests`. Expected: panic `not yet implemented` from the `todo!()` left by M1-4.

- [ ] **Step 3: Minimal implementation.** Replace the `reconcile` body in `src\sources\mod.rs` (keep the `QuotaReading` struct and `pub mod` lines from M1-4):
  ```rust
  pub fn reconcile(
      statusline: Option<QuotaReading>,
      oauth: Option<QuotaReading>,
      last_good: Option<QuotaReading>,
      now: SystemTime,
      statusline_max_age: Duration,
  ) -> Option<(QuotaReading, Provenance)> {
      // 1) Fresh status-line reading wins outright.
      if let Some(sl) = statusline {
          let age = now.duration_since(sl.observed_at).unwrap_or(Duration::ZERO);
          if age <= statusline_max_age {
              let prov = sl.source.clone();
              return Some((sl, prov));
          }
      }
      // 2) Otherwise the OAuth reading.
      if let Some(oa) = oauth {
          let prov = oa.source.clone();
          return Some((oa, prov));
      }
      // 3) Otherwise degrade the last-good reading to Stale.
      if let Some(last) = last_good {
          let prov = Provenance::Stale { last_good_at: last.observed_at };
          return Some((last, prov));
      }
      None
  }
  ```
  > Add `use std::time::Duration;` to the existing `std::time` import line at the top of `mod.rs` if not already present.

- [ ] **Step 4: Run the test, expect PASS.** `cargo test --lib sources::m2_reconcile_tests` → `5 passed; 0 failed`.

- [ ] **Step 5: Commit.**
  ```
  git add src/sources/mod.rs
  git commit -m "feat(sources): implement reconcile() freshness-based quota selection"
  ```

---

### Task M2-2: `ui/theme.rs` — `Level → color` (TDD)

Map `Level` to a gpui `Hsla` and provide a legibility-scrim helper. Pure and deterministic.

> **Note:** M2 establishes `level_color`; M6-2 adds `scrim()` / `on_scrim_text()` for the legibility card. To avoid a name clash, M2 names its early scrim helper `scrim_color()`; M6-2 introduces `scrim()` (the card fill) and `on_scrim_text()` separately. Both can coexist, or M6-2 may consolidate — M6-2's note covers this.

**Files:**
- Create: `C:\Users\oz\Desktop\claude-usage\src\ui\theme.rs`
- Create: `C:\Users\oz\Desktop\claude-usage\src\ui\mod.rs` (add `pub mod theme;`)
- Modify: `C:\Users\oz\Desktop\claude-usage\src\main.rs` (add `mod ui;`)

- [ ] **Step 1: Declare the `ui` module.** Add `mod ui;` to `src\main.rs`. Create `src\ui\mod.rs` with `pub mod theme;` (more `pub mod` lines added in M2-4 and M4).

- [ ] **Step 2: Write the failing test.** Create `src\ui\theme.rs` with production functions as `unimplemented!()` and the test:
  ```rust
  use crate::model::Level;
  use gpui::{hsla, Hsla};

  /// Map a usage Level to its widget color: Ok=green, Warn=amber, Critical=red.
  pub fn level_color(_level: Level) -> Hsla {
      unimplemented!()
  }

  /// A faint translucent scrim color drawn behind numbers for legibility on
  /// frosted backdrops (spec §7.3). Near-black with low alpha.
  pub fn scrim_color() -> Hsla {
      unimplemented!()
  }

  #[cfg(test)]
  mod tests {
      use super::*;

      #[test]
      fn ok_is_green_hue() {
          let c = level_color(Level::Ok);
          assert!((c.h - 0.33).abs() < 0.06, "hue was {}", c.h);
          assert!(c.s > 0.4 && c.l > 0.2);
      }

      #[test]
      fn warn_is_amber_hue() {
          let c = level_color(Level::Warn);
          assert!((c.h - 0.11).abs() < 0.05, "hue was {}", c.h);
      }

      #[test]
      fn critical_is_red_hue() {
          let c = level_color(Level::Critical);
          assert!(c.h < 0.04 || c.h > 0.96, "hue was {}", c.h);
          assert!(c.s > 0.5);
      }

      #[test]
      fn scrim_is_dark_and_translucent() {
          let s = scrim_color();
          assert!(s.l < 0.2, "scrim should be dark");
          assert!(s.a > 0.0 && s.a < 0.6, "scrim should be translucent");
      }
  }
  ```
  > The `Hsla` fields (`h`, `s`, `l`, `a`) and the `hsla(h, s, l, a)` constructor are gpui's public API; confirm against the pinned gpui rev from M0 if compilation complains.

- [ ] **Step 3: Run the test, expect FAIL.** `cargo test --lib ui::theme::tests` → 4 tests panic with `not implemented`.

- [ ] **Step 4: Minimal implementation.** Replace the two bodies:
  ```rust
  pub fn level_color(level: Level) -> Hsla {
      match level {
          Level::Ok => hsla(0.33, 0.62, 0.45, 1.0),       // green
          Level::Warn => hsla(0.11, 0.85, 0.52, 1.0),     // amber
          Level::Critical => hsla(0.01, 0.80, 0.55, 1.0), // red
      }
  }

  pub fn scrim_color() -> Hsla {
      hsla(0.0, 0.0, 0.05, 0.35)
  }
  ```

- [ ] **Step 5: Run the test, expect PASS.** `cargo test --lib ui::theme::tests` → `4 passed; 0 failed`.

- [ ] **Step 6: Commit.**
  ```
  git add src/ui/theme.rs src/ui/mod.rs src/main.rs
  git commit -m "feat(ui): map Level to color and add legibility scrim helper"
  ```

---

### Task M2-3: `Collector::tick()` in `collector.rs` (TDD)

The orchestrator: read the status-line cache, conditionally poll OAuth (respecting `quota_poll_secs` and skipping when status-line is fresh), `reconcile`, assemble a `UsageSnapshot`. Built with **injected source closures** so `tick` is testable without files or network; production `Collector::new` wires the real `sources::*` functions.

> **Note:** this is the M2 baseline. The token half returns `TokenStats::default()` (M3-6 replaces it); the constructor seam here is `new_with_sources` (M5-4 renames it to `with_sources` and drops the `now` parameter on the statusline/oauth closures). Public `new`/`tick` are stable.

**Files:**
- Create: `C:\Users\oz\Desktop\claude-usage\src\collector.rs`
- Modify: `C:\Users\oz\Desktop\claude-usage\src\main.rs` (add `mod collector;`)

- [ ] **Step 1: Declare the module + write the failing test.** Add `mod collector;` to `src\main.rs`. Create `src\collector.rs` and add this test module (the impl comes in Step 3). It injects fakes so no I/O happens, asserting: (a) fresh status-line is used and OAuth is *not* polled; (b) OAuth is polled and throttled by the floor; (c) both-absent degrades to `Stale`.
  ```rust
  #[cfg(test)]
  mod tests {
      use super::*;
      use crate::config::Config;
      use crate::model::{Provenance, Window};
      use crate::sources::QuotaReading;
      use std::cell::Cell;
      use std::rc::Rc;
      use std::time::{Duration, SystemTime};

      fn t0() -> SystemTime {
          SystemTime::UNIX_EPOCH + Duration::from_secs(2_000_000)
      }

      fn reading(used: f32, source: Provenance, observed_at: SystemTime) -> QuotaReading {
          QuotaReading {
              five_hour: Window { used_pct: used, resets_at: None },
              seven_day: Window { used_pct: used / 2.0, resets_at: None },
              seven_day_opus: None,
              source,
              observed_at,
          }
      }

      #[test]
      fn fresh_statusline_skips_oauth_poll() {
          let now = t0();
          let oauth_calls = Rc::new(Cell::new(0u32));
          let oc = oauth_calls.clone();
          let sl = reading(72.0, Provenance::StatusLine, now - Duration::from_secs(10));
          let mut c = Collector::new_with_sources(
              Config::default(),
              Box::new(move |_now| Some(sl.clone())),
              Box::new(move |now2| { oc.set(oc.get() + 1); Some(reading(50.0, Provenance::OAuth, now2)) }),
              Box::new(|_now| crate::model::TokenStats::default()),
          );
          let snap = c.tick(now);
          assert_eq!(snap.five_hour.used_pct, 72.0);
          assert_eq!(snap.source, Provenance::StatusLine);
          assert_eq!(oauth_calls.get(), 0, "fresh status-line must not poll OAuth");
      }

      #[test]
      fn oauth_poll_is_throttled_by_quota_poll_secs() {
          let now = t0();
          let oauth_calls = Rc::new(Cell::new(0u32));
          let oc = oauth_calls.clone();
          let mut cfg = Config::default();
          cfg.quota_poll_secs = 180;
          let mut c = Collector::new_with_sources(
              cfg,
              Box::new(|_now| None),
              Box::new(move |now2| { oc.set(oc.get() + 1); Some(reading(40.0, Provenance::OAuth, now2)) }),
              Box::new(|_now| crate::model::TokenStats::default()),
          );
          let s1 = c.tick(now);
          assert_eq!(s1.source, Provenance::OAuth);
          assert_eq!(oauth_calls.get(), 1);
          let s2 = c.tick(now + Duration::from_secs(60));
          assert_eq!(oauth_calls.get(), 1, "must not re-poll before quota_poll_secs");
          assert_eq!(s2.five_hour.used_pct, 40.0);
          let _s3 = c.tick(now + Duration::from_secs(200));
          assert_eq!(oauth_calls.get(), 2, "must re-poll after quota_poll_secs elapsed");
      }

      #[test]
      fn both_sources_absent_degrades_to_stale_after_a_good_reading() {
          let now = t0();
          let phase = Rc::new(Cell::new(0u32));
          let p = phase.clone();
          let mut c = Collector::new_with_sources(
              Config::default(),
              Box::new(|_now| None),
              Box::new(move |now2| if p.get() == 0 { Some(reading(55.0, Provenance::OAuth, now2)) } else { None }),
              Box::new(|_now| crate::model::TokenStats::default()),
          );
          let s1 = c.tick(now);
          assert_eq!(s1.source, Provenance::OAuth);
          phase.set(1);
          let s2 = c.tick(now + Duration::from_secs(300));
          assert!(
              matches!(s2.source, crate::model::Provenance::Stale { .. }),
              "expected Stale, got {:?}", s2.source
          );
          assert_eq!(s2.five_hour.used_pct, 55.0, "stale snapshot keeps last-good data");
      }
  }
  ```

- [ ] **Step 2: Run the test, expect FAIL.** `cargo test --lib collector::tests`. Expected: compile error — `Collector::new_with_sources` does not exist.

- [ ] **Step 3: Minimal implementation.** Write the non-test contents of `src\collector.rs`:
  ```rust
  use crate::config::Config;
  use crate::model::{TokenStats, UsageSnapshot};
  use crate::sources::{self, QuotaReading};
  use std::time::{Duration, SystemTime};

  type StatuslineFn = Box<dyn FnMut(SystemTime) -> Option<QuotaReading>>;
  type OauthFn = Box<dyn FnMut(SystemTime) -> Option<QuotaReading>>;
  type TokensFn = Box<dyn FnMut(SystemTime) -> TokenStats>;

  pub struct Collector {
      config: Config,
      last_good: Option<QuotaReading>,
      last_oauth_at: Option<SystemTime>,
      read_statusline: StatuslineFn,
      poll_oauth: OauthFn,
      read_tokens: TokensFn,
  }

  impl Collector {
      pub fn new(config: Config) -> Self {
          let read_statusline: StatuslineFn = Box::new(move |_now| {
              sources::statusline::read_cache(&sources::statusline::cache_path())
          });
          let poll_oauth: OauthFn = Box::new(move |_now| {
              let token = read_oauth_token().ok()?;
              let version = claude_code_version();
              sources::oauth::fetch(&token, &version).ok()
          });
          let read_tokens: TokensFn = Box::new(|_now| TokenStats::default());
          Self::new_with_sources(config, read_statusline, poll_oauth, read_tokens)
      }

      pub fn new_with_sources(
          config: Config,
          read_statusline: StatuslineFn,
          poll_oauth: OauthFn,
          read_tokens: TokensFn,
      ) -> Self {
          Self { config, last_good: None, last_oauth_at: None, read_statusline, poll_oauth, read_tokens }
      }

      pub fn tick(&mut self, now: SystemTime) -> UsageSnapshot {
          let tokens = (self.read_tokens)(now);

          let statusline = (self.read_statusline)(now);
          let max_age = Duration::from_secs(self.config.statusline_max_age_secs);
          let statusline_fresh = statusline
              .as_ref()
              .map(|r| now.duration_since(r.observed_at).unwrap_or(Duration::ZERO) <= max_age)
              .unwrap_or(false);

          let floor = Duration::from_secs(self.config.quota_poll_secs);
          let floor_elapsed = match self.last_oauth_at {
              None => true,
              Some(prev) => now.duration_since(prev).unwrap_or(Duration::ZERO) >= floor,
          };
          let oauth = if !statusline_fresh && floor_elapsed {
              let r = (self.poll_oauth)(now);
              self.last_oauth_at = Some(now);
              r
          } else {
              None
          };

          let chosen = sources::reconcile(statusline, oauth, self.last_good.clone(), now, max_age);

          match chosen {
              Some((reading, provenance)) => {
                  if !matches!(provenance, crate::model::Provenance::Stale { .. }) {
                      self.last_good = Some(reading.clone());
                  }
                  UsageSnapshot {
                      five_hour: reading.five_hour,
                      seven_day: reading.seven_day,
                      seven_day_opus: reading.seven_day_opus,
                      tokens,
                      source: provenance,
                      fetched_at: now,
                  }
              }
              None => UsageSnapshot {
                  five_hour: crate::model::Window { used_pct: 0.0, resets_at: None },
                  seven_day: crate::model::Window { used_pct: 0.0, resets_at: None },
                  seven_day_opus: None,
                  tokens,
                  source: crate::model::Provenance::Stale { last_good_at: now },
                  fetched_at: now,
              },
          }
      }
  }

  /// Read the OAuth access token from ~/.claude/.credentials.json.
  fn read_oauth_token() -> anyhow::Result<String> {
      use anyhow::Context;
      let home = dirs::home_dir().context("no home dir")?;
      let path = home.join(".claude").join(".credentials.json");
      let body = std::fs::read_to_string(&path)
          .with_context(|| format!("reading {}", path.display()))?;
      let v: serde_json::Value = serde_json::from_str(&body).context("parsing .credentials.json")?;
      let token = v.get("claudeAiOauth").and_then(|o| o.get("accessToken")).and_then(|t| t.as_str())
          .context("claudeAiOauth.accessToken missing")?;
      Ok(token.to_string())
  }

  /// Bare Claude Code version for the User-Agent (fetch() prepends "claude-code/").
  fn claude_code_version() -> String {
      "2.1.16".to_string()
  }
  ```
  > Depends on M1's `sources::oauth::fetch(token, cc_version)` and the M1-4 placeholder `sources::statusline::{read_cache, cache_path}`. Those statusline fns are real in M5; until then the placeholder file makes them resolve. For this task's tests (which inject their own closures), the production wiring only needs to *compile*. **If `statusline::read_cache`/`cache_path` are still bare placeholder comments at this point, add minimal stub signatures** (`pub fn cache_path() -> std::path::PathBuf { dirs::home_dir().unwrap_or_default().join(".claude/widget-cache/ratelimits.json") }` and `pub fn read_cache(_p: &std::path::Path) -> Option<crate::sources::QuotaReading> { None }`) to `src\sources\statusline.rs` so `new()` compiles; M5-1 replaces them.

- [ ] **Step 4: Run the test, expect PASS.** `cargo test --lib collector::tests` → `3 passed; 0 failed`.

- [ ] **Step 5: Commit.**
  ```
  git add src/collector.rs src/sources/statusline.rs src/main.rs
  git commit -m "feat(collector): tick() reconciles sources into UsageSnapshot with poll throttle"
  ```

---

### Task M2-4: `ui/bars.rs` — render the bars view (manual-verify)

Render a `UsageSnapshot` as the bars layout (spec §7.1): header with provenance dot, a primary 5H `ProgressBar` with %, a reset countdown, a smaller 7D bar, a footer with today's tokens + dominant model. Colored via `theme::level_color`. GUI code — verified by running.

**Files:**
- Modify: `C:\Users\oz\Desktop\claude-usage\src\timeutil.rs` (add `format_countdown`)
- Create: `C:\Users\oz\Desktop\claude-usage\src\ui\bars.rs`
- Modify: `C:\Users\oz\Desktop\claude-usage\src\ui\mod.rs` (declare `pub mod bars;`)

- [ ] **Step 1: Add a reset-countdown formatter to `timeutil.rs` (TDD).** Write the failing test first — append to `src\timeutil.rs`:
  ```rust
  #[cfg(test)]
  mod bars_fmt_tests {
      use super::*;
      use std::time::{Duration, SystemTime};

      #[test]
      fn formats_hours_and_minutes() {
          let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
          let resets = now + Duration::from_secs(2 * 3600 + 13 * 60);
          assert_eq!(format_countdown(Some(resets), now), "2h13m");
      }

      #[test]
      fn formats_minutes_only_under_one_hour() {
          let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
          let resets = now + Duration::from_secs(47 * 60);
          assert_eq!(format_countdown(Some(resets), now), "47m");
      }

      #[test]
      fn past_or_none_reads_now() {
          let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
          assert_eq!(format_countdown(None, now), "—");
          assert_eq!(format_countdown(Some(now - Duration::from_secs(5)), now), "now");
      }
  }
  ```
  Run `cargo test --lib timeutil::bars_fmt_tests` → FAIL (`cannot find function format_countdown`). Then implement (append above the test module):
  ```rust
  /// Human reset countdown like "2h13m" / "47m" / "now" / "—".
  pub fn format_countdown(resets_at: Option<std::time::SystemTime>, now: std::time::SystemTime) -> String {
      let Some(resets) = resets_at else { return "—".to_string() };
      let remaining = match resets.duration_since(now) {
          Ok(d) => d,
          Err(_) => return "now".to_string(),
      };
      let secs = remaining.as_secs();
      if secs == 0 { return "now".to_string(); }
      let hours = secs / 3600;
      let mins = (secs % 3600) / 60;
      if hours > 0 { format!("{hours}h{mins:02}m") } else { format!("{mins}m") }
  }
  ```
  Run again → PASS (`3 passed`). Note `{mins:02}` yields `2h13m` for 13 and `2h03m` for 3.

- [ ] **Step 2: Write `ui/bars.rs`.** Create `src\ui\bars.rs`. Confirm the `ProgressBar` builder and `div`/`px`/style imports against the M0 example (the two marked lines).
  ```rust
  use crate::model::{Level, Provenance, UsageSnapshot, Window};
  use crate::timeutil::format_countdown;
  use crate::ui::theme::{level_color, scrim_color};
  use gpui::{div, px, App, Hsla, IntoElement, ParentElement, Styled, Window as GpuiWindow};
  use gpui_component::progress::ProgressBar;

  pub fn render_bars(
      snapshot: &UsageSnapshot,
      now: std::time::SystemTime,
      warn: f32,
      critical: f32,
      _window: &mut GpuiWindow,
      _cx: &mut App,
  ) -> impl IntoElement {
      let header = format!("Claude · Max 5×   {}", provenance_label(&snapshot.source));
      let five = bar_row("5H", &snapshot.five_hour, now, warn, critical, true);
      let seven = bar_row("7D", &snapshot.seven_day, now, warn, critical, false);
      let footer = footer_line(snapshot);

      div()
          .flex()
          .flex_col()
          .gap_1()
          .p_3()
          .rounded(px(10.0))
          .bg(scrim_color())
          .text_color(gpui::white())
          .child(div().text_sm().child(header))
          .child(five)
          .child(seven)
          .child(div().text_xs().opacity(0.8).child(footer))
  }

  fn bar_row(
      label: &str,
      window: &Window,
      now: std::time::SystemTime,
      warn: f32,
      critical: f32,
      primary: bool,
  ) -> impl IntoElement {
      let level = Level::from_pct(window.used_pct, warn, critical);
      let color: Hsla = level_color(level);
      let pct = window.used_pct;
      let reset = format_countdown(window.resets_at, now);
      let bar_width = if primary { px(180.0) } else { px(150.0) };

      div()
          .flex()
          .flex_col()
          .gap_0p5()
          .child(
              div()
                  .flex()
                  .flex_row()
                  .items_center()
                  .gap_2()
                  .child(div().w(px(22.0)).text_sm().child(label.to_string()))
                  // CONFIRM against M0 example: ProgressBar value is 0..=100.
                  .child(ProgressBar::new().value(pct).color(color).w(bar_width))
                  .child(div().text_sm().child(format!("{pct:.0}%"))),
          )
          .child(div().text_xs().opacity(0.75).child(format!("resets {reset}")))
  }

  fn footer_line(snapshot: &UsageSnapshot) -> String {
      let tokens = &snapshot.tokens;
      let total = humanize(tokens.today_total_output);
      match tokens.by_model.first() {
          Some((model, _)) => format!("today {total} · {model}"),
          None => format!("today {total}"),
      }
  }

  fn provenance_label(p: &Provenance) -> &'static str {
      match p {
          Provenance::StatusLine => "·live",
          Provenance::OAuth => "·oauth",
          Provenance::Stale { .. } => "·stale",
      }
  }

  fn humanize(n: u64) -> String {
      if n >= 1_000_000 { format!("{:.2}M", n as f64 / 1_000_000.0) }
      else if n >= 1_000 { format!("{:.1}K", n as f64 / 1_000.0) }
      else { n.to_string() }
  }
  ```
  Declare the module in `src\ui\mod.rs`: add `pub mod bars;`.
  > The two "CONFIRM against M0" surfaces: the `ProgressBar::new().value(..).color(..).w(..)` builder, and the `div()`/`px`/style-method imports. Reuse the exact import paths and builder calls that compiled in M0. If the crate exposes `ProgressBar` at a different path (e.g. `gpui_component::ProgressBar`), update the `use` line.

- [ ] **Step 3: Compile-check the new UI code.** `cargo build`. Expected: clean build. Fix any import-path mismatches against the M0 example now (the most likely failure point).

- [ ] **Step 4: Commit.**
  ```
  git add src/ui/bars.rs src/ui/mod.rs src/timeutil.rs
  git commit -m "feat(ui): render bars view from UsageSnapshot with level colors and countdowns"
  ```

---

### Task M2-5: Wire the poller into `ui/mod.rs` + `main.rs` (manual-verify)

Make the `Root` view hold the latest `UsageSnapshot`, spawn a background loop that calls `Collector::tick` every `refresh_secs` and pushes new snapshots into the view, and have `main.rs` open the window with this live `Root`. Replace M0's hardcoded `ProgressBar` with `render_bars`.

**Files:**
- Modify: `C:\Users\oz\Desktop\claude-usage\src\ui\mod.rs`
- Modify: `C:\Users\oz\Desktop\claude-usage\src\main.rs`
- Create: `C:\Users\oz\Desktop\claude-usage\src\statusline_cmd.rs` (stub; real body in M5)

- [ ] **Step 1: Give the `Root` view a snapshot + config + a render that calls `render_bars`.** Edit `src\ui\mod.rs` so the root entity stores the snapshot and config and renders bars (gauge wiring lands in M4):
  ```rust
  pub mod bars;
  pub mod theme;
  // gauge declared in M4.

  use crate::config::Config;
  use crate::model::{Provenance, TokenStats, UsageSnapshot, Window};
  use gpui::{div, Context, IntoElement, ParentElement, Render, Window as GpuiWindow};
  use std::time::SystemTime;

  pub struct Root {
      pub snapshot: UsageSnapshot,
      pub config: Config,
  }

  impl Root {
      pub fn new(config: Config) -> Self {
          Self { snapshot: placeholder_snapshot(), config }
      }

      /// Called from the background poller via cx.update on the UI thread.
      pub fn set_snapshot(&mut self, snapshot: UsageSnapshot, cx: &mut Context<Self>) {
          self.snapshot = snapshot;
          cx.notify();
      }
  }

  impl Render for Root {
      fn render(&mut self, window: &mut GpuiWindow, cx: &mut Context<Self>) -> impl IntoElement {
          let now = SystemTime::now();
          // ViewMode::Gauge falls back to bars until M4 lands gauge.rs.
          div().child(bars::render_bars(
              &self.snapshot,
              now,
              self.config.warn_threshold,
              self.config.critical_threshold,
              window,
              cx.app_mut(),
          ))
      }
  }

  fn placeholder_snapshot() -> UsageSnapshot {
      let now = SystemTime::now();
      UsageSnapshot {
          five_hour: Window { used_pct: 0.0, resets_at: None },
          seven_day: Window { used_pct: 0.0, resets_at: None },
          seven_day_opus: None,
          tokens: TokenStats::default(),
          source: Provenance::Stale { last_good_at: now },
          fetched_at: now,
      }
  }
  ```
  > `cx.app_mut()` reaches `&mut App` from `Context<Self>` for `render_bars`; if the pinned gpui rev names it differently, match the M0 example's `Render` impl accessor. Same for `cx.notify()`.

- [ ] **Step 2: Open the window and spawn the poller in `main.rs`.** Edit `src\main.rs` so the GUI branch, after `gpui_component::init`, creates the `Collector`, opens the window with a `Root` entity wrapped in `gpui_component::Root::new`, applies Mica + topmost (from M0), and starts a detached loop. Keep the M0 window-open calls verbatim; only the collector/poller body is new.
  ```rust
  mod collector;
  mod config;
  mod model;
  mod sources;
  mod statusline_cmd; // body lands in M5
  mod timeutil;
  mod ui;
  mod win;

  use std::time::{Duration, SystemTime};

  fn main() {
      let args: Vec<String> = std::env::args().collect();
      if args.iter().any(|a| a == "--statusline") {
          statusline_cmd::run_statusline_stub();
          return;
      }
      run_gui();
  }

  fn run_gui() {
      let config = config::Config::load();
      gpui_platform::application().run(move |cx| {
          gpui_component::init(cx);

          // Open the borderless Mica window (M0 established these options).
          // CONFIRM open_window/Root::new/backdrop/topmost calls against the M0 code.
          let window = open_widget_window(cx, &config);

          let root_view = cx.new(|_cx| ui::Root::new(config.clone()));
          // gpui_component::Root::new(view, window, cx) — mandatory wrapper (spec §8).

          win::backdrop::apply_from_config(&window, &config, cx);
          win::topmost::apply_topmost_from_window(&window, cx);

          let refresh = Duration::from_secs(config.refresh_secs.max(1));
          let collector = collector::Collector::new(config.clone());
          spawn_poller(cx, root_view, collector, refresh);
      });
  }

  /// Detached loop: tick the collector every `refresh`, push snapshot into the view.
  fn spawn_poller(
      cx: &mut gpui::App,
      root: gpui::Entity<ui::Root>,
      mut collector: collector::Collector,
      refresh: Duration,
  ) {
      cx.spawn(move |mut acx| async move {
          loop {
              let now = SystemTime::now();
              let snapshot = collector.tick(now);
              let _ = root.update(&mut acx, |root, cx| {
                  root.set_snapshot(snapshot, cx);
              });
              acx.background_executor().timer(refresh).await;
          }
      })
      .detach();
  }
  ```
  Create the stub `src\statusline_cmd.rs` so the binary links before M5:
  ```rust
  /// Placeholder until M5 implements the real status-line helper.
  pub fn run_statusline_stub() {
      use std::io::Read;
      let mut buf = String::new();
      let _ = std::io::stdin().read_to_string(&mut buf);
      println!("claude-usage: statusline not yet wired");
  }
  ```
  > Calls marked "CONFIRM against M0": `open_widget_window`, the `gpui_component::Root::new(view, window, cx)` wrap, `win::backdrop::apply_from_config`, `win::topmost::apply_topmost_from_window`, and the `cx.spawn`/`background_executor().timer` shape. M0 already ran the window with Mica + topmost + a hardcoded `ProgressBar`; copy those exact calls and rename the illustrative wrappers above to match M0's actual functions rather than inventing new ones. `win::topmost::apply_topmost(hwnd)` from M0 is the real entry — wrap it in whatever helper extracts the HWND from the live window. (M0's `topmost.rs` already has `apply_topmost`; `apply_topmost_from_window` is a thin wrapper you add here or inline the HWND extraction.)

- [ ] **Step 3: Build.** `cargo build`. Expected: clean build. Most likely fixes are gpui API names (`cx.spawn`, `cx.new`, `background_executor`, the window-open call) — reconcile each against the M0 example.

- [ ] **Step 4: Run + observe (the M2 acceptance check).** `cargo run`. Look for ALL of:
  - A borderless, frosted (Mica) window stays **above** other windows (topmost from M0).
  - A **5H** bar and a **7D** bar with a real percentage label, not the hardcoded M0 value.
  - Bar color matches level: **green** <70%, **amber** 70–89%, **red** ≥90% (compare against your real account state).
  - A `resets …` countdown under each bar (e.g. `resets 2h13m`), or `—` if no reset time.
  - A provenance tag: `·oauth` when it polled, or `·stale` if both sources failed (kill network to force `·stale` and confirm the bars **do not blank or crash** — they keep last-good values).
  - Leave it running ~3+ minutes: it **re-polls** (percentage/countdown update) without the UI freezing.

  If OAuth returns 429, re-check the mandatory `User-Agent: claude-code/<version>` header from M1's `oauth::fetch`.

- [ ] **Step 5: Commit.**
  ```
  git add src/main.rs src/ui/mod.rs src/statusline_cmd.rs
  git commit -m "feat(ui): wire background poller into live bars view with topmost Mica window"
  ```

---

### Exit criteria for M2

- [ ] `cargo test --lib` passes, including `sources::m2_reconcile_tests`, `collector::tests`, `ui::theme::tests`, and `timeutil::bars_fmt_tests`.
- [ ] `reconcile()` prefers fresh status-line, falls through to OAuth when stale, degrades last-good to `Stale` when both absent — proven by tests.
- [ ] `Collector::tick()` builds a `UsageSnapshot`, does **not** poll OAuth while a fresh status-line reading exists, and respects the `quota_poll_secs` floor — proven by injected-source tests.
- [ ] `cargo run` shows a borderless, always-on-top, Mica window rendering **live** 5H + 7D bars from a real `UsageSnapshot`, color-coded by `Level`, with reset countdowns and a provenance tag.
- [ ] Killing the network degrades the widget to a dim `·stale` state showing last-good values **without blanking or crashing**; restoring it lets the next poll recover.
- [ ] The window keeps updating over a multi-minute run and the UI never blocks.
- [ ] All tasks committed with conventional-commit messages.

---

## Milestone M3: JSONL detail

**Goal:** Implement `sources/jsonl.rs` end-to-end — `parse_line`, `TokenLedger` (dedup on `(message_id, request_id)`, UTC-day bucket, `live_tok_per_min` from records < 90 s old), and `Cursor::update` (glob `~/.claude/projects/*/*.jsonl`, exclude `subagents`, incremental append-only read with rotation handling) — so the collector can fold local token detail into the `UsageSnapshot`.

**Verification kind: TDD** — strict test-first cycle for every behavior. Tests live inline; JSONL fixtures live under `tests/fixtures/`.

> Every symbol comes from the JSONL contract in the Shared Contracts: `AssistantRecord`, `parse_line`, `TokenLedger::{new, ingest, stats}`, `Cursor::{new, update}`, producing `crate::model::TokenStats`. `timestamp` is parsed via `timeutil::parse_iso8601` and the UTC-day bucket uses `timeutil::utc_day_start` (both from M1). Headline is **output-token-weighted**; daily boundary is **UTC midnight**.

---

### Task M3-1 — `parse_line`: one JSONL line → `Option<AssistantRecord>`

**Files:**
- Modify: `C:\Users\oz\Desktop\claude-usage\src\sources\jsonl.rs` (replace the M1-4 placeholder)
- (Module already declared `pub mod jsonl;` in `sources/mod.rs` from M1-4.)

- [ ] **Step 1: Write the failing test.** Replace the placeholder `src\sources\jsonl.rs` with ONLY the type, a stub, and the tests (stub returns `unimplemented!()`). Covers: valid assistant line, non-assistant line (`Ok(None)`), missing-`usage` line, malformed JSON (`Err`), and the `project` passthrough.
  ```rust
  use std::time::SystemTime;

  #[derive(Clone, Debug, PartialEq)]
  pub struct AssistantRecord {
      pub message_id: String,
      pub request_id: String,
      pub model: String,
      pub output_tokens: u64,
      pub timestamp: SystemTime,
      pub project: String, // parent dir name of the jsonl file
  }

  /// Parse one JSONL line; Ok(None) if not an assistant usage line. Pure, unit-tested.
  pub fn parse_line(_line: &str, _project: &str) -> anyhow::Result<Option<AssistantRecord>> {
      unimplemented!()
  }

  #[cfg(test)]
  mod tests {
      use super::*;
      use crate::timeutil;

      const ASSISTANT_LINE: &str = r#"{"type":"assistant","timestamp":"2026-06-08T10:15:30.000Z","requestId":"req_abc","message":{"id":"msg_001","model":"claude-opus-4-8","usage":{"input_tokens":1200,"output_tokens":850,"cache_read_input_tokens":40000,"cache_creation_input_tokens":2000}}}"#;

      #[test]
      fn parses_valid_assistant_line() {
          let rec = parse_line(ASSISTANT_LINE, "my-project").expect("parse ok").expect("is a record");
          assert_eq!(rec.message_id, "msg_001");
          assert_eq!(rec.request_id, "req_abc");
          assert_eq!(rec.model, "claude-opus-4-8");
          assert_eq!(rec.output_tokens, 850);
          assert_eq!(rec.project, "my-project");
          let expected = timeutil::parse_iso8601("2026-06-08T10:15:30.000Z").expect("iso parses");
          assert_eq!(rec.timestamp, expected);
      }

      #[test]
      fn ignores_non_assistant_line() {
          let user_line = r#"{"type":"user","timestamp":"2026-06-08T10:15:00.000Z","message":{"role":"user","content":"hi"}}"#;
          assert_eq!(parse_line(user_line, "p").unwrap(), None);
      }

      #[test]
      fn ignores_assistant_line_without_usage() {
          let no_usage = r#"{"type":"assistant","timestamp":"2026-06-08T10:15:30.000Z","requestId":"req_x","message":{"id":"msg_x","model":"claude-opus-4-8"}}"#;
          assert_eq!(parse_line(no_usage, "p").unwrap(), None);
      }

      #[test]
      fn errors_on_garbage_then_caller_skips() {
          assert!(parse_line("{not json", "p").is_err());
      }
  }
  ```

- [ ] **Step 2: Run the test, expect FAIL.** `cargo test --lib sources::jsonl::tests`. Expected: the three `parse_line` tests panic with `not implemented`; `errors_on_garbage_then_caller_skips` also panics (stub never returns `Err`). FAILED.

- [ ] **Step 3: Minimal implementation.** Replace the stub `parse_line` (keep `AssistantRecord`):
  ```rust
  use anyhow::Context;
  use crate::timeutil;

  pub fn parse_line(line: &str, project: &str) -> anyhow::Result<Option<AssistantRecord>> {
      let line = line.trim();
      if line.is_empty() {
          return Ok(None);
      }
      let v: serde_json::Value = serde_json::from_str(line).context("jsonl line is not valid JSON")?;
      if v.get("type").and_then(|t| t.as_str()) != Some("assistant") {
          return Ok(None);
      }
      let message = match v.get("message") { Some(m) => m, None => return Ok(None) };
      let usage = match message.get("usage") { Some(u) => u, None => return Ok(None) };
      let output_tokens = match usage.get("output_tokens").and_then(|n| n.as_u64()) {
          Some(n) => n,
          None => return Ok(None),
      };
      let message_id = message.get("id").and_then(|s| s.as_str()).unwrap_or_default().to_string();
      let request_id = v.get("requestId").and_then(|s| s.as_str()).unwrap_or_default().to_string();
      let model = message.get("model").and_then(|s| s.as_str()).unwrap_or_default().to_string();
      let ts_str = v.get("timestamp").and_then(|s| s.as_str())
          .context("assistant line missing top-level timestamp")?;
      let timestamp = timeutil::parse_iso8601(ts_str)
          .with_context(|| format!("bad ISO-8601 timestamp: {ts_str}"))?;
      Ok(Some(AssistantRecord { message_id, request_id, model, output_tokens, timestamp, project: project.to_string() }))
  }
  ```

- [ ] **Step 4: Run the test, expect PASS.** `cargo test --lib sources::jsonl::tests` → all four pass.

- [ ] **Step 5: Commit.**
  ```
  git add src/sources/jsonl.rs
  git commit -m "feat(jsonl): parse one assistant JSONL line into AssistantRecord"
  ```

---

### Task M3-2 — `TokenLedger`: dedup + UTC-day bucket + per-model/project totals

**Files:**
- Modify: `C:\Users\oz\Desktop\claude-usage\src\sources\jsonl.rs`

- [ ] **Step 1: Write the failing tests.** Append a `#[cfg(test)] mod ledger_tests` asserting: streaming duplicates on the same `(message_id, request_id)` collapse to the **final** record (keep last `output_tokens`, not sum); distinct keys sum; `by_model` and `top_projects` sorted desc; `today_total_output` output-weighted.
  ```rust
  #[cfg(test)]
  mod ledger_tests {
      use super::*;
      use crate::timeutil;

      fn rec(msg: &str, req: &str, model: &str, out: u64, iso: &str, project: &str) -> AssistantRecord {
          AssistantRecord {
              message_id: msg.to_string(),
              request_id: req.to_string(),
              model: model.to_string(),
              output_tokens: out,
              timestamp: timeutil::parse_iso8601(iso).expect("iso parses"),
              project: project.to_string(),
          }
      }

      fn now() -> SystemTime {
          timeutil::parse_iso8601("2026-06-08T12:00:00.000Z").expect("iso parses")
      }

      #[test]
      fn dedup_keeps_final_record_not_sum() {
          let mut led = TokenLedger::new();
          led.ingest(&rec("m1", "r1", "claude-opus-4-8", 100, "2026-06-08T10:00:00.000Z", "p"), now());
          led.ingest(&rec("m1", "r1", "claude-opus-4-8", 400, "2026-06-08T10:00:01.000Z", "p"), now());
          led.ingest(&rec("m1", "r1", "claude-opus-4-8", 850, "2026-06-08T10:00:02.000Z", "p"), now());
          assert_eq!(led.stats(now()).today_total_output, 850);
      }

      #[test]
      fn distinct_requests_sum() {
          let mut led = TokenLedger::new();
          led.ingest(&rec("m1", "r1", "claude-opus-4-8", 850, "2026-06-08T10:00:00.000Z", "p"), now());
          led.ingest(&rec("m2", "r2", "claude-sonnet-4-5", 300, "2026-06-08T11:00:00.000Z", "p"), now());
          assert_eq!(led.stats(now()).today_total_output, 1150);
      }

      #[test]
      fn by_model_sorted_desc() {
          let mut led = TokenLedger::new();
          led.ingest(&rec("m1", "r1", "claude-sonnet-4-5", 300, "2026-06-08T10:00:00.000Z", "p"), now());
          led.ingest(&rec("m2", "r2", "claude-opus-4-8", 900, "2026-06-08T10:05:00.000Z", "p"), now());
          led.ingest(&rec("m3", "r3", "claude-opus-4-8", 100, "2026-06-08T10:06:00.000Z", "p"), now());
          assert_eq!(
              led.stats(now()).by_model,
              vec![("claude-opus-4-8".to_string(), 1000), ("claude-sonnet-4-5".to_string(), 300)]
          );
      }

      #[test]
      fn top_projects_sorted_desc() {
          let mut led = TokenLedger::new();
          led.ingest(&rec("m1", "r1", "claude-opus-4-8", 500, "2026-06-08T10:00:00.000Z", "alpha"), now());
          led.ingest(&rec("m2", "r2", "claude-opus-4-8", 200, "2026-06-08T10:05:00.000Z", "beta"), now());
          led.ingest(&rec("m3", "r3", "claude-opus-4-8", 400, "2026-06-08T10:06:00.000Z", "alpha"), now());
          assert_eq!(
              led.stats(now()).top_projects,
              vec![("alpha".to_string(), 900), ("beta".to_string(), 200)]
          );
      }
  }
  ```

- [ ] **Step 2: Run the tests, expect FAIL.** `cargo test --lib sources::jsonl::ledger_tests`. Expected: compile error — `cannot find type TokenLedger`. (Failing-to-compile is the red phase.)

- [ ] **Step 3: Minimal implementation (no live-rate yet).** Add below `parse_line`:
  ```rust
  use std::collections::HashMap;

  type DedupKey = (String, String);

  pub struct TokenLedger {
      day_start: SystemTime,
      today: HashMap<DedupKey, AssistantRecord>,
  }

  impl TokenLedger {
      pub fn new() -> Self {
          TokenLedger { day_start: SystemTime::UNIX_EPOCH, today: HashMap::new() }
      }

      /// Dedup on (message_id, request_id); roll the UTC-day bucket when now crosses midnight.
      pub fn ingest(&mut self, rec: &AssistantRecord, now: SystemTime) {
          let cur_day = timeutil::utc_day_start(now);
          if cur_day != self.day_start {
              self.day_start = cur_day;
              self.today.clear();
          }
          if timeutil::utc_day_start(rec.timestamp) != self.day_start {
              return;
          }
          let key = (rec.message_id.clone(), rec.request_id.clone());
          self.today.insert(key, rec.clone());
      }

      pub fn stats(&self, _now: SystemTime) -> crate::model::TokenStats {
          let mut today_total_output: u64 = 0;
          let mut by_model_map: HashMap<String, u64> = HashMap::new();
          let mut by_project_map: HashMap<String, u64> = HashMap::new();
          for rec in self.today.values() {
              today_total_output = today_total_output.saturating_add(rec.output_tokens);
              *by_model_map.entry(rec.model.clone()).or_insert(0) += rec.output_tokens;
              *by_project_map.entry(rec.project.clone()).or_insert(0) += rec.output_tokens;
          }
          crate::model::TokenStats {
              today_total_output,
              by_model: sorted_desc(by_model_map),
              live_tok_per_min: None,
              top_projects: sorted_desc(by_project_map),
          }
      }
  }

  impl Default for TokenLedger {
      fn default() -> Self { Self::new() }
  }

  /// Collapse a map into a Vec sorted by tokens desc, then name asc for stable ties.
  fn sorted_desc(map: HashMap<String, u64>) -> Vec<(String, u64)> {
      let mut v: Vec<(String, u64)> = map.into_iter().collect();
      v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
      v
  }
  ```

- [ ] **Step 4: Run the tests, expect PASS.** `cargo test --lib sources::jsonl::ledger_tests` → 4 pass.

- [ ] **Step 5: Commit.**
  ```
  git add src/sources/jsonl.rs
  git commit -m "feat(jsonl): TokenLedger with (msg,req) dedup, UTC-day bucket, per-model/project totals"
  ```

---

### Task M3-3 — `TokenLedger` live tokens/min + UTC-day roll

**Files:**
- Modify: `C:\Users\oz\Desktop\claude-usage\src\sources\jsonl.rs`

- [ ] **Step 1: Write the failing tests.** Append `#[cfg(test)] mod live_tests`. `live_tok_per_min` is derived from records whose `timestamp` is **< 90 s old** vs `now`; older records do not contribute. Rate = in-window tokens / minutes spanned (oldest in-window record → now, clamped to a 1 s floor). Day-roll: a new-UTC-day record clears yesterday.
  ```rust
  #[cfg(test)]
  mod live_tests {
      use super::*;
      use crate::timeutil;
      use std::time::Duration;

      fn rec_at(msg: &str, req: &str, out: u64, ts: SystemTime) -> AssistantRecord {
          AssistantRecord {
              message_id: msg.to_string(), request_id: req.to_string(),
              model: "claude-opus-4-8".to_string(), output_tokens: out,
              timestamp: ts, project: "p".to_string(),
          }
      }

      #[test]
      fn live_rate_uses_only_records_under_90s() {
          let now = timeutil::parse_iso8601("2026-06-08T12:00:00.000Z").expect("iso");
          let t_60s_ago = now - Duration::from_secs(60);
          let t_120s_ago = now - Duration::from_secs(120);
          let mut led = TokenLedger::new();
          led.ingest(&rec_at("m1", "r1", 1000, t_60s_ago), now);
          led.ingest(&rec_at("m2", "r2", 5000, t_120s_ago), now);
          let rate = led.stats(now).live_tok_per_min.expect("some live rate");
          assert!((rate - 1000.0).abs() < 1.0, "rate was {rate}");
      }

      #[test]
      fn live_rate_none_when_no_recent_records() {
          let now = timeutil::parse_iso8601("2026-06-08T12:00:00.000Z").expect("iso");
          let t_old = now - Duration::from_secs(600);
          let mut led = TokenLedger::new();
          led.ingest(&rec_at("m1", "r1", 1000, t_old), now);
          assert_eq!(led.stats(now).live_tok_per_min, None);
      }

      #[test]
      fn day_roll_clears_yesterday() {
          let day1_now = timeutil::parse_iso8601("2026-06-08T23:59:00.000Z").expect("iso");
          let day1_rec_ts = timeutil::parse_iso8601("2026-06-08T23:58:00.000Z").expect("iso");
          let day2_now = timeutil::parse_iso8601("2026-06-09T00:01:00.000Z").expect("iso");
          let day2_rec_ts = timeutil::parse_iso8601("2026-06-09T00:00:30.000Z").expect("iso");
          let mut led = TokenLedger::new();
          led.ingest(&rec_at("m1", "r1", 5000, day1_rec_ts), day1_now);
          assert_eq!(led.stats(day1_now).today_total_output, 5000);
          led.ingest(&rec_at("m2", "r2", 200, day2_rec_ts), day2_now);
          assert_eq!(led.stats(day2_now).today_total_output, 200);
      }
  }
  ```

- [ ] **Step 2: Run the tests, expect FAIL.** `cargo test --lib sources::jsonl::live_tests`. Expected: `live_rate_uses_only_records_under_90s` panics on `None` (current impl hardcodes `live_tok_per_min: None`). `day_roll_clears_yesterday` already passes (day-roll was implemented in M3-2) — a guard test.

- [ ] **Step 3: Minimal implementation.** Replace the `stats` body (keep the rest of `TokenLedger` and `sorted_desc`):
  ```rust
  pub fn stats(&self, now: SystemTime) -> crate::model::TokenStats {
      let mut today_total_output: u64 = 0;
      let mut by_model_map: HashMap<String, u64> = HashMap::new();
      let mut by_project_map: HashMap<String, u64> = HashMap::new();

      const LIVE_WINDOW: std::time::Duration = std::time::Duration::from_secs(90);
      let mut live_tokens: u64 = 0;
      let mut oldest_live: Option<SystemTime> = None;

      for rec in self.today.values() {
          today_total_output = today_total_output.saturating_add(rec.output_tokens);
          *by_model_map.entry(rec.model.clone()).or_insert(0) += rec.output_tokens;
          *by_project_map.entry(rec.project.clone()).or_insert(0) += rec.output_tokens;

          if let Ok(age) = now.duration_since(rec.timestamp) {
              if age < LIVE_WINDOW {
                  live_tokens = live_tokens.saturating_add(rec.output_tokens);
                  oldest_live = Some(match oldest_live {
                      Some(o) if o <= rec.timestamp => o,
                      _ => rec.timestamp,
                  });
              }
          }
      }

      let live_tok_per_min = match (live_tokens, oldest_live) {
          (0, _) | (_, None) => None,
          (tokens, Some(oldest)) => {
              let span = now.duration_since(oldest).unwrap_or(std::time::Duration::from_secs(1));
              let secs = span.as_secs_f64().max(1.0);
              Some(tokens as f64 / secs * 60.0)
          }
      };

      crate::model::TokenStats {
          today_total_output,
          by_model: sorted_desc(by_model_map),
          live_tok_per_min,
          top_projects: sorted_desc(by_project_map),
      }
  }
  ```

- [ ] **Step 4: Run the tests, expect PASS.** `cargo test --lib sources::jsonl::live_tests` → 3 pass. Then `cargo test --lib sources::jsonl` to confirm no M3-1/M3-2 regression.

- [ ] **Step 5: Commit.**
  ```
  git add src/sources/jsonl.rs
  git commit -m "feat(jsonl): live_tok_per_min from records under 90s old"
  ```

---

### Task M3-4 — JSONL fixtures (streaming dups, non-assistant noise, subagents)

**Files:**
- Create: `C:\Users\oz\Desktop\claude-usage\tests\fixtures\jsonl\projects\alpha\session-1.jsonl`
- Create: `C:\Users\oz\Desktop\claude-usage\tests\fixtures\jsonl\projects\beta\session-2.jsonl`
- Create: `C:\Users\oz\Desktop\claude-usage\tests\fixtures\jsonl\projects\alpha-subagents\session-sub.jsonl`

The directory layout mirrors `~/.claude/projects/*/*.jsonl`; the jsonl's **parent dir name** is the project. `alpha-subagents` exists to prove the `subagents` exclusion.

- [ ] **Step 1: Create `projects/alpha/session-1.jsonl`** (one user line ignored; a 3-row streaming sequence for `(msg_a1, req_a1)` deduping to 900; a distinct `(msg_a2, req_a2)` of 250):
  ```
  {"type":"user","timestamp":"2026-06-08T09:00:00.000Z","message":{"role":"user","content":"start"}}
  {"type":"assistant","timestamp":"2026-06-08T09:00:05.000Z","requestId":"req_a1","message":{"id":"msg_a1","model":"claude-opus-4-8","usage":{"input_tokens":1000,"output_tokens":100,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}
  {"type":"assistant","timestamp":"2026-06-08T09:00:06.000Z","requestId":"req_a1","message":{"id":"msg_a1","model":"claude-opus-4-8","usage":{"input_tokens":1000,"output_tokens":500,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}
  {"type":"assistant","timestamp":"2026-06-08T09:00:07.000Z","requestId":"req_a1","message":{"id":"msg_a1","model":"claude-opus-4-8","usage":{"input_tokens":1000,"output_tokens":900,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}
  {"type":"assistant","timestamp":"2026-06-08T09:05:00.000Z","requestId":"req_a2","message":{"id":"msg_a2","model":"claude-sonnet-4-5","usage":{"input_tokens":800,"output_tokens":250,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}
  ```

- [ ] **Step 2: Create `projects/beta/session-2.jsonl`** (one assistant line, 600 output, opus, project `beta`):
  ```
  {"type":"assistant","timestamp":"2026-06-08T10:00:00.000Z","requestId":"req_b1","message":{"id":"msg_b1","model":"claude-opus-4-8","usage":{"input_tokens":500,"output_tokens":600,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}
  ```

- [ ] **Step 3: Create `projects/alpha-subagents/session-sub.jsonl`** (huge token count that MUST be excluded because its path contains `subagents`):
  ```
  {"type":"assistant","timestamp":"2026-06-08T10:30:00.000Z","requestId":"req_sub","message":{"id":"msg_sub","model":"claude-opus-4-8","usage":{"input_tokens":100,"output_tokens":99999,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}
  ```

- [ ] **Step 4: Verify the fixtures exist (sanity).** `dir tests\fixtures\jsonl\projects /s /b`. Expected: the three `.jsonl` paths are listed.

- [ ] **Step 5: Commit.**
  ```
  git add tests/fixtures/jsonl
  git commit -m "test(jsonl): fixtures for streaming dups, non-assistant noise, subagents exclusion"
  ```

---

### Task M3-5 — `Cursor::update`: incremental, exclude subagents, rotation

`Cursor::update(&mut self, projects_root: &Path, ledger: &mut TokenLedger, now: SystemTime) -> anyhow::Result<usize>` — globs `projects_root/*/*.jsonl`, **excludes any path containing `subagents`**, reads only appended bytes since `last_size` (re-reads from 0 if shrank), parses new lines, ingests, returns the count of new records.

**Files:**
- Modify: `C:\Users\oz\Desktop\claude-usage\src\sources\jsonl.rs`

- [ ] **Step 1: Write the failing tests.** Append `#[cfg(test)] mod cursor_tests`: (a) first pass over M3-4 fixtures counts deduped records and excludes subagents; (b) second `update` with no changes returns 0; (c) appended bytes read incrementally; (d) a shrunk file re-reads from 0. Tests (c)/(d) copy the fixture tree into a unique temp dir.
  ```rust
  #[cfg(test)]
  mod cursor_tests {
      use super::*;
      use crate::timeutil;
      use std::fs;
      use std::path::{Path, PathBuf};

      fn fixtures_root() -> PathBuf {
          Path::new(env!("CARGO_MANIFEST_DIR"))
              .join("tests").join("fixtures").join("jsonl").join("projects")
      }

      fn now() -> SystemTime {
          timeutil::parse_iso8601("2026-06-08T12:00:00.000Z").expect("iso")
      }

      fn copy_tree(src: &Path, dst: &Path) {
          fs::create_dir_all(dst).unwrap();
          for entry in fs::read_dir(src).unwrap() {
              let entry = entry.unwrap();
              let path = entry.path();
              let target = dst.join(entry.file_name());
              if path.is_dir() { copy_tree(&path, &target); } else { fs::copy(&path, &target).unwrap(); }
          }
      }

      fn unique_tmp(tag: &str) -> PathBuf {
          let nanos = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_nanos();
          let dir = std::env::temp_dir().join(format!("cuw-jsonl-{tag}-{nanos}"));
          fs::create_dir_all(&dir).unwrap();
          dir
      }

      #[test]
      fn first_pass_counts_deduped_and_excludes_subagents() {
          let mut cur = Cursor::new();
          let mut led = TokenLedger::new();
          let n = cur.update(&fixtures_root(), &mut led, now()).expect("update ok");
          assert_eq!(n, 4, "new record count");
          let stats = led.stats(now());
          assert_eq!(stats.today_total_output, 1750);
          assert_eq!(
              stats.by_model,
              vec![("claude-opus-4-8".to_string(), 1500), ("claude-sonnet-4-5".to_string(), 250)]
          );
      }

      #[test]
      fn second_pass_no_changes_is_zero() {
          let mut cur = Cursor::new();
          let mut led = TokenLedger::new();
          cur.update(&fixtures_root(), &mut led, now()).unwrap();
          let again = cur.update(&fixtures_root(), &mut led, now()).unwrap();
          assert_eq!(again, 0);
          assert_eq!(led.stats(now()).today_total_output, 1750);
      }

      #[test]
      fn incremental_append_reads_only_new_bytes() {
          let tmp = unique_tmp("append");
          copy_tree(&fixtures_root(), &tmp);
          let mut cur = Cursor::new();
          let mut led = TokenLedger::new();
          let first = cur.update(&tmp, &mut led, now()).unwrap();
          assert_eq!(first, 4);
          let beta = tmp.join("beta").join("session-2.jsonl");
          let mut content = fs::read_to_string(&beta).unwrap();
          content.push_str("{\"type\":\"assistant\",\"timestamp\":\"2026-06-08T11:00:00.000Z\",\"requestId\":\"req_b2\",\"message\":{\"id\":\"msg_b2\",\"model\":\"claude-opus-4-8\",\"usage\":{\"output_tokens\":300}}}\n");
          fs::write(&beta, content).unwrap();
          let second = cur.update(&tmp, &mut led, now()).unwrap();
          assert_eq!(second, 1);
          assert_eq!(led.stats(now()).today_total_output, 1750 + 300);
      }

      #[test]
      fn rotation_shrink_rereads_from_zero() {
          let tmp = unique_tmp("rotate");
          copy_tree(&fixtures_root(), &tmp);
          let mut cur = Cursor::new();
          let mut led = TokenLedger::new();
          cur.update(&tmp, &mut led, now()).unwrap();
          let beta = tmp.join("beta").join("session-2.jsonl");
          fs::write(&beta, "{\"type\":\"assistant\",\"timestamp\":\"2026-06-08T11:30:00.000Z\",\"requestId\":\"req_rot\",\"message\":{\"id\":\"msg_rot\",\"model\":\"claude-opus-4-8\",\"usage\":{\"output_tokens\":42}}}\n").unwrap();
          let n = cur.update(&tmp, &mut led, now()).unwrap();
          assert_eq!(n, 1);
          assert!(led.stats(now()).by_model.iter().any(|(_, t)| *t >= 42));
      }
  }
  ```

- [ ] **Step 2: Run the tests, expect FAIL.** `cargo test --lib sources::jsonl::cursor_tests`. Expected: compile error — `cannot find type Cursor`.

- [ ] **Step 3: Minimal implementation.** Add to `src\sources\jsonl.rs`. Hand-rolled two-level directory walk (no glob crate; none in the dependency block); skip any path containing `subagents`; seek to `last_size` (or 0 on shrink); ingest parsed records; project = parent dir name.
  ```rust
  use std::io::{Read, Seek, SeekFrom};
  use std::path::{Path, PathBuf};

  pub struct Cursor {
      seen: HashMap<PathBuf, (u64, SystemTime)>, // path -> (last_size, last_mtime)
  }

  impl Cursor {
      pub fn new() -> Self { Cursor { seen: HashMap::new() } }

      pub fn update(
          &mut self,
          projects_root: &Path,
          ledger: &mut TokenLedger,
          now: SystemTime,
      ) -> anyhow::Result<usize> {
          let mut new_records: usize = 0;
          for path in discover_jsonl(projects_root) {
              if path.to_string_lossy().contains("subagents") {
                  continue;
              }
              let meta = match std::fs::metadata(&path) { Ok(m) => m, Err(_) => continue };
              let size = meta.len();
              let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
              let last_size = self.seen.get(&path).map(|(s, _)| *s).unwrap_or(0);
              let start = if size < last_size { 0 } else { last_size };
              if size == start {
                  self.seen.insert(path.clone(), (size, mtime));
                  continue;
              }
              let project = path.parent().and_then(|p| p.file_name())
                  .map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
              let mut file = match std::fs::File::open(&path) { Ok(f) => f, Err(_) => continue };
              if file.seek(SeekFrom::Start(start)).is_err() { continue; }
              let mut buf = String::new();
              if file.read_to_string(&mut buf).is_err() { continue; }
              for line in buf.lines() {
                  match parse_line(line, &project) {
                      Ok(Some(rec)) => { ledger.ingest(&rec, now); new_records += 1; }
                      Ok(None) => {}
                      Err(e) => eprintln!("jsonl: skipping bad line in {}: {e:#}", path.display()),
                  }
              }
              self.seen.insert(path.clone(), (size, mtime));
          }
          Ok(new_records)
      }
  }

  impl Default for Cursor {
      fn default() -> Self { Self::new() }
  }

  /// Find projects_root/*/*.jsonl (two levels). Empty Vec if the root is missing.
  fn discover_jsonl(projects_root: &Path) -> Vec<PathBuf> {
      let mut out = Vec::new();
      let project_dirs = match std::fs::read_dir(projects_root) { Ok(rd) => rd, Err(_) => return out };
      for proj in project_dirs.flatten() {
          let proj_path = proj.path();
          if !proj_path.is_dir() { continue; }
          let files = match std::fs::read_dir(&proj_path) { Ok(rd) => rd, Err(_) => continue };
          for f in files.flatten() {
              let p = f.path();
              if p.extension().and_then(|e| e.to_str()) == Some("jsonl") { out.push(p); }
          }
      }
      out.sort();
      out
  }
  ```
  > Edge case: reading the appended tail with `read_to_string` can fail if a write lands mid-UTF-8; we skip that file for the tick and retry next time (cursor advances only on a successful read). Acceptable for a 30 s loop. Do not add `Vec<u8>` + `from_utf8_lossy` complexity until a test demonstrates the need.

- [ ] **Step 4: Run the tests, expect PASS.** `cargo test --lib sources::jsonl::cursor_tests` → 4 pass.

- [ ] **Step 5: Run the whole module to confirm no regressions.** `cargo test --lib sources::jsonl`.

- [ ] **Step 6: Commit.**
  ```
  git add src/sources/jsonl.rs
  git commit -m "feat(jsonl): incremental Cursor::update with subagents exclusion and rotation handling"
  ```

---

### Task M3-6 — Wire JSONL into the collector tick

In M2 the collector's `read_tokens` closure returned `TokenStats::default()`. This task replaces that closure in `Collector::new` with one owning a real `Cursor` + `TokenLedger` (captured via `RefCell`), driving `cursor.update(projects_root, ledger, now)` then `ledger.stats(now)`. The quota/reconcile half (M2) is unchanged. The collector stays closure-based (consistent with M2 and M5-4) — **no `tick_at_root`/struct-field rewrite**.

**Files:**
- Modify: `C:\Users\oz\Desktop\claude-usage\src\collector.rs`

- [ ] **Step 1: Write the failing test.** Append to `src\collector.rs`'s `mod tests`. It injects a `read_tokens` closure pointing at the M3-4 fixtures (via a new helper `tokens_closure_for(root)`) and asserts the snapshot carries real stats.
  ```rust
  #[cfg(test)]
  mod jsonl_wire_tests {
      use super::*;
      use crate::config::Config;
      use crate::model::Provenance;
      use crate::timeutil;
      use std::path::{Path, PathBuf};
      use std::time::SystemTime;

      fn fixtures_root() -> PathBuf {
          Path::new(env!("CARGO_MANIFEST_DIR"))
              .join("tests").join("fixtures").join("jsonl").join("projects")
      }

      fn now() -> SystemTime {
          timeutil::parse_iso8601("2026-06-08T12:00:00.000Z").expect("iso")
      }

      #[test]
      fn tick_folds_jsonl_token_stats_into_snapshot() {
          // No quota sources → snapshot quota half is empty/Stale, but the token
          // half must reflect the fixtures.
          let mut c = Collector::new_with_sources(
              Config::default(),
              Box::new(|_now| None),
              Box::new(|_now| None),
              tokens_closure_for(&fixtures_root()),
          );
          let snap = c.tick(now());
          assert_eq!(snap.tokens.today_total_output, 1750);
          assert_eq!(snap.tokens.by_model.first().map(|(m, _)| m.as_str()), Some("claude-opus-4-8"));
          assert!(matches!(snap.source, Provenance::Stale { .. }));
      }
  }
  ```

- [ ] **Step 2: Run the test, expect FAIL.** `cargo test --lib collector::jsonl_wire_tests`. Expected: compile error — `cannot find function tokens_closure_for` (and the `read_tokens` closure in `new` is still the default).

- [ ] **Step 3: Minimal implementation.** In `src\collector.rs`: add the `tokens_closure_for` factory and use it inside `Collector::new`. Add imports and helpers:
  ```rust
  use crate::sources::jsonl::{Cursor, TokenLedger};
  use std::cell::RefCell;
  use std::path::{Path, PathBuf};

  /// ~/.claude/projects
  fn projects_root() -> PathBuf {
      dirs::home_dir().map(|h| h.join(".claude").join("projects")).unwrap_or_default()
  }

  /// Build a tokens closure that owns a live Cursor+TokenLedger over `root`.
  /// Tolerates I/O errors (keeps prior aggregates). Used by `new` and by tests.
  fn tokens_closure_for(root: &Path) -> TokensFn {
      let root = root.to_path_buf();
      let ledger = RefCell::new(TokenLedger::new());
      let cursor = RefCell::new(Cursor::new());
      Box::new(move |now: SystemTime| {
          let mut led = ledger.borrow_mut();
          let mut cur = cursor.borrow_mut();
          if let Err(e) = cur.update(&root, &mut led, now) {
              eprintln!("collector: jsonl update failed: {e:#}");
          }
          led.stats(now)
      })
  }
  ```
  Then change `Collector::new` to use it (replace the `read_tokens` line):
  ```rust
  // in Collector::new, replace:
  //   let read_tokens: TokensFn = Box::new(|_now| TokenStats::default());
  // with:
  let read_tokens: TokensFn = tokens_closure_for(&projects_root());
  ```
  > `TokensFn` is the type alias from M2 (`Box<dyn FnMut(SystemTime) -> TokenStats>`). `tokens_closure_for` returns it; the `RefCell`s are owned by the closure so the cursor/ledger state persists across ticks. This keeps `Collector` closure-based — M5-4 reuses this exact `tokens_closure_for` when it rewrites the constructor.

- [ ] **Step 4: Run the test, expect PASS.** `cargo test --lib collector::jsonl_wire_tests` → passes (`today_total_output == 1750`, dominant model opus).

- [ ] **Step 5: Run the full collector + jsonl suites.** `cargo test --lib collector` and `cargo test --lib sources::jsonl` — no regression.

- [ ] **Step 6: Commit.**
  ```
  git add src/collector.rs
  git commit -m "feat(collector): fold incremental JSONL TokenStats into the per-tick snapshot"
  ```

---

### Exit criteria for M3

- [ ] `cargo test --lib sources::jsonl` passes: `parse_line`, `TokenLedger` (dedup-keeps-final, distinct-sum, sorted desc, UTC-day roll, live rate < 90 s), and `Cursor::update` (first-pass + subagents exclusion, no-change=0, incremental append, rotation re-read).
- [ ] Streaming duplicates on `(message_id, request_id)` collapse to the **final** record's `output_tokens` — never the sum.
- [ ] Any path containing `subagents` is excluded: the 99 999-token fixture line never contributes.
- [ ] `Cursor::update` reads only appended bytes since `last_size` and re-reads from 0 when a file shrinks; a corrupt line is logged-and-skipped, never aborting the update.
- [ ] `Collector::tick` produces a `UsageSnapshot` whose `tokens` field is real (the M2 `TokenStats::default()` placeholder is gone) while the M2 quota/reconcile half is unchanged; the collector stays closure-based.
- [ ] Fixtures committed under `tests\fixtures\jsonl\projects\` (`alpha`, `beta`, `alpha-subagents`).
- [ ] Six conventional-commit commits (M3-1 … M3-6); full `cargo test` green with no M1/M2 regressions.
- [ ] No new crates: only `serde_json`, `anyhow`, `chrono` (via `timeutil`), `dirs`, std.

---

## Milestone M4: Gauge + chrome

**Goal:** Add the gauge (ProgressCircle rings) view, wire the right-click context menu (toggle view, backdrop switch, refresh-now, scale, quit), implement left-drag-to-move, and persist every chrome change back to `widget-config.json` — so the widget is fully interactive and remembers its state across restarts.

**Verification kind: manual-verify.** GUI rendering, the Mica backdrop switch, mouse drag, and the menu cannot be unit-tested. Each task ends in a concrete `cargo run` + observation, then a commit. Pure logic touched here (scale clamping, reset formatting, config round-trip) gets a tiny inline test where cheap.

> Prerequisite (M2): `src/ui/mod.rs` holds a `Root` view carrying `snapshot: UsageSnapshot` + `config: Config`, rendering `ui/bars.rs` via `render_bars(&snapshot, now, warn, critical, window, app)`. `src/ui/theme.rs` exposes `level_color`. M4 fills in the `ViewMode::Gauge` branch and the interaction layer, and **adds three fields to `Root`**: `menu_at: Option<Point<Pixels>>`, `request_refresh: bool`, `pending_position_save: bool` (all initialized in `Root::new`).

> **Signature convention (matches M2):** both view renderers take `(snapshot, now, warn, critical, window, app)`. M4 keeps that — it does NOT switch to passing `&Config`. Thresholds come from `self.config.warn_threshold` / `critical_threshold` at the call site.

---

### Task M4-1: Gauge view (ProgressCircle rings)

**Files:**
- Create: `C:\Users\oz\Desktop\claude-usage\src\ui\gauge.rs`
- Modify: `C:\Users\oz\Desktop\claude-usage\src\ui\mod.rs`
- Modify: `C:\Users\oz\Desktop\claude-usage\src\ui\theme.rs`

- [ ] **Step 1: Add a duration formatter to `ui/theme.rs` (TDD).** The gauge prints countdowns ("2h13m", "4d5h"). Pure and cheap. Append to `src\ui\theme.rs`:
  ```rust
  use std::time::{Duration, SystemTime};

  /// "time until reset" for a ring label, e.g. "2h13m", "4d5h", "now". None → "—".
  pub fn fmt_reset_in(resets_at: Option<SystemTime>, now: SystemTime) -> String {
      let Some(at) = resets_at else { return "—".to_string() };
      let remaining = at.duration_since(now).unwrap_or(Duration::ZERO);
      let secs = remaining.as_secs();
      if secs == 0 { return "now".to_string(); }
      let days = secs / 86_400;
      let hours = (secs % 86_400) / 3_600;
      let mins = (secs % 3_600) / 60;
      if days > 0 { format!("{days}d{hours}h") }
      else if hours > 0 { format!("{hours}h{mins:02}m") }
      else { format!("{mins}m") }
  }

  #[cfg(test)]
  mod reset_fmt_tests {
      use super::*;
      use std::time::Duration;

      fn t0() -> SystemTime { SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000) }

      #[test] fn none_is_dash() { assert_eq!(fmt_reset_in(None, t0()), "—"); }
      #[test] fn hours_and_minutes() {
          assert_eq!(fmt_reset_in(Some(t0() + Duration::from_secs(2*3600 + 13*60)), t0()), "2h13m");
      }
      #[test] fn days() {
          assert_eq!(fmt_reset_in(Some(t0() + Duration::from_secs(4*86_400 + 5*3600)), t0()), "4d5h");
      }
      #[test] fn past_clamps_to_now() {
          assert_eq!(fmt_reset_in(Some(t0() - Duration::from_secs(60)), t0()), "now");
      }
      #[test] fn minutes_only() {
          assert_eq!(fmt_reset_in(Some(t0() + Duration::from_secs(45*60)), t0()), "45m");
      }
  }
  ```
  > Distinct from M2's `timeutil::format_countdown` (used by bars). Bars and gauge intentionally use slightly different formats; both are pure and tested. (If you prefer a single shared formatter, have `fmt_reset_in` delegate to `timeutil::format_countdown` and adjust the `days` test — optional.)

- [ ] **Step 2: Run the helper test, expect PASS.** `cargo test --lib ui::theme::reset_fmt_tests` → `5 passed`.

- [ ] **Step 3: Write the gauge render function in `src/ui/gauge.rs`.** One `ProgressCircle` per window; signature mirrors `render_bars`. Confirm the `gpui_component::ProgressCircle` builder (`::new(id).value(..).color(..).with_size(..)`) and label nesting against the M0-pinned example — adjust only the marked line.
  ```rust
  use std::time::SystemTime;

  use gpui::{div, px, App, IntoElement, ParentElement, SharedString, Styled, Window as GpuiWindow};
  use gpui_component::progress::ProgressCircle;

  use crate::model::{Level, UsageSnapshot, Window as UsageWindow};
  use crate::ui::theme::{fmt_reset_in, level_color};

  fn ring(
      id: &'static str,
      label: &'static str,
      win: &UsageWindow,
      now: SystemTime,
      warn: f32,
      critical: f32,
      diameter: f32,
  ) -> impl IntoElement {
      let pct = win.used_pct.clamp(0.0, 100.0);
      let level = Level::from_pct(pct, warn, critical);
      let color = level_color(level);
      let pct_label: SharedString = format!("{}%", pct.round() as i64).into();
      let reset_label: SharedString = fmt_reset_in(win.resets_at, now).into();

      div()
          .flex()
          .flex_col()
          .items_center()
          .gap_1()
          .child(
              div()
                  .relative()
                  // CONFIRM against M0 example: ProgressCircle builder + value units.
                  .child(ProgressCircle::new(id).value(pct as usize).color(color).with_size(px(diameter)))
                  .child(
                      div().absolute().inset_0().flex().items_center().justify_center().child(pct_label),
                  ),
          )
          .child(div().text_xs().child(SharedString::from(label)))
          .child(div().text_xs().opacity(0.7).child(reset_label))
  }

  /// Render the gauge (rings) view. Mirrors render_bars' signature.
  pub fn render_gauge(
      snap: &UsageSnapshot,
      now: SystemTime,
      warn: f32,
      critical: f32,
      _window: &mut GpuiWindow,
      _cx: &mut App,
  ) -> impl IntoElement {
      let mut row = div().flex().flex_row().items_start().gap_4();
      row = row.child(ring("ring-5h", "5H", &snap.five_hour, now, warn, critical, 64.0));
      row = row.child(ring("ring-7d", "7D", &snap.seven_day, now, warn, critical, 64.0));
      if let Some(opus) = &snap.seven_day_opus {
          row = row.child(ring("ring-opus", "Opus", opus, now, warn, critical, 56.0));
      }
      div().flex().flex_col().items_center().gap_2().p_3().child(row)
  }
  ```

- [ ] **Step 4: Register the module + dispatch on `view_mode` in `src/ui/mod.rs`.** Add `pub mod gauge;` to the module list, and branch the `Render` body. Update the `Render` impl in `src\ui\mod.rs`:
  ```rust
  pub mod bars;
  pub mod gauge;
  pub mod theme;

  use crate::config::ViewMode;
  use gpui::{div, IntoElement, ParentElement, Render, Styled, Context, Window as GpuiWindow};
  use std::time::SystemTime;

  impl Render for Root {
      fn render(&mut self, window: &mut GpuiWindow, cx: &mut Context<Self>) -> impl IntoElement {
          let now = SystemTime::now();
          let warn = self.config.warn_threshold;
          let critical = self.config.critical_threshold;
          let app = cx.app_mut();
          let body = match self.config.view_mode {
              ViewMode::Bars => crate::ui::bars::render_bars(&self.snapshot, now, warn, critical, window, app).into_any_element(),
              ViewMode::Gauge => crate::ui::gauge::render_gauge(&self.snapshot, now, warn, critical, window, app).into_any_element(),
          };
          div().size_full().child(body)
      }
  }
  ```
  > `into_any_element()` unifies the two `impl IntoElement` arms. If `cx.app_mut()` can't be borrowed alongside `window` in the pinned gpui rev, capture `warn`/`critical` first (as shown) and pass `cx`/`window` per the M0 example's accessor pattern. The M4-2 menu wiring will further extend this `Render` body — keep edits additive.

- [ ] **Step 5: Run + observe the gauge.** Temporarily set the default to gauge for this run only: in `config.rs`'s `Default`, change `view_mode: ViewMode::Bars` to `ViewMode::Gauge`. Then `cargo run`. Look for: a borderless frosted window showing **two rings side by side** — left `5H` filled to the snapshot %, center showing `72%`, a `2h13m`-style countdown below; right `7D` ring + day/hour countdown. Ring color matches level (red ≥90, amber 70–89, green else). A third smaller ring if `seven_day_opus` is present.

- [ ] **Step 6: Revert the temporary default.** Restore `view_mode: ViewMode::Bars` in `config.rs` `Default`. `cargo run` again shows bars by default.

- [ ] **Step 7: Commit.**
  ```
  git add src/ui/gauge.rs src/ui/mod.rs src/ui/theme.rs
  git commit -m "feat(ui): add gauge (ProgressCircle rings) view with reset countdowns"
  ```

---

### Task M4-2: Right-click context menu (toggle view, backdrop, refresh, scale, quit)

**Files:**
- Modify: `C:\Users\oz\Desktop\claude-usage\src\ui\mod.rs`
- Modify: `C:\Users\oz\Desktop\claude-usage\src\config.rs`
- Modify: `C:\Users\oz\Desktop\claude-usage\src\main.rs` (poller honors `request_refresh`)

- [ ] **Step 1: Add config mutators to `config.rs` (with clamp tests).** Append:
  ```rust
  impl Config {
      pub fn toggle_view(&mut self) {
          self.view_mode = match self.view_mode { ViewMode::Bars => ViewMode::Gauge, ViewMode::Gauge => ViewMode::Bars };
      }
      pub fn set_backdrop(&mut self, backdrop: Backdrop) { self.backdrop = backdrop; }
      /// Multiply scale by `factor`, clamped to 0.6..=2.0.
      pub fn nudge_scale(&mut self, factor: f32) { self.scale = (self.scale * factor).clamp(0.6, 2.0); }
  }

  #[cfg(test)]
  mod menu_tests {
      use super::*;

      #[test]
      fn toggle_view_round_trips() {
          let mut c = Config::default();
          assert_eq!(c.view_mode, ViewMode::Bars);
          c.toggle_view(); assert_eq!(c.view_mode, ViewMode::Gauge);
          c.toggle_view(); assert_eq!(c.view_mode, ViewMode::Bars);
      }
      #[test]
      fn nudge_scale_clamps_high() {
          let mut c = Config::default();
          for _ in 0..20 { c.nudge_scale(1.1); }
          assert!((c.scale - 2.0).abs() < 1e-3);
      }
      #[test]
      fn nudge_scale_clamps_low() {
          let mut c = Config::default();
          for _ in 0..20 { c.nudge_scale(0.9); }
          assert!((c.scale - 0.6).abs() < 1e-3);
      }
  }
  ```

- [ ] **Step 2: Run the mutator tests, expect PASS.** `cargo test --lib config::menu_tests` → `3 passed`.

- [ ] **Step 3: Extend `Root` with interaction state + a menu builder in `src/ui/mod.rs`.** First, update `Root` and `Root::new` to carry the three interaction fields:
  ```rust
  use gpui::{Pixels, Point};

  pub struct Root {
      pub snapshot: UsageSnapshot,
      pub config: Config,
      pub menu_at: Option<Point<Pixels>>,
      pub request_refresh: bool,
      pub pending_position_save: bool,
  }

  impl Root {
      pub fn new(config: Config) -> Self {
          Self {
              snapshot: placeholder_snapshot(),
              config,
              menu_at: None,
              request_refresh: false,
              pending_position_save: false,
          }
      }
      // set_snapshot unchanged from M2.

      fn save_config(&self) {
          if let Err(e) = self.config.save() { eprintln!("config save failed: {e:#}"); }
      }
  }
  ```
  Then add the menu builder + backdrop applier. The `gpui_component` menu API (`ContextMenu`/`PopupMenu`) must be confirmed against the M0-pinned example; closest-known form:
  ```rust
  use gpui::{deferred, AnyElement, Context, InteractiveElement, MouseButton, MouseDownEvent, Window as GpuiWindow};
  use gpui_component::menu::ContextMenu;
  use crate::config::Backdrop;

  impl Root {
      fn build_menu(&mut self, at: Point<Pixels>, _window: &mut GpuiWindow, cx: &mut Context<Self>) -> AnyElement {
          deferred(
              ContextMenu::new("widget-menu")
                  .anchored(at)
                  .menu("Toggle view", cx.listener(|this, _, _w, cx| { this.config.toggle_view(); this.save_config(); cx.notify(); }))
                  .menu("Backdrop: Mica", cx.listener(|this, _, w, cx| this.set_backdrop(Backdrop::Mica, w, cx)))
                  .menu("Backdrop: Mica Alt", cx.listener(|this, _, w, cx| this.set_backdrop(Backdrop::MicaAlt, w, cx)))
                  .menu("Backdrop: Acrylic", cx.listener(|this, _, w, cx| this.set_backdrop(Backdrop::Acrylic, w, cx)))
                  .menu("Backdrop: Transparent", cx.listener(|this, _, w, cx| this.set_backdrop(Backdrop::Transparent, w, cx)))
                  .menu("Backdrop: Opaque", cx.listener(|this, _, w, cx| this.set_backdrop(Backdrop::Opaque, w, cx)))
                  .menu("Refresh now", cx.listener(|this, _, _w, cx| { this.request_refresh = true; cx.notify(); }))
                  .menu("Scale +", cx.listener(|this, _, _w, cx| { this.config.nudge_scale(1.1); this.save_config(); cx.notify(); }))
                  .menu("Scale -", cx.listener(|this, _, _w, cx| { this.config.nudge_scale(0.9); this.save_config(); cx.notify(); }))
                  .separator()
                  .menu("Quit", cx.listener(|_this, _, _w, cx| cx.quit())),
          )
          .with_priority(1)
          .into_any_element()
      }

      fn set_backdrop(&mut self, backdrop: Backdrop, window: &mut GpuiWindow, cx: &mut Context<Self>) {
          self.config.set_backdrop(backdrop);
          self.save_config();
          let appearance = crate::win::backdrop::appearance_for(self.config.backdrop); // M6-1 helper
          window.set_background_appearance(appearance);
          cx.notify();
      }
  }
  ```
  > `cx.quit()` and `window.set_background_appearance(..)` are gpui API; confirm exact names against the M0 pin (`set_background_appearance` may be `set_window_background_appearance`). **`win::backdrop::appearance_for` is delivered in M6-1.** Until M6-1 lands, `set_backdrop` can call `cx.notify()` only (persist + repaint) and skip the live re-apply; wire the live `appearance_for` call when M6-1 is done. (Backdrop also re-applies from config on next launch via `main.rs`.)

- [ ] **Step 4: Wire the right-click handler + render the menu.** Extend the `Render` body from M4-1 so the outer element records right-clicks and renders the menu:
  ```rust
  impl Render for Root {
      fn render(&mut self, window: &mut GpuiWindow, cx: &mut Context<Self>) -> impl IntoElement {
          let now = SystemTime::now();
          let warn = self.config.warn_threshold;
          let critical = self.config.critical_threshold;
          let body = {
              let app = cx.app_mut();
              match self.config.view_mode {
                  ViewMode::Bars => crate::ui::bars::render_bars(&self.snapshot, now, warn, critical, window, app).into_any_element(),
                  ViewMode::Gauge => crate::ui::gauge::render_gauge(&self.snapshot, now, warn, critical, window, app).into_any_element(),
              }
          };

          let mut root = div()
              .id("widget-root")
              .size_full()
              .on_mouse_down(MouseButton::Right, cx.listener(|this, ev: &MouseDownEvent, _w, cx| {
                  this.menu_at = Some(ev.position);
                  cx.notify();
              }))
              .child(body);

          if let Some(at) = self.menu_at.take() {
              let menu = self.build_menu(at, window, cx);
              root = root.child(menu);
          }
          root
      }
  }
  ```

- [ ] **Step 5: Make the poller honor `request_refresh` in `main.rs`.** In the spawned poller loop (M2-5), before awaiting the full interval, read+clear `root.request_refresh` to force an immediate next tick:
  ```rust
  // inside the poller loop, after pushing the snapshot, replacing the bare timer await:
  let forced = root
      .update(&mut acx, |root, _cx| std::mem::replace(&mut root.request_refresh, false))
      .unwrap_or(false);
  let interval = if forced { Duration::from_millis(0) } else { refresh };
  acx.background_executor().timer(interval).await;
  ```
  > Confirm the `update`/entity-accessor names against the M0 pin. `refresh` is the `Duration` captured by `spawn_poller`.

- [ ] **Step 6: Run + observe the menu.** `cargo run`, then:
  1. **Right-click** → menu lists Toggle view, five Backdrop entries, Refresh now, Scale +, Scale -, a separator, Quit.
  2. **Toggle view** → bars↔rings; menu closes.
  3. **Backdrop: Opaque** → frosted glass becomes solid immediately (if M6-1's `appearance_for` is wired); **Backdrop: Mica** → frost returns.
  4. **Scale +** a few times grows the widget; **Scale -** shrinks it; stops at 2.0×/0.6×.
  5. **Refresh now** → no crash (an immediate tick fires).
  6. **Quit** → app exits cleanly.

- [ ] **Step 7: Commit.**
  ```
  git add src/ui/mod.rs src/config.rs src/main.rs
  git commit -m "feat(ui): right-click menu for view toggle, backdrop, scale, refresh, quit"
  ```

---

### Task M4-3: Left-drag-to-move + persist window position

**Files:**
- Modify: `C:\Users\oz\Desktop\claude-usage\src\ui\mod.rs`
- Modify: `C:\Users\oz\Desktop\claude-usage\src\main.rs`

- [ ] **Step 1: Add a left-button drag handler to the root element.** gpui drives the OS move loop via `window.start_window_move()` on left-mouse-down (custom caption: the whole body is draggable). Confirm the method name (`start_window_move` vs `start_system_move`) against the M0 pin. Add the left-button handler BEFORE the right-button one in the `Render` body:
  ```rust
  let mut root = div()
      .id("widget-root")
      .size_full()
      .on_mouse_down(MouseButton::Left, cx.listener(|this, _ev: &MouseDownEvent, window, cx| {
          window.start_window_move();
          this.pending_position_save = true;
          cx.notify();
      }))
      .on_mouse_down(MouseButton::Right, cx.listener(|this, ev: &MouseDownEvent, _w, cx| {
          this.menu_at = Some(ev.position);
          cx.notify();
      }))
      .child(body);
  ```

- [ ] **Step 2: Persist the window origin after a move.** Add a helper on `Root` and call it at the end of `Render::render`:
  ```rust
  impl Root {
      fn persist_position_if_pending(&mut self, window: &GpuiWindow) {
          if !self.pending_position_save { return; }
          self.pending_position_save = false;
          let bounds = window.bounds();
          let origin = bounds.origin;
          self.config.position = Some((f32::from(origin.x), f32::from(origin.y)));
          self.save_config();
      }
  }
  ```
  and call it just before returning `root`:
  ```rust
          self.persist_position_if_pending(window);
          if let Some(at) = self.menu_at.take() {
              let menu = self.build_menu(at, window, cx);
              root = root.child(menu);
          }
          root
  ```
  > `f32::from(Pixels)` may need to be `origin.x.0` depending on the pinned gpui `Pixels` type — confirm against M0 and use whichever yields raw `f32`. The tuple matches `Config::position: Option<(f32, f32)>`.

- [ ] **Step 3: Apply the saved position at startup in `main.rs`.** Where `WindowOptions` is built (M0/M2), set `window_bounds` from `config.position` when present. Confirm `Bounds`/`WindowBounds` construction against the M0 pin:
  ```rust
  let size = gpui::size(gpui::px(260.0), gpui::px(140.0));
  let bounds = match config.position {
      Some((x, y)) => gpui::Bounds { origin: gpui::point(gpui::px(x), gpui::px(y)), size },
      None => gpui::Bounds { origin: gpui::point(gpui::px(48.0), gpui::px(48.0)), size },
  };
  // window_bounds: Some(gpui::WindowBounds::Windowed(bounds)) in WindowOptions
  ```

- [ ] **Step 4: Run + observe drag + persistence.** `cargo run`:
  1. **Left-press and drag** anywhere on the body → the window follows the cursor; release drops it.
  2. Note the location, **Quit** (right-click → Quit).
  3. Open `C:\Users\oz\AppData\Roaming\claude-usage\widget-config.json` and confirm `"position"` holds the dragged coordinates.
  4. `cargo run` **again** → the widget reopens at the **same** location.

- [ ] **Step 5: Commit.**
  ```
  git add src/ui/mod.rs src/main.rs
  git commit -m "feat(ui): left-drag-to-move with persisted window position"
  ```

---

### Task M4-4: End-to-end chrome verification + config round-trip

**Files:**
- Modify: `C:\Users\oz\Desktop\claude-usage\src\config.rs` (test only; impl only if a bug surfaces)

- [ ] **Step 1: Add a serialize→deserialize round-trip test.** Append to `src\config.rs`:
  ```rust
  #[cfg(test)]
  mod roundtrip_tests {
      use super::*;

      #[test]
      fn chrome_changes_survive_serde_round_trip() {
          let mut c = Config::default();
          c.toggle_view();
          c.set_backdrop(Backdrop::Acrylic);
          c.nudge_scale(1.1);
          c.position = Some((123.0, 456.0));
          let json = serde_json::to_string(&c).expect("serialize");
          let back: Config = serde_json::from_str(&json).expect("deserialize");
          assert_eq!(back.view_mode, ViewMode::Gauge);
          assert_eq!(back.backdrop, Backdrop::Acrylic);
          assert!((back.scale - 1.1).abs() < 1e-3);
          assert_eq!(back.position, Some((123.0, 456.0)));
      }

      #[test]
      fn unknown_fields_are_tolerated() {
          let json = r#"{ "scale": 1.5, "future_field": 42 }"#;
          let c: Config = serde_json::from_str(json).expect("forward-compatible");
          assert!((c.scale - 1.5).abs() < 1e-3);
          assert_eq!(c.view_mode, ViewMode::Bars);
      }
  }
  ```

- [ ] **Step 2: Run the round-trip tests, expect PASS.** `cargo test --lib config::roundtrip_tests` → `2 passed`. If `unknown_fields_are_tolerated` fails, the `#[serde(default)]` on `Config` is missing — fix that, not the test.

- [ ] **Step 3: Full-app manual round-trip.** `cargo run`, then in one session: Toggle view → Gauge; Backdrop → Acrylic; Scale + twice; left-drag to a new corner; Quit. Re-launch `cargo run`. Look for: the widget opens **in Gauge view, Acrylic backdrop, larger scale, dragged corner** — all four restored at once, rings color-coded, floating above other windows.

- [ ] **Step 4: Run the whole suite.** `cargo test`. Expect all M1–M4 unit tests green.

- [ ] **Step 5: Commit.**
  ```
  git add src/config.rs
  git commit -m "test(config): chrome settings survive serde round-trip and forward-compat"
  ```

---

### Exit criteria for M4

- [ ] Right-click → **Toggle view** switches between bars and a working **gauge (ProgressCircle rings)** view; both render the same `UsageSnapshot`, color-coded by `Level`, each ring labelled (5H / 7D / optional Opus) with a reset countdown.
- [ ] The right-click **context menu** performs: Toggle view, the five Backdrop choices, Refresh now, Scale +/−, Quit.
- [ ] Selecting a **backdrop** changes the window material live (once M6-1's `appearance_for` is wired); the window stays borderless and always-on-top.
- [ ] **Left-drag** moves the borderless window; **Scale ±** resizes, clamped 0.6×–2.0×.
- [ ] View mode, backdrop, scale, and position persist to `%APPDATA%/claude-usage/widget-config.json` and are re-applied on the next launch.
- [ ] `cargo test` green: `ui::theme::reset_fmt_tests`, `config::menu_tests`, `config::roundtrip_tests`.
- [ ] Each task committed; working tree clean.

---

## Milestone M5: Status-line path

**Goal:** Implement the `claude-usage --statusline` helper that Claude Code invokes after each assistant message — parse the session JSON on stdin, atomically write the `rate_limits` block to `~/.claude/widget-cache/ratelimits.json`, print a one-line status. Then confirm the status-line cache is wired into `reconcile()` and `Collector::tick()` (zero OAuth calls while actively coding), and add an optional `settings.json` registration helper.

**Verification kind: mixed** — TDD the pure logic (`write_cache_from_stdin`/`read_cache` round-trip, `run_statusline`, `reconcile` boundaries, `Collector::tick` OAuth-skip, the `settings.json` merge); manual-verify the end-to-end `--statusline` invocation and live-while-coding GUI behavior.

> `sources/statusline.rs` and `reconcile` were stubbed/implemented earlier. M5-1 fills the real statusline body (replacing the M2-3 minimal stub), M5-3 adds boundary tests around the M2-1 `reconcile` (body already final), and M5-4 adds boundary tests for the M2/M3 collector's OAuth-skip logic (already implemented in M2-3). **The collector seam stays `new_with_sources` with `now`-taking closures (M2/M3 shape) — M5 does NOT rename it or change the closure signatures.**

---

### Task M5-1: Atomic cache write + read round-trip (TDD)

The status-line stdin JSON: `rate_limits.{five_hour,seven_day}.{used_percentage, resets_at}`, where `resets_at` is **Unix epoch SECONDS** (≠ the OAuth ISO-8601 form). We normalize on write and persist `written_at` so `read_cache` populates `QuotaReading.observed_at` for freshness.

**Files:**
- Modify: `C:\Users\oz\Desktop\claude-usage\src\sources\statusline.rs` (replace the M2-3 minimal stub)

- [ ] **Step 1: Write the failing test.** Append to `src\sources\statusline.rs`:
  ```rust
  #[cfg(test)]
  mod tests {
      use super::*;
      use std::time::{Duration, UNIX_EPOCH};

      fn tmp_path(name: &str) -> std::path::PathBuf {
          let mut p = std::env::temp_dir();
          p.push(format!("cuw-test-{}-{}.json", std::process::id(), name));
          p
      }

      #[test]
      fn write_then_read_round_trips_percent_and_reset() {
          let stdin = r#"{
              "rate_limits": {
                  "five_hour": { "used_percentage": 72.5, "resets_at": 1749412380 },
                  "seven_day": { "used_percentage": 41.0, "resets_at": 1749744000 }
              }
          }"#;
          let path = tmp_path("roundtrip");
          let _ = std::fs::remove_file(&path);
          write_cache_from_stdin(stdin, &path).expect("write ok");
          let reading = read_cache(&path).expect("read ok");
          assert_eq!(reading.five_hour.used_pct, 72.5);
          assert_eq!(reading.seven_day.used_pct, 41.0);
          assert_eq!(reading.five_hour.resets_at, Some(UNIX_EPOCH + Duration::from_secs(1_749_412_380)));
          assert_eq!(reading.seven_day.resets_at, Some(UNIX_EPOCH + Duration::from_secs(1_749_744_000)));
          assert!(reading.seven_day_opus.is_none());
          assert_eq!(reading.source, crate::model::Provenance::StatusLine);
          let _ = std::fs::remove_file(&path);
      }

      #[test]
      fn read_cache_returns_none_on_missing() {
          let path = tmp_path("missing");
          let _ = std::fs::remove_file(&path);
          assert!(read_cache(&path).is_none());
      }

      #[test]
      fn read_cache_returns_none_on_corrupt() {
          let path = tmp_path("corrupt");
          std::fs::write(&path, b"{ this is not json").unwrap();
          assert!(read_cache(&path).is_none());
          let _ = std::fs::remove_file(&path);
      }

      #[test]
      fn write_is_atomic_no_leftover_temp() {
          let stdin = r#"{"rate_limits":{
              "five_hour":{"used_percentage":10.0,"resets_at":1749412380},
              "seven_day":{"used_percentage":5.0,"resets_at":1749744000}}}"#;
          let path = tmp_path("atomic");
          let _ = std::fs::remove_file(&path);
          write_cache_from_stdin(stdin, &path).unwrap();
          let tmp = path.with_extension("json.tmp");
          assert!(!tmp.exists(), "temp file must be renamed away");
          assert!(path.exists());
          let _ = std::fs::remove_file(&path);
      }
  }
  ```

- [ ] **Step 2: Run the test, expect FAIL.** `cargo test --lib sources::statusline::tests`. Expected: the M2-3 minimal `read_cache` returns `None` and `write_cache_from_stdin` does not exist yet (or is the stub) → round-trip and atomic tests fail.

- [ ] **Step 3: Implement the cache types + `write_cache_from_stdin` + `read_cache` + `cache_path`.** Replace the entire non-test contents of `src\sources\statusline.rs` (this supersedes the M2-3 minimal stubs; keep the public signatures from the Shared Contracts):
  ```rust
  use crate::model::{Provenance, Window};
  use crate::sources::QuotaReading;
  use anyhow::{Context, Result};
  use std::path::{Path, PathBuf};
  use std::time::{Duration, SystemTime, UNIX_EPOCH};

  /// Our cache file: already-normalized seconds so the reader is unit-agnostic.
  #[derive(serde::Serialize, serde::Deserialize)]
  struct CacheFile {
      five_hour_pct: f32,
      five_hour_resets_at: Option<u64>,
      seven_day_pct: f32,
      seven_day_resets_at: Option<u64>,
      written_at: u64, // unix seconds
  }

  #[derive(serde::Deserialize)]
  struct StdinPayload { rate_limits: RateLimits }
  #[derive(serde::Deserialize)]
  struct RateLimits { five_hour: StdinWindow, seven_day: StdinWindow }
  #[derive(serde::Deserialize)]
  struct StdinWindow {
      used_percentage: f32,
      resets_at: Option<u64>, // Unix SECONDS in the status-line form
  }

  /// ~/.claude/widget-cache/ratelimits.json
  pub fn cache_path() -> PathBuf {
      let mut p = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
      p.push(".claude");
      p.push("widget-cache");
      p.push("ratelimits.json");
      p
  }

  /// Parse the status-line stdin JSON and write the normalized cache atomically.
  pub fn write_cache_from_stdin(stdin_json: &str, path: &Path) -> Result<()> {
      let payload: StdinPayload =
          serde_json::from_str(stdin_json).context("parse status-line stdin JSON")?;
      let now_secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
      let cache = CacheFile {
          five_hour_pct: payload.rate_limits.five_hour.used_percentage,
          five_hour_resets_at: payload.rate_limits.five_hour.resets_at,
          seven_day_pct: payload.rate_limits.seven_day.used_percentage,
          seven_day_resets_at: payload.rate_limits.seven_day.resets_at,
          written_at: now_secs,
      };
      let body = serde_json::to_vec_pretty(&cache).context("serialize cache")?;
      if let Some(dir) = path.parent() {
          std::fs::create_dir_all(dir).with_context(|| format!("create cache dir {}", dir.display()))?;
      }
      let tmp = path.with_extension("json.tmp");
      std::fs::write(&tmp, &body).with_context(|| format!("write temp {}", tmp.display()))?;
      std::fs::rename(&tmp, path).with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
      Ok(())
  }

  fn secs_to_systemtime(secs: Option<u64>) -> Option<SystemTime> {
      secs.map(|s| UNIX_EPOCH + Duration::from_secs(s))
  }

  /// Read the cache. None on missing/corrupt — never errors.
  pub fn read_cache(path: &Path) -> Option<QuotaReading> {
      let body = std::fs::read_to_string(path).ok()?;
      let cache: CacheFile = serde_json::from_str(&body).ok()?;
      Some(QuotaReading {
          five_hour: Window { used_pct: cache.five_hour_pct, resets_at: secs_to_systemtime(cache.five_hour_resets_at) },
          seven_day: Window { used_pct: cache.seven_day_pct, resets_at: secs_to_systemtime(cache.seven_day_resets_at) },
          seven_day_opus: None, // the status line does not carry the opus split
          source: Provenance::StatusLine,
          observed_at: secs_to_systemtime(Some(cache.written_at)).unwrap_or_else(SystemTime::now),
      })
  }
  ```

- [ ] **Step 4: Run the test, expect PASS.** `cargo test --lib sources::statusline::tests` → all four pass.

- [ ] **Step 5: Commit.**
  ```
  git add src/sources/statusline.rs
  git commit -m "feat(statusline): atomic cache write + tolerant read round-trip"
  ```

---

### Task M5-2: `run_statusline` — stdin → cache + printed line (TDD)

`run_statusline(stdin: &str, cache_path: &Path) -> anyhow::Result<String>` writes the cache and returns the one-line status. `main.rs`'s `--statusline` branch reads stdin, calls it, and `println!`s the result.

**Files:**
- Modify: `C:\Users\oz\Desktop\claude-usage\src\statusline_cmd.rs` (replace the M2-5 stub)
- Modify: `C:\Users\oz\Desktop\claude-usage\src\main.rs` (real `--statusline` dispatch)

- [ ] **Step 1: Write the failing test.** Append to `src\statusline_cmd.rs`:
  ```rust
  #[cfg(test)]
  mod tests {
      use super::*;

      fn tmp_path(name: &str) -> std::path::PathBuf {
          let mut p = std::env::temp_dir();
          p.push(format!("cuw-cmd-{}-{}.json", std::process::id(), name));
          p
      }

      #[test]
      fn writes_cache_and_returns_status_line() {
          let stdin = r#"{"rate_limits":{
              "five_hour":{"used_percentage":72.0,"resets_at":1749412380},
              "seven_day":{"used_percentage":41.0,"resets_at":1749744000}}}"#;
          let path = tmp_path("ok");
          let _ = std::fs::remove_file(&path);
          let line = run_statusline(stdin, &path).expect("run ok");
          let reading = crate::sources::statusline::read_cache(&path).expect("cache present");
          assert_eq!(reading.five_hour.used_pct, 72.0);
          assert_eq!(reading.source, crate::model::Provenance::StatusLine);
          assert!(line.contains("5H 72%"), "line was: {line:?}");
          assert!(line.contains("7D 41%"), "line was: {line:?}");
          let _ = std::fs::remove_file(&path);
      }

      #[test]
      fn bad_stdin_errors_without_writing_cache() {
          let path = tmp_path("bad");
          let _ = std::fs::remove_file(&path);
          let res = run_statusline("not json at all", &path);
          assert!(res.is_err());
          assert!(!path.exists(), "no cache written on parse failure");
      }
  }
  ```

- [ ] **Step 2: Run the test, expect FAIL.** `cargo test --lib statusline_cmd::tests`. Expected: fails because the file currently has only `run_statusline_stub` (M2-5), not `run_statusline`.

- [ ] **Step 3: Implement `run_statusline`.** Replace the body of `src\statusline_cmd.rs` (drop the `run_statusline_stub`):
  ```rust
  use crate::sources::statusline;
  use anyhow::Result;
  use std::path::Path;

  /// `claude-usage --statusline`: parse stdin JSON, write the cache atomically,
  /// return the one-line status string to print. Zero network.
  pub fn run_statusline(stdin_json: &str, cache_path: &Path) -> Result<String> {
      statusline::write_cache_from_stdin(stdin_json, cache_path)?;
      let line = match statusline::read_cache(cache_path) {
          Some(r) => format!(
              "Claude · 5H {}% · 7D {}%",
              r.five_hour.used_pct.round() as i64,
              r.seven_day.used_pct.round() as i64
          ),
          None => "Claude · usage n/a".to_string(),
      };
      Ok(line)
  }
  ```

- [ ] **Step 4: Run the test, expect PASS.** `cargo test --lib statusline_cmd::tests` → both pass.

- [ ] **Step 5: Wire `--statusline` into `main.rs`.** Replace the M2-5 dispatch (which called `run_statusline_stub`) so `main` returns `anyhow::Result<()>` and the branch reads stdin and prints the line:
  ```rust
  fn main() -> anyhow::Result<()> {
      let args: Vec<String> = std::env::args().collect();
      if args.iter().any(|a| a == "--statusline") {
          return run_statusline_command();
      }
      run_gui();
      Ok(())
  }

  fn run_statusline_command() -> anyhow::Result<()> {
      use std::io::Read;
      let mut stdin_json = String::new();
      std::io::stdin().read_to_string(&mut stdin_json)
          .map_err(|e| anyhow::anyhow!("read stdin: {e}"))?;
      let path = crate::sources::statusline::cache_path();
      match crate::statusline_cmd::run_statusline(&stdin_json, &path) {
          Ok(line) => { println!("{line}"); Ok(()) }
          Err(e) => {
              eprintln!("claude-usage --statusline: {e:#}");
              println!("Claude · usage n/a"); // never break Claude Code's prompt
              Err(e)
          }
      }
  }
  ```
  > `run_gui()` is the M2-5 GUI bootstrap (it returns `()`; `main` returns `Ok(())` after it). Ensure `mod statusline_cmd;` and `mod sources;` are declared. Remove the now-unused `run_statusline_stub`.

- [ ] **Step 6: Build + full lib tests.** `cargo build` then `cargo test --lib`. Expected: clean build; M5-1/M5-2 tests plus all prior tests pass.

- [ ] **Step 7: Manual end-to-end verify of the helper (run + observe).** PowerShell:
  ```
  '{"rate_limits":{"five_hour":{"used_percentage":72,"resets_at":1749412380},"seven_day":{"used_percentage":41,"resets_at":1749744000}}}' | cargo run -- --statusline
  ```
  Observe: stdout prints `Claude · 5H 72% · 7D 41%`; `~/.claude/widget-cache/ratelimits.json` now exists (`Get-Content $env:USERPROFILE\.claude\widget-cache\ratelimits.json`) with `five_hour_pct: 72.0`, a `written_at`, and no `.tmp` sibling.

- [ ] **Step 8: Commit.**
  ```
  git add src/statusline_cmd.rs src/main.rs
  git commit -m "feat(statusline): run_statusline writes cache and prints status line; wire --statusline dispatch"
  ```

---

### Task M5-3: `reconcile` status-line preference — boundary tests (TDD)

`reconcile`'s body is final from M2-1. This task adds boundary coverage (exact-max-age freshness, no-statusline path) to lock the §3.1 preference.

**Files:**
- Modify: `C:\Users\oz\Desktop\claude-usage\src\sources\mod.rs` (tests only)

- [ ] **Step 1: Write the tests.** Append `#[cfg(test)] mod m5_reconcile_tests` to `src\sources\mod.rs`:
  ```rust
  #[cfg(test)]
  mod m5_reconcile_tests {
      use super::*;
      use crate::model::{Provenance, Window};
      use std::time::{Duration, SystemTime, UNIX_EPOCH};

      fn at(secs: u64) -> SystemTime { UNIX_EPOCH + Duration::from_secs(secs) }
      fn reading(pct: f32, src: Provenance, observed: SystemTime) -> QuotaReading {
          QuotaReading {
              five_hour: Window { used_pct: pct, resets_at: None },
              seven_day: Window { used_pct: pct / 2.0, resets_at: None },
              seven_day_opus: None, source: src, observed_at: observed,
          }
      }
      const MAX_AGE: Duration = Duration::from_secs(120);

      #[test]
      fn no_statusline_uses_oauth() {
          let (chosen, prov) = reconcile(None, Some(reading(40.0, Provenance::OAuth, at(995))), None, at(1000), MAX_AGE).expect("some");
          assert_eq!(chosen.five_hour.used_pct, 40.0);
          assert_eq!(prov, Provenance::OAuth);
      }

      #[test]
      fn both_fail_degrades_last_good_to_stale() {
          let (chosen, prov) = reconcile(None, None, Some(reading(55.0, Provenance::OAuth, at(800))), at(1000), MAX_AGE).expect("some");
          assert_eq!(chosen.five_hour.used_pct, 55.0);
          assert_eq!(prov, Provenance::Stale { last_good_at: at(800) });
      }

      #[test]
      fn statusline_exactly_at_max_age_is_still_fresh() {
          let sl = reading(72.0, Provenance::StatusLine, at(1000 - 120)); // == max age
          let oa = reading(40.0, Provenance::OAuth, at(995));
          let (_chosen, prov) = reconcile(Some(sl), Some(oa), None, at(1000), MAX_AGE).expect("some");
          assert_eq!(prov, Provenance::StatusLine);
      }
  }
  ```

- [ ] **Step 2: Run the tests, expect PASS immediately.** `cargo test --lib sources::m5_reconcile_tests`. Because the M2-1 body uses `age <= statusline_max_age` (inclusive), all three pass without code changes. (If `statusline_exactly_at_max_age_is_still_fresh` fails, the M2-1 comparison used `<` instead of `<=` — fix it to `<=`.)

- [ ] **Step 3: Commit.**
  ```
  git add src/sources/mod.rs
  git commit -m "test(reconcile): lock status-line freshness boundary and no-statusline fallback"
  ```

---

### Task M5-4: `Collector::tick` OAuth-skip — boundary tests + creds wiring (TDD)

The §3.1 net effect (zero network while coding) is already implemented in M2-3's `tick`. This task adds explicit boundary tests and confirms the production OAuth-creds reader passes the **bare** version string to `oauth::fetch` (which prepends `claude-code/`).

**Files:**
- Modify: `C:\Users\oz\Desktop\claude-usage\src\collector.rs`

- [ ] **Step 1: Write the failing/guard tests.** Append `#[cfg(test)] mod m5_oauth_skip_tests` to `src\collector.rs`. Uses the M2 seam `new_with_sources` (closures take `now`); asserts zero OAuth calls when statusline fresh, one call when stale + floor elapsed, and suppression within the floor.
  ```rust
  #[cfg(test)]
  mod m5_oauth_skip_tests {
      use super::*;
      use crate::config::Config;
      use crate::model::{Provenance, Window};
      use crate::sources::QuotaReading;
      use std::cell::Cell;
      use std::rc::Rc;
      use std::time::{Duration, SystemTime, UNIX_EPOCH};

      fn at(secs: u64) -> SystemTime { UNIX_EPOCH + Duration::from_secs(secs) }
      fn sl(observed: SystemTime) -> QuotaReading {
          QuotaReading { five_hour: Window { used_pct: 72.0, resets_at: None }, seven_day: Window { used_pct: 41.0, resets_at: None }, seven_day_opus: None, source: Provenance::StatusLine, observed_at: observed }
      }
      fn oa(observed: SystemTime) -> QuotaReading {
          QuotaReading { five_hour: Window { used_pct: 12.0, resets_at: None }, seven_day: Window { used_pct: 6.0, resets_at: None }, seven_day_opus: None, source: Provenance::OAuth, observed_at: observed }
      }

      #[test]
      fn fresh_statusline_means_no_oauth_call() {
          let calls = Rc::new(Cell::new(0u32));
          let c2 = calls.clone();
          let mut c = Collector::new_with_sources(
              Config::default(),
              Box::new(move |_now| Some(sl(at(10_000 - 10)))), // 10s old → fresh
              Box::new(move |now| { c2.set(c2.get() + 1); Some(oa(now)) }),
              Box::new(|_now| crate::model::TokenStats::default()),
          );
          let snap = c.tick(at(10_000));
          assert_eq!(calls.get(), 0);
          assert_eq!(snap.source, Provenance::StatusLine);
          assert_eq!(snap.five_hour.used_pct, 72.0);
      }

      #[test]
      fn stale_statusline_triggers_oauth_when_floor_elapsed() {
          let calls = Rc::new(Cell::new(0u32));
          let c2 = calls.clone();
          let mut c = Collector::new_with_sources(
              Config::default(), // quota_poll_secs 180
              Box::new(move |_now| Some(sl(at(10_000 - 600)))), // 600s old → stale
              Box::new(move |now| { c2.set(c2.get() + 1); Some(oa(now)) }),
              Box::new(|_now| crate::model::TokenStats::default()),
          );
          let snap = c.tick(at(10_000));
          assert_eq!(calls.get(), 1);
          assert_eq!(snap.source, Provenance::OAuth);
          assert_eq!(snap.five_hour.used_pct, 12.0);
      }

      #[test]
      fn oauth_floor_suppresses_repeat_polls() {
          let calls = Rc::new(Cell::new(0u32));
          let c2 = calls.clone();
          let mut c = Collector::new_with_sources(
              Config::default(),
              Box::new(|_now| None),
              Box::new(move |now| { c2.set(c2.get() + 1); Some(oa(now)) }),
              Box::new(|_now| crate::model::TokenStats::default()),
          );
          let _ = c.tick(at(10_000));
          let snap = c.tick(at(10_030)); // within 180s floor
          assert_eq!(calls.get(), 1);
          assert_eq!(snap.source, Provenance::OAuth);
      }
  }
  ```

- [ ] **Step 2: Run the tests.** `cargo test --lib collector::m5_oauth_skip_tests`. Expected: all three PASS using the M2-3 `tick` as-is. (If any fail, the M2-3 OAuth-skip/floor logic regressed — fix `tick`, not the tests.)

- [ ] **Step 3: Confirm the production creds reader passes the BARE version.** Verify `src\collector.rs`'s `claude_code_version()` returns `"2.1.16"` (bare) and `Collector::new` calls `sources::oauth::fetch(&token, &version)` — NOT a pre-formatted `claude-code/...`. `fetch` (M1-5) prepends `claude-code/`. If M2-3 accidentally pre-formatted the UA, fix it to pass the bare version. No code change if already correct.

- [ ] **Step 4: Commit.**
  ```
  git add src/collector.rs
  git commit -m "test(collector): lock OAuth-skip-on-fresh-statusline and poll-floor boundaries"
  ```

---

### Task M5-5: Manual verify — live update while a session runs (run + observe)

Proves the §3.1 net effect end-to-end: with the helper feeding the cache, the GUI updates from local data and makes no OAuth calls.

**Files:** (none — integration gate.)

- [ ] **Step 1: Seed a fresh cache, then launch the GUI.** PowerShell:
  ```
  '{"rate_limits":{"five_hour":{"used_percentage":83,"resets_at":1749412380},"seven_day":{"used_percentage":52,"resets_at":1749744000}}}' | cargo run -- --statusline
  cargo run
  ```
  Observe: the widget opens (Mica, topmost) and **5H reads 83%, 7D reads 52%**, provenance shows the status-line source (`·live`), not `·oauth`.

- [ ] **Step 2: Rewrite the cache while the GUI stays open.** In a second terminal:
  ```
  '{"rate_limits":{"five_hour":{"used_percentage":91,"resets_at":1749412380},"seven_day":{"used_percentage":52,"resets_at":1749744000}}}' | cargo run -- --statusline
  ```
  Observe: within ~`refresh_secs` (30 s) the 5H reading climbs to **91%** and flips to **red** (Critical, ≥90). No flicker/blank/crash. Provenance stays status-line.

- [ ] **Step 3: Verify stale fallback.** Stop writing the cache and wait > `statusline_max_age_secs` (120 s). Observe: provenance switches to `·oauth` (a real poll fires, respecting the 180 s floor) or, if OAuth also fails (offline), a **dim Stale** dot with last-good numbers — never a blank widget.

- [ ] **Step 4:** No code change → no commit (do not create an empty commit).

---

### Task M5-6: Optional `settings.json` registration helper (TDD core + manual apply)

The user's `~/.claude/settings.json` has **no** `statusLine` key, so registration is a clean insert that must preserve all other keys. The pure merge is unit-tested; the side-effecting wrapper is manual-verified.

**Files:**
- Create: `C:\Users\oz\Desktop\claude-usage\src\statusline_register.rs`
- Modify: `C:\Users\oz\Desktop\claude-usage\src\main.rs` (declare `mod statusline_register;` + `--register-statusline` branch)

- [ ] **Step 1: Write the file with the pure merge + tests.** Create `src\statusline_register.rs`:
  ```rust
  use anyhow::{Context, Result};
  use std::path::{Path, PathBuf};

  /// ~/.claude/settings.json
  pub fn settings_path() -> PathBuf {
      let mut p = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
      p.push(".claude");
      p.push("settings.json");
      p
  }

  /// Insert/replace the `statusLine` block, preserving every other key. Pure.
  pub fn merge_statusline(existing_json: &str, exe_path: &str) -> Result<String> {
      let mut root: serde_json::Value = if existing_json.trim().is_empty() {
          serde_json::json!({})
      } else {
          serde_json::from_str(existing_json).context("parse settings.json")?
      };
      let obj = root.as_object_mut().context("settings.json root is not an object")?;
      obj.insert(
          "statusLine".to_string(),
          serde_json::json!({ "type": "command", "command": format!("{exe_path} --statusline"), "padding": 0 }),
      );
      serde_json::to_string_pretty(&root).context("serialize settings.json")
  }

  /// Side-effecting: read settings (or start empty), merge, write back.
  pub fn register(settings: &Path, exe_path: &str) -> Result<()> {
      let existing = std::fs::read_to_string(settings).unwrap_or_default();
      let merged = merge_statusline(&existing, exe_path)?;
      if let Some(dir) = settings.parent() { std::fs::create_dir_all(dir).ok(); }
      std::fs::write(settings, merged).with_context(|| format!("write {}", settings.display()))?;
      Ok(())
  }

  #[cfg(test)]
  mod tests {
      use super::*;

      #[test]
      fn inserts_statusline_preserving_other_keys() {
          let out = merge_statusline(r#"{ "theme": "dark", "model": "opus" }"#, r"C:\bin\claude-usage.exe").unwrap();
          let v: serde_json::Value = serde_json::from_str(&out).unwrap();
          assert_eq!(v["theme"], "dark");
          assert_eq!(v["model"], "opus");
          assert_eq!(v["statusLine"]["type"], "command");
          assert_eq!(v["statusLine"]["command"], r"C:\bin\claude-usage.exe --statusline");
      }

      #[test]
      fn empty_input_produces_valid_settings() {
          let out = merge_statusline("", r"C:\bin\claude-usage.exe").unwrap();
          let v: serde_json::Value = serde_json::from_str(&out).unwrap();
          assert_eq!(v["statusLine"]["type"], "command");
          assert_eq!(v.as_object().unwrap().len(), 1);
      }

      #[test]
      fn replaces_existing_statusline() {
          let out = merge_statusline(r#"{ "statusLine": { "type": "command", "command": "old" }, "x": 1 }"#, r"C:\bin\claude-usage.exe").unwrap();
          let v: serde_json::Value = serde_json::from_str(&out).unwrap();
          assert_eq!(v["x"], 1);
          assert_eq!(v["statusLine"]["command"], r"C:\bin\claude-usage.exe --statusline");
      }
  }
  ```

- [ ] **Step 2: Run the test, expect FAIL.** `cargo test --lib statusline_register::tests`. Expected: compile error — `mod statusline_register;` not yet declared. Add it in Step 3, then the tests go green.

- [ ] **Step 3: Declare the module + add the CLI dispatch in `main.rs`.** Add `mod statusline_register;` to the module list, and extend the arg dispatch:
  ```rust
  fn main() -> anyhow::Result<()> {
      let args: Vec<String> = std::env::args().collect();
      if args.iter().any(|a| a == "--statusline") {
          return run_statusline_command();
      }
      if args.iter().any(|a| a == "--register-statusline") {
          return register_statusline_command();
      }
      run_gui();
      Ok(())
  }

  fn register_statusline_command() -> anyhow::Result<()> {
      let exe = std::env::current_exe().map_err(|e| anyhow::anyhow!("current_exe: {e}"))?;
      let exe_str = exe.to_string_lossy().to_string();
      let path = crate::statusline_register::settings_path();
      crate::statusline_register::register(&path, &exe_str)?;
      println!("Registered status line: {} --statusline -> {}", exe_str, path.display());
      Ok(())
  }
  ```

- [ ] **Step 4: Run the test, expect PASS.** `cargo test --lib statusline_register::tests` → 3 pass; `cargo build` clean.

- [ ] **Step 5: Manual verify the registration (run + observe).** PowerShell:
  ```
  Copy-Item $env:USERPROFILE\.claude\settings.json $env:USERPROFILE\.claude\settings.json.bak -ErrorAction SilentlyContinue
  cargo run -- --register-statusline
  Get-Content $env:USERPROFILE\.claude\settings.json
  ```
  Observe: stdout prints the `Registered status line: …` confirmation; `settings.json` now has a `statusLine` block whose `command` ends in `--statusline`, with all previously-present keys intact (diff against `.bak`). Optionally restore: `Move-Item -Force $env:USERPROFILE\.claude\settings.json.bak $env:USERPROFILE\.claude\settings.json`.

- [ ] **Step 6: Commit.**
  ```
  git add src/statusline_register.rs src/main.rs
  git commit -m "feat(statusline): optional settings.json registration helper (--register-statusline)"
  ```

---

### Exit criteria for M5

- [ ] `claude-usage --statusline` reads session JSON from stdin, writes `~/.claude/widget-cache/ratelimits.json` **atomically** (temp + rename, no leftover `.tmp`), and prints a one-line status; `run_statusline` is unit-tested and `read_cache`/`write_cache_from_stdin` round-trip (Unix-seconds `resets_at` → `SystemTime`).
- [ ] `reconcile` boundary tests lock fresh-status-line preference (inclusive of exact `statusline_max_age`), OAuth fallback, and stale degrade.
- [ ] `Collector::tick` boundary tests confirm **zero** OAuth calls when the status-line cache is fresh, one poll when stale + 180 s floor elapsed, and suppression within the floor; the creds reader passes the bare version to `oauth::fetch`.
- [ ] Manual: rewriting the cache via `--statusline` updates the live GUI within one `refresh_secs` (incl. color flip), provenance shows the status-line source, no network call while fresh; after `statusline_max_age_secs` it falls back cleanly.
- [ ] Optional `--register-statusline` cleanly inserts the `statusLine` block into `~/.claude/settings.json`, preserving other keys (`merge_statusline` unit-tested; `register` manually verified).
- [ ] `cargo build` and `cargo test --lib` green; each task ends in a conventional-commit.

---

## Milestone M6: Polish & packaging

**Goal:** Make the widget robust and shippable — a Win10/Win11 backdrop fallback chain, a legibility scrim behind the numbers, DPI validation at 100/150/200 %, and a single-`.exe` distribution with an embedded icon plus a README documenting build prereqs and setup.

**Verification kind: manual-verify.** GUI/Win32/packaging surface that `cargo test` cannot exercise. Each task replaces the TDD cycle with a concrete run + observe step, and still ends in a commit. The pure backdrop-fallback mapping is the one testable piece and keeps an inline unit test.

> Prerequisite (M0–M5): `src/win/topmost.rs` has `apply_topmost`; `src/ui/theme.rs` maps `Level → color`; `src/ui/bars.rs` and `src/ui/gauge.rs` render a live `UsageSnapshot`; `src/config.rs` round-trips `Backdrop`. M6 hardens and packages — no new model types. **M6-1 creates `src/win/backdrop.rs`** (declared `pub mod backdrop;` in `src/win/mod.rs`, which M0 left as a `// backdrop added in M6` comment) and delivers `appearance_for` (referenced by M4-2/M4-3/M2-5).

---

### Task M6-1: Win10 backdrop fallback chain (Mica → Acrylic → Transparent → Opaque)

`win/backdrop.rs` maps the configured `Backdrop` + the detected Windows build to a concrete `WindowBackgroundAppearance`, never panicking. Mica is Win11-only (build ≥ 22000; MicaAlt ≥ 22621); older builds fall back to acrylic blur, then transparent, then opaque.

**Files:**
- Create: `C:\Users\oz\Desktop\claude-usage\src\win\backdrop.rs`
- Modify: `C:\Users\oz\Desktop\claude-usage\src\win\mod.rs` (add `pub mod backdrop;`)
- Modify: `C:\Users\oz\Desktop\claude-usage\src\main.rs` (consume the chosen appearance in `WindowOptions`)

> **Variant-name reconciliation (critical):** M0 used `WindowBackgroundAppearance::MicaBackdrop` in `WindowOptions`. The four right-hand sides below (`Mica`/`Blurred`/`Transparent`/`Opaque`) must match the **exact** variant spellings of the gpui rev pinned in M0. If gpui uses the `*Backdrop` suffix (e.g. `MicaBackdrop`, `BlurredBackdrop`), rename the four mapped values here and nowhere else — the mapping logic is unaffected. Whatever spelling M0's `MicaBackdrop` literal used is authoritative.

- [ ] **Step 1: Declare the module + add the build detector and pure chooser.** Change `src\win\mod.rs` to:
  ```rust
  pub mod topmost;
  pub mod backdrop;
  ```
  Create `src\win\backdrop.rs`:
  ```rust
  //! Choose the concrete window backdrop appearance from Config + the running
  //! Windows build, with a Win10 fallback chain. All unsafe/Win32 confined here.

  use crate::config::Backdrop;
  use gpui::WindowBackgroundAppearance;

  const WIN11_MICA_MIN_BUILD: u32 = 22000;
  const WIN11_MICA_ALT_MIN_BUILD: u32 = 22621;

  /// Current OS build number (e.g. 26200). 0 if it cannot be read.
  pub fn os_build() -> u32 {
      use windows::Win32::Foundation::NTSTATUS;
      #[repr(C)]
      struct OsVersionInfoW {
          dw_os_version_info_size: u32,
          dw_major_version: u32,
          dw_minor_version: u32,
          dw_build_number: u32,
          dw_platform_id: u32,
          sz_csd_version: [u16; 128],
      }
      #[link(name = "ntdll")]
      unsafe extern "system" {
          fn RtlGetVersion(lp: *mut OsVersionInfoW) -> NTSTATUS;
      }
      let mut info = OsVersionInfoW {
          dw_os_version_info_size: core::mem::size_of::<OsVersionInfoW>() as u32,
          dw_major_version: 0, dw_minor_version: 0, dw_build_number: 0,
          dw_platform_id: 0, sz_csd_version: [0u16; 128],
      };
      // SAFETY: info is correctly sized and fully initialized; RtlGetVersion only writes into it.
      unsafe { let _ = RtlGetVersion(&mut info); }
      info.dw_build_number
  }

  /// Map the configured backdrop to an appearance the running build can render.
  /// Pure. Fallback: Mica → Blurred(acrylic) → Transparent → Opaque.
  pub fn choose_appearance(backdrop: Backdrop, build: u32) -> WindowBackgroundAppearance {
      match backdrop {
          Backdrop::Mica | Backdrop::MicaAlt if build >= mica_min_build(backdrop) => {
              WindowBackgroundAppearance::Mica
          }
          Backdrop::Mica | Backdrop::MicaAlt | Backdrop::Acrylic => {
              WindowBackgroundAppearance::Blurred
          }
          Backdrop::Transparent => WindowBackgroundAppearance::Transparent,
          Backdrop::Opaque => WindowBackgroundAppearance::Opaque,
      }
  }

  fn mica_min_build(backdrop: Backdrop) -> u32 {
      match backdrop {
          Backdrop::MicaAlt => WIN11_MICA_ALT_MIN_BUILD,
          _ => WIN11_MICA_MIN_BUILD,
      }
  }

  /// Read the live OS build and choose for the configured backdrop.
  pub fn appearance_for(backdrop: Backdrop) -> WindowBackgroundAppearance {
      choose_appearance(backdrop, os_build())
  }
  ```
  > The `RtlGetVersion` import path: in newer `windows` crates `NTSTATUS` is under `Win32_Foundation` (already a feature in the dependency block). Confirm against `cargo doc -p windows` for the locked version and adjust only the `use` line if needed.

- [ ] **Step 2: Add the pure unit test (fallback mapping).** Append to `src\win\backdrop.rs`:
  ```rust
  #[cfg(test)]
  mod tests {
      use super::*;
      use crate::config::Backdrop;
      use gpui::WindowBackgroundAppearance;

      #[test]
      fn mica_on_win11_stays_mica() {
          assert_eq!(choose_appearance(Backdrop::Mica, 26200), WindowBackgroundAppearance::Mica);
      }
      #[test]
      fn mica_on_win10_falls_back_to_blurred() {
          assert_eq!(choose_appearance(Backdrop::Mica, 19045), WindowBackgroundAppearance::Blurred);
      }
      #[test]
      fn mica_alt_needs_22621() {
          assert_eq!(choose_appearance(Backdrop::MicaAlt, 22000), WindowBackgroundAppearance::Blurred);
          assert_eq!(choose_appearance(Backdrop::MicaAlt, 22621), WindowBackgroundAppearance::Mica);
      }
      #[test]
      fn opaque_and_transparent_are_passthrough() {
          assert_eq!(choose_appearance(Backdrop::Opaque, 19045), WindowBackgroundAppearance::Opaque);
          assert_eq!(choose_appearance(Backdrop::Transparent, 19045), WindowBackgroundAppearance::Transparent);
      }
  }
  ```

- [ ] **Step 3: Run the pure test.** `cargo test --lib win::backdrop` → `4 passed`. If `WindowBackgroundAppearance` variants differ, the compile error here is the signal to apply the rename from the variant-name reconciliation note.

- [ ] **Step 4: Wire the chooser into `main.rs`.** Replace the M0/M2 literal `window_background:` with the config-driven, build-aware choice:
  ```rust
  let window_background = crate::win::backdrop::appearance_for(config.backdrop);
  // ... in WindowOptions { window_background, .. }
  ```
  Reuse the single `config` value already loaded for the poller/position (do not call `Config::load()` twice).

- [ ] **Step 5: Run + observe — Win11 path.** `cargo run --release`. Observe: the window opens with the **Mica frosted backdrop** (wallpaper tint shows through; no solid black rectangle; no border/title bar). Expected on this machine (build 26200).

- [ ] **Step 6: Run + observe — forced fallback.** Temporarily change the line to `let window_background = crate::win::backdrop::choose_appearance(config.backdrop, 19045);`, then `cargo run --release`. Observe: the window renders with **acrylic blur** (heavier blur than Mica, still translucent — not opaque, not black). Then **revert** to `appearance_for(config.backdrop)` and re-run to confirm Mica returns.

- [ ] **Step 7: Commit.**
  ```
  git add src/win/backdrop.rs src/win/mod.rs src/main.rs
  git commit -m "feat(backdrop): Win10/11 fallback chain Mica->Acrylic->Transparent->Opaque"
  ```

---

### Task M6-2: Legibility scrim behind the numbers

Translucent Mica washes out small text (spec §7.3). Mitigate with a faint rounded scrim card behind the numbers in both views plus high-contrast text. `ui/theme.rs` owns the helper.

> **Reconciliation:** M2 already added `scrim_color()` (a translucent fill used by `bars.rs`). M6-2 adds `scrim()` (the card fill, ~45% black) and `on_scrim_text()` (high-contrast fg). To avoid duplication, treat `scrim()` as the canonical card fill and have `bars.rs` switch from `scrim_color()` to `scrim()` here; keep `scrim_color()` only if still referenced elsewhere (otherwise delete it to avoid a dead-code warning). The gauge gets the same card.

**Files:**
- Modify: `C:\Users\oz\Desktop\claude-usage\src\ui\theme.rs`
- Modify: `C:\Users\oz\Desktop\claude-usage\src\ui\bars.rs`
- Modify: `C:\Users\oz\Desktop\claude-usage\src\ui\gauge.rs`

- [ ] **Step 1: Add `scrim()` + `on_scrim_text()` to `ui/theme.rs`.** Append (reuse the existing `use gpui::{hsla, Hsla};` — do not duplicate):
  ```rust
  /// Faint rounded dark scrim behind numbers/labels for legibility over a
  /// translucent Mica/acrylic backdrop. ~45% black.
  pub fn scrim() -> Hsla {
      hsla(0.0, 0.0, 0.0, 0.45)
  }

  /// High-contrast foreground for text on top of `scrim()`.
  pub fn on_scrim_text() -> Hsla {
      hsla(0.0, 0.0, 0.96, 1.0)
  }
  ```

- [ ] **Step 2: Apply the scrim card to the bars outer container.** In `src\ui\bars.rs`, on the outer `div()` returned by `render_bars`, switch the background to `theme::scrim()` and set the text color (replace the M2 `.bg(scrim_color())` + `.text_color(gpui::white())` lines):
  ```rust
  use crate::ui::theme;
  use gpui::px;
  // outer container:
  //   .rounded(px(10.0))
  //   .bg(theme::scrim())
  //   .text_color(theme::on_scrim_text())
  ```
  Update the `use` line that imported `scrim_color` (remove it if no longer used). Keep `level_color` imported.

- [ ] **Step 3: Apply the same scrim card to the gauge outer container.** In `src\ui\gauge.rs`, on the outer `div()` returned by `render_gauge`:
  ```rust
  use crate::ui::theme;
  use gpui::px;
  //   .rounded(px(12.0))
  //   .bg(theme::scrim())
  //   .text_color(theme::on_scrim_text())
  ```
  > Confirm the rounding builder name against the M0 pin: `.rounded(px(..))` on recent gpui; if the pinned rev only exposes `.rounded_md()`/`.rounded_lg()`, substitute the nearest preset. `.bg(..)`/`.text_color(..)` are stable.

- [ ] **Step 4: Run + observe — bars legibility.** `cargo run --release`. Observe: a faint rounded dark card sits behind the 5H/7D percentages; the numbers and `resets …` text are crisp and high-contrast; the Mica tint is still visible around the card edges.

- [ ] **Step 5: Run + observe — gauge legibility.** Right-click → Toggle view. Observe: the two rings sit on the same rounded scrim card; the center `72%`/`41%` labels and the countdown row are legible. Toggle back and forth to confirm both views carry the scrim.

- [ ] **Step 6: Commit.**
  ```
  git add src/ui/theme.rs src/ui/bars.rs src/ui/gauge.rs
  git commit -m "feat(ui): legibility scrim card behind numbers in bars and gauge"
  ```

---

### Task M6-3: DPI validation at 100 / 150 / 200 % scaling

Pure manual validation, recorded so the milestone has an explicit DPI gate. If text clips/blurs, the fix is a base-font bump in `ui/theme.rs`.

**Files:**
- Modify (only if a DPI defect is found): `C:\Users\oz\Desktop\claude-usage\src\ui\theme.rs` (+ `bars.rs`/`gauge.rs`)

- [ ] **Step 1: Build the release binary.** `cargo build --release` → `target\release\claude-usage-widget.exe`.

- [ ] **Step 2: Observe at 100 %.** Windows Settings → System → Display → Scale = 100 %, run `.\target\release\claude-usage-widget.exe`. Observe: both views render fully inside the window (no clipped numbers, no overlapping bars/rings); text sharp; scrim card edges crisp.

- [ ] **Step 3: Observe at 150 %.** Scale = 150 %, relaunch. Observe: the widget scales up proportionally, stays borderless and frosted, percentages remain centered within bars/rings without truncation.

- [ ] **Step 4: Observe at 200 %.** Scale = 200 %, relaunch. Observe: still legible and non-clipped; Mica still renders (no opaque fallback); topmost still holds. Restore your preferred scale afterward.

- [ ] **Step 5 (only if a defect was observed): bump the base font and re-check.** Add to `src\ui\theme.rs`:
  ```rust
  use gpui::{px, Pixels};
  /// Base font size for widget text; larger than gpui's default to keep glyphs
  /// crisp over translucent backdrops (spec §7.3 mitigation a).
  pub fn base_font_size() -> Pixels { px(14.0) }
  ```
  Then add `.text_size(theme::base_font_size())` to the outer scrim `div()` in both `bars.rs` and `gauge.rs`. Re-run Steps 2–4.

- [ ] **Step 6: Commit.** If Step 5 changed files:
  ```
  git add src/ui/theme.rs src/ui/bars.rs src/ui/gauge.rs
  git commit -m "fix(ui): bump base font size for DPI legibility on frosted backdrop"
  ```
  If no code changed (DPI passed as-is), record the checkpoint:
  ```
  git commit --allow-empty -m "test: validate DPI legibility at 100/150/200 percent scaling"
  ```

---

### Task M6-4: Single-`.exe` packaging with embedded icon

Confirm the icon is wired through the Windows resource (M0 added `build.rs` + `app.rc` + `assets/icon.ico`), verify the release profile produces one lean binary, and prove the icon shows in Explorer.

**Files:**
- Verify: `C:\Users\oz\Desktop\claude-usage\build.rs`, `C:\Users\oz\Desktop\claude-usage\app.rc`, `C:\Users\oz\Desktop\claude-usage\assets\icon.ico`
- Create: `C:\Users\oz\Desktop\claude-usage\tests\fixtures\statusline_stdin.json` (for Step 6)

- [ ] **Step 1: Confirm the resource script + build.rs (from M0-3) are intact.** `app.rc` contains `1 ICON "assets/icon.ico"`; `build.rs` calls `embed_resource::compile("app.rc", embed_resource::NONE)` under `#[cfg(target_os = "windows")]`. (These were standardized in M0-3 to `app.rc` + `assets/icon.ico` — no rename needed here.)

- [ ] **Step 2: Confirm a real `.ico` is present.** `Test-Path C:\Users\oz\Desktop\claude-usage\assets\icon.ico` → `True`. If `False`, add a multi-resolution `.ico` (16/32/48/256 px) at that path.

- [ ] **Step 3: Build the release single-exe.** `cargo build --release`. Expected: clean build (the `[profile.release] lto = true, strip = true` from the dependency block is already in `Cargo.toml`). Output: `target\release\claude-usage-widget.exe`.

- [ ] **Step 4: Run + observe — self-contained exe with icon.** `.\target\release\claude-usage-widget.exe`. Observe:
  1. Launches with **no console window** and no missing-DLL error (self-contained; OS-only dependency).
  2. The **taskbar button shows the embedded icon** (not the generic default).
  3. In Explorer, `target\release\claude-usage-widget.exe` shows the **same icon**.
  Note the size: `(Get-Item .\target\release\claude-usage-widget.exe).Length / 1MB` — expect ~12 MB+ (spec §10), confirming `strip`/`lto` ran.

- [ ] **Step 5: Create a statusline fixture for Step 6.** Create `tests\fixtures\statusline_stdin.json`:
  ```json
  {"rate_limits":{"five_hour":{"used_percentage":72,"resets_at":1749412380},"seven_day":{"used_percentage":41,"resets_at":1749744000}}}
  ```

- [ ] **Step 6: Confirm `--statusline` works from the packaged binary.** PowerShell:
  ```
  Get-Content .\tests\fixtures\statusline_stdin.json | .\target\release\claude-usage-widget.exe --statusline
  ```
  Observe: it prints a one-line status (e.g. `Claude · 5H 72% · 7D 41%`) and writes `~/.claude/widget-cache/ratelimits.json` — proving the same single exe serves the GUI and the status-line helper.

- [ ] **Step 7: Commit.**
  ```
  git add build.rs app.rc assets/icon.ico tests/fixtures/statusline_stdin.json
  git commit -m "build: verify single-exe release packaging with embedded icon"
  ```

---

### Task M6-5: README — build prerequisites and setup

**Files:**
- Create: `C:\Users\oz\Desktop\claude-usage\README.md`

- [ ] **Step 1: Write the README.** Create `C:\Users\oz\Desktop\claude-usage\README.md`:
  ```markdown
  # claude-usage-widget

  A small, always-on-top Windows 11 desktop widget that shows your live Claude
  usage limits at a glance: the **5-hour** rolling window (primary) and the
  **7-day** window (secondary), color-coded green/amber/red, in a frosted-glass
  (Mica) borderless window. Secondary local token stats (today's tokens,
  per-model split, live tokens/min) come from your `~/.claude` JSONL history.

  Personal, single-user tool. Windows 11 (build 26200) is the target; Windows 10
  degrades the backdrop gracefully (Acrylic → Transparent → Opaque).

  ## Build prerequisites

  - **Rust** toolchain `1.95.0` (pinned in `rust-toolchain.toml`, edition 2024).
    `rustup` auto-installs the pinned toolchain on first build.
  - **Visual Studio 2022** with the **"Desktop development with C++"** workload
    (MSVC toolset) — required to build gpui from source.
  - **CMake** on `PATH` — also required by gpui's native dependencies.
  - Git (the gpui / gpui-component dependencies are git-only, with pinned revs).

  The first build is heavy (gpui compiles from source) and the binary is ~12 MB.
  `Cargo.lock` is committed: build reproducibly and treat dependency upgrades as
  deliberate events — **never** run `cargo update`.

  ## Build & run

  ```powershell
  cargo build --release
  .\target\release\claude-usage-widget.exe
  ```

  Zero configuration: it reads the OAuth token in `~/.claude/.credentials.json`
  and your local `~/.claude/projects` JSONL files. Right-click for the menu
  (toggle view, backdrop, refresh, scale, quit). Left-drag to move. Position,
  scale, view mode, backdrop, opacity, and color thresholds persist to
  `%APPDATA%\claude-usage\widget-config.json`.

  ## Optional: live updates via the Claude Code status line

  By default the widget polls the OAuth usage endpoint (no more than every
  180 s). For **live, zero-network** updates while coding, register the bundled
  helper. Run it once:

  ```powershell
  .\target\release\claude-usage-widget.exe --register-statusline
  ```

  This inserts a `statusLine` block into `~/.claude/settings.json` (preserving
  all other keys):

  ```json
  {
    "statusLine": {
      "type": "command",
      "command": "C:\\Users\\oz\\Desktop\\claude-usage\\target\\release\\claude-usage-widget.exe --statusline"
    }
  }
  ```

  Claude Code then pipes each session's JSON to the helper after every assistant
  message; the helper writes `~/.claude/widget-cache/ratelimits.json` atomically
  and prints a one-line status. The widget prefers this cache when fresher than
  ~2 minutes, otherwise falls back to OAuth. Registration is optional — without
  it the widget always uses OAuth.

  ## Configuration

  All fields optional with sane defaults; forward-compatible (extra/missing
  fields tolerated). Defaults:

  | Field                     | Default          |
  |---------------------------|------------------|
  | `scale`                   | `1.0` (0.6–2.0)  |
  | `view_mode`               | `Bars`           |
  | `backdrop`                | `Mica`           |
  | `opacity`                 | `1.0`            |
  | `warn_threshold`          | `70.0`           |
  | `critical_threshold`      | `90.0`           |
  | `refresh_secs`            | `30`             |
  | `quota_poll_secs`         | `180`            |
  | `statusline_max_age_secs` | `120`            |

  ## Notes & limitations

  - The OAuth usage endpoint and status-line JSON shape are **undocumented and
    version-tied** (verified against Claude Code 2.1.16x). The widget degrades
    gracefully — on a fetch failure it keeps the last-good snapshot, marks it
    stale (a dim provenance dot), and never blanks or crashes.
  - Token totals are approximate (streaming rows de-duplicated on
    `(message_id, request_id)`); the authoritative number is the server-side
    utilization %, which the bars/rings show.
  - Windows only. The data/collector layer is portable, but `win/` and the
    backdrop are Windows-specific.
  ```
  > Update the `statusLine.command` path if the repo is cloned elsewhere; the path above matches the current working tree.

- [ ] **Step 2: Run + observe — README renders.** `Get-Content C:\Users\oz\Desktop\claude-usage\README.md -TotalCount 20`. Observe: the file exists and the first lines show the title and intro. Optionally preview the Markdown to confirm the table and JSON block render cleanly.

- [ ] **Step 3: Commit.**
  ```
  git add README.md
  git commit -m "docs: add README with build prereqs, setup, and statusline registration"
  ```

---

### Exit criteria for M6

- **Backdrop fallback:** `cargo test --lib win::backdrop` passes; on Win11 the widget renders Mica; forcing a Win10 build number renders acrylic blur (not black/opaque), proving the Mica → Acrylic → Transparent → Opaque chain. No panic when a backdrop is unsupported.
- **Legibility scrim:** both views draw a faint rounded scrim card behind the numbers; percentages and countdowns are high-contrast and crisp over the frosted backdrop.
- **DPI:** legible and non-clipped at 100/150/200 % scaling, with Mica and topmost still holding at 200 %.
- **Single-exe packaging:** `cargo build --release` produces one self-contained `target\release\claude-usage-widget.exe` (~12 MB, stripped/LTO'd) that launches with no console and no missing DLLs; the embedded icon shows in taskbar and Explorer; `--statusline` works from that same binary.
- **README:** documents MSVC + CMake + Rust 1.95.0 prerequisites, build/run, optional `--register-statusline`, and the config table.
- Each task committed (conventional-commit messages), including the DPI validation checkpoint.

---

## Plan Self-Review

### (a) Spec coverage — each spec section → implementing task(s)

| Spec § | Topic | Task(s) | Notes |
|--------|-------|---------|-------|
| §1 Purpose | 5h/7d utilization at a glance | M2-4, M2-5 (bars), M4-1 (gauge) | Headline 5H primary / 7D secondary rendered live. |
| §2 Scope (in) | Real %, reset countdowns, color, two views, Mica topmost, local token stats, config persistence | M0 (Mica+topmost), M1 (data), M2 (bars+color+countdown), M3 (token stats), M4 (gauge+chrome+persist) | Full in-scope set covered. |
| §2 Scope (out) | USD cost, heatmaps, tray, file-watching, cross-platform | — | Deliberately not implemented; §14 cost hook noted as future, not built. |
| §3.1 Quota (status-line + OAuth + reconcile) | Both sources, freshness preference, 180 s floor, stale-degrade, mandatory UA | M1-5 (oauth fetch+UA), M2-1 (reconcile), M2-3 (tick throttle/skip), M5-1/M5-2 (statusline cache+helper), M5-3/M5-4 (boundary tests) | Mandatory `User-Agent: claude-code/<version>` enforced in `oauth::fetch`; 429 risk called out repeatedly. |
| §3.2 Token detail (JSONL incremental) | Glob, exclude subagents, dedup `(message.id, requestId)`, incremental cursor, rotation, UTC day, output-weighted, live tok/min | M3-1..M3-5 (parse/ledger/cursor), M3-6 (wire into collector) | Dedup keeps final record; subagents excluded; rotation re-reads from 0; live rate < 90 s. |
| §3.3 Rejected sources | stats-cache.json, OTEL | — | Not read (startup seed from stats-cache explicitly omitted as optional/YAGNI). |
| §4 Core data model | `UsageSnapshot`/`Window`/`Level`/`Provenance`/`TokenStats` | M1-1 (model.rs), M1-4 (QuotaReading) | Exact canonical types in Shared Contracts; used verbatim everywhere. |
| §5 Architecture / module layout | Module map, isolation, single exe | File Structure section; module-by-module across M0–M6 | All 16 modules present. `win/topmost.rs` + `win/backdrop.rs` confine unsafe. |
| §6 Data flow & refresh | Detached poller, ~30 s, off-UI-thread I/O, repaint | M2-5 (spawn_poller), M4-2 (refresh-now), collector tick | `background_executor().timer`; `ureq` (chosen over reqwest per spine). |
| §7.1 Two views | Bars + gauge, same snapshot | M2-4 (bars), M4-1 (gauge), M4-2 (toggle) | Both pure renders of `UsageSnapshot`. |
| §7.2 Color thresholds | <70 green / 70–89 amber / ≥90 red, configurable | M1-1 (`Level::from_pct`), M2-2 (`level_color`) | Inclusive lower bounds tested at 70/90. |
| §7.3 Frosted glass + legibility | MicaBackdrop default, alternatives, Win10 fallback, scrim, larger font, DPI 100/150/200 | M0-4 (Mica), M6-1 (fallback chain), M6-2 (scrim), M6-3 (DPI + optional font bump) | Fallback chain Mica→Blurred→Transparent→Opaque. |
| §7.4 Interactions | Left-drag move (persist), right-click menu (toggle/backdrop/refresh/scale/opacity/quit), scroll resize | M4-2 (menu), M4-3 (drag+persist) | See gaps below re: scroll-to-resize and opacity menu item. |
| §8 Window shell & topmost | Borderless, backdrop, HWND_TOPMOST via raw-window-handle, init+Root::new wrappers | M0-4 (borderless+init+Root::new), M0-5 (topmost) | `gpui_component::init` + `Root::new` mandatory wrappers honored. |
| §9 Config & persistence | All fields, defaults, written-on-change, forward-compatible | M1-3 (Config+serde default), M4-2/M4-3 (write on change), M4-4 (round-trip + forward-compat test) | Path `%APPDATA%/claude-usage/widget-config.json`. |
| §10 Build & toolchain | 1.95.0 pin, MSVC+CMake, git-only pinned revs, commit lock, single exe + icon, ~12 MB | M0-1 (toolchain), M0-2 (pin revs+lock), M0-3 (icon), M6-4 (packaging), M6-5 (README) | `cargo update` forbidden; revs pinned from `Cargo.lock`. |
| §11 Milestones | M0–M6 risk-first | All milestones map 1:1 to spec milestone list | Order preserved. |
| §12 Testing strategy | TDD pure logic, manual GUI/Win32 | TDD in M1/M3 + mixed M2/M5; manual M0/M4/M6 | Every TDD task: failing test → FAIL → impl → PASS → commit. |
| §13 Risks & mitigations | Topmost, glass legibility/Win10, OAuth fragility, JSONL overcount, unpinned deps, format drift, build prereqs | M0-5, M6-1/M6-2, M1-5/M2-3/reconcile stale-degrade, M3 dedup, M0-2 pins, graceful-degrade throughout, M6-5 README | All seven risks have a corresponding task. |
| §14 Open questions / future | Cost opt-in, charts, macOS | — | Explicitly out of v1; not built. |
| Appendix A | On-disk references | M1-5/collector (credentials), M3 (projects jsonl), M5 (settings.json), config (paths) | Token path `~/.claude/.credentials.json → claudeAiOauth.accessToken`. |

**Gaps / deliberate deviations (called out honestly):**
1. **§7.4 scroll-to-resize:** the spec lists scroll-to-scale (0.6×–2.0×). The plan implements scale via the right-click **Scale +/−** menu items (M4-2) with the same clamp, not a scroll-wheel handler. Functionally equivalent (scale persists, clamped); a `on_scroll_wheel` handler calling `config.nudge_scale` can be added in M4-2 if literal scroll is required. Flagged, not silently dropped.
2. **§7.4 opacity menu item / §9 `opacity`:** `Config.opacity` exists, defaults to 1.0, and round-trips (M1-3/M4-4), but no menu entry sets it and the renderer does not yet apply window opacity. Low-value for v1 (Mica already translucent); the field is wired for forward-compat. To fully honor §7.4, add an "Opacity ▸" submenu in M4-2 and apply via `window.set_opacity` (or element alpha) — noted as a minor follow-up.
3. **§8 re-assert topmost on focus loss:** M0-5 applies `HWND_TOPMOST` once. The spec/§13 mention re-asserting on focus loss. The plan explicitly defers this as "a later refinement" (M0-5 Step 5 note). For a hardened v1 it should be wired (a focus-event listener re-calling `apply_topmost`); currently a known, documented soft-gap rather than an omission.
4. **§3.3 stats-cache startup seed:** the optional "read once at startup for cheap historical seed" is not implemented (YAGNI); JSONL incremental parsing fully covers today's tokens.

These four are the only spec items not 100 % implemented; each is intentional and documented, none affect the core headline (server utilization %).

### (b) Placeholder scan result

Scanned every code step for non-compilable placeholders (`...`, `TODO`, `unimplemented!()` left in shipped impls, fictional APIs):
- **`unimplemented!()` / `todo!()`** appear ONLY as the deliberate "red" state of a TDD cycle (failing-test step), always replaced by real code in the same task's impl step. None survive into a milestone's exit state. Verified for M1-1 (`from_pct`), M1-2 (timeutil), M1-3 (config seams), M1-4 (`reconcile` `todo!` → M2-1), M1-5 (oauth), M2-2 (theme), M3-1/2/3/5 (jsonl).
- **Intentional cross-milestone stubs** are explicit and replaced on schedule: `sources/oauth.rs`/`statusline.rs`/`jsonl.rs` placeholder comments (M1-4 → filled M1-5/M5/M3); `statusline.rs` minimal `cache_path`/`read_cache` stubs (M2-3 → replaced M5-1); `statusline_cmd::run_statusline_stub` (M2-5 → replaced M5-2); collector `read_tokens = TokenStats::default()` (M2-3 → replaced M3-6). Each has a forward-reference note.
- **No fictional crates/APIs in load-bearing logic.** All quota/JSONL/config/time code uses only `serde_json`, `anyhow`, `chrono`, `dirs`, `ureq`, `windows`, and std — exactly the dependency block. gpui/gpui-component call sites that may drift between revs are marked "CONFIRM against M0 example" rather than asserted (window open, `ProgressBar`/`ProgressCircle` builders, `Root::new`, `cx.spawn`, menu API, `set_background_appearance`, `start_window_move`). This is honest about the one genuinely unpinnable surface (a fast-moving git dep) instead of inventing exact signatures.

Result: **no stray placeholders**; every `unimplemented!()` is paired with its replacement in the same task.

### (c) Type-consistency result

Checked every type/function/method name against the Shared Contracts:
- **`Level::from_pct`, `Window`, `Provenance`, `TokenStats`, `UsageSnapshot`** — used verbatim across model/collector/ui/sources. `TokenStats` fields (`today_total_output`, `by_model`, `live_tok_per_min`, `top_projects`) consistent in M3 producer and M2/M4 consumers.
- **`QuotaReading { five_hour, seven_day, seven_day_opus, source, observed_at }`** — identical in `sources/mod.rs`, `oauth.rs`, `statusline.rs`, `collector.rs`, and all tests. `observed_at` (not `fetched_at`) used everywhere for freshness; snapshot uses `fetched_at`. No field-name drift.
- **`reconcile(statusline, oauth, last_good, now, statusline_max_age) -> Option<(QuotaReading, Provenance)>`** — signature identical in M1-4 decl, M2-1 impl, M5-3 tests.
- **`Collector::new(config)` + `tick(now)`** — public surface stable across M2-3, M3-6, M5-4. **Reconciled a real conflict:** the source-section drafts disagreed on the test seam — M2 used `new_with_sources` with `now`-taking closures; M5's draft introduced `with_sources` without `now` and M3's draft added struct fields + `tick_at_root`. The assembled plan standardizes on **one** seam — `new_with_sources(config, statusline_fn, oauth_fn, tokens_fn)` with all closures taking `SystemTime` — and M3-6 injects JSONL via a `tokens_closure_for(root)` factory (no `tick_at_root`, no struct-field rewrite), M5-4 reuses the same seam for its boundary tests. This removes the contradiction.
- **`timeutil`** — `parse_iso8601`, `from_unix_secs`, `from_unix_millis`, `utc_day`, `utc_day_start`, `format_countdown` all defined in M1-2/M2-4 and consumed by oauth (ISO), statusline (Unix-secs, defined in M5-1 inline), jsonl (`parse_iso8601` + `utc_day_start`), bars (`format_countdown`). **Reconciled:** M3's draft referenced `utc_day_start` which M1's draft did not export — added `utc_day_start` to M1-2 (alongside `utc_day`) so M3 consumes, never redefines, time logic.
- **`Config`** — exact field set + `ViewMode`/`Backdrop` enums consistent; mutators (`toggle_view`, `set_backdrop`, `nudge_scale`) added in M4-2 only.
- **`statusline::{cache_path, read_cache, write_cache_from_stdin}`** and **`statusline_cmd::run_statusline(stdin, path)`** — signatures match Shared Contracts; `run_statusline` finalized to take an injectable `path` (noted) for testability, production passes `cache_path()`.
- **`win::backdrop::{choose_appearance, appearance_for}`** — introduced in M6-1, referenced forward by M4-2 (`set_backdrop` live re-apply) and M2-5/M4-3 (`main.rs` window setup), with explicit "delivered in M6-1" notes so the forward reference is intentional, not dangling.
- **Asset naming** — standardized to `assets/icon.ico` + `app.rc` across M0-3 and M6-4 (removed the `assets/app.ico` drift from the M0 draft).
- **`WindowBackgroundAppearance` variants** — the M0 draft used `MicaBackdrop`; M6-1 maps to `Mica`/`Blurred`/`Transparent`/`Opaque`. A reconciliation note in M6-1 makes M0's actual literal authoritative and instructs renaming the four mapped values to match — so the plan does not assert a possibly-wrong variant spelling.

**Issues found and fixed during assembly:** (1) collector test-seam divergence across M2/M3/M5 → unified on `new_with_sources` + `tokens_closure_for`; (2) missing `utc_day_start` export → added to M1-2; (3) icon asset name drift → standardized to `icon.ico`/`app.rc`; (4) `render_bars`/`render_gauge` signature mismatch (`&Config` vs `warn/critical` args) → unified on `(snapshot, now, warn, critical, window, app)`; (5) `scrim_color` (M2) vs `scrim()` (M6) duplication → M6-2 note consolidates; (6) `fetch` User-Agent — ensured the collector passes the **bare** version since `fetch` prepends `claude-code/` (M5-4 Step 3 guard). No remaining type inconsistencies.
