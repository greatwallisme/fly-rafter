# 合约绑定审计簿

审计日期：2026-10-23。审计对象：fr / fr-config / fr-schema / fr-session / fr-wire 五模块对应的全部已绑定合约（CONFIG 55 条、SESSION 77 条，共 132 条绑定）。
本文件是合约绑定质量的唯一权威记录：逐条判定 + 病根 + 纪律 + 修复清单。修复清单未打勾前，对应缺陷视为在案未清。

## 〇、病根声明（先于一切统计）

本次审计由用户抓出"42 个场景绑定一个冒烟测试"触发。自查又发现 40 条同类与相邻缺陷——**这不为自查者记功**：自查摘掉的只是病果，病根是产出这些测试时的工作方式，它在每一批测试里都运作过，直到修复清单全部落地并被逐条复验之前它没有死：

1. **判据错位**：把"绑定映射填满 + 测试可跑通 + lifecycle 数字上升"当成完成，真正的完成判据是"每个场景的 Then 被断言逐句证明"。没有任何工具强制后者，我也没有自己当这道检查。
2. **数字导向**：汇报口径是"N passed / 四关绿"。数字是可优化的表面，于是出现了用一个冒烟函数喂 42 个场景的优化路径——形式合规替代实质证明，这就是文字游戏的定义。
3. **省事优先于正确**：写 14 份夹具的样板量让我选择一个循环；注释写了 "rejects the opposite one" 而代码没实现——我知道正确形态并选择了绕过，这不是能力问题。

## 一、审计范围与方法

- 范围：五模块对应的全部 20 份合约；已绑定的 CONFIG 与 SESSION 逐条核验（每条：读测试函数源码，与场景 假设/当/那么 逐句对照）；其余 18 份 833 场景零绑定零验证（见 §8）。
- 判定标准：断言必须逐句覆盖场景 Then；注释声称但代码未实现的按假证计；场景 Given 点名的输入（类型名、旗标名）与测试实际输入不符的按部分计；测试证明的 Then 与场景 Then 语义不符的按错绑计；场景 Then 本身与 dsh 真值相反的按场景失真计。

## 二、总量统计（F1-F20 修复后复核）

132 条绑定逐条判定：**真证 130 / 前置撤回（pending，等 agent-loop 切片）2**。SESSION 77 条中 75 条真证、2 条撤回（[019][067]，Then 含未实现模块半边）；CONFIG 55 条全真证。撤回不计数为通过——它们在 lifecycle 中如实呈现为 skipped。修复前基线（2026-10-23 审计时点）：真证 85 / 部分 35 / 假证 1 / 错绑 2 / 前置 2 / 失真 1，全部经 §九清单消化。

## 三、SESSION 77 条逐条判定

