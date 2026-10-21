#!/usr/bin/env python3
"""系统化枚举场景生成器（合约场景库的封闭面部分）。

从权威枚举表（PRD 第 17 节工具清单、dsh 源码笔记的封闭集合）生成 Rule 归属场景，
输出 @@ID 分节格式供补丁器并入 doc/requirements-intake.md。幂等：表不变则输出不变。
用法：python3 scripts/gen-scenarios.py [--out /tmp/scenarios-generated.txt]
"""
from __future__ import annotations

import sys
from pathlib import Path

OUT = Path(sys.argv[sys.argv.index("--out") + 1]) if "--out" in sys.argv else Path("/tmp/scenarios-generated.txt")

# (工具名, 域标签, 特判场景后缀列表)——特判来自 PRD 17 与 dsh-source-notes
TOOLS = [
    ("bash", "shell", ["超时由 timeout-ms 结算", "run_in_background 产生可管理作业", "沙箱升级走审批 seam"]),
    ("pwsh", "shell", ["非 windows 平台默认禁用对应沙箱行"]),
    ("persistent bash", "shell", ["默认不装配", "跨调用保留 shell 状态"]),
    ("persistent pwsh", "shell", ["默认不装配"]),
    ("read", "file", ["越出 workspace 的读取按观察策略处理"]),
    ("write", "file", ["workspace 外写入被 fence 拒绝"]),
    ("edit", "file", ["编辑不存在的文件报明确错误"]),
    ("read_image", "file", ["非图片文件报错"]),
    ("glob", "file", ["超容量结果不采样（默认配置）"]),
    ("grep", "file", ["ripgrep 缺失明确报错", "FR_RIPGREP 指认生效"]),
    ("str_replace_editor", "file", ["默认不装配"]),
    ("web_search", "web", ["provider 可切换"]),
    ("web_fetch", "web", ["fetch 可关闭（配置项）"]),
    ("todo_write", "state", ["非法状态流转被拒", "允许并行 in-progress"]),
    ("create_goal", "state", ["重复目标报错"]),
    ("get_goal", "state", ["目标不存在报错"]),
    ("update_goal", "state", ["非法状态流转被拒"]),
    ("job_kill", "state", ["作业不存在报错"]),
    ("job_list", "state", ["列表含运行与已完成作业"]),
    ("job_output", "state", ["无输出作业返回空"]),
    ("schedule_create", "state", ["非法 cron 表达式报错"]),
    ("schedule_delete", "state", ["删除不存在的计划报错"]),
    ("schedule_list", "state", ["按时间序返回"]),
    ("schedule_update", "state", ["更新生效"]),
    ("ask_user_question", "interactive", ["headless 下 fail-closed", "TUI 渲染结构化选项"]),
    ("exit_plan_mode", "interactive", ["计划模式外调用报错"]),
    ("session_search", "session-query", ["关键词命中排序"]),
    ("session_trace", "session-query", ["不存在的会话报错"]),
    ("session_event_read", "session-query", ["seq 越界报错"]),
    ("session_event_search", "session-query", ["类型过滤生效"]),
    ("session_event_trace", "session-query", ["跨会话引用可追"]),
    ("terminal_open", "pty", ["PTY 六件套 opt-in 才装配"]),
    ("terminal_read", "pty", ["未打开的终端报错"]),
    ("terminal_send", "pty", ["写入字节流生效"]),
    ("terminal_signal", "pty", ["信号送达进程组"]),
    ("terminal_list", "pty", ["列出活动终端"]),
    ("terminal_close", "pty", ["关闭后资源释放"]),
    ("skill", "skill", ["未注册技能报错"]),
    ("load_workspace_dependencies", "skill", ["无依赖文件返回空"]),
    ("lsp", "lsp", ["服务器不可用明确报错"]),
    ("working_directory", "dir", ["cwd 变更后 context 同步"]),
    ("cordis_inspect_list", "introspection", ["列出全部插件与缝"]),
    ("cordis_inspect_query", "introspection", ["按 id 查询详情"]),
    ("plugin_manager", "introspection", ["运行时启停插件生效"]),
    ("run_code", "ptc", ["不可被能力过滤移除", "node 缺失 fail-closed", "子调度回流完整管线", "绑定名非法拒绝", "输出超预算归类 output-limit"]),
]

