# Task Plan: 基于 doc/prd.md 生成 agent-spec 合约 → M1 headless 实现

## Goal
将 doc/prd.md（fly-rafter 唯一需求真源）转化为 agent-spec 合约体系（已完成，1193 场景门禁全绿）；随后按 PRD 第 20 节实现 M1（headless）：约 20 个 crate、六条验收标准，质量门四关全绿。

## Next Step
16f 批 2 步⑥c 已收口（2026-11-02）：skill_tools.rs 全模块（SkillRuntime+catalog 五臂决策 1:1+gesture find_iter 尾边界等价+工具体+skills 服务面 serve）；METHOD_PRE_STEP=1500+toolVisible=注册活性（本会话双裁决，PRD 19.2 条 36）；service.rs 201-203+describe 165；schema.rs Q97-A；fr-wire skills 常量。**钉测揭出 ⑥a 真缺陷已修：SkillRegistry register/register_provider 注册后未失效 collect 缓存（dsh ScopedLayers.effect onChange store.ts:271-272 佐证），两处补 invalidate。**门全绿（fmt/clippy/workspace 1684/doc/三门禁）。判例升级=≤50 行/步+tripwire 惯例拦伪影。**下一动作=步⑥d：loop 接线（descriptor inject skills+driver pre_step 调用：waterfall 后/instructions 前+loop 侧钉测）→ plugin_shell 注册（skills runtime 装配+descriptor provide 第六行+call-export 1500 段路由+工具注册行 201-203 端点+mark_registered+BAKED_SKILL_TOOLS）→ fr-schema（tool-skill 工具组+三 config 组+组钉测）→ catalog_gen 表项 → CONFIG/TOOLS spec 绑定 → 终门 → 步⑦ 收口。**
