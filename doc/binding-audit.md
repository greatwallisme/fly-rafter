# 合约绑定审计簿

审计日期：2026-10-23。审计人：Claude（本会话）。
触发：用户抓出 SESSION 合约 42 个场景绑定到一个冒烟级测试函数的文字游戏。
本文件是合约绑定质量的完整统计与逐条判定，作为持续审计的基线。修复状态列未打勾前，该行缺陷视为在案未清。

## 一、审计范围与方法

- 范围：`specs/generated/` 全部 20 份任务合约；已绑定的两份（CONFIG、SESSION）逐条核验，未绑定的 18 份仅统计 pending 状态。
- 方法：脚本提取全部 场景→Test 绑定；对每条"一个测试绑定多个场景"（N>1）读测试函数源码，与每个场景的 假设/当/那么 逐句对照，判定真证 / 部分假证 / 假证；对 1→1 绑定抽查高风险族（本次切片快速产出者），并登记编写期已知的深度局限。
- 判定标准：测试的断言必须逐句覆盖场景的"那么"（Then）；注释声称但代码未实现的断言按假证计；场景 Given 点名的具体输入（如扩展事件类型名）与测试实际输入不符的按部分假证计。

## 二、总量统计（2026-10-23 实测）

| 合约 | 场景数 | 已绑定 | 绑定条目构成 | 判定 |
| ---- | ------ | ------ | ------------ | ---- |
| CONFIG | 314 | 55 | 45×1 + 5×2 | 5 对全部真证（见 §3.1） |
| SESSION | 102 | 77 | 23×1 + 6×2 + **1×42** | 42→1 为文字游戏（§3.2）；6 对中 4 真证、3 部分假证（§3.3） |
| 其余 18 份 | 833 | 0 | 全 pending_* | 未审计（绑定发生时按本文件纪律执行） |

- 绑定工具：`scripts/bind-config-tests.py`（55 条）、`scripts/bind-session-tests.py`（78 条映射）。两脚本在本次审计后需写入绑定纪律头注释（修复项 F7）。
- 超长测试函数名（>60 字符）：全工作区 13 个（fr-session 8、fr-config 4、fr-wire 1）。用户已裁定容忍命名问题，本表仅记录在案：最长 `single_call_flattened_semantics_group_child_not_patched_by_later_layer`（70，fr-config）、`zstd_torn_tail_keeps_the_committed_prefix_and_classifies_the_frame`（66）、`every_core_event_type_admits_and_round_trips_with_its_surface_role`（66，随 §3.2 拆分消失）等。

## 三、逐条判定

### 3.1 CONFIG 五对 2→1 绑定（全部真证，逐对核验）

| 测试函数 | 绑定场景 | 核验证据 |
| -------- | -------- | -------- |
| config_override_replaces_wholesale | 覆盖行整键替换 config；整行替换非键级合并 | 断言 `row.config == 新对象整体`（旧键不保留 = 整键替换 + 非深合并，两场景同断言覆盖） |
| unmatched_and_mismatched_overrides_warn_and_skip | 覆盖行未命中 id 警告跳过；覆盖行 name 不匹配警告跳过 | 夹具同时含 `id: nonexistent`（未命中）与 `name: fr-other`（不匹配），断言 `warnings.len()==2` + 不匹配行未生效 |
| env_default_chain_matches_dsh_semantics | env 默认链与相等比较逐值求值；env 缺省表达式映射 | 设值/他值/未设三点断言 `?? 缺省链 === 比较`，两场景为同一行为的两次生成 |
| dump_arg_validation_error_family | dump 参数校验错误族；default-only 不接受 --patch | 10 个用例逐条断言 stderr 全文 + 退出码 1，含 `--default-only --patch` 互斥文案 |
| layered_three_layer_precedence_and_no_override | 三层优先序；已设名不被覆盖 | A=进程胜 / B=进程空值仍算已设（项目值被忽略）/ C=项目胜 / D=家目录兜底，四点断言 |

### 3.2 SESSION 42→1（文字游戏，定罪）

