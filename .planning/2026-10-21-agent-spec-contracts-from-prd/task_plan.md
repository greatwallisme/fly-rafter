# Task Plan: 基于 doc/prd.md 生成 agent-spec 合约 → M1 headless 实现

## Goal
将 doc/prd.md（fly-rafter 唯一需求真源）转化为 agent-spec 合约体系（已完成，1193 场景门禁全绿）；随后按 PRD 第 20 节实现 M1（headless）：约 20 个 crate、六条验收标准，质量门四关全绿。

## Next Step
**fr-compaction 切片执行中（2026-11-02）**：17a ✓（PRD/intake/重导入/三门禁）；17b ✓ 全链闭（dispatch_requirements 定稿→ABI v5 实现→E1 矩阵 14 测→demo 臂→D32/CHANGELOG/skills→0.5.0 bump→review 7修1跳（跳过项=panic 姿态维持裸，用户已裁）→提交 9bb3b0b→**五 crate 已发布 crates.io v0.5.0**→handoff 成稿）；17c 前半 ✓（fr 升 0.5.0：34 测试字面量补 dispatch 桩+4 处 abiVersion 钉测 5+四关绿）。**下一动作=17c 后半 fr-token-meter 新 crate（度量引擎 1:1 移植+dougong 插件+provide "token-meter" 服务缝+PRD 4.5 行已备）→17d 核心域→17e 选区管线→17f summarizer+接线→17g pruner+offload→17h spill+compactNow+绑定+七步收口。**