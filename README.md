# claude-usage-widget

A small, always-on-top, **frosted-glass** Windows desktop widget (Rust) that shows
how close you are to your Claude usage limits — the real **5-hour** and **7-day**
rate-limit utilization, color-coded, plus local token detail.

Built in two crates so the data/logic core is testable on its own:

| Crate | What | Builds with |
|---|---|---|
| **`usage-core`** | All data + logic: quota sources, JSONL token parsing, reconciliation, the `UsageSnapshot` model, config, the status-line helper. **No GUI.** | stable Rust 1.96 — **no cmake** |
| **`usage-widget`** | The gpui + gpui-component GUI shell (window, gauges, always-on-top, Mica). Thin cap over `usage-core`. | Rust 1.96 + **CMake** + MSVC |

## Status (2026-06-08)

| Piece | State |
|---|---|
| `usage-core` — model, timeutil, config, sources (oauth/statusline/jsonl), reconcile, diagnostics, collector, statusline helper | ✅ **done, 59 tests green** |
| `cargo run --example snapshot` (reads your real `~/.claude` data) | ✅ **working** |
| `claude-usage-statusline` bin (populates the quota cache) | ✅ **working** |
| Live OAuth fetch (`--features net`) | ✅ **compiles**; not yet exercised against the live endpoint |
| `usage-widget` GUI (window, bars, menu, topmost, Mica, icon) | ✅ **builds** with pinned `Cargo.lock` (`cargo check --locked`) |

The interesting core — getting the numbers right — is covered by tests. The GUI
uses git-only gpui dependencies, so reproducible widget builds require the
committed `Cargo.lock` and `--locked`.

## Try it now (no CMake needed)

```powershell
# Run the full test suite
cargo test --manifest-path crates/usage-core/Cargo.toml

# Print a snapshot from your real local data (token detail is live;
# quota shows Stale/0% until a status-line cache exists or you add --features net)
cargo run --manifest-path crates/usage-core/Cargo.toml --example snapshot

# Build the status-line helper
cargo build --release --manifest-path crates/usage-core/Cargo.toml --bin statusline
# -> crates/usage-core/target/release/statusline.exe

# Check the widget without changing git dependency SHAs
cargo check --locked --manifest-path crates/usage-widget/Cargo.toml
```

### Wire up live quota with zero network (recommended)

Register the helper as Claude Code's status line in `~/.claude/settings.json`:

```json
{
  "statusLine": {
    "type": "command",
    "command": "C:\\Users\\oz\\Desktop\\claude-usage\\crates\\usage-core\\target\\release\\statusline.exe"
  }
}
```

After that, every assistant message refreshes `~/.claude/widget-cache/ratelimits.json`
with your **real** 5h/7d utilization, which the widget (and the `snapshot` example)
read with no network call. Re-run the snapshot example and the quota source becomes
`StatusLine` with real percentages.

## Building the GUI

1. Install prerequisites: **CMake** (`winget install Kitware.CMake`) and
   **Rust 1.96** (`rustup toolchain install 1.96.0`). MSVC "Desktop development
   with C++" is already present.
2. `cd crates/usage-widget`
3. `cargo build --release --locked`

The gpui dependency entries intentionally omit `rev` in `Cargo.toml` because
`gpui-component` depends on the same Zed repo without `rev`; mixing `zed#sha`
and `zed?rev=sha#sha` creates two incompatible `gpui` crates. The exact SHAs
are pinned by `Cargo.lock`.

## Data sources

- **Quota (the headline %)** — server-side only. Read either from the local
  status-line cache (preferred when fresh, no network) or the OAuth usage endpoint
  `GET /api/oauth/usage` (fallback; uses the token in `~/.claude/.credentials.json`).
  Reconciled in `collector::Collector::tick`.
- **Token detail** — parsed incrementally from `~/.claude/projects/*/*.jsonl`
  (per-file byte cursor, dedup on `(message.id, requestId)`, `subagents` excluded).

## Docs

- Design spec: [`docs/superpowers/specs/2026-06-08-claude-usage-widget-design.md`](docs/superpowers/specs/2026-06-08-claude-usage-widget-design.md)
- Implementation plan (M0–M6, 36 tasks): [`docs/superpowers/plans/2026-06-08-claude-usage-widget-plan.md`](docs/superpowers/plans/2026-06-08-claude-usage-widget-plan.md)

> Note: the plan was written against a single-crate layout; the actual build uses
> the two-crate split above (so the core is testable without gpui) and a
> `Collector::with_deps(...)` test seam instead of the plan's `new_with_sources`.
> The task-level code and tests transfer with those path/seam adjustments.

## Layout

```
claude-usage/
├─ README.md
├─ docs/superpowers/{specs,plans}/…
└─ crates/
   ├─ usage-core/                 # pure logic — 59 tests green
   │  ├─ src/{model,timeutil,config,collector,statusline_cmd}.rs
   │  ├─ src/sources/{mod,oauth,statusline,jsonl}.rs
   │  ├─ src/bin/statusline.rs    # the registerable helper
   │  └─ examples/snapshot.rs     # real-data demo
   └─ usage-widget/               # gpui GUI shell — scaffolded, build-pending
      ├─ src/main.rs
      ├─ src/ui/{mod,theme}.rs
      └─ src/win/{mod,topmost,backdrop}.rs
```
