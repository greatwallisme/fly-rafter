# Findings

## 2026-10-21 — agent-spec 合约生成

- `agent-spec` CLI 1.4.0 已安装（`~/.cargo/bin/agent-spec`）。
- `requirements import` 只消费显式 `<!-- agent-spec:requirement -->` 标记块（或 YAML 方言），从不解释原始散文；记载属性仅 id/title/tags/source。
- 记载的导入管线：import → lint-knowledge --gate → requirements graph --gate → plan --gate → work-units → draft-specs。`plan --gate` 在无满足规格早期会报 ready-without-satisfying-specs 诊断，属预期流水线状态，不是缺陷。
- 工作单元"ready"条件：需求无阻断 open questions 且有场景；open questions 非阻断 lint，但阻断 ready。
- spec 层（用户可见合约）解析器接受 `### Scenario:` 兼容形式，但作者规范要求裸 `场景:`/`测试:` 行；不允许自创顶级节（如 ## Milestones）。
- PRD 切分映射（PRD 节 → REQ 块）：6→CONFIG、7→SESSION、8→AGENT-LOOP、9.1-9.3→TOOLS、9.4→PTC、10→PROMPT、11.1→APPROVAL、11.2→SANDBOX、12→COMPACTION、13→LLM、14→MCP、15→SUBAGENT、16→COMMANDS、17→BUILTIN-TOOLS、18.1→HEADLESS、18.2→TUI、18.3→ACP、18.4→SDK、18.5→WEB、21+5.1+1.4.5→QUALITY、19→SCOPE。
