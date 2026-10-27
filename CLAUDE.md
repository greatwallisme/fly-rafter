# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## 项目

Fly Rafter（飞椽，命令 `fr`）：以 Dougong 为基座的 Rust 版 agent harness 系统，行为与协议 1:1 对标 deepseek-harness（下称 dsh）。全部组件均为 Dougong 插件（agent 主循环也是插件）；宿主二进制只做装配器 + 事件泵 + 入口分发。

基座与参照：
- dougong：crates.io https://crates.io/crates/dougong ，本机 `~/project/dougong`（内核设计裁决见其 `doc/research/dougong-design.md`）
- deepseek-harness：https://github.com/deepseek-ai/deepseek-harness.git ，本机 `~/project/reference/cordis/deepseek-harness`（行为权威规格：其 `docs/architecture.md`、`docs/tool-catalog.md`、`docs/config-catalog.md`）

## 当前阶段（2026-10-28）

M1 实现进行中，已落切片（均按七步合约流闭环、四门全绿）：fr-config 分层编译（切片 1-6，dump-config 系 CLI 前置）；fr-session v4 全量（SESSION 82 真实/0/26）；fr-llm seam+三 provider（B2 通知拉取流消费）；fr-tools+fr-approval+fr-sandbox（TOOLS/APPROVAL/SANDBOX 绑定）；fr-agent-loop（AGENT-LOOP 23 真实+3 跨切片 skip：fiber_realm Phase 18/provider_retry llm-retry 外/header_reason fr-session 侧）；fr-system-prompt+loop 桥（切片 8 闭环：PROMPT 46 真实+1 跨切片 skip=working-directory context Phase 18；测试基线 lifecycle 82/0/26、CONFIG 60/0/259、AGENT-LOOP 23/0/3、PROMPT 46/0/1，workspace 862 tests）。绑定回放脚本：`scripts/bind-session-tests.py` / `bind-config-tests.py` / `bind-prompt-tests.py`（draft-specs 全量重生成后必须重放，其余切片的绑定随各切片提交内联维护）。注意：workspace 测试门与 PROMPT lifecycle 需 `FR_DSH_SOURCE=<本机 dsh checkout>`（缺席时 extract-prompts --check 钉测按设计 fail）。下一片：fr-tools-builtin + fr-shell（Phase 16）。入口顺序 headless → TUI → ACP → SDK → web（PRD 第 20 节里程碑 M1-M5）。

## 文档地图（真源优先级）

1. `doc/prd.md` —— 唯一需求真源（23 节，含决策记录、偏差治理规则）
2. `doc/coverage-audit.md` —— PRD 与合约的覆盖度审计（17 处缺口及修复记录）
3. `doc/requirements-intake.md` —— KLL 导入源（20 个 `<!-- agent-spec:requirement -->` 标记块）；`doc/requirements-deferred-web.md` —— Web 块（M5 延后，用户裁决）
4. `knowledge/requirements/` —— 20 个 accepted 需求（107 条 MUST 条款），由 intake 导入生成的派生物
5. `specs/generated/` —— 20 份任务合约草稿（`satisfies: [REQ-*]`；`pending_*` 测试选择器待对应里程碑绑定真实测试）

## 常用命令

合约体系门禁（agent-spec CLI，任何需求或合约改动后必须全绿）：

```bash
agent-spec lint-knowledge --knowledge knowledge --gate                          # 0 errors 才算通过；warning 是 advisory
agent-spec requirements graph --knowledge knowledge --format json --gate
agent-spec requirements plan --knowledge knowledge --specs specs/generated --format json --gate
```

修改需求后的再生成序列（需求真源在 intake；KLL 与草稿是派生物，不要手改）：

```bash
agent-spec requirements import --from doc/requirements-intake.md --out knowledge/requirements
# import 会把 status 重置为 proposed，需重新核准（依据用户确认）：
agent-spec requirements transition REQ-FR-<ID> --to accepted
rm -rf specs/generated && agent-spec requirements draft-specs --knowledge knowledge --out specs/generated
python3 scripts/refine-drafts.py   # 依赖行清理 + PRD 4.5 真实 crate 边界 + 共享 Forbidden
# 生成器会把需求 id 误写进草稿 frontmatter depends:（该字段只接受 spec 名），必须删除：
sed -i '/^depends: \[REQ-FR-/d' specs/generated/*.spec.md
# draft-specs 全量重生成会把 Test 选择器回退为 pending_*；已实现切片须重放幂等绑定脚本：
python3 scripts/bind-session-tests.py && python3 scripts/bind-config-tests.py   # SESSION 82 真实 / CONFIG 60 真实（重放后核对 lifecycle 基线 82/0/26 与 60/0/259）
agent-spec requirements questions --knowledge knowledge --specs specs/generated --format json   # 反向访谈问题（blocking 才需处理）
agent-spec trace REQ-FR-<ID> --specs specs/generated --code .                                   # satisfies 追溯与 liveness
```

