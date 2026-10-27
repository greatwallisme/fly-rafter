// Rust guideline compliant 2026-10-28
//! Verbatim-prompt verification (PRD 22: script extraction, no hand-copying).
//!
//! Covers: regeneration drift against the dsh sources, the identity-sentence
//! brand mapping (PRD 10), and the structural placement rules dsh itself
//! tests (unique, integral, at least ten apart).

use std::process::Command;

use fr_system_prompt::orders::{CONTEXT_ORDERS, SECTION_ORDERS};
use fr_system_prompt::prompts::HARNESS_IDENTITY_TEXT;

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