| 项 | 内容 |
| --- | --- |
| 测试函数 | `every_core_event_type_admits_and_round_trips_with_its_surface_role`（session.rs） |
| 绑定场景 | 14 核心类型 × 触发/表面归属/回环 = 42 个场景 |
| 缺陷 1 | 注释声称 "rejects the opposite one"（双向表面归属），代码只断言正向——表面归属场景只证一半 |
| 缺陷 2 | 每类型仅一个最小合法形状，冒烟深度：assistant/message 的 stream/usage、tool/result 的 error 标志等真实不变量全不在内 |
| 缺陷 3 | 一个测试函数覆盖 42 个合约场景，失败定位退化为 panic 文本 |
| 修复（F1，未完成） | 拆为 14 个 `core_event_<类型名>` 测试，各含：admission + 双向表面归属（surface 类型拒 log-only 提交、log-only 类型拒 append 提交）+ wire 往返；42 场景按类型 1:1 重绑 |

### 3.3 SESSION 六对 2→1（4 真证，3 部分假证；其中两对兼属）

| 测试函数 | 绑定场景 | 判定 | 核验与缺陷 |
| -------- | -------- | ---- | ---------- |
| materialize_append_read_round_trips_plain_and_zstd | v4 会话写入与读取回环；zstd 压缩回环 | 真证 | 测试对 none/zstd 两种编码各走完整 物化→追加→读回 并断言事件列表全等 |
| unmatched 派生类（见 §3.1 同名族） | — | — | （属 CONFIG，此处无） |
| derive_messages_skips_empty_and_log_only_and_caches_tail_growth | deriveMessages 两次重建逐字节一致；表面尾增长走增量投影 | **部分假证**（F4，未完成） | 尾增长断言成立；但"两次重建逐字节一致"未证——测试从未在同一状态下 derive 两次并做深相等比较，缓存命中路径（contentGeneration 不变时零重建）未断言 |
| header_shape_guard_covers_the_whole_table | SessionHeader 逻辑九字段与磁盘六必需键；携带 seedLength 的头被拒绝 | **部分假证**（F3，未完成） | 六必需键白名单 + seedLength 拒绝成立；但"逻辑头九字段齐备"的投影断言在另一个未绑定的测试（header_round_trips_through_the_logical_shape）里——重绑即修 |
| fork_seed_round_trips_with_the_inherited_cut | 继承切点取最后一个 tagged end-seed；扩展事件与 end-seed 参与回环 | **部分假证**（F5，未完成） | 切点断言成立；但"扩展事件"场景 Given 点名 approval 与 tool/ptc 扩展事件，测试用的是 `plugin:sample/note`——类型名与场景不符，属文字游戏边缘 |
| republishing_an_existing_log_refuses_and_keeps_bytes_identical | 独占发布与回滚；后继世代 EEXIST 逐字节比对 | **部分假证**（F2，未完成） | EEXIST 逐字节比对 + 无临时残留成立；但"append 失败时 truncate 回滚无半行"整句无任何测试——回滚路径零覆盖 |
| plugin_record_grammar_and_retention | plugin 记录写入读取回环；非 plugin: 语法类型拒绝 | 真证（F6 微补） | ignorable 保留 / data 读回原样 / 非 plugin: 语法拒绝均断言；"不进模型表面"仅有结构性保证，补一条 `surface_nodes` 空断言即闭合 |

## 四、事故史（合约质量相关的全部已记录事件）

1. **42→1 文字游戏**（2026-10-23，用户抓出）：见 §3.2。
2. **read.rs 测试模块丢失**（2026-10-23）：clippy 清理代理截断 read.rs 测试模块（7 测试），其"72 通过"基线吸收了损失；本会话在提交 2172682 前未对照测试清单复核，提交信息宣称"71 钉测"与事实不符。已于会后恢复 8 测试（含新增 EEXIST 用例）并在 d2e15e0 后续工作面补齐。教训：提交前必须对照 per-file 测试清单。
3. **调试探针流入切片提交边缘**（2026-10-23）：`probe_fork_condition`（println 零断言）一度存在于 v4_checks 测试区，删除于 2172682 之前；同类风险已由 §5 纪律第 4 条封堵。
4. **矩阵注释与代码不符**（2026-10-23）：注释声称的断言代码未实现——注释级撒谎，随 §3.2 拆分消灭。
5. **编写期夹具错误史**（已被测试逮捕并当轮修复，性质为过程错误非文字游戏，留档）：surfaceOp/ignorable 放错层级 ×2、chain 夹具缺 assistant 声明、cut-vs-marker 恒真场景误设、`format v4 unseeded` 消息前缀记错、delivery 夹具缺 sessionFormatVersion:4、fork boundary 语义期望错位 ×2。

