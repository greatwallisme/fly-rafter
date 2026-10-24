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

### Q22 裁决落地（2026-10-22 用户裁决 22A1 indexmap 路线）
- `EnvLayer.values` 与 `EnvSnapshot.process` 由 BTreeMap 改 `indexmap::IndexMap`——`FromIterator` 重复键语义 = JS `Map`（首现位保留、末值胜），与 dsh `new Map(Object.entries(...))`（launch-environment src/index.ts:85）逐语义同构；折叠提取为私有 `fold_keys(values, fold)` 注入式助手，碰撞路径在任意平台真实执行（`lookup_key` win32 臂仅原生 Windows 构建覆盖，与 dsh `v8 ignore` 同覆盖口径，代码注释已注明）。
- 新增 3 钉测：`layer_keeps_file_order_and_last_duplicate_value`（层文件序 + 重复键首现位末值胜）、`fold_keys_case_collision_keeps_file_later_value_at_first_position`（Path/PATH 大写折叠碰撞文件后者胜 + 恒等折叠保序双向）、`snapshot_layer_case_variants_stay_distinct_off_windows`（非 Windows 恒等不碰撞）。`parse_env` 公共面保持 BTreeMap 零变化（9073 差分输入钉死的行为面不动）。
- 验证：四关全绿（fmt / clippy --workspace --all-targets -D warnings / fr-config 65 + fr-schema 16 tests / doc 零警告）；CONFIG lifecycle 35 passed / 266 skipped / 0 failed 与基线一致；guard exit 0；合同零改动（CONFIG 合约仅 patch 行「插入序」钉死文本，无 env 层存储顺序条目，rg 实证）。
- 类型泄漏裁定（M-DONT-LEAK-TYPES）：全仓消费面核实 `EnvLayer`/`EnvSnapshot` 仅 fr-config 内部使用 + lib.rs 再导出（fr 宿主 crate 未建），工作区内 crate 顺序保留即 22A1 裁决语义本体，`pub values: IndexMap` 字段文档已注明依据——接受已声明泄漏。

## 2026-10-22 — dump 数据面真源全读（fr-config 切片动工前，会话恢复后）

### 事实（file:line 实证，本会话核实）
1. 根文件：`PROFILE_ROOT_FILENAME='cordis.yml'`（apps/cli/src/profile-boot.ts:88）；`PROFILE_ROOT_CONFIG` = 3 行注释头 + `[]`（"Edit cordis.patch.yml, not this file"，profile-boot.ts:88-93 邻域）；`prepareProfile`（profile-boot.ts:194-201）**每次装载无条件重写**该文件，boot 与 dump 共用；文件存在理由 = cordis Loader 需真实 include 根锚定 baseUrl + dump 同锚组合（:194-199 docstring），兼防 Loader 树写回把组合行烘进文件——fr dougong loader 吃整份 JSON、无 include 根、reconcile 不写回配置文件，两项理由在 fr 均不存在。
2. 错误三连落点 = `renderConfigDump` 读根文件三段 throw（app-boot index.ts:635-）；`runDumpConfig` 传 `join(loaded.dir, PROFILE_ROOT_FILENAME)`（dump-config.ts:41）。
3. `prepareProfile(name, userLayer=!defaultOnly, fromDefaultProfile)`（profile-boot.ts:194-201）：有 fromDefaultProfile 先替换；`userLayer=false` 跳过 cordis.patch.yml 读与解析（default-only 的"从不解析"）。
4. `initializeProfileFromDefault`（profile-boot.ts:102-165）：模板未知名 throw（expected 按序）；目标名撞 shipped 名（大小写不敏感）throw "names shipped profile"；staging = `mkdtemp('.<name>.staging-')` 于 profiles 目录旁（同一文件系统）→ `initProfile(staging/new/<name>)` → `rename(dir→staging/previous)`（ENOENT 容忍）→ `rename(fresh→dir)`；并发撞车 = 目标已存在（Windows EPERM 口径按 existsSync 判）→ 删 previous + throw "recreated concurrently"；restore 失败 → AggregateError 指名 staging 路径；收尾 staging 清不掉仅 stderr 警告（替换已完成）。
5. `applyEntryPatches`（vendor/include/src/index.ts:57-127）全语义：patches 空 → `[...data]`；否则 `structuredClone(data)`；索引 id→entry **递归进 group 条目的数组 config**（:70-77）；insert 带 id → 目标须存在且 group（警告 `patch insert: entry %C not found` / `patch insert: entry %C is not a group`），target.config 数组 push；insert 无 id → 根级 push；**每次 insert 后 buildMap(insert)**（同列表后补丁可命中先插行，:113-118）；无 id 非插入行 → 警告 `patch: id is required for non-insert patches`；未命中 → `patch: entry %C not found`；name 不匹配 → `patch: name mismatch for %C (expected %C, got %C), skipping`；其余键逐键覆盖（id 键跳过）。`%C` = `JSON.stringify(参数)`（renderConfigDump warn 内联替换，index.ts:671-676）。dump 警告输出格式：`<bin>: [<层label>] <替换后文本>`。
6. fr compose() 既有已裁差异（Q1/Q2，boot 路径不动）：无 group、insert 不带 id、id 缺失解析期拒绝、警告文案 fr 变体（`patch: entry {id} not found, skipped` 等）。
7. config-dump.spec.ts 6 测试：①分层组合+!!js 逐字+注释标注（:48-101，`# == base.yml, patched by surface.yml`、插入行 origin=插入层、注释序断言）②连续同源行单注释（:103-116）③单次展平调用 = group 子行不被后续层命中 + 层标签警告（:118-150）④未命中 warn 带层标签继续组合（:152-174）⑤warn 默认 stderr 逐行（:176-186）⑥根文件缺失/坏 YAML/非数组三连 throw（:188-200）。
8. 全库 rg 实证零命中：cordis.yml/根文件/fr.yml 于 PRD/intake/KLL/spec——根文件从未被裁决（Q1-Q5 布局 = profile.yml + fr.patch.yml，PRD 6.1）。

### 新分叉登记（呈 Q23/Q24，裁决记录待补）
- Q23 dump base 锚点与错误三连落点（23A 增根文件 fr.yml / 23B 内存空 base + 死文本勘误）——见会话消息。
- Q24 dump 离线工具口径（24A dsh structural-trust 逐字移植 / 24B 受限于 fr 裁定格式）——见会话消息。

### Q23/Q24 裁决落地（2026-10-22 用户裁决 23A + 24A）
- Q23A：profile 布局增根文件 `fr.yml`（cordis.yml 品牌映射），load/prepare 每次装载无条件重写（boot+dump 共用同参 base），dump 读它为 base、错误三连真实成立；PRD 6.1 布局勘误 + 23 决策 23。
- Q24A：dump 离线工具 = dsh structural-trust 逐字移植——层文件 shape-only 解析（任意键保留、!!js raw）+ raw compose 完整移植 applyEntryPatches（group 递归索引、insert 带 id、五条 dsh 警告文案原样、%C=JSON.stringify）；boot 严格解析不动。

### dump 数据面切片落地（2026-10-22，Q23A/Q24A 执行）
- PRD：6.1 装载文件布局补 fr.yml 根文件（每次装载无条件重写，boot 与 dump 共用同参 base）；23 决策 23（Q23A/Q24A 全文）。
- crates/fr-config 新增 src/dump.rs（Q24A dsh structural-trust 逐字移植）：DumpLayer/collect_dump_layers（entry bundle 恒为首层 + manifest bundle 层 + profile patch(存在才入) + home patch + overlays 绝对路径 label；default_only 仅 bundle 层不读用户文件）+ render_dump（层前缀各单次 apply_entry_patches 展平调用、# == 连续同源分组注释、!!js 逐字（denormalize_js_tags 把 serde_yaml_ng 的 !js 反规范化回 !!js 值位）、canonical_form 变更检测（Tagged 显式标注）、警告新尾段带层标签）+ apply_entry_patches 全语义移植（group 递归索引、insert 带 id 指向 group、五条 dsh 警告文案原样、name 缺失渲染 "undefined"、%C=JSON 式引述）+ parse_raw_patch_list shape-only（任意键保留、!!js raw、flow 式 tag 仍 fail-closed）+ raw insert 名锚定（Q6 同规则）。
- stack.rs：ROOT_FILENAME=fr.yml + ROOT_CONFIG 模板（品牌中文注释头 + []，Q23A）；load_profile 增 user_layer 参数（false = 跳过 fr.patch.yml 读与解析）；prepare_profile（from_default 替换 → load → 无条件重写根文件）；initialize_profile_from_default（staging rename 舞步：mkdtemp 式 .name.staging-* 旁目录、ENOENT 容忍、并发撞车 existsSync 判定、restore 失败聚合报错、清理失败仅 warn——dsh profile-boot.ts:102-165 逐字；shipped 名大小写不敏感保留，SHIPPED_PROFILE_NAMES=["headless"]）；load_stack 增 from_default_profile + warn 参数（Some(profile) 走 prepare）；EntryKind::bundle_name()；LoadedProfile.bundle_names（dump 重解析 raw 用，boot 级解析已求值不可复用）。
- json_quote（JSON.stringify 式引述，手写避 unwrap）pub(crate) 于 stack.rs，dump 复用。
- 绑定 +3：注释分层同型 → composes_layers_prints_verbatim_js_and_labels_sections；default-only 不读用户层 → default_only_collects_bundle_layers_only_and_skips_broken_user_files；from-default-profile 替换 → from_default_profile_replaces_existing_profile。bind-config-tests.py 补已绑定态幂等（current==new 即跳过）——原实现只在重生成态可重放，增量运行会误报 expected 不匹配。
- 新测试 13：dump 侧 10（dsh config-dump.spec.ts 六测对应移植 + boot-mount 等价锚点（dump 逐行 FlatRow == compose(stack.layers).rows）+ default-only + 层序 label + raw shape + tag override 往返）+ stack 侧 3（prepare 重写根/user_layer 门 + from-default 替换 + unknown/shipped 拒绝）。
- 测试自纠四件（首轮 6 败）：①夹具把整段 Sequence 当单条 patch（raw_rows 助手）②js_tagged 测试 base 误写为 patch list（insert 组）而非 entry list——build_map 不递归 insert 键，覆盖行永不命中③boot-mount 等价断言误用 compose()（提取行是 entry 非 override，compose 全数 warn-skip）→ 改逐行 FlatRow 对比④layer_order 测试传 cwd 相对路径；另错误文案断言 starts_with → contains（fr Error 变体带自身前缀）；!!js 引号形状 = serde_yaml_ng 单引号 '' 转义，与 js-yaml 对同内容的输出逐字节同型，钉死之。
- 门禁终态：四关全绿（fmt / clippy --workspace --all-targets -D warnings / fr-config 78 + fr-schema 16 / doc -D warnings）；CONFIG lint Quality 100% exit 0；lifecycle = 38 passed / 263 skipped / 0 failed / 0 uncertain（exit 1 预期）；guard exit 0；stamp dry-run 38/301 passed（无可盖章 trailer，与既有一致）。
- 留 pending（本切片剩余）：fr-schema 投影面（-schema 侧 $defs/x-fr/entry 五态/diagnostics，Session 7 真源）→ 绑 x-fr 五键 + complete 假退 1 + 来自真实装配；CLI 层场景（三态互斥/拒收位置参数/default-only 禁 --patch/退出码）Phase 18。

## 2026-10-22 — /code-review high --fix（commit 8686626 dump 数据面切片）

### 评审结论（7 findings：6 修复提交 5753fc1，finding 7 呈 Q25）
1. dump.rs 悬挂引用保真（高危）：apply_entry_patches 原按位置 RowPath 解析 id，dsh 索引持对象引用——group 的 config 整体替换后，后续层补丁/警告命中占据旧位置的新行（dsh 则不可见改旧子行），合法 fr 文件上 dump≠boot 装载。修复：RowTarget::{Live, Detached}（dump.rs:426）+ detach_descendants（dump.rs:744，config 写回前快照冻结旧子树全部索引行；name/group 警告仍读冻结行、变更不可见，逐点复刻 dsh 引用语义）；apply_insert_patch/apply_override_patch 拆分 + PatchKeys 共享（dump.rs:533/604/650，消 too_many_lines/too_many_arguments，无 allow）。钉测 2：dump.rs:1314 / dump.rs:1370。
2. stack.rs staging 残留（高危）：initialize_profile_from_default 错误路径（init 失败/移走失败/并发撞车/恢复成功后）残留 .name.staging-* 旁目录，dsh 外层 catch 凡 previous 不在 staging 内即清理。修复：四路径补 remove_dir_all（stack.rs:506-553），仅 restore 失败路径保留（previous 仍在 staging 持原 profile，同 dsh）；错误路径不可移植测试，纯代码修复。
3. dump.rs bundle 锚定（中危，潜在）：collect_dump_layers 对内置 bundle 层 raw 解析带相对名锚定（anchor_base("headless") 落 cwd），boot 对内嵌常量保持名字面——含 ./ ../ 名的 bundle 上 dump≠boot。修复：parse_raw_patch_list 增 anchor_relative 参数（dump.rs:341），bundle 层传 false；钉测 dump.rs:1429。
4. stack.rs json_quote 转义差（低危）：手写逃逸对 \b（0x08）\f（0x0C）出 \u0008/\u000c 而非 dsh JSON.stringify（与 serde_json 同）短形式。修复：serde_json::to_string（&str 永不失败，stack.rs:438），删 std::fmt::Write 导入。
5. 正则每调用重编译（效率）：denormalize_js_tags（dump.rs:276 每组 flush）与 normalize_js_tags（patch.rs:278 每文件解析，本 commit 触及）每次 Regex::new。修复：OnceLock（patch.rs:273、dump.rs:269）。
6. SHIPPED_PROFILE_NAMES 文档承诺无背书（低危）：doc 注释称"单测断言同步"但测试不存在。修复：新增 shipped_profile_names_stay_in_sync_with_templates（stack.rs:1114）。
7. SKIPPED——块标量 !!js 文本翻转，呈 Q25（下节）。

### Q25（新发现，行锚定 !!js 归一化无 YAML 结构感知，待裁决）
- 事实（本会话核实）：normalize_js_tags / denormalize_js_tags 为行锚定正则（patch.rs:278、dump.rs:276），无 YAML 结构感知——字符串值的字面文本含形如 `k: !!js v` 的行（典型：块标量 `note: |` 内容行）会在解析期被翻为 `!js`（boot 装载进来的文本即已被改），dump 输出再被 denormalize 翻回 `!!js` → 对该字符串 dump ≠ boot。
- 评审侧记录：input 侧行为先于 8686626 存在（patch.rs 归一化非本切片新增）；根修 = YAML 结构感知归一化（按解析树只对真值位翻 tag），超出本 diff 范围。
- 待裁决：25A 现状接受 + 登记 PRD 19.2 偏差（偏差治理规则路径）；25B 立即实现结构感知归一化（独立切片，按七步合约流先精化合同再动工）；25C 后置排期（记入下一步，fr-schema 投影面之后处理）。

### 门禁复验（本会话独立复跑，非转述子代理）
- cargo fmt --check / cargo clippy --workspace --all-targets -- -D warnings / cargo doc 全绿；cargo test --workspace 82+16 passed / 0 failed。
- lifecycle 38 passed / 263 skipped / 0 failed（exit 1 = 263 pending 预期，基线不变）；guard exit 0。合约零改动。

## 2026-10-22 — Q25B 切片：!!js 方言结构感知归一化（dump==boot 对任意字符串）

### 事实（本会话探针实证，crates/fr-config/tests/yaml_tag_probe.rs 已删）
1. serde_yaml_ng 0.10 对 `!!` 二级句柄 tag **一律静默丢弃**：quoted/unquoted/flow/序列项皆然（`k: !!js "x"` → String("x")）；单叹号 `!js` 存活为 `Value::Tagged`。patch.rs 旧注释「quoted 丢弃」不全。
2. emitter 对全拼 tag 渲染长形式 `!tag:yaml.org,2002:js` 而非 `!!js` 短形式 → 输出侧 emitter 捷径不可行。
3. 块标量/引号多行内容含 tag 形文本 raw 解析逐字保留（内容真值可免费获得）。
4. 解析期 `Tag` 的 Display 含 `!` 前缀（`format!("{}", tag)` 出 `!fr-js-position-guard`），与 `Tag::new` 惯例不同——比较一律 `to_string().trim_start_matches('!')`。
5. PRD/intake 未钉死「行锚定」机制措辞（行为级钉死：!!js 逐字、flow fail-closed、dump==boot）→ 机制升级属实现内部。

### 设计（Q25B 裁决落地）
- 输入侧 = 结构感知差分解析（patch.rs:295 parse_js_dialect）：raw 解析为内容真值 + 翻转解析为标签位置真值（识别位正则不变，patch.rs:268 normalize_js_tags 降为内部助手）+ 剩余 `!!js` 换守卫 tag `!fr-js-position-guard`（patch.rs:259）检出识别位外结构值位 fail-closed（文案不变）+ restore_verbatim_strings（patch.rs:350）以 raw 为真值还原字符串内容（映射键序校验、标量恒等分支、(Tagged,Tagged)/(Tagged,标量) 双分支）。`ensure_no_flow_style_js` + REMAINING_JS_TAG 正则删除——守卫 tag 检出更精确（flow 式与异形键结构位同样拒绝，纯字符串内容不再误拒）。
- 输出侧 = flush_group 双序列化行差分（dump.rs:273 denormalize_group + :298 flip_tag_line + :317 strip_js_tags）：A=带 `!js` tag 序列化、B=剥 js tag 序列化，行数相同前提下行差异 ⟺ js 标签行，flip_tag_line 校验差异段恰为被剥 `!js`（含 tag 后导空格折叠）后增长为 `!!js`，内容行天然逐字；异常回退旧正则（denormalize_js_tags 保留为 fallback）。
- PRD：19.2 条 12（结构值位 fail-closed 范围，先于本条存在的行为定稿登记）+ 23 决策 24；intake 增 [REQ-FR-CONFIG-14] + Rule 五场景。

