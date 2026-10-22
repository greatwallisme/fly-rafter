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

## 2026-10-22 — 代码评审 b2f3ff0（Session 8，/code-review high --fix 不限数；后台代理三度因 GLM-5.3-FALSH API 400 失败，本会话执行，subagent 0/2）

基线核实：四关 exit 0（fmt / clippy --workspace --all-targets -D warnings / 42+15 tests / doc -D warnings）——Session 7 排障记录中「clippy 余 1 错」在提交前已解决；合约三门禁复跑全绿（lint 0 errors、advisory 1062 条、graph/plan exit 0，场景 1215）。dsh 引证补核通过：config-schema/types.ts:7-26（ConfigJsonSchemaObject 关键词面）与 :42（entry status 五态含 absent）。挂接点次序核实：patch.rs:426 disabled 过滤 → resolve → 信任边界 :438 → schema 槽 :447-457 → 产物；违例行 fail-closed 排除不阻塞其余行、absent 跳过，两钉测齐。

修复 2 项（fr-schema/src/schema.rs 单文件，+97/-57）：

- R1（中，语义漂移）：Integer 校验原用 as_i64()，零小数部分浮点（如 1.0）被拒——与自身投影的 JSON Schema 2020-12 关键词面矛盾（2020-12 明确零小数部分数值即 integer，dsh e2e 的 Ajv2020 亦然），Q7A「单一真源不漂移」在语义层被破坏；超大 u64 整数同理误拒。修：接受检测改 f64 域（as_f64 + fract()==0 判整），界宽经 const fn widen（#[expect clippy::cast_precision_loss reason=对齐 dsh double 运行时]）放宽后走 check_f64_range；新增钉测 integer_matches_json_schema_zero_fraction_semantics（1.0 / 2^63 u64 / 1.5 三点）。
- R2（低，构造不变量）：default: Option<Value> 无类型约束，声明层可构造「默认值违反自身声明」的描述符（如 candidates 外 string default），削弱提交信息宣称的「子集外/违例声明不可构造 fail-closed」；修：String/Integer/Number 的 default 分别改 Option<String>/Option<i64>/Option<f64>（Boolean 已是 Option<bool> 先例），投影经 map(Value::from)/map(Value::String)，违例默认值在类型层不可构造；3 处测试字面量同步，patch.rs fixture（default: None）不受影响。
- 连带 clippy 清零 4 项（新代码引入）：unreadable_literal（2^63 测试字面量加 _ 分隔）、Option→Option 包装 fn 形态（widen_bound 改值域 const fn widen i64→f64 + 调用点 .map(widen)）、too_many_lines（to_json_schema 107/100 → 提取 type_object / project_properties 两投影 helper）、missing_const_for_fn（widen 加 const）。

核实无缺口 2 项：激活期链接副本再校验场景（pending_req_fr_config_fail_closed）按决策 20 属插件 crate 落地期、pending 选择器合规非缺陷；patch.rs format_config_violations 的 let _ = write!（String fmt::Write 不可能失败）带注释合规。

终态：四关复跑 exit 0（fmt / clippy / 42+16 tests / doc -D warnings）；工作树 schema.rs 单文件改动待用户评审；合约三门禁不受影响（未触需求/合约文件）。

## 2026-10-22 — Phase 12 续：.env 分层 dsh 真源核实（Session 9）

基线：`55408f5`（Session 8 评审修复）已提交，工作树净。

dsh .env 分层权威事实（app-boot/src/index.ts + util 两包，仓库 ~/project/reference/cordis/deepseek-harness）：

