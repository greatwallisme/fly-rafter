# Task Plan: 基于 doc/prd.md 生成 agent-spec 合约 → M1 headless 实现

## Goal
将 doc/prd.md（fly-rafter 唯一需求真源）转化为 agent-spec 合约体系（已完成，1193 场景门禁全绿）；随后按 PRD 第 20 节实现 M1（headless）：约 20 个 crate、六条验收标准，质量门四关全绿。

## Next Step
**fr-compaction 切片执行中**：17a ✓（PRD/intake/重导入/三门禁）；17b ✓（dougong dispatch 升级+五 crate 0.5.0 发布）；17c ✓ 全链（fr 升 0.5.0+34 桩+4 钉测→提交 3194403；后半 fr-token-meter 新 crate 11 模块 35 测全落地）；17d ✓（fr-compaction 新 crate 5 模块 28 钉测=brand/types/checkpoint/tool_pairing/config 全量+fr-schema "fr-compaction" 注册行+1 钉测；判决=Engine trait/服务缝随 17f、AgentContext 随 17e、PREAMBLE/括弧=dsh summarizer.ts 17f；四关 1759/0+三门禁 0 errors）。**下一动作=17e：选区+压缩管线+括弧+summary-error waterfall（dsh compaction-basic/src/region.ts 600 行+index.ts 压力触发/溢出恢复管线核心）→17f summarizer+接线→17g pruner+offload→17h spill+compactNow+绑定+七步收口。**
