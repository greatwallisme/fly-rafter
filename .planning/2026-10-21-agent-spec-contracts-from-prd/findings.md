# Findings

## 2026-10-21 — agent-spec 合约生成

- `agent-spec` CLI 1.4.0 已安装（`~/.cargo/bin/agent-spec`）。
- `requirements import` 只消费显式 `<!-- agent-spec:requirement -->` 标记块（或 YAML 方言），从不解释原始散文；记载属性仅 id/title/tags/source。
- 记载的导入管线：import → lint-knowledge --gate → requirements graph --gate → plan --gate → work-units → draft-specs。`plan --gate` 在无满足规格早期会报 ready-without-satisfying-specs 诊断，属预期流水线状态，不是缺陷。
- 工作单元"ready"条件：需求无阻断 open questions 且有场景；open questions 非阻断 lint，但阻断 ready。
- spec 层（用户可见合约）解析器接受 `### Scenario:` 兼容形式，但作者规范要求裸 `场景:`/`测试:` 行；不允许自创顶级节（如 ## Milestones）。
- PRD 切分映射（PRD 节 → REQ 块）：6→CONFIG、7→SESSION、8→AGENT-LOOP、9.1-9.3→TOOLS、9.4→PTC、10→PROMPT、11.1→APPROVAL、11.2→SANDBOX、12→COMPACTION、13→LLM、14→MCP、15→SUBAGENT、16→COMMANDS、17→BUILTIN-TOOLS、18.1→HEADLESS、18.2→TUI、18.3→ACP、18.4→SDK、18.5→WEB、21+5.1+1.4.5→QUALITY、19→SCOPE。

## 2026-10-22 — Phase 12 装载栈 dsh 真源核实（Session 6）

基线：`6528ee8` 四关全绿 26 tests（fmt / clippy -D warnings / test / doc）。

dsh 装载栈权威事实（app-boot，仓库 ~/project/reference/cordis/deepseek-harness）：

