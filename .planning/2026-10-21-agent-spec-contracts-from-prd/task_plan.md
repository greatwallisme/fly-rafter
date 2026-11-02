# Task Plan: 基于 doc/prd.md 生成 agent-spec 合约 → M1 headless 实现

## Goal
将 doc/prd.md（fly-rafter 唯一需求真源）转化为 agent-spec 合约体系（已完成，1193 场景门禁全绿）；随后按 PRD 第 20 节实现 M1（headless）：约 20 个 crate、六条验收标准，质量门四关全绿。

## Next Step
16f 批 2 步①-⑤+步⑥a 已提交/收口（2026-11-02）：plugin_manager 全机+19.2 条 29-31；步⑥a skill_runtime registry 本体 11 钉测+19.2 条 32-35（watcher 签名轮询/品牌映射/scope 单层/skills/change 无消费者）。判例升级=retype 长上下文行必须 git diff 对账（条 31 漂移例）。**下一动作=步⑥b skill_filesystem.rs（12 键 config/roots .fr-skills 序/frontmatter serde_yaml_ng/发现/读取/watch 签名轮询线程+钉测）→ 步⑥c（工具+catalog/gesture+skills 服务面+方法号 201-203+describe 165+schema.rs）→ 步⑥d（loop 接线+plugin_shell 注册+fr-schema 三 config 组+catalog_gen+CONFIG/TOOLS 绑定+终门）→ 步⑦ 收口。**
