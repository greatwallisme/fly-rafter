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

/// The `todo_write` registration equals the frozen catalog extraction and
/// carries the exclusive scheduling face (the dsh source registers no
/// `isConcurrencySafe` and no presentResult/presentationMeta endpoints,
/// so the composed row is the base outright; Q97-A's full-schema pin for
/// the tool family).
#[test]
fn todo_write_registration_matches_the_frozen_catalog() {
    assert_eq!(
        crate::schema::todo_write_description(true),
        crate::catalog_gen::TODO_WRITE_DESCRIPTION
    );
    // The false variant differs exactly by the middle segment (the dsh
    // describe() composition).
    assert_eq!(
        crate::schema::todo_write_description(false),
        format!(
            "{}{}{}",
            crate::catalog_gen::TODO_WRITE_DESCRIPTION_HEAD,
            crate::catalog_gen::TODO_WRITE_DESCRIPTION_SINGLE,
            crate::catalog_gen::TODO_WRITE_DESCRIPTION_TAIL
        )
    );
    let frozen: Value = serde_json::from_str(crate::catalog_gen::TODO_WRITE_PARAMETERS_JSON)
        .expect("the frozen table parses");
    assert_eq!(crate::schema::todo_write_parameters(), frozen);
    // The output contract rides the dsh source surface (the schemastery
    // row form; the inProgress wire key is the protocol exemption).
    let output = crate::schema::todo_write_output_schema();
    assert_eq!(
        output["properties"]
            .as_object()
            .expect("properties")
            .keys()
            .collect::<Vec<_>>(),
        vec!["todos", "counts"]
    );
    assert_eq!(
        output["properties"]["counts"]["properties"]
            .as_object()
            .expect("counts properties")
            .keys()
            .collect::<Vec<_>>(),
        vec!["pending", "inProgress", "completed"]
    );
    assert_eq!(
        output["properties"]["todos"]["items"]["properties"]["status"]["enum"],
        serde_json::json!(["pending", "in_progress", "completed"])
    );
    // The scheduling face: no `isConcurrencySafe` key (fail-closed
    // exclusive) and the `other` presentation family riding presentCall.
    let registration = crate::plugin_shell::todo_write_registration();
    assert!(
        registration.get("isConcurrencySafe").is_none(),
        "the tool stays exclusive"
    );
    assert_eq!(
        registration["presentCall"]["service"],
        crate::service::SERVICE_NAME
    );
    assert_eq!(
        crate::todo::present_call(&serde_json::json!({
            "todos": [{"content": "a", "status": "pending"}]
        }))["kind"],
        serde_json::json!("other")
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

/// The composed `read_image` registration equals the frozen catalog
/// extraction: the single `file_path` row freezes verbatim and the
/// output contract rides the dsh read-image shapes (the builtin-tools
/// contract's freeze clause for the image tool).
#[test]
fn read_image_registration_matches_the_frozen_catalog() {
    let frozen: Value = serde_json::from_str(crate::catalog_gen::READ_IMAGE_PARAMETERS_JSON)
        .expect("the frozen table parses");
    assert_eq!(crate::schema::read_image_parameters(), frozen);
    let output = crate::schema::read_image_output_schema();
    let properties = output["properties"].as_object().expect("properties");
    assert_eq!(properties.keys().collect::<Vec<_>>(), vec!["path", "image"]);
    assert_eq!(
        output["properties"]["image"]["properties"]["mediaType"]["enum"],
        serde_json::json!(["image/png", "image/jpeg", "image/webp", "image/gif"])
    );
}

/// The three goal controls equal the frozen catalog extraction and
/// carry the exclusive scheduling face (the dsh sources register no
/// `isConcurrencySafe` and no presentResult/presentationMeta endpoints;
/// Q97-A's full-schema pins for the goal family, covering every shape
/// and constraint scenario the catalog rows express).
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "the family pin mirrors the catalog rows one assertion per face; splitting it would hide the correspondence"
)]
fn goal_registrations_match_the_frozen_catalog() {
    // The descriptions ride the catalog-verbatim constants.
    assert_eq!(
        crate::catalog_gen::GET_GOAL_DESCRIPTION,
        "Read the current session goal, including the id and revision that update_goal requires."
    );
    assert_eq!(
        crate::catalog_gen::UPDATE_GOAL_DESCRIPTION,
        "Update the current goal."
    );
    assert!(
        crate::catalog_gen::CREATE_GOAL_DESCRIPTION
            .starts_with("Create a persisted goal that keeps this session working"),
        "{}",
        crate::catalog_gen::CREATE_GOAL_DESCRIPTION
    );
    // The parameter tables equal the frozen catalog JSON.
    for (composed, frozen) in [
        (
            crate::schema::get_goal_parameters(),
            crate::catalog_gen::GET_GOAL_PARAMETERS_JSON,
        ),
        (
            crate::schema::create_goal_parameters(),
            crate::catalog_gen::CREATE_GOAL_PARAMETERS_JSON,
        ),
        (
            crate::schema::update_goal_parameters(),
            crate::catalog_gen::UPDATE_GOAL_PARAMETERS_JSON,
        ),
    ] {
        let frozen: Value = serde_json::from_str(frozen).expect("the frozen table parses");
        assert_eq!(composed, frozen);
    }
    // The create table pins the required row and the optional cap.
    let create = crate::schema::create_goal_parameters();
    assert_eq!(create["required"], json!(["objective"]));
    assert_eq!(
        create["properties"]["max_goal_rounds"]["type"],
        json!("number")
    );
    // The update table pins the enum order and the required trio.
    let update = crate::schema::update_goal_parameters();
    assert_eq!(
        update["properties"]["action"]["enum"],
        json!(crate::goal_tools::UPDATE_ACTIONS)
    );
    assert_eq!(update["required"], json!(["goal_id", "revision", "action"]));
    // The shared output contract: the oneOf null/plain pair, the goal
    // key set, and the activation enum.
    let output = crate::schema::goal_output_schema();
    assert_eq!(output["oneOf"].as_array().expect("oneOf").len(), 2);
    assert_eq!(
        output["oneOf"][0]["properties"]["goal"]["type"],
        json!("null")
    );
    assert_eq!(
        output["oneOf"][1]["properties"]["goal"]["properties"]
            .as_object()
            .expect("properties")
            .keys()
            .collect::<Vec<_>>(),
        vec![
            "id",
            "revision",
            "objective",
            "phase",
            "roundsStarted",
            "maxGoalRounds",
            "blockedReason"
        ]
    );
    assert_eq!(
        output["oneOf"][1]["properties"]["goal"]["properties"]["phase"]["enum"],
        json!(crate::goal::PHASES)
    );
    assert_eq!(
        output["oneOf"][1]["properties"]["activation"]["enum"],
        json!(["armed", "disarmed"])
    );
    // The scheduling faces: the three registrations stay exclusive
    // with the partial endpoint set.
    for registration in [
        crate::plugin_shell::get_goal_registration(),
        crate::plugin_shell::create_goal_registration(),
        crate::plugin_shell::update_goal_registration(),
    ] {
        assert!(
            registration.get("isConcurrencySafe").is_none(),
            "the goal tools stay exclusive"
        );
        assert!(registration.get("presentResult").is_none());
        assert!(
            registration
                .get("output")
                .expect("output")
                .get("presentationMeta")
                .is_none()
        );
        assert_eq!(
            registration["execute"]["service"],
            crate::service::SERVICE_NAME
        );
    }
    // The presentation families: get rides `read`, the other two ride
    // `other`.
    assert_eq!(
        crate::goal_tools::present_call_get(&Value::Null)["kind"],
        json!("read")
    );
    assert_eq!(
        crate::goal_tools::present_call_create(&json!({ "objective": "o" }))["kind"],
        json!("other")
    );
    assert_eq!(
        crate::goal_tools::present_call_update(&json!({ "action": "edit", "goal_id": "g" }))["kind"],
        json!("other")
    );
}

