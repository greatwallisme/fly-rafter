# Task Plan: 基于 doc/prd.md 生成 agent-spec 合约 → M1 headless 实现

## Goal
将 doc/prd.md（fly-rafter 唯一需求真源）转化为 agent-spec 合约体系（已完成，1193 场景门禁全绿）；随后按 PRD 第 20 节实现 M1（headless）：约 20 个 crate、六条验收标准，质量门四关全绿。

## Next Step
**Phase 18 切片划分裁决 1A 2B（18a→f 顺序批准；18c/18d 脚手架随各自切片建，18a 装载清单留两行占位）。当前切片=18a 宿主装配面：开工勘察三事实项（dougong realm 契约/cmdline-appExit 等价面/cdylib 工件核查）→ headless spec 扩边界+18a 场景 → lint 0.7 → 实现。**