- loadLayeredEnv（app-boot/src/index.ts:238-259）：home=resolveDshHome()；inherited={...process.env}；两文件先全部解析校验、后统一应用（:244 注释 "a rejection must not leave one file applied"——任一层违例两文件均不应用）；project=readEnvLayer(cwd)，user=(home===resolve(cwd))?undefined:readEnvLayer(home)——cwd==home 时家层不读（:246）；应用序 [project,user]，`process.env[name]===undefined` 才写（:251，不覆盖既有、project 先应用故同名 project 胜 user）；返回快照 [{process,inherited},{project-env,path,values},{user-env,path,values}]（:254-258）。
- readEnvLayer（index.ts:193-225）：读 resolve(dir)/.env；ENOENT 静默返回 undefined；其他读错误 warn 一行 stderr 后返回 undefined（非致命跳过层，:201-207）；parseEnv=node:util 内置 .env 解析（:11,:209）；bootstrap 检查大小写不敏感（toUpperCase :180,:211）；home 层代理四键豁免（:212-213）；违例 throw（消息含 remedy 文案 :214-222）→ 启动终止（bin.ts:41 StartupError 路径）。
- BOOTSTRAP_NAMES（index.ts:136-157，49 精确名）：PATH,HOMEDIR 组（PATH/HOME/USERPROFILE/SHELL）；NODE_OPTIONS/NODE_PATH/NODE_EXTRA_CA_CERTS；LD_PRELOAD/LD_LIBRARY_PATH/LD_AUDIT；解释器 hooks（BASH_ENV/ENV/SHELLOPTS/BASHOPTS/PERL5OPT/PERL5LIB/PYTHONSTARTUP/PYTHONPATH/RUBYOPT/RUBYLIB/JAVA_TOOL_OPTIONS/_JAVA_OPTIONS/JDK_JAVA_OPTIONS/PYTHONHOME）；VCS/编辑器组（GIT_SSH/GIT_SSH_COMMAND/GIT_EXTERNAL_DIFF/GIT_PAGER/GIT_EDITOR/GIT_ASKPASS/SSH_ASKPASS/GIT_CONFIG_GLOBAL/GIT_CONFIG_SYSTEM/GIT_CONFIG_COUNT/EDITOR/VISUAL/PAGER/BROWSER）；网络信任组（DEEPSEEK_BASE_URL/DEEPSEEK_SEARCH_BASE_URL/SSL_CERT_FILE/SSL_CERT_DIR/HTTP_PROXY/HTTPS_PROXY/ALL_PROXY/NO_PROXY/REQUESTS_CA_BUNDLE/CURL_CA_BUNDLE/NODE_TLS_REJECT_UNAUTHORIZED）。
- BOOTSTRAP_PREFIXES（index.ts:160）：DSH_、XDG_、DYLD_、BASH_FUNC_——无 NODE_ 前缀（NODE_* 概称是 PRD 侧失真）。
- HOME_LAYER_PROXY_NAMES（index.ts:169）：HTTP_PROXY/HTTPS_PROXY/ALL_PROXY/NO_PROXY（仅 home 层豁免，:163-168 注释阐明理由：随仓库 clone 而来的 invoking 目录文件不可信、home 文件是用户自有，且 DSH_HOME 本身 bootstrap-only 无法被 .env 搬走）。
- loadEnv 单层变体（index.ts:121-133，process.loadEnvFile、无 bootstrap 校验）：仅测试夹具使用（app-boot.spec.ts:45-90、loader-smoke headless-driver.ts:21）——产品路径全走 loadLayeredEnv；fr 不实现单层等价物（负向钉）。
- 快照包 util/launch-environment/src/index.ts（137 行）：get/getFrom 按信任序 process>project-env>user-env（SOURCE_ORDER :22）；win32 名折叠大写、POSIX 精确（lookupKey :66-69）；ctx 槽 launchEnvironment（:126-130，launcher 于任何 config entry mount 前填）+ launchEnvironmentOf（无槽回退 {process,process.env}）；launchedThroughSsh（:120-125：SSH_CONNECTION/SSH_TTY 仅 process 层、非空即真——"Project and user .env values never establish an SSH session"）；README：consumers resolve through it instead of a flattened process.env; launchers may still materialize accepted values——**dsh 双机制：快照（consumer 权威）+ materialize 进 process.env（config 表达式/三方库可见）**。
- 快照在 app-boot src 内无消费（profile-boot.ts 无 launchEnvironment 命中）——消费方属后续里程碑（core ctx 槽/SSH 检测）。
- resolveDshHome（home-paths/src/index.ts:115-121 + resolveRoot :76-97）：configured > $DSH_HOME（trim 后空串=未设）> join(homedir(),'.dsh')；expandHomePath（'~'/'~/'/'~\'，:59-64）；resolve 归一。品牌映射（PRD:178）：DSH_HOME→FR_HOME、.dsh→.fly-rafter。
- 调用点：apps/cli/src/bin.ts:36（profile 启动 environment: loadLayeredEnv('dsh')，先于 runProfile）、desktop-host:26、webworker worker-host:358/436。
- .env 解析语法 = node:util parseEnv（Node 内置 .env 方言）；fr Rust 侧载体未定。