/// The `tool:goal` section composes over the extracted threshold
/// template: the default deployment reads threshold 3, the composed
/// text equals the dsh `guidance(3)` byte-for-byte shape, and the
/// registration rides the `TOOL_GOAL` order.
#[test]
fn goal_section_composes_over_the_extracted_template() {
    let text = crate::schema::goal_section_text(crate::goal_tools::BAKED_BLOCKED_AFTER);
    assert_eq!(
        text,
        format!(
            "{}3{}",
            crate::catalog_gen::GOAL_SECTION_TEXT_HEAD,
            crate::catalog_gen::GOAL_SECTION_TEXT_TAIL
        )
    );
    assert!(text.starts_with("create_goal may infer goal intent"));
    assert!(text.ends_with("or useful remaining work is not blocked."));
    assert!(text.contains("Mark blocked only after the same blocking condition persists for at least 3 consecutive goal rounds"));
    // The sections table registers exactly one goal section at the
    // TOOL_GOAL order.
    let sections = crate::plugin_shell::prompt_sections();
    let goal_sections: Vec<_> = sections
        .iter()
        .filter(|(name, _, _)| name == "tool:goal")
        .collect();
    assert_eq!(goal_sections.len(), 1);
    assert_eq!(goal_sections[0].1, 2400.0);
}

