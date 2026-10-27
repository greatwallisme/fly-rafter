#!/usr/bin/env python3
"""Bind implemented fr-system-prompt scenarios in the PROMPT task contract
to real cargo test selectors (slice 8 batch 3, 2026-10-28; Q90-A ruling:
table-value scenarios bind the full-table pins, behavior scenarios carry
their own short names).

Binding discipline (doc/binding-audit.md section 7 — binding every selector
through this script means accepting all eight rules):

1. One test bound to several scenarios must assert every clause of each
   scenario's Then; partial coverage stays pending.
2. Every input the scenario Given names must appear in the test fixtures —
   no "equivalent" stand-ins.
3. Comments claiming assertions never count; the audit judges code asserts.
4. No println / zero-assert probes in any commit.
5. Before committing, diff the per-file `#[test]` list against this map;
   names and counts must match.
6. Mappings above two scenarios need a verification entry in the audit book
   first (doc/binding-audit.md section 10 covers this batch).
7. A Then half-covered by a module still awaiting its owning slice never
   binds (no premature bindings).
8. A Then conflicting with dsh truth goes to contract-errata ruling first;
   never bind the implementation side.

Idempotent: each pair asserts the current selector before replacing, so
re-running after `draft-specs` regeneration re-applies the binding map.
Deliberately left pending: working-directory context follows cwd (the
working-directory context provider registers from its own plugin, a
Phase 18 assembly — no proving test exists in this slice).

Multi-clause composites added with this batch (each asserts every clause of
its scenario's Then in one test): interpolation_and_tool_order_recompose_
the_assembly, equal_orders_and_unlisted_tools_sort_deterministically,
loader_enforces_size_budget_dedup_and_root_markers,
complete_conflict_and_refresh_context_only,
touched_changes_reinject_and_deletions_remove, plus the always-on
orders_tables_match_the_dsh_values twin in tests/prompts_verify.rs.
"""

import sys
from pathlib import Path

SPEC = Path("specs/generated/task-req-fr-prompt-requirement.spec.md")

# (scenario title, new selector)
PAIRS = [
    ("变量插值与 tool-order 重排", "interpolation_and_tool_order_recompose_the_assembly"),
    ("section 全序与 dsh 源一致", "orders_tables_match_the_dsh_values"),
    ("身份句默认渲染", "constructor_registers_identity_and_both_personas"),
    ("身份句可关闭与前缀", "constructor_omits_identity_when_disabled"),
    ("项目指令进入 agent-instructions", "compose_emits_the_baseline_frame"),
    ("提示词常量与 dsh 源逐字一致", "generated_constants_match_the_dsh_sources"),
    ("plan-mode 文案与 base patch 逐字", "generated_constants_match_the_dsh_sources"),
    ("无 AGENTS.md 不产生空 section", "compose_without_instructions_returns_none"),
    ("同序工具名排序与未列工具", "equal_orders_and_unlisted_tools_sort_deterministically"),
    ("注入为 user 消息 system-reminder 帧", "compose_emits_the_baseline_frame"),
    ("变更与删除再注入", "touched_changes_reinject_and_deletions_remove"),
    ("尺寸门控与去重", "loader_enforces_size_budget_dedup_and_root_markers"),
    ("complete section 唯一", "complete_conflict_and_refresh_context_only"),
    ("interpolate false 保留字面", "interpolate_false_keeps_literal_text"),
]

# The 32 per-entry SECTION_ORDERS table scenarios all bind the one
# full-table pin (Q90-A: table-value scenarios bind full-table pins; the
# audit book's section 10 carries the verification entry).
SECTION_ORDER_NAMES = [
    "HARNESS_IDENTITY", "DEPLOYMENT_PERSONA_PREFIX", "PLAN_POLICY",
    "TEAM_POLICY", "PTC_ONLY", "FILE_REFERENCE", "TOOL_BASH", "TOOL_PWSH",
    "TOOL_READ", "TOOL_WRITE", "TOOL_EDIT", "TOOL_GLOB", "TOOL_GREP",
    "TOOL_JOBS", "TOOL_PTY", "TOOL_WEB_SEARCH", "TOOL_WEB_FETCH", "TOOL_LSP",
    "TOOL_SESSION_QUERY", "TOOL_GOAL", "TOOL_WORKFLOW", "TOOL_RALPH",
    "TOOL_SUBAGENT", "TOOL_REPORT", "TOOL_COMPUTER_USE", "MCP_SERVERS",
    "TOOLS_SDK", "DELIVERABLE_FILE_REFERENCES", "STRUCTURED_OUTPUT",
    "HARNESS_SOURCE", "WEB_SURFACE", "DEPLOYMENT_PERSONA_SUFFIX",
]
for name in SECTION_ORDER_NAMES:
    PAIRS.append((f"section {name} 序值", "orders_tables_match_the_dsh_values"))


def main() -> None:
    lines = SPEC.read_text(encoding="utf-8").splitlines(keepends=True)
    bound = 0
    for title, new in PAIRS:
        hits = [i for i, line in enumerate(lines) if line.rstrip("\n") == f"Scenario: {title}"]
        if len(hits) != 1:
            raise SystemExit(f"scenario title not unique: {title} ({len(hits)} hits)")
        index = hits[0] + 1
        if not lines[index].startswith("  Test: "):
            raise SystemExit(f"no Test line under: {title}")
        current = lines[index].removeprefix("  Test: ").strip()
        if current == new:
            continue
        if not current.startswith("pending_req_fr_prompt"):
            raise SystemExit(f"{title}: already bound to {current}, refusing to rebind")
        lines[index] = f"  Test: {new}\n"
        bound += 1
    SPEC.write_text("".join(lines), encoding="utf-8")
    sys.stdout.write(
        f"applied {bound} changes ({len(PAIRS)} total in map, rest already bound)\n"
    )


if __name__ == "__main__":
    main()