与 PRD/intake 的差异（勘误/缺口候选）：
- G1 PRD:263「NODE_* 前缀」概称失真：dsh 无 NODE_ 前缀，仅 4 个精确 NODE_ 名；PRD 概称比 dsh 宽（会多拒任意 NODE_FOO）。
- G2 PRD 前缀清单（FR_/XDG_）缺 DYLD_、BASH_FUNC_（非品牌前缀，应逐字搬运）。
- G3 PRD 专名仅示例（PATH/代理名/DEEPSEEK_BASE_URL）；dsh 49 名全表须入规格；DEEPSEEK_BASE_URL/DEEPSEEK_SEARCH_BASE_URL 是否品牌化未显式裁决（intake 现文保留原名）。
- G4 拒绝语义未钉：违例=throw 启动终止；两文件先全解析校验再统一应用（原子）；读错误非 ENOENT=warn 一行+层跳过非致命；ENOENT 静默。
- G5 专名检查大小写不敏感未记（.env 设 path= 也拒）。
- G6 cwd==home 家层不读（去重）未记。
- G7 .env 解析语法=Node util.parseEnv；Rust 载体未定。
- G8 应用机制=dsh 双机制（快照+materialize）；fr cond.rs:60 现直读 std::env::var；Rust edition 2024 中 env::set_var 为 unsafe。
- fr 侧现状：stack.rs load_stack(home:&Path,...) home 参数化；FR_HOME 解析未实现（PRD:178 品牌行已有）。

## 待裁决问题登记（2026-10-22 呈 Q11-Q14）

### Q11 bootstrap 名单口径（G1/G2/G3）— 待裁决
- 事实：dsh = 49 精确名（index.ts:136-157）+ 4 前缀（DSH_/XDG_/DYLD_/BASH_FUNC_ :160），无 NODE_ 前缀；检查大小写不敏感。
- A（推荐）：49 名逐字搬运 + 前缀 FR_（DSH_ 品牌映射）/XDG_/DYLD_/BASH_FUNC_；DEEPSEEK_BASE_URL 与 DEEPSEEK_SEARCH_BASE_URL 保留原名（provider 名 deepseek-official 未变，1:1）；PRD 6.4 勘误（删「NODE_*」概称、补全表与四前缀）+ intake CONFIG-13 同步
- B：按 PRD 现文字面（NODE_ 前缀 + FR_/XDG_ + 示例专名）——宽严双向偏离 dsh

### Q12 失败与边界语义（G4/G5/G6）— 待裁决
- 事实：dsh 违例=该层解析即 throw→启动终止（bin.ts:41 StartupError）；两文件先全解析校验再统一应用（原子）；读错误非 ENOENT=stderr 一行 warn+该层跳过非致命；ENOENT 静默；大小写不敏感检查；cwd==home 家层不读。
- A（推荐）：五点全对齐 dsh；PRD 6.4 补语义段 + intake CONFIG-13 扩充 + 场景 +N
- B：违例仅跳过该变量不终止启动（偏离 dsh fail-closed）