- 应用序（readProfilePatches，packages/boot/app-boot/src/profile-context.ts:74-85）：bundle 层（profile manifest `dsh.profile.bundles` 顺序，每 bundle 的 `dsh.bundle.patch` 文件序，profile.ts:58-75）→ profile patch `<profile>/cordis.patch.yml`（可选，缺文件=空层）→ 家目录 patch `<home>/cordis.patch.yml`（可选，profile-context.ts:78）→ CLI `--patch` overlays（必需文件）→（dsh 末尾 telemetry patch，profile-context.ts:81-83；fr 按 PRD 19.1 排除）。
- 失败语义：patch 解析错误一律抛（parsePatchList，app-boot/src/index.ts:575-593）；可选 patch ENOENT 返回 undefined、其他读错误抛（loadOptionalPatches index.ts:520-529）；overlay 缺文件抛（loadOverlayPatches index.ts:540-548）；bundle 解析/读失败进 skippedBundles 非致命（profile.ts:751-768）+ stderr 一次性打印（reportSkippedBundles profile.ts:118-122）；manifest 读错误抛。
- profile 目录：`<home>/profiles/<name>/`（PROFILES_DIR，profile.ts:37）；名字校验拒 ''、含 `/`、含 `\`、`.`、`..`、`node_modules`（resolveProfileDir profile.ts:169-176）；manifest = package.json `dsh.profile.bundles`（loadProfileDirectory profile.ts:740-774）；首用模板自动初始化 PROFILE_TEMPLATES（profile.ts:179-195：headless=[dsh-base,dsh-headless]、acp=[base,acp-app]、web=[base,web-app]、sdk=[base,sdk-app]、sdk-minimal=[sdk-minimal]；loadProfile profile.ts:789-806 → initProfile profile.ts:262-281）；DEFAULT_PROFILE_BUNDLES=[dsh-base]（profile.ts:213）。
- 组合：composeEntries（profile.ts:816-823）= 摊平后单次 applyEntryPatches——fr 的 compose 已对齐此语义。
- bundle 行数盘点：base 1 顶层行/529 行文（单 insert 组平铺）、headless 7 行/43 行、acp-app 4 行/23 行、sdk-app 5 行/41 行、sdk-minimal 1 行/172 行、web-app 28 行/589 行（patch = 5 文件数组含 presets/ 4 份）。
- 命名映射：PROFILE_PATCH_FILENAME = cordis.patch.yml，profile patch 与家目录 patch 两处共用（profile.ts:40）→ fr 对应 fr.patch.yml（PRD 6.2 泛指格式名）。
- 不一致发现：doc/prd.md:206 家目录 patch 括号写 `~/.fly-rafter/profiles/`，dsh 真源为 home 根 `<home>/cordis.patch.yml`——列为勘误候选。

## 待裁决问题登记（2026-10-22 呈 Q1-Q5）— 已裁决：Q1A/Q2A/Q3A/Q4A/Q5B（2026-10-22）

背景：Phase 12 分层装载栈动工前核实 dsh app-boot 真源（见上节）；PRD 6.1 只钉应用顺序，文件布局与 bundle 语义存在分叉。按集中提问格式登记如下；裁决后先落 PRD/intake 勘误并复跑管线，再写 fr-config stack.rs。

### Q1 家目录 patch 位置（PRD 勘误候选）— 待裁决
- 事实：dsh 家目录 patch = `<home>/cordis.patch.yml`（home 根，profile-context.ts:78）；PROFILE_PATCH_FILENAME 两处共用（profile.ts:40）。PRD doc/prd.md:206 括号写 `~/.fly-rafter/profiles/`，与真源不一致。
- A（推荐）：对齐 dsh —— 家目录 patch = `<FR_HOME>/fr.patch.yml`，profile patch = `<FR_HOME>/profiles/<name>/fr.patch.yml`；PRD 6.1 勘误 + 19.2 记录
- B：按 PRD 字面放 `<FR_HOME>/profiles/` 下（偏离 dsh）

### Q2 profile 清单格式与 bundle 栈语义 — 待裁决
- 事实：dsh profile manifest = package.json `dsh.profile.bundles`（npm 包栈，node 解析 + pnpm 项目，profile.ts:740-774）；fr 无 npm 概念，PRD 未定义 fr bundle 工件形态。
- A（推荐）：profile 清单 = `<profile>/profile.yml`，`bundles:` 只引用宿主内置命名 bundle（base/headless/...），未知名 → skipped warning 非致命（dsh 语义）；第三方 bundle 不支持
- B：清单直接列 patch 文件路径（bundle 概念溶解为 patch 文件栈，偏离 dsh 模型）
- C：同构 package.json（`fr.profile.bundles`，无 npm 语义的空壳形式）

### Q3 profile 模板自动初始化（M1 范围）— 待裁决
- 事实：dsh 首用自动 init（PROFILE_TEMPLATES profile.ts:179-195；loadProfile → initProfile profile.ts:789-806、262-281；未知名且无目录报错）。
- A：M1 实现（1:1 行为）
- B（推荐）：M1 只读已存在 profile，初始化行为后置（M1 六条验收不含 profile；`fr headless` 不经 profile 可走通）

### Q4 入口默认 bundle 形态 — 待裁决
- 事实：dsh bundle patch 与用户 patch 同一 schema（app-boot/src/index.ts:268 `userPatchesSchema = entryListSchema`）。
- A（推荐）：嵌入 YAML 文本常量，走与用户 patch 完全相同的 parse_patch_lines 路径
- B：Rust 常量直接构造 PatchLine（绕过 YAML，与 dsh 单一解析路径不符）

### Q5 入口默认 bundle 内容来源与 M1 范围 — 待裁决
- 事实：dsh base = 1 顶层行/529 行文、headless = 7 行/43 行（上节 bundle 盘点）；fr M1 约 20 crate 与 dsh 90 行树不一一对应。
- A：脚本从 dsh bundle YAML 抽取 + 品牌映射表生成常量（类提示词抽取纪律；需行级映射表，部分行 M1 无对应 crate）
- B（推荐）：M1 手写最小 headless 默认 bundle（仅装 M1 存在的 crate，config 值逐条对齐 PRD 6.4 表，同型 = 单 insert 组平铺）；完整品牌映射抽取留后续（全树 dump-config diff 锚点时）

### 裁决后动作序列（任一裁决落地时执行）
1. Q1A → doc/prd.md 6.1 勘误 + 19.2 记录；Q2/Q3 涉及用户可见文件布局与行为范围，若改合约语义则同步 intake CONFIG 条款与场景，全管线复跑三门禁
2. fr-config stack.rs：可选/必需文件装载语义（dsh index.ts:520-593）、profile 名字校验（profile.ts:169-176）、bundle skip 非致命（skippedBundles 语义）、层装配 API（内置 bundle 常量 + profile 栈 + 家目录 patch + --patch overlays）
3. 对照 REQ-FR-CONFIG 场景落测试（CONFIG-1 分层场景族），四关全绿

### 裁决结果与落地记录（2026-10-22）
- Q1A：家目录 patch = `<FR_HOME>/fr.patch.yml`（PRD 6.1 勘误 + 19.2 条 7 + 23 决策 18）
- Q2A：profile 清单 `profile.yml`，`bundles` 仅引用内置命名 bundle（PRD 6.1 装载文件语义段 + CONFIG-1 条款）
- Q3A：模板自动初始化纳入 M1（与推荐 B 不同，按裁决执行；fr 模板 bundles=[] 为 Q2A+Q5B 的推导——入口 bundle 恒在层 1 不重复挂载）
- Q4A：入口 bundle = 嵌入 YAML 常量（`bundles/headless/fr.patch.yml`，include_str!，与用户 patch 同一 parse_patch_lines 路径）
- Q5B：M1 最小 headless 默认 bundle（14 行单 insert 组；6.4 工具级默认值随 Phase 16 fr-tools-builtin config schema 落地）
- 落地：PRD 5 处 + intake CONFIG-1 扩充 + 8 新场景（管线复跑全绿，场景 1202→1210）；fr-config stack.rs + bundle 资产，tests 26→37，四关全绿
