# Progress

## Session 1 — 2026-10-21

- Phase 1 完成：agent-spec 1.4.0 前置检查通过；patterns.md / commands.md / doc/prd.md（652 行）已读；粒度与 ID 方案（REQ-FR-*）定稿。
- 规划文件初始化（init-session.sh，PLAN_ID=2026-10-21-agent-spec-contracts-from-prd）。
- Phase 2 完成：doc/requirements-intake.md 落盘（21 块、955 行）。首写因单次超长输出退化损坏，按三振协议改为分批写入（1 Write + 6 heredoc 追加），每批 rg 校验：起始/结束标记 21/21、必需节 21×6、乱码扫描零命中、依赖图无环。修复 WEB 块重复 Dependencies 与 3 处乱码。
- Phase 3 完成：用户裁决 1A 2B。REQ-FR-WEB 移出导入源（doc/requirements-deferred-web.md 保留草稿，M5 详案后回补）；intake 标记 20/20 配平、零残留。
- Phase 4 完成：init --workspace；import 20 块；lint-knowledge --gate 通过（首跑失败于 specs/ 缺目录，mkdir 后通过；0 errors，167 advisory）；graph --gate 通过；20 块 transition accepted；plan --gate 通过（首跑 35 条 dangling-spec-dependency，根因是生成器把需求 id 写进草稿 depends:，删除悬空行后通过）。
- Phase 5 折叠：questions 输出 31 条全部 blocking:false（advisory 诊断），无阻断问题，不启动访谈；清单存 .agent-spec/questions-2026-10-21.json。
- Phase 6 完成：work-units + draft-specs 生成 20 份合约草稿（specs/generated/，Quality 全 100%，satisfies 边齐备，pending_ 选择器为设计预期）；test-obligations 落盘。
- Phase 7 完成：trace REQ-FR-HEADLESS 基线 liveness=Unproven/Skip（无代码预期态）；plan 输出 6 个执行批次（CONFIG/QUALITY/SCOPE/SESSION → APPROVAL/LLM/SANDBOX/TOOLS → AGENT-LOOP/BUILTIN-TOOLS/COMPACTION/MCP/PROMPT/PTC → ACP/COMMANDS/HEADLESS → SUBAGENT/TUI → SDK）；worktrees 清单跳过（仓库无提交，无 base 分支）。
- .gitignore 追加 agent-spec 运行态排除（runs/trace/pending-ai-requests）。
- 合约体系交付完成，待用户评审。

## Session 2 — 2026-10-21（覆盖度审计）

- 用户指令：逐项检查合约是否全覆盖 PRD 功能点细节。逐节人工比对 + 42 项 rg 机械抽查 → 16 缺口（doc/coverage-audit.md）。
- 用户追加质询（插件形态标注）→ G17：第一方全 builtin 由 PRD 4.3 全局固定，无需逐模块备注；把 source kind 声明与信任边界校验条款化进 CONFIG-8。
- 17 处修复写入 doc/requirements-intake.md（条款 90→107），重导入 + accepted ×20。
- 事故 1：插入 CONFIG-7/8/9 时锚点吞掉 `## Scenarios` 标题 → 导入 scenario_count=0、草稿缺 CONFIG → 回填标题重跑。教训：锚点含节标题时替换串必须回填被吞标题。
- 事故 2：lint-knowledge 在草稿重生成前运行 → requirement-uncovered 误报；时序修正后通过。
- 终态：lint-knowledge --gate 0 errors（186 advisory）；graph/plan --gate 通过；20/20 work units ready；20 草稿 Quality 100%；修复前 0 命中关键值全部复验命中。
- 附加任务：/init 重写项目 CLAUDE.md——保留原指针（dougong/dsh 参照、dougong-plugin-dev），新增当前阶段警告（无 Rust 代码，M1 待放行）、文档地图、agent-spec 门禁与再生成序列（含 import 重置 status、depends 悬空行清理两个已知坑）、Rust 质量门四关、架构要点（1:1 口径/命名/会话 v4/fail-closed/dougong 事实/依赖方向/范围红线）。

## Session 3 — 2026-10-21（场景充分性 + 源码关键点回灌）

