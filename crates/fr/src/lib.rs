//! Fly Rafter host library face: the boot assembly, manifest, diagnostics,
//! event pump, and signal faces the `fr` binary drives (PRD 4.3 宿主壳三件套:
//! 装配器 + 事件泵 + 入口分发).
//!
//! The library/target split lets the integration tests drive the real
//! assembly (the six 18a scenarios) against real cdylib artifacts; the
//! binary remains the single `fr` executable.

#![forbid(unsafe_code)]

pub mod assembly;
pub mod diagnostics;
pub mod exit_watcher;
pub mod manifest;
pub mod pump;
pub mod signals;
