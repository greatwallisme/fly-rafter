# Task Plan: 基于 doc/prd.md 生成 agent-spec 合约 → M1 headless 实现

## Goal
将 doc/prd.md（fly-rafter 唯一需求真源）转化为 agent-spec 合约体系（已完成，1193 场景门禁全绿）；随后按 PRD 第 20 节实现 M1（headless）：约 20 个 crate、六条验收标准，质量门四关全绿。

## Next Step
**18b c 全绿完成（fr-schema json-stream 八帧 schema 文档常量+校验器；fr-headless startup/json_stream/reasoning/adopt 四纯件 33 测试+help_text 生成件；fr-agent-loop export.rs 导出表隔离[b-a 判例兑现]；workspace 四关绿 1968/0+21 cdylib 工件+boot 集成绿）。下一步=d（runner 集成：headlessStartup 消费行+create/resume 双检+resolveAgent 采纳链+followup+whenIdle 等待+waitForChildren 提前唤醒抑制+summarize final 面+headless/exit 退出守望+fr 宿主 in-process 监听插件）→ mock e2e（TcpListener SSE+base-url patch）→ 25 场景绑定+四关+lifecycle/guard/stamp 七步收口。全锚见 findings.md「18b c 完成」。**