# 通用维度（每工具都生成）
DIMENSIONS = [
    ("schema 对照", "当 以生成脚本比对该工具的参数级 schema 与冻结 catalog 时", "那么 逐字段一致"),
    ("executionMode", "当 读取该工具的调度声明时", "那么 exclusive 或 parallel 定义明确且与 catalog 一致"),
    ("presentation", "当 推断该工具的呈现元数据时", "那么 视图族与 dsh presentation 元模型一致"),
    ("参数校验", "当 提交违反 schema 的参数时", "那么 校验失败并返回结构化错误"),
    ("事件落盘", "当 该工具执行完成时", "那么 产生配对的 tool/call 与 tool/result 且参数原始串未解析保存"),
]

EVENTS = [
    ("turn/start", "开启 turn 时"), ("turn/end", "turn 结束时携带 reason 枚举"),
    ("step/start", "step 开启时"), ("step/end", "step 收尾时"),
    ("user/message", "用户输入提交时"), ("developer/message", "工具增删声明时"),
    ("system/message", "系统提示词入表面时"), ("assistant/message", "成功结算时内嵌完整 stream"),
    ("assistant/attempt", "失败尝试时且不进模型历史"), ("tool/call", "派发前追加且参数原始未解析"),
    ("tool/result", "finalize 后按模型序提交且 cite callSeq"), ("request/header", "世代判定满足时"),
    ("request/context", "四字段变化时"), ("session/end-seed", "继承标记时"),
    ("approval/policy", "策略切换时且 log-only"), ("approval/asked", "ask 发起时与 decided 配对"),
    ("approval/decided", "应答结算时"), ("tool/ptc-dispatch-start", "子调用获得槽位时"),
    ("tool/ptc-dispatch", "子调用结算时配对"),
]

CHUNK_KINDS = ["block-start", "text-delta", "reasoning-delta", "tool-call-delta", "block-end", "usage", "finish"]

SECTIONS = ["HARNESS_IDENTITY", "DEPLOYMENT_PERSONA_PREFIX", "PLAN_POLICY", "TEAM_POLICY", "PTC_ONLY",
    "FILE_REFERENCE", "TOOL_BASH", "TOOL_PWSH", "TOOL_READ", "TOOL_WRITE", "TOOL_EDIT", "TOOL_GLOB",
    "TOOL_GREP", "TOOL_JOBS", "TOOL_PTY", "TOOL_WEB_SEARCH", "TOOL_WEB_FETCH", "TOOL_LSP",
    "TOOL_SESSION_QUERY", "TOOL_GOAL", "TOOL_WORKFLOW", "TOOL_RALPH", "TOOL_SUBAGENT", "TOOL_REPORT",
    "TOOL_COMPUTER_USE", "MCP_SERVERS", "TOOLS_SDK", "DELIVERABLE_FILE_REFERENCES", "STRUCTURED_OUTPUT",
    "HARNESS_SOURCE", "WEB_SURFACE", "DEPLOYMENT_PERSONA_SUFFIX"]

FAILURES = ["exception", "timeout", "abort", "worker-exit", "invalid-output", "output-limit", "protocol", "sandbox-unavailable"]

ACP_METHODS = ["initialize", "authenticate", "session/new", "session/list", "session/resume",
    "session/close", "session/set_config_option", "session/prompt", "session/cancel", "$/cancel_request"]

SDK_METHODS = ["initialize", "session/prompt", "session/resume", "session/wait",
    "session/working-directory/get", "session/working-directory/set", "shutdown"]