### Q13 应用机制与快照（G8）— 待裁决
- 事实：dsh 双机制=materialize 进 process.env（:249-253，!!js 表达式与三方库依赖此）+ 快照返回（consumer 权威，M1 无消费方）；fr cond.rs:60 直读 std::env::var；edition 2024 env::set_var 为 unsafe。
- A：双机制全对齐——materialize（unsafe set_var，启动期单线程、任何线程/异步运行时启动前，Safety 注释论证）+ LaunchEnvironmentSnapshot 等价物随 fr-config 交付（消费方后置）
- B（推荐）：M1 仅 materialize（unsafe set_var 同上论证）；快照等价物后置到出现消费方的里程碑（避免无消费方代码；结构小，Phase 18 宿主接线时按需补）
- C：仅快照不 materialize，cond.rs 改经快照查——未来任意 std::env::var 调用点与子进程 environ 漏读，偏离 dsh materialize 理由

### Q14 .env 解析器载体（G7）— 待裁决
- 事实：dsh 用 node:util parseEnv（Node 内置 .env 方言：# 注释、单/双引号、双引号内转义与多行值等）。
- A（推荐）：fr-config 自实现语义对齐解析器，实现期以 Node 官方文档+源码为钉测蓝本逐条钉
- B：dotenvy crate——含变量扩展等 Node 无语义，禁用后仍有漂移风险，与 1:1 冲突需逐项核对

### 推定项（非分叉，可否决）
- D0：fr-config 增 resolve_home(configured: Option<&Path>) -> PathBuf，对齐 home-paths resolveRoot 语义（FR_HOME trim 空白=未设 > ~/.fly-rafter，~ 展开，resolve 归一）；依赖方向规则（领域 crate 不得依赖入口 crate）使归属唯一，stack.rs 现有 home 参数由其喂。

### 裁决后动作序列
1. PRD 6.4 .env 段勘误/扩充（按 Q11A/Q12A）+ 19.2 条 + 23 决策；intake CONFIG-13 条款扩充 + 场景补齐（三层序/不覆盖/拒绝+例外已有 3 场景，补原子性/读错误跳过/大小写/cwd==home 去重/全表拒绝族）
2. 全管线复跑三门禁（import→accepted×20→lint/graph/plan --gate）
3. fr-config 实现：resolve_home + env.rs（parse 层 + bootstrap 校验 + materialize）+ 钉测；四关全绿

### Q11-Q14 裁决结果与落地记录（2026-10-22，Session 9 续）

- 用户裁决 Q11A/Q12A/Q13A/Q14A（D0 未否决按推定成立）：bootstrap 名单 = dsh 49 精确名逐字（DEEPSEEK_* 保留原名）+ FR_/XDG_/DYLD_/BASH_FUNC_ 四前缀；失败语义五点全对齐 dsh；应用 = materialize + 快照双轨全对齐；解析器 = fr-config 自实现对齐 Node util.parseEnv。
- 计数自纠：bootstrap 名单实为 49 名（原记 46 系点数失误——REQUESTS_CA_BUNDLE/CURL_CA_BUNDLE/NODE_TLS_REJECT_UNAUTHORIZED 在 NO_PROXY 之后），PRD/intake/findings/progress 已全部改正并以 dsh 源 rg -o 实证 49。
- PRD 落地 3 处：6.4 .env 段重写（层序/原子性/双轨/49 名全表/代理例外/失败语义/解析语法/D0 home 解析）；19.2 条 10；23 决策 21。intake CONFIG-13 条款重写 + 6 新场景（原子性/读错误跳层/大小写/cwd==home 去重/双轨快照/解析方言），场景 1215→1221；管线复跑全绿（accepted 20/20、lint 0 errors、graph/plan --gate exit 0、questions 0 blocking）。
- 语法钉死：parseEnv 实现为 Node C++ Dotenv::ParseContent（src/node_dotenv.cc，v22.x；main 分支无此函数、C++ 才是真源）。关键语义：\r 全局剥除；双引号快路径捕获到剩余内容最后一个闭引号（可跨真实换行，node v24.22 实证 {"A":"value\nB=2\nC="}）；未闭引号取行含引号符；未加引号值遇 # 即切（无需空格）；export 前缀（含多空格）剥离；重复键后者胜；\n 展开重扫 pos+=1 怪癖（\\n → 反斜杠+真实换行）。
- 实现落地：crates/fr-config/src/env.rs（496→约 920 行含 19 tests；parse_env 全语义移植 + read_env_layer fail-closed + EnvSnapshot get/get_from（win32 折叠）+ load_layered_env_into 原子两文件先校验后应用 + resolve_home/resolve_home_from（FR_HOME 空白未设 > ~/.fly-rafter，~ 展开，std::path::absolute）+ 官方 valid.env fixture 逐字节入 tests/fixtures 并按 node 官方测试全量断言）；error.rs 增 EnvBootstrap(String)；patch.rs err_message 补臂。
- 关键结构决定（呈用户知悉）：fr-config lib.rs 既有 #![forbid(unsafe_code)]，而 Rust 2024 set_var 为 unsafe——未放宽 forbid（质量红线），以 EnvSink trait 承载 materialize 接缝；含 unsafe 的 ProcessEnv 适配器（3 行）落 Phase 18 宿主 crate 启动最先调用。Q13A 的双轨机制（materialize 语义 + 快照结构）已全部交付于 fr-config 侧。
- 排障：EXPECTED 数组 46 长度误记（实 45，node checkObj 45 键）编译期逮住；两处测试期望错（双引号快路径跨行捕获、get_from 层可达性）经 node v24.22 实证后改测试而非改实现；clippy 12 项清零（too_long_first_doc_paragraph×4/doc_markdown×2/option_if_let_else×2/collapsible_if×2/range_plus_one×2/single_match_else×2 中的实际清单）。

