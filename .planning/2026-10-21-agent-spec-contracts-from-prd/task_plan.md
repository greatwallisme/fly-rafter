# Task Plan: 基于 doc/prd.md 生成 agent-spec 合约

## Goal
将 doc/prd.md（fly-rafter 唯一需求真源）转化为 agent-spec 合约体系：KLL 需求（knowledge/requirements）+ 任务合约（specs/，satisfies: [REQ-*]），全程经 CLI 门禁与用户确认。

## Next Step
三项裁决已全部落定（TUI 基线确认、.env 分层纳入 CONFIG-13、勘误维持），合约体系门禁全绿（1193 场景）。等待用户放行后按 PRD 第 20 节 M1（headless）动工；动工时精化草稿（绑定真实测试选择器替换 pending_*）。

## Current Phase
Phase 10

## Phases

### Phase 1: 环境与切分设计
- [x] agent-spec CLI 前置检查（1.4.0 已安装）
- [x] 读 patterns.md / commands.md 参考
- [x] 读 doc/prd.md 全文，确定切分粒度：按子系统 21 块（PRD 第 5-21 节映射）
- **Status:** complete

### Phase 2: 起草候选需求块
- [x] 写 doc/requirements-intake.md（21 个 `<!-- agent-spec:requirement -->` 标记块，955 行）
- [x] 每块含 Problem / Requirements(MUST) / Scenarios / Dependencies / Source Trace / Open Questions
- [x] 结构校验：21 起始 + 21 结束标记、21×6 必需节、依赖图无环
- **Status:** complete

### Phase 3: 用户确认候选块
- [x] 用户裁决 1A 2B（2026-10-21）：粒度确认；WEB 块移出本期导入（草稿存 doc/requirements-deferred-web.md）
- **Status:** complete

### Phase 4: 导入 KLL + 门禁
- [x] `agent-spec init --workspace`（knowledge/ 五目录 + canon + config）
- [x] `requirements import`（20 块全部导入，status: proposed）
- [x] `lint-knowledge --gate` 通过（0 errors；167 条 advisory 提示已清单化）
- [x] `requirements graph --gate` 通过；20 块全部 `transition --to accepted`（依据本轮用户确认）
- [x] `requirements plan --gate` 通过（修复：删除生成器写入的悬空 `depends: [REQ-*]` 行——spec 级依赖应为 spec 名，需求级依赖真源在 KLL）
- **Status:** complete

### Phase 5: 反向访谈与答案回写
- [x] `requirements questions`：31 条全部 blocking:false（weak-then/single-statement 等 advisory 诊断），无阻断问题，访谈折叠；清单存 `.agent-spec/questions-2026-10-21.json`
- **Status:** complete

### Phase 6: 生成任务合约
- [x] `work-units` + `draft-specs`：20 份草稿落盘 `specs/generated/`，Quality 全部 100%
- [x] test-obligations 清单存 `.agent-spec/test_obligations.json`
- [ ] 里程碑启动时的人工精化：绑定真实测试选择器、补 stderr/输出模式场景、Rule 分组、真实 Boundaries（草稿态按设计预期 lifecycle 不通过）
- **Status:** complete（草稿态交付；精化移至 M1 启动）

### Phase 7: 依赖图与验证基线
- [x] `trace REQ-FR-HEADLESS` 基线：liveness=Unproven、场景 Skip（pending 选择器，无代码前预期态）
- [x] worktrees 清单跳过（仓库尚无提交、无 base 分支可排程，记录于 findings）
- **Status:** complete

### Phase 8: 覆盖度审计与修复（用户指令：检查合约是否全覆盖功能点细节）
- [x] 逐节人工比对 + 42 项机械抽查 → doc/coverage-audit.md（16 缺口）
- [x] 用户追加质询（插件形态 dylib/wasm 标注）→ G17 条款化（第一方全 builtin 无需逐模块标注；source kind 声明 + 信任校验入 CONFIG-8）
- [x] 17 处修复写入 intake → 重导入 → transition accepted ×20 → 门禁全绿（0 errors、20/20 ready、20 草稿 Quality 100%）
- [x] 事故记录：CONFIG 锚点吞 `## Scenarios` 标题致 draft 缺失；lint 时序误报——均已修复并复跑验证
- **Status:** complete

### Phase 9: 逐合约充分性审计（用户指令：场景全过是否即近 1:1；每审完一份立刻修改）
- [x] 弱域真源补齐：4 个并行探查代理（TUI/命令+子代理/压缩+MCP/装配+其余插件）→ dsh-source-notes.md 报告 5-8（含 dsh TUI 包已上游删除的基线事实）
- [x] 20 份合约逐份审计 + 即时修补：充分 2、补强后充分 5、重修 13（COMPACTION/MCP/SUBAGENT/COMMANDS 四份严重不充分重写）
- [x] 3 处源码矛盾修正（seedLength、空渲染、SDK -32603）+ PTC 2 处（fs 沙箱/__proto__）+ PROMPT 行号
- [x] scripts/regen-pipeline.sh 入库；每份修补独立过三门禁；场景 1062→1190
- [x] doc/coverage-audit.md 第六节裁决表 + 3 项待裁决事项落盘
- **Status:** complete

## Decisions Made
| Decision | Rationale |
|----------|-----------|
| 粒度 = 每子系统一个 REQ 块（21 块） | 每块对应 PRD 独立可验证行为域；工作单元阶段再按里程碑归组 |
| KLL 场景 DSL 用中文（场景/假设/当/那么） | 解析器双语支持（patterns.md）；项目文档语言为中文 |
| 标记属性仅 id/title/tags/source | commands.md 只记载这四个属性；confidence 放 Source Trace 条目，避免未记载属性引发导入风险 |
| 候选块独立落盘 doc/requirements-intake.md | import 只读显式标记块、不解释原始散文；PRD 保持纯净 |

## Errors Encountered
| Error | Resolution |
|-------|------------|
| 单次 Write 生成 955 行长文档时输出退化：SANDBOX 块标记错写为 agent-spec:block、后半 13 块缺失、尾部噪声、多处乱码 | 三振协议换方法：整体重写为分批写入（Write 前 3 块 + 6 次 heredoc 各 3 块），每批后 rg 校验标记数与乱码扫描；另修复 3 处乱码（Sur一概/prd页）与 WEB 块重复 Dependencies 节 |
| 首次 Write 路径笔误 /home/greatclosing（不存在） | 改用正确路径 /home/greatwallisme/project/fly-rafter/doc/requirements-intake.md |
| lint-knowledge --gate 首跑失败：唯一 Error 为 specs/ 目录不存在 | mkdir -p specs 后复跑通过（该目录随即被 draft-specs 填充） |
| plan --gate 失败：35 条 dangling-spec-dependency——生成器把需求 id 写入草稿 `depends:`，而该字段要求 spec 名 | 删除全部草稿的悬空 depends 行（依赖真源在 KLL Requirements 节，spec 经 satisfies 追溯）；复跑 plan --gate 通过 |