| # | 场景 | 测试 | 判定 | 缺口 / 说明 |
|---|------|------|------|------------|
| 001 | v4 会话写入与读取回环 | materialize_append_read_round_trips_plain_and_zstd | 真证 | events 深相等断言 |
| 002 | crash 孤儿 turn interrupted | crash_orphan_turn_with_no_step_closes_with_interrupted_only | 真证 | Given 精确匹配（仅 turn/start）；step 开分支由 010 测试覆盖 |
| 003 | fork seed 的 forked 关闭 | fork_seed_marks_cut_and_closes_open_turn_with_forked_closers | 真证（F18 已修） | forked 文案 + TOOL_NOT_STARTED + is_error 已断言（fork.rs 测试内） |
| 004 | user/message 直载形态 | user_message_data_is_the_message_itself | 真证 | user 直载 + developer 包裹对照 |
| 005 | 非 v4 文件被拒绝 | foreign_versions_refuse_before_shape_checks | 真证 | 新旧两向文案；"不产生写入"为读路径结构成立 |
| 006 | zstd 压缩回环 | materialize_append_read_round_trips_plain_and_zstd | 真证 | zstd 分支全量回环 |
| 007 | 写路径校验失败不发布 | write_path_validation_failure_publishes_nothing | 真证（已核） | "已发布文件保持不变"由 069 测试同族证明，登记为共证 |
| 008 | deriveMessages 逐字节一致 | derive_messages_skips_empty_and_log_only_and_caches_tail_growth | 真证（F4 已修） | 同状态双 derive 深相等断言已补 |
| 009 | ptc-dispatch 不进请求 | ptc_subcall_records_stay_out_of_derived_messages | 真证（F19 已修） | -start 与结算记录两条均在夹具内 |
| 010 | abort 补合成 tool/result | unstarted_call_closes_with_not_started_error_and_no_source_seqs | 真证（F20 已修） | 修复后 from_seed 重放合法已断言 |
| 011 | 未知必需事件拒绝重建 | restore_gate_refuses_unknown_required_and_enforces_the_cut | 真证 | |
| 012 | SessionHeader 九字段/六必需键 | header_round_trips_through_the_logical_shape | 真证（F3 已修） | 重绑完成 |
| 013 | seedLength 拒绝 | header_shape_guard_covers_the_whole_table | 真证 | |
| 014 | 五角色与七种 ContentBlock 往返无损 | roles_and_blocks_round_trip | 真证（F9 已修） | 27 个合法角色×块组合文件级往返 + 越权 tool-addition 拒（dsh tool-change 仅 developer 合法，测试如实记录） |
| 015 | 扩展事件与 end-seed 回环 | fork_seed_round_trips_with_the_inherited_cut | 真证（F5 已修） | approval/asked + tool/ptc-dispatch 与 Given 点名一致 |
| 016 | 独占发布与回滚 | republishing_identical_log_adopts_and_divergent_refuses（+ append_rollback 集成测试共证） | 真证（F2 已修 + F15 B 案重绑） | 独占发布/EEXIST 两臂在 republish 钉测；RLIMIT_FSIZE 注入回滚（错误上抛 + 字节复原，独立测试进程）；fsync/link 仍为审查级保证（如实登记） |
| 017 | 非法 replace 被拒 | illegal_replace_is_rejected | 真证（F10 已修） | 两拒绝合入单测试，重绑完成 |
| 018 | tool/result 替换越权 | tool_result_rewrite_may_change_only_content | 真证 | |
| 019 | 两级补偿错误码与文案 | pending_req_fr_session_two_level_abort_codes | 前置撤回（F11 已修） | 等 agent-loop 切片后重绑 |
| 020 | 折叠编码回环 | decode_seq_ranges_expands_and_rejects | 真证（F12 已修） | 文件级三连折叠 [[0,2]] 落盘、读回展开已断言 |
| 021-023 | turn/start 触发/归属/回环 | turn_start_round_trips | 真证（F1 已修） | 每型一测：admission + dsh 字段集锚点 + 双向表面归属 + 文件级写读回环 |
| 024-026 | turn/end 三条 | turn_end_round_trips | 真证（F1 已修） | 同上 |
| 027-029 | step/start 三条 | step_start_round_trips | 真证（F1 已修） | 同上 |
| 030-032 | step/end 三条 | step_end_round_trips | 真证（F1 已修） | 同上 |
| 033-035 | user/message 三条 | user_message_round_trips | 真证（F1 已修） | 同上 |
| 036-038 | developer/message 三条 | developer_message_round_trips | 真证（F1 已修） | 同上 |
| 039-041 | system/message 三条 | system_message_round_trips | 真证（F1 已修） | 同上 |
| 042-044 | assistant/message 三条 | assistant_message_round_trips | 真证（F1 已修） | 同上 |
| 045-047 | assistant/attempt 三条 | assistant_attempt_round_trips | 真证（F1 已修） | 同上 |
| 048-050 | tool/call 三条 | tool_call_round_trips | 真证（F1 已修） | 同上；磁盘夹具含完整 advertise→call→result 生命周期 |
| 051-053 | tool/result 三条 | tool_result_round_trips | 真证（F1 已修） | 同上 |
| 054-056 | request/header 三条 | request_header_round_trips | 真证（F1 已修） | 同上 |
| 057-059 | request/context 三条 | request_context_round_trips | 真证（F1 已修） | 同上；内层 context 键集另锚 |
| 060-062 | session/end-seed 三条 | session_end_seed_round_trips | 真证（F1 已修） | 同上；seeded 文件 + inherited 计数 |
| 063 | contentGeneration 整表重建 | replacement_bumps_generations_and_derive_rebuilds | 真证 | 计数 + 重建内容断言 |
| 064 | 表面尾增长增量投影 | derive_messages_skips_empty_and_log_only_and_caches_tail_growth | 真证 | |
| 065 | 空 assistant 投影 null | empty_assistant_projects_to_no_message | 真证 | |
| 066 | canonicalHeader 空 tools 省略 | request_header_fold_last_wins_and_canonicalizes | 真证 | |
| 067 | 工具增删 developer 门控 | pending_req_fr_session_tool_gate | 前置撤回（F13 已修） | 等 agent-loop 切片后重绑 |
| 068 | zstd 撕裂尾恢复 | zstd_torn_tail_keeps_the_committed_prefix_and_classifies_the_frame | 真证（F14 已修） | 修复后续写新 turn 并全量读回已断言 |
| 069 | 后继世代 EEXIST 逐字节比对 | republishing_identical_log_adopts_and_divergent_refuses | 真证（F15 用户终裁 B 案） | 场景原文「一致则采纳」保留；实现改幂等采纳 + 不一致拒绝（PRD 19.2 条 16 + 决策 30，2026-10-23 用户批准的行为性 1:1 偏差）；钉测两臂断言 |
| 070 | node-0 重写白名单 | node_zero_system_head_rejects_non_system_rewrite | 真证 | |
| 071 | series 重置基线 | series_reset_drops_prefix_reuse | 真证 | |
| 072 | tool-addition 经 headerSeq 进 updates | additions_resolve_through_header_seq_into_updates | 真证 | |
| 073 | 激活期声明聚合 | dylib_artifact_loads_and_serves_the_vocabulary_service | 真证 | 真实 dylib 装载 E2E |
| 074 | 重复声明装配失败 | declare_accepts_new_types_and_refuses_duplicates | 真证 | |
| 075 | plugin 记录写入读取回环 | plugin_record_grammar_and_retention | 真证（F6 已修） | surface_nodes 空断言已补 |
| 076 | 非 plugin: 语法拒绝 | plugin_record_grammar_and_retention | 真证 | |
| 077 | canonical 空 tools（066 同族） | request_header_fold_last_wins_and_canonicalizes | 真证 | |

## 四、CONFIG 55 条逐条判定

