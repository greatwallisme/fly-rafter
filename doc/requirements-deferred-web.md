# 延后需求块（暂不导入 KLL）

REQ-FR-WEB（Web 入口）——用户裁决 2026-10-21（Q2 B）：本期不导入 knowledge/requirements；M5 详案附录定稿后再补导入。
原始草稿块保留如下，届时移回 doc/requirements-intake.md 走 import 管线。

<!-- agent-spec:requirement id=REQ-FR-WEB title="Web 入口" tags=web,entry source=doc/prd.md -->
## Problem

Web 是最后一个入口（M5，能力清单级）。PRD 明确：动工前另出详案附录并评审，本阶段只锁定能力清单与默认监听地址；技术栈选型在详案附录定。

## Requirements

- [REQ-FR-WEB-1] MUST 提供本地 HTTP 服务器，默认监听 `127.0.0.1:3080`。
- [REQ-FR-WEB-2] 能力清单 MUST 覆盖（对齐 dsh web profile 用户可见面）：会话列表与续用；对话时间线（流式 markdown、工具卡、reasoning）；附件上传/拖拽/粘贴（图片草稿预览）；审批与 ask_user_question 界面；斜杠命令；plan mode 与 goal 面板；会话 fork；设置。
- [REQ-FR-WEB-3] M5 动工前 MUST 另出详案附录（技术栈与实现细节）并经评审，本需求不约束实现细节。

## Scenarios

场景: 服务器监听默认地址
  假设 以默认配置启动 web 入口
  当 服务就绪时
  那么 监听 127.0.0.1:3080 且会话列表可访问

## Dependencies

- REQ-FR-AGENT-LOOP
- REQ-FR-SESSION
- REQ-FR-APPROVAL

## Source Trace

- doc/prd.md#18.5: Web 能力清单级规格；详案附录先行评审后再动工
- confidence: medium（能力级；实现细节由未来详案附录定）

## Open Questions

- 详案附录的范围与评审时点（M5 前另出）？
- 技术栈选型（Rust 侧服务器 + 前端框架）在详案附录确定？
<!-- /agent-spec:requirement -->
