# Task Plan: 基于 doc/prd.md 生成 agent-spec 合约 → M1 headless 实现

## Goal
将 doc/prd.md（fly-rafter 唯一需求真源）转化为 agent-spec 合约体系（已完成，1193 场景门禁全绿）；随后按 PRD 第 20 节实现 M1（headless）：约 20 个 crate、六条验收标准，质量门四关全绿。

## Next Step
16f 批 2 步⑥d 已收口（2026-11-02）：loop 接线（DriverError::Skills+SkillPreStep trait+DriverDeps.skills+pre_step 调用=waterfall 后/instructions 前仅 Enter，dsh agent.ts:267-287 reject 早退铁证；skill_client.rs SkillsClient；descriptor inject 第 5 项）+builtin 注册（descriptor provide 第 6 项+skill_registration 行 201/202/203+BAKED_SKILL_TOOLS 门+try_new 装配 registry128/provider/SkillRuntime next_id Arc Sync+mark_registered+call-export 1500 段臂）+fr-schema 三组钉测+catalog_gen skill 表项+8 场景绑定（builtin-tools 六+CONFIG 二）。**持久化链闭合=dsh agent.ts:424-434 首尝试全提交，fr admit_step_inputs 承接，skills 注入免费持久化。**门全绿（fmt/clippy 0 警告/workspace 1694/0/doc/三门禁 0 errors/lifecycle CONFIG 139-0-180+BUILTIN-TOOLS 355-0-53，八场景全 pass；stamp 瞬时 1 failed 三连复验排除）。**下一动作=16f 批 2 步⑦ 后续窗任务规划（批 2 剩余项核对或 Phase 16 收口面），随用户指令开新切片。**