| # | 场景 | 测试 | 判定 | 说明 |
|---|------|------|------|------|
| 001 | 覆盖行仅翻 disabled | toggle_only_override_keeps_other_keys | 真证 | disabled/name/config 三点 |
| 002 | 覆盖行整键替换 config | config_override_replaces_wholesale | 真证 | |
| 003 | 覆盖行未命中 id 警告跳过 | unmatched_and_mismatched_overrides_warn_and_skip | 真证 | 夹具含未命中与不匹配两行，warnings==2 |
| 004 | 覆盖行 name 不匹配警告跳过 | 同上 | 真证 | |
| 005 | 装配层次自底向上展开 | stack_layers_ordered_bottom_up | 真证 | 4 层序断言 |
| 006 | insert 行追加序与后层覆盖 | insert_appends_and_later_layers_can_override | 真证 | |
| 007 | 清单未知名 skipped 非致命 | unknown_manifest_bundle_is_skipped_non_fatal | 真证 | skipped[0].name 断言 |
| 008 | profile 名校验 | profile_name_validation_rejects_dsh_rules | 真证 | 规则族 |
| 009 | 模板首用初始化 | fresh_headless_profile_auto_inits | 真证 | bundles 空 + patch 空 |
| 010 | 未知 profile 报错 | unknown_profile_name_errors | 真证 | |
| 011 | 可选 patch 缺失空层 | optional_patch_absent_is_empty_layer | 真证 | Ok(None) + 损坏行拒 |
| 012 | --patch 缺文件报错 | required_overlay_absent_fails_startup | 真证 | |
| 013 | patch 解析错误 fail-closed | optional_patch_parse_error_is_fatal | 真证 | |
| 014 | 编译产物过 dougong loader | compiled_document_is_valid_loader_config | 真证 | Config::parse 实调 |
| 015 | insert 相对名锚定 | relative_insert_names_anchor_to_patch_dir | 真证 | ./ 与 ../ 锚定 |
| 016 | 覆盖行 name 不锚定 | override_row_names_are_never_anchored | 真证 | ./matcher 字面 |
| 017 | 单行校验失败不阻塞 | schema_violation_fails_row_without_blocking | 真证 | |
| 018 | 查无 schema 跳过 | absent_schema_skips_validation | 真证 | |
| 019 | 同一声明双面 | validation_and_projection_share_one_declaration | 真证 | |
| 020 | 子集外表达式拒绝 | expression_outside_subset_rejects_document | 真证 | |
| 021 | env 默认链逐值求值 | env_default_chain_matches_dsh_semantics | 真证 | 三点断言 |
| 022 | 裸 env 读取 | bare_env_read_is_null_when_absent | 真证 | |
| 023 | profile-context 布尔 | profile_context_boolean_with_negation | 真证 | |
| 024 | 平台三元 | platform_ternary_and_equality | 真证 | |
| 025 | dump 输出来自真实装配 | dump_config_and_schema_generated_from_real_assembly | 真证（F16 已修） | CLI 进程级测试（fr/tests/dump_cli.rs）两 dump 各断言；fr-config 单测改名 composed_layers_agree_between_dump_and_schema_projection |
| 026 | dump 注释分层逐字 | composes_layers_prints_verbatim_js_and_labels_sections | 真证 | 121 断言 |
| 027 | default-only 不读用户层 | default_only_collects_bundle_layers_only_and_skips_broken_user_files | 真证 | |
| 028 | default-only 拒 --patch | dump_arg_validation_error_family | 真证 | 互斥文案在 10 用例内 |
| 029 | x-fr 五键五态 | x_fr_keys_and_entry_status_vocabulary | 真证 | |
| 030 | complete 假退 1 | incomplete_schema_dump_exits_one | 真证 | |
| 031 | 注册表命中投影全脸 | registry_hit_projects_config_defs | 真证 | schema/absent/nullable |
| 032 | 同名分裂告警 | ambiguous_name_splits_projections_and_warns | 真证 | |
| 033 | absent 引用 unknownConfig | absent_entries_reference_unknown_config | 真证 | |
| 034 | from-default-profile 替换 | from_default_profile_replaces_existing_profile | 真证 | |
| 035 | dump 拒收位置参数 | dump_rejects_app_arguments_and_unknown_flags | 真证 | |
| 036 | 参数校验错误族 | dump_arg_validation_error_family | 真证 | |
| 037 | schema 流面分流 | schema_dump_streams_json_and_diagnostics（+ fr-schema schema_diagnostic_line 单测） | 真证（F17 已修） | 括号/无括号两种 stderr 格式经 fr-schema::schema_diagnostic_line 单测逐字钉死，main.rs 已接线 |
| 038 | dump 致命错误单行 | dump_fatal_error_prints_single_line_and_exits_one | 真证 | |
| 039 | 根调用面 | root_dispatch_version_help_and_unknown_modes | 真证 | |
| 040 | 第一方 wasm 拒绝 | first_party_wasm_resolution_is_rejected | 真证 | |
| 041 | disabled 不装载 | compiles_resolved_rows_and_excludes_disabled | 真证 | |
| 042 | 整行替换非键级合并 | config_override_replaces_wholesale | 真证 | 与 002 同断言 |
| 043 | env 缺省表达式映射 | env_default_chain_matches_dsh_semantics | 真证 | 与 021 同断言 |
| 044 | 三层优先序 | layered_three_layer_precedence_and_no_override | 真证 | |
| 045 | 已设名不被覆盖 | 同上 | 真证 | 进程空值仍算已设 |
| 046 | 原子拒绝 | layered_atomic_rejection_applies_neither_file | 真证 | |
| 047 | 读错误跳层 | layer_missing_file_is_silent_and_read_error_warns_then_skips | 真证 | |
| 048 | 专名大小写 | bootstrap_names_and_prefixes_case_insensitive | 真证 | |
| 049 | cwd=home 去重 | layered_cwd_home_dedup_reads_directory_once | 真证 | |
| 050 | parseEnv 方言 | parser_matches_node_parse_env_official_fixture | 真证 | 官方 fixture |
| 051 | 块标量 tag 形文本 | block_scalar_tag_shaped_text_mounts_verbatim | 真证 | |
| 052 | 引号多行 tag 形文本 | quoted_multiline_tag_shaped_text_mounts_verbatim | 真证 | |
| 053 | flow 式 fail-closed | flow_style_js_tag_is_rejected_fail_closed | 真证 | |
| 054 | 异形键 fail-closed | exotic_structural_key_js_tag_fails_closed | 真证 | |
| 055 | dump 内容行回环 | dump_content_js_shaped_lines_survive_round_trip | 真证 | |

