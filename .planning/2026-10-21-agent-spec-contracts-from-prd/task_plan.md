# Task Plan: 基于 doc/prd.md 生成 agent-spec 合约 → M1 headless 实现

## Goal
将 doc/prd.md（fly-rafter 唯一需求真源）转化为 agent-spec 合约体系（已完成，1193 场景门禁全绿）；随后按 PRD 第 20 节实现 M1（headless）：约 20 个 crate、六条验收标准，质量门四关全绿。

## Next Step
16f 批 2 步①-⑤+步⑥a 已提交/收口（2026-11-02）：plugin_manager 全机+19.2 条 29-31；步⑥a skill_runtime registry 本体 11 钉测+19.2 条 32-35（watcher 签名轮询/品牌映射/scope 单层/skills/change 无消费者）。判例升级=retype 长上下文行必须 git diff 对账（条 31 漂移例）。步⑥b skill_filesystem.rs 已提交（2026-11-02）：12 键 config/roots 序/frontmatter 族/FsDomain 读取面/发现/owner 簿记+签名轮询（两水平签名+稳定梯）/host-mutation 门；SkillRegistry 增 pub invalidate；12 钉测；全 crate 381 绿。事故判例=clippy 建议误应用致死循环（重推等价性+ps CPU% 定位）。**下一动作=步⑥c skill_tools.rs（工具 execute/render/presentCall+catalog/gesture 双 pre-step listener+skills 服务面 METHOD_PRE_STEP=0+方法号 201-203+describe 165+schema.rs Q97-A 钉测）→ 步⑥d（loop 接线+plugin_shell 注册+fr-schema 三 config 组+catalog_gen+CONFIG/TOOLS 绑定+终门）→ 步⑦ 收口。**