/// Q97-A's full-schema pin for the job family: the three descriptions
/// and parameter tables ride the frozen catalog, the shared
/// projection row keeps the seven-key shape, and the registrations
/// stay exclusive with the partial endpoint set (the goal-family pin
/// judgment, one assertion per face).
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "the family pin mirrors the catalog rows one assertion per face; splitting it would hide the correspondence"
)]
fn job_registrations_match_the_frozen_catalog() {
    // The descriptions ride the catalog-verbatim constants.
    assert_eq!(
        crate::catalog_gen::JOB_OUTPUT_DESCRIPTION,
        "Read a background job: output since the previous read for stream jobs, or the result of a finished final-output job."
    );
    assert_eq!(
        crate::catalog_gen::JOB_LIST_DESCRIPTION,
        "List your background jobs (running and finished) with their ids, kinds, and statuses."
    );
    assert_eq!(
        crate::catalog_gen::JOB_KILL_DESCRIPTION,
        "Request cancellation of a running background job."
    );
    // The parameter tables equal the frozen catalog JSON.
    for (composed, frozen) in [
        (
            crate::schema::job_output_parameters(),
            crate::catalog_gen::JOB_OUTPUT_PARAMETERS_JSON,
        ),
        (
            crate::schema::job_list_parameters(),
            crate::catalog_gen::JOB_LIST_PARAMETERS_JSON,
        ),
        (
            crate::schema::job_kill_parameters(),
            crate::catalog_gen::JOB_KILL_PARAMETERS_JSON,
        ),
    ] {
        let frozen: Value = serde_json::from_str(frozen).expect("the frozen table parses");
        assert_eq!(composed, frozen);
    }
    // The output table pins the optional wait and timeout rows; the
    // kill table pins the required id.
    let output = crate::schema::job_output_parameters();
    assert_eq!(output["required"], json!(["job_id"]));
    assert_eq!(output["properties"]["wait"]["type"], json!("boolean"));
    assert_eq!(output["properties"]["timeout_ms"]["type"], json!("number"));
    let kill = crate::schema::job_kill_parameters();
    assert_eq!(kill["required"], json!(["job_id"]));
    assert!(kill["properties"].get("wait").is_none());
    let list = crate::schema::job_list_parameters();
    assert_eq!(list["properties"].as_object().expect("properties").len(), 0);
    // The shared projection row: seven keys, the terminal-status enum,
    // and the optional detail/finishedAt rows.
    let row = crate::schema::public_job_schema();
    assert_eq!(
        row["properties"]
            .as_object()
            .expect("properties")
            .keys()
            .collect::<Vec<_>>(),
        vec![
            "id",
            "kind",
            "label",
            "status",
            "detail",
            "startedAt",
            "finishedAt"
        ]
    );
    assert_eq!(
        row["properties"]["status"]["enum"],
        json!(["running", "stopping", "completed", "killed", "failed"])
    );
    // The three output contracts compose the row.
    let output_schema = crate::schema::job_output_schema();
    assert_eq!(output_schema["properties"]["text"]["required"], json!(true));
    assert_eq!(output_schema["properties"]["job"]["required"], json!(true));
    let list_schema = crate::schema::job_list_schema();
    assert_eq!(list_schema["type"], json!("array"));
    assert!(list_schema["items"].is_object());
    let kill_schema = crate::schema::job_kill_schema();
    assert_eq!(
        kill_schema["properties"]["outcome"]["enum"],
        json!(["cancellation-requested", "already-finished"])
    );
    // The scheduling faces: the three registrations stay exclusive
    // with the partial endpoint set and the self-service endpoints.
    for registration in [
        crate::plugin_shell::job_output_registration(),
        crate::plugin_shell::job_list_registration(),
        crate::plugin_shell::job_kill_registration(),
    ] {
        assert!(
            registration.get("isConcurrencySafe").is_none(),
            "the job tools stay exclusive"
        );
        assert!(registration.get("presentResult").is_none());
        assert!(
            registration
                .get("output")
                .expect("output")
                .get("presentationMeta")
                .is_none()
        );
        assert_eq!(
            registration["execute"]["service"],
            crate::service::SERVICE_NAME
        );
    }
    // The presentation families: the two read faces and the kill's
    // execute face, with the raw input on the named-call controls.
    let output_call = crate::job_tools::present_call_output(&json!({ "job_id": "j1" }));
    assert_eq!(output_call["kind"], json!("read"));
    assert_eq!(output_call["rawInput"], json!("j1"));
    assert_eq!(
        crate::job_tools::present_call_list(&Value::Null)["kind"],
        json!("read")
    );
    let kill_call = crate::job_tools::present_call_kill(&json!({ "job_id": "j1" }));
    assert_eq!(kill_call["kind"], json!("execute"));
    assert_eq!(kill_call["rawInput"], json!("j1"));
}