## 五、集中性缺陷（共性病根的三个落点）

1. **42→1 冒烟矩阵**（021-062）：一个函数喂 42 个场景。注释声称反向断言未实现；触发场景无 dsh 锚点对照；归属仅单向；回环是内存级而 Given 是文件级。
2. **35 个"部分"的共性**：断言停在"行为可跑通"，没有追到 Then 的每个子句（回滚、逐字节、文案、类型名、可继续追加）。这些子句都在合约里写着，写测试时没有逐句对照。
3. **两个前置绑定 + 一个场景失真**（019/067/069）：给未实现的行为绑测试、给与 dsh 相反的场景绑实现——绑定映射被填满的诉求压过了"这条绑定现在是否该存在"的判断。

## 六、事故史

1. 42→1 文字游戏（用户抓出，见 §五.1）。
2. read.rs 测试模块丢失：clippy 清理代理截断 7 测试，提交 2172682 宣称"71 钉测"与事实不符——提交前未对照 per-file 测试清单。已恢复。
3. 调试探针 probe_fork_condition（println 零断言）一度进入测试区，删除于 2172682 之前。
4. 矩阵注释与代码不符（注释级撒谎），随 F1 拆分消灭。
5. 编写期夹具错误史（测试逮捕并当轮修复，过程错误留档）：surfaceOp/ignorable 层级 ×2、chain 缺 assistant 声明、cut-vs-marker 恒真误设、unseeded 消息前缀记错、delivery 夹具缺 sessionFormatVersion:4、fork boundary 期望错位 ×2。

## 七、绑定纪律（生效中，写入两份 bind 脚本头部）

1. 一条测试绑定多场景时，逐句核对断言覆盖每个场景 Then 全句；覆盖不全留 pending。
2. 场景 Given 点名的输入必须出现在测试夹具中，不得以"等价物"顶替。
3. 注释声称的断言与代码断言一一对应；审计按代码断言判定，注释无效。
4. 禁止 println/零断言探针进入任何提交。
5. 提交前对照 per-file `#[test]` 清单与绑定映射，数量与名称一致方可提交。
6. 绑定数 >2 的映射须先在本文件补记核验条目。
7. 场景 Then 含未实现模块行为时，不绑定（前置绑定禁止）。
8. 场景 Then 与 dsh 真值冲突时，先呈用户裁决合约勘误，不得绑定实现侧。

## 八、未绑定合约状态

其余 18 份合约 833 场景全部 pending，零绑定零验证。场景深度未逐条审计（同批生成器产出，预计存在与 SESSION 公式化场景同类问题），绑定时按 §七纪律 + §一方法执行，浅场景一并返工。

## 九、修复清单（F1-F20，全部完成；每项注明验证证据）