## 待裁决问题登记（2026-10-22 呈 Q18-Q20：dump-config / dump-config-schema 数据面）

### 事实（dsh 真源，file:line 实证，2026-10-22 本轮核实）
- CLI 旗标（apps/cli/src/args.ts:168-173,111-136）：--profile / --from-default-profile <name>（:169，出厂模板替换自定义 profile）/ --patch（可重复）/ --dump-config / --dump-config-schema / --dump-default-config 三 dump 旗标互斥（:120）；dumps 拒收 app 位置参数（:125-127）；--dump-default-config 禁 --patch（:132-134）。
- runDumpConfig（apps/cli/src/dump-config.ts:32-42）：prepareProfile(profile, !defaultOnly, fromDefaultProfile) → collectConfigDumpLayers → renderConfigDump('dsh', profile 根文件, layers) 写 stdout；defaultOnly = 省略 profile 用户层与 overlays 且**从不解析** profile patch 文件（:26-28，坏 cordis.patch.yml 的恢复诊断）。
- collectConfigDumpLayers（dump-config.ts:51-75）：bundle 层（label=packageName）→ profile patch（label=路径，existsSync 才入）→ 家 patch（loadOptionalPatches）→ 各 --patch overlay（label=绝对路径，loadOverlayPatches）；defaultOnly 仅 bundle 层。schema dump 复用同一收集器（dump-config-schema.ts:8）。
- renderConfigDump（packages/boot/app-boot/src/index.ts:635-）：读+解析 profile 根文件为 base（错误三连 throw：`dsh: failed to read config` / `failed to parse config` / `must be a top-level YAML array of entries`）；快照算法 = 层前缀 1..k 各做**单次** applyEntryPatches（structuredClone 展平，与 boot 完全同参；:661-675 注释钉死单次调用语义——分层调用会让 group config 替换的子行被后续层命中，boot 不会）；输出 = YAML 文本，`# == <源文件>[, patched by <层>…]` 分隔注释按连续同 (origin, patchers) 行分组；`!!js` 表达式**逐字打印不求值**（spec: `dump toContain '!!js process.env.DSH_DUMP_SPEC'`）；整篇可再 parse 且等于 boot 挂载结果；插入行 origin = 插入层。规格测试 packages/boot/app-boot/tests/config-dump.spec.ts:47-140 逐字钉死注释形状。
- loadOverlayPatches（app-boot/src/index.ts:540）/ loadOptionalPatches / ConfigDumpLayer{label, patches}（:596-599）。
- dump-config-schema 侧：Session 7 已核（ConfigSchemaDump types.ts:49-60、x-cordis、entry 五态 types.ts:42、diagnostics stderr `dsh: <level>:[<path>] <message>`、!x-cordis.complete → exit 1、stdout 单份 JSON.stringify(dump, null, 2)；机制规模 collect 249/document 169/projector 455 等 ≈1130 行）。
- PRD 现状：6.3（PRD:246）钉 fr dump-config 输出合成后插件树 + 「x- 扩展键品牌前缀于 fr 宿主实现期定」；6.4（PRD:261）默认装配树 90 行插件 id 集合为同型 diff 锚点；M1 验收 5 = 两命令可用。PRD/intake 未载 --dump-default-config 与 --from-default-profile 等价物（rg 实证零命中）——两能力为真源新发现。