/// The `tool:jobs` section rides the extracted static text verbatim:
/// the dsh `TOOL_JOBS` order 1600, the opening and closing anchors,
/// and the one-section-serves-three shape (the extract-then-freeze
/// judgment).
#[test]
fn jobs_section_pins_the_extracted_text() {
    let sections = crate::plugin_shell::prompt_sections();
    let jobs = sections
        .iter()
        .find(|(name, _, _)| name == "tool:jobs")
        .expect("the jobs section registers");
    assert_eq!(jobs.1, 1600.0);
    assert_eq!(jobs.2, crate::catalog_gen::JOBS_SECTION_TEXT);
    assert!(
        crate::catalog_gen::JOBS_SECTION_TEXT
            .starts_with("Track every background job id you start."),
        "{}",
        crate::catalog_gen::JOBS_SECTION_TEXT
    );
    assert!(
        crate::catalog_gen::JOBS_SECTION_TEXT
            .contains("job_output (set wait: true only when you are genuinely blocked on it)"),
        "{}",
        crate::catalog_gen::JOBS_SECTION_TEXT
    );
    assert!(
        crate::catalog_gen::JOBS_SECTION_TEXT
            .ends_with("and job_kill jobs that stopped mattering."),
        "{}",
        crate::catalog_gen::JOBS_SECTION_TEXT
    );
}