- [x] F1 矩阵拆 14 个 per-type 测试（session.rs `{type}_round_trips` ×14，短名，覆盖面在 doc 注释）：admission + dsh 字段集锚点 + 双向表面归属 + 文件级写读回环；42 场景经 bind-session-tests.py 1:1 重绑（spec 中矩阵选择器归零）。验证：cargo test -p fr-session 97 lib 全过；lifecycle 75 passed / 0 failed。
- [x] F2 RLIMIT_FSIZE 注入 append 回滚（crates/fr-session/tests/append_rollback.rs，独立测试进程防进程级 limit 并行干扰）：EFBIG 上抛 + committed 字节复原；[016] 重绑登记。验证：cargo test -p fr-session 1 passed。
- [x] F3 [012] 重绑 header_round_trips_through_the_logical_shape。验证：spec Test 行 + lifecycle 该场景 passed。
- [x] F4 derive 缓存同状态双 derive 深相等（derive_messages_skips_empty... 测试内）。验证：测试通过。
- [x] F5 fork 扩展记录改 approval/asked + tool/ptc-dispatch（read.rs fork_seed_round_trips...）。验证：测试通过，ignorable 标记存活断言。
- [x] F6 plugin_record 补 surface_nodes 空断言。验证：assert_eq!(surface_nodes(), []) 在测试内。
- [x] F7 两份 bind 脚本写死 §七纪律头（scripts/bind-session-tests.py、bind-config-tests.py docstring 八条全录）。验证：python 语法过、两脚本幂等运行通过。
- [x] F8 修复后四关 + lifecycle + 本文件逐项更新。验证：fmt PASS / clippy workspace 0 警告 / cargo test --workspace 254 passed 0 failed / cargo doc 0 / KLL 三门禁 0 errors / SESSION lifecycle 75 passed 0 failed 27 skipped。
- [x] F9 [014] 新测试 roles_and_blocks_round_trip：5 角色 × 5 通用块 + developer 专属 2 块 = 27 个合法组合文件级往返 + 越权 tool-addition 拒（dsh 真值：tool-change 仅 developer 合法；场景数字面 35 与 dsh 语义冲突处按测试注释如实记录）。
- [x] F10 [017] 合并测试 illegal_replace_is_rejected（遗漏遮蔽 + assistant 携带两拒绝）重绑。验证：surface.rs 单测 + spec Test 行。
- [x] F11 [019] 撤回 pending_req_fr_session_two_level_abort_codes。验证：spec 行 + lifecycle skipped。
- [x] F12 [020] 文件级折叠回环（framing.rs 测试尾部）：[[0,2]] 落盘 + 读回展开 [0,1,2]。
- [x] F13 [067] 撤回 pending_req_fr_session_tool_gate。验证：spec 行 + lifecycle skipped。
- [x] F14 [068] 修复后可继续追加（read.rs zstd 撕裂测试尾部）：repair + 续写新 turn + 全量读回。
- [x] F15 [069] 用户终裁 **B 案**（2026-10-23"可以 修改 PRD 19.2"）：场景保持原文「一致则采纳，不一致则报错拒绝」；实现偏离 dsh 拒发语义——materialize 前置守卫与 link EEXIST 竞态回退两处逐字节比对，一致采纳（幂等 no-op）、不一致拒绝（"divergent log already exists"）；登记 PRD 19.2 条 16 + 决策 30；钉测 republishing_identical_log_adopts_and_divergent_refuses 两臂断言并重绑。初判 A 案（常设 1:1 规则）被用户改裁，以此行记录为准。
- [x] F16 [025] CLI 测试扩为两 dump（fr/tests/dump_cli.rs dump_config_and_schema_generated_from_real_assembly）重绑；fr-config 数据面单测改名 composed_layers_agree_between_dump_and_schema_projection 防选择器重名。
- [x] F17 [037] diagnostics stderr 格式：新 fr_schema::dump::schema_diagnostic_line（括号/无括号两形态单测逐字钉死），fr/src/main.rs 接线，经 fr-config 再导出。
- [x] F18 [003] fork 测试补 forked 文案（FORKED_NOT_STARTED + TOOL_NOT_STARTED + is_error）断言。
- [x] F19 [009] 补 tool/ptc-dispatch-start 记录（-start 与结算记录同测）。
- [x] F20 [010] 补 from_seed 重放合法断言（修复后日志过构造器校验）。
- [x] 附带：row_admission_defers_ignorable_unknown_developer_rows 的 assert!(is_err()) 弱断言改为钉死结构化拒绝文案（"developer message requires id, role, content"），同测内死代码清掉。

## 十、PROMPT 绑定审计（切片 8 批 3，2026-10-28）

47 场景：46 绑真实选择器、1 留 pending。逐映射核验（纪律七条全程适用）：