SDK_NOTIFICATIONS = ["session.event", "session.status", "subagent.started", "subagent.finished"]

TUI_KEYS = ["Enter", "Ctrl+Enter", "Alt+Enter", "Ctrl+J", "行尾反斜杠加 Enter", "Backspace", "Delete",
    "左方向", "右方向", "Home", "End", "Ctrl+A", "Ctrl+E", "Ctrl+U", "上方向", "下方向",
    "Ctrl+C", "Esc", "PageUp", "PageDown", "Shift+Tab", "Ctrl+O", "Ctrl+T", "Ctrl+R", "?", "滚轮"]

DEFAULTS = [
    ("provider", "deepseek-official"), ("model", "deepseek-flash"), ("permission-mode", "workspace-write"),
    ("approval-policy", "ask"), ("bash timeout-ms", "60000"), ("repeat-thresholds", "[3,5,8]"),
    ("argumentsPreviewChars", "500"), ("sample-over-cap-glob-results", "false"),
    ("allow-parallel-in-progress", "true"), ("pruner threshold", "8192"), ("pruner head", "4096"),
    ("pruner tail", "1024"), ("spill max-inline-tokens", "12500"), ("标题回退", "5 词 40 字节上限 80"),
    ("LLM 标题 targetWords", "5（CJK 10）"), ("LLM 标题 maxInput", "4096 字节"),
    ("LLM 标题 timeout", "60s"), ("LLM 标题 maxOutputTokens", "4096"),
    ("agent-instructions maxBytes", "65536"), ("projection-cache", "200 事件 5000ms"),
    ("web searchProvider", "deepseek-official"), ("web fetchProvider", "http"),
    ("tool-web searchTimeoutMs", "60000"), ("tools 默认 mode", "native"),
    ("storage-domain backend", "json"), ("PTC execution-mode", "per-call"),
    ("PTC timeoutMs", "120000"), ("PTC maxTimeoutMs", "600000"), ("PTC maxOutputBytes", "64MiB"),
    ("PTC maxOldGenerationSizeMb", "512"), ("PTC maxMessageBytes", "128MiB"),
    ("PTC maxPendingCalls", "128"), ("PTC graceMs", "3000"), ("max-parallel-sub-calls", "10"),
    ("maxParallelToolCalls", "10"), ("ACP session-list-page-size", "100"),
    ("retry maxRetries", "5"), ("retry backoff initial", "500ms"), ("retry backoff max", "10000ms"),
]

def block(rid: str, title: str, scen_lines: list[str]) -> str:
    body = "\n\n".join(scen_lines)
    return f"\n@@{rid}\nRule: {rid} — {title}（枚举生成）\n\n{body}\n"

def scen(name: str, given: str, when: str, then: str, extra: list[str] | None = None) -> str:
    lines = [f"场景: {name}", f"  假设 {given}", f"  当 {when}", f"  那么 {then}"]
    for e in extra or []:
        lines.append(f"  并且 {e}")
    return "\n".join(lines)

out: list[str] = []

# 1) 工具全集：每工具 通用 5 维 + 特判
lines: list[str] = []
for name, domain, extras in TOOLS:
    slug = name.replace(" ", "-")
    for dim, when, then in DIMENSIONS:
        lines.append(scen(f"{slug} {dim}", f"工具 {name} 已注册", when, then))
    for ex in extras:
        lines.append(scen(f"{slug} 特判 {ex[:18]}", f"工具 {name} 在标准配置下", "执行相关路径时", ex))
out.append(block("REQ-FR-BUILTIN-TOOLS-1", "工具全集枚举", lines))

