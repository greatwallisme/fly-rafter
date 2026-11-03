# Task Plan: 基于 doc/prd.md 生成 agent-spec 合约 → M1 headless 实现

## Goal
将 doc/prd.md（fly-rafter 唯一需求真源）转化为 agent-spec 合约体系（已完成，1193 场景门禁全绿）；随后按 PRD 第 20 节实现 M1（headless）：约 20 个 crate、六条验收标准，质量门四关全绿。

## Next Step
**17g ✓（2026-11-03：pruner+image-offload 两件+消息投影机制（19.2 条 14 落地）全落地；容量协议结构发现修复（require_with_initial 调用方初容量纪律）；summary-error 内建臂进程内直调定案（ln 执法强制）；四关 1828/0+三门禁 0 errors+bind 幂等重放零漂移）**。**下一动作=17h：spill 三件+compactNow+maintenance 窗口消费+全链绑定+lifecycle/guard/stamp 七步收口+19.2 条 38 形状偏差呈裁。**