### 合同层（七步流 1-3）
- 管线全序列重跑：import → transition 20/20 accepted → draft-specs 20 → refine 20 → depends 清理 → 绑定重放+新增（38→43 对）→ 三门禁 0/0/0 → questions 39 条 0 blocking → CONFIG lint Quality 100%。
- 事故自纠两件：①首跑 `rg '\K'` 语法错误致 transition 全空跑 + specs/generated 已删后 draft 静默零产物（proposed 状态不产草稿）——以 sed 提取 id 重跑全序列恢复，HEAD 有已提交备份无损失；②rg `\K` 属 PCRE 语法，rg 默认引擎不支持，提取字段一律 sed -n 's///p'。
- 新增绑定 5：块标量逐字 / 引号多行逐字 / flow fail-closed（绑既有测试）/ 异形键 fail-closed / dump 内容行 round-trip。

### 实现与测试（七步流 4-7）
- 新测试 4：patch.rs:882 块标量逐字、:896 引号多行逐字、:910 异形键 fail-closed、dump.rs:1512 内容行 round-trip（全树等值断言 dump==boot）。绑定 5 场景 lifecycle 全 pass。
- 自纠四件（首轮 23 败 → 4 败 → 全绿）：①restore 兜底分支把相同标量对（Bool/Number/Null）误判失败——补标量恒等分支；②解析期 Tag Display 含 `!`，守卫检测与 strip 比较失配——trim 后比较；③merge 缺 (Tagged,Tagged) 分支，单叹号直写文档误判键分歧——补分支；④flip_tag_line 差一空格（serde 剥 tag 连后导空格移除）——strip_prefix(' ') 折叠。
- clippy assigning_clones：restore 字符串改 clone_from；fmt 排版套用。
- 门禁终态：四关全绿（fmt / clippy -D warnings / 86+16 tests / doc）；lifecycle = 43 passed / 263 skipped / 0 failed（exit 1 = 263 pending 预期，绑定 38→43）；guard exit 0；explain 306 场景 43/0/263/0；stamp dry-run 与既有一致。合约改动 = intake CONFIG-14 + PRD 19.2 条 12/决策 24 + 5 新绑定，均经 Q25B 裁决授权。
- 下一切片（不变）：fr-schema 投影面（dump-config-schema 侧 $defs/x-fr/entry 五态/diagnostics，D3=Session 7 真源）→ 绑 x-fr 五键/complete 假退 1/来自真实装配 → 呈报。

## 2026-10-22 — fr-schema 投影面真源全读（切片动工前，会话恢复后）

### 事实（file:line 实证，本会话核实）
- CLI（apps/cli/src/dump-config-schema.ts 48 行全读）：prepareProfile(profile, true, fromDefaultProfile) → collectConfigDumpLayers(loaded, **false**, patches)（与 dump-config 同收集器，非 default-only）→ generateConfigSchema → stdout 单份 `JSON.stringify(dump, null, 2)` + '\n'；生成期 stdout 重定向 stderr（trusted module 防护——fr 静态注册表无动态 import，该机制结构性缺席非偏差）；diagnostics 逐条 stderr；`!dump['x-cordis'].complete` → exitCode 1。
- **stderr 格式勘误（转写勘误，非分叉）**：真源 `${NAME}: ${diagnostic.level}:${location} ${message}`，location = path 存在时 ` [${path}]`（**前导空格**）→ 实际 `dsh: error: [/0] msg`；无 path → `dsh: warning: msg`。findings Q18 节 / PRD:246 / intake CONFIG-5 记为 `fr: <level>:[<path>] <message>`，缺 level 冒号后空格——待修 PRD 6.3 + intake 后管线重跑。
- composeEntries（profile.ts 尾部）= `applyEntryPatches([], structuredClone(layers.flat()), warn)`——**空基合成，schema dump 不读根文件**（dump-config 才读 fr.yml 作显示基）；fr 等价 = `apply_entry_patches(&[], &flattened, warnings)`（patch.rs 既有）。合成 warnings → warning diagnostics。
- skippedBundles → 生成起点 **error** diagnostics：`Selected profile bundle ${JSON.stringify(packageName)} could not be loaded; repair or remove its bundle selection.`（config-schema/index.ts:31-37）；fr 对接点 = `LoadedProfile.skipped_bundles`（stack.rs:140）。
- document.ts 169 行全读：顶层 title `Cordis configuration for profile ${profile}` / description / $comment + `$defs` 骨架 {loaderExpression, entryMetadata, entryList, patchList, unknownConfig, includeConfig, includePatch, entry, patch, configN…}；entry name 条件规则（dormant：disabled=true 行 config 免 required）+ patch id last-wins 规则（indexTargets）+ 一名多 schema ambiguity warning；complete 三条件 = 无 error diagnostics ∧ 无 partial/unsupported/error entries ∧ 无一名多 schema；diagnostics 终态按 entry path 位次稳定排序。
- LOADER_EXPRESSION_SCHEMA（projector.ts:8-12）= `{type:'object', properties:{__jsExpr:{type:'string'}}, required:['__jsExpr'], description:'Inert representation of a YAML !!js scalar from the Cordis entry-list parser…'}`。
- collect walk（collect.ts 249 行）：validateEntry（mapping + name/id 字面 string + group 字面 bool/null，违例 → error diagnostic + 该行 status error，不丢兄弟行）→ 缺省 absent → 查 Config → schema/unsupported → 异常 error；group/include carrier 下行子树（`${path}/config`、`${path}/include`，含 include 文件读+环检测）。fr 无 builtin 形态（PRD:33 已裁决）→ fr M1 行走为平铺，无下行面。
- 五态可达性（fr M1）：schema=注册表命中（callback 注入钉测，compile_to_loader_doc 第 4 参同型）；absent=查无（Q9A，M1 空表常态，configRef='#/$defs/unknownConfig'）；error=坏行/投影失败；partial/unsupported 结构性不可达（描述符构造 fail-closed + to_json_schema 全投影）——状态类型保留五态（场景钉），不可达两态文档注明触发条件。
- 依赖与实现裁量：fr-schema 保持无 IO，新增 dump 模块（行走 + 文档构造 + 类型），入参 = `&[serde_yaml_ng::Value]` 合成行 + `schema_of: &dyn Fn(&str) -> Option<&Schema>`；fr-config 薄胶泥（prepare + collect_dump_layers 复用 + 空基 compose + skipped_bundles 对接）。serde_json workspace 无 preserve_order（BTreeMap 字典序）——若裁 26A 逐字序对齐需加 preserve_order 并按 dsh 插入序构造键。
- include/group carrier 面张力：dsh 文档 $defs 含 includeConfig/includePatch 且 names 表种子常量 cordis:group→entryList / cordis:include→includeConfig；fr 装配无 builtin → 这些描述面描述 fr 实际不存在的形态（patchSchema 校验 --patch 时文档声称接受 fr 拒绝的行）→ Q26 分叉。

### Q26（文档体 $defs 骨架对齐深度，待裁决）
- A：全骨架逐字（含 includeConfig/includePatch + names 常量 + tree 值域），品牌映射后与 dsh 文档体逐字同型；include 面描述 fr 不存在的形态，19.2 登记描述性分歧
- B（推荐）：公共骨架全移植（顶层文案 + loaderExpression/entryMetadata/entryList/patchList/unknownConfig/entry/patch + dormant/patch-id/ambiguity/complete 全口径 + configN 投影），裁 includeConfig/includePatch 与 names 表 builtin 常量（PRD:33 无 builtin 裁决落地到文档面）；tree 字段类型保留、M1 永不产出；19.2 登记文档体 include 缺口
- C：本切片仅 x-fr 面 + entries/diagnostics + patchList 常量，骨架后置独立切片

### Q26 裁决落地（2026-10-22 用户裁决 26B）+ fr-schema 投影面切片完成

- 合同层（七步流 1-3）：PRD 6.3 stderr 格式勘误（`fr: <level>: [<path>] <message>`，无 path 时 `fr: <level>: <message>`）+ 19.2 条 13（26B 文档体裁剪：includeConfig/includePatch 与 names 表 builtin 常量裁剪；tree 字段类型保留 M1 不产出；acceptsMissing 折叠后置）+ 23 决策 25；intake CONFIG-5 勘误 + 2 新场景（注册表命中投影 configN / 查无 schema 标注 absent），场景 301→306（CONFIG 内）；管线全序列重跑：import → accepted 20/20 → draft-specs → refine → depends 清理 → 绑定重放 43+5=48 → 三门禁 0 errors → questions 0 blocking → CONFIG lint Quality 100%。
- 品牌映射字符串清单（既定映射应用，dsh→fr / Cordis→Dougong / Loader→dougong loader / Schemastery→fr-schema）：title `Dougong configuration for profile <name>`；description `Parse !!js with the Dougong entry-list YAML dialect`；loaderExpression 描述 `from the Dougong entry-list parser`；entryMetadata.id 描述 `The dougong loader generates an id…`；disabled 描述 `The dougong loader coerces…`；entry 描述 `dougong loader entry. …`；$comment `fr-schema validates a fallback…` ×2；$comment 裁掉 include 句（`Include-local patches have a separate index.`）。
- 实现层：crates/fr-schema/src/dump.rs 新模块（walk_rows 形状校验逐字消息 / index_targets last-wins+clear 语义含 group-child 下行 / project_definitions configN 按名缓存 / entry_rules+ambiguity warning / patch_rules id 条件 / sort_diagnostics 稳定序 path-less 先 / assemble_document 26B 公共骨架 + x-fr 五键）+ fr-config collect_config_schema 胶水（skippedBundles→error diagnostics 逐字 / composeEntries 空基单次合成 / 合成 warnings→warning diagnostics）；显式生命周期 's（Session 7 同款 HRTB 先例）。
- 测试 +15（fr-schema 11 + fr-config 2 合约绑定 + fr-schema 技术钉）；JSON/YAML 双 Value 域别名（RowValue = serde_yaml_ng）；workspace serde_json 加 preserve_order（x-fr 五键按 dsh 插入序打印；json! 宏比较与既有测试不受影响）。
- 门禁：四关全绿（fmt / clippy --workspace --all-targets 0 / 88+27 tests / doc 0）；lifecycle 48 passed / 260 skipped / 0 failed（43+5 全部真实验证）；guard exit 0；stamp dry-run 48/308 0 failed。
- 测试自纠五件：hook 拦 unreachable! 改先验证后 push / 独立 tests.rs 文件不被 hook 识别为测试改回仓库惯例内联 mod tests / yaml-json 双 Value 域混用 72 错误逐一归域 / 空字符串 patch 夹具非法改 `[]` / clippy 13 项（const fn as_str×3、collapsible_if×5、let-chains（edition 2024）、usize 排序键去 i64 cast、needless_borrow、map_or→is_some_and/is_none_or、assert_is_empty、doc 段落与私有链接）。事故自纠：胶水函数曾插在 render_dump 文档注释与函数之间切断文档关联（clippy missing_docs 抓获）——重定位到注释块之前。
- M1 不可达态：partial/unsupported 结构性不可达（描述符 fail-closed 构造 + to_json_schema 全投影），类型保留（x-fr 面钉五态），PRD 19.2 条 13 载明；schema 态经 schema_of callback 注入钉测（Q10A 框架先行）。
- 下一步：宿主 CLI 期把 dump-config-schema 接到 fr 二进制（stdout 单份 JSON+\n、diagnostics 逐条 stderr、complete 假退 1——宿主 crate 未建，属 M1 宿主切片）；或按计划推进下一 crate 切片。

## 2026-10-23 — /code-review high --fix（commit 9eddb40 fr-schema 投影面切片；后台代理评审+修复，本会话独立复验）

### 评审结论（6 findings：2 修复提交 7850a55，4 呈报/记录）