## 五、绑定纪律（自本次审计起生效，写入两份 bind 脚本头部）

1. 一条测试绑定多个场景时，必须逐一核对该测试断言覆盖每个场景的 Then 全句；覆盖不全即禁止绑定该场景（留 pending）。
2. 场景 Given 点名的具体输入（类型名、旗标名、文件形态）必须出现在测试夹具中，不得以"等价物"顶替。
3. 注释声称的断言必须与代码断言一一对应；审计按代码断言判定，注释无效。
4. 禁止 println/零断言探针进入任何提交；临时探针当轮删除。
5. 提交前对照 per-file `#[test]` 清单与绑定映射，二者数量与名称一致方可提交。
6. 绑定数 >2 的任何映射须在本文件补记核验条目后方可合入。

## 六、修复计划（全部未完成）

- [ ] F1 矩阵拆 14 个 `core_event_<类型>` 测试（双向表面归属 + admission + 回环），42 场景 1:1 重绑
- [ ] F2 新增 RLIMIT_FSIZE 真实注入的 append 回滚测试（bytes 复原 + 错误上抛），"独占发布与回滚"重绑
- [ ] F3 "SessionHeader 逻辑九字段"重绑至 header_round_trips_through_the_logical_shape
- [ ] F4 derive 缓存测试补同状态双 derive 深相等断言（逐字节一致）
- [ ] F5 fork 扩展记录改用场景点名的 approval/decided + tool/ptc-dispatch 类型
- [ ] F6 plugin_record 测试补 surface_nodes 空断言
- [ ] F7 两份 bind 脚本写入 §五 纪律头注释
- [ ] F8 修复后 SESSION lifecycle 复跑 + 四关全绿 + 本文件状态列更新

---

## 附录 A：SESSION 77 条逐条判定（2026-10-23，逐条对照测试体与场景 假设/当/那么）

判定图例：真证 = 断言逐句覆盖 Then；部分 = 存在缺口（注明）；假证 = 绑定的测试根本不证该 Then；错绑 = 绑到了语义不符的测试；场景失真 = 场景 Then 本身与 dsh 真值相反（合约层错误）。

