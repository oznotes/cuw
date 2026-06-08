//! Windows-specific glue: always-on-top and backdrop selection. All `unsafe`
//! and platform code lives under this module.

pub mod autostart;
pub mod backdrop;
pub mod topmost;
