// Rust guideline compliant 2026-10-28
//! Verbatim-prompt verification (PRD 22: script extraction, no hand-copying).
//!
//! Covers: regeneration drift against the dsh sources, the identity-sentence
//! brand mapping (PRD 10), the structural placement rules dsh itself tests
//! (unique, integral, at least ten apart), and the complete order tables as
//! an always-on twin of the `--check` drift pass (these live in the lib so
//! the spec lifecycle's test discovery reaches them; the extractor owns
//! `orders.rs` itself).

use std::process::Command;

use crate::orders::{CONTEXT_ORDERS, SECTION_ORDERS};
use crate::prompts::HARNESS_IDENTITY_TEXT;

fn dsh_source() -> String {
    std::env::var("FR_DSH_SOURCE")
        .expect("set FR_DSH_SOURCE to the deepseek-harness checkout (prompt constants verify against the sources)")
}

#[test]
fn generated_constants_match_the_dsh_sources() {
    let status = Command::new("python3")
        .arg("../../scripts/extract-prompts")
        .arg("--check")
        .env("FR_DSH_SOURCE", dsh_source())
        .status()
        .expect("python3 is available to run scripts/extract-prompts");
    assert!(
        status.success(),
        "extract-prompts --check reported drift: rerun scripts/extract-prompts"
    );
}

#[test]
fn identity_sentence_is_brand_mapped() {
    assert!(
        HARNESS_IDENTITY_TEXT.contains("Fly Rafter"),
        "the identity sentence must name Fly Rafter: {HARNESS_IDENTITY_TEXT}"
    );
    assert!(
        !HARNESS_IDENTITY_TEXT.contains("DeepSeek"),
        "the identity sentence alone is brand-mapped (PRD 10)"
    );
}

#[test]
fn order_placements_are_unique_integral_and_spaced() {
    assert_eq!(SECTION_ORDERS.len(), 32, "the dsh SECTION_ORDERS table");
    assert_eq!(CONTEXT_ORDERS.len(), 7, "the dsh CONTEXT_ORDERS table");
    // dsh keeps section placements unique, integral, and at least ten
    // apart; context placements are unique and integral only (the policy
    // slots 110/115 pack deliberately).
    let mut sections: Vec<i64> = SECTION_ORDERS.iter().map(|(_, value)| *value).collect();
    let raw_len = sections.len();
    sections.sort_unstable();
    sections.dedup();
    assert_eq!(
        sections.len(),
        raw_len,
        "section order values must be unique"
    );
    for pair in sections.windows(2) {
        assert!(
            pair[1] - pair[0] >= 10,
            "adjacent section placements must stay at least ten apart"
        );
    }
    let mut contexts: Vec<i64> = CONTEXT_ORDERS.iter().map(|(_, value)| *value).collect();
    let raw_len = contexts.len();
    contexts.sort_unstable();
    contexts.dedup();
    assert_eq!(
        contexts.len(),
        raw_len,
        "context order values must be unique"
    );
}

/// The complete allocation tables, verbatim (dsh system-prompt/src/
/// index.ts:131-165 and :170-179; the `--check` pass pins the same tables
/// against the dsh sources — this is the always-on twin that needs no
/// checkout).
#[test]
fn orders_tables_match_the_dsh_values() {
    assert_eq!(
        SECTION_ORDERS,
        &[
            ("HARNESS_IDENTITY", -1000),
            ("DEPLOYMENT_PERSONA_PREFIX", 0),
            ("PLAN_POLICY", 500),
            ("TEAM_POLICY", 600),
            ("PTC_ONLY", 800),
            ("FILE_REFERENCE", 900),
            ("TOOL_BASH", 1000),
            ("TOOL_PWSH", 1010),
            ("TOOL_READ", 1100),
            ("TOOL_WRITE", 1200),
            ("TOOL_EDIT", 1300),
            ("TOOL_GLOB", 1400),
            ("TOOL_GREP", 1500),
            ("TOOL_JOBS", 1600),
            ("TOOL_PTY", 1700),
            ("TOOL_WEB_SEARCH", 2000),
            ("TOOL_WEB_FETCH", 2100),
            ("TOOL_LSP", 2200),
            ("TOOL_SESSION_QUERY", 2300),
            ("TOOL_GOAL", 2400),
            ("TOOL_WORKFLOW", 2600),
            ("TOOL_RALPH", 2700),
            ("TOOL_SUBAGENT", 2800),
            ("TOOL_REPORT", 2900),
            ("TOOL_COMPUTER_USE", 3000),
            ("MCP_SERVERS", 3100),
            ("TOOLS_SDK", 5000),
            ("DELIVERABLE_FILE_REFERENCES", 9000),
            ("STRUCTURED_OUTPUT", 9900),
            ("HARNESS_SOURCE", 10000),
            ("WEB_SURFACE", 10100),
            ("DEPLOYMENT_PERSONA_SUFFIX", 10200),
        ]
    );
    assert_eq!(
        CONTEXT_ORDERS,
        &[
            ("WORKING_DIRECTORY", 100),
            ("SANDBOX_POLICY", 110),
            ("APPROVAL_POLICY", 115),
            ("SUBAGENT_DELEGATION", 120),
            ("TEAM_IDENTITY", 125),
            ("HARNESS_SOURCE", 200),
            ("WEB_SURFACE", 210),
        ]
    );
}