- **finding 1（已修）**：$defs 与 patch 规则输出序偏离 dsh 插入序——BTreeMap 使 $defs 字典序打印、patch 规则按 id 序，dsh JS Map 为首现序（钉住文件 apps/cli/tests/expected/dump-config-schema.json 本会话实证 defs 序 loaderExpression→entryMetadata→entryList→patchList→unknownConfig→includeConfig→includePatch→config0→config1→entry→patch）。修复：definitions 改 serde_json::Map（preserve_order 插入序）；targets 改 OrderedTargets（Vec 复现 JS Map 语义：set 原位覆盖、remove 保序收紧——serde_json::Map::remove 在 preserve_order 下为 swap_remove 会重排幸存键）；+3 顺序钉测（基骨架序 / configN 追加序 / patch 规则 first-set 序 + clear 保序）。
- **finding 4（已修）**：walk_rows 返回 HashSet<usize>（contains O(1)），index_targets 文档明示 valid 仅 collectable 层消费；纯成本/可读性，无输出差异。
- **finding 2（呈 Q27，待裁决）**：configN 投影为裸 to_json_schema，缺 dsh projector 值位包裹（projector.ts:90 nullable / :100-121 expressions 值位 anyOf [原始, #/$defs/loaderExpression] / :447 应用于 configN 投影，本会话核实）——raw 行 `!!js` 表达式以字符串存在时 dumped schema 判该行 invalid 而 boot 求值成功（dump 严于 boot）。修则变更 fr-schema 公共投影语义。
- **finding 3（呈 Q29，待裁决）**：project_definitions 按插件名缓存投影（HashMap<String,String>）；同名异 schema 静默复用首个投影 → ambiguity warning 与 complete 三条件之「一名多 schema」成死码。受支持输入（schema_of 纯注册表查找，同名必同 schema）不受影响，防御性加固。
- **finding 5（呈 Q28，待裁决）**：合约场景「complete 为假退出码 1」绑定 error_entry_sets_diagnostics_and_incomplete，该测试只断言 SchemaDump.complete==false——退出码半边无宿主 CLI 不可验证（宿主 crate 未建）；绑定绿但 pinned 行为未验。
- **finding 6（呈报确认）**：workspace serde_json preserve_order 随 9eddb40 落地（全局 serde_json::Map 序列化字典序→插入序），系 x-fr/$defs 对齐机制本体；与 Q26B 待呈报两项（x-fr 五键序 {profile, complete, entries, diagnostics, patchSchema}，dump.rs:458-469 与 dsh 钉住文件 x-cordis 键序逐位一致；品牌映射字符串清单见上节 Q26B 落地记录）合并呈用户确认。

### 门禁复验（本会话独立复跑，非转述子代理；2026-10-23）

- fmt --check / clippy -- -D warnings / cargo doc exit 0；cargo test 88+27 全过 0 fail。
- lifecycle（CONFIG 合约，--format json）：308 场景 48 passed / 260 skipped（全 pending 选择器 Skip）/ 0 failed——与 9eddb40 基线逐位一致；exit 1 属 skipped 基线。guard exit 0。
- 工具性自纠一件：lifecycle --format json 输出 JSON 后带附加行，json.load 抛 Extra data——改 json.JSONDecoder().raw_decode 解析成功；首轮临时文件在解析失败分支仍被 rm 误删，重跑补齐计数。

## 2026-10-23 — Q27A/Q28A/Q29A 裁决落地（评审轮呈问闭环）

### 裁决与实现
- **Q27A（configN 投影全脸）**：`to_json_schema` 重构为单一全投影面（对应 dsh 单一 projector）——`project_core` 每节点 nullable（required → 附加 `not:{type:'null'}`，非必需 → type 并 'null'；`Property.required` ↔ dsh `meta.required`，根默认非必需 = dsh 未声明缺省，projector.ts:90-97/426）+ `wrap_value_positions` 值位包裹（properties 值/items/additionalProperties 递归；注解 [title, description, default, $comment, x-fr] 按 dsh 列序前提；anyOf [内核, `#/$defs/loaderExpression`]，projector.ts:100-122/447）。dsh 钉住文件 config0 形态（根 `type:["object","null"]` + 嵌套 `{default, anyOf:[{type:[..,'null'],minimum,maximum}, ref]}`）当场逐位核符。dsh 的 typeless/boolean-core nullable 分支与 propertyNames/prefixItems/anyOf/not 包裹分支在 fr 受限面不可构造（PRD 6.3 子集 fail-closed），函数文档注明不移植；projector.ts:450 第二遍包裹仅作用于 projector 内部递归定义（fr M1 无递归 schema）不涉。
- **Q29A（同名分裂活化）**：`project_definitions` 缓存键 =（插件名，`&Schema` 指针地址——dsh 描述符节点身份的 fr 等价）；同名异 schema 分裂 config0/config1 按发现序，既有 name_choices / entry_rules anyOf 并集 / ambiguity 警告 / complete 门自此由死码活化（评审 finding 3）；+1 钉测 `ambiguous_name_splits_projections_and_warns`。
- **Q28A（退出码场景解绑）**：bind-config-tests.py 对改 `pending_req_fr_config_complete_1`（重生成天然回 pending，脚本幂等保持）；`error_entry_sets_diagnostics_and_incomplete` 测试保留（error 态面仍由其钉）。
- 呈确认三项 D1-D3 未否决按成立（preserve_order / x-fr 五键序 / 品牌映射清单），随决策 26 记录。

### 合同层（七步流 1-2）
- PRD 6.3 to_json_schema 投影全脸条款；**19.2 补条 13——发现决策 25 原引用「19.2 条 13」悬空：Q26B 会话只写引用未落条目，本轮补记**（文档体裁剪：includeConfig/includePatch 与 names 表 builtin 常量不载、tree 类型保留 M1 不产出、acceptsMissing/native 元数据折叠后置）；23 决策 26（三项裁决 + D1-D3 + validate 侧显式 null 接受口径留待 fr boot 校验落地时对齐——本轮仅投影面，dsh Ajv 面行为）。
- intake CONFIG-5：「注册表命中投影 configN」Then 扩投影全脸 + 新场景「同名异 schema 投影分裂告警」，306→309；绑定 48→48（+1 分裂绑定，−1 complete_1 解绑）。
- 管线全序列重跑：import → accepted 20/20 → draft-specs → refine 20 → depends 清理 → 绑定重放 → 三门禁 0/0/0 → questions 39 total 0 blocking → CONFIG lint 仅 advisory（open-question 提示，非门禁）。

### 门禁（本会话实跑）
- 四关全绿：fmt / clippy --workspace --all-targets -D warnings / cargo test 88+28 全过 0 fail / cargo doc。
- lifecycle（CONFIG）：309 场景 48 passed / 261 skipped / 0 failed；guard exit 0；stamp dry-run 48/309 0 failed 一致。

### 自纠
- 首轮测试期望漏写 model 位被前提的 default 注解——实际输出与 dsh 钉住形态一致，期望错；修正后过。
- clippy --all-targets 抓 7850a55 遗留三处（文档首段过长 / `nth % 2` → `is_multiple_of` / 测试内嵌 fn rule_id 提至 tests mod 层）——前轮项目门 `clippy -- -D warnings` 不含 --all-targets 漏检测试代码；本轮起双口径并跑。
- code-quality hook 拦 nullable 中 unreachable!——改 type_name 由构造臂传入的全函数，无占位类构造。
- 管线 transition 首轮误用文件名且 rg 缺 -I 致 ID 污染全败——改 frontmatter id 纯提取后 20/20 过。

## 2026-10-23 — Q30B 宿主切片：fr 二进制 + dump CLI 流面

### 裁决与实现
- 用户裁决 Q30B（推荐 A 被否，选 B 宿主切片）：crates/fr 新 bin crate，workspace members 增补；依赖方向合规 = fr 只依赖 fr-config（fr-schema 注册表经 `pub use fr_schema::lookup as schema_lookup` 再导出，fr 不直依 fr-schema，PRD 4.5）。
- dsh CLI 真源全读（args.ts 全文 + bin.ts + dump-config.ts + dump-config-schema.ts + args.spec.ts + dump-config-schema.expected.e2e.ts），行为面逐条移植：校验序 = profile 缺/空 → --patch 空值 → --from-default-profile 空值 → 内参非空 → default-only+patch 互斥（dsh resolveBoot 顺序）；首个无法识别 token 起整体为内参（含 -h 与后续已知旗标，dsh allowUnknownOption + passThroughOptions 语义）；got-list 逐参数 JSON.stringify。
- fr 子命令形态映射（决策 27）：dsh 三旗标（--dump-config/--dump-default-config/--dump-config-schema）→ fr 两子命令 + --default-only 旗标（kebab 映射，互斥文案同步映射旗标名）；dsh 三旗标互斥报错在子命令形态无可触达调用不移植；dsh desktop 拒绝与 plugin/pnpm 转发不适用 fr（无该入口/无 pnpm 层，未移植）。
- 退出码面：!complete → 顺序输出后以 ExitCode 1 结束（等价 dsh process.exitCode=1）；dump 致命错误单行 stderr + exit 1（dsh 未捕获 throw 的 fr 等价，消息为 fr-config 错误三连）；schema happy 路径 stderr 为空（absent 不破 complete，empty registry 下 headless 14 行全 absent → complete true）。
- M1 入口包层恒为 headless（EntryKind 单变体），入口选择面留 M2+（决策 27 载明）。

### 合同层（七步流 1-2）
- PRD 6.3 三 dump 态段勘误（fr CLI 形态 + 错误族逐字 + 根调用面）+ CLI 旗标尾句改已落地；23 决策 27；intake CONFIG-5 条款扩写 + 5 新场景（拒收内参/参数校验族/流面分流/致命错误/根调用面），309→314；bind-config-tests.py complete_1 重绑 incomplete_schema_dump_exits_one + default-only 拒 --patch 补绑 dump_arg_validation_error_family + 5 新绑定。
- 管线全序列重跑（import → accepted 20 → draft-specs → refine → depends 清理 → 绑定重放）；lint CONFIG Quality 门 exit 0（仅 open-question advisory）。

### 门禁（本会话实跑）
- 四关全绿：fmt --check / clippy 双口径（--all-targets + plain，-D warnings）/ cargo test 8+88+28 全过 0 fail / cargo doc。
- lifecycle（CONFIG）：314 场景 55 passed / 259 skipped / 0 failed（exit 1 属 skipped 基线）；guard exit 0；三门禁 0 errors；questions 40 total 0 blocking。

### 自纠
- 首轮 cargo build 3 错（Result 路径 fr_config::error::Result；overlays 类型 Vec<PathBuf>）且 `| tail` 再吞退出码——pipefail 纪律复记；转后台任务一律读输出文件实判。
- 测试用例错误一次：`fr dump-config-schema --bogus` 期望内参报错但 dsh 语义 profile 缺失检查先行（action 先于 resolveBoot）——实现本就正确，用例补 --profile 后过；exact 断言定案。
- clippy --all-targets 抓 2 处 assert_is_empty（workspace lint 表既有项）→ assert_eq!。
- 管线 transition 复踩 REQ-NNN 占位符（req-template.md 的 id）——agent-spec 拒绝 accepted→accepted 无状态影响；后续 ID 提取应过滤 req-fr-*。

## 2026-10-23 — 会话恢复：验收基线实跑 + guard 门禁面事实（新发现，呈 Q31）

- 本会话实跑验收基线（HEAD 01039f6，工作树净）：四关 exit 0（fr 11 + fr-config 88 + fr-schema 28 = 127 tests，fmt/clippy 双口径/doc 全绿）；三门禁 0 errors；lifecycle CONFIG = 314 场景 55 passed / 259 skipped / 0 failed（exit 1 属 pending 基线，与上轮一致）；explain 渲染成功（Verification Summary 314/55/0/259/0，Pass Rate 17.5%）。
- **guard 扫描面事实（双调用面当场对照实证）**：`agent-spec guard --spec-dir specs --code .`（CLAUDE.md 质量门命令 + `.git/hooks/pre-commit` 现行调用面）exit 0；`agent-spec guard --spec-dir specs/generated --code .` exit 1，逐份列出 20 份 `verification not passing: 0 failed, N skipped`。目录事实：specs/ 直下仅 generated/ 子目录、无直置 spec 文件 → specs 面扫描集为空（空转通过），specs/generated 面才真正扫描且把 skipped 计为 not passing。
- 影响（溯源）：Session 10 观察的 "guard FAIL 20/20 verification not passing" 为 specs/generated 面（真实扫描）；此后各轮（7f8948c 评审起）记录的 "guard exit 0" 基线均为 specs 空转面；pre-commit hook 自 2026-10-22（Q16A）安装以来从未真正阻断过提交——与 Q16A「会阻断提交直至合约转绿」的已知照预期不符。1221 场景中仅 55 绑定，真实 guard 面将持续红至全部合约绑定。
- 呈 Q31（门禁面裁决）：A = CLAUDE.md 质量门命令与 pre-commit hook 统一改 `--spec-dir specs/generated`（恢复 Q16A 阻断语义，红至合约全绑，提交期阻断已知照）；B = 报告与切片验证以逐合约 lifecycle 为准（现状已覆盖），hook 维持空转并在 PRD 19.2/决策登记该事实；C = 用户另定。
- **Q31A 裁决落地（2026-10-23 用户裁决 A + 追加质询修正）**：CLAUDE.md 七步表第 5 步与 `.git/hooks/pre-commit` 统一改 `agent-spec guard --spec-dir specs/generated --code . --min-score 0.7`。用户质询 min-score 0.6 来源——查证为 `agent-spec install-hooks` 模板默认（= guard CLI 默认），非项目裁决；2026-10-22 装 hook 时只修 `--code src → .` 路径缺陷、未审其余参数，系我方疏漏；已按项目既定 0.7 标准（七步表第 2 步）显式钉死。实跑验证：hook exit 1 且 20 处 `verification not passing`、guard min-score 0.7 exit 1——新常态 = 实扫 20 份、阻断直至合约全绑；阻断期内提交一律 `git commit --no-verify`（标准操作，Q16A/Q31A 已知照）。
- Phase 13 动工纪律：fr-session 是 dougong 插件 crate，实现期必用 dougong-plugin-dev 与 rust-guidelines 技能；动工前 dsh 会话域真源全读（findings 后续节落盘）。

## 2026-10-23 — Phase 13 真源全读（一）：core/session src 3324 行全读完成

标注约定：PRD✓ = PRD §7 已钉死且本轮逐点核实成立；新 = PRD 未载。文件均在 `~/project/reference/cordis/deepseek-harness/packages/core/session/src/`。

### types.ts（562）
- PRD✓：SessionEvent 判别联合 13 核心 type；公共字段 type/seq/time/data/ignorable?/surfaceOp?/sourceEventSeqs?（后两者仅五类 surface 类型合法，assistant/message 禁 sourceEventSeqs）。
- 新（勘误候选 E1）：TurnEndReason 实为七变体——completed / aborted{reason: TurnEndCancelCause} / blocked / error{error: LlmFailure} / max-tokens / **interrupted**（crash-orphaned turn 由 resume 追加 closer，loop 永不 live 发出）/ **forked**（仅 fork seed 携带）。PRD §7.1 表只列五变体。aborted.reason 结构 {kind: 'user'|'parent'|'hook'{reason}|'disposed'} 未载。
- 新（E2）：user/message 的 data = UserMessage **直接**（无 {turn,step,message} 包裹）；developer/system/assistant/tool-result 均 {turn,step,...} 包裹——wire 形状差异。
- 新：developer/message data.headerSeq?（有 tool-addition 时必需，指向 request/header；surface.ts:368-395 全链校验：须早于本事件且为 request/header、每个 tool-addition 的 toolName 恰命中该 header 一个工具、定义须含 description string + parameters record、deferLoading 存在必须 ===true）。
- 新：request/header data.startsSeries?: true；request/context = RequestContext{provider,model,contextWindow?,systemPromptUpdate?}（仅 route/capacity/systemPromptUpdate 模式变化时落、不参与 header 相等）；EpochHeader.system?: never（append 期拒绝，surface.ts:204）。
- 新：append 期 snapshotJsonValue 全数据丢失无损 JSON 校验（BigInt/function/undefined/-0/非有限数/循环/稀疏数组/Map/Set/Date/类实例皆拒，违例 append 点即抛）+ append 重入拒绝。
- 新：session/end-seed {inherited?: true}；构造器规则（index.ts:666-698）：isSeeded 头必须配 seed+inheritedEventCount；inherited>log.length 拒；fresh 子在切点无 tagged marker 自动补 {inherited:true}；restore 路径末尾非 end-seed 补 untagged {}；切点后第二 tagged marker 拒。
- 新（Q33 候选）：plugin: 记录通道——appendPluginRecord（ignorable:true、type 语法 `plugin:` 小写段正则、仅 experimental 包可调）+ pluginRecordOf；reader 对未知 ignorable 记录保留跳过；fork/resume 带走。
- 新（Q34 候选）：KNOWN_SESSION_EVENT_TYPES = 生成目录 64 类型（core 13 + 插件持久事件全集 approval×3/compaction×4/todo/subagent×3/hook×2/llm-retry×2/session-title×2 等），读路径拒未知非 ignorable 类型；dsh 为构建期 TS 合并，fr 需等价注册机制。
- 新（Q35 候选）：MESSAGE_PROJECTION_EVENT_TYPES = {'image/offload'}——改变既有消息内容的插件事件必须提供纯投影解释器（SessionMessageProjection{type, project(event,ctx)->Map<seq,Message>}），缺定义读路径抛、定义注销缓存失效（surface.ts:698-711）。
- 新（Q36 候选）：ToolHistoryProjection（tool-history.ts）——request/header 进 headers 表，reason==='series'||startsSeries||同名工具定义 stringify 变化 → 重置基线（弃 prefix reuse）；developer/message tool-addition 从 headerSeq 解析定义；snapshot={tools:基线,updates:[{messageId,additions}]}，active≠available 时 legacy 回退 updates:[]。PRD §7.4 未载 tools 投影（fr-llm toolUpdate 供方）。

### request-header.ts / seq-ranges.ts / repair.ts / fork.ts
- PRD✓ request-header：canonicalHeader（adapterDefaults 仅两标记之一 ===true 时保留；空 tools 省略）+ headerEquals（callConfigEquals+两布尔+tools JSON.stringify 顺序敏感）+ fold last-wins —— 与 §7.4 一致。
- PRD✓ seq-ranges：encode 严格递增才折叠、run≥3→[start,end]（否则逐个原样）；decode 数字或 [start,end] 对、start<=end、maxEntries 上限、带范围整体须严格递增。
- PRD✓+精确形态 repair：CLOSER_TEXT 四文案（interrupted/forked × started/notStarted）全量到手；openTurnClosers = recovery.results() 先行 + step/end（若开）+ turn/end{kind}，seq 自 last.seq+1 连续、time=last.time；合成 message id = `<cause>-tool-result-<callId>-<seq>`；started→error{name:ToolOutcomeUnknownError,code:TOOL_OUTCOME_UNKNOWN}+sourceEventSeqs=[callSeq]，未启动→ToolNotStartedError/TOOL_NOT_STARTED；ToolCallRecovery：pending 从 assistant/message tool-call 块注册（tool/call 仅记 callSeq），turn/start、turn/end、step/end 清空 pending，tool/result 仅 surfaceOp==='append' 且 turn/step 匹配才消账。
- PRD✓ fork：buildForkSeed = slice(0,boundary+1)+tagged{inherited:true}@boundary+1（time=boundary 事件 time）+openTurnClosers('forked')；inheritedEventCount=boundary+1 不计 closers；store.fork 拒绝码 SESSION_NOT_FOUND/SESSION_NOT_LIVE/SESSION_ALREADY_EXISTS/INVALID_BOUNDARY。

### index.ts（1453）store/append/derive
- 新：store 生命周期 = prepare/enter/announce 三段事务（enter 先于 announce；session/created 同步 throw 可否决并回滚）；session/event 观察 feed per-listener 容错；session/flush 并行等待检查点（返回是否有监听者、全部落定后抛首个失败）——**持久化在 dsh 是插件 concern**（订阅 session/event、drain on session/flush）；fr 对接点 = fr-session 需提供等价挂点（dougong 事件监听器），另有 dsh-session-checkpoint-policy 拥有 per-request 持久化屏障（与 §7.2 fsync 语义衔接，persistence 读毕定位）。
- 新：append 精确序 = snapshotJsonValue(data) → surfaceOp/sourceEventSeqs 元数据快照 → seq=log.length、time=Date.now() → validateSessionEventData → surfaceManager.validateNext（**先验证后入 log**，失败不部分变更）→ commit（push+失效快照+观察者）。
- 新：deriveMessages 缓存 = contentGeneration 变化整表重建/纯尾增长只投影新节点/返回新数组但消息共享深冻结（PRD✓ §7.4 + 实现形态）；requestHeader()/requestContext()/toolHistory() 均增量折叠。
- 新：SessionHeader 校验精确错误族（version===4/id 匹配/createdAt 非负安全整数/cwd 须绝对路径/isSeeded 必需 boolean/origin==='subagent'/delegationDepth 非负安全整数/agentPreset string；restore 加原型纯度检查）；seedLength 显式拒绝 PRD✓。
- 新：seed 校验 = append 同不变量（信封键白名单 type/seq/time/data/surfaceOp/sourceEventSeqs/ignorable；seq===index 连续自 0；ignorable 只许 true；消息 id 非空 string+role 按 MESSAGE_ROLE_BY_TYPE+source.kind 判别（system-prompt/model+provider+model/tool+callId）+toolCallId===source.callId）。

### surface.ts（712）
- PRD✓ §7.3 全部成立：五类入表面、非表面携带即抛（例外：未知 ignorable 记录保留不透明元数据不抛）、五类缺 marker 抛（PRD 未明写反向要求）、replace 形状恰三键且双闭区间须早于当前 seq、sourceEventSeqs 非空/无重复/全更小/覆盖全部被遮蔽、assistant 禁带、tool/result 只改 content（深相等双方 content 置 null）、node-0 system 只许 system/message 单节点重写、replaceGeneration/contentGeneration 双计数。
- 新：validate→plan→apply 三段原子折叠（validateNext 产出 pending plan，commit 时复用，失败零变更）；isAppendSurfaceEvent（人读 transcript 源）vs isReplacementSurfaceEvent（model-only）区分。
- 新：validateSessionEventData 追加规则族——developer 角色配对、tool-change 块须 developer 角色、tool-addition 非空 toolName+禁内联 tool 定义、headerSeq 恰在有 additions 时存在、request/header 禁 header.system/空 tools/空 adapterDefaults、tool/result error 须 isError===true。

### 分叉候选（persistence 读毕集中呈 Q33+）
Q33 plugin: 记录通道 M1 范围 / Q34 KNOWN 词表注册机制 / Q35 message projection 机制预留 / Q36 ToolHistory 投影补遗 / E1 turn-end 七变体勘误 / E2 user/message 直载形态勘误。

## 2026-10-23 — Phase 13 真源全读（二）：session-persistence-jsonl + session-format（核心面读毕）

文件：session-persistence-jsonl/src/{format 538, index 1837, lease 135, storage 571} 全读；generation.ts(1059)/win32.ts(208)/migration-verifier/catalog-migration（fr 排除迁移链，不读）；zstd.ts(156) 帧结构经 index.ts 调用面掌握；session-format/{filename, json} + v3-to-v4/validation.ts 全读（relationships 385 行细项留实现期）。

### 物理布局与命名（PRD✓+精确形态）
- PRD✓：`session.v4.jsonl`（正则 `^session(?:\.v([1-9][0-9]*))?\.jsonl$`，v0 无 .vN；zstd 加 `.zstd` 后缀）；世代命名永不改名；读到非 v4 → SessionFormatUnsupportedError（"upgrade the harness" 文案族，filename 版本 vs header storedVersion 不一致报错）。
- 目录：`<root>/<projectKey(cwd)|_no-cwd>/<encodeSegment(id)>/`；projectKey = 分隔符折叠 '-' + 安全字符字面 + `~XXXX` 转义 + `--<slug≤251>--` 包装 + 'root' 回退；encodeSegment = UTF-16 码元单射（安全集 [A-Za-z0-9._-] 字面、'.'/'..' 特判、余者 ~XXXX 大写十六进制、拒空串）；session.lock 与日志同目录。
- 错误词汇：SessionAlreadyExistsError / SessionAlreadyOwnedError / SessionPersistenceNotFoundError / SessionPersistenceCorruptionError（`corrupt session log: …(raw log: <path>)`）/ SessionFormatUnsupportedError / SessionReadOnlyError / SessionHandleClosedError；身份校验 = header id+cwd 反推路径须与实际一致（realpath 容大小写别名）。

### 物理不变量（PRD✓ §7.2 逐条核实 + 精确形态）
- materialize POSIX：mkdir root(0700)→fsync 根父→mkdir project→fsync root→mkdir dir→fsync project→拒既有日志→tmp=`<final>.<12hex随机>.tmp` open 'wx' 0600 写+fsync+close→**link(tmp,final)**（EEXIST 互斥，禁 rename）→fsync dir→尽力 rm tmp（失败仅吞）。
- appendLines：open 'a'→记 before size→write→sync；失败 = close+truncate 回滚 before+fsync 再重抛（回滚失败 AggregateError）。
- zstd 帧：header 行独立一帧、每个 append 批独立一帧（materialize 含事件 = headerFrame+eventFrame；空 = 仅 headerFrame；首帧必须恰一行 header）；读 = scanZstdFrames→逐帧解码（500ms 让出）→撕裂末帧：完整 JSONL 记录可解出则 recoveredTail（写路径先 truncate(tornStart) 再重写），纯字节残片无完整记录则只 truncate。
- 写租约 lease.ts：`session.lock`（目录内）+ POSIX 非阻塞 flock（EAGAIN/EWOULDBLOCK→SessionAlreadyOwnedError）+ **锁后 inode 复验**（锁的 inode ≠ 路径现 inode 则重开重锁，3 次后报争用）；**锁文件永不删除**（保 inode 稳定供复验）；无过期（ deliberate——不让瘫痪写者被剥夺）；Windows = 路径派生命名内核信号量（无锁文件）；内核随描述符关闭/进程死亡释放。
- 读路径修复：torn tail 首次 append 前先 truncate+fsync（warn "recovered from a torn tail"）；句柄单调读视图（observedLength 只增，日志变短即抛）。

### 句柄/存储运行时（storage.ts 全读）
- LIVE_WRITE_BATCH_MAX_DELAY_MS=200（路由 live 事件批窗口）；句柄 per-handle promise 链串行；append 先验证+快照再入链；flush 对空会话 materialize header-only（显式 flush 的空会话存活）；close = 循环 drain 至空→锁释放→记账释放（失败聚合）。
- Tracker：单写者/id（claimWrite/SessionAlreadyOwnedError）+ created-unmaterialized pending 集（内存 revision `memory:<name>:<n>`）+ flushAll + install（session/event→enqueueLive、session/flush→drain+flush、session/disposed→close）——**持久化在 dsh 是订阅 core/session 事件的插件**，fr 对接 = fr-session 经 dougong 监听器等价接线。
- 惰性物化：create 后进程内立即可见、磁盘工件在首次 append/flush、崩溃前从未存在（无文件足迹）。

### v4 校验层（session-format + v3-to-v4/validation）
- HeaderLine 物理首行：必选六键 type:'session'/version/id/createdAt/isSeeded/delegationDepth（逻辑头 delegationDepth 缺省写 0）+ 可选 cwd/parentSession/origin/agentPreset，键白名单严格；retired 字段 sandboxMode/approvalPolicy 拒。
- v4 artifact 校验（fr 读路径须镜像）：header 形状（assertReleasedV4Header）→ inheritedEventCount ≤ 事件数、unseeded 须 0 → 逐事件：未知类型须 ignorable（否则 unsupported 拒）、seq 稠密 ===index、retired 语法/system 字段/tool-result 角色/fork-result 校验 → 最后 tagged end-seed 的 index 须 ===inheritedEventCount（seeded）或不存在（unseeded）→ assertReleasedV4Relationships（developer 数据/消息来源/delivery 所有权/subagent catalog 子 id 去重/生命周期关系——细项实现期读 relationships.ts:385）。
- 增量扫描 SessionLogScanner：header 恰一行 0x0A 结尾、撕裂尾 = 无换行末记录忽略、recoverable 模式吞首个坏行（后见 turn/end 即抛）、committedBytes = 安全 append 偏移。

### 新分叉/补遗候选（集中呈 Q33-Q39 + E1/E2）
- Q33 plugin: 记录通道 M1 范围（dsh core 机制，仅 experimental 域调用；fr M1 无调用方）
- Q34 KNOWN 事件词表注册机制（dsh 构建期生成 64 类型目录 vs fr 运行时注册面）
- Q35 message projection 机制（image/offload 解释器）M1 预留与否
- Q36 ToolHistory 投影 PRD §7.4 补遗 + fr-session 交付（请求组装必需件）
- Q37 zstd 载体（Rust zstd crate + checksum 帧对齐 dsh）——推定项可否决
- Q38 后端 Config 默认（compression 默认 zstd 对齐 dsh DEFAULT_COMPRESSION；listConcurrency 属 JS 调度产物，fr tokio 形态取舍）
- 推定 D 项（未否决成立）：物理细节逐字——projectKey/encodeSegment/`_no-cwd`/session.lock/tmp 命名/12hex/错误文案族/单调读视图/200ms 批窗/惰性物化
- E1（勘误）§7.1 turn/end 七变体（interrupted/forked 缺）+ aborted reason 结构；E2（勘误）user/message data 直载无 {turn,step} 包裹
- 后续切片分叉登记（本轮不问）：dsh-session-checkpoint-policy 的 per-request 持久化屏障归属（fr-agent-loop/宿主切片定）

### Q33-Q38 + E1/E2 裁决记录（2026-10-23，用户裁决：Q33A / Q35B / Q36A；Q37/Q38 及物理推定项未否决成立）
- **Q33A**：M1 实现 plugin: 记录通道——fr-session 服务提供 append-record 入口（ignorable:true 强制 + `plugin:` 小写段语法校验 + pluginRecordOf 等价读回），M1 无调用方；dsh 的 verify-plugin-record-callers repo 门禁由 SESSION 合约 Boundaries 约束调用方等价。
- **Q35B**：message projection 机制（image/offload 解释器）M1 不预留；SESSION 合约登记，Phase 17 需要时加。语义自洽：M1 无写入方故日志不含 image/offload；若读旧日志遇之，按 dsh 同语义"已知类型缺解释器"抛错（缺 projection 定义抛错）。
- **Q36A**：PRD §7.4 补遗 ToolHistory 投影（header 基线 + tool-addition updates + series 重置）+ intake SESSION 场景 + fr-session 交付 ToolHistoryProjection 等价物（fr-llm toolUpdate 差异表供方、请求组装必需件）。
- **Q37/Q38/物理推定项**：未否决成立——Rust zstd crate + checksum 帧对齐；compression 默认 zstd、listConcurrency 以 tokio 信号量等价默认 3；projectKey/encodeSegment/_no-cwd/session.lock+inode 复验+锁文件永不删/tmp 12hex/错误文案族/单调读视图/200ms 批窗/惰性物化逐字对齐。
- **E1/E2**：勘误候选待确认（turn/end 七变体 + aborted reason 结构；user/message data 直载）——随 Q34 选型一并确认后落 PRD §7.1。
- **Q34 重新详析（用户要求细化 + 一切皆插件约束）**：dsh 词表门语义（未知非 ignorable 类型拒重建/ignorable 保留跳过）PRD 已钉；分歧仅在 fr 全集来源。构建期聚合结构上不可能（Rust 无声明合并 + 插件为独立 cdylib 仅经动态通道交互，fr-session 编译期不可见其他插件类型）；"fr-session 写死 64 类型静态表"方案违反一切皆插件（中央化他插件知识，卸载/换版本即假）。候选：A（推荐）激活期声明——各插件 apply 期间调 fr-session 声明方法（dougong 五步契约第 2 步本是一切注册发生地），代码真值无漂移、随 LIFO 撤销收窄、零内核改动；重复声明同类型 = 装配错误 fail-closed。B descriptor 声明——descriptor 已有 listen 事件名声明先例，但 schema 由 dougong-abi 拥有且严格拒未知字段，扩展须改内核 ABI，fr 单侧不可落地。C 装配配置声明——fr.patch.yml 行 config 携带，人工可编辑与代码真值漂移风险最大且 dsh 机制本体为代码派生，不推荐。

### Q34 二轮详析前的事故自纠与 dougong 源码事实修正（2026-10-23）
- **事故**：`rg -rn "" -m1` 复踩 -r 陷阱（CLAUDE.md 明载 + 2026-10-22 已两次留档"rg 一律不带 -r"），输出全文逐字符替换显示；仅显示失真未改文件。属明知禁令仍复踩，已记 progress。
- **B 方案关键陈述失真自纠**：上轮"descriptor schema 严格拒未知字段、须改内核 ABI"未回源码核实（凭 skill 摘要 pitfall）。本轮读 `~/project/dougong/crates/dougong-dylib/src/lib.rs:511-551` 实证：descriptor 解析为自由 `serde_json::Value`，宿主**逐键抽取** provide/inject/listen（:512/:523/:534），**未知键天然忽略**——B 不需要改 descriptor schema；但 `DynPlugin::descriptor()`（:138）当前**私有**，宿主拿不到 descriptor Value，B 需内核加透出访问（极小）。用户已明示：dougong 可升级（其为维护者，0.2.0 期）。

### Q34 三轮：A/B 多维权衡 + 用户终裁 Q34A + 技能核对与修正（2026-10-23）
- 用户终裁 **Q34A**（激活期声明）。内核补充事实：宿主侧 `Plugin` trait（dougong/src/plugin.rs:62）仅暴露 name/inject/apply——descriptor 只在装载期被 dougong-dylib 内部消费、从不回透宿主；A 方案零内核依赖成立，B 方案确需内核加透出（此前成本分析最终无误）。
- **dougong-plugin-dev 技能核对结论**：SKILL.md 与 references 全部可检陈述与源码相符（ABI_VERSION 2 ✓、listen 语义 ✓、buffer 协议 ✓、激活期注册 ✓、teardown-only ✓）——**无误述**；我方"严格拒未知字段"系自己过度引申非技能内容错误。但技能有一处**文档缺口**导致误读：descriptor 节未说明未知键待遇与宿主回读面。已修正 `references/dylib-plugin.md`（descriptor 节补三句：自由 JSON 逐键抽取/未知键今日忽略/自定义键是惰性数据不作扩展通道，扩展走 apply 激活期注册）。技能目录是指向 dougong 仓的符号链接，一次编辑两份生效；**dougong 仓侧改动未提交**（留用户评审）。
- 落地批次：PRD §7.1 三处（词表机制 + plugin 通道 / turn/end 七变体 E1 / user/message 直载 E2）+ §7.4 tools 投影补遗（Q36A）+ 19.2 条 14（Q35B）+ 23 决策 28；intake SESSION-1 扩写 + 新增条款 18/19/20 + 9 新场景（SESSION 93→102，全局 1240→1249）+ Source Trace 四行 dsh 证据。
- 管线全序列重跑：import 20 ✓ → transition 20/20（**新坑**：sed 提取 frontmatter id 为大写 REQ-FR-*，case 模式写小写 req-fr-* 静默零匹配——transition 全空跑、draft 零产物；改正后 20/20）→ draft-specs 20 ✓ → refine ✓ → depends 清理 ✓ → bind 重放 ✓（CONFIG lifecycle 55 passed / 259 skipped / 0 failed 基线不变）→ 三门禁 0/0/0 → questions 40/0 blocking → SESSION lint min-score 0.7 PASS（仅 pending 选择器 open-question advisory，草稿态预期）。
- 自纠两件：intake 编辑曾混入垃圾标题行"## Scenarios 追加占位不存在"（当轮发现即除）；场景计数三连错（生成稿为裸 `Scenario: ` 行，非 `场景:` 亦非 `### Scenario: `，计数模式改为 `^Scenario: ` 后数字落定）。

## 2026-10-23 — Phase 13 实现切片 A：fr-wire 会话域 wire 形状库（首批落地）

- crate 新建 `crates/fr-wire`（workspace members/内部依赖/共享元数据；workspace.package 补 description）。定位 = PRD 4.4/4.5：纯 serde 形状库，各插件工件链接副本、载荷 JSON 字节跨界；会话 wire 类型单源于此。
- 类型真源全读：dsh-llm {types 571, message 307, call-config 96, brand 76, content 492（行为层，类型在 types）, assistant-stream 记录段} + dsh-attachment types（Image/FileAttachmentRef + ImageMediaType 四值）。
- 核心设计定案：**全部 merge-extensible union（ContentBlock/Message/MessageSource/FinishReason/StreamChunk）= 已知变体强类型 + `Other(RawObject)` 原样保留回放**（未知 kind/type/role 绝不拒绝亦不变形）；已知来源/消息以 `#[serde(flatten)] extra: RawObject` 保留生产者扩展字段；`TrueOnly` 标记类型只接受字面 `true`（offloaded/isError/deferLoading/startsSeries 族）；SessionSeq/SessionLogOffset = u64 ≤ 2^53−1（SAFE_INTEGER_MAX 对齐 dsh safe-integer 域）。
- 模块面：ids（7 个 opaque id 宏 + 2 个 seq newtype）/ content（7 内容块 + attachment refs）/ message（5 角色 + 4 来源 + 别名族）/ call_config（Failure/TokenUsage/FinishReason/ReplayEnvelope/ToolSchema/ToolHistory/CallControls/CallConfig(flat controls)/AdapterDefaults/SystemPromptUpdate/ToolUpdate）/ stream（StreamChunk 七种 + AssistantStreamRecord 四种打包形）。
- 测试 16：wire 键逐字钉（toolName/toolCallId/isError/argumentsDelta/time0/replayState）+ 未知变体保留回环 + TrueOnly 拒 false + safe-integer 域。
- 事故（重大，两起 Write 工具生成损坏）：大段枚举 Rust 经 Write 两次产出乱码——一次错误路径（/home/crate0.sol，权限拒未落盘）、一次 message.rs 乱码落盘；按 Session 7 既有教训改分块 heredoc（≤40 行/块）+ 每块乱码扫描重建；后续一次 python 批量 `Some(Some(`→`Some(` 吞掉 assert 尾括号三处，逐点修复。
- serde 知识点（4 测试失败根因）：**枚举级 `rename_all` 只作用于变体名，不级联变体字段**——全部带字段变体逐一补 `#[serde(rename_all = "camelCase")]`（27 处）；另 event 联合（type 字段与 data 单源、未知事件保留）将用手工 serde 实现（下一切片 event.rs）。
- 四关全绿：fmt / clippy 双口径 -D warnings / test 143（127+16）/ doc 零警告。
- 待续切片：event.rs（SessionEvent 信封手工 serde + SessionHeader/EpochHeader/SurfaceOp/EventData 14 变体）→ fr-session crate（信封校验/surface/deriveMessages/ToolHistory/词表/plugin 通道/持久化）。

## 2026-10-23 — Phase 13 实现切片 B：fr-wire event.rs 会话信封（手工 serde）

- event.rs 954 行 + 10 钉测（fr-wire 16→26）。核心机制：**SessionEvent 手工 Serialize/Deserialize**——`type` 键与 `data` 对象单源于 `SessionEventData` 联合（serialize 按 variant 分派；deserialize 按 type 名分发到具名 Data 结构体，未知名落入 `Unknown{type_name, data: RawObject}` 原样保留）；信封键序与 dsh writer 插入序一致（type, seq, time, data, sourceEventSeqs?, surfaceOp?, ignorable?——preserve_order 下逐字节锚点稳定）；信封未知键在 wire 层即拒（错误含 `unknown envelope keys`；dsh 六键白名单的 fr 对应）。
- 变体面（14 + Unknown）：turn/start|end（reason 七变体 + CancelCause 四 kind + merge-extensible Other）、step/start|end（同构 StepBoundsData）、user/message（**data 直载 Message**，E2 落地）、developer/system/assistant/message、assistant/attempt、tool/call、tool/result（error{name,code,reason?} + meta）、request/header（EpochHeader：config + adapterDefaults? + tools?；system 为 persistence-reserved 不建模，admission 拒）、request/context、session/end-seed（inherited?）。
- SurfaceOp：untagged{Append(AppendOp), Replace(ReplaceOp{op,startSeq,endSeq})}；Append/ReplaceTag 自定义 serde（裸字符串 "append"/"replace"）。头部族：SessionHeader（逻辑头，delegationDepth 可选——物理行形态属持久化层）/Origin/EpochHeader/RequestContextData/ToolResultError。
- 设计要点：14 个 `*Data` 具名结构体（避免 untagged 内联枚举的字段形状歧义碰撞——如 step/start 与 assistant/attempt 字段子集关系）；SessionEventData 不派生 serde（手工单源）；Eq 全量可派生即派生，三个 untagged Other 回退（ContentBlock/MessageSource/FinishReason/TurnEndReason）以 `#[expect(clippy::derive_partial_eq_without_eq, reason=…)]` 带因豁免。
- clippy 自纠：doc_markdown 批量（--fix）、Eq 逐类型、match 臂合并（StepStart|StepEnd）、redundant clone ×3、文档段过长拆段；测试断言改为 `json["key"]` 直接比较与 `expect_err`（避 assertions_on_result_states）。
- 四关全绿：fmt / clippy 双口径 -D warnings / workspace test（fr-wire 26）/ doc 零警告。
- 待续切片 C：fr-session crate 本体（信封校验/surface 折叠双计数/deriveMessages/ToolHistory/词表/plugin 通道/持久化）——dsh 精确语义在 findings 三节真源记录。

## 2026-10-23 — Phase 13 实现切片 C：fr-session 本体首批（admission + surface + tool_history）

- crate 新建 `crates/fr-session`（deps fr-wire/serde_json/thiserror；本轮 rlib 纯逻辑，cdylib 目标与 dougong-abi 壳留 plugin 接线切片——PRD 4.3 dylib 工件形态不变，先立语义内核）。workspace members/内部依赖接线。
- **admission.rs**（append/restore 数据校验）：角色↔事件类型配对（MESSAGE_ROLE_BY_TYPE 等价）、id 非空、source.kind 判别（system-prompt/model+provider+model 非空/tool+callId 双向一致 toolCallId===source.callId）、developer 角色配对、tool-change 块仅 developer、tool-addition 非空 toolName + headerSeq 恰在有 additions 时存在、tool/result error 须 isError===true。Message::Other（未知角色）在校验面拒绝。未知角色来源字段经 fr-wire flatten 保留。restore 侧 reserved-raw 原文检查（header.system/seedLength/retired 头字段/raw tool 内联键）留持久化扫描边界（切片 C4）——fr-wire 类型化构造使 append 侧结构性 fail-closed。
- **surface.rs**（折叠与硬约束，14 钉测）：eligibility 五类、marker 必带/非表面禁带（未知 ignorable 保留无效果——dsh surfaceOpOf 早返回语义）、replace 界早于当前 seq、sourceEventSeqs 非空/无重复/全更小/覆盖全部被遮蔽、assistant 禁带、tool/result 改写恰单节点+仅 content（可比形态 = 全字段 content 置 Null 后 Value 深比较）、node-0 system 单节点 system 重写保护、developer headerSeq 全链（早于 + request/header + 恰命中一工具定义）、双计数（replace_generation/content_generation 仅 replace 递增）、seq 连续性、plan→apply 两段（先验证后变更）。**校验分层修正**：surface 折叠不跑 validate_event_data（dsh 中数据校验属 seed 边界、折叠仅元数据/developer-header/改写检查）——首轮次序偏差由测试逮住后修正。
- **tool_history.rs**（Q36A）：request/header 进 headers 表 + series 重置（reason==='series'||startsSeries||同名工具 JSON.stringify 漂移）；developer/message tool-addition 经 headerSeq 解析定义（缺失 panic，dsh 文案逐字 + # Panics 节）；快照 {tools,updates}；active≠available legacy 回退 updates:[]。
- 夹具：`tests_support.rs`（cfg(test) pub(crate)）共享 seq/header_tool/developer_addition。
- 四关全绿：fmt / clippy 双口径 -D warnings / workspace test 169（127+26+16）/ doc 零警告。
- clippy 自纠清单：Eq 全量可派生即派生（4 处）、const fn 访问器化（fr-wire KnownMessage 五访问器，content 因 Vec deref 非 const 除外）、needless_borrow、collapsible_if→let-else、assertions_on_result_states→expect_err/unwrap_err、identical match 臂合并、redundant clone ×3、cast usize→try_from+MAX、空函数删除（validate_attempt_fields/settlement 两轮纠缠后净删）。
- 待续切片 D：Session 增量壳（append/derived 缓存/请求头折叠）+ 词表声明聚合 + plugin: 通道 + 持久化物理层（format/lease/zstd/scan/restore 侧 reserved-raw 准入）+ dougong 插件壳。

## 2026-10-23 — Phase 13 实现切片 D：Session 增量壳 + 词表聚合 + plugin 通道

- **session.rs**（Session 增量壳）：new/from_seed（种子连续性 + admission + 折叠）；append_surface/append_log_only/append_record 三入口（eligibility 运行时对账 + plan→apply 先验证后变更）；derive_messages 缓存语义钉死（contentGeneration 变化全表重建 projecting 列表、纯尾增长只投影新节点；空 assistant/system/developer → 不投影；返回 &Message 借用共享）；request_header 增量折叠 + canonical（空 adapterDefaults（双标记全缺）/空 tools 折叠为 None）；append 时钟为显式 time 参数（host 注入缝）；&mut self 结构性杜绝 dsh append 重入。
- **vocabulary.rs**（Q34A）：EventVocabulary::core（14 静态）+ declare（重复声明装配错误，消息 `session event type "x" is already declared`）+ knows；refuse_unknown_required（读路径门：未知非 ignorable 类型拒重建，dsh validation.ts 消息逐字 `format v4 contains unknown event type "x" at seq N`）。
- **admission.rs 补**：is_plugin_record_type（Q33A 语法：`plugin:` + `/` 分段 [a-z0-9] 首 + [a-z0-9._-]*，手写无 regex）。
- 钉测 9（session 7 + vocabulary 内嵌于 session 测试）：节点/日志分野、派生缓存尾增长与空投影、replace 双计数后重建、header 折叠 canonical、plugin 记录保留与语法拒、词表重复声明拒 + 读路径拒、seed 连续性拒。
- 事故自纠：分块 heredoc 两处 impl 未闭合 + 测试夹具垃圾行一次（"marker on log-only" 用例在类型化构造层不可实现——dsh 该检查属 restore 原文边界，删测留 C4）。
- 四关全绿：fmt / clippy 双口径 -D warnings / workspace test 178 / doc 零警告。
- 待续切片 E：fork/repair（buildForkSeed + ToolCallRecovery + openTurnClosers）+ 持久化物理层（format/scan/materialize/append/lease/zstd/restore 侧 reserved-raw 准入）+ dougong 插件壳（cdylib + descriptor + apply 词表声明服务）。

## 2026-10-23 — Phase 13 实现切片 E：fork/repair + 持久化物理层 + dougong 插件壳

### 真源复读补遗（切片动工时，file:line 实证）
- repair.ts 全文：CLOSER_TEXT 四文案（interrupted/forked × started/notStarted）+ TOOL_NOT_STARTED/TOOL_OUTCOME_UNKNOWN；ToolCallRecovery.observe（turn/start|end、step/end 清 pending；assistant/message tool-call 块注册；tool/call 记 callSeq；tool/result 仅 surfaceOp==='append' 且 turn/step 匹配消账）；results() 消息 id `<cause>-tool-result-<callId>-<seq>` + 插入序；openTurnClosers = results 先行 + step/end（若开）+ turn/end{kind}，seq 自 last.seq+closers+1、time=last.time。
- fork.ts 全文：buildForkSeed = slice(0,boundary+1) + tagged end-seed@boundary+1（time=boundary 事件 time）+ concat forked closers；inheritedEventCount = end-seed 所在 seq（= boundary+1）。
- format.ts 全文：HeaderLine 六必需四可选键 + isHeaderLine 全守卫；assertNoRetiredHeaderFields（sandboxMode/approvalPolicy）；refuseForeignFormatVersion（先于形状检查，sessionFormatVersionRefusal 双向文案）；encodeSegment/projectKey（UTF-16 码元，astral 逐码元 ~XXXX）；SessionLogScanner（recoverable 首坏行吞、见 turn/end 即抛、admission 双类错误恒抛不吞、committedBytes 只进好行、撕裂尾 finish 忽略）。
- session-format v1-to-v2 codec：物理头键序（type, version, id, createdAt, [cwd], [parentSession], isSeeded, [origin], delegationDepth, [agentPreset]——v4 spread 原位覆盖 version）；信封 exactKeys + released v2 row 错误族 + seq gap 消息；seq-ranges 编解码全语义（run≥3 折叠、非严格递增原样返回）。
- v3-to-v4 全族：assertReleasedV4Header（五必需+四可选逻辑头）；assertV4RowAdmission（developer-data/source-row/retired-syntax/system-fields/tool-result/fork-result；ignorable developer/message 无词表时延后）；restoreReleasedV4Artifact（词表门 `format v4 contains unknown event type...` + 稠密 + cut/marker 对齐）；relationships 385 行状态机（turn/step 序、tool 生命周期、notStartedRepair 引用 interrupted notStarted 文案逐字、protected head、PTC dispatch 配对、compaction/retry/title/command 全分支）；**dsh 无尾平衡检查**（open tail = 崩溃恢复前缀合法）。
- catalog generated.ts：restoreCurrent = restoreReleasedV4Artifact(KNOWN) → Session.fromRestore——jsonl 读路径未知类型拒消息 = validation.ts 文案（切片 D refuse_unknown_required 消息选择正确）；storage-contract "newer harness" 文案在 jsonl 面死码（跨后端防御层），但其 request/header reason==='fallback' 拒绝 + adopt Corruption 包装可达（已移植，fallback 拒绝内联于 scanner.finish 原始行层——typed 枚举反序列化会在其前失败）。
- index.ts 物理段：materializePosix（mkdir 0700 逐级 + syncDir 逐级 + rejectExistingLog + tmp 12hex wx 0600 + link 发布 EEXIST→AlreadyExists + syncDir + 尽力 rm tmp）；appendLines（before size + write+fsync + 失败 truncate 回滚 + 回滚失败聚合文案）；repair（truncate+offset + fsync）；encodingMismatch/legacyLayout/rejectOppositeArtifact 文案逐字（opposite artifact 也用 encodingMismatch）。
- zstd.ts：帧扫描逐字节（magic 0xFD2FB528 LE / 描述符保留位 0x18 / blockType 3 拒 / RLE / checksum 4B / maxFrames / tornStart）；压缩 checksum 帧（唯一非默认参数）；撕裂前缀 = 流式解码出已刷盘 plaintext（Node ZSTD_e_flush 等价）。
- lease.ts：POSIX flock 非阻塞 + 锁后 inode/dev 复验（3 次）+ 锁文件永不删 + 无过期；win32 = 路径派生命名信号量（fr 未移植，Q39）。

### 实现落盘（crates/fr-session/**）
- **closer_text.rs**：scripts/extract-session-texts.py 自 dsh repair.ts 抽取生成（CLOSER_TEXT 四文案 + 两 code），禁手抄纪律落地；repair 与 relationships 共用单一真源。
- **repair.rs**：OpenTurnCloseCause（id_prefix/closer_text/turn_end_kind 三映射）+ ToolCallRecovery（IndexMap 保插入序）+ open_turn_closers + interrupted_turn_closers；SessionSeq::new Result 传播对齐 dsh SessionSeq() throw。
- **fork.rs**：build_fork_seed（越界 → SessionSeqRangeError；tagged end-seed time 复用切点事件）。
- **persist/**（八模块）：error（PersistError 五变体 + session_format_version_refusal）/filename（世代名 + 解析表 + 路径装配）/paths（encodeSegment/projectKey 逐字——astral 按 UTF-16 码元 ~D83D~DE00 与 dsh 逐字同型）/header_line（to_header_line serde_json Map 逐键插入序 + is_header_line 全守卫（null=present 拒）+ parse_header_record）/framing（seq-ranges + decode_event released v2 错误族 + 安全整数域含负 time）/v4_checks（行准入族 + assert_released_v4_header + restore_released_v4_artifact + relationships 状态机全分支；RELATIONSHIP_TYPES ∩ 词表参数化）/zstd（帧扫描 + compress/decompress checksum 帧 + 撕裂前缀 DCtx 流式）/lease（flock + inode 复验 + 锁文件永不删 + Drop 幂等）/io（create_dir_all_mode + sync_dir + materialize 全链 + append 回滚 + repair + encode_materialization 双帧）/read（SessionLogScanner recoverable/strict + 逐行号 + zstd 前缀读 + torn 分类 + 身份校验 + same_file realpath + resolve_generation_in_directory + read_stored_log 全链 + fallback 拒内联 finish 原始行层 + Session::from_seed 会话级采纳）。
- **plugin_shell.rs**：dougong-abi v2 导出表（name/descriptor/setup/call/on_event/on_next/teardown）+ descriptor（provide ["fr-session"]、inject []、listen {}）+ dispatch 纯函数（METHOD_DECLARE_EVENT_TYPES=0 declare、METHOD_DESCRIBE=1 describe）+ 词表 State（宿主按实例串行化，无锁）。
- **tests/plugin_shell_e2e.rs**：真实 cdylib 工件装载 E2E（cargo build 定位 libfr_session.so → dougong_dylib::load → consumer plugin 经 Invoker 声明 compaction 两类型 → 重复声明 fail-closed → describe knownTypes 14+2=16）——Q34A 激活期声明机制全链走通。
- crate 形态：crate-type ["cdylib","rlib"]；deps + dougong-abi/libc/zstd/zstd-safe；dev-deps + dougong/dougong-dylib/tokio；workspace deps 补 dougong-abi/dougong-dylib/libc/zstd/zstd-safe。测试 71（repair 6 + fork 3 + persist 面全套 + shell 3 + E2E 1）。

### 实现语义发现（钉测逮捕，记录在案）
- **relationships tool() 初版判定错误**：写成 `!started && !repair → throw`——dsh 语义是仅 `!started` 时要求 exact repair（started 的正常 result 直删）；测试（full chain tool/call 后正常 result）逮捕，修实现。
- fold_surface 对五类缺 surfaceOp 抛 'replacement start must be a non-negative safe integer'——relationships 层实质要求五类必带 marker（真实日志恒带；与 session 层 validateNext 的 marker 必带互为表里）。
- restore 门 cut-vs-marker 比较在 scanner 路径恒真（cut 由 marker 派生）；分歧场景 = cut 独立到达的 artifact/迁移面（测试以 cut=1 vs marker=2 钉死）。
- serde_json Value 相等：1（整数）≠ 1.0（浮点），JS isDeepStrictEqual 视为相等——影响 PTC dispatch 与 compaction shadowedSeqs 深比较的病态手改文件角落；dsh 侧 JSON.parse 往返后同样折叠，接受（记 19.2 候选附注，不单独呈报）。
- dsh scanner 层序：admission 双类错误恒抛（recoverable 不吞）、JSON parse 失败可吞、framing/seq-gap 按策略吞 + turn/end 升级；finish 无 issue 终检（recoverable 吞后 = committed 前缀 + tornTruncateTo，写路径截断）——全部逐条复刻。
- `-0` 语义：serde_json 解析 "-0" 归一 0（as_u64 Some(0)）——dsh isHeaderLine 显式拒 -0 的角落依赖 serde 数字归一，接受（createdAt: -0 病态输入，JSON 往返后即 0）。

### 遗留与呈报项（切片末集中呈）
- Q39 Windows 写租约：dsh 双实现（POSIX flock + win32 命名信号量 win32.ts 208 行）；fr M1 已实现 POSIX 侧，win32 侧未移植（本机 Linux 无法验证）——呈 A（M1 POSIX-only + 19.2 登记 + cfg(windows) 明示不支持的编译门）/B（立即移植 win32 信号量，不可测）。
- Q40 dsh 插件命名空间知识：relationships 的 validateDeliveryAccepted（'session-log-deepseek/delivery-accepted' 专检）与 title-llm 源 kind 字面 'dsh-session-title-llm'——前者 fr 侧结构性 omit（一切皆插件：他插件命名空间知识不入 fr-session 核心；DELIVERY_CHECK_OMITTED 文档化），后者逐字保留（M1 词表不含 session/title-llm-request，路径不可达；fr-session-title 插件落地时随 19.2 裁决品牌映射）——呈 19.2 登记确认。
- 存储运行时（Tracker/live 路由 200ms 批窗/句柄 promise 链/flushAll/memo LRU/list 发现面）不在本切片——属 store 服务接线切片（fr-agent-loop 宿主集成期）。
- SESSION 合约测试绑定：切片 C/D/E 均按既有模式交付钉测、选择器留 pending（绑定基础设施 bind-session-tests.py 属独立切片，同 CONFIG 模式）。

## 2026-10-23 — Q39A/Q40B 裁决落地 + Q41A 切片定向

- **Q39A**：PRD 19.2 条 15（Windows 写租约后置：M1 仅 POSIX flock 侧；win32 命名信号量随 M1 平台门逐字移植）+ persist/mod.rs `#[cfg(windows)] compile_error!` 明示编译门（fail-closed，非静默缺位）。
- **Q40B（否决推荐 A）**：恢复校验器知识范围按 1:1 优先——`validateDeliveryAccepted`（session-log-deepseek/delivery-accepted）逐字移植 + `catalogFact`（facts.ts:67-86 真值表）移植 + assertReleasedV4Relationships 前奏族（developer data + 全量 message-sources + delivery 所有权 + catalog 子 id 去重）补入 restore_released_v4_artifact——**顺带补上此前漏移的全量 assertV4MessageSources 检查**（此前只有行准入的 plugin-kind 变体）；title-llm 字面维持逐字。PRD 23 决策 29 载明（1:1 优先于知识归属，类型经词表声明后激活，与 dsh KNOWN 集同构）。
- 钉测 +1：delivery_ownership_pins_session_binding_and_payload（own 接受 / foreign session 拒 / throughSeq 不早于拒 / 空 id 拒 / 异代 sessionFormatVersion 忽略 / 未声明类型走词表门）。自纠一件：夹具缺 sessionFormatVersion: 4（缺省即 0 代被 dsh 语义忽略，foreign 场景须显式 v4）。
- 门禁：四关复跑全绿（workspace 225 tests，clippy 0，doc 0）+ 三门禁 0 errors。
- **Q41A**：下一切片 = fr-session store 服务接线（Tracker/句柄/live 200ms 批窗/flushAll/list 发现面），Phase 13 完整闭环。

## 2026-10-23 — Q42A' 裁决落地：切片 F 持久化运行时实现

- **Q42A' 裁决**（用户"A"）：同步 core + 写句柄 actor worker + 有界并行 list + Condvar 调度线程。Q38「listConcurrency 以 tokio 信号量等价」推定文字随之修订为「同步有界并行（std::thread::scope + Mutex+Condvar 信号量），默认 3 语义逐字」——PRD/19.2 无需改动（该推定仅存 findings）。
- **实现落点**（全部 crates/fr-session/src/persist/）：scheduler.rs（TimerTarget lazy 失效 armed 位 + Condvar/deadline 堆单线程，catch_unwind 防火）、handle.rs（Op 枚举 + per-handle worker 线程 `fr-session-handle-<id>` + 每-op mpsc 回执 ≡ dsh promise 链；CloseSlot 幂等共享结局 ≡ closing ??=；SharedReadState/缓冲小锁零 I/O 持锁）、tracker.rs（writers/pending/open_handles，uid 身份）、backend.rs（create/open 双臂 + stat/list + findLog/legacy 拒/编码守卫/版本门 + readStableBytes 一次重试截断前缀 + read_generation_header 首行读器）。
- **关键 1:1 发现（登记，Phase 15 必守）**：日志内 turn 编号 1 起——dsh relationships.ts `n = 1`（验证器首个 turn/start 必须为 turn 1），若 0 起则 dsh 无法恢复自身会话；fr 验证器 next_turn:1 同构已对。fr-agent-loop 发首个 turn/start 时 MUST 用 1。
- **极端钉测逮捕真 bug 一件**：create_dir_all_mode 的 `create_dir` 并发竞态下输家 EEXIST 直接上抛（10 并发写句柄测试必现）——dsh 用 Node `mkdir recursive` 天然容忍；已修为 EEXIST 容忍（mkdir -p 语义），5× 复跑稳定。
- **机械登记（内部结构不镜像 dsh，非行为偏差）**：JsonlHandleStorage 接缝内联为具体 Arc<JsonlBackend>（Rust 模块同 crate 无循环问题）；AbortSignal/eventState/coldLogMemo 不移植（Q42 已登记）；ensure_root_encoding 只 memo 确定性编码拒绝（transient I/O 重试更有益，dsh 全 memo 的偏差理由已注释）；HandleInner.uid（进程内计数器）作 tracker 记账身份。
- **钉测 +19**（backend.rs tests 模块，文案逐字）：round-trip/pending 可见性/duplicate 双拒/claim 拒/NotFound/contiguity/readonly 双拒/closed 先于参数/offset 域/单调缩水/torn zstd 帧修复/live 窗口 drain/drainPaused 暂停后 close 修复/版本门 v3+v5 双文案/legacy flat/10 并发写句柄/大会话库有界并行 list+坏工件跳过/新鲜度跨句柄/close 失败即结局且幂等。
- **门禁终态（实跑）**：fmt PASS / clippy --workspace --all-targets 0（35 条机械尾全数真修，零 allow）/ cargo test --workspace 275 passed 0 failed / doc 0 / SESSION lifecycle 75 passed 0 failed 27 skipped（基线保持）。余 27 pending 均依赖 agent-loop 触发/SessionStore 服务/prompt 投影，本切片无新绑定对象（bind-session-tests.py 无需扩表）。

## 待裁决问题登记（2026-10-23 呈 Q42：切片 F 并发载体）

**用户设计指令（2026-10-23，约束性）**：M1 必须把多会话并发、大会话库等极端情况纳入首日设计，禁止留待 M2 打补丁。据此 Q42 方案 A' 增补「极端优先」四项（原文见会话呈报，裁决后按此落地）：
1. worker 线程仅属写句柄（读句柄无线程——dsh read 不入链，storage.ts:118-156 实证；线程数 = 活跃写会话数）。
2. 有界并行 list **M1 即实现**（mapSessionDirectories 按 dsh listConcurrency=3 语义：std::thread::scope + Mutex+Condvar 信号量；项目先序、首错 = 输入序、项目失败后仍收齐目录结果再抛——index.ts:1095-1130 settle 语义逐字）；listConcurrency 成为后端真实字段。
3. live 缓冲无界 + drainPaused 失败保留重试 = dsh 原语义（storage.ts:89-93,294-316），登记为 1:1 事实不改动；整日志驻留内存（read_stored_log 全量读）同为 dsh 同构事实。
4. 跨进程安全已由切片 E 结构性覆盖（flock 租约 + materialize EEXIST 互斥 + 进程内 claim），无新增工作。
本切片增配极端钉测：10 并发写句柄交错 append（序列正确性 + 各句柄单调读）；大会话库有界并行 list（序与首错语义）；事件洪峰慢 drain（缓冲保留 + drainPaused 重试 + 后台失败回报）。
机制修正：A 案串行化骨架 = per-handle actor worker（dsh promise 链字面直译，Op 枚举 + 单消费者），Mutex 退为小共享单元（live 缓冲/primed/observed_length）专用；Condvar+deadline 堆调度线程扛 200ms 批窗；lock-free/io_uring/mio/rayon/ArcSwap 逐项裁决为不适用（fsync 毫秒主导、平台锁死、无行为增益），理由已呈会话。

事实：
- dsh 全 Promise：句柄串行 = per-handle promise 链（storage.ts:357-370 enqueueChain/run）；200ms 批窗 = setTimeout（storage.ts:36,277-281）；列表并发 = pLimit(3)（index.ts:91,328）；取消 = AbortSignal（写前 throwIfAborted）。
- fr-session 现状：全同步（std::fs + libc flock），tokio 仅 dev-deps（切片 E E2E 用）；workspace.dependencies 已有 tokio 1（rt-multi-thread/time/sync/fs 等）。PRD:107 宿主事件泵、fr-ptc、TUI 均 tokio，但域 crate 无一引入。
- SESSION 合约 102 场景无一钉 200ms/取消/并发（rg 实证零命中）；200ms 批窗为 Q38 物理推定项（未否决成立）；Q38 曾记「listConcurrency 以 tokio 信号量等价默认 3」的推定文字——选项 A 需修订该推定。
- dsh index.ts:225-231 自证：settleLimited 并发执行的结果与首错选取「matches a sequential loop whatever the completion order」——sequential loop 即语义参照，并发仅缩墙钟。

选项：
- A. 同步 core（推荐）：句柄/后端 API 全同步；Mutex 复现 promise 链串行（dsh 链持锁跨文件 I/O，同构）；200ms 批窗 = 后端私有调度线程（Condvar + deadline 堆，单线程）；list/listArtifacts 顺序执行（结果与首错逐项同果；listConcurrency 键的处置——接受惰性或暂不暴露——随步 3 config schema 接线再呈）；AbortSignal 不移植（同步 API 无取消通道；dsh 信号仅写前检查，fr 调用方不可取消即语义为「无取消」）；零新生产依赖；测试即时确定。sync core 与后续宿主 async 壳兼容。
- B. tokio async：句柄/后端 async fn；tokio 进 fr-session [dependencies]；timer = tokio::time::sleep；listConcurrency = Semaphore(3)（Q38 原文）；取消可留 CancellationToken。代价：域 crate 公共面全 async 化、每测试 #[tokio::test]、与现有同步 Session 模型层组合需桥接。

附带事实登记（非分叉，两案共通）：Rust 所有权使 eventState shared-frozen/深冻结无对应物——读结果恒为 owned 拷贝，SessionHandleReadResult.eventState 不移植（seam 内部形态，非 wire 协议）；coldLogMemo（generation.ts:255-285 readStableJsonlFile 一次重试 + 截断前缀语义保留，memo 缓存本身不移植——纯性能件零行为差）；迁移机械（prepared/publication/migrationPreparations/historicalCorpusRevision）按 PRD 19.1 排除，v≠4 一律 session_format_version_refusal。

裁决后动作：
- A：修订 Q38 推定登记 → persist/{handle,tracker,backend}.rs + assert_contiguous/created_at 快照校验 + PersistError 扩展（ReadOnly/HandleClosed/InvalidInput/Aggregate）→ 钉测（文案逐字）→ 绑定 SESSION 场景。
- B：tokio 进生产依赖后按 async 全家桶实现。

## 2026-10-23 — Phase 13 切片 F 真源全读（三）：persistence 运行时面（storage.ts 571 + handle.ts 117 + store.fork）

### storage.ts（句柄/追踪器/路由全文）
- LIVE_WRITE_BATCH_MAX_DELAY_MS=200；JsonlHandleStorage 接口（persistBatch/persistHeader/truncateTornTail/resolveCurrentLog/readStoredLog/hasPendingSession/acquireWriteLease/releaseHandle——fr 切片 E 物理层即此接缝的实现体）。
- StorageHandleState：cursor（stored next-seq）/materialized/tornTruncateTo/recoveredTail/inheritedEventCount/primed（write-open 的已验证前缀，供读直至首 append）。
- JsonlSessionHandle：per-handle promise 链串行（enqueueChain 吞错续链 / run 先 assertOpen 再链内再 assertOpen）；close 幂等不可取消 + 循环 drainLive 至缓冲空 + await chain + lease 释放 + releaseHandle，drain/释放双失败聚合（`session "<id>": close failed to drain and to release its write lock`）。
- read：closed 拒先于参数校验（SessionHandleClosedError）；offset/length safe-integer TypeError 文案逐字；primed（write 读 primed / read 读 primed 后遇 resolveCurrentLog 存在则弃 primed 重扫）；未物化 write → 空 detached；否则 resolveCurrentLog → readCurrent（单调视图：source.events.length < observedLength → `session "<id>": stored log shrank below a previously observed prefix (N < M)`）→ 无文件且 hasPending → 空 detached → 否则 NotFound。
- append：materializeAppendBatch 先于入链（校验值=持久值）；persistContiguous：ReadOnly 拒 → 空批短路 → ensureLease（create 句柄首次物化写前取锁，物化失败仍持锁至 close）→ assertContiguous（`append seq mismatch for "<id>": expected N at index I, got K`）→ tornTruncateTo 先修（truncate+清态，失败留态重试）→ recoveredTail 重写（>0 才写）→ persistBatch → materialized=true/cursor+=n/primed 清/observedLength=cursor。
- flush：ReadOnly 拒（`flush` op 名）；已物化短路；ensureLease + persistHeader（空会话显式 flush 存活）。
- live 路由：enqueueLive（structuredClone 入缓冲 + 闲时 arm 200ms 定时；drainPaused 时不再 arm）；drainLive 单飞（finally 清 draining）；drainBuffered（清定时器 + 清 drainPaused + while 循环链内 splice 全量批，失败 batch 前插回 + drainPaused=true + 抛）。
- JsonlBackendTracker：registerCreated（writers.has → SessionAlreadyExistsError；占位 null + pending 集 + revision `memory:<name>:<n>`）；claimWrite（SessionAlreadyOwnedError）；releaseClaim/pendingOf/hasPending/pendingEntries/materialized（物化后清 pending）；adopt（openHandles + write 绑 writers）；release（close 时 openHandles 删 + write 删 claim + 未物化删 pending = 从未存在）；flushAll（遍历 writers，drainLive+flush，SessionHandleClosedError 视为已 flush 跳过，余错误聚合 `<name> flush failed`）；install（effect：teardown 先注册后 LIFO——先关全部 openHandles 聚合 `<name> dispose failed`，再订阅 session/event→writers.get(id)?.enqueueLive（后台失败 warn `session-persistence: background write for session "<id>" failed (buffered events retained): ...`）、session/flush→drainLive+flush、session/disposed→close（失败仅 warn））。

### handle.ts seam（契约面全文）
- SessionAccess = 'read' | 'write'；SessionHandleReadResult {eventState, events}；SessionHandle 四操作（read(offset,length)/append/flush/close）+ AsyncDispose。
- 新鲜度承诺：append/flush 解析后同后端任何 handle/stat/list 后续读取至少见该前缀；并发读只承诺有效连续前缀；handle 读永不回退（单调）；撕裂尾永不返回。
- append best-effort：解析=接受+有序+本实例可见；仅 flush 承诺崩溃存活（fr 物理层 append 即 fsync，flush = materialize-if-needed——dsh jsonl 同形）。

### core/session store.fork（拒绝码族）
- SessionForkErrorCode：SESSION_NOT_FOUND（源 id 不在 live store）/ SESSION_NOT_LIVE（对象非 live 实例——fr 无对象身份语义，等价=store 中无该 id 的 live 会话）/ SESSION_ALREADY_EXISTS（子 id 已占）/ INVALID_BOUNDARY（非负安全整数 + < events.length + events[boundary].seq === boundary 三查，文案 `fork boundary for session "<id>" must be a non-negative safe integer, got ...` / `fork boundary N does not exist in session "<id>" (last seq: M|none)` / `fork boundary N does not match a contiguous event seq in session "<id>"`）。
- fork 流：子 id 查重 → resolve 源 → snapshotEvents → boundary 解析（缺省=末事件 seq；空会话+缺省 → 无种子）→ buildForkSeed → create(child, {seed, inheritedEventCount=resolved+1|0, meta:{cwd 透传, parentSession, isSeeded:true}})。
- store 生命周期（前轮已录）：prepare/enter/announce 三段事务、session/created 同步 throw 可否决回滚、session/event feed per-listener 容错、session/flush 并行等待抛首错。

### 切片 F 范围确认（Q41A 授权文字）
- 本切片 = 持久化运行时：JsonlSessionHandle + Tracker + live 路由 200ms + flushAll + stat/list 发现面 + create/open 流（findLog/ensureRootEncoding/legacy 拒）。
- SessionStore 内存层（prepare/enter/announce/fork/resume + 拒绝码）= 后续切片（依赖本切片句柄面）；ctx 事件（session/event|flush|disposed）在 fr = dougong 事件监听器（fr-session 服务内自持 event bus 或经内核事件——接线分叉留实现期呈报）。
- contract 层动作：bind-session-tests.py（CONFIG 模式）建立 + 切片 E 已交付测试绑定 SESSION 既有场景（v4 回环/非 v4 拒/zstd 回环/写路径校验失败不发布/fork 场景族）。

## 2026-10-23 — 切片 G 真源全读：store 四事件精确语义 + dougong 接线面事实（呈 Q43）

恢复基线：HEAD b77bac4（切片 F 已提交），工作树净。本轮为 SessionStore 内存层切片（切片 G）前置真源读；文件均在 `~/project/reference/cordis/deepseek-harness/packages/core/session/src/index.ts`（dsh）与 `~/project/dougong/crates/`（dougong）。

### dsh store 四事件语义（核心发现：不经内核派发模式，调用全部手工）
- **监听者解析与调用分离**：`collectSessionCallbacks = [...ctx.events.dispatch('emit', args)]`（index.ts:394-396）仅向 cordis 取监听者快照（含 scope 过滤，carrier = scopeTarget(session, scopeOf(ctx)) :1212）；四事件的调用全部手工实现，**不用任何 cordis 派发模式**。
- **session/created**（announce :1265-1293）：守卫 not live（`session "X" is not live in this store` :1338-1345）与重复（announced||announcing 双态拒 `session "X" was already announced` :1267-1269，含监听者内重入）；**mark-before-emit**（:1273 announced=true 先置——回滚仍配对 disposal 边，监听者不能递归造第二生命周期边）；**同步 throw 故意传播 = 否决**（:1279-1283），async 返回 promise 的 rejection 只 warn（`session "X": session/created listener rejected: ...` :1286）；否决回滚机制 = create 的生成器 effect 先 yield detach 再 announce（:1111-1122），throw 时 effect 释放已 yield 的 disposer → detach → emitDisposed（announced 已 true，配对边成立）；finally announcing=false + detachRequested 延迟兑现（:1289-1292）。
- **session/event**（Session.commit :876-897）：回调快照采集**先于 log.push**；逐监听者 containment（invokeContainedSessionObservers :398-416）：同步 throw → warn `session "X": session/event listener threw: ...`、async rejection → warn `... listener rejected: ...`、feed 继续；append 重入守卫（publicationEntry :862-868 `session append cannot reenter while another append is being published`）；detachRequested 于 append 展开后兑现（:891-895）。
- **session/disposed**（emitDisposed :1296-1304）：detachEntered（:1247-1256；stale capability 守卫 store.get(id)!==entry → no-op）删除 store+attachments 后**announced 才发**（含发布回滚；prepared 未 enter 或未 announce 的丢弃不发）；整段 try/catch warn `session "X": session/disposed dispatch threw: ...` + 逐监听者 containment。
- **session/flush**（store.flush :1319-1336）：**唯一 flush 入口**（"one owner and one spelling"；调用方 = checkpoint policy per-request 屏障 / goal-round-driver idle 检查点 / teardown 排干 / 读存储前自 flush 的消费方 :1307-1313）；快照 → Promise.allSettled（同步 throw 转 Promise.reject 保真 rejection 值）→ **全部落定后**抛注册序首个 rejected（:1333-1334）→ 返回 `callbacks.length > 0`（是否有持久化监听者参与）。
- **事务三段**：prepare（:1143-1186）id 缺省 mint `session-<n>` 计数器、重复 id throw `session "X" already exists`、meta 校验（lossless-JSON record + cwd 绝对路径）、header 构造（createdAt = meta.createdAt ?? Date.now()）；enter（:1210-1244）**authoritative collision boundary**（prepare 与 enter 间可插任意工作，重复再查 :1215 + attachments.has 拒 `session "X" is already attached to a store`）、返回单发 detach（entered 幂等；announcing/appending 中延迟为 detachRequested）；resume = prepare(id, {seed, eventState, inheritedEventCount, meta}) 同管道（Session.fromRestore）。

### dougong 接线面事实（源码实证）
- 五派发模式全 tokio async（events.rs:216-351）：emit = 每监听者 spawn、失败仅 log_error 永不传播（:216-237）；parallel = 并发全等待、失败聚合为一个 Error::aggregate（:244-273，非 dsh 的 allSettled 首败 + participated 返回）；serial/bail = 顺序、Err 传播、非 null 值 bail（:280-313）；waterfall = 续链、丢 Next 否决（:321-351）。监听者 = 注册 fiber 的 effect（register_listener :428-474，LIFO 撤销）。
- **无「只取快照不派发」公共面**：EventStore/begin_dispatch/Dispatch.entries 均 crate 内私有（events.rs:131-213）——插件侧无法复刻 dsh 的 resolve-then-manual-invoke 形态。
- **dylib ABI v2 纯被动拉取**（dougong-abi lib.rs:199-262）：导出表 {abi_version, name, descriptor, setup, call, on_event, on_next, teardown}，call/on_event/on_next 全部 host→plugin；**插件无对外服务调用、无 emit、无运行时监听注册**（监听 = descriptor listen 静态声明 → 宿主 ctx.on_dispatch 桥 dougong-dylib:450）；descriptor inject 仅进内核声明检查（DynPlugin::inject dougong-dylib:354-355），无消费通路；in-process 插件才有 ctx.emit/on 全 API（context.rs:479-554）。per-instance 锁串行化（ABI 文档 :229-231）。
- scope 面存在（scope.rs ScopeNode 链 + on_global + matching filter），M2+ 子代理隔离用，M1 单 realm 无过滤效果。

### M1 消费方盘点（dsh 监听者实证）
- 本 crate：persistence tracker（session/event→enqueueLive、session/flush→drain+flush、session/disposed→close——storage.ts install）；切片 F tracker 已有对应方法面，缺的只是 store→tracker 接线。
- 跨插件（Phase 15-18）：agent-loop（agent.ts:334 stopRecovery、runtime-context.ts:134、index.ts:481 session/disposed checkReleased）→ fr-agent-loop；compaction-basic:184 → fr-compaction；terminal-bash:53 → fr-shell；agent-instructions:326 → fr-system-prompt 域；headless json-stream → fr-headless `--json` 事件流（M1 验收第 2 条需要明确投递面）。

### 架构预告登记（非本问裁决项）
Phase 15 fr-agent-loop 为 dylib，需在循环内消费 llm/tools/session 服务——当前 ABI 无对外调用通路，与 Q43 同根。裁 B 则 ABI 升级设计应一并覆盖「对外调用 + emit」两通路；裁 A/C 则 Phase 15 动工前需就消费通路单独呈报。

### Q43（ctx 事件接线形态）——裁决记录待补
- A（推荐）：内部精确总线（dsh 四语义逐字同步落地于 SessionStore）+ tracker 同 crate 直订 + fr-session 服务增事件出站面（per-session 单调 cursor + drain），宿主事件泵拉取后经内核 Context emit 镜像投递 `session/event|created|flush|disposed`（载荷 {sessionId, event} JSON），Phase 15-18 插件经 descriptor listen 接收。零 dougong 改动；跨插件投递为异步镜像（持久化正确性全在 in-crate 同步路径，跨插件监听者本属 advisory）。
- B：内部总线同 A；内核投递经 dougong ABI 升级直发（v3 对外通路：host 上下文 vtable 或 call CPS 化）。免宿主轮询；代价 = ABI 破坏性升级 + 全插件重编译 + 设计周期；agent-loop 消费通路可一并解决。
- C：M1 纯内部总线不设投递面，Phase 15-18 各自解决。偏离 dsh 架构位置（事件总线是跨插件解耦本体），--json 流返工风险最高，不推荐。

## 2026-10-23 — agent-loop 出站消费问题详析 + Q44 登记（用户明示：不能延后，全系统影响）

### 规范级互斥（三处现行文字 vs ABI 事实）
- dougong-abi v2 纯被动拉取（lib.rs:233-262：call/on_event/on_next 全 host→plugin；无 import/vtable/回调表 → 插件无法发起服务调用、emit、运行时注册监听；inject 仅声明检查 dougong-dylib:354-355）；dylib 无 fiber 体——激活 = 宿主驱动 setup/descriptor，不存在插件侧异步任务体。
- PRD 4.3（:112-121）第一方全 dylib × PRD 8.1（:336）「每会话一个循环插件 fiber，置于该会话 realm；循环实现仅存在于 fr-agent-loop 工件，宿主代码不含循环逻辑」× 上述 ABI 事实 = **「循环插件 fiber」在 ABI v2 下不可存在**。PRD 4.5（:130）「Invoker 消费统一经 ctx.require」是 in-process API，预设消费者持 Context——dylib 没有。须 PRD 修订/19.2 登记 + 裁决执行。

### 循环的结构需求（dsh agent-loop + PRD Phase 15 清单）
- 长时不归控制流：inbox claim → turn/start → step 循环（deriveMessages 组装 → llm 流式 generate → 流结算 → append assistant/message → 工具提案 → 分组调度执行（exclusive 屏障/parallel 池；pre-execute waterfall 交 approval）→ append tool/result → 无 tool-call 终止）→ turn/end；状态机三相位、abort 两级补偿、steer/inject/followup 于 step 边界入 inbox。
- 每 turn 出站调用面：llm（流式）、system-prompt 组装、session（append/derive/flush）、tools execute（扇出全部工具插件 + approval + sandbox）、compaction（shadow llm + replace-op）、approval/llm-retry 的 session append。中断入站：steer mid-turn、abort mid-stream。

### 传染性盘点（不止 agent-loop）
除叶子提供者（provider/工具实现）外几乎全体插件都是消费者：fr-tools（工具插件 + approval + sandbox）、fr-approval（session append）、fr-compaction（llm + session）、fr-headless（agent + session 事件流）、llm-retry/todo/title（session append）。**出站通路 = 全系统血管系统**；agent-loop 只是要求最高的消费者。dsh 原生具备（进程内 TS）。

### 决定性单点事实：per-instance 锁
「宿主经 per-instance 锁串行化共享同一 handle 的一切外来调用」（dougong-abi:229-231）→ 循环若占住一次 call() 跑完整 turn，steer/abort 将被锁阻塞至 turn 结束——**朴素阻塞模型被锁死：任何方案下循环不能占住单个 call()**。

### 机制三族（Q44 呈报，与 Q43 同轮集中裁决）
- 44-A 宿主驱动 CPS 状态机：call 返回 DEFERRED + 挂起出站请求，宿主解析后经 on_next 类通路投回（D14 事件 CPS 模式移植到 call——call 契约扩展，ABI 小改）。零线程；宿主持有步进交错点（steer/abort 天然）；代价 = 循环逻辑手工状态机化（Phase 15 全清单控制流显式 continuation 化），每 await 点 2 次 FFI+JSON。
- 44-B 插件自持线程 + 宿主织物中继（推荐）：call(START) 起自有 OS 线程跑自然阻塞控制流（Q42A' 同款风格）；出站（请求+事件）入队，eventfd 唤醒宿主泵（fd 经 INIT 调用载荷传递，走现有 ABI 零改动）；宿主织物 = RPC fabric：请求信封（fr-wire）、按声明 inject 权限校验、回复经 call(POST) 投回、有界解析池（防叶子长调用——如 bash 工具秒级 call——阻塞泵）。通用规则 = orchestrator 插件=线程+队列，leaf=被动；steer/abort = 入站队列 = 任意队列读取点。代价 = 宿主织物复杂度、每边泵往返延迟、PRD「fiber」措辞 19.2 登记。
- 44-C ABI v3 一等公民对外通路：host vtable 或 call CPS 全面升级。架构最正；代价 = vtable SAFETY 契约新立 + 重入语义（call 内 require 回自身）+ per-instance 锁重设计 + 全工件重编译 + M1 排期与 dougong 设计周期耦合。

### 与 Q43 的关系
Q43-A（内部精确总线 + 宿主泵事件中继）= 织物的事件车道，与 44-A/B/C 全兼容且可独立先行；Q43-B（emit 直发）预设 44-C。事件四语义在全部方案下均留在 fr-session 内部注册表（Q43 §0 结论不变）。

### 用户指令（2026-10-23）
「这个不能再议 这个对系统有很大影响」——Q43.1 原「Phase 15 前再议」选项撤回；织物裁决（Q44）升格为 now-decision，与 Q43 同轮集中呈报。

### Q44 衍生产物：dougong 升级需求文档（2026-10-23，用户追加两条件后成稿）
- 用户指令链：「按你推荐的来是否还要修改dougong」（答：推荐组合零内核改动）→「如果使用44c方案 需要干什么」→「先对dougong的升级生成详细的修改需求文档 requirements.md」→「务必给我完整需求，我不接受改完后你又要增加需求」→「必须详细到清空上下文仍能理解不误解不丢失」。
- 产出：`~/project/dougong/doc/requirements.md`（444 行，§1-§9）。结构 = 背景（现状/缺口/锁语义）+ 目标/非目标 + 功能需求六域 A-F（vtable 契约/require/emit/版本迁移/测试矩阵 11 条/文档）+ §4 冷读者自足入口（术语表/fr 缺口叙事/三路径对照/入口×出站交互矩阵）+ §5 设计分叉 D-V3-1..5（含接口草图 + 宿主 require 五步规定动作）+ §6 文件级实施分解 + §7 fr 衔接 + §8 风险 + §9 变更控制（推导方法五条 + 后补需求一律 v3.1 独立提案）。
- 分析期实质修正一处：重入策略原「全禁令案」会禁掉合法 service→service 链（fr 审批 ask 处理器内 require session 落账）——改为「同线程重入环检测」（b'）为倾向案，A→B→A 回环转确定性错误非死锁。
- 补全期新钉事实：再激活语义（plugin.rs:66-67，vtable 必须 per-activation 构造失效）；setup 期 require 合法性 = Q34A 激活期声明的 dylib 完成件（B5/G5）；emit 载荷必须 JSON 而 require 载荷不透明的显式非对称（C2/B6）；内核本体预计零改动（桥全走公共面 ctx.get + events::emit，5.2）。
- 待裁决：D-V3-1（vtable 传递）/D-V3-3（重入策略）/D-V3-4（执行器）/D-V3-5（错误模型）——维护者裁决后冻结定稿；fr 侧 44-B 先行 vs 等 v3 的排期问题仍开放（文档 §4.2/§7 两轨并存）。

### dougong requirements.md v2 重构（2026-10-23，用户三连纠正后定稿待裁）
- 用户纠正三连：①「重要备注写在很远的另外一处」→ 全文重构为**主题集中**原则：一个主题一个自足块（接口草图入 2.1、重入三案+死锁事实+实现五步入 2.2、触点清单入 2.4、矩阵入 2.0），零跨节追读；②排除清单被读成路线图 → 唯一决定声明；③「M2+ 大概率需要 = 违反一次性提明确」→ **第 9 项事件作用域过滤升格结构性排除**：作用域/隔离语义归应用层（会话服务自持注册表+发射策略承载隔离，与四事件手工调用同原理），内核事件道为广播道；fr M2+ 子代理隔离由应用层满足——**此决定取代本 findings 早前「M2+ 如需 scoped 投递属独立内核提案」的表述**。九项全部结构性排除，文档无条件项。
- 产出：`~/project/dougong/doc/requirements.md` v2（342 行，§1 背景/术语/目标 → §2 功能需求 A/B/C/D 四块全部内联 → §3 测试矩阵 11 场景 → §4 待裁决 D-V3-1/3/4/5 → §5 范围外九项绝对 → §6 文档同步 → §7 风险与变更控制）。
- 待裁决：D-V3-1（vtable 传递，倾向 setup 携带）/ D-V3-3（重入，倾向 b' 同线程环检测）/ D-V3-4（执行器，实现期基准）/ D-V3-5（错误模型，倾向状态码+详情）。fr 侧 44-B 先行 vs 等 v3 排期问题仍开放。

## 2026-10-23 — Phase 13 切片 G 落地：SessionStore 内存层（fr-session 129 tests，四关全绿）

- **实现落点**：`crates/fr-session/src/store.rs`（~950 行含 10 钉测）+ session.rs 两处小增（`header()` 访问器原已存在、`from_seed_parts(header, seed, inherited_event_count)` 承载 dsh `inheritedEventCount`——fork cut 持久化字段此前只有私有 0 值）。lib.rs 导出 SessionStore/DetachHandle/SessionMeta/SubscriberId。
- **dsh 语义逐字落地**（core/session index.ts:1018-1345，本会话真源全读节为准）：prepare（`session-<n>` 计数 mint 跳活、重复 `session "X" already exists`、cwd 绝对路径校验——文案取 JSDoc 描述措辞 `meta.cwd must be an absolute path`，dsh 未捕获运行时原文已注明）；enter（authoritative collision 二查 + 单发 DetachHandle）；create 三段事务（announce 否决 → detach 回滚 → announced 已 mark → 配对 disposal 边，index.ts:1111-1122）；announce（mark-before-emit :1273、announcing 窗口、`was already announced` 双态拒、created 监听者 Err 传播 = 否决）；event/disposed feed（快照先于提交、逐监听者 containment、warn 文案 `session "X": session/event listener threw: …` / `session/disposed listener threw: …`）；flush（唯一入口、全落定后抛**注册序首个**失败、返回 participated，:1319-1336）；fork（拒绝族全文：`fork boundary N does not exist in session "X" (last seq: M|none)` / `does not match a contiguous event seq`、子头 cwd 透传 + parentSession + isSeeded、inherited=boundary+1、空源缺省=无种子、SESSION_ALREADY_EXISTS 文案同 create）；deferred detach（announcing/appending 窗口内置 detachRequested，提交展开后兑现 :891-895）。
- **结构决定**：监听者注册表四列表（created/event/flush/disposed，注册序即 flush 首败序）；监听签名同步闭包（tracker 形状）——dsh async-rejection warn 分支（`listener rejected:`）无同步对应物，结构性缺席已记录；`attachments` 对象身份表无 Rust 对应（move 语义）→ 双重 attach 抛错结构性不可达；append 重入抛错在 &mut 下不可达，保留防御性检查与逐字文案；fork 边界非整数/负数臂在 u64 类型下不可达。clock 与 warn 均注入缝（测试确定性）。
- **门禁（实跑）**：四关全绿（workspace 286 tests / fmt PASS / clippy --workspace --all-targets -D warnings 0 / doc 0）；lifecycle SESSION = 75 passed / 0 failed / 27 skipped 基线逐位保持；guard 后台运行（阻断期 20 合约未过预期）。
- **clippy 尾修 8 条真修零 allow**：AnnounceState 三态枚举替换 4-bool（struct_excessive_bools）、SessionStore 手写 Debug（missing-debug-implementations）、fork 早退分支重构消 redundant_clone + single-match、flush let-chain + map_or、模块文档首段拆分。
- **余项（切片 H）**：plugin 服务面（create/append/flush/fork/resume/restore 方法号 + fr-wire 载荷）+ tracker 装配（store.subscribe ↔ tracker.enqueue/close）+ 出站 drain 面（Q43-A，宿主泵中继）+ bind-session-tests.py 绑定基础设施。27 skipped 场景中 agent-loop/prompt/title/approval/ptc 依赖项属 Phase 15-18。

## 2026-10-24 — 切片 H 落地：plugin 服务面 + tracker 装配 + Q43-A drain 面 + 绑定收口

恢复基线：HEAD 91a1ca3（切片 G 已提交），工作树净。用户指令「继续完成切片m」判定为切片 H（Phase 13 唯一余项）。

### 交付结构
- **store.rs 事务变体**：`create_with`/`fork_with`（prepare → persist 挂钩 → enter → announce；挂钩失败在任何生命周期边之前返回，零残留）；`create`/`fork` 退化为无挂钩委托；`fork_with` 的 childId 为 Option（缺省 = store 计数器铸造，dsh `create(undefined)` 同形）。挂钩位置 = dsh `createStoredSession`（prepare 之后、enter 之前）——veto 经 detach → 配对 disposal 边回滚。
- **persist 路由面**：tracker `writer(id)`（dsh `writers.get`）；backend `route_live`（enqueue_live，缺写柄 no-op）、`flush_session`（drain+flush，HandleClosed 记为已 flush 与 flush_all 同判）、`close_session`、`append_suffix`（dsh `appendUnstoredSuffix` 缝）。
- **service.rs（新，~1200 行含 11 钉测）**：`SessionService` = store + backend + 共享 announced 集 + 出站镜像日志。四监听者装配（持久化 flush 先注册——注册序决定 store 首败；镜像后注册）：
  - session/event：**announced 门控路由**——dsh 发布前 append 不经 session/event 复现（否则 veto 的 create 会把缓冲 drain 进物化文件，违反无残留契约；镜像记录不门控，全事件镜像）；
  - session/flush：持久化 drain+flush（Err 传播 = 首败序）+ 镜像；
  - session/disposed：去标记 + 镜像 + close（close 失败 = 监听者 Err → store containment warn，dsh 仅-warn 处置）；
  - session/created：标记 + 镜像（payload 仅 {sessionId}，头经 stat/open 面；dsh 对象载荷无 wire 对应，M1 无 title 类消费者）。
- **方法号**：2 create / 3 append / 4 flush / 5 fork / 6 resume / 7 restore / 8 drain（0/1 = 壳 declare/describe）。
- **append 路由**（dsh `append(type, data, opts?)` 逐字）：plugin: 语法 → append_record（携带 surface 元数据拒绝，fr 自撰文案——dsh 该路径无可达形状）；surfaceOp 有 → append_surface；无 → append_log_only（`requires a surfaceOp marker` / `cannot carry surfaceOp` 文案由 session.rs 既有检查逐字产出）；sourceEventSeqs 无 surfaceOp 拒绝（fr 自撰，TS 重载对不可表达）。time = 注入 clock（dsh Date.now() 同为内部赋值）。回复 = 提交后事件全文（dsh append 返回事件同形）。
- **resume**（dsh agent-loop resumeWith 会话级移植）：open write 先行（并发 resume 互斥）→ 冷读 → `repair::interrupted_turn_closers` → 经句柄 append closers → prepare(seed, meta=盘上头, inherited) → 发布；失败路径 close 吞错（dsh 回滚吞 disposal rejection）。二次 resume = AlreadyOwned（open 先于 prepare，dsh 同序）。
- **restore**：内存重建（payload 种子 + 发布，零持久化交互）；与 create 的差别 = 不挂持久化。
- **Q43-A drain 面**：per-session 单调 cursor 镜像日志（created/event/flush/disposed 四类，全局 push 序）；drain 破坏式（≤ floor 丢弃），回复带下一游标（丢弃与已投递一并推进——首版实现漏计已投递，测试逮住修正）；advisory 车道 + 单消费者（泵）契约入模块文档。缓冲无界 = dsh live 缓冲同款取舍。
- **fr-wire**：`SessionEventData::from_parts(type, data)` 公开（信封解码器的 data 分派复用）。
- **seed 后缀同步**：seed 重放不经 session/event（dsh 事实）→ 落盘头无前缀行，读门拒（测试逮住）→ create/fork 成功后 `sync_unstored_suffix` 经写柄补写全部事件（dsh appendUnstoredSuffix 发布点）；放在发布边之后 = veto 保持无残留（比 dsh 时序更干净，dsh 的窗口是调用方避免的疣）。
- **绑定**：backend 补钉测 `filename_header_version_mismatch_refuses`（filename v4 + 头 v5 → selection 先读头，version gate 的 newer-harness 文案先于 mismatch 分支——实测钉死）；bind-session-tests.py 增 1 对，lifecycle 76/0/26（+1 诚实转正）。

### 真源事实（本轮新增）
- turn 编号 1 起（读侧 v4 生命周期门强制；append 侧 admission 不查 turn 连续性——测试用 turn 0 时 append 成功、落盘读回被拒。**登记待裁决**：dsh 在 append 站点失败（"a bad event fails at the append site"），fr 是否需要在 append 侧补 turn 连续性检查 = 契约级问题，未擅动）。
- dsh 会话编排（agent-loop index.ts:692/860）：create = prepare → persistence.create(header)（发布前取写权）→ setup 窗口 → appendUnstoredSuffix → publish；resume = open write → 冷读 → closers → prepare → suffix → publish。store.fork 本身不接持久化（生产调用方走 agent 种子路径）；fr 服务面 fork 挂钩 backend.create 取更优时序（veto 零残留）。
- dsh `KnownMessage::User` wire 形 = {role:"user", id, content, source:{kind:"user"}}（id/source 必需）。

### 门禁（实跑）
四关全绿：fmt PASS / clippy --workspace --all-targets -D warnings 0 / test --workspace 299 passed 0 failed / doc -D 0；三道需求门禁 exit 0；lifecycle SESSION 76/0/26；guard 阻断期预期（20 合约）。
### 显式缓议（非本切片砍除）
- dylib 壳未接服务面：ABI v2 setup() 无配置通道，root/vocabulary 注入属 Q44-B fabric INIT/宿主装配器（Phase 18）职权；本切片交付纯 dispatch + 运行时（测试经纯函数面），壳保持 vocabulary 面不变。

## 2026-10-24 — /code-review high --fix（审核对象 = 整个 fr-schema 模块，后台代理执行 + 本会话独立复验）

恢复基线：HEAD 3c2aa15（切片 H + fr-session 评审修复 + B 案已入库），工作树净。评审范围 = crates/fr-schema/src 全部 4 文件（2463 行）；dsh 权威参照 config-schema/{collect,document,projector}.ts + dump-config-schema.ts，fr-config 调用方（dump.rs/patch.rs）交叉核对。子代理用量 0/2（上限内自选内联角度）。

### 修复清单（4 项全修 + 1 项部分修，工作区 2 文件 +30/-28）
1. dump.rs `project_definitions` 投影缓存键去插件名、仅按 `&Schema` 指针（原 (指针, 名) 二元组）——dsh document.ts:70 `projections = new Map<NativeConfigSchema, ...>` 按声明节点作键（本会话补核属实）；两插件别名共享同一声明此前会发两份相同 configN 并扰动后续编号，现共享声明只投影一次。
2. dump.rs `js_truthy` NaN 改 falsy，对齐 JS `row.group &&` 短路语义（YAML `.nan` 组此前误真值，带序列配置的行会误入子目标清除族）；+.nan/.inf 两钉测（既有测试函数内扩展）。
3. dump.rs helper_pins 死断言删除（断言字面空 Vec 为空）+ 失去用途的 Collected 测试导入清理。
4. schema.rs `insert_i64_bounds`/`insert_f64_bounds` 合并为泛型 `insert_bounds<T: Into<Value>>`。
5. schema.rs `Property::name` 唯一性契约补文档注释（部分修复；行为面残留 = Q46）。

### 待裁决问题登记（2026-10-24 呈 Q45-Q47）
- **Q45（中）null 省略回退不对称**。fr 侧三面互斥（全部本会话补核）：`validate` 每节点拒 `Value::Null`（schema.rs:110-175，各变体经 as_str/as_f64/as_object None 臂，无可选放行旁路）；投影面对非必需节点发 `type:["T","null"]`（schema.rs:379-393，Q27A 钉住 dsh config0 形态）；dump $comment 自述 "validates a fallback on null/omission"（dump.rs:513）。可达路径：patch.rs:459/484 省略 config 的行以 Null 进 :562-563 校验槽（本会话补核）——插件声明 schema 后该行 violations 进 failures 被剔。dsh 侧事实（本会话补核）：dump 路径 collect.ts:203-215 只挂接 native schema 供投影、不引导校验行 config 值；行值的省略/null 消解属 schemastery 运行时（应用默认值放行——评审代理引证，未逐行追 schemastery）。
  - **45A 放行**：validate 对非必需节点放行 Null（即 $comment 已自述的意图面；与投影 nullable、dsh「省略即回退」三方一致；required 节点仍拒）。
  - **45B 维持 fail-closed**：PRD 19.2 登记偏差 + dump.rs:513 $comment 措辞修正（当前文案与行为相悖）。
- **Q46（低）重复 Property 名双面分歧**：`Schema::Object` 同名两条声明 → validate 两条都跑（交集语义）vs 投影 last-wins 且 required 数组可重复输出。dsh 对象字面量不可能重复键，该状态 fr 独有。
  - 46A 构造期拒绝重复名（fail-closed，声明面 API 收紧）；46B 双面统一 last-wins（validate 只认最后一条）；46C 维持现状（文档注释已载明，接受冷僻输入下两派生面分歧）。
- **Q47（低）非有限默认值**：`Schema::Number` 默认 NaN/inf 经 `Value::from(f64)` 静默投影 `"default": null`，`check_f64_range` 对 NaN 双过（两向比较均假）→ 违反「默认值必满足自身声明」自洽（验证通过、投影变 null）。
  - 47A 构造/校验期拒绝非有限默认；47B 登记 19.2 缓议（评审代理判 M1 过于冷僻，仅报告未动）。

### 门禁与复验（本会话实跑）
四关全绿：fmt PASS / clippy --workspace -D warnings 0 / test --workspace 305 passed 0 failed（fr 11 + fr-config 88 + fr-schema 29 + fr-session 148 + E2E 2 + fr-wire 27；fr-schema 与基线同量）/ doc PASS。真源补核：document.ts:70、collect.ts:203-215、patch.rs:459/484/562-563、schema.rs:110-175/379-393、dump.rs:513。
- 自纠一件：首轮 clippy 命令带 `| tail -5` 吞退出码（progress.md 留档过的同款坑，本轮复踩），识别后净命令重跑。
- 工作区 2 文件未提交（crates/fr-schema/src/dump.rs + schema.rs），待用户验收提交。

## 2026-10-24 — 用户裁决 Q45A/Q46A/Q47A 落地：validate null 回退口径收口 + 声明面构造纪律

- 裁决执行：45A（validate 非必需节点放行 null 视同省略走默认，必需位拒绝，镜像投影 nullable/`not:{type:'null'}`——PRD 决策 26 预留项收口）；46A（`Properties` 构造期拒绝重复属性名）；47A（`Finite` 构造期拒绝非有限 Number 默认）。
- 实现：schema.rs 新增 `DeclarationError`（Result 非 panic——M-PANIC-ON-BUG：声明是插件作者输入非编程错误）+ `Finite(f64)`（TryFrom<f64> 拒 NaN/±inf；From<Finite> for Value 保 finite 恒投影数字）+ `Properties(Vec<Property>)`（TryFrom 拒重名；Deref<Target=[Property]>）；`Schema::Number.default: Option<Finite>`、`Schema::Object.properties: Properties`；`validate_value` 增 required 管道（根/items/dict 值 = false、Object 属性 = property.required——与 project_core 投影管道逐位镜像）+ null 早退分支；lib.rs 再导出三类型。
- fr-config 集成钉测：patch.rs `null_config_falls_back_at_schema_slot`（省略 config 行 → Value::Null → 槽内放行、行正常装载进 loader doc）；`plugin_schema_fixture` 改 Properties 构造。
- 测试自纠两件（实现正确、测试 authored 错）：①null_falls_back 钉测夹具漏带必需键 model——"missing required key" 是必需键缺席的正当拒绝非实现缺陷，夹具补 model；②非有限默认投影断言误索引 `["anyOf"][0]["default"]`——wrap_value_positions 把 default 注解提升至包裹位顶层（projection_integer_and_number_bounds 钉测形态即证），改 `["default"]`。
- clippy 两处机械修：Display::fmt 参数名回 trait 默认 `f`（renamed_function_params）；From<Finite> impl 内 `Value::from` → `Self::from`（use_self）。
- 文档面：PRD 6.3 validate 括注补 null 回退面与构造纪律 + 23 决策 32；无 19.2 偏差（45A = dsh schemastery「省略即默认」同构对齐，46A/47A = fr 内部纪律）；dump $comment 与 CONFIG 场景 240 投影全脸文本零改动（45A 使 validate 与二者一致而非相悖）。
- 门禁（本会话实跑）：fmt PASS / clippy --workspace --all-targets -D warnings 0 / test --workspace 309 passed 0 failed（fr-schema 32 = 29+3，fr-config 89 = 88+1，余与基线同量）/ RUSTDOCFLAGS -D warnings doc PASS；CONFIG lifecycle 55 passed / 0 failed / 259 skipped 与基线逐位一致（合约零改动）；guard 未复跑（阻断期常态，20 份 pending 合约未变动）。
- 残留观察（未行动，47A 裁决未扩围）：Number min/max 界与 check_f64_range 对 NaN 的惰性（NaN 界恒不触发、NaN 配置值双过）——属 bound/value 面；dsh JS 同为 `typeof NaN === 'number'` 且比较恒假，疑为 1:1 同款，若需收口须另呈裁决。
- 工作区 5 文件未提交（fr-schema schema.rs/lib.rs、fr-config patch.rs、doc/prd.md、规划留档），待用户验收提交。

## 2026-10-24 — Q51A/Q52A 裁决落地前置真源审计（patch 装载诊断文案 + from-default-profile 库层守卫）

- dsh reader 族全文（app-boot index.ts:520-590）：loadOptionalPatches 读失败 `${binName}: failed to read patches ${file}: ${String(error)}`（:526）、label='patches'；loadOverlayPatches 读失败 `${binName}: failed to read overlay ${file}: ...`（:545）、label='overlay'；parsePatchList 三连（:582/:585/:589）：`failed to parse {label} {file}` / `{label} {file} must be a top-level YAML array of loader patch entries` / `{label} entry {N} in {file} must be a mapping (a loader patch entry)`（N 1-based）。dsh 消费路径 label 实证：--patch overlays、bundle patch 文件与 profile 用户层走 loadOverlayPatches（profile.ts:763/771、plugin-manager index.ts:626/726）；家目录层与 profile-context 上下文层走 loadOptionalPatches（profile-context.ts:77-78、plugin-manager patch.ts:40）。
- fr dump 路径已对齐（dump.rs parse_raw_patch_list/load_raw_optional/load_raw_overlay，bin+label 已穿参，三连同文）；boot 侧（stack.rs/patch.rs）即 finding 4 缺口：stack.rs:267/:285 读失败缺 bin 前缀、parse_patch_lines 三处文案 fr 自造且无 bin/label/file。
- fr 内 label 约定（沿 dump 侧既定选择）：内嵌 bundle 资产与可选/家目录层 = patches（Path::new(bundle 名) 充 file 位，collect_dump_layers 同款）；--patch 层 = overlay。dsh boot 读 bundle patch 文件实为 overlay 标签——fr 内嵌资产无文件身份且 boot/dump 两侧必须同文案，定性记 PRD 决策 34（不可达内部面）。
- Display 裸化：error.rs Io/Profile/PatchParse 三变体 thiserror 前缀（config io error:/profile error:/patch parse error:）系早期脚手架，令 stderr 成双前缀（`config io error: fr: failed to read ...`），dump 侧钉测 contains() 形态掩盖未暴露——dsh 抛错文本即 stderr 整行，三变体改 #[error("{0}")]（EnvBootstrap 裸面先例）；PatchLine 行包装保留（fr 自有校验面）。rg 全库零前缀钉位测试。
- Q51A 前提复核成立：fr 宿主 main.rs:148-157 镜像 dsh resolveBoot 强制 --profile（`error: --profile <name> is required`），dsh 同样先拒——「from-default-profile 而 profile=None」两侧同为不可达态，库层 fail-closed 无 1:1 冲突；文案 `--from-default-profile requires --profile` 为 fr 自有面（PRD 决策 34）。
- Q53 登记（未行动待裁）：manifest 族文案偏离同源——stack.rs:154 `failed to read {path}` vs dsh profile.ts:617 `${binName}: failed to read profile manifest ${path}`；:159 `{path} must be a mapping` vs dsh :622 `${binName}: profile manifest ${path} must hold a JSON object`；:108 `invalid profile name {name:?}` vs dsh :173 `dsh: invalid profile name ${JSON.stringify(name)}`（Debug vs JSON.stringify + bin 前缀）；:361 `profile {name:?} does not exist` vs dsh :798 `${bin}: profile ... does not exist; create it with 'dsh plugin --profile ...'`（fr M1 无 plugin add 命令，补救后缀不移植待裁）；JSON.parse 失败 dsh 裸抛 SyntaxError vs fr :156 包装。本轮 Display 裸化使这些文案失去前缀包装，内容差异留给 Q53 裁决。