### Q18 x- 扩展键品牌前缀（dump-config-schema 输出，PRD:246 明示本轮定）
- A（推荐）：`x-fr`——与 FR_ 环境前缀、命令名 fr、产品短名一致
- B：`x-fly-rafter`——全名 kebab
- C：保留 `x-cordis`——该键是 dsh 自有 CLI 输出扩展、非协议 wire，保留他品牌与命名硬约束冲突

### Q19 --dump-default-config 等价物（恢复诊断，真源新发现）
- A（推荐）：纳入 M1——数据面 default_only 分支 + `fr dump-config --default-only`（形态随 fr 子命令风格 kebab）；PRD 6.3 + intake CONFIG-5 补条款场景
- B：M1 排除，PRD 19.2 登记偏差（1:1 缺口）

### Q20 --from-default-profile 等价物（出厂模板替换，真源新发现）
- 事实：launcher 全局旗标，dump 与 boot 共用；语义 = 出厂模板**替换**自定义 profile（区别于已实现的首用初始化绝不覆盖）。
- A（推荐）：纳入规格——Phase 18 宿主 CLI flag 表 + 本期数据面 API 预留 from_default_profile 参数；PRD 18/6.3 + intake 补条款
- B：排除，19.2 登记

### 推定项（非分叉，可否决）
- D1：品牌 NAME 'dsh'→'fr'（错误前缀 `fr: failed to read config …`）；PRD 品牌映射既定原则覆盖
- D2：dump 输出 = YAML 带注释逐字节同型（# == 分隔、!!js 逐字、前缀快照单次 applyEntryPatches 算法、整篇可 parse）——PRD「同型 diff 锚点」直接落实
- D3：dump-config-schema 输出形状以 Session 7 真源为准，x- 键按 Q18 裁决

### 裁决后动作序列
1. PRD 6.3 勘误（三 dump 态、输出形状逐字、x- 定名）+ 6.4/18 按 Q19/Q20 + 19.2/23 决策；intake CONFIG-5 扩条款 + 场景；管线复跑三门禁
2. fr-config 数据面：collect_dump_layers + render_dump（快照算法 + 分隔注释 + !!js 不求值变体 + prepare 语义）；fr-schema 投影面（-schema 侧）
3. 四关 + lifecycle 基线（35 passed）上新增绑定 + 呈报
- 终态：四关全绿（fmt / clippy --workspace --all-targets -D warnings / 61+16 tests / doc -D warnings）；工作树待用户评审提交；Next Step → dump-config / dump-config-schema 数据面。

