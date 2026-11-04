# Task Plan: 基于 doc/prd.md 生成 agent-spec 合约 → M1 headless 实现

## Goal
将 doc/prd.md（fly-rafter 唯一需求真源）转化为 agent-spec 合约体系（已完成，1193 场景门禁全绿）；随后按 PRD 第 20 节实现 M1（headless）：约 20 个 crate、六条验收标准，质量门四关全绿。

## Next Step
**18b 进行中：阶段 a+b1+b2 全绿（b2=Q8A 裁决 SETTLED agent 过滤+fr-agent-loop ToolsClient 生产客户端 token 桥接全链真；workspace 四关绿、全测 1915/0、cdylib 双工件链接实证）。下一步=b3（生命周期臂 CREATE=10/RESUME=11/LIVE=12：plugin_shell 臂+DriverDeps 生产组装{SessionReadsClient+SystemPromptClient+LlmClient+ToolsClient}+AgentEvents+mailbox attach+DriverThread spawn 判例+teardown join+driver.rs agent/error 发射 throwError 1:1）→c（fr-schema json-stream schema+fr-headless 参数/投影/采纳纯件）→d（runner 集成+退出守望+mock e2e+25 场景绑定+七步收口）。全锚与完成判例见 findings.md「18b b2 完成」。**
