# Task Plan: 基于 doc/prd.md 生成 agent-spec 合约 → M1 headless 实现

## Goal
将 doc/prd.md（fly-rafter 唯一需求真源）转化为 agent-spec 合约体系（已完成，1193 场景门禁全绿）；随后按 PRD 第 20 节实现 M1（headless）：约 20 个 crate、六条验收标准，质量门四关全绿。

## Next Step
16f 批 1 已全收口（2026-11-01）：步①-⑤ 完整闭环——四 provider+web_tools.rs+步⑤集成收口（service 174-183、六 provider 注册、12 段、Q97-A 双 schema 钉测、fr-schema 嵌套七组[用户 A 裁决]、catalog 扩源、TOOLS 15+CONFIG 17 场景绑定、binding-audit 十六节）；门：workspace test/doc/clippy/fmt 全绿、fr-tools-builtin 313×3 稳、--all-targets 债清零、lifecycle TOOLS 297/0/111+CONFIG 131/0/188（0 fail，skip 全=未落相位）、guard 结构性 pending 判例照旧。**下一动作=提交后开 16f 批 2（skill/lsp/working_directory/内省组，Q94-A 拆分），沿用批 1 判例族（Q89-A/Q95-A 抽取脚本+Q97-A 全 schema 钉测+baked 承载 base patch）。**
