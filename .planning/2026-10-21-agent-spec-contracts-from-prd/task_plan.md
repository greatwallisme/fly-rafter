# Task Plan: 基于 doc/prd.md 生成 agent-spec 合约 → M1 headless 实现

## Goal
将 doc/prd.md（fly-rafter 唯一需求真源）转化为 agent-spec 合约体系（已完成，1193 场景门禁全绿）；随后按 PRD 第 20 节实现 M1（headless）：约 20 个 crate、六条验收标准，质量门四关全绿。

## Next Step
**17h-c ✓（COMPACT_NOW 服臂+窗口消费接线：BEGIN/poll/END 全链+cancel 中途中止 llm+command_face 文案+分类失败文档面，+13 钉测，四关+三门禁绿，基线 1884）**。**下一动作=17h-d：全链绑定（pending_req_fr_compaction_compact→plugin_shell::tests::manual_compaction_* 等真实测试）+lifecycle+guard+stamp 七步收口。**