/// Q97-A's full-schema pin for the schedule family: the four
/// descriptions and parameter tables ride the frozen catalog, the
/// shared view union keeps its six rule variants and the nine-code
/// error union, and the registrations stay exclusive with the partial
/// endpoint set (the goal-family pin judgment, one assertion per face).
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "the family pin mirrors the catalog rows one assertion per face; splitting it would hide the correspondence"
)]
fn schedule_registrations_match_the_frozen_catalog() {
    // The descriptions ride the catalog-verbatim constants.
    assert_eq!(
        crate::catalog_gen::SCHEDULE_CREATE_DESCRIPTION,
        "Create a reminder in the current session that delivers prompt when it becomes due. \
Supply exactly one timing parameter: after_seconds, at, every_seconds, daily, weekly, or cron. \
Local times that do not exist in the zone are skipped; repeated local times fire once, at the earlier instant. \
After downtime, a recurring reminder delivers only its latest missed occurrence. Delivery can repeat after a crash."
    );
    assert_eq!(
        crate::catalog_gen::SCHEDULE_LIST_DESCRIPTION,
        "List the active reminders in the current session."
    );
    assert_eq!(
        crate::catalog_gen::SCHEDULE_DELETE_DESCRIPTION,
        "Delete a reminder in the current session, active or inactive. \
Deletion does not retract a reminder message that is already queued."
    );
    assert!(
        crate::catalog_gen::SCHEDULE_UPDATE_DESCRIPTION
            .starts_with("Change a reminder in place, keeping its id."),
        "{}",
        crate::catalog_gen::SCHEDULE_UPDATE_DESCRIPTION
    );
    // The parameter tables equal the frozen catalog JSON.
    for (composed, frozen) in [
        (
            crate::schema::schedule_create_parameters(),
            crate::catalog_gen::SCHEDULE_CREATE_PARAMETERS_JSON,
        ),
        (
            crate::schema::schedule_list_parameters(),
            crate::catalog_gen::SCHEDULE_LIST_PARAMETERS_JSON,
        ),
        (
            crate::schema::schedule_delete_parameters(),
            crate::catalog_gen::SCHEDULE_DELETE_PARAMETERS_JSON,
        ),
        (
            crate::schema::schedule_update_parameters(),
            crate::catalog_gen::SCHEDULE_UPDATE_PARAMETERS_JSON,
        ),
    ] {
        let frozen: Value = serde_json::from_str(frozen).expect("the frozen table parses");
        assert_eq!(composed, frozen);
    }
    // The create table pins the required pair and the six timing
    // selectors; the update table pins the required id and its
    // optional content rows.
    let create = crate::schema::schedule_create_parameters();
    assert_eq!(create["required"], json!(["prompt", "title"]));
    for selector in [
        "after_seconds",
        "at",
        "every_seconds",
        "daily",
        "weekly",
        "cron",
    ] {
        assert!(
            create["properties"].get(selector).is_some(),
            "{selector} rides the table"
        );
    }
    assert!(
        create["properties"]["title"]["description"]
            .as_str()
            .expect("description")
            .contains("at most 120 characters"),
        "{}",
        create["properties"]["title"]["description"]
    );
    let update = crate::schema::schedule_update_parameters();
    assert_eq!(update["required"], json!(["id"]));
    assert!(update["properties"].get("after_seconds").is_none());
    // The shared view union: six rule variants, the state/deliveryMode
    // rows, and the nine-code error union on every output contract.
    for output in [
        crate::schema::schedule_create_output_schema(),
        crate::schema::schedule_list_output_schema(),
        crate::schema::schedule_delete_output_schema(),
        crate::schema::schedule_update_output_schema(),
    ] {
        let variants = output["oneOf"].as_array().expect("oneOf");
        let errors = &variants[variants.len() - 9..];
        assert_eq!(errors.len(), 9);
        assert_eq!(
            errors[0]["properties"]["code"]["const"],
            json!("invalid_prompt")
        );
        assert_eq!(
            errors[8]["properties"]["code"]["const"],
            json!("internal_error")
        );
    }
    let create_output = crate::schema::schedule_create_output_schema();
    let create_variants = create_output["oneOf"].as_array().expect("oneOf");
    let view = &create_variants[0];
    assert_eq!(view["oneOf"].as_array().expect("view variants").len(), 6);
    let first = &view["oneOf"][0]["properties"];
    assert_eq!(first["kind"]["const"], json!("after"));
    assert_eq!(first["state"]["enum"], json!(["scheduled", "overdue"]));
    assert_eq!(first["deliveryMode"]["const"], json!("host"));
    // The delete contract keeps its two non-mutating arms.
    let delete_output = crate::schema::schedule_delete_output_schema();
    let delete_variants = delete_output["oneOf"].as_array().expect("oneOf");
    assert_eq!(
        delete_variants[0]["properties"]["deleted"]["const"],
        json!(true)
    );
    assert_eq!(
        delete_variants[1]["properties"]["code"]["const"],
        json!("schedule_not_found")
    );
    // The scheduling faces: the four registrations stay exclusive
    // with the partial endpoint set and the self-service endpoints.
    for registration in [
        crate::plugin_shell::schedule_create_registration(),
        crate::plugin_shell::schedule_list_registration(),
        crate::plugin_shell::schedule_delete_registration(),
        crate::plugin_shell::schedule_update_registration(),
    ] {
        assert!(
            registration.get("isConcurrencySafe").is_none(),
            "the schedule tools stay exclusive"
        );
        assert!(registration.get("presentResult").is_none());
        assert!(
            registration
                .get("output")
                .expect("output")
                .get("presentationMeta")
                .is_none()
        );
        assert_eq!(
            registration["execute"]["service"],
            crate::service::SERVICE_NAME
        );
    }
    // The presentation families: list rides `read`, the other three
    // ride `other` with the raw input on the named controls.
    assert_eq!(
        crate::schedule::present_call_list(&Value::Null)["kind"],
        json!("read")
    );
    assert_eq!(
        crate::schedule::present_call_create(&json!({ "prompt": "p" }))["kind"],
        json!("other")
    );
    assert_eq!(
        crate::schedule::present_call_delete(&json!({ "id": "schedule-1" }))["kind"],
        json!("other")
    );
    assert_eq!(
        crate::schedule::present_call_update(&json!({ "id": "schedule-1" }))["kind"],
        json!("other")
    );
}

