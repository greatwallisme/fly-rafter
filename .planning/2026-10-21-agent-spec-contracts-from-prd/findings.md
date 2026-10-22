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

## 2026-10-22 — Phase 12 续：ConfigSchema 框架 dsh 真源核实（Session 7）

基线：`2050d12`（Q6A 锚定）四关全绿 40 tests 复跑确认。

dsh config-schema 真源（app-boot/src/config-schema/ + apps/cli/src/dump-config-schema.ts）：

- CLI：`--dump-config-schema`（args.ts:172，help "print JSON Schema for profile entries and patches without mounting"）；`runDumpConfigSchema`（dump-config-schema.ts:25-48）：stdout 单份 `JSON.stringify(dump, null, 2)`；diagnostics 逐条 stderr（`dsh: <level>:[<path>] <message>`）；`!dump['x-cordis'].complete` → exitCode 1；收集可执行插件顶层代码但从不 apply、从不求值 `!!js`。
- 输出形状 ConfigSchemaDump（types.ts:49-60）：JSON Schema 2020-12（$schema 固定 `https://json-schema.org/draft/2020-12/schema`；关键词子集 types.ts:7-26：$ref/$defs/type/properties/required/items/prefixItems/additionalProperties/propertyNames/anyOf/allOf/not/if/then/default/description）+ `x-cordis`{profile, complete, entries, diagnostics, patchSchema:'#/$defs/patchList'}；entry={path, id?, name?, status:'schema'|'partial'|'absent'|'unsupported'|'error', configRef?, tree?:'group'|'include'}；diagnostic={level:'warning'|'error', path?, message}。e2e（dump-config-schema.expected.e2e.ts）用 Ajv2020 验证输出合法性。
- native graph（types.ts:63-90）：schemastery 对象 = {type, meta{role,extra,hidden,disabled,collapse,link,comment,badges,required,volatile,default,min,max,step,pattern{source,flags},description,loose}, dict?, inner?, sKey?, list?, value?, builder?}；探测 `Symbol.for('schemastery')`（native.ts:13）。
- 机制规模：collect 249 / document 169 / projector 455（LOADER_EXPRESSION_SCHEMA：`!!js` 表达式投影为 schema）/ pattern 62 / native 16 / index 36 / types 95 ≈ 1130 行；app-boot 导出 generateConfigSchema / createConfigProjector / isNativeConfigSchema（index.ts:33-36）。
- 插件声明模式：`static Config: z<Config> = z.object({...})`（plugin-manager/src/index.ts:209）；plugin-manager 与 vendor/include 无显式 `.validate(` —— 容器侧消费机制未核，留 Phase 13 插件 crate 动工时补核（不影响本轮框架分叉）。
- fr 挂接点：`compile_to_loader_doc`（fr-config patch.rs:413-451）resolve → 信任边界 → [schema 校验槽待插] → `CompileFailure{id,name,reason}`。
- 备忘：fr dump 命令实现期 `x-cordis` 品牌前缀需另裁（fr 侧 x- 前缀命名），非本轮分叉。

待裁决 Q7-Q10（schema 表示法 / schema 库载体 / 注册表与查无 schema 语义 / M1 验证载体）已按集中格式呈报，见会话记录；裁决后动作序列：PRD 6.3 勘误（若 Q7A）→ intake CONFIG-3 条款核对/补场景 → 管线复跑三门禁 → fr-schema 实现 + 四关。

### Q7-Q10 裁决结果与落地记录（2026-10-22，Session 7）

- 用户裁决 Q7A/Q8A/Q9A/Q10A：描述树单一真源 / fr-schema 独立 crate / 查无 schema 跳过（absent）/ 框架先行自造钉测。
- PRD 落地 3 处：6.3 重写（fr-schema 描述树真源 + 链接副本双侧校验 + absent 语义 + dump 产物 2020-12 文档、x- 前缀留宿主实现期）；19.2 条 9；23 决策 20（插入时一度与 Q6A 的条 19 重号，已修复为 19/20 顺排）。
- intake CONFIG-3 条款重写 + Source Trace 6.3 引文同步 + 3 新场景（查无跳过 / 单源派生双能力 / 激活期再校验），场景 1212→1215；管线复跑全绿：accepted 20/20、三门禁 0 errors。
- 实现：新 crate `crates/fr-schema`（Cargo.toml + lib.rs + schema.rs + registry.rs）：`Schema` 描述符七变体（String{candidates,default,description}/Integer/Number（含 min/max）/Boolean/Object{properties,additional_properties,description}/Array{items}/Dict{values}；子集外声明不可构造——fail-closed）、`Property`、`Violation{path,message}`（点路径 + `[i]` 数组、根为空串）、`validate`（全违例收集、additional_properties=false 拒未知键、必填缺失）、`to_json_schema`（2020-12 关键词面：type/enum/minimum/maximum/properties/required/items/additionalProperties/default/description）、registry `lookup`（PLUGIN_SCHEMAS 静态表，M1 为空表 + 空名永缺测试钉）。fr-schema tests 15 + 1。
- fr-config 挂接：`compile_to_loader_doc<'s>` 增第 4 参 `schema_of: &dyn Fn(&str) -> Option<&'s Schema>`（显式生命周期——省略形式在 trait object 返回位触发 HRTB 亚型检查失败 `one type is more general than the other`）；信任边界后校验，违例行 `format_config_violations`（`path (message)`、根 `<root>`）进 failures 不阻塞，查无 schema 跳过；新增钉测 schema_violation_fails_row_without_blocking / absent_schema_skips_validation。
- 排障（Session 7 工具面）：长 Rust 代码经 bash heredoc 内嵌 python 三引号或 Edit 大段 old/new 反复损坏（乱码 token 混入）——两次重做后改用「Write/小 heredoc 分块写片段文件（≤25 行/块）+ 极短 python 拼接」可靠完成；一次误插到 mod tests 闭括号外、一次 23 决策重号，均 git checkout patch.rs/stack.rs 后重做恢复。clippy 修 7 项：use_self（枚举内 Box<Self>）、needless_pass_by_value（path &str 化）、str_as_str、needless_borrow、assert_is_empty（assert_eq!(v, Vec::new())）、if_then_some_else_none（match 化）、E0560 测试字面量错位。
