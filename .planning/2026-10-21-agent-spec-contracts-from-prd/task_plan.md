# Task Plan: 基于 doc/prd.md 生成 agent-spec 合约 → M1 headless 实现

## Goal
将 doc/prd.md（fly-rafter 唯一需求真源）转化为 agent-spec 合约体系（已完成，1193 场景门禁全绿）；随后按 PRD 第 20 节实现 M1（headless）：约 20 个 crate、六条验收标准，质量门四关全绿。

## Next Step
**18b 进行中：阶段 a 全绿（runtime-registry+通道代理+provider 生成面；workspace clippy 0/全测 1911/0）+b1 全绿（fr-session 三读臂+read surface 字段+生产 SessionReadsClient）。下一步=b2（ToolsFace 生产客户端：fr-tools token 基调度面桥接——WireExecution/wire_execution 复用+settle→callId 路由，非简单配对）→b3（CREATE/RESUME/LIVE 臂+agent/error 发射）→c（fr-schema schema+fr-headless 纯件）→d（runner 集成+退出守望+mock e2e+25 场景绑定+七步收口）。续作地图+全锚见 findings.md「阶段 b 开工勘察」与 progress.md 末节。**