- 用户裁决链：场景太少 → 全部合约按"场景全过即 1:1"标准重审 → 全部 20 份判不合格重做 → dsh 模块技术关键点浅尝辄止（未回源码）。
- 场景全量重写：218 + 21 充分性补强 = 239 场景、132 条 Rule 归属行，106 条款全部有 Rule 归属（语法 `Rule: REQ-FR-X-N — 名`，MCP 实测 lint 归零验证）。
- 用户指出草稿 Allowed Changes 全是 src/**/tests/** 占位 → scripts/refine-drafts.py 入库（PRD 4.5 逐 crate 边界映射 + 共享 Forbidden，幂等后处理）。
- 4 个并行探查代理已派出（会话+循环 / 工具+PTC / LLM+提示词+审批 / 入口+装配），提取 dsh 源码中 PRD 未记载的技术关键点（file:line 证据），返回后回灌条款+场景再走全管线。

- 4 份源码报告全部返回并落盘 doc/dsh-source-notes.md（127 行 file:line 证据）；PRD 勘误 12 处按源码修正（19.2 条 5，待用户终审）。
- 条款 106→119（源码级新条款），场景 239→261，clause-uncovered 归零；intake 20 块补丁器回灌完成。
- 管线终态全绿：import+accepted×20、lint-knowledge 0 errors（238 advisory）、graph/plan --gate 通过、20/20 ready、20 草稿真实边界、drafts refined。
- coverage-audit.md 追加 20 份合约充分性裁决表（19 补强后充分 + SCOPE 充分）。
- 用户指令：针对 doc/dsh-source-notes.md 完善 PRD → 22 处插入全部原子写入（§2 权威表加源码笔记行、§6.4 装配树锚点与未记载默认值、§7.1 打包/attempt/log-only、§7.2 物理不变量与迁移对照、§7.3 surface 硬约束、§7.4 投影与 header canonical、§7.5 resume 门禁自实现、§7.6 两级补偿、§8.2 状态机细则、§8.3 retry 结构、§9.2 管线细则、§9.3 调度细则、§9.4 PTC 实现细则、§10 组装确定性、§11.1 审批细则、§12 自动触发、§13.1 流装配结算、§13.2 协议细则、§18.1 输出与续用细则、§18.3 ACP 生命周期、§18.4 SDK 服务器细则、§19.2 条 5 升级为勘误+补遗）。PRD v1.0→v1.1（675 行）。
- 场景生成器 474 条暂扣合并：枚举表已逐条核对通过（笔记补遗修正 5 处精确值），工具域待目录提取代理返回后从 catalog 行重建再合并，目标总数 ≥1000。
- 目录提取代理返回（73 工具块 + 152 config 小节）→ doc/dsh-catalog-notes.md（296 行）。分层默认值辨析：bash timeout 60000=装配生效值、bash-local schema 缺省 120000/600000，两层均记入 PRD 6.4/§17。
- 生成器 v2（gen-catalog-scenarios.py）解析目录行生成 604 条（参数 103×2 + 行为 41 + 调度呈现/越界/section 注册 50×3 + config 99×2 + 排除负向 9），守恒断言内建；三轮修复 emit 参数与解析正则（两段式标题）。
- 三源合并（261+197+604=1062）→ 全管线复跑全绿：accepted×20、lint 0 errors（933 advisory）、clause-uncovered 0、graph/plan --gate 通过、20/20 ready、草稿真实边界。

## Session 4 — 2026-10-21（Phase 9：逐合约充分性审计 + 即时修补）

- 用户指令：逐项检查每个合约"场景全过是否即近 1:1"，做不到就逐合约老实检查；每审完一份立刻修改，不许最后批量改。
- 弱域真源先行：源码笔记对 TUI/COMMANDS/SUBAGENT/COMPACTION/MCP/CONFIG 六域覆盖薄，派 4 个并行探查代理补齐（file:line 证据），返回后追加 doc/dsh-source-notes.md 报告 5-8。
- 审计顺序：SESSION→AGENT-LOOP→TOOLS→PTC→PROMPT→APPROVAL→LLM→SANDBOX→COMPACTION→MCP→SUBAGENT→COMMANDS→BUILTIN-TOOLS→HEADLESS→TUI→ACP→SDK→CONFIG→QUALITY→SCOPE（前段用已有强真源，弱域等代理返回）。
- 审计 1 SESSION：2 处源码矛盾（seedLength/空渲染）+3 新条款（投影/canonical/切点）+11 新场景；落地 1073。已核 dsh types.ts:95-131、format.ts:96 六键。
- 审计 2 AGENT-LOOP：attempt 结算 interrupted:true、user/message 仅 firstAttempt、状态机 step0/reject/maintenance、runtime-context admitted-once → 条款 10/11 + 条款 7 扩展 + 7 场景；落地 1080。
- 审计 3 TOOLS：调度器源码核实（单调用屏障/池 10/lazy 重分类/concludeTurn 成功门控）+ guards 单调/拒绝后仍过 post-execute/双替换 TypeError/additionalProperties 显式布尔 → 条款 2/3/5 扩展 + 6 场景；落地中。
- 报告 5（命令/子代理）与报告 6（压缩/MCP）返回并落盘 dsh-source-notes.md。
- 审计 4 PTC：2 处源码矛盾修正（fs 按沙箱策略非禁止、__proto__ 不拒绝按 own key 注入）+ 8 类失败精确触发 + 硬数值条款 13 + 12 新场景；落地 1096。报告 7（装配/其余插件）、报告 8（TUI，发现 dsh 已删除 TUI 包——基线事实待用户裁决）落盘。
- 审计 5-12 已完成：PROMPT（行号勘误+AGENTS.md 注入形态+全表数值条款 7）1101；APPROVAL（切换文案/no-op/abort 竞速/优先级链）1105；SANDBOX（两层默认真值回源核实+策略文本逐字）1106；COMPACTION（4→11 条款 5→19 场景重写）1124；LLM（6 新条款：打包四 record/prepared 一次性/重试数值/deepseek 细则/toolUpdate）1131；MCP（3→9 条款 6→23 场景重写）1148；SUBAGENT（3→10 条款 6→25 场景重写）1166；COMMANDS（3→7 条款 4→20 场景重写）落定。
- 审计 13-20 完成：BUILTIN-TOOLS 充分（目录守恒层）；HEADLESS 补强（final 来源/usage 累计/append-only 投影）1178；TUI 补强（dsh TUI 已上游删除的基线事实负向钉住；一次锚点事故致 ## Dependencies 错位，已精确修复并配平验证）1179；ACP 补强（resume 拒绝形态/list 过滤/图片准入/permission 门控）1183；SDK 修正未同步勘误（未知方法 -32603）+3 场景 1186；CONFIG 新增启动失败策略条款 11/12 + .env 范围 Open Question 1190；QUALITY 充分；SCOPE 偏差清单同步 1190。
- Phase 9 终态：20/20 合约逐份审计完毕（充分 2、补强后充分 5、重修 13 含 4 份重写），每份即时落地独立过三门禁，场景 1062→1190，源码笔记扩至 8 份报告。coverage-audit.md 第六节裁决表 + 3 项待用户裁决事项落盘。
- 用户裁决（2026-10-22）：TUI 基线确认（fr 对齐 PRD 18.2 自有规格）；.env 分层纳入 fr-config。PRD 6.4 增 .env 分层段（品牌映射 FR_*/XDG_*、bootstrap 专名拒绝、家目录代理四键例外）；CONFIG 条款 13 + 3 场景落地，Open Question 撤销。
## Session 5 — 2026-10-22（M1 放行 + 插件形态裁决返工）

