#!/usr/bin/env python3
"""Bind implemented fr-session scenarios in the SESSION task contract to real
cargo test selectors (slice E/F, 2026-10-23).

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
   first.
7. A Then half-covered by an unimplemented module never binds (no premature
   bindings).
8. A Then conflicting with dsh truth goes to contract-errata ruling first;
   never bind the implementation side.

Idempotent CONFIG-pattern binding: each pair asserts the current selector
before replacing, so re-running after `draft-specs` regeneration re-applies
the binding map. Scenarios whose behavior has no proving test yet are
deliberately left pending_* (never bind a selector that does not assert the
scenario's Then): title/session store, title LLM, system-prompt routing,
filename/header generation mismatch, request/header reason enumeration,
tool-schema order sensitivity, the plugin-owned event types awaiting their
owning plugins (approval/*, tool/ptc-dispatch*), the two-level abort codes
and the tool gate (agent-loop halves, withdrawn by F11/F13).
"""

from pathlib import Path

SPEC = Path("specs/generated/task-req-fr-session-v4.spec.md")

# (scenario title, new selector)
PAIRS = [
    ("v4 会话写入与读取回环", "materialize_append_read_round_trips_plain_and_zstd"),
    ("zstd 压缩回环", "materialize_append_read_round_trips_plain_and_zstd"),
    ("crash 孤儿 turn 的 interrupted 关闭", "crash_orphan_turn_with_no_step_closes_with_interrupted_only"),
    ("fork seed 的 forked 关闭", "fork_seed_marks_cut_and_closes_open_turn_with_forked_closers"),
    ("user/message 直载形态", "user_message_data_is_the_message_itself"),
    ("非 v4 文件被拒绝", "foreign_versions_refuse_before_shape_checks"),
    ("写路径校验失败不发布后继文件", "write_path_validation_failure_publishes_nothing"),
    ("独占发布与回滚", "republishing_identical_log_adopts_and_divergent_refuses"),
    ("后继世代 EEXIST 逐字节比对", "republishing_identical_log_adopts_and_divergent_refuses"),
    ("zstd 撕裂尾恢复", "zstd_torn_tail_keeps_the_committed_prefix_and_classifies_the_frame"),
    ("abort 补合成 tool/result", "unstarted_call_closes_with_not_started_error_and_no_source_seqs"),
    ("两级补偿错误码与文案", "started_call_closes_with_outcome_unknown_and_call_seq_source"),
    ("未知必需事件拒绝重建", "restore_gate_refuses_unknown_required_and_enforces_the_cut"),
    ("扩展事件与 end-seed 参与回环", "fork_seed_round_trips_with_the_inherited_cut"),
    ("SessionHeader 逻辑九字段与磁盘六必需键", "header_round_trips_through_the_logical_shape"),
    ("携带 seedLength 的头被拒绝", "header_shape_guard_covers_the_whole_table"),
    ("非法 replace 被拒", "illegal_replace_is_rejected"),
    ("tool/result 替换越权", "tool_result_rewrite_may_change_only_content"),
    ("折叠编码回环", "decode_seq_ranges_expands_and_rejects"),
    ("继承切点取最后一个 tagged end-seed", "fork_seed_round_trips_with_the_inherited_cut"),
    ("激活期声明聚合为读路径已知集", "dylib_artifact_loads_and_serves_the_vocabulary_service"),
    ("重复声明同类型装配失败", "declare_accepts_new_types_and_refuses_duplicates"),
    ("plugin 记录写入读取回环", "plugin_record_grammar_and_retention"),
    ("非 plugin: 语法类型拒绝", "plugin_record_grammar_and_retention"),
    ("series 重置基线弃 prefix reuse", "series_reset_drops_prefix_reuse"),
    ("tool-addition 经 headerSeq 解析进 updates", "additions_resolve_through_header_seq_into_updates"),
    ("deriveMessages 两次重建逐字节一致", "derive_messages_skips_empty_and_log_only_and_caches_tail_growth"),
    ("表面尾增长走增量投影", "derive_messages_skips_empty_and_log_only_and_caches_tail_growth"),
    ("contentGeneration 变化触发整表重建", "replacement_bumps_generations_and_derive_rebuilds"),
    ("空 assistant 投影为 null", "empty_assistant_projects_to_no_message"),
    ("canonicalHeader 空 tools 省略", "request_header_fold_last_wins_and_canonicalizes"),
    ("工具增删 developer 声明门控", "header_seq_required_exactly_when_additions_present"),
    ("node-0 重写白名单", "node_zero_system_head_rejects_non_system_rewrite"),
    ("ptc-dispatch 子调用不进请求", "ptc_subcall_records_stay_out_of_derived_messages"),
    ("五角色与七种 ContentBlock 往返无损", "roles_and_blocks_round_trip"),
    ("文件名世代与头版本不一致报错", "filename_header_version_mismatch_refuses"),
]

# The 14 core types x 触发/表面归属/回环 map to their per-type pin test
# (admission + dsh field set + both surface directions + disk round trip).
CORE_TYPES = [
    "turn/start", "turn/end", "step/start", "step/end", "user/message",
    "developer/message", "system/message", "assistant/message",
    "assistant/attempt", "tool/call", "tool/result", "request/header",
    "request/context", "session/end-seed",
]
for core in CORE_TYPES:
    per_type = core.replace("/", "_").replace("-", "_") + "_round_trips"
    PAIRS.append((f"事件 {core} 触发", per_type))
    PAIRS.append((f"事件 {core} 表面归属", per_type))
    PAIRS.append((f"事件 {core} 回环", per_type))


# Authorised outgoing selectors: a pair may replace a binding only when the
# current selector is listed here (the F1 matrix split and the F3/F9/F10
# rebinds). Anything else bound refuses, so accidental overwrites stay loud.
AUTHORISED_OUTGOING = {
    "every_core_event_type_admits_and_round_trips_with_its_surface_role",
    "replace_bounds_not_earlier_are_rejected",
    "header_shape_guard_covers_the_whole_table",
    "text_block_round_trips_with_verbatim_type_key",
    "republishing_an_existing_log_refuses_and_keeps_bytes_identical",
}

# Withdrawals: bindings whose Then carries an unimplemented module half go
# back to pending until the owning slice lands (F11/F13).
WITHDRAW = [
    ("两级补偿错误码与文案", "pending_req_fr_session_two_level_abort_codes"),
    ("工具增删 developer 声明门控", "pending_req_fr_session_tool_gate"),
]


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
        bound_to_real = not current.startswith("pending_req_fr_session")
        if bound_to_real and current not in AUTHORISED_OUTGOING:
            raise SystemExit(f"{title}: already bound to {current}, refusing to rebind")
        lines[index] = f"  Test: {new}\n"
        bound += 1
    for title, pending in WITHDRAW:
        hits = [i for i, line in enumerate(lines) if line.rstrip("\n") == f"Scenario: {title}"]
        if len(hits) != 1:
            raise SystemExit(f"scenario title not unique: {title} ({len(hits)} hits)")
        index = hits[0] + 1
        current = lines[index].removeprefix("  Test: ").strip()
        if current == pending:
            continue
        if current.startswith("pending_req_"):
            raise SystemExit(f"{title}: already pending ({current}), refusing to double-withdraw")
        lines[index] = f"  Test: {pending}\n"
        bound += 1
    SPEC.write_text("".join(lines), encoding="utf-8")
    total = len(PAIRS) + len(WITHDRAW)
    print(f"applied {bound} changes ({total} total in map, rest already bound)")


if __name__ == "__main__":
    main()
