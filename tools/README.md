# tools/

Developer-only utilities — **not part of the shipped widget** (each is
`publish = false`). Build/run with `--manifest-path`.

- **`icongen`** — rasterizes `crates/usage-widget/assets/icon.svg` into a
  multi-size `icon.ico` (which `usage-widget/build.rs` embeds into the exe).
  `cargo run --manifest-path tools/icongen/Cargo.toml`
- **`glass-capture`** — captures a screen rectangle via **DXGI Desktop
  Duplication** to a PNG (with mean/stddev luma). Unlike GDI `BitBlt`, it sees
  gpui's DirectComposition window and any DWM backdrop, so it's how the widget's
  glass is *verified* — including the shelved frosted-backdrop experiment (see
  [`docs/glass-backdrop-research.md`](../docs/glass-backdrop-research.md)).
  `cargo run --manifest-path tools/glass-capture/Cargo.toml -- <hwnd> <pad> <out.png>`
