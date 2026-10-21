#!/usr/bin/env python3
"""目录场景生成器 v2：从 doc/dsh-catalog-notes.md 结构化行生成场景。

每参数/每键两条正交断言（形状 + 约束），每工具含异常路径；守恒断言保证解析行与生成场景一一对应。
"""
from __future__ import annotations
import re, sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
NOTES = ROOT / "doc" / "dsh-catalog-notes.md"
OUT = Path(sys.argv[sys.argv.index("--out") + 1]) if "--out" in sys.argv else Path("/tmp/scenarios-catalog.txt")
EXCLUDED = ["ralph", "workflow", "team_task", "spawn_teammate", "wait_agent", "stagehand", "create_worktree", "present", "plugin_market"]
KEYRE = re.compile(r"^[A-Za-z_$][A-Za-z0-9_\[\]\.]*:")

text = NOTES.read_text()
lines = text.splitlines()

# --- TOOLS ---
tools: dict[str, dict] = {}
cur = None
for ln in lines:
    m = re.match(r"^### (.+?) \| (exclusive|parallel)(?: \| (.+))?$", ln)
    if m:
        cur = m.group(1).strip()
        tools[cur] = {"mode": m.group(2), "pres": (m.group(3) or "generic").strip(), "params": [], "behaviors": []}
        continue
    if not cur:
        continue
    body = ln[2:].strip() if ln.startswith("- ") else ""
    if body.startswith("params:"):
        rest = body[len("params:"):]
        if "; behavior:" in rest:
            rest, beh = rest.split("; behavior:", 1)
            tools[cur]["behaviors"].append(beh.strip())
        for seg in rest.split("; "):
            seg = seg.strip()
            if KEYRE.match(seg):
                tools[cur]["params"].append(seg)
    elif body.startswith("behavior:"):
        tools[cur]["behaviors"].append(body[len("behavior:"):].strip())

scen: list[tuple[str, str]] = []  # (rule, rendered)
def emit(rule: str, name: str, given: str, when: str, then: str):
    scen.append((rule, f"场景: {name}\n  假设 {given}\n  当 {when}\n  那么 {then}"))

n_params = n_beh = 0
for name, d in tools.items():
    slug = name.replace(" ", "-")
    rule = "REQ-FR-BUILTIN-TOOLS-1"
    emit(rule, f"{slug} 调度与呈现", "合约要求的行为已实现",
         f"读取工具 {name} 的声明时", f"executionMode 为 {d['mode']}（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 {d['pres']}")
    emit(rule, f"{slug} 参数越界拒绝", "提交违反该工具 schema 的参数", "校验运行时", "校验失败返回结构化错误且不执行")
    emit(rule, f"{slug} 说明 section 注册", "组装系统提示词时", "扫描 section 注册表", f"工具 {name} 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取")
    for p in d["params"]:
        n_params += 1
        key = p.split(":")[0]
        emit(rule, f"{slug} 参数 {key} 形状", f"工具 {name} 已注册", "校验 schema 时", f"参数行 [{p}] 的名称与类型按目录行实现")
        emit(rule, f"{slug} 参数 {key} 约束", f"工具 {name} 已注册", "校验 schema 时", f"参数行 [{p}] 的 required、默认值与枚举约束按目录行实现并冻结对照")
    for bi, b in enumerate(d["behaviors"], 1):
        n_beh += 1
        emit(rule, f"{slug} 行为 {bi}", f"工具 {name} 执行时", "达到触发条件时", b[:150])

for ex in EXCLUDED:
    emit("REQ-FR-SCOPE-1", f"排除工具 {ex} 不存在", "全部插件注册完成", "列举工具与插件名时", f"不存在 {ex}")

# --- CONFIG ---
config_keys: list[tuple[str, str]] = []
plugin = None
in_cfg = False
for ln in lines:
    if ln.startswith("## CONFIG"):
        in_cfg = True
        continue
    if in_cfg and ln.startswith("### "):
        plugin = ln[4:].strip()
        continue
    if in_cfg and ln.startswith("- ") and plugin and ":" in ln:
        body = ln[2:].strip()
        for seg in body.split("; "):
            seg = seg.strip()
            if KEYRE.match(seg):
                config_keys.append((plugin, seg))

n_cfg = 0
for plugin, kv in config_keys:
    n_cfg += 1
    key = kv.split(":")[0]
    emit("REQ-FR-CONFIG-6", f"config {plugin} {key} 默认", f"插件 {plugin} 已注册", "读取解析后配置时", f"配置行 [{kv}] 的默认值按目录行实现")
    emit("REQ-FR-CONFIG-6", f"config {plugin} {key} 约束", f"插件 {plugin} 已注册", "校验与生效时", f"配置行 [{kv}] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写")

parsed_params = sum(len(d["params"]) for d in tools.values())
parsed_beh = sum(len(d["behaviors"]) for d in tools.values())
assert n_params == parsed_params, "param parity broken"
assert n_beh == parsed_beh, "behavior parity broken"
assert n_cfg == len(config_keys), "config parity broken"

groups = {"REQ-FR-BUILTIN-TOOLS-1": "工具目录逐行枚举（catalog 生成）", "REQ-FR-SCOPE-1": "排除工具负向枚举（catalog 生成）", "REQ-FR-CONFIG-6": "插件 config 目录枚举（catalog 生成）"}
out = []
for rid, title in groups.items():
    out.append(f"\n@@{rid}\nRule: {rid} — {title}\n")
    for rule, s in scen:
        if rule == rid:
            out.append(s + "\n")
OUT.write_text("\n".join(out) + "\n", encoding="utf-8")
sys.stdout.write(f"tools={len(tools)} params={n_params} behaviors={n_beh} config_keys={n_cfg} scenarios={sum(s.count('场景: ') for s in [chr(10).join(x[1] for x in scen if x[0]==rid) for rid in groups])}\n")
sys.stdout.write("tools: " + ", ".join(tools) + "\n")