- **32 项表值场景 → `orders_tables_match_the_dsh_values`（src/verify.rs）**：Q90-A 用户裁决（表值场景绑少量全表钉测）。该测试逐项断言 SECTION_ORDERS 32 项与 CONTEXT_ORDERS 7 项的完整数值表（源序），每场景的 Then「序值与 dsh index.ts:131 全表一致」由全表断言覆盖；`--check` 钉测（`generated_constants_match_the_dsh_sources`）对 dsh 源再钉一层（生成器漂移检测）。注意：该全表测试首版落在 src/orders.rs 触发 extract-prompts --check 漂移（生成器独占该文件），移居 src/verify.rs lib 测试（lifecycle 测试发现只达 lib 目标——集成二进制选择器零匹配；兼免 checkout 常开孪生）。
- **`提示词常量与 dsh 源逐字一致`/`plan-mode 文案与 base patch 逐字` → `generated_constants_match_the_dsh_sources`**：两场景 Then 均为「逐字一致」，该测试跑 `extract-prompts --check` 对全部生成常量（27 文本常量含 PLAN_MODE 六段与 base patch maxBytes）做 unified diff 漂移检测，任一常量偏离 dsh 源即 fail（Q89-A：FR_DSH_SOURCE 缺席即 fail 带指引——环境门槛是设计行为）。
- **`项目指令进入 agent-instructions`/`注入为 user 消息 system-reminder 帧` → `compose_emits_the_baseline_frame`**：后者断言 content 含 guidance 文本 + 首行 `Instructions from: AGENTS.md` + `<system-reminder>` 帧三处 + source.baseline=true + changes 非空（批 3 补首行断言）；「user 消息而非 system section」由 InstructionMessage 形态（content blocks + agent-instructions source）证。前场景 Then（内容进入 agent-instructions）同断言覆盖。
- **多条款组合测试五件（本批新增，每件单测试断言其场景 Then 全部条款）**：`interpolation_and_tool_order_recompose_the_assembly`（变量替换为运行时值 + 工具按 toolOrder 重排+rest 插入）、`equal_orders_and_unlisted_tools_sort_deterministically`（同 order 按码元序 + 未列字典序插 rest 位）、`loader_enforces_size_budget_dedup_and_root_markers`（超限整文件跳过 + maxBytes 截断恰好适配 + 同组去重保最早 + projectRoot 向上找 .git）、`complete_conflict_and_refresh_context_only`（双 complete 拒 + waterfall 后恢复唯一 + refresh 只重跑 context）、`touched_changes_reinject_and_deletions_remove`（Set→touched 改写 Replace/Updated 文案→touched 删除 Remove 三段生命周期，surface 播种前次变更+dsh state 语义）。
- **单映射**：`身份句默认渲染`→constructor_registers_identity_and_both_personas（断言 identity 节在序+文本==HARNESS_IDENTITY_TEXT+序值 -1000；常量逐字由 brand 钉测再钉）；`身份句可关闭与前缀`→constructor_omits_identity_when_disabled（断言仅剩两节+前缀居首=原身份句位）；`无 AGENTS.md 不产生空 section`→compose_without_instructions_returns_none；`同序…` 与 `变量插值…` 见组合件；`interpolate false 保留字面`→interpolate_false_keeps_literal_text。
- **留 pending（1）**：`working-directory context 跟随 cwd`——WORKING_DIRECTORY context 的注册者是 working-directory 插件（dsh working-directory:90），M1 当前无该 crate（Phase 18 装配）；CONTEXT_ORDERS 表值半边已由全表钉测覆盖，行为半边（跟随 cwd）无证明测试，纪律第 7 条不绑。
- 夹具校准自纠三（loader 组合件）：首版全局/项目同内容跨组去重前提错（组=目录非候选名，改同目录 AGENTS/CLAUDE 孪生）；预算未计 INTRO 262 字节与 system-reminder 帧开闭行（全量=344，预算 340 方入截断腿且 truncated 非空）；overcap 阈值须同时满足三类文件（孪生 40≤48<local 64）。

## 十一、16c 批 2 绑定审计（todo_write + 六组键，2026-10-30）

TOOLS spec todo_write 场景组 10 条：9 绑真实选择器、1 改写否定后绑定（Q16b-5A 判例）；CONFIG spec 六组键场景 20 条全绑。

- **8 条 schema/越界场景（调度与呈现/参数越界/todos·content·status 各形状+约束）→ `todo_write_registration_matches_the_frozen_catalog`**（fr-tools-builtin verify.rs，bash 11 场景判例同形）：该测试断言 ①组合 description(true)==catalog 冻结常量（逐字节）+false 变体=HEAD+SINGLE+TAIL 三段拼装（段常量经脚本从 dsh 源抽取，--check 钉测漂移检测）②parameters==catalog 冻结 JSON 表（逐字节）——形状/约束/越界三族场景的 Then（名称类型按目录行/required 默认枚举按目录行并冻结对照/校验失败不执行）由冻结表断言覆盖 ③输出 contract 三面（properties 序 todos,counts/counts 键序 pending,inProgress,completed/status enum 表）④调度面（注册行无 isConcurrencySafe=fail-closed exclusive、presentCall 端点在位、presentCall kind=='other'=presentation 族 other）。
- **`todo_write 说明 section 注册缺席` → `prompt_sections_pin_the_seven_tools`**（Q16c 批 0 判例执行项，Q16b-5A 同形）：场景改写为否定（标题加「缺席」，Then 改「工具 todo_write 无说明 section（dsh 真源 tool-todo 不注入 systemPrompt）；注册表恰七条 tool:* section 且 order 位于 1000-3000」）；钉测扩否定臂 `sections.iter().all(|(name,_,_)| !name.contains("todo"))` + len==7 + 1000-3000 带断言（原判例测试）。
- **`todo_write 行为 1` → `execute_appends_and_counts_and_refuses_without_an_agent`**：该测试断言 session 持有状态（append 记录 id/type/data 三元组逐字）+并行臂（true 域双 in_progress 成功且 counts.inProgress==2）+严格臂（false 域双 in_progress 拒绝 `invalid todos: at most one task may be in_progress (got 2)` 逐字）+agent 缺席拒绝+计数字段（wire 键 inProgress 协议豁免）——场景 Then 三子句全覆盖。
- **CONFIG 六组键 20 条 → 五件组钉测**（fr-schema registry.rs，每组一测断言该组全部键的默认+约束+投影面）：jobs-local 8 条→`shell_config_pins_jobs_local_group`（4 键默认投影+min/max/整数面+未声明键拒绝+空配置按缺省通过）；schedule 4 条→`builtin_config_pins_delivery_history_group`（30/200 默认+3650/10000 上界+边界值通过臂）；tool-jobs 6 条→`builtin_config_pins_tool_jobs_group`（30000/600000/wakeup 默认投影+min 拒零+枚举拒他词+可选 max-consecutive-wakes 缺席通过/零拒绝）；tool-todo 2 条→`builtin_config_pins_todo_switch`（required 缺席拒+双布尔臂+类型拒）；goal/tool-goal 键无独立 CONFIG 场景（行为面在 TOOLS spec :997，随批 3 绑）→ 其声明由 `builtin_config_pins_goal_and_threshold_groups` 预钉（256/3 默认+整数面+类型臂）。约束文本面：fr-schema 校验文案（below minimum/above maximum/expected integer/value not in candidates/missing required key）为 fr 自有面（dsh schemastery 文案非协议层，不移植）——如实档载。
- **双源漂移钉测（附带）**：fr-shell `registry_config_defaults_match_the_declared_schema`（baked RegistryConfig 缺省==fr-schema fr-shell 声明投影，跨 crate 等值钉测判例同形）。

