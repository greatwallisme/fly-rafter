#!/usr/bin/env python3
"""Extract model-visible session text constants from the dsh sources.

Model-visible wording (PRD 1:1 rule) is never hand-copied: this script reads
the authoritative TypeScript sources and regenerates the Rust constants in
crates/fr-session/src/closer_text.rs. Run from the repository root:

    python3 scripts/extract-session-texts.py

Source of truth: packages/core/session/src/repair.ts (CLOSER_TEXT and the
shared recovery codes) in the pinned deepseek-harness checkout.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

DSH_REPAIR_TS = Path(
    "~/project/reference/cordis/deepseek-harness/packages/core/session/src/repair.ts"
).expanduser()

OUT_PATH = Path(__file__).resolve().parent.parent / "crates/fr-session/src/closer_text.rs"

OBJECT_RE = re.compile(
    r"const CLOSER_TEXT = \{(?P<body>.*?)\} as const", re.DOTALL
)
ENTRY_RE = re.compile(r"(?P<key>[A-Za-z]+): '(?P<value>(?:[^'\\]|\\.)*)'")
CONST_RE = re.compile(
    r"export const (?P<name>TOOL_NOT_STARTED|TOOL_OUTCOME_UNKNOWN) = '(?P<value>[^']*)'"
)

HEADER = """\
//! Model-visible session text constants (GENERATED FILE — do not edit by
//! hand). Regenerate with `python3 scripts/extract-session-texts.py`; the
//! source of truth is the dsh `repair.ts` CLOSER_TEXT block.
//!
//! These texts reach the model as synthetic tool-result content, so they are
//! extracted verbatim from the dsh source per the PRD 1:1 wording rule.

// Rust guideline compliant {date}
"""


def unescape(literal: str) -> str:
    """Resolve the TypeScript single-quote escapes the source may contain."""
    return (
        literal.replace("\\\\", "\x00")
        .replace("\\'", "'")
        .replace("\x00", "\\")
    )


def main() -> int:
    source = DSH_REPAIR_TS.read_text(encoding="utf-8")
    block = OBJECT_RE.search(source)
    if block is None:
        print("error: CLOSER_TEXT block not found in repair.ts", file=sys.stderr)
        return 1
    # The two-level map is parsed per cause group; entry order inside a group
    # does not matter because the Rust constants are named by cause and stem.
    grouped: dict[str, dict[str, str]] = {}
    for cause in ("interrupted", "forked"):
        group_match = re.search(
            rf"{cause}: \{{(?P<group>.*?)\}}", block.group("body"), re.DOTALL
        )
        if group_match is None:
            print(f"error: CLOSER_TEXT group {cause} not found", file=sys.stderr)
            return 1
        grouped[cause] = {
            key: unescape(value)
            for key, value in ENTRY_RE.findall(group_match.group("group"))
        }

    consts = CONST_RE.findall(source)
    codes = {name: value for name, value in consts}
    if set(codes) != {"TOOL_NOT_STARTED", "TOOL_OUTCOME_UNKNOWN"}:
        print("error: recovery codes not found in repair.ts", file=sys.stderr)
        return 1

    def rust_str(value: str) -> str:
        escaped = (
            value.replace("\\", "\\\\")
            .replace('"', '\\"')
            .replace("\n", "\\n")
        )
        return f'"{escaped}"'

    lines = [HEADER.format(date="2026-10-23"), ""]
    for cause in ("interrupted", "forked"):
        group = grouped.get(cause, {})
        if set(group) != {"started", "notStarted"}:
            print(f"error: incomplete CLOSER_TEXT group {cause}", file=sys.stderr)
            return 1
        for stem in ("started", "notStarted"):
            name = "INTERRUPTED_" if cause == "interrupted" else "FORKED_"
            name += "STARTED" if stem == "started" else "NOT_STARTED"
            lines.append(f"/// CLOSER_TEXT.{cause}.{stem}, verbatim from dsh repair.ts.")
            lines.append(f"pub const {name}: &str = {rust_str(group[stem])};")
            lines.append("")
    for ts_name, rust_name in (
        ("TOOL_NOT_STARTED", "TOOL_NOT_STARTED"),
        ("TOOL_OUTCOME_UNKNOWN", "TOOL_OUTCOME_UNKNOWN"),
    ):
        lines.append(f"/// The dsh `{ts_name}` recovery code, verbatim.")
        lines.append(f"pub const {rust_name}: &str = {rust_str(codes[ts_name])};")
        lines.append("")

    OUT_PATH.write_text("\n".join(lines), encoding="utf-8")
    print(f"wrote {OUT_PATH}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
