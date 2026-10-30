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

#[test]
fn read_registration_matches_the_frozen_catalog() {
    let frozen: Value = serde_json::from_str(crate::catalog_gen::READ_PARAMETERS_JSON)
        .expect("the frozen table parses");
    // The read table carries no escalation pair; the composed table is
    // the catalog base outright.
    assert_eq!(crate::schema::read_parameters(), frozen);
}

#[test]
fn write_registration_matches_the_frozen_catalog() {
    let frozen: Value = serde_json::from_str(crate::catalog_gen::WRITE_PARAMETERS_JSON)
        .expect("the frozen table parses");
    assert_eq!(
        base_registration(crate::schema::write_parameters(true)),
        frozen
    );
}

#[test]
fn edit_registration_matches_the_frozen_catalog() {
    let frozen: Value = serde_json::from_str(crate::catalog_gen::EDIT_PARAMETERS_JSON)
        .expect("the frozen table parses");
    assert_eq!(
        base_registration(crate::schema::edit_parameters(true)),
        frozen
    );
}

#[test]
fn glob_registration_matches_the_frozen_catalog() {
    // The catalog freezes the sampled over-cap variant: the composed
    // sampled=true description IS the frozen text, and the baked
    // bundle switch (false) differs by exactly the one phrase.
    assert_eq!(
        crate::schema::glob_description(true),
        crate::catalog_gen::GLOB_DESCRIPTION
    );
    let baked = crate::schema::glob_description(
        crate::search_core::baked_caps().sample_over_cap_glob_results,
    );
    assert_eq!(
        baked,
        crate::catalog_gen::GLOB_DESCRIPTION.replace(
            crate::catalog_gen::GLOB_OVERCAP_SAMPLED_TEXT,
            crate::catalog_gen::GLOB_OVERCAP_HEAD_TEXT,
        )
    );
    assert!(
        baked.contains("a larger result keeps the first paths and reports"),
        "{baked}"
    );
    let frozen: Value = serde_json::from_str(crate::catalog_gen::GLOB_PARAMETERS_JSON)
        .expect("the frozen table parses");
    assert_eq!(crate::schema::glob_parameters(), frozen);
    let output = crate::schema::glob_output_schema();
    assert_eq!(
        output["properties"]
            .as_object()
            .expect("properties")
            .keys()
            .collect::<Vec<_>>(),
        vec!["root", "paths"]
    );
    assert_eq!(output["properties"]["paths"]["items"]["type"], "string");
}

#[test]
fn grep_registration_matches_the_frozen_catalog() {
    assert_eq!(
        crate::schema::grep_description(),
        crate::catalog_gen::GREP_DESCRIPTION
    );
    let frozen: Value = serde_json::from_str(crate::catalog_gen::GREP_PARAMETERS_JSON)
        .expect("the frozen table parses");
    assert_eq!(crate::schema::grep_parameters(), frozen);
    let output = crate::schema::grep_output_schema();
    let item = &output["properties"]["matches"]["items"];
    assert_eq!(
        item["properties"]
            .as_object()
            .expect("properties")
            .keys()
            .collect::<Vec<_>>(),
        vec!["path", "lineNumber", "line"]
    );
    assert_eq!(item["properties"]["lineNumber"]["type"], "integer");
}

#[test]
fn str_replace_editor_registration_matches_the_frozen_catalog() {
    // The opt-in editor carries no escalation pair and no section (the
    // dsh source injects no systemPrompt; the Q16b-4A ruling), so the
    // table is the catalog base outright.
    assert_eq!(
        crate::schema::str_replace_editor_description(),
        crate::catalog_gen::STR_REPLACE_EDITOR_DESCRIPTION
    );
    let frozen: Value =
        serde_json::from_str(crate::catalog_gen::STR_REPLACE_EDITOR_PARAMETERS_JSON)
            .expect("the frozen table parses");
    assert_eq!(crate::schema::str_replace_editor_parameters(), frozen);
    assert_eq!(
        crate::schema::str_replace_editor_output_schema(),
        serde_json::json!({ "type": "string" })
    );
    // The command enum rides the catalog row verbatim.
    let parameters = crate::schema::str_replace_editor_parameters();
    assert_eq!(
        parameters["properties"]["command"]["enum"],
        serde_json::json!(["view", "create", "str_replace", "insert"])
    );
    assert_eq!(
        parameters["required"],
        serde_json::json!(["command", "path"])
    );
}

/// The fs escalation variants differ from the frozen bases exactly by
/// the escalation pair, whose description carries the operation
/// subject (the dsh `FsSandboxController` judgment).
#[test]
fn fs_escalation_variant_differs_exactly_by_the_escalation_pair() {
    for full in [
        crate::schema::write_parameters(true),
        crate::schema::edit_parameters(true),
    ] {
        let properties = full
            .get("properties")
            .and_then(Value::as_object)
            .expect("properties");
        assert!(properties.contains_key("sandbox_permissions"));
        assert!(properties.contains_key("justification"));
        assert_eq!(
            properties["sandbox_permissions"]["enum"],
            json!(["workspace-write", "danger-full-access"])
        );
        assert!(
            properties["sandbox_permissions"]["description"]
                .as_str()
                .expect("description")
                .contains("the exact operation"),
            "{}",
            properties["sandbox_permissions"]["description"]
        );
    }
    let base: Value =
        serde_json::from_str(crate::catalog_gen::WRITE_PARAMETERS_JSON).expect("parses");
    assert_eq!(
        base_registration(crate::schema::write_parameters(true)),
        base
    );
}

/// The fs output contracts pin the canonical shapes (the catalog
/// documents parameters alone; the output schemas ride the dsh tool
/// sources).
#[test]
fn fs_output_schemas_pin_the_shapes() {
    let read = crate::schema::read_output_schema();
    assert_eq!(read["properties"].as_object().expect("properties").len(), 4);
    assert_eq!(read["additionalProperties"], false);
    assert_eq!(
        read["properties"]["lines"]["items"]["properties"]["number"]["type"],
        "integer"
    );
    let write = crate::schema::write_output_schema();
    assert_eq!(
        write["properties"]["operation"]["enum"],
        json!(["create", "update"])
    );
    assert_eq!(
        write["properties"]["before"]["oneOf"]
            .as_array()
            .expect("oneOf")
            .len(),
        2
    );
    let edit = crate::schema::edit_output_schema();
    assert_eq!(edit["properties"].as_object().expect("properties").len(), 3);
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
