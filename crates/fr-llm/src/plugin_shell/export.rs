//! The dylib export table, isolated in its own module (18b finding): the
//! version script promotes `DOUGONG_PLUGIN_EXPORTS` to an exported GC
//! root in every cdylib link, so the symbol's codegen unit must never
//! share an object with generic monomorphizations a dependent crate's
//! rlib link could pull — co-location rooted the table into dependent
//! artifacts and duplicated the symbol (the fr-attachment link failure
//! this module split fixes).

use dougong_abi::PluginExports;

use super::{ABI_VERSION, call, descriptor, on_event, on_next, plugin_name, setup, teardown};

/// Export table for the fr-llm dylib artifact; the host resolves this
/// symbol by name at load time.
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
