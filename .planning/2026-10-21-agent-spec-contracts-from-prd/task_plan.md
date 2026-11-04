# Task Plan: 基于 doc/prd.md 生成 agent-spec 合约 → M1 headless 实现

## Goal
将 doc/prd.md（fly-rafter 唯一需求真源）转化为 agent-spec 合约体系（已完成，1193 场景门禁全绿）；随后按 PRD 第 20 节实现 M1（headless）：约 20 个 crate、六条验收标准，质量门四关全绿。

## Next Step
**18b b3 全绿完成（生命周期臂 CREATE=10/RESUME=11/LIVE=12 + Q4B agent/error {sessionId,turn,step,error,seq} 全链 + Q9A 裁决 agentOptions 四字段投影；workspace 四关绿、1921/0、lifecycle 27 真实+3 既有跨切片 skip、四新场景全真绑定）。下一步=b4 或收口（按续作地图：c=fr-schema json-stream schema 模块+fr-headless 参数/投影/采纳纯件→d=runner 集成+退出守望+mock e2e+25 场景绑定+四关+lifecycle/guard/stamp 七步收口）。全锚见 findings.md「18b b3 完成」。**

