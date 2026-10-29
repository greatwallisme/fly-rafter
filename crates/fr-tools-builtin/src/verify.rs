// Rust guideline compliant 2026-10-29
//! Catalog-freeze verification (the builtin-tools contract: parameter
//! schemas frozen against the dsh `docs/tool-catalog.md`, script-extracted).
//!
//! The `--check` drift pass verifies the generated constants against the
//! live catalog (the fr-system-prompt judgment: it lives in the lib so
//! the spec lifecycle's test discovery reaches it, and it fails by
//! design when `FR_DSH_SOURCE` is absent). The freeze tests compare the
//! composed registrations against the frozen constants, so the
//! assembly-versus-catalog diff stays empty.

use std::process::Command;

use serde_json::{Value, json};

use crate::catalog_gen;
use crate::schema::{
    bash_description, bash_parameters, pwsh_description, pwsh_parameters, shell_output_schema,
};

fn dsh_source() -> String {
    std::env::var("FR_DSH_SOURCE")
        .expect("set FR_DSH_SOURCE to the deepseek-harness checkout (catalog constants verify against the sources)")
}

#[test]
fn generated_catalog_matches_the_dsh_sources() {
    let status = Command::new("python3")
        .arg("../../scripts/generate-tool-catalog.py")
        .arg("--check")
        .env("FR_DSH_SOURCE", dsh_source())
        .status()
        .expect("python3 is available to run scripts/generate-tool-catalog.py");
    assert!(
        status.success(),
        "generate-tool-catalog --check reported drift: rerun scripts/generate-tool-catalog.py"
    );
}

/// Strips the fr-only escalation pair from a composed parameter table,
/// leaving exactly what the catalog documents (the base registration).
fn base_registration(mut parameters: Value) -> Value {
    let properties = parameters
        .get_mut("properties")
        .and_then(Value::as_object_mut)
        .expect("a composed table carries properties");
    properties.remove("sandbox_permissions");
    properties.remove("justification");
    parameters
}

/// The composed registration equals the frozen catalog extraction: the
/// assembly-versus-catalog diff is empty (the builtin-tools contract's
/// freeze clause; Q95-A's second product).
#[test]
fn bash_registration_matches_the_frozen_catalog() {
    assert_eq!(bash_description(), catalog_gen::BASH_DESCRIPTION);
    let frozen: Value =
        serde_json::from_str(catalog_gen::BASH_PARAMETERS_JSON).expect("the frozen table parses");
    assert_eq!(base_registration(bash_parameters(true, true, true)), frozen);
}

#[test]
fn pwsh_registration_matches_the_frozen_catalog() {
    assert_eq!(pwsh_description(false), catalog_gen::PWSH_DESCRIPTION);
    let frozen: Value =
        serde_json::from_str(catalog_gen::PWSH_PARAMETERS_JSON).expect("the frozen table parses");
    assert_eq!(base_registration(pwsh_parameters(true, true, true)), frozen);
}

/// The escalation variant differs from the frozen base exactly by the
/// escalation pair (the fr-only registration facts).
#[test]
fn escalation_variant_differs_exactly_by_the_escalation_pair() {
    let base: Value = serde_json::from_str(catalog_gen::BASH_PARAMETERS_JSON).expect("parses");
    let full = bash_parameters(true, true, true);
    assert_eq!(base_registration(full.clone()), base);
    let properties = full
        .get("properties")
        .and_then(Value::as_object)
        .expect("properties");
    assert_eq!(properties.len(), 7);
    assert!(properties.contains_key("sandbox_permissions"));
    assert!(properties.contains_key("justification"));
    assert_eq!(
        properties["sandbox_permissions"]["enum"],
        json!(["workspace-write", "danger-full-access"])
    );
}

/// The output contract stays the shared three-union shape (the
/// catalog documents parameters alone; the output schema rides the dsh
/// source surface, pinned here against the registered payload shape).
#[test]
fn output_schema_pinned_against_the_registration() {
    let schema = shell_output_schema();
    assert_eq!(schema["oneOf"].as_array().expect("oneOf").len(), 3);
}