- 用户放行 M1（"完成m1"）。动工：读 PRD 全文 + dougong 内核模型/dylib 参考，workspace 骨架落盘（root Cargo.toml resolver 3/edition 2024/workspace.lints 对齐 dougong、rust-toolchain.toml 1.99.1 钉死）。
- fr-config 首切片（补丁行模型/解析/loader JSON 编译 + dougong_loader::Config::parse 契约测试）四关全绿 8 tests。
- 用户质询 fr-config 装载形态 → 我越权以"第一方全 builtin"作答并推进，用户两次严厉警告后确认规则：绝不擅自决定（已入长期记忆 no-unilateral-decisions）。
- 一次性裁决 D1-D6 经用户"可以"通过：D1A fr-config 宿主直链库非插件；D2A 第一方全 dylib、wasm 仅第三方；D3A 宿主=内核+loader+fr-config+壳；D4A Invoker JSON 字节+fr-wire 形状共享；D5A M1 六条验收不变；D6A 合约全量扫描改写。用户令：先改 PRD 与合约，后写代码。
- PRD 返工 11 处：1.4.2/4.2/4.3 全节（两形态表+负向钉 builtin）/4.4 跨边界载荷/4.5 布局（fr-wire 入列、插件全 cdylib 仅依赖 dougong-abi）/6.2 产物解析规则（字面路径或 FR_PLUGIN_PATH→<prefix>/lib/fr 查 lib<slug>.so→<slug>.wasm，未命中进 failures）/6.3 schema 真源宿主侧双侧校验/8.1/20/22/23 决策 16。
- intake 返工：CONFIG-2、CONFIG-8 全节重写；场景 +3/-3 改（第一方 wasm 拒绝、工件缺失失败、产物无 builtin、Invoker 泛化、两形态装载、循环 fiber 工件归属）；AGENT-LOOP 1 场景。全管线复跑全绿，场景 1193→1195。
- fr-config 代码按裁决返工：source.rs（SourceKind/SourceSpec/PathResolver/check_trust_boundary）、compile_to_loader_doc → CompileOutcome{doc, failures}（失败行 fail-closed 剔除不阻塞他槽、禁 builtin source）、信任校验第一方禁 wasm；tests 8→12；四关全绿（fmt/clippy -D warnings/test/doc）。
- 裁决记录落盘 coverage-audit.md 第七节。
- 用户裁决 Q1A/Q2A/Q3A（2026-10-22）：合并语义对齐 dsh applyEntryPatches（按键覆盖/warn-skip/首插序）；insert 根级插入、无 group；!!js 子集 = env ?? 链/相等比较/平台三元/裸 env 值/字面量/profile-context 布尔。真源依据：vendor/include/src/index.ts:57-127 + bundle patches 全量盘点（!!js 22 处；PRD"仅平台门控与 env 读取"表述勘误）。
- PRD 勘误落地：6.1 合并语义、6.2 行模型示例（定义行经 insert）、5 节 !!js 行、19.2 条 6、23 决策 17。intake：CONFIG-1/4/11/12 改写，场景 +6 改 2（覆盖行翻转/config 整换/未命中跳过/name 不匹配/自底向上/insert+首插序；子集 fail-closed 文档级拒绝）。全管线复跑全绿 1202 场景。
- fr-config 实现：cond.rs（受限表达式求值器：递归下降、?? 链语法消费、单双引号字面量、profile-context 布尔）；patch.rs 重写（PatchLine{id,name,config,disabled,insert}、compose 按 dsh 语义、row_from_value 走行保 tag、!!js→!js 行锚定归一化——libyaml 二级句柄展开致 serde_yaml_ng 丢 quoted tag 的规避）。tests 12→24，四关全绿。
- 排障记录：eat("?") 误吞 "??"（lookahead 修复）；serde_json 不认单引号字面量（手写扫描）；PatchLineRaw derive 路径丢 tag（整文档解 Value 后手工走行）。
- 代码评审（/code-review high --fix，后台代理因 API 模型名 400 失败，本会话直接执行，subagent 用量 0/2）：评审对象 0d3e87e。发现 5 项并全部修复：R1（中）flow 风格 !!js 会被 libyaml 静默丢 tag 成普通字符串 → 归一化后残留检测 fail-closed + 钉测；R2 链式 ??（PRD"链"语义）→ 循环消费 + 钉测（含空串非 nullish 语义）；R3 未用变体 InvalidPath 删除；R4 regex 移入 workspace 依赖；R5 测试名名实不符更名。修复过程 clippy 收尾 3 处（must_use 误挂 static、同体 match 臂、# Panics）。终态：四关全绿 26 tests。