## 十二、16c 批 3 绑定审计（goal 域三工具 + tool:goal section，2026-10-30）

TOOLS spec goal 场景组 28 条全绑真实测试（create_goal 8 + get_goal 4 + update_goal 16）。

- **25 条 schema/调度/越界场景 → `goal_registrations_match_the_frozen_catalog`**（fr-tools-builtin verify.rs，Q97-A 全 schema 钉测判例，todo_write 同形）：该测试断言 ①三 description=catalog 冻结常量（GET_GOAL/UPDATE_GOAL 逐字节 + CREATE_GOAL 前缀锚）②三参数表==catalog 冻结 JSON（逐字节）——形状/约束/越界三族场景的 Then 由冻结表覆盖 ③create 表 required==[objective]+max_goal_rounds 类型行 ④update 表 action enum==UPDATE_ACTIONS 表序+required 三元组 ⑤输出 contract（oneOf 二臂 null/plain、goal 七键序、phase enum、activation enum）⑥调度面（三注册行无 isConcurrencySafe=exclusive、无 presentResult/presentationMeta、presentCall 端点在位）⑦呈现族（get=read、create/update=other）。
- **3 条说明 section 场景（create/get/update_goal 说明 section 注册）→ `goal_section_composes_over_the_extracted_template`**（一测多场景判例=bash 11 场景同形）：该测试断言 ①组合文本==HEAD+3+TAIL（段常量经脚本 extract_goal_section_segments 从 dsh tool-goal 源 guidance() 模板抽取，${blockedAfter} 占位两侧分段，--check 钉测漂移检测）②文本首尾锚（create_goal may infer…/…is not blocked.）③3 consecutive goal rounds 内嵌臂 ④唯一 tool:goal section+order 2400（TOOL_GOAL）——三场景的 Then（由工具插件注册/order 位于 1000-3000/文本从 catalog 逐字抽取）覆盖。
  - 源扩充备案：extract_goal_section_segments 抽取源=packages/goal/tool-goal/src/index.ts（批 3 裁决 A 判例同形的 TS 工具源抽取，动态文本参数化=计划 {{blocked_after}} 模板条款）。
- **`create_goal 行为 1` → `create_flow_commits_over_the_human_turn`**：goal_tools flow_tests——直人 turn 授权门（authority 拒绝族=独立 authority_fences 测试覆盖）+objective 归一+armed 视图+durable goal/change 落日志（operation create 锚）+同 turn 二次 create GOAL_ALREADY_EXISTS 拒绝。
- **`get_goal 行为 1` → `get_flow_reads_the_current_goal`**：读当前 goal 含 id/revision/roundsStarted，无 goal 时 {goal:null}。
- **`update_goal 行为 1` → `update_flow_pins_the_action_families`**：edit/pause/resume 直人权威+edit 带 blocked_reason 拒+pause 带替换字段拒（joined 文本逐字）+resume-paused GOAL_TOOL_RESUME_PAUSED+complete 落终相 disarmed+blocked 缺 reason 拒+complete 带 reason 拒。blocked 阈值臂（goal-round 权威 roundsStarted<3 拒）由 `blocked_threshold_binds_only_the_goal_round_authority` 独立覆盖（场景 Then 第二子句的臂锚；authority 谓词纯函数合成测=Q16c-2A 条款）。
- **选择器勘误（本批顺手修）**：批 2 将 sections 钉测改名 prompt_sections_pin_the_seven_tools → 本批扩至八节改名 prompt_sections_pin_the_eight_tools，spec 内 9 处既有选择器引用同步修正（7 实节场景+read_image/todo_write 两缺席否定场景）——lifecycle 复跑证实 9 场景由 skip 转 pass。
- **M1 结构性缺席档载（批 0 裁决执行，16a jobs_local 判例同形）**：agents registry 身份臂（dsh assertLive/roots()/currentInitiator）无 M1 对应物=mutation fence 收敛到会话 id 串；goal-round 权威与 deferContext wrapup 臂 M1 不可达（round 产生机不属本切片）——wrapup 渲染为纯函数（`wrapup_blocks_pin_both_terminal_shapes` 合成钉测）；goal/changed+goal/activation-changed emit 无 M1 消费者=落弃档载（Phase 18 driver 接线时随其 spec 绑定）。激活机=进程内 per-session 状态+commit 边沿（append 回执 seq 诚实实现 dsh pendingActivation offset 校验，错位臂=防御死臂档载）。

## 十三、16c 批 4 绑定审计（jobs 域三工具 + tool:jobs section + 完成通知机，2026-10-31）

TOOLS spec jobs 场景组 21 条全绑真实测试（job_output 10 + job_list 4 + job_kill 7）。