/// The interaction registrations equal the frozen catalog extraction
/// (Q97-A): the ask/exit descriptions ride the catalog-verbatim
/// constants, the parameter tables equal the frozen JSON, and the
/// scheduling faces pin the dsh shapes — both stay exclusive, ask
/// renders without call cards, and exit carries the
/// presentCall/presentResult pair with the `other`/generic card.
#[test]
fn interaction_registrations_match_the_frozen_catalog() {
    assert_eq!(
        crate::catalog_gen::ASK_USER_QUESTION_DESCRIPTION,
        "Ask the user a concise question when you need confirmation, a choice, or missing information before proceeding."
    );
    assert_eq!(
        crate::catalog_gen::EXIT_PLAN_MODE_DESCRIPTION,
        "Use only in plan mode. Present your plan for the user's review and, on approval, leave plan mode. The user may approve (carry out the plan from your next step) or keep planning — their feedback comes back in the tool result; revise and present again."
    );
    for (composed, frozen) in [
        (
            crate::schema::ask_user_question_parameters(),
            crate::catalog_gen::ASK_USER_QUESTION_PARAMETERS_JSON,
        ),
        (
            crate::schema::exit_plan_mode_parameters(),
            crate::catalog_gen::EXIT_PLAN_MODE_PARAMETERS_JSON,
        ),
    ] {
        let frozen: Value = serde_json::from_str(frozen).expect("the frozen table parses");
        assert_eq!(composed, frozen);
    }
    // The questions row pins the per-item required pair, the optional
    // header/options/multi_select rows, and the label-required option
    // items; the exit row pins the required plan string.
    let ask = crate::schema::ask_user_question_parameters();
    assert_eq!(ask["required"], json!(["questions"]));
    let item = &ask["properties"]["questions"]["items"];
    assert_eq!(item["required"], json!(["id", "question"]));
    for optional in ["header", "options", "multi_select"] {
        assert!(
            item["properties"].get(optional).is_some(),
            "{optional} rides the item table"
        );
    }
    assert_eq!(item["properties"]["multi_select"]["type"], json!("boolean"));
    assert_eq!(
        item["properties"]["options"]["items"]["required"],
        json!(["label"])
    );
    let exit = crate::schema::exit_plan_mode_parameters();
    assert_eq!(exit["required"], json!(["plan"]));
    assert_eq!(exit["properties"]["plan"]["type"], json!("string"));
    // The output contracts: the answers array and the approved boolean
    // (the fr dialect marks requiredness per property, not per array).
    let ask_out = crate::schema::ask_user_question_output_schema();
    let answers_item = &ask_out["properties"]["answers"]["items"]["properties"];
    assert_eq!(
        answers_item["id"],
        json!({ "type": "string", "required": true })
    );
    assert_eq!(answers_item["selected"]["type"], json!("array"));
    assert_eq!(answers_item["selected"]["required"], json!(true));
    let exit_out = crate::schema::exit_plan_mode_output_schema();
    assert_eq!(
        exit_out["properties"]["approved"],
        json!({ "type": "boolean", "required": true })
    );
    // The scheduling faces: both stay exclusive (no isConcurrencySafe),
    // and only exit mounts the call/result card endpoints (the dsh
    // plan-mode presentation; ask renders the raw JSON text).
    let ask_registration = crate::plugin_shell::ask_user_question_registration();
    let exit_registration = crate::plugin_shell::exit_plan_mode_registration();
    for registration in [&ask_registration, &exit_registration] {
        assert!(
            registration.get("isConcurrencySafe").is_none(),
            "the interaction tools stay exclusive"
        );
        assert_eq!(
            registration["execute"]["service"],
            crate::service::SERVICE_NAME
        );
    }
    assert!(ask_registration.get("presentCall").is_none());
    assert!(ask_registration.get("presentResult").is_none());
    assert!(exit_registration.get("presentCall").is_some());
    assert!(exit_registration.get("presentResult").is_some());
    let card = crate::interaction_tools::present_call_exit(&json!({ "plan": "# Ship\nx" }));
    assert_eq!(card["kind"], json!("other"));
    assert_eq!(card["card"], json!("generic"));
}

