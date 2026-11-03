# Task Plan: 基于 doc/prd.md 生成 agent-spec 合约 → M1 headless 实现

## Goal
将 doc/prd.md（fly-rafter 唯一需求真源）转化为 agent-spec 合约体系（已完成，1193 场景门禁全绿）；随后按 PRD 第 20 节实现 M1（headless）：约 20 个 crate、六条验收标准，质量门四关全绿。

## Next Step
**19.2 条 38 修正已落（用户裁决选项 1，commit 3363b0c）；17h-a ✓（spill storage seam+local backend+cleanup sweep 全落地 +12 钉测，四关+三门禁绿，基线 1840）**。**下一动作=17h-b：spill-policy 两监听（output-retention 端口+token-meter/llm/attachments/fs 缝摸底先行）→17h-c compactNow+maintenance 窗口消费→17h-d 全链绑定+lifecycle+stamp 七步收口。**