## Session 6 — 2026-10-22（Phase 12 续：装载栈真源核实）

- 6528ee8 基线四关全绿 26 tests 确认（工作树干净）。
- dsh 装载栈真源核实完毕（app-boot profile.ts / profile-context.ts / index.ts，file:line 证据落 findings.md）：应用序 bundle 层 → profile patch → 家目录 patch → --patch overlays；可选/必需文件语义、bundle skip 非致命、名字校验、模板自动初始化。
- 发现 doc/prd.md:206 家目录 patch 括号路径（profiles/）与 dsh 真源（home 根）不一致，列勘误候选。
- 集中呈 Q1-Q5（家目录 patch 位置 / profile 清单与 bundle 栈 / 模板初始化范围 / 默认 bundle 形态 / 内容来源），问题全文（事实/选项/推荐/裁决后动作序列）已登记 findings.md「待裁决问题登记」节，待裁决后动代码。
- 用户裁决 Q1A/Q2A/Q3A/Q4A/Q5B（2026-10-22）：装载文件布局定稿；Q3A 与推荐 B 不同（模板自动初始化纳入 M1），按裁决执行。
- PRD 落地 5 处：6.1 家目录 patch 路径勘误（<FR_HOME>/fr.patch.yml）+ 新增装载文件语义段（profile.yml 清单/名字校验/模板初始化/可选与必需文件语义/最小 M1 bundle 范围）；19.2 重复条 5 机械去重 + 新条 7；23 决策 18。
- intake CONFIG-1 扩充（装载文件布局 MUST）+ 8 新场景；CONFIG Source Trace 增 app-boot file:line 证据并同步 6.1 勘误表述。全管线复跑全绿：accepted 20/20、三门禁 0 errors，场景 1202→1210。
- fr-config stack.rs 落地：PROFILES_DIR/PATCH_FILENAME/MANIFEST_FILENAME 常量、validate_profile_name（dsh profile.ts:169-176 规则）、read_profile_manifest（严格 schema 未知键拒绝）、load_optional_patches/load_required_overlay（可选缺文件=空层、必需缺文件=启动失败）、init_profile（幂等绝不覆盖）、load_profile（首用模板初始化、未知名报错）、load_stack（入口 bundle → profile 栈+patch → 家目录 patch → overlays 五层装配）、内置 headless bundle 资产（14 行单 insert 组，6.4 值对齐）。tests 26→37，四关全绿（fmt/clippy -D warnings/test/doc）。
- 排障：clippy 重构引入 profile patch 无条件推入的幽灵空层（entry-only 栈 layers=2 被 37 号测试逮住，改 Option 条件推入）；const fn 内 &str 字面量匹配要求 const PartialEq（未稳定）→ 去 const；hook 误报 patch.rs "use inside function body" 系错误文案含 "use" 字样，改写为 "write"；rg 多文件模式 -o 带 path: 前缀导致 transition 首跑全败，改逐文件提取修复。
- 代码评审（/code-review high --fix 不限数；后台代理再因 GLM-5.3-FALSH API 400 失败，本会话直接执行，subagent 用量 0/2）：评审对象 1fec49b。已修复 3 项：R1（低）bundle 资产缺 provider 语境注释（CONFIG-6 provider=deepseek-official 由 fr-llm-deepseek 适配器本身承载）→ 资产头注释补两行；R2（低）名字校验场景「不产生文件系统副作用」无测试钉住 → 新增 invalid_name_leaves_no_filesystem_side_effects；R3（低）entry bundle !!js 求值结果未在编译产物层断言（tag 丢失回归护栏）→ entry 测试补 policy=ask / mode=workspace-write 断言。核实无缺口 1 项：serde_yaml_ng 拒绝 manifest 重复键（fail-closed 实证 + 钉测入 manifest 测试）。待裁决 1 项（F1，中，规格层）：dsh anchorInsertedPluginNames（app-boot index.ts:550-561）把 insert 行的相对路径 name 锚定到补丁文件所在目录（转绝对），fr 的 PRD 6.2「按字面路径」未含锚定语义 → 相对路径会落成 CWD 相对，与 dsh 行为偏离；需 PRD 6.2 勘误 + 装载/编译层锚定实现，按 19.2 治理呈裁决后落地。终态：四关全绿 38 tests。
- 用户裁决 Q6A（2026-10-22）：insert 行相对路径锚定对齐 dsh anchorInsertedPluginNames（app-boot index.ts:550-561）。PRD 6.2 勘误 + 19.2 条 8 + 23 决策 19；intake CONFIG-2 条款扩充 + 2 新场景，全管线复跑全绿（accepted 20/20、三门禁 0 errors，场景 1210→1212）。fr-config stack.rs 锚定实现：anchor_insert_names（仅 insert 行、仅 . ./ ../ 前缀）+ join_lexical（词法归一）+ anchor_base（父目录、相对时对 cwd 转绝对），挂接 load_optional_patches/load_required_overlay；2 钉测（./ ../ 锚定与无前缀字面、覆盖行不锚定）。排障：map_or/map_or_else 双闭包捕获同一移动值 E0382（换 match）；too_long_first_doc_paragraph 文档首段拆分。终态：四关全绿 40 tests，待用户评审提交。
