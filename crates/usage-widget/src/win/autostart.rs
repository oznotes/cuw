//! "Start on login" via the per-user `HKCU\…\Run` registry value. Toggled by
//! the user from the widget's menu (user-initiated, so it's their choice — not
//! something the app forces).

#[cfg(target_os = "windows")]
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
#[cfg(target_os = "windows")]
const VALUE_NAME: &str = "ClaudeUsageWidget";

/// Is the widget currently registered to start on login?
#[cfg(target_os = "windows")]
pub fn is_enabled() -> bool {
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;

    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(RUN_KEY)
        .ok()
        .and_then(|k| k.get_value::<String, _>(VALUE_NAME).ok())
        .is_some()
}

/// Enable/disable start-on-login by writing/removing the Run value, pointing at
/// the currently running executable.
#[cfg(target_os = "windows")]
pub fn set(enabled: bool) -> std::io::Result<()> {
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;

    let run = RegKey::predef(HKEY_CURRENT_USER).create_subkey(RUN_KEY)?.0;
    if enabled {
        let exe = std::env::current_exe()?;
        run.set_value(VALUE_NAME, &format!("\"{}\"", exe.display()))
    } else {
        // Deleting a missing value is fine.
        match run.delete_value(VALUE_NAME) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }
}

#[cfg(not(target_os = "windows"))]
pub fn is_enabled() -> bool {
    false
}

#[cfg(not(target_os = "windows"))]
pub fn set(_enabled: bool) -> std::io::Result<()> {
    Ok(())
}