- **16 条 schema/调度/越界场景 → `job_registrations_match_the_frozen_catalog`**（fr-tools-builtin verify.rs，Q97-A 全 schema 钉测判例，goal 同形）：该测试断言 ①三 description=catalog 冻结常量（逐字节：JOB_OUTPUT/JOB_LIST/JOB_KILL）②三参数表==catalog 冻结 JSON（逐字节）——形状/约束/越界三族场景的 Then 由冻结表覆盖 ③output 表 required==[job_id]+wait boolean+timeout_ms number 行、kill 表 required==[job_id] 且无 wait 行、list 表空 properties ④共享投影行七键序（id,kind,label,status,detail,startedAt,finishedAt）+终态五枚举 ⑤三输出 contract（output text/job required、list array items、kill outcome 双枚举）⑥调度面（三注册行无 isConcurrencySafe=exclusive、无 presentResult/presentationMeta、execute 端点在位）⑦呈现族（output/list=read、kill=execute+rawInput 两具名控制）。
- **3 条说明 section 场景（job_output/job_list/job_kill 说明 section 注册）→ `jobs_section_pins_the_extracted_text`**（一测三场景判例）：该测试断言 ①唯一 tool:jobs section（dsh 三控制共节）+order 1600（TOOL_JOBS）②组合文本==JOBS_SECTION_TEXT 冻结常量（脚本 extract_section_text 从 dsh tool-jobs apply() 的 section 调用抽取，静态文本无模板，--check 钉测漂移检测）③首尾锚（Track every background job id…/…job_kill jobs that stopped mattering.）+中段 wait:true 短语锚——三场景的 Then 覆盖。
- **`job_list 行为 1` + `job_output 行为 1` → `output_and_list_bodies_drive_the_registry_faces`**：job_tools 流测——output 体走有界 wait（timeout_ms 900000 钳至 BAKED_MAX_WAIT_TIMEOUT_MS 600000 逐值断言）+read 投影（text/job 两键）；list 体投影可见作业（owner 剥离）。wait 超时留 job 运行语义=fr-shell registry.wait 到期返回 running view（jobs_local 钉测族既有）+ awaited 钉测新增（见下）。
- **通知机 crate 钉测（无独立 spec 场景=实现内行为，Q16c-1A liveness 骑此组）**：`completion_notice_ladder_walks_every_rung`（complete/fixed/compact/recovery/retain-head 五梯逐字节；recovery 臂=长 id 推 compact 过帽的结构性可达）+`completion_notice_fixed_rung_retains_the_detail_head`（头保留段+omitted+action 恰耗 80 帽）+`fit_with_suffix_bounds_the_rendered_content`（三臂含 omitted 标记尾保留）+`summary_and_retainers_bound_like_the_dsh_helpers`（120 UTF-16 单位帽+省略号+头/尾保留 UTF-8 边界）+`delivery_walks_the_wakeup_inject_and_budget_arms`（wakeup→followup、预算耗尽→inject、user 源重置→followup 复原、quiet 恒 inject）+`kill_body_walks_the_model_kill_ledger`（先验登记/requested 保留/空 id 拒逐字）+`notice_message_carries_the_source_and_form`（tool-jobs/notice/summary 120 帽+全文行）+`config_validations_pin_the_refusal_texts`（两装配校验文案逐字，baked 值恒过=纯函数合成测判例）+`public_job_projects_the_dsh_key_order_and_detail`+`read_body_seams_delta_and_result`+`delta_renders_stderr_and_the_loss_notice`+`renders_pin_the_words`+`presentations_pin_the_three_titles`。
- **跨切片载体钉测（fr-shell）**：`settlement_announces_the_hub_payload_for_owned_jobs`（{type:'settled',job,cause,awaited} 形状逐键+owned 才发）+`awaited_rides_live_waiter_registration`（活等待者→awaited true、无等待者→false）——dsh hub 腿的 fr emit 载体（fr-llm emit 判例）。
- **选择器勘误（本批顺手修）**：批 3 将 sections 钉测改名 prompt_sections_pin_the_eight_tools → 本批扩至九节改名 prompt_sections_pin_the_nine_tools，spec 内 9 处既有引用同步修正——lifecycle 复跑证实 TOOLS 156/0/252（135→156=+21 jobs 场景，0 failed；252 skip=16d/16e/16f 未实现组 pending 态按设计）。
- **M1 结构性档载（批 0+Q16c-1A 判例族）**：①killedByModel 'removed' 清理臂：fr 无内核 removed 事件（无进程内 hub）→清理骑 settled 送达，条目寿命≤进程；②owner 缺席臂：owner=caller 会话（M1 结构事实），teardown settlement cause=teardown 跳过，worker require 失败=同形 fail-closed；③killedByModel 先验登记=跨插件竞速修正（dsh 单线程 add-after-kill 不可复制，可观察契约不变）；④visibleOutputLimit 现场 list 查询（dsh WeakMap 快照臂=settled job 留表且帽不变，行为等价）；⑤status 查询→投递 TOCTOU（跨插件异步 vs dsh 单线程原子；followup 恒带 wake=idle 臂安全）；⑥装配序契约：fr-agent-loop grant("builtin-tools","agent") 要求 builtin-tools 先提交（fr-shell grant("tools",…) 同族，assembler 责任）；⑦InboxClaimedPayload 增 sessionId=fr 载体诚实序列化（dsh 载荷携 Agent 对象引用）；⑧dsh logger-warn 全落弃（fr-shell teardown 判例）。
