//! The headless first-party loading manifest (装载清单).
//!
//! The composition comes from fr-config (entry default bundle embedded
//! constants → builtin bundles → profile patch → home patch → overlays);
//! this manifest carries the first-party rows the headless composition
//! must have that the composition itself does not name (the post-Q5B
//! facility/guard/token-meter set) plus the two decision-47 reserved rows
//! (fr-llm-retry, fr-working-directory) that await their own slices
//! (2026-11-04 ruling 2B: until then they fail artifact resolution and
//! surface as pending-slice warnings, never a silent gap).
//!
//! The manifest never names a `builtin` source: decision 16 keeps builtin
//! sources out of fr assembly documents entirely.

use serde_json::{Value, json};

/// One manifest row appended to the composed rows when the composition
/// does not already carry the plugin name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestRow {
    /// The composition key (the loader slot name).
    pub id: &'static str,
    /// The plugin name resolved to a dylib artifact by the search dirs.
    pub name: &'static str,
    /// Slot config; every manifest row relies on baked plugin defaults.
    pub config: Value,
}

/// The rows the headless composition must carry beyond its own bundle.
///
/// Order is dependency-sane (providers before hard-activation consumers);
/// the kernel's park-and-converge handles the interleaving with bundle
/// rows. Built per call: `serde_json::json!` is not const-callable.
#[must_use]
pub fn manifest_rows() -> Vec<ManifestRow> {
    vec![
        ManifestRow {
            id: "net",
            name: "fr-net",
            config: json!({}),
        },
        ManifestRow {
            id: "attachment",
            name: "fr-attachment",
            config: json!({}),
        },
        ManifestRow {
            id: "storage",
            name: "fr-storage",
            config: json!({}),
        },
        ManifestRow {
            id: "llm",
            name: "fr-llm",
            config: json!({}),
        },
        ManifestRow {
            id: "token-meter",
            name: "fr-token-meter",
            config: json!({}),
        },
        ManifestRow {
            id: "guard-timeout",
            name: "fr-guard-timeout",
            config: json!({}),
        },
        ManifestRow {
            id: "guard-repeat-reminder",
            name: "fr-guard-repeat-reminder",
            config: json!({}),
        },
    ]
}

/// The two decision-47 reserved rows (fr-llm-retry, fr-working-directory):
/// declared so the manifest covers them; they fail artifact resolution
/// until their slices build the crates (2026-11-04 ruling 2B).
#[must_use]
pub fn reserved_rows() -> Vec<ManifestRow> {
    vec![
        ManifestRow {
            id: "llm-retry",
            name: "fr-llm-retry",
            config: json!({}),
        },
        ManifestRow {
            id: "working-directory",
            name: "fr-working-directory",
            config: json!({}),
        },
    ]
}

/// The full first-party plugin name set the headless composition covers
/// (manifest rows, reserved rows, and the four bundle rows whose crates
/// arrive with their own slices).
///
/// A name is first-party when it starts with the `fr-` prefix, so the
/// compile face's trust-boundary check sees every fr assembly row as
/// trusted.
#[must_use]
pub fn is_first_party(name: &str) -> bool {
    name.starts_with("fr-")
}
