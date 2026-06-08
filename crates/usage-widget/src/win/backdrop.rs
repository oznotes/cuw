//! Map the configured [`Backdrop`] to a gpui `WindowBackgroundAppearance`,
//! downgrading the Win11-only Mica materials on older Windows.
//!
//! M0/M6: confirm the gpui `WindowBackgroundAppearance` variant spellings
//! against the pinned revision (`MicaBackdrop`, `MicaAltBackdrop`, `Blurred`,
//! `Transparent`, `Opaque`).

use usage_core::config::Backdrop;

/// The gpui appearance to apply for a configured backdrop. On pre-Win11, Mica
/// requests fall back to `Blurred` (acrylic), then the OS handles the rest.
pub fn appearance(backdrop: Backdrop) -> gpui::WindowBackgroundAppearance {
    use gpui::WindowBackgroundAppearance as A;

    let mica = matches!(backdrop, Backdrop::Mica | Backdrop::MicaAlt);
    if mica && !is_win11() {
        return A::Blurred;
    }
    match backdrop {
        Backdrop::Mica => A::MicaBackdrop,
        Backdrop::MicaAlt => A::MicaAltBackdrop,
        Backdrop::Acrylic => A::Blurred,
        Backdrop::Transparent => A::Transparent,
        Backdrop::Opaque => A::Opaque,
    }
}

/// True on Windows 11 (build >= 22000), where Mica is supported.
///
/// M0: bind `RtlGetVersion` (ntdll) for a manifest-independent build number, or
/// read `HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\CurrentBuildNumber`.
/// This machine is build 26200, so the conservative default is `true`.
#[cfg(target_os = "windows")]
fn is_win11() -> bool {
    true
}

#[cfg(not(target_os = "windows"))]
fn is_win11() -> bool {
    false
}