## 代码评审 round（commit 7f8948c，/code-review high --fix，2026-10-22）
- 5 findings（crates/fr-config/src/env.rs，全数 1:1 dsh 偏差或注释失真）；4 项已修复并提交 **324c8a3**，1 项留裁决（Q21）。
- 已修 4 项：① FR_HOME 非空值原样直用、trim 仅判空（dsh resolveRoot，home-paths index.ts:102-109；原实现连带测试把偏差行为钉死，已一并勘正，期望改为 std::path::absolute(原值)——前导空格按 Node resolve 语义变 cwd 相对）；② EnvSnapshot 新增私有 from_layers 构造器，三层存储键全部经 lookup_key 折叠（dsh 存储期折叠，launch-environment index.ts:60-100；Unix 恒等无行为变化，Windows 分叉消除）；③ 非 ENOENT 读失败告警内嵌失败文件路径（对齐 dsh `${String(error)}` 含路径）；④ 双引号快路径注释勘误为「首个闭引号即止、余文至行尾跳过」（原注释称 LAST closing quote，与代码及 Node 行为均不符，差分 harness 9073 输入 = 73 定向 + 9000 fuzz 对 Node v24.22 util.parseEnv 零语义分歧，harness 用后即删）。
- 修复后独立复跑验证：四关全绿（fmt / clippy --workspace --all-targets -D warnings / 61+16 tests / doc）；CONFIG lifecycle 复跑 = 35 passed / 266 skipped / 0 failed / 0 uncertain（与修前基线一致，exit 1 属预期）；stamp --dry-run Spec-Passing: false（266 未绑，无可盖 trailer，与既有提交同）。
- Q21（finding 5 未修，留裁决）：bootstrap 违规名校验遍历 BTreeMap 字典序键——.env 同时含多个违规名时 fatal 诊断点名「字典序首个」，dsh（Object.keys 插入序）点名「文件内首个」；同为拒绝启动，仅诊断文本变量名不同。5A 保持现状（重构 parse_env 保文件序涉及公共返回类型语义与已绑定场景回归，收益仅诊断文本顺序）；5B 重构 parse_env 保留文件序并对齐点名顺序（需回归 .env 分层 9 绑定场景）。

### Q21 裁决落地（2026-10-22 用户裁决 5B，提交见 git log）
- parse_env 重构保序：解析核心 `parse_env_entries` 产出文件序 `Vec<(String, String)>`（重复键保留首现位置，collect 成 map 即 last-wins 视图），公共 `parse_env -> BTreeMap` 包裹零变化（差分 harness 钉死的 9,073 输入行为面不动）；`read_env_layer` 迭代文件序 entries 点名首个违规名，对齐 dsh `readEnvLayer` 的 `Object.keys` 插入序（app-boot index.ts:213-231，单遍逐键 exact+prefix 一次判定）。
- 新增钉测 `bootstrap_violation_names_first_in_file_order`：`VISUAL=vim`+`PATH=/evil` 双向文件序各一例——字典序两种都点名 PATH，唯文件序才能双向各自点名，钉死语义。
- 验证：四关全绿（62+16 tests，fmt 自动归一后 --check 过）；CONFIG lifecycle 35 passed / 266 skipped / 0 failed 与基线一致；合同零改动（spec 无顺序钉死文本，rg 实证）。

### Q22（新发现，Windows 折叠碰撞覆盖序分叉，待裁决）
- 事实：dsh `createLaunchEnvironmentSnapshot`（launch-environment index.ts:85-93）以 `Object.entries`（文件序）构造折叠 Map——同层大小写变体碰撞时**文件后者胜**；fr `EnvSnapshot::from_layers`（env.rs:346-）折叠 BTreeMap（字典序），碰撞时**字典序后者胜**（ASCII `PATH` < `Path`，反而文件前者存活）。例：家层 `.env` `Path=a`+`PATH=b`，Windows 下 `snapshot.get("PATH")`：dsh 返回 b，fr 返回 a。
- 触发条件三重叠加：Windows（`lookup_key` 仅 win32 折叠，dsh `lookupKey` 同构 index.ts:63-66）+ 同层内 + 同折叠键不同原写法。Unix 恒等无碰撞，快照其余行为与 finding 2 修复后完全一致。
- 修复代价：`EnvLayer.values` 现为 `BTreeMap`（文件序在存储时已丢），保序需层存储改序敏感容器（indexmap 依赖或 Vec 双轨），波及 `EnvLayer` 公共字段与快照构造链。
- 选项：22A 修（层存储保序，对齐 dsh 文件序覆盖语义）；22B 不修，PRD 19.2 登记已知偏差。