/// The session-query registrations equal the frozen catalog extraction
/// (Q97-A): the five descriptions ride the catalog-verbatim constants,
/// the parameter tables equal the frozen JSON, and the scheduling
/// faces pin the dsh shapes — the two search faces ride `timeoutMs`
/// and stay exclusive while the three reads advertise
/// `isConcurrencySafe`, and every face renders the string output.
#[test]
fn session_query_registrations_match_the_frozen_catalog() {
    assert_eq!(
        crate::catalog_gen::SESSION_SEARCH_DESCRIPTION,
        "Search prior sessions in the caller workspace and return the strongest matching event from each session."
    );
    assert_eq!(
        crate::catalog_gen::SESSION_EVENT_SEARCH_DESCRIPTION,
        "Search prior events in one authorized session; the current session excludes the step performing this call."
    );
    assert_eq!(
        crate::catalog_gen::SESSION_TRACE_DESCRIPTION,
        "Read the authorized session lineage around one session, including complete visible ancestor and descendant relationships."
    );
    assert_eq!(
        crate::catalog_gen::SESSION_EVENT_READ_DESCRIPTION,
        "Read one full unabridged event and optional neighboring raw-event summaries from an authorized session."
    );
    assert_eq!(
        crate::catalog_gen::SESSION_EVENT_TRACE_DESCRIPTION,
        "Read every direct replacement and relationship to a cited source event for one event in an authorized session."
    );
    for (composed, frozen) in [
        (
            crate::schema::session_search_parameters(),
            crate::catalog_gen::SESSION_SEARCH_PARAMETERS_JSON,
        ),
        (
            crate::schema::session_event_search_parameters(),
            crate::catalog_gen::SESSION_EVENT_SEARCH_PARAMETERS_JSON,
        ),
        (
            crate::schema::session_trace_parameters(),
            crate::catalog_gen::SESSION_TRACE_PARAMETERS_JSON,
        ),
        (
            crate::schema::session_event_read_parameters(),
            crate::catalog_gen::SESSION_EVENT_READ_PARAMETERS_JSON,
        ),
        (
            crate::schema::session_event_trace_parameters(),
            crate::catalog_gen::SESSION_EVENT_TRACE_PARAMETERS_JSON,
        ),
    ] {
        let frozen: Value = serde_json::from_str(frozen).expect("the frozen table parses");
        assert_eq!(composed, frozen);
    }
    // The required rows: query on both search faces, seq on both event
    // targets; the trace face keeps session_id optional.
    let search = crate::schema::session_search_parameters();
    assert_eq!(search["required"], json!(["query"]));
    assert!(search["properties"].get("availability").is_some());
    let event_search = crate::schema::session_event_search_parameters();
    assert_eq!(event_search["required"], json!(["query"]));
    let event_read = crate::schema::session_event_read_parameters();
    assert_eq!(event_read["required"], json!(["seq"]));
    let event_trace = crate::schema::session_event_trace_parameters();
    assert_eq!(event_trace["required"], json!(["seq"]));
    let trace = crate::schema::session_trace_parameters();
    assert!(trace.get("required").is_none());
    // The scheduling faces: the two search faces ride the baked search
    // timeout and stay exclusive; the three reads advertise
    // isConcurrencySafe; every face mounts execute/render/presentCall
    // and no presentResult.
    let searches = [
        crate::plugin_shell::session_search_registration(),
        crate::plugin_shell::session_event_search_registration(),
    ];
    let reads = [
        crate::plugin_shell::session_trace_registration(),
        crate::plugin_shell::session_event_read_registration(),
        crate::plugin_shell::session_event_trace_registration(),
    ];
    for registration in searches.iter().chain(reads.iter()) {
        assert_eq!(
            registration["execute"]["service"],
            crate::service::SERVICE_NAME
        );
        assert_eq!(
            registration["output"]["schema"],
            json!({ "type": "string" })
        );
        assert!(registration.get("presentCall").is_some());
        assert!(registration.get("presentResult").is_none());
    }
    for registration in &searches {
        assert!(
            registration.get("isConcurrencySafe").is_none(),
            "the search faces stay exclusive"
        );
        assert_eq!(
            registration.get("timeoutMs"),
            Some(&json!(crate::session_query_tools::BAKED_SEARCH_TIMEOUT_MS))
        );
    }
    for registration in &reads {
        assert_eq!(registration.get("isConcurrencySafe"), Some(&json!(true)));
        assert!(registration.get("timeoutMs").is_none());
    }
}
