// claude-usage - a Claude usage widget for Windows.
// Copyright (c) 2026 Ozgur Oz. MIT License (see LICENSE).
//
//! Embed the Windows `.exe` icon (`assets/icon.ico` via `assets/app.rc`, using
//! the embed-resource crate). Skipped gracefully when either asset is absent so
//! the build never fails on a missing icon.

fn main() {
    #[cfg(target_os = "windows")]
    {
        if std::path::Path::new("assets/app.rc").exists()
            && std::path::Path::new("assets/icon.ico").exists()
        {
            embed_resource::compile("assets/app.rc", embed_resource::NONE);
        } else {
            println!(
                "cargo:warning=assets/app.rc or assets/icon.ico missing; building without an .exe icon"
            );
        }
    }
}
