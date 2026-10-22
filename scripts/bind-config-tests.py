#!/usr/bin/env python3
"""Bind implemented fr-config scenarios in the CONFIG task contract to real
cargo test selectors (Q15A, 2026-10-22 user ruling).

Idempotent: each pair asserts the current selector before replacing, so
re-running after `draft-specs` regeneration re-applies the binding map.
Scenarios whose behavior has no proving test yet are deliberately left
pending_* (never bind a selector that does not assert the scenario's Then).
"""

from pathlib import Path

SPEC = Path("specs/generated/task-req-fr-config-requirement.spec.md")

# (scenario title, expected current selector prefix, new selector)
PAIRS = [
    ("覆盖行仅翻转 disabled", "pending_req_fr_config", "toggle_only_override_keeps_other_keys"),
    ("覆盖行整键替换 config", "pending_req_fr_config", "config_override_replaces_wholesale"),
    ("覆盖行未命中 id 警告跳过", "pending_req_fr_config", "unmatched_and_mismatched_overrides_warn_and_skip"),
    ("覆盖行 name 不匹配警告跳过", "pending_req_fr_config", "unmatched_and_mismatched_overrides_warn_and_skip"),
    ("装配层次自底向上展开", "pending_req_fr_config", "stack_layers_ordered_bottom_up"),
    ("insert 行向根插入并保持首插序", "pending_req_fr_config", "insert_appends_and_later_layers_can_override"),
    ("profile 清单未知名 skipped 警告非致命", "pending_req_fr_config", "unknown_manifest_bundle_is_skipped_non_fatal"),
    ("profile 名校验拒绝非法名", "pending_req_fr_config", "profile_name_validation_rejects_dsh_rules"),
    ("profile 模板首用自动初始化", "pending_req_fr_config", "fresh_headless_profile_auto_inits"),
    ("未知 profile 名报错", "pending_req_fr_config", "unknown_profile_name_errors"),
    ("可选 patch 缺文件为空层", "pending_req_fr_config", "optional_patch_absent_is_empty_layer"),
    ("--patch 缺文件报错", "pending_req_fr_config", "required_overlay_absent_fails_startup"),
    ("patch 解析错误 fail-closed", "pending_req_fr_config", "optional_patch_parse_error_is_fatal"),
    ("编译产物为合法 loader 文档", "pending_req_fr_config", "compiled_document_is_valid_loader_config"),
    ("insert 行相对路径锚定到补丁文件目录", "pending_req_fr_config", "relative_insert_names_anchor_to_patch_dir"),
    ("覆盖行 name 与无前缀相对 name 不锚定", "pending_req_fr_config", "override_row_names_are_never_anchored"),
    ("单行 config 校验失败不阻塞其余槽位", "pending_req_fr_config", "schema_violation_fails_row_without_blocking"),
    ("查无 schema 的行跳过校验", "pending_req_fr_config", "absent_schema_skips_validation"),
    ("同一声明派生校验与 JSON Schema 投影", "pending_req_fr_config", "validation_and_projection_share_one_declaration"),
    ("超出受限子集的条件表达式 fail-closed", "pending_req_fr_config", "expression_outside_subset_rejects_document"),
    ("env 默认链与相等比较逐值求值", "pending_req_fr_config", "env_default_chain_matches_dsh_semantics"),
    ("裸 env 值读取作为配置值", "pending_req_fr_config", "bare_env_read_is_null_when_absent"),
    ("profile-context 布尔门控", "pending_req_fr_config", "profile_context_boolean_with_negation"),
    ("平台三元按字面语义求值", "pending_req_fr_config", "platform_ternary_and_equality"),
    ("第一方名解析为 wasm 被拒", "pending_req_fr_config", "first_party_wasm_resolution_is_rejected"),
    ("disabled 行使插件不装载", "pending_req_fr_config", "compiles_resolved_rows_and_excludes_disabled"),
    ("整行替换非键级合并", "pending_req_fr_config", "config_override_replaces_wholesale"),
    ("env 缺省表达式映射", "pending_req_fr_config", "env_default_chain_matches_dsh_semantics"),
    ("三层优先序", "pending_req_fr_config", "layered_three_layer_precedence_and_no_override"),
    ("已设名不被覆盖", "pending_req_fr_config", "layered_three_layer_precedence_and_no_override"),
    ("任一层违例两文件均不应用", "pending_req_fr_config", "layered_atomic_rejection_applies_neither_file"),
    ("读错误非 ENOENT 警告跳层", "pending_req_fr_config", "layer_missing_file_is_silent_and_read_error_warns_then_skips"),
    ("专名检查大小写不敏感", "pending_req_fr_config", "bootstrap_names_and_prefixes_case_insensitive"),
    ("cwd 即家目录时家层不读", "pending_req_fr_config", "layered_cwd_home_dedup_reads_directory_once"),
    ("env 解析方言对齐 Node util.parseEnv", "pending_req_fr_config", "parser_matches_node_parse_env_official_fixture"),
    ("dump 输出注释分层逐字同型", "pending_req_fr_config", "composes_layers_prints_verbatim_js_and_labels_sections"),
    ("default-only 恢复诊断不读不解析用户层", "pending_req_fr_config", "default_only_collects_bundle_layers_only_and_skips_broken_user_files"),
    ("from-default-profile 以出厂模板替换", "pending_req_fr_config", "from_default_profile_replaces_existing_profile"),
]


def main() -> None:
    lines = SPEC.read_text(encoding="utf-8").splitlines(keepends=True)
    bound = 0
    for title, expected, new in PAIRS:
        hits = [i for i, l in enumerate(lines) if l.startswith(f"Scenario: {title}\n")]
        if len(hits) != 1:
            raise SystemExit(f"scenario title not unique: {title!r} -> {len(hits)} hits")
        si = hits[0]
        ti = next(i for i in range(si + 1, len(lines)) if lines[i].startswith("  Test: "))
        current = lines[ti].removeprefix("  Test: ").strip()
        if current == new:
            continue
        if not current.startswith(expected):
            raise SystemExit(f"{title!r}: expected selector {expected!r}, found {current!r}")
        lines[ti] = f"  Test: {new}\n"
        bound += 1
    if bound == 0 and len(PAIRS) > 0:
        return
    SPEC.write_text("".join(lines), encoding="utf-8")


if __name__ == "__main__":
    main()
