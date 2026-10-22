#!/usr/bin/env python3
"""精化 specs/generated 任务合约草稿（draft-specs 生成后的必备后处理）。

1. 删除生成器误写入 frontmatter 的悬空 depends: [REQ-*] 行（该字段只接受 spec 名）。
2. 将占位 Allowed Changes（src/**、tests/**）替换为 PRD 4.5 对应子系统的真实 crate 边界。
3. 追加共享 Forbidden（依赖方向、禁改条款换绿灯、禁引入排除清单项）。

幂等：重复运行结果一致。用法：python3 scripts/refine-drafts.py
"""
from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SPECS = ROOT / "specs" / "generated"

BOUNDARIES: dict[str, list[str]] = {
    "REQ-FR-CONFIG": ["crates/fr-config/**", "crates/fr-schema/**", "crates/fr/**"],
    "REQ-FR-SESSION": ["crates/fr-session/**"],
    "REQ-FR-AGENT-LOOP": ["crates/fr-agent-loop/**"],
    "REQ-FR-TOOLS": ["crates/fr-tools/**"],
    "REQ-FR-PTC": ["crates/fr-ptc/**"],
    "REQ-FR-PROMPT": ["crates/fr-system-prompt/**"],
    "REQ-FR-APPROVAL": ["crates/fr-approval/**"],
    "REQ-FR-SANDBOX": ["crates/fr-sandbox/**"],
    "REQ-FR-COMPACTION": ["crates/fr-compaction/**"],
    "REQ-FR-LLM": [
        "crates/fr-llm/**",
        "crates/fr-llm-deepseek/**",
        "crates/fr-llm-openai/**",
        "crates/fr-llm-anthropic/**",
    ],
    "REQ-FR-MCP": ["crates/fr-mcp/**"],
    "REQ-FR-SUBAGENT": ["crates/fr-subagent/**", "crates/fr-subagent-acp/**"],
    "REQ-FR-COMMANDS": ["crates/fr-commands/**"],
    "REQ-FR-BUILTIN-TOOLS": ["crates/fr-tools-builtin/**"],
    "REQ-FR-HEADLESS": ["crates/fr-headless/**"],
    "REQ-FR-TUI": ["crates/fr-tui/**"],
    "REQ-FR-ACP": ["crates/fr-acp/**"],
    "REQ-FR-SDK": ["crates/fr-sdk/**"],
    "REQ-FR-QUALITY": ["crates/**", "Cargo.toml", "rust-toolchain.toml"],
    "REQ-FR-SCOPE": ["crates/**", "doc/**"],
}

SHARED_FORBIDDEN = [
    "任何领域 crate 不得依赖入口 crate（fr-headless/fr-tui/fr-acp/fr-sdk）",
    "不得修改本合约 satisfies 指向的 knowledge/requirements 条款来让验证通过",
    "不得引入 PRD 19.1 排除清单条目",
]


def satisfies_id(text: str) -> str | None:
    m = re.search(r"^satisfies: \[(REQ-FR-[A-Z-]+)\]", text, re.M)
    return m.group(1) if m else None


def refine(path: Path) -> bool:
    text = path.read_text()
    original = text
    req = satisfies_id(text)
    if req is None:
        return False

    # 1. 悬空 depends 行
    text = re.sub(r"^depends: \[REQ-FR-[^\n]*\n", "", text, flags=re.M)

    # 2. Allowed Changes 替换
    globs = BOUNDARIES.get(req)
    if globs:
        allowed_block = "### Allowed Changes\n" + "".join(f"- {g}\n" for g in globs)
        text = re.sub(
            r"### Allowed Changes\n(?:- .*\n)+", allowed_block, text, count=1
        )

    # 3. 共享 Forbidden 追加（去重）
    m = re.search(r"(### Forbidden\n(?:- .*\n)+)", text)
    if m:
        block = m.group(1)
        add = "".join(f"- {f}\n" for f in SHARED_FORBIDDEN if f not in block)
        text = text.replace(block, block + add, 1)

    if text != original:
        path.write_text(text)
        return True
    return False


def main() -> int:
    if not SPECS.is_dir():
        sys.stderr.write(f"missing {SPECS}\n")
        return 1
    changed = 0
    for path in sorted(SPECS.glob("*.spec.md")):
        if refine(path):
            changed += 1
    sys.stdout.write(f"refined {changed} specs under {SPECS}\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
