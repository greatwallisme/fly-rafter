//! Verbatim-prompt verification (PRD 22: script extraction, no
//! hand-copying) for the compaction constants: regeneration drift
//! against the dsh summarizer source, always-on through the extractor's
//! `--check` pass (the fr-system-prompt verify precedent; this lives in
//! the lib so the spec lifecycle's test discovery reaches it).

use std::process::Command;

fn dsh_source() -> String {
    std::env::var("FR_DSH_SOURCE")
        .expect("set FR_DSH_SOURCE to the deepseek-harness checkout (prompt constants verify against the sources)")
}

#[test]
fn compaction_constants_match_the_dsh_sources() {
    let status = Command::new("python3")
        .arg("../../scripts/extract-compaction-prompts")
        .arg("--check")
        .env("FR_DSH_SOURCE", dsh_source())
        .status()
        .expect("python3 is available to run scripts/extract-compaction-prompts");
    assert!(
        status.success(),
        "extract-compaction-prompts --check reported drift: rerun scripts/extract-compaction-prompts"
    );
}
