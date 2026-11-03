# Task Plan: 基于 doc/prd.md 生成 agent-spec 合约 → M1 headless 实现

## Goal
将 doc/prd.md（fly-rafter 唯一需求真源）转化为 agent-spec 合约体系（已完成，1193 场景门禁全绿）；随后按 PRD 第 20 节实现 M1（headless）：约 20 个 crate、六条验收标准，质量门四关全绿。

## Next Step
**fr-compaction 切片执行中**：17a-17e ✓（见 progress）；**17f ✓（2026-11-03：summarizer LLM 缝（summarize_with_llm begin/drain/abort 拉取环+目标三臂 verbatim）+四监听 kernel 接线（fr-compaction descriptor listen 四事件+CPS on_event/on_next；loop 侧 pre-step/request-error 出站 dispatch+恒等续体消费+决策解码）+agent 服务维护窗口三面（BEGIN/POLL/END+sync_window 镜像+converge+wake 重放+cancel 置 aborted 旗）+STATUS route 字段（Q1=1）+summary-error 派发经 Relay 中继线程（B6）+session/event 监听侧全量（生产方随 Phase 18 store 接线，披露在案）；四关 1807/0 两连+三门禁 0 errors）**。**下一动作=17g：pruner+image-offload 两监听（批 5：compaction-tool-result-pruner 303 行+compaction-image-offload 197 行移植；IMAGE_OFFLOAD_REQUIRED 恢复件消费 17f 已钉的 request-error/summary-error 形状；pruner 可选缝=dsh ctx.get 形态）→17h spill 三件+compactNow+maintenance 窗口消费+全链绑定+七步收口。**
