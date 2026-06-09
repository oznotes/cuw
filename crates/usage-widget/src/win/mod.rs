// claude-usage - a Claude usage widget for Windows.
// Copyright (c) 2026 Ozgur Oz. MIT License (see LICENSE).
//
//! Windows-specific glue: always-on-top and start-on-login. All `unsafe` and
//! platform code lives under this module.

pub mod autostart;
pub mod topmost;
