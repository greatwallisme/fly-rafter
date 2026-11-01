# Task Plan: 基于 doc/prd.md 生成 agent-spec 合约 → M1 headless 实现

## Goal
将 doc/prd.md（fly-rafter 唯一需求真源）转化为 agent-spec 合约体系（已完成，1193 场景门禁全绿）；随后按 PRD 第 20 节实现 M1（headless）：约 20 个 crate、六条验收标准，质量门四关全绿。

## Next Step
16f 批 1 步④已收口（2026-11-01）：web_tools.rs 双工具全脸 15 钉测绿；crate-route 五缺口整改（strip_tags 泄漏→exclude_selectors+Tier2、colspan 爆炸→重建扫描剥 span、raw-text 截断臂、collapse→hidden 声明重写、未终结注释截尾）；ureq 池化 keep-alive 竞态根除（19 处 loopback 响应补 Connection: close，8 连跑稳）；workspace 1588/0+doc 零警告。**下一动作=步⑤集成收口：service.rs 174-183+describe 带→plugin_shell 注册行→sections 2000/2100→schema.rs 参数/输出表（Q97-A）→fr-schema config 七组→catalog 扩源→十门全绿+TOOLS spec 绑定。**

