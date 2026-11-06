// Rust guideline compliant 2026-11-06
//! The dylib export table, isolated in its own module (the 18b CGU
//! co-location finding): the version script promotes
//! `DOUGONG_PLUGIN_EXPORTS` to an exported GC root in every cdylib link,
//! so the symbol's codegen unit must never share an object with generic
//! monomorphizations a dependent crate's rlib link could pull.

use dougong_abi::{ABI_VERSION, PluginExports};

use crate::plugin_shell::{call, descriptor, on_event, on_next, plugin_name, setup, teardown};

/// Export table for the fr-subagent dylib artifact; the host resolves
/// this symbol by name at load time.
#[unsafe(no_mangle)]
pub static DOUGONG_PLUGIN_EXPORTS: PluginExports = PluginExports {
    abi_version: ABI_VERSION,
    name: plugin_name,
    descriptor,
    setup,
    call,
    on_event,
    on_next,
    teardown,
};
