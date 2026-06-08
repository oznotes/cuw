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

fn supports_mica(build: u32) -> bool {
    build >= 22_000
}

/// True on Windows 11 (build >= 22000), where Mica is supported. Uses
/// `RtlGetVersion`, which is not affected by application compatibility
/// manifests the way older version helpers can be.
#[cfg(target_os = "windows")]
fn is_win11() -> bool {
    windows_build_number().map(supports_mica).unwrap_or(false)
}

#[cfg(not(target_os = "windows"))]
fn is_win11() -> bool {
    false
}

#[cfg(target_os = "windows")]
fn windows_build_number() -> Option<u32> {
    use windows::Wdk::System::SystemServices::RtlGetVersion;
    use windows::Win32::System::SystemInformation::OSVERSIONINFOW;

    let mut version = OSVERSIONINFOW::default();
    version.dwOSVersionInfoSize = std::mem::size_of::<OSVERSIONINFOW>() as u32;

    // SAFETY: `version` points to initialized writable storage for the OS to
    // fill, and the size field is set as required by RtlGetVersion.
    let status = unsafe { RtlGetVersion(&mut version) };
    status.is_ok().then_some(version.dwBuildNumber)
}

#[cfg(test)]
mod tests {
    use super::supports_mica;

    #[test]
    fn mica_requires_windows_11_build() {
        assert!(!supports_mica(21_999));
        assert!(supports_mica(22_000));
        assert!(supports_mica(26_200));
    }
}