| # | 场景 | 测试 | 判定 | 缺口 / 说明 |
|---|------|------|------|------------|
| 001 | v4 会话写入与读取回环 | materialize_append_read_round_trips_plain_and_zstd | 真证 | events 深相等断言 |
| 002 | crash 孤儿 turn interrupted | crash_orphan_turn_with_no_step_closes_with_interrupted_only | 真证 | Given 精确匹配（仅 turn/start）；step 开分支由 010 测试覆盖 |
| 003 | fork seed 的 forked 关闭 | fork_seed_marks_cut_and_closes_open_turn_with_forked_closers | 部分 | tag/turn-end 断言成立；"文案取 forked CLOSER_TEXT"未在该测试断言（在另一未绑定测试）→F18 |
| 004 | user/message 直载形态 | user_message_data_is_the_message_itself | 真证 | user 直载 + developer 包裹对照均断言 |
| 005 | 非 v4 文件被拒绝 | foreign_versions_refuse_before_shape_checks | 真证 | 新旧两向文案；"不产生写入"为读路径结构性成立 |
| 006 | zstd 压缩回环 | materialize_append_read_round_trips_plain_and_zstd | 真证 | zstd 分支全量回环 |
| 007 | 写路径校验失败不发布 | write_path_validation_failure_publishes_nothing | 部分 | "不产生文件"已证；"已发布文件保持不变"子句缺本测试内断言（在 069 测试）→F |
| 008 | deriveMessages 逐字节一致 | derive_messages_skips_empty_and_log_only_and_caches_tail_growth | 部分 | 同状态双 derive 深相等未断言 →F4 |
| 009 | ptc-dispatch 不进请求 | ptc_subcall_records_stay_out_of_derived_messages | 部分 | 只含 tool/ptc-dispatch，缺 -start →F19 |
| 010 | abort 补合成 tool/result | unstarted_call_closes_with_not_started_error_and_no_source_seqs | 部分 | closer 断言成立；"重放合法"未断言 →F20 |
| 011 | 未知必需事件拒绝重建 | restore_gate_refuses_unknown_required_and_enforces_the_cut | 真证 | |
| 012 | SessionHeader 九字段/六必需键 | header_shape_guard_covers_the_whole_table | 错绑 | 九字段投影断言在 header_round_trips_through_the_logical_shape →F3 重绑 |
| 013 | seedLength 拒绝 | header_shape_guard_covers_the_whole_table | 真证 | |
| 014 | 五角色与七种 ContentBlock 往返无损 | text_block_round_trips_with_verbatim_type_key | 假证 | 单 text block 冒烟顶 5 角色×7 block=35 组合 →F9 新测试 |
| 015 | 扩展事件与 end-seed 回环 | fork_seed_round_trips_with_the_inherited_cut | 部分 | 扩展类型与 Given 点名不符（plugin:sample/note ≠ approval/tool-ptc）→F5 |
| 016 | 独占发布与回滚 | republishing_an_existing_log_refuses_and_keeps_bytes_identical | 部分 | 回滚子句零覆盖 →F2；首发 fsync/link 为审查级保证（测试不可观，如实登记） |
| 017 | 非法 replace 被拒 | replace_bounds_not_earlier_are_rejected | 错绑 | Then 两子句（遗漏遮蔽节点 / assistant 携带）各由 replace_missing_coverage_is_rejected 与 assistant_message_forbids_source_event_seqs 证明，绑定的 bounds 测试不证 →F10 |
| 018 | tool/result 替换越权 | tool_result_rewrite_may_change_only_content | 真证 | |
| 019 | 两级补偿错误码与文案 | started_call_closes_with_outcome_unknown_and_call_seq_source | 部分前置绑定 | ABORTED_BEFORE_DISPATCH/AbortError 半边属 agent-loop 未实现 →F11 撤回 pending |
| 020 | 折叠编码回环 | decode_seq_ranges_expands_and_rejects | 部分 | 单元级折叠展开已证；"落盘并读回"文件级未证 →F12 |
| 021-062 | 42 条公式化场景（触发/归属/回环） | every_core_event_type_admits_and_round_trips_with_its_surface_role | 部分×14 / 假证×28 | 触发：Then"字段与 dsh types.ts 逐字一致"未对照 dsh 锚点（fr-wire 键序钉测另有但未绑定）；归属：仅正向；回环：Given 是"持久化写读"，测试是内存往返 →F1 拆 14 测试逐一实现三断言后 1:1 重绑 |
| 063 | contentGeneration 整表重建 | replacement_bumps_generations_and_derive_rebuilds | 真证 | 计数 + 重建内容断言 |
| 064 | 表面尾增长增量投影 | derive_messages_skips_empty_and_log_only_and_caches_tail_growth | 真证 | |
| 065 | 空 assistant 投影 null | empty_assistant_projects_to_no_message | 真证 | |
| 066 | canonicalHeader 空 tools 省略 | request_header_fold_last_wins_and_canonicalizes | 真证 | |
| 067 | 工具增删 developer 门控 | header_seq_required_exactly_when_additions_present | 部分前置绑定 | Then 含 agent-loop 决策半边（未实现）→F13 撤回 pending |
| 068 | zstd 撕裂尾恢复 | zstd_torn_tail_keeps_the_committed_prefix_and_classifies_the_frame | 部分 | 恢复断言成立；"会话可继续追加"未断言 →F14 |
| 069 | 后继世代 EEXIST 逐字节比对 | republishing_an_existing_log_refuses_and_keeps_bytes_identical | 场景失真 | Then"一致则采纳"dsh 无此行为（dsh 为拒发）；fr 按 dsh 拒绝。合约场景生成错误 →F15 合约勘误呈用户 |
| 070 | node-0 重写白名单 | node_zero_system_head_rejects_non_system_rewrite | 真证 | |
| 071 | series 重置基线 | series_reset_drops_prefix_reuse | 真证 | |
| 072 | tool-addition 经 headerSeq 进 updates | additions_resolve_through_header_seq_into_updates | 真证 | |
| 073 | 激活期声明聚合 | dylib_artifact_loads_and_serves_the_vocabulary_service | 真证 | 真实 dylib 装载 E2E |
| 074 | 重复声明装配失败 | declare_accepts_new_types_and_refuses_duplicates | 真证 | |
| 075 | plugin 记录写入读取回环 | plugin_record_grammar_and_retention | 真证（F6 微补 surface 空断言） | |
| 076 | 非 plugin: 语法拒绝 | plugin_record_grammar_and_retention | 真证 | |
| 077 | canonical 空 tools（同 066 族） | request_header_fold_last_wins_and_canonicalizes | 真证 | |

