# Task Plan: 基于 doc/prd.md 生成 agent-spec 合约 → M1 headless 实现

## Goal
将 doc/prd.md（fly-rafter 唯一需求真源）转化为 agent-spec 合约体系（已完成，1193 场景门禁全绿）；随后按 PRD 第 20 节实现 M1（headless）：约 20 个 crate、六条验收标准，质量门四关全绿。

## Next Step
Phase 12 续：分层文件装载栈（入口默认 bundle / profile 栈 / 家目录 patch / --patch 的加载与展开 API）。

## Current Phase
Phase 12

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
- [x] doc/coverage-audit.md 第六节裁决表 20 行裁决 + 3 项待裁决事项落盘
- **Status:** complete

### Phase 10: 裁决落地与首次提交
- [x] 三项裁决落地（TUI 基线、.env 分层 CONFIG-13、勘误维持），全管线复跑全绿（1193 场景）
- [x] root commit `7bf9493`（70 文件，26045 行），工作树干净
- **Status:** complete

## M1（headless）实现阶段 — PRD 第 20 节

范围：workspace 骨架与质量门；fr-config 分层编译（含 .env 分层与 dump-config/dump-config-schema）；fr-session v4 全量；fr-agent-loop；fr-tools 管线 + 第 17 节全部工具（subagent-acp 桥除外）；fr-approval/fr-sandbox；fr-compaction；fr-system-prompt；三 provider；fr-mcp；fr-ptc；进程内子代理 spawn/fork；fr-shell（bash seam + 作业管理，工具域依赖）；fr-headless + fr 宿主。验收：1. `fr headless "任务"` 完整 turn 周期产出 v4 会话文件；2. `--json` 事件流过 schema 校验；3. 与 dsh 同任务会话日志逐事件同型；4. 审批 fail-closed / --session-id 续用 / abort 补偿集成测试；5. dump-config 系可用；6. 质量门四关全绿。

### Phase 11: workspace 骨架与质量门基线
- [ ] root Cargo.toml（resolver 3、edition 2024、workspace.lints 对齐 dougong、workspace deps）
- [ ] rust-toolchain.toml（1.99.1，与 dougong 补丁级对齐）
- [ ] fr-config 骨架（首批 crate，含最少实现过四关）
- **Status:** in_progress

### Phase 12: fr-config 分层编译
- [x] 2026-10-22 Q1-Q3 裁决：合并语义对齐 dsh applyEntryPatches（按键覆盖/warn-skip/首插序）；insert 根级；!!js 子集扩充（env ?? 链/相等/平台三元/裸 env/字面量/profile-context 布尔）——PRD 6.1/6.2/5/19.2/23 与 intake CONFIG-1/4/11/12 已勘误，管线全绿 1202 场景
- [x] 补丁行模型 + 合并器 compose（dsh 语义）+ 产物解析 + 信任校验 + 编译产出 doc+failures（禁 builtin）
- [x] cond.rs 受限表达式求值器（递归下降；!!js→!js 行锚定归一化绕 libyaml tag 丢失）
- [ ] 分层文件装载栈（入口默认 bundle / profile 栈 / 家目录 patch / --patch 文件加载与展开）
- [ ] ConfigSchema 声明框架（宿主侧 schema 库，装配期校验进 failures，激活期双侧校验）
- [ ] .env 分层（进程 env > 项目 .env > FR_HOME/.env；已设名不覆盖；专名拒绝；代理四键例外）
- [ ] dump-config / dump-config-schema 数据面（CLI 侧在 fr 宿主）
- [ ] 对照 REQ-FR-CONFIG 场景逐条落测试（已落 24，随切片递增）
- **Status:** in_progress

### Phase 13: fr-session v4 全量
- [ ] SessionEvent 全集（判别联合、dsh types.ts 逐字字段、五扩展 log-only）
- [ ] 持久化物理不变量（wx+0600+fsync → link+unlink 发布 → 目录 fsync → append truncate 回滚 → zstd 撕裂尾恢复 → 写租约）与世代/版本校验
- [ ] surface/replace-op 硬约束全集（五类入表面、双闭区间、sourceEventSeqs 校验、折叠编码、双计数）
- [ ] deriveMessages 投影（不可变请求投影）+ canonical request/header（空 tools 省略、adapterDefaults 条件保留、顺序敏感相等比较）
- [ ] fork（sourceEventSeqs 前缀派生）+ resume 四拒门禁
- [ ] 对照 REQ-FR-SESSION 场景逐条落测试
- Foundation Note: 本 crate 是管线咽喉：fr-agent-loop/fr-headless 依赖其 deriveMessages/surface 语义，语义字段逐字对齐 dsh types.ts
- **Status:** pending

