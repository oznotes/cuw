//! Embed the Windows `.exe` icon, if present. Skipped gracefully when the icon
//! is absent so the build never fails on a missing asset (add `assets/icon.ico`
//! + `assets/app.rc` in milestone M6 to give the binary an icon).

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