**SESSION 判定合计：真证 32 / 部分 33 / 假证 1 / 错绑 2 / 部分前置绑定 2 / 场景失真 1。（77 条）**
**假绿口径：lifecycle 报 77 passed，其中 33 部分 + 1 假证 + 2 错绑 + 2 前置 = 38 条绑定绿得不干净（49%）。**

## 附录 B：CONFIG 55 条逐条判定（逐测试体断言核验）

| # | 场景 | 测试 | 判定 | 说明 |
|---|------|------|------|------|
| 001 | 覆盖行仅翻 disabled | toggle_only_override_keeps_other_keys | 真证 | disabled/name/config 三点 |
| 002/042 | 整键替换 ×2 | config_override_replaces_wholesale | 真证 | |
| 003/004 | 未命中/不匹配 ×2 | unmatched_and_mismatched_overrides_warn_and_skip | 真证 | |
| 005 | 装配层次展开 | stack_layers_ordered_bottom_up | 真证 | 4 层断言 |
| 006 | insert 追加序 | insert_appends_and_later_layers_can_override | 真证 | |
| 007 | 清单未知名 skipped | unknown_manifest_bundle_is_skipped_non_fatal | 真证 | skipped[0].name 断言 |
| 008 | profile 名校验 | profile_name_validation_rejects_dsh_rules | 真证 | 规则族断言 |
| 009 | 模板首用初始化 | fresh_headless_profile_auto_inits | 真证 | bundles 空 + patch 空 |
| 010 | 未知 profile 报错 | unknown_profile_name_errors | 真证 | |
| 011 | 可选 patch 缺失空层 | optional_patch_absent_is_empty_layer | 真证 | Ok(None) + 损坏行拒绝 |
| 012 | --patch 缺文件报错 | required_overlay_absent_fails_startup | 真证 | |
| 013 | patch 解析错误 fail-closed | optional_patch_parse_error_is_fatal | 真证 | lifecycle 已过 |
| 014 | 编译产物过 dougong loader | compiled_document_is_valid_loader_config | 真证 | Config::parse 实调 |
| 015 | insert 相对名锚定 | relative_insert_names_anchor_to_patch_dir | 真证 | ./ 与 ../ 锚定断言 |
| 016 | 覆盖行 name 不锚定 | override_row_names_are_never_anchored | 真证 | ./matcher 字面保持 |
| 017 | 单行校验失败不阻塞 | schema_violation_fails_row_without_blocking | 真证 | |
| 018 | 查无 schema 跳过 | absent_schema_skips_validation | 真证 | |
| 019 | 同一声明双面 | validation_and_projection_share_one_declaration | 真证 | |
| 020 | 子集外表达式拒绝 | expression_outside_subset_rejects_document | 真证 | |
| 021/043 | env 缺省链 ×2 | env_default_chain_matches_dsh_semantics | 真证 | |
| 022 | 裸 env 读取 | bare_env_read_is_null_when_absent | 真证 | |
| 023 | profile-context 布尔 | profile_context_boolean_with_negation | 真证 | |
| 024 | 平台三元 | platform_ternary_and_equality | 真证 | |
| 025 | dump 输出来自真实装配 | dump_config_and_schema_generated_from_real_assembly | 部分 | 绑定的是 fr-config 数据面单测；"运行 fr"二进制面由未绑定的 dump_config_writes_composed_yaml_to_stdout 证明 →F16 |
| 026 | dump 注释分层逐字 | composes_layers_prints_verbatim_js_and_labels_sections | 真证 | 121 断言 |
| 027 | default-only 不读用户层 | default_only_collects_bundle_layers_only_and_skips_broken_user_files | 真证 | |
| 028 | default-only 拒 --patch | dump_arg_validation_error_family | 真证 | |
| 029 | x-fr 五键五态 | x_fr_keys_and_entry_status_vocabulary | 真证 | |
| 030 | complete 假退 1 | incomplete_schema_dump_exits_one | 真证 | |
| 031 | 注册表命中投影全脸 | registry_hit_projects_config_defs | 真证 | schema/absent/nullable 断言 |
| 032 | 同名分裂告警 | ambiguous_name_splits_projections_and_warns | 真证 | |
| 033 | absent 引用 unknownConfig | absent_entries_reference_unknown_config | 真证 | |
| 034 | from-default-profile 替换 | from_default_profile_replaces_existing_profile | 真证 | |
| 035 | dump 拒收位置参数 | dump_rejects_app_arguments_and_unknown_flags | 真证 | |
| 036 | 参数校验错误族 | dump_arg_validation_error_family | 真证 | |
| 037 | schema 流面分流 | schema_dump_streams_json_and_diagnostics | 部分 | 单 JSON+换行已证；"diagnostics 逐条 stderr 格式"子句在快乐路径（stderr 空）未触发 →F17 |
| 038 | dump 致命错误单行 | dump_fatal_error_prints_single_line_and_exits_one | 真证 | |
| 039 | 根调用面 | root_dispatch_version_help_and_unknown_modes | 真证 | |
| 040 | 第一方 wasm 拒绝 | first_party_wasm_resolution_is_rejected | 真证 | |
| 041 | disabled 不装载 | compiles_resolved_rows_and_excludes_disabled | 真证 | |
| 044/045 | 三层序/不覆盖 ×2 | layered_three_layer_precedence_and_no_override | 真证 | |
| 046 | 原子拒绝 | layered_atomic_rejection_applies_neither_file | 真证 | |
| 047 | 读错误跳层 | layer_missing_file_is_silent_and_read_error_warns_then_skips | 真证 | |
| 048 | 专名大小写 | bootstrap_names_and_prefixes_case_insensitive | 真证 | |
| 049 | cwd=home 去重 | layered_cwd_home_dedup_reads_directory_once | 真证 | |
| 050 | parseEnv 方言 | parser_matches_node_parse_env_official_fixture | 真证 | 官方 fixture 全量 |
| 051-054 | !!js 四态 ×4 | 各具名测试 | 真证 | |
| 055 | dump 内容行回环 | dump_content_js_shaped_lines_survive_round_trip | 真证 | |