# 2) 会话事件全集：每事件 3 维
lines = []
for ev, trigger in EVENTS:
    lines.append(scen(f"事件 {ev} 触发", f"会话运行中满足条件：{trigger}", "结算时", f"日志追加 {ev} 事件且字段与 dsh types.ts 逐字一致"))
    lines.append(scen(f"事件 {ev} 表面归属", f"事件 {ev} 落盘", "重放分类时", "按类型正确判定 入表面 或 log-only"))
    lines.append(scen(f"事件 {ev} 回环", f"含 {ev} 的会话持久化", "写出并重载时", "事件逐字节还原"))
out.append(block("REQ-FR-SESSION-1", "事件全集枚举", lines))

# 3) StreamChunk 全型 × 结算规则
lines = []
for kind in CHUNK_KINDS:
    lines.append(scen(f"chunk {kind} 字段", "适配器流式输出", f"产生 {kind} chunk 时", "字段与 dsh types.ts 470 行定义逐字一致"))
lines.append(scen("usage 强制先于 finish", "正常结算流", "消费 chunk 序列时", "usage 出现在 terminal finish 之前且其后无任何 chunk"))
lines.append(scen("重复 close 忽略", "同一 block 索引收到两次 block-end", "装配时", "首次生效后续忽略"))
lines.append(scen("delta 落在已关闭块", "delta 的目标块已 block-end", "装配时", "该 delta 被忽略"))
lines.append(scen("缺 finish 默认 stop", "流结束但无 finish chunk", "装配时", "默认 finish kind 为 stop"))
out.append(block("REQ-FR-LLM-3", "StreamChunk 枚举与装配", lines))

# 4) SECTION_ORDERS 全表
lines = []
for i, s in enumerate(SECTIONS):
    lines.append(scen(f"section {s} 序值", "读取 SECTION_ORDERS 常量表", f"查 {s} 时", "序值与 dsh index.ts:131 全表一致"))
out.append(block("REQ-FR-PROMPT-2", "SECTION_ORDERS 全表", lines))

# 5) PTC 失败分类 × 触发
lines = []
for f in FAILURES:
    lines.append(scen(f"失败分类 {f} 归类", f"构造触发 {f} 的运行条件", "结算时", f"失败分类精确为 {f} 且 fail-closed 返回"))
out.append(block("REQ-FR-PTC-3", "失败分类枚举", lines))

# 6) ACP 方法面枚举
lines = []
for m in ACP_METHODS:
    lines.append(scen(f"ACP {m} 契约", "标准 ACP 客户端连接", f"调用 {m} 时", "行为与 dsh packages/acp 实现一致且 stdout 无协议外流量"))
out.append(block("REQ-FR-ACP-2", "方法面枚举", lines))

# 7) SDK 方法与通知枚举
lines = []
for m in SDK_METHODS:
    lines.append(scen(f"SDK {m} 契约", "JSON-RPC 客户端连接", f"调用 {m} 时", "行为与 PRD 18.4 表逐字一致"))
for n in SDK_NOTIFICATIONS:
    lines.append(scen(f"SDK 通知 {n}", "运行时内会话活动", f"触发 {n} 时", "载荷字段与 PRD 18.4 一致且实时下发"))
out.append(block("REQ-FR-SDK-2", "方法与通知枚举", lines))

# 8) TUI 键位全表
lines = []
for k in TUI_KEYS:
    lines.append(scen(f"键位 {k}", "TUI 运行中", f"按下 {k}", "行为与 PRD 18.2 键位表一致且有 TestBackend 断言"))
out.append(block("REQ-FR-TUI-4", "键位全表枚举", lines))

# 9) 默认值全表
lines = []
for k, v in DEFAULTS:
    lines.append(scen(f"默认值 {k}", "全默认配置启动", f"读取 {k} 时", f"值为 {v}"))
out.append(block("REQ-FR-CONFIG-6", "默认值全表枚举", lines))

text = "".join(out)
OUT.write_text(text, encoding="utf-8")
count = text.count("场景: ")
sys.stdout.write(f"generated {count} scenarios across {text.count(chr(64)+chr(64))} sections -> {OUT}\n")