### Phase 13b: fr-llm seam + 三 provider
- [ ] fr-llm：LlmAdapter trait（stream 必实现 + 7 默认方法）、GenerateOptions、StreamChunk 七种、流装配结算（block-end 上 delta 忽略、重复 close 忽略、delta-only 兜底、缺 id/name 兜底 call-<index>、max-tokens 过滤、ReplayEnvelope 一致性）、prepare_call 代际绑定、toolUpdate 能力差异表
- [ ] provider_retry_policy 结构（normal/always、maxRetries 5、retryableCodes 默认 5 类、backoff 500/10000/jitter 0.1 对称抖动；执行方 llm-retry 挂 agent 失败步扩展点，LLM 核心不自执行）
- [ ] fr-llm-deepseek：Anthropic Messages 协议（/v1 强制补、SSE 解析、结算校验、stop_reason 映射、usage message_start 起底 message_delta 覆盖、错误分类 401/403→AUTH 等）
- [ ] fr-llm-openai / fr-llm-anthropic
- [ ] 对照 REQ-FR-LLM 场景逐条落测试
- **Status:** pending

### Phase 14: fr-tools + fr-approval + fr-sandbox
- [ ] fr-tools：ToolRegistry（register：名称/JSON Schema/executionMode/presentation/section order 1000-3000）、MCP raw schema 动态注册
- [ ] 执行管线五段（createExecution 冻结 → pre-execute waterfall（ask 交 ApprovalService）→ guards 单调串 → tools/execute waterfall → 工具体 → post-execute）；ptc 坍缩调用策略前置终结；post-execute 双替换 TypeError/失败结果禁 value 替换/finalize 快照冻结；成功结果过 output schema 校验
- [ ] 调度（isConcurrencySafe 严格 true 才 parallel；exclusive 组=单调用屏障；池上限 max-parallel-tool-calls 默认 10；lazy 重分类；concludeTurn 成功门控）
- [ ] fr-approval：ApprovalPolicy ask/never、request() fail-closed 'unavailable' 归一化、approval/policy 事件、'never' dispatch 前自判、abort 竞速 cancelled、切换文案逐字
- [ ] fr-sandbox：三预设表、FR_PERMISSION_MODE 覆盖、后端链 bubblewrap→Landlock/Seatbelt/ACL、sandbox-unavailable fail-closed
- [ ] 对照 REQ-FR-TOOLS/APPROVAL/SANDBOX 场景逐条落测试
- **Status:** pending

### Phase 15: fr-system-prompt + fr-agent-loop
- [ ] 提示词抽取脚本（scripts/extract-prompts，从 dsh 源文件生成常量，禁手抄）
- [ ] fr-system-prompt：有序 section/context 注册、SECTION_ORDERS/CONTEXT_ORDERS 全表、{{var}} 校验、tool-order 保留标记、complete section 至多一个、AGENTS.md/CLAUDE.md 注入（user 帧+system-reminder、Updated/removed 再注入）
- [ ] fr-agent-loop：inbox（claim 语义/一次 next-turn/next-step 全取/splice 规范化）、turn/preStep/step 控制流、waterfall 提案、流结算与重试、工具执行分组调度、状态机三相位、abort 补偿两级
- [ ] 对照 REQ-FR-PROMPT/AGENT-LOOP 场景逐条落测试
- **Status:** pending

### Phase 16: fr-tools-builtin + fr-shell
- [ ] fr-shell：bash 执行 seam、子进程/作业管理（job_kill/list/output）、沙箱策略执行体对接
- [ ] 第 17 节工具：shell 组（bash/pwsh/persistent）、文件组 7 件、web 2 件、todo/goal/jobs/schedule、交互 2 件（headless fail-closed）、会话查询 5 件、PTY 6 件（opt-in）、skill 2 件、lsp、working_directory、内省 3 件
- [ ] 每工具参数 schema 对照 dsh tool-catalog.md 逐字冻结
- [ ] 对照 REQ-FR-BUILTIN-TOOLS 场景逐条落测试
- **Status:** modules pending

### Phase 17: fr-compaction + fr-mcp + fr-ptc
- [ ] fr-compaction：剪枝 8192/4096/1024、压缩契约与自动触发、图片卸载、spill max-inline-tokens 12500、replace-op 落地
- [ ] fr-mcp：stdio + Streamable HTTP、命名空间、raw schema 动态注册、资源工具 3 件
- [ ] fr-ptc：node 子进程 strip-only、wire 4B BE 长度前缀、8 类崩溃分类、绑定名校验并集、声明生成、子调度 submission-ordered 池 10、approveEscalation、persistent-in-memory opt-in
- [ ] 对照 REQ-FR-COMPACTION/MCP/PTC 场景配平
- **Status:** modules pending

### Phase 18: fr-subagent + fr-headless + fr 宿主
- subagent spawn/fork 子 realm、配套 5 工具；headless CLI/输出/截断梯/--session-id 四拒门禁；fr 装配器+事件泵+入口分发；dump-config 系 CLI；M1 验收六条逐条验证
- **Status:** pending

### Phase 19: M1 验收收尾
- [ ] 六条验收标准逐条验证（headless 走通/--json schema 校验/与 dsh 逐事件同型 diff/三集成测试/dump-config 系/四关全绿）
- [ ] 生成式文档（tool-catalog/config-catalog 等价物）与 dsh 同名目录 diff 评审
- [ ] 里程碑精化：绑定真实测试选择器替换 pending_*（按 crate 逐个）、lifecycle 从 Skip 转真实验证
- **Status:** pending

## Decisions Made
| Decision | Rationale | Rationale |
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
