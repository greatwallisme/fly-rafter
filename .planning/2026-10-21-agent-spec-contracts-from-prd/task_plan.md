# Task Plan: 基于 doc/prd.md 生成 agent-spec 合约 → M1 headless 实现

## Goal
将 doc/prd.md（fly-rafter 唯一需求真源）转化为 agent-spec 合约体系（已完成，1193 场景门禁全绿）；随后按 PRD 第 20 节实现 M1（headless）：约 20 个 crate、六条验收标准，质量门四关全绿。

## Next Step
16f 批 2 步①-⑤ 已提交（0f39129/ff65330/3e836c3/08ae143/步⑤，2026-11-02）：working_directory+PROMPT 收口、cordis_inspect、lsp+config、load_workspace_dependencies、plugin_manager 全机（磁盘面+审批前置+ManagementError 拒绝族+19.2 条 29/30/31）；lifecycle TOOLS 350/0/58+CONFIG 137/0/182+PROMPT 47/0/0，workspace 1643×2，方法号至 200、describe 162，binding-audit 二十一节。**下一动作=步⑥（skill 全家，Q16f-C 全量：registry+filesystem+catalog+gesture，最大件）→ 步⑦ 收口（catalog SECTION_TEXTS 扩源+终门）。判例族=批 2 步①-⑤ 同款（小步分片+写后即验+一次一函数）。**
