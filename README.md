# claude-usage-widget

A small, always-on-top, **frosted-glass** Windows desktop widget (Rust) that shows
how close you are to your Claude usage limits — the real **5-hour** and **7-day**
rate-limit utilization, color-coded, plus local token detail.

Built in two crates so the data/logic core is testable on its own:

| Crate | What | Builds with |
|---|---|---|
| **`usage-core`** | All data + logic: quota sources, JSONL token parsing, reconciliation, the `UsageSnapshot` model, config, the status-line helper. **No GUI.** | stable Rust (here: 1.94.1) — **no cmake** |
| **`usage-widget`** | The gpui + gpui-component GUI shell (window, gauges, always-on-top, Mica). Thin cap over `usage-core`. | Rust 1.96 + **CMake** + MSVC |

## Status (2026-06-08)

| Piece | State |
|---|---|
| `usage-core` — model, timeutil, config, sources (oauth/statusline/jsonl), reconcile, collector, statusline helper | ✅ **done, 51 tests green** |
| `cargo run --example snapshot` (reads your real `~/.claude` data) | ✅ **working** |
| `claude-usage-statusline` bin (populates the quota cache) | ✅ **working** |
| Live OAuth fetch (`--features net`) | ✅ **compiles**; not yet exercised against the live endpoint |
| `usage-widget` GUI (window, bars, gauge, menu, topmost, Mica) | 🚧 **scaffolded, not built** — needs CMake + the M0 spike |

The interesting 70% — getting the numbers right — is done and verified. What
remains is the GUI shell, which is gated on installing CMake and iterating against
gpui's (unstable, git-only) API.

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

## Finishing the GUI (milestone M0 onward)

1. Install prerequisites: **CMake** (`winget install Kitware.CMake`) and
   **Rust 1.96** (`rustup toolchain install 1.96.0`). MSVC "Desktop development
   with C++" is already present.
2. `cd crates/usage-widget`
3. **M0 spike:** get a borderless gpui-component window up with the Mica backdrop +
   `HWND_TOPMOST`, rendering one hardcoded `ProgressBar`. This proves the riskiest
   stack on this machine. Capture the working gpui/gpui-component git revs into
   `Cargo.lock` and pin them in `Cargo.toml`.
4. Continue with M1–M6 from the plan (below). The `usage-core` API the GUI calls
   is final; only the gpui code in `src/ui/` + `src/win/` needs to be brought up.

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
   ├─ usage-core/                 # pure logic — 51 tests green
   │  ├─ src/{model,timeutil,config,collector,statusline_cmd}.rs
   │  ├─ src/sources/{mod,oauth,statusline,jsonl}.rs
   │  ├─ src/bin/statusline.rs    # the registerable helper
   │  └─ examples/snapshot.rs     # real-data demo
   └─ usage-widget/               # gpui GUI shell — scaffolded, build-pending
      ├─ src/main.rs
      ├─ src/ui/{mod,theme}.rs
      └─ src/win/{mod,topmost,backdrop}.rs
```