Rust 质量门四关（PRD 第 21 节定义，实现开始后生效；零警告零错误，禁止放宽 lint 或断言换绿灯）：

```bash
cargo fmt --check && cargo clippy -- -D warnings && cargo test && cargo doc
cargo test -p <crate> <test_name>   # 运行单个测试
```

## 开发工作流（agent-spec 七步合约流，2026-10-22 起强制）

需求层三开关（`lint-knowledge` / `requirements graph` / `requirements plan`，见上文）之外，每个实现任务必须按七步合约流闭环：

| 步 | 所有者 | 命令 | 要点 |
| ---- | ---- | ---- | ---- |
| 1 写合同 | 人 | `agent-spec init` 后手写四要素 | 异常场景 ≥ 正常场景 |
| 2 质量门 | 机器 | `agent-spec lint <spec> --min-score 0.7` | 对合同做代码审查 |
| 3 读合同 | agent | `agent-spec contract <spec>` | 决策、边界、完成条件三重约束 |
| 4 自检循环 | agent | `agent-spec lifecycle <spec> --code . --format json` | fail → 读证据 → 修码 → 重跑，不许改合同 |
| 5 守卫 | 机器 | `agent-spec guard --spec-dir specs/generated --code . --min-score 0.7` | 全仓合同一次验证，pre-commit/CI 阻断（守卫非递归扫描，必须直指 specs/generated；skipped 计 not passing；质量分阈值与第 2 步同为 0.7，不用工具默认 0.6） |
| 6 验收 | 人 | `agent-spec explain <spec> --format markdown` | 读合同级摘要，不读 diff |
| 7 盖章归档 | 机器 | `agent-spec stamp <spec> --dry-run` / `agent-spec archive` | Git trailer 建立合同↔提交溯源 |

**第 4 步纪律**：lifecycle 失败时，义务是修代码，不是改合同——改合同让验证变绿是谄媚（sycophancy）不是修复；确需修改合同（合同本身错误或场景缺失）必须先与用户沟通裁决后才可改动。

## 架构要点（需要跨文件理解的部分）

- **1:1 口径 = 行为与协议对齐**：工具名、提示词文本、会话格式、SDK/ACP wire 逐字对齐 dsh；内部结构按 dougong 插件模型重造，不镜像 dsh 的 TypeScript 模块划分
- **命名硬约束**：Rust 标识符 snake_case（禁 camelCase）；自有用户可见命名（CLI flag、配置键、crate 名）kebab-case；环境变量 `FR_` 前缀 SCREAMING_SNAKE；dsh 的 camelCase 配置键转 kebab-case；协议 wire 层（会话事件字段、工具 schema、SDK/ACP 参数）豁免、保持逐字
- **协议即会话日志**：会话只实现 v4（追加式 JSONL + 可选 zstd，世代命名永不改名）；每次 LLM 请求由 deriveMessages 从日志确定性重建并冻结（逐字节可重现，对照测试锚点）；`tool/ptc-dispatch*` 子调用不进模型历史
- **fail-closed**：审批应答链缺席或失败 → `'unavailable'`（唯一授予值 `'allowed-once'`，无持久 allow-list）；沙箱后端缺失 → `sandbox-unavailable`；绝不静默放行
- **dougong 事实**：loader 只接受整份 JSON 文档（无分层/行覆盖/YAML）——`fr.patch.yml` 分层语义由 fr-config 重建后编译为 loader JSON 再 reconcile；wasm 插件零能力 WASI（fuel/epoch/内存帽）；Invoker 消费统一 `ctx.require`，禁 `ctx.get`
- **依赖方向**（PRD 4.5）：`fr` 只依赖 fr-config 与入口插件；任何领域 crate 不得依赖入口 crate；全部插件 crate 依赖 dougong
- **范围红线**：PRD 19.1 排除清单一律不实现（语音输入、Agent Teams、浏览器、遥测、插件市场、Python SDK、会话 v0-v3 迁移链等）；发现新的不可避免偏差先记录到 PRD 19.2 并经用户确认后才可落地
- **提示词文本**：除身份句（`You are an AI agent powered by Fly Rafter.`）外全部逐字取自 dsh，且必须由脚本从 dsh 源文件抽取生成常量，禁止手抄

## 技能

most important skill: `dougong-plugin-dev`（任何 dougong 插件、ABI、WIT、loader 相关工作必用）

其他项目技能：合约工作流 `agent-spec-tool-first` / `agent-spec-intent-compiler` / `agent-spec-authoring`；文件规划 `planning-with-files`（当前计划 PLAN_ID=2026-10-21-agent-spec-contracts-from-prd，位于 `.planning/`）