**CONFIG 判定合计：真证 53 / 部分 2（025、037）。（55 条）**

## 附录 C：未绑定合约状态

其余 18 份合约共 833 场景全部 pending_*，零绑定零验证。其中场景深度未逐条审计（同批生成器产出，预计与 SESSION 公式化场景同类问题），绑定时应按 §五纪律 + 本附录方法执行，浅场景一并返工。

## 修复计划 v2（F1-F20，替换 §六；全部未完成）

- [ ] F1 矩阵拆 14 个 per-type 测试：admission + 双向表面归属 + **文件级写读回环** + dsh 键序锚点断言；42 场景 1:1 重绑
- [ ] F2 RLIMIT_FSIZE 注入的 append 回滚测试（bytes 复原 + 错误上抛）；016 重绑
- [ ] F3 012 重绑 header_round_trips_through_the_logical_shape
- [ ] F4 derive 缓存测试补同状态双 derive 深相等
- [ ] F5 fork 扩展记录改 approval/decided + tool/ptc-dispatch
- [ ] F6 plugin_record 补 surface_nodes 空断言
- [ ] F7 两份 bind 脚本写死 §五纪律头注释
- [ ] F8 修复后 SESSION lifecycle + 四关复跑 + 本文件状态更新
- [ ] F9 014 新测试：5 角色 × 7 block 文件级往返
- [ ] F10 017 合并测试（遗漏遮蔽 + assistant 携带两拒绝）重绑
- [ ] F11 019 撤回 pending（agent-loop 半边未实现）
- [ ] F12 020 补文件级折叠回环断言
- [ ] F13 067 撤回 pending（agent-loop 半边未实现）
- [ ] F14 068 补修复后可继续追加断言
- [ ] F15 069 场景失真——提交用户裁决：A 按 dsh 改场景 Then（拒绝语义）/ B 按场景实现采纳语义（偏离 dsh）
- [ ] F16 025 CLI 测试扩为两 dump 并重绑
- [ ] F17 037 补 diagnostics stderr 格式用例
- [ ] F18 003 fork 测试补 forked 文案断言
- [ ] F19 009 补 tool/ptc-dispatch-start 记录
- [ ] F20 010 补 from_seed 重放合法断言
