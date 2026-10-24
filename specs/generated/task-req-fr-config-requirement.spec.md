spec: task
name: "装配与配置系统"
tags: [requirements, generated-draft, config, assembly]
satisfies: [REQ-FR-CONFIG]
---

## Intent

dsh 依赖 cordis 原生分层补丁能力装配插件；dougong loader 只接受整份 JSON 文档，无分层、行覆盖、YAML 概念。fly-rafter 必须在 fr 层重建分层装配语义，同时保持 dougong reconcile 的幂等收敛与失败不阻塞语义。

## Decisions

- Generated draft from KLL requirement artifact; human review must confirm boundaries and test selectors before implementation.
- fr-config MUST 解析 fr.patch.yml 有序分层装配（自底向上：入口默认 bundle → profile bundle 栈 → profile patch → 家目录 patch → `--patch` 命令行补丁），合并语义 MUST 对齐 dsh `applyEntryPatches`（2026-10-22 Q1/Q2 裁决，vendor/include/src/index.ts:57-127）：行内 id/name 为匹配器，其余键**逐键覆盖**（config 整键替换不深合并；只携 disabled 的覆盖行仅翻转 disabled）；覆盖行未命中 id 或 name 不匹配 → 警告跳过（不致命、不进 failures）；行序 = 首次插入序；`insert` 行 MUST 向装配根插入新行（fr 不做 group 嵌套），后续层 MUST 能覆盖先前层插入的行。装载文件布局 MUST（2026-10-22 Q1/Q2/Q3 裁决，对齐 dsh app-boot profile.ts / profile-context.ts / index.ts）：profile = `<FR_HOME>/profiles/<name>/`，名字校验拒绝空名、含 `/` 或 `\`、`.`、`..`、`node_modules`；清单 `profile.yml` 的 `bundles:` 仅引用宿主内置命名 bundle，未知名 → skipped 警告非致命；profile patch = `<profile>/fr.patch.yml`（可选，缺文件 = 空层）；家目录 patch = `<FR_HOME>/fr.patch.yml`（可选，缺文件 = 空层）；`--patch` 为必需文件、缺文件报错终止；任何 patch 解析错误 fail-closed；已知入口名的 profile 首次使用自动初始化（清单 + 空白 patch 模板，绝不覆盖既有文件；fr 模板 bundles 为空列表——入口默认 bundle 恒在层 1，不重复挂载），未知名报错。patch 装载诊断文案 MUST 逐字对齐 dsh parsePatchList 族（2026-10-24 Q52A 裁决，app-boot index.ts:520-590 实证）：读失败 `fr: failed to read patches <path>: <原因>`（--patch 层 `fr: failed to read overlay <path>: <原因>`）、解析失败 `fr: failed to parse <label> <path>: <原因>`、非序列文档 `fr: <label> <path> must be a top-level YAML array of loader patch entries`、非映射行 `fr: <label> entry <N> in <path> must be a mapping (a loader patch entry)`（N 1-based；label：可选/家目录层与内嵌 bundle 资产 patches、--patch 层 overlay）；错误变体裸输出（Io/Profile/PatchParse 无 fr 包装前缀）；`--from-default-profile` 未伴随 profile 时 MUST fail-closed 拒绝装载（2026-10-24 Q51A 裁决：`--from-default-profile requires --profile`，dsh resolveBoot 在 CLI 层先行拒绝故 dsh 无此库态，文案为 fr 自有面）。清单与 profile 名诊断文案 MUST 逐字对齐 dsh（2026-10-24 Q53A 裁决，app-boot profile.ts:617/622/173/798 实证）：读失败 `fr: failed to read profile manifest <path>: <原因>`、非对象 `fr: profile manifest <path> must hold a JSON object`、非法名 `fr: invalid profile name "<name>"`（JSON.stringify 引号形）、未知 profile `fr: profile "<name>" does not exist`；:798 尾部补救命令后缀不移植（fr M1 无 plugin 命令，PRD 19.2 条 19）；清单解析错误文案系引擎自有（dsh V8 SyntaxError / fr YAML 解析器文案），同为裸抛结构不可字节对齐（同条）。
- fr-config MUST 将展开后的装配编译为 dougong loader JSON 文档（source.kind 仅 dylib/wasm：行 name 经产物解析规则映射——字面路径，或 `FR_PLUGIN_PATH` 各目录与安装库目录 `<prefix>/lib/fr/` 下按 slug（`-`→`_`）先查 `lib<slug>.so`（dylib）再查 `<slug>.wasm`（wasm），先命中定形态，未命中进入 failures；config 对象、isolate），经 `Reconciler::replace + apply` 幂等收敛；编译产物 MUST 为 dougong `Config::parse` 可接受的合法文档，且 MUST NOT 出现 builtin source。insert 行 name 为 `./`/`../` 开头的相对路径 MUST 在装载期锚定到所在补丁文件目录转绝对（2026-10-22 Q6 裁决，对齐 dsh anchorInsertedPluginNames index.ts:550-561，词法归一不做符号链接解析）；覆盖行 name 为匹配器 MUST NOT 锚定；无 `./`/`../` 前缀的含分隔符 name 保持字面路径。
- fr 的 config schema 以 fr-schema 声明式描述树为单一真源（2026-10-22 Q7 裁决，等价 dsh schemastery 校验+投影双重角色）：每插件 schema 为 fr-schema 描述符（受限子集，M1 面 = string/number/integer/boolean/object/array/dict；meta 注解仅 default 与 description；子集外声明不可构造——fail-closed），同一声明派生 `validate` 与 `to_json_schema`（JSON Schema 2020-12 关键词面，对齐 dsh ConfigJsonSchemaObject）。fr-config MUST 在装配期按插件 slug 查 fr-schema 注册表对每行 config 校验：查无 schema MUST 跳过该行校验（2026-10-22 Q9 裁决，对齐 dsh absent 标注；第三方字面路径工件宿主侧本无其 schema，不构成装载障碍）；有 schema 且单行校验失败进入 `Report::failures`，MUST NOT 阻塞其余槽位。插件工件 MUST 在激活体顶部以 fr-schema 链接副本对 config 再校验（双侧校验），激活期校验失败按插件装载失败 fail-closed。
- patch 行内条件表达式（`!!js` 标记或 `!!js ` 前缀字符串）MUST 限于受限声明式子集（2026-10-22 Q3A 勘误扩充）：`env.NAME ?? 默认值` 链、相等比较（`==`/`===`）、`platform === 'linux'|'macos'|'windows'` 与平台三元、裸 env 值读取、字面量、`profile-context` 布尔（dsh `ctx.get('profileContext')` 的 fr 等价物：入口/profile 上下文存在性，支持 `!` 取反）；MUST NOT 支持任意 JS 求值；子集外表达式（如 `getBuiltinModule` 类）MUST 求值失败 fail-closed（patch 文档整体拒绝，启动终止——对齐 dsh 用户层 patch 错误抛出语义）。
- fr MUST 提供 `fr dump-config` 与 `fr dump-config-schema`，内容由真实装配生成（文档即生成物）。`fr dump-config` MUST 输出合成装配树 YAML 文本、逐字节同型对齐 dsh `--dump-config`（2026-10-22 Q18-Q20 裁决，apps/cli/src/dump-config.ts + app-boot index.ts:635- 实证）：`# == <源文件>[, patched by <层>…]` 分隔注释按连续同源行分组；`!!js` 表达式逐字打印不求值；快照算法 = 层前缀各做单次 applyEntryPatches 同参展平（分层调用会让 group config 替换的子行被后续层命中，boot 不会）；整篇可再 parse 且等于同参启动挂载结果；层序 = bundle 层（label=包名）→ profile patch（label=路径，存在才入）→ 家目录 patch → `--patch` overlays（label=绝对路径）；错误三连 `fr: failed to read config` / `fr: failed to parse config` / `fr: config <path> must be a top-level YAML array of entries`。`fr dump-config-schema` MUST 输出 ConfigSchemaDump 等价物：`$schema` 2020-12 + `x-fr` 扩展键（Q18A，对齐 dsh `x-cordis`：{profile, complete, entries, diagnostics, patchSchema:'#/$defs/patchList'}）+ entry 五态 schema/partial/absent/unsupported/error + diagnostics 逐条 stderr（`fr: <level>: [<path>] <message>`（无 path 时 `fr: <level>: <message>`））+ complete 为假时退出码 1。三 dump 态互斥且拒收 app 位置参数；`--default-only`（Q19A，对齐 dsh `--dump-default-config`）为恢复诊断：仅 bundle 层、不读不解析 profile patch 与家目录 patch（坏 patch 文件亦可 dump）、不接受 `--patch`。`from-default-profile`（Q20A，对齐 dsh `--from-default-profile`）= 以出厂模板**替换**自定义 profile 后装载（区别于首用初始化绝不覆盖），dump 与 boot 共用；CLI 已随宿主切片落地（2026-10-23 Q30B）：子命令形态 `fr dump-config` / `fr dump-config-schema`；dsh 三旗标互斥在子命令形态结构性成立（文案不移植）；子命令自首个无法识别参数起整体视为 app 内参拒收（`error: config dumps take no app arguments, got <逐参数 JSON 串列>`，含 `-h`）；参数校验错误族逐字：`error: --profile <name> is required` / `error: --profile needs a name` / `error: select a profile only once` / `error: --patch needs a path` / `error: --from-default-profile needs a name`；`--default-only` 与 `--patch` 同给报 `error: --default-only prints the bundle layers and takes no --patch`；dump-config-schema stdout 恰单份 JSON 加换行、diagnostics 逐条 stderr、complete 假退出码 1；dump-config 致命错误（错误三连）以单行 stderr 诊断与退出码 1 终止；根调用 `-V/--version` 版本退出码 0、`-h/--help` 用法退出码 0、其余根调用用法错误退出码 1（fr 自有 launcher 面，M1 宿主全量时扩展入口模式）。
- fr MUST 提供关键默认值：默认模型 provider `deepseek-official`、model `deepseek-flash`；`FR_PERMISSION_MODE ?? workspace-write`（workspace root = 进程启动 cwd）；非 danger-full-access 时审批策略 `'ask'`；会话存储 `<FR_HOME>/sessions`、storages `<FR_HOME>/storages`；bash `timeout-ms` 60000；重复工具提醒阈值 [3, 5, 8]；`sample-over-cap-glob-results: false`；`allow-parallel-in-progress: true`；会话标题默认：无 LLM 时回退 5 词/40 字节（上限 80 字节），LLM 标题 targetWords 5（CJK 10）、maxInput 4096 字节、超时 60s。
- fr 宿主二进制 MUST 仅含装配器、事件泵、入口分发三件事：事件泵驱动 tokio runtime，把入口信号（headless 任务 / TUI 按键 / ACP·SDK 协议帧）转发到对应入口插件监听器；按子命令装配对应入口 profile（headless/tui/acp/sdk/web）；宿主对 agent 循环零感知。FR_HOME 默认 `~/.fly-rafter`，家目录结构与 dsh `~/.dsh/` 同构（sessions/storages/profiles）。
- 插件仅 dylib 与 wasm 两形态（2026-10-22 用户裁决，废除 builtin），MUST 共用 dougong 五步契约（声明 → 激活 → 服务 → 监听 → 释放，跨边界经 descriptor + C-ABI 导出表表达）：全部第一方插件 MUST 为 C-ABI dylib 工件（cdylib、内核家族仅依赖 dougong-abi、代码镜像进程内永不卸载，换版本即重启进程）；wasm 组件（wasm32-wasip2）仅为第三方沙箱扩展（零能力 WASI：wasip2 guest 必然声明 wasi:io 导入而宿主挂载零能力 WasiCtx 的惰性定义，能力面为零、导入声明不为零；fuel/epoch/内存帽；经 WIT world `dougong:kernel/host` 交互；零能力 WASI 无网络与文件系统，第一方组件 MUST NOT 以 wasm 承载）。fr-config MUST 按信任边界校验每行：第一方插件名 MUST 解析为 dylib source（解析命中 wasm 产物即为违规），wasm 仅当产物为第三方 `.wasm`；违规行进入 failures；loader builtin 表 MUST NOT 出现在 fr 装配产物中。
- 第三方工具插件 MUST 经 Invoker 通道接入：跨边界契约 = 事件名 + JSON 载荷 schema + 方法号常量表（descriptor 声明 + 能力自检）；isolate 列 MUST 承载 realm（会话级装配在会话 realm 内放置同名插件，域内 provide 不外泄、注入先域内后回退全局默认）；`internal/update` waterfall 仅作热更新预留扩展点，本期 MUST NOT 暴露。
- 默认装配树 MUST 与 dsh base cordis.patch.yml 插件 id 集合同型（品牌映射后），并冻结 PRD 6.4 未记载默认值：agent-instructions maxBytes 65536、projection-cache 200 事件 5000ms、web searchProvider deepseek-official 与 fetchProvider http、repeat argumentsPreviewChars 500、标题 maxOutputTokens 4096、tools 默认 native、storage-domain backend json（dsh cordis.patch.yml 各行）。
- 启动失败策略 MUST（对齐 dsh required-id 审计 [app-boot index.ts:944-961,1092-1114]）：仅 bootstrap 入口 Include 与必需插件 id 集（fr 对应 agent-loop 与当前子命令入口插件）激活失败时以 StartupError 拒启动（错误列出失败与等待清单）；其余插件失败一律单条警告、兄弟继续；注入服务缺失 → 插件 fiber 置 pending 容忍（其 id 属必需集则致命）；导入失败记录错误后不留实例、不中止装配；patch 未知 id 警告跳过永不致命；覆盖行语义 MUST 为按键覆盖（2026-10-22 Q1 勘误，对齐 dsh applyEntryPatches），patch 行内仅其余键作用于该行自身。
- patch 行内 env 缺省表达式 MUST 以受限子集（CONFIG-4）表达 dsh !!js 形态的对应语义：`env.FR_PERMISSION_MODE ?? workspace-write` 与 `danger-full-access` 的相等比较求值结果 MUST 与 dsh `process.env.DSH_PERMISSION_MODE ?? 'workspace-write'` 逐值一致（dsh cordis.patch.yml:246,262）。
- .env 分层 MUST（2026-10-22 用户裁决纳入，Q11A/Q12A/Q13A/Q14A 定稿；bootstrap 49 名全表以 PRD 6.4 为真源）：fr-config 启动期解析环境 = 继承进程 env > 项目 `.env`（自 cwd）> 家目录 `.env`（`<FR_HOME>/.env`）；两文件 MUST 先全部解析校验、后统一应用（任一层 bootstrap 违例则两文件均不应用）；`cwd == FR_HOME` 时家目录层 MUST NOT 重复读取；`.env` 中已设名 MUST NOT 覆盖更高层已存在的值（应用序 project → user，逐名仅在更高层未设时 materialize 进程 env）；bootstrap 专名在任何 `.env` 层拒绝（大小写不敏感）：49 个精确名 = dsh BOOTSTRAP_NAMES（app-boot index.ts:136-157）逐字（DEEPSEEK_BASE_URL 与 DEEPSEEK_SEARCH_BASE_URL 保留原名）+ 四前缀 FR_ / XDG_ / DYLD_ / BASH_FUNC_（DSH_ 品牌映射）；家目录层例外仅 HTTP_PROXY / HTTPS_PROXY / ALL_PROXY / NO_PROXY 四键（项目层设代理名仍拒绝）；bootstrap 违例 MUST 终止启动（fail-closed）；`.env` 读错误非 ENOENT = stderr 一行警告 + 该层按空层跳过（非致命）；ENOENT = 静默空层；机制为双轨 = materialize 进程 env（unsafe set_var 启动期单线程）+ 返回不可变环境快照（层序 process > project-env > user-env 的 get / get_from 逐名来源查询，消费方后续里程碑接线）；`.env` 解析语法 MUST 对齐 Node util.parseEnv 方言（自实现，以 Node 官方源码为钉测蓝本）；home 解析 = 显式 configured > `$FR_HOME`（trim 后空白视为未设）> `~/.fly-rafter`（~ 前缀展开后归一）；dsh 单层 loadEnv 变体仅测试夹具使用，fr 不实现（负向钉）
- `!!js` 方言解析与 dump 输出 MUST 结构感知（2026-10-22 Q25B 裁决）：字符串值内容中形如 `key: !!js v` 的文本（块标量、引号多行标量、普通多行标量）MUST 逐字装载，不翻转为单叹号、不作为表达式求值；识别值位（行首 `key: ` 或 `- ` 前缀后）的 `!!js` 照旧归一为表达式求值；识别位之外的结构值位（flow 式、异形键）MUST fail-closed 拒绝且错误含 `outside a recognized value position` 文案；`dump-config` 输出中字符串内容行形如 `key: !js v` MUST 保持单叹号逐字、真实 `!!js` 标签行照常打印 `!!js`，整篇再 parse 等于同参挂载结果（dump == boot 对任意字符串成立）

## Boundaries

### Allowed Changes
- crates/fr-config/**
- crates/fr-schema/**
- crates/fr/**

### Forbidden
- Do not weaken or remove the source requirement clauses.
- Do not mark this generated draft complete until each `Test:` selector names a real test.
- 任何领域 crate 不得依赖入口 crate（fr-headless/fr-tui/fr-acp/fr-sdk）
- 不得修改本合约 satisfies 指向的 knowledge/requirements 条款来让验证通过
- 不得引入 PRD 19.1 排除清单条目

## Completion Criteria

Scenario: 覆盖行仅翻转 disabled
  Test: toggle_only_override_keeps_other_keys
  假设 覆盖行仅携带 id 与 disabled:true 且未携 config 或 name
  当 合并到已存在的同名 id 行时
  那么 仅该行 disabled 变为 true，name 与 config 保持不变

Scenario: 覆盖行整键替换 config
  Test: config_override_replaces_wholesale
  假设 覆盖行携带新 config 对象
  当 合并到已存在 id 行时
  那么 该行 config 整体替换为新对象（不深合并，旧键不保留）

Scenario: 覆盖行未命中 id 警告跳过
  Test: unmatched_and_mismatched_overrides_warn_and_skip
  假设 覆盖行的 id 在装配中不存在
  当 合并时
  那么 发出单条警告并跳过该行，不进 failures、不致命

Scenario: 覆盖行 name 不匹配警告跳过
  Test: unmatched_and_mismatched_overrides_warn_and_skip
  假设 覆盖行 id 命中但 name 与既有行不同
  当 合并时
  那么 发出单条警告并跳过该行，不致命

Scenario: 装配层次自底向上展开
  Test: stack_layers_ordered_bottom_up
  假设 入口默认、profile、家目录 patch 与 --patch 各定义了同一 id
  当 展开装配时
  那么 按入口默认 → profile → 家目录 patch → --patch 顺序覆盖，最终值来自最后一层

Scenario: insert 行向根插入并保持首插序
  Test: insert_appends_and_later_layers_can_override
  假设 某 bundle 层以 insert 行插入两个新 id
  当 合并完成后
  那么 新行按插入序追加于行列表尾，后续层可按 id 覆盖这些插入行

Scenario: profile 清单 bundle 栈按清单序叠加
  Test: pending_req_fr_config_profile_bundle
  假设 profile.yml 的 bundles 列表为 [a, b] 且两者均为内置命名 bundle
  当 装载该 profile 时
  那么 bundle 层按清单序（先列者先应用）叠加于入口默认 bundle 之后、profile patch 之前

Scenario: profile 清单未知名 skipped 警告非致命
  Test: unknown_manifest_bundle_is_skipped_non_fatal
  假设 profile.yml bundles 引用了不存在的 bundle 名
  当 装载该 profile 时
  那么 该名进入 skipped 列表并附原因，其余 bundle 正常装载，不致命

Scenario: profile 名校验拒绝非法名
  Test: invalid_profile_name_pins_dsh_text
  假设 profile 名为空、含 / 或 \、等于 . 或 ..、或为 node_modules
  当 解析 profile 引用时
  那么 拒绝并报错，不产生文件系统副作用，文案逐字 `fr: invalid profile name "<name>"`（对齐 dsh profile.ts:173，JSON.stringify 引号形）

Scenario: profile 模板首用自动初始化
  Test: fresh_headless_profile_auto_inits
  假设 FR_HOME/profiles/headless 目录不存在
  当 首次以 profile 名 headless 装载时
  那么 自动创建目录、profile.yml（bundles 为空列表）与空白 fr.patch.yml 模板，绝不覆盖既有文件

Scenario: 未知 profile 名报错
  Test: unknown_profile_pins_dsh_text
  假设 profile 名无内置模板且对应目录不存在
  当 装载该 profile 时
  那么 报错指明 profile 不存在并终止启动，文案逐字 `fr: profile "<name>" does not exist`（dsh profile.ts:798 前段；补救命令后缀不移植，PRD 19.2 条 19）

Scenario: profile 清单读失败与非对象文案逐字对齐
  Test: manifest_read_diagnostics_pins_dsh_texts
  假设 profile 清单文件不可读（如为目录）或解析结果非映射
  当 装载该 profile 时
  那么 分别以单行错误终止启动：`fr: failed to read profile manifest <path>: <原因>`、`fr: profile manifest <path> must hold a JSON object`（对齐 dsh readProfileManifest profile.ts:617/622）

Scenario: 可选 patch 缺文件为空层
  Test: optional_patch_absent_is_empty_layer
  假设 profile patch 与家目录 patch 文件均不存在
  当 装载时
  那么 两处均按空层处理，不报错

Scenario: --patch 缺文件报错
  Test: required_overlay_absent_fails_startup
  假设 --patch 给出的路径不存在
  当 装载 overlays 时
  那么 报错并终止启动

Scenario: patch 解析错误 fail-closed
  Test: optional_patch_parse_error_is_fatal
  假设 任一层 patch 文件存在解析错误（行 schema 违例或子集外表达式）
  当 装载时
  那么 报错并终止启动，不产生部分装配产物

Scenario: 可选 patch 读失败文案逐字对齐
  Test: optional_patch_read_failure_pins_dsh_text
  假设 profile patch 或家目录 patch 路径存在但不可读（如为目录）
  当 装载时
  那么 以单行错误终止启动：`fr: failed to read patches <path>: <原因>`（对齐 dsh loadOptionalPatches index.ts:526）

Scenario: --patch 读失败文案逐字对齐
  Test: overlay_read_failure_pins_dsh_text
  假设 --patch 给出的路径不可读或不存在
  当 装载 overlays 时
  那么 以单行错误终止启动：`fr: failed to read overlay <path>: <原因>`（对齐 dsh loadOverlayPatches index.ts:545）

Scenario: patch 文档形态错误文案逐字对齐
  Test: patch_document_shape_errors_pin_dsh_texts
  假设 patch 文档为非序列、YAML 语法错误或含非映射行
  当 装载时
  那么 分别以单行错误终止启动：`fr: <label> <path> must be a top-level YAML array of loader patch entries`、`fr: failed to parse <label> <path>: <原因>`、`fr: <label> entry <N> in <path> must be a mapping (a loader patch entry)`（label 按层 patches/overlay，对齐 dsh parsePatchList index.ts:582-589）

Scenario: from-default-profile 缺 profile 拒绝装载
  Test: from_default_profile_without_profile_rejected
  假设 装载调用给出 from-default-profile 模板但未选 profile
  当 装载时
  那么 fail-closed 拒绝：`--from-default-profile requires --profile`（2026-10-24 Q51A 裁决；dsh resolveBoot 在 CLI 层先拒，库层同拒为 fr 自有面）

Scenario: 编译产物为合法 loader 文档
  Test: compiled_document_is_valid_loader_config
  假设 一次完整装配展开完成
  当 编译并执行 Reconciler::replace 加 apply 时
  那么 编译产物通过 dougong Config::parse，每行含 source/config/isolate

Scenario: insert 行相对路径锚定到补丁文件目录
  Test: relative_insert_names_anchor_to_patch_dir
  假设 某补丁文件以 insert 行插入 name 为 ./ext/libfoo.so 与 ../sib/bar.wasm 的插件
  当 装载该补丁层时
  那么 两个 name 锚定为该补丁文件目录下的绝对路径（../ 逐级上折），后续编译按字面路径解析

Scenario: 覆盖行 name 与无前缀相对 name 不锚定
  Test: override_row_names_are_never_anchored
  假设 覆盖行 name 为 ./matcher 或 insert 行 name 为 plugins/foo.so（无 ./ ../ 前缀）
  当 装载该补丁层时
  那么 两处 name 保持原样（匹配器不锚定；无前缀含分隔符 name 保持字面路径）

Scenario: 单行 config 校验失败不阻塞其余槽位
  Test: schema_violation_fails_row_without_blocking
  假设 某插件行的 config 违反其 ConfigSchema
  当 执行装配收敛时
  那么 该行进入 failures，其余插件正常装载

Scenario: 查无 schema 的行跳过校验
  Test: absent_schema_skips_validation
  假设 某行插件名在 fr-schema 注册表中无声明（如第三方字面路径工件）
  当 执行装配收敛时
  那么 该行 config 不校验、照常装载，不进 failures（dsh absent 语义）

Scenario: 同一声明派生校验与 JSON Schema 投影
  Test: validation_and_projection_share_one_declaration
  假设 fr-schema 描述符声明了候选枚举 string 字段与嵌套对象结构
  当 分别执行 validate 与 to_json_schema 时
  那么 两者出自同一声明：validate 拒绝枚举外与结构违例值，投影产出 2020-12 关键词面 JSON Schema

Scenario: 激活期链接副本再校验 fail-closed
  Test: pending_req_fr_config_fail_closed
  假设 插件工件激活体收到违反其 schema 的 config
  当 激活体顶部以 fr-schema 链接副本再校验时
  那么 校验失败按插件装载失败 fail-closed，服务不激活

Scenario: 超出受限子集的条件表达式 fail-closed
  Test: expression_outside_subset_rejects_document
  假设 patch 行使用任意 JS 表达式（如 getBuiltinModule 路径解析类）
  当 解析 fr.patch.yml 求值该表达式时
  那么 求值失败进 failures 并按启动失败策略处理

Scenario: env 默认链与相等比较逐值求值
  Test: env_default_chain_matches_dsh_semantics
  假设 patch 行含 env.FR_PERMISSION_MODE ?? workspace-write 与 danger-full-access 相等比较
  当 分别在 FR_PERMISSION_MODE 设为 danger-full-access、其他值与未设置下求值时
  那么 求值结果与 dsh process.env.DSH_PERMISSION_MODE ?? 'workspace-write' 逐值一致

Scenario: 裸 env 值读取作为配置值
  Test: bare_env_read_is_null_when_absent
  假设 patch 行 config 值为裸 env.FR_TOOLS_MODE 表达式
  当 求值时
  那么 env 存在时取其字符串值，不存在时取 null

Scenario: profile-context 布尔门控
  Test: profile_context_boolean_with_negation
  假设 patch 行 disabled 为 profile-context 取反布尔（dsh !ctx.get('profileContext') 的 fr 等价物）
  当 分别在有与无入口 profile 上下文时求值
  那么 有上下文时 disabled=false，无上下文时 disabled=true

Scenario: 平台三元按字面语义求值
  Test: platform_ternary_and_equality
  假设 patch 行含 linux 平台三元
  当 在不同平台断言下求值时
  那么 按平台真值选择分支，无 JS 求值器介入

Scenario: dump 命令输出来自真实装配
  Test: dump_config_and_schema_generated_from_real_assembly
  当 运行 fr dump-config 与 fr dump-config-schema 时
  那么 分别输出合成后插件树与全部插件 config schema

Scenario: dump 输出注释分层逐字同型
  Test: composes_layers_prints_verbatim_js_and_labels_sections
  假设 bundle、profile patch、家目录 patch 与 --patch 多层修改同一行并插入新行
  当 运行 fr dump-config 时
  那么 输出 YAML 文本中 # == 源文件[, patched by 层…] 分隔注释按连续同源行分组，!!js 表达式逐字未求值，整篇可再 parse 且等于同参启动挂载结果

Scenario: default-only 恢复诊断不读不解析用户层
  Test: default_only_collects_bundle_layers_only_and_skips_broken_user_files
  假设 profile 的 fr.patch.yml 含语法损坏内容且以 --default-only 调用
  当 运行 fr dump-config 时
  那么 仅输出 bundle 层，损坏文件未读未解析且不报错

Scenario: default-only 不接受 --patch
  Test: dump_arg_validation_error_family
  假设 --default-only 与 --patch 同时给出
  当 解析调用时
  那么 报错终止

Scenario: x-fr 扩展键与 entry 五态
  Test: x_fr_keys_and_entry_status_vocabulary
  假设 运行 fr dump-config-schema
  当 检查输出文档时
  那么 $schema 为 2020-12，含 x-fr 的 profile/complete/entries/diagnostics/patchSchema 五键，entry status 为五态之一

Scenario: complete 为假退出码 1
  Test: incomplete_schema_dump_exits_one
  假设 装配存在 error 态 entry
  当 运行 fr dump-config-schema 时
  那么 diagnostics 含该条且进程退出码为 1

Scenario: 注册表命中投影 configN
  Test: registry_hit_projects_config_defs
  假设 装配行 name 命中 fr-schema 注册表
  当 运行 fr dump-config-schema 时
  那么 该 entry status 为 schema 且 configRef 指向 $defs/configN，其 $defs 内容与同一声明的 to_json_schema 投影一致，且投影为 dsh 全脸（非必需位 type 并 'null'、必需位附加 not:{type:'null'}、根与嵌套值位 anyOf loaderExpression 包裹）

Scenario: 同名异 schema 投影分裂告警
  Test: ambiguous_name_splits_projections_and_warns
  假设 同一插件名的两行经 schema_of 命中不同声明
  当 运行 fr dump-config-schema 时
  那么 config0/config1 按发现序分裂，ambiguity 警告入 diagnostics，complete 为假

Scenario: 查无 schema 标注 absent
  Test: absent_entries_reference_unknown_config
  假设 装配行 name 未命中 fr-schema 注册表
  当 运行 fr dump-config-schema 时
  那么 该 entry status 为 absent 且 configRef 为 #/$defs/unknownConfig

Scenario: from-default-profile 以出厂模板替换
  Test: from_default_profile_replaces_existing_profile
  假设 自定义 profile 已存在且携带 from-default-profile 指向 headless
  当 数据面装载该 profile 时
  那么 出厂模板替换既有 profile 内容后装载（非首用初始化的绝不覆盖语义）

Scenario: dump 拒收 app 位置参数与未知旗标
  Test: dump_rejects_app_arguments_and_unknown_flags
  假设 dump 子命令调用携带位置参数或未知旗标（含 -h）
  当 解析参数时
  那么 stderr 输出 error: config dumps take no app arguments, got <逐参数 JSON 串列>，退出码 1，stdout 无输出

Scenario: dump 参数校验错误族
  Test: dump_arg_validation_error_family
  假设 dump 子命令分别缺 --profile、--profile 为空值、--profile 重复、--patch 空值或 --from-default-profile 空值
  当 解析参数时
  那么 依次输出 error: --profile <name> is required、error: --profile needs a name、error: select a profile only once、error: --patch needs a path、error: --from-default-profile needs a name，均以退出码 1 终止

Scenario: dump-config-schema 流面分流
  Test: schema_dump_streams_json_and_diagnostics
  假设 运行 fr dump-config-schema 且装配完整
  当 输出时
  那么 stdout 恰为单份 JSON 文档加一个换行，diagnostics 逐条 stderr（fr: <level>: [<path>] <message>，无 path 时无方括号段），退出码 0

Scenario: dump 致命错误单行诊断退出码 1
  Test: dump_fatal_error_prints_single_line_and_exits_one
  假设 --patch 指向无法读取或无法解析的文件
  当 运行 fr dump-config 时
  那么 stderr 输出含对应错误三连文案的单行诊断，退出码 1，stdout 无输出

Scenario: dump 之外的根调用面
  Test: root_dispatch_version_help_and_unknown_modes
  假设 fr 以 -V/--version、-h/--help、无子命令或未知子命令调用
  当 解析根调用时
  那么 -V/--version 输出版本号退出码 0，-h/--help 输出用法退出码 0，其余以用法错误行退出码 1 拒绝

Scenario: 未配置时关键默认值生效
  Test: pending_req_fr_config_requirement
  假设 全默认配置启动
  当 读取解析后的配置时
  那么 provider 为 deepseek-official、model 为 deepseek-flash、权限模式为 workspace-write、bash timeout-ms 为 60000

Scenario: FR_PERMISSION_MODE 覆盖默认权限模式
  Test: pending_req_fr_config_fr_permission_mode
  假设 FR_PERMISSION_MODE 为 read-only
  当 解析权限模式时
  那么 生效 read-only 而非默认 workspace-write

Scenario: 宿主按子命令装配对应入口 profile
  Test: pending_req_fr_config_profile
  假设 调用 fr headless
  当 装配时
  那么 装配 headless 入口 profile，宿主二进制不含 agent 循环实现

Scenario: 事件泵转发入口信号
  Test: pending_req_fr_config_requirement
  假设 headless 任务、TUI 按键或 ACP/SDK 协议帧到达
  当 事件泵运转时
  那么 信号被转发到对应入口插件的监听器

Scenario: 第一方名解析为 wasm 被拒
  Test: first_party_wasm_resolution_is_rejected
  假设 patch 行 name 为 fr-session 且解析命中 <slug>.wasm 产物
  当 fr-config 信任边界校验时
  那么 该行进入 failures 且不装载

Scenario: dylib 镜像永不卸载
  Test: pending_req_fr_config_dylib
  假设 已加载一个第三方 dylib 插件
  当 变更其补丁行并 reconcile 时
  那么 代码镜像保持驻留不卸载，换版本要求重启进程

Scenario: 第一方 dylib 工件缺失启动失败
  Test: pending_req_fr_config_dylib
  假设 patch 行 name 为 fr-session 且解析路径不存在对应 .so
  当 装配收敛时
  那么 该行进入 failures 并按启动失败策略处理，其余槽不受影响

Scenario: 装配产物不含 builtin source
  Test: pending_req_fr_config_builtin_source
  假设 默认 headless 装配展开完成
  当 检查编译产物全部 source.kind 时
  那么 全部为 dylib，无任何 builtin 条目

Scenario: 会话 realm 域内解析不外泄
  Test: pending_req_fr_config_realm
  假设 会话 realm 内放置了同名插件
  当 会话内 require 该服务时
  那么 先命中域内实现，域内 provide 不外泄到全局默认

Scenario: 插件经 Invoker 通道接入
  Test: pending_req_fr_config_invoker
  当 插件（第一方 dylib 或第三方 wasm）跨边界提供服务时
  那么 经事件名加 JSON 载荷 schema 加方法号常量表的 Invoker 契约接入，且 descriptor 声明与能力自检通过

Scenario: profile 选择与缩写
  Test: pending_req_fr_config_profile
  假设 定义了名为 work 的 profile
  当 分别以 --profile work 与位置参数缩写 fr work 启动时

Scenario: 全表默认值逐项生效
  Test: pending_req_fr_config_requirement
  假设 全默认配置启动
  当 逐项读取会话存储路径、会话标题参数（无 LLM 回退 5 词 40 字节上限 80、LLM targetWords 5 CJK 10 maxInput 4096 超时 60s）、重复工具提醒阈值、glob 与 todo 配置时
  那么 全部与 PRD 6.4 表一致

Scenario: dump-config 插件树与 dsh base patch 同型
  Test: pending_req_fr_config_dump_config_dsh_base_patch
  假设 默认装配导出 fr dump-config
  当 与 dsh packages/bundle/base/cordis.patch.yml 展开的插件树同型比对时
  那么 插件名集合与默认配置逐行同型（品牌键除外）

Scenario: disabled 行使插件不装载
  Test: compiles_resolved_rows_and_excludes_disabled
  假设 后行将某 id 行置 disabled 为 true
  当 收敛时
  那么 该插件不装载且编译产物不含该行实例

Scenario: dylib 与 wasm source 行装载
  Test: pending_req_fr_config_dylib_wasm_source
  假设 装配分别包含第一方 dylib 行与第三方 wasm 行
  当 收敛时
  那么 两种 source 按各自形态装载且能力边界生效

Scenario: isolate 变更触发 retire 加重插
  Test: pending_req_fr_config_isolate_retire
  假设 某行的 isolate 列发生变化
  当 reconcile 时
  那么 该插件先 retire 再重插，其余行不受影响

Scenario: 重复收敛产出 unchanged
  Test: pending_req_fr_config_unchanged
  假设 同一文档连续两次收敛
  当 第二次收敛时
  那么 Report 仅含 unchanged，无 insert/update/retire

Scenario: 装配树名单 diff
  Test: pending_req_fr_config_diff
  假设 以默认装配导出 dump-config
  当 与 base cordis.patch.yml 插件 id 集合按品牌映射比对时
  那么 集合一致且未记载默认值逐项命中

Scenario: 默认值 provider
  Test: pending_req_fr_config_provider
  假设 全默认配置启动
  当 读取 provider 时
  那么 值为 deepseek-official

Scenario: 默认值 model
  Test: pending_req_fr_config_model
  假设 全默认配置启动
  当 读取 model 时
  那么 值为 deepseek-flash

Scenario: 默认值 permission-mode
  Test: pending_req_fr_config_permission_mode
  假设 全默认配置启动
  当 读取 permission-mode 时
  那么 值为 workspace-write

Scenario: 默认值 approval-policy
  Test: pending_req_fr_config_approval_policy
  假设 全默认配置启动
  当 读取 approval-policy 时
  那么 值为 ask

Scenario: 默认值 bash timeout-ms
  Test: pending_req_fr_config_bash_timeout_ms
  假设 全默认配置启动
  当 读取 bash timeout-ms 时
  那么 值为 60000

Scenario: 默认值 repeat-thresholds
  Test: pending_req_fr_config_repeat_thresholds
  假设 全默认配置启动
  当 读取 repeat-thresholds 时
  那么 值为 [3,5,8]

Scenario: 默认值 argumentsPreviewChars
  Test: pending_req_fr_config_argumentspreviewchars
  假设 全默认配置启动
  当 读取 argumentsPreviewChars 时
  那么 值为 500

Scenario: 默认值 sample-over-cap-glob-results
  Test: pending_req_fr_config_sample_over_cap_glob_results
  假设 全默认配置启动
  当 读取 sample-over-cap-glob-results 时
  那么 值为 false

Scenario: 默认值 allow-parallel-in-progress
  Test: pending_req_fr_config_allow_parallel_in_progress
  假设 全默认配置启动
  当 读取 allow-parallel-in-progress 时
  那么 值为 true

Scenario: 默认值 pruner threshold
  Test: pending_req_fr_config_pruner_threshold
  假设 全默认配置启动
  当 读取 pruner threshold 时
  那么 值为 8192

Scenario: 默认值 pruner head
  Test: pending_req_fr_config_pruner_head
  假设 全默认配置启动
  当 读取 pruner head 时
  那么 值为 4096

Scenario: 默认值 pruner tail
  Test: pending_req_fr_config_pruner_tail
  假设 全默认配置启动
  当 读取 pruner tail 时
  那么 值为 1024

Scenario: 默认值 spill max-inline-tokens
  Test: pending_req_fr_config_spill_max_inline_tokens
  假设 全默认配置启动
  当 读取 spill max-inline-tokens 时
  那么 值为 12500

Scenario: 默认值 标题回退
  Test: pending_req_fr_config_requirement
  假设 全默认配置启动
  当 读取 标题回退 时
  那么 值为 5 词 40 字节上限 80

Scenario: 默认值 LLM 标题 targetWords
  Test: pending_req_fr_config_llm_targetwords
  假设 全默认配置启动
  当 读取 LLM 标题 targetWords 时
  那么 值为 5（CJK 10）

Scenario: 默认值 LLM 标题 maxInput
  Test: pending_req_fr_config_llm_maxinput
  假设 全默认配置启动
  当 读取 LLM 标题 maxInput 时
  那么 值为 4096 字节

Scenario: 默认值 LLM 标题 timeout
  Test: pending_req_fr_config_llm_timeout
  假设 全默认配置启动
  当 读取 LLM 标题 timeout 时
  那么 值为 60s

Scenario: 默认值 LLM 标题 maxOutputTokens
  Test: pending_req_fr_config_llm_maxoutputtokens
  假设 全默认配置启动
  当 读取 LLM 标题 maxOutputTokens 时
  那么 值为 4096

Scenario: 默认值 agent-instructions maxBytes
  Test: pending_req_fr_config_agent_instructions_maxbytes
  假设 全默认配置启动
  当 读取 agent-instructions maxBytes 时
  那么 值为 65536

Scenario: 默认值 projection-cache
  Test: pending_req_fr_config_projection_cache
  假设 全默认配置启动
  当 读取 projection-cache 时
  那么 值为 200 事件 5000ms

Scenario: 默认值 web searchProvider
  Test: pending_req_fr_config_web_searchprovider
  假设 全默认配置启动
  当 读取 web searchProvider 时
  那么 值为 deepseek-official

Scenario: 默认值 web fetchProvider
  Test: pending_req_fr_config_web_fetchprovider
  假设 全默认配置启动
  当 读取 web fetchProvider 时
  那么 值为 http

Scenario: 默认值 tool-web searchTimeoutMs
  Test: pending_req_fr_config_tool_web_searchtimeoutms
  假设 全默认配置启动
  当 读取 tool-web searchTimeoutMs 时
  那么 值为 60000

Scenario: 默认值 tools 默认 mode
  Test: pending_req_fr_config_tools_mode
  假设 全默认配置启动
  当 读取 tools 默认 mode 时
  那么 值为 native

Scenario: 默认值 storage-domain backend
  Test: pending_req_fr_config_storage_domain_backend
  假设 全默认配置启动
  当 读取 storage-domain backend 时
  那么 值为 json

Scenario: 默认值 PTC execution-mode
  Test: pending_req_fr_config_ptc_execution_mode
  假设 全默认配置启动
  当 读取 PTC execution-mode 时
  那么 值为 per-call

Scenario: 默认值 PTC timeoutMs
  Test: pending_req_fr_config_ptc_timeoutms
  假设 全默认配置启动
  当 读取 PTC timeoutMs 时
  那么 值为 120000

Scenario: 默认值 PTC maxTimeoutMs
  Test: pending_req_fr_config_ptc_maxtimeoutms
  假设 全默认配置启动
  当 读取 PTC maxTimeoutMs 时
  那么 值为 600000

Scenario: 默认值 PTC maxOutputBytes
  Test: pending_req_fr_config_ptc_maxoutputbytes
  假设 全默认配置启动
  当 读取 PTC maxOutputBytes 时
  那么 值为 64MiB

Scenario: 默认值 PTC maxOldGenerationSizeMb
  Test: pending_req_fr_config_ptc_maxoldgenerationsizemb
  假设 全默认配置启动
  当 读取 PTC maxOldGenerationSizeMb 时
  那么 值为 512

Scenario: 默认值 PTC maxMessageBytes
  Test: pending_req_fr_config_ptc_maxmessagebytes
  假设 全默认配置启动
  当 读取 PTC maxMessageBytes 时
  那么 值为 128MiB

Scenario: 默认值 PTC maxPendingCalls
  Test: pending_req_fr_config_ptc_maxpendingcalls
  假设 全默认配置启动
  当 读取 PTC maxPendingCalls 时
  那么 值为 128

Scenario: 默认值 PTC graceMs
  Test: pending_req_fr_config_ptc_gracems
  假设 全默认配置启动
  当 读取 PTC graceMs 时
  那么 值为 3000

Scenario: 默认值 max-parallel-sub-calls
  Test: pending_req_fr_config_max_parallel_sub_calls
  假设 全默认配置启动
  当 读取 max-parallel-sub-calls 时
  那么 值为 10

Scenario: 默认值 maxParallelToolCalls
  Test: pending_req_fr_config_maxparalleltoolcalls
  假设 全默认配置启动
  当 读取 maxParallelToolCalls 时
  那么 值为 10

Scenario: 默认值 ACP session-list-page-size
  Test: pending_req_fr_config_acp_session_list_page_size
  假设 全默认配置启动
  当 读取 ACP session-list-page-size 时
  那么 值为 100

Scenario: 默认值 retry maxRetries
  Test: pending_req_fr_config_retry_maxretries
  假设 全默认配置启动
  当 读取 retry maxRetries 时
  那么 值为 5

Scenario: 默认值 retry backoff initial
  Test: pending_req_fr_config_retry_backoff_initial
  假设 全默认配置启动
  当 读取 retry backoff initial 时
  那么 值为 500ms

Scenario: 默认值 retry backoff max
  Test: pending_req_fr_config_retry_backoff_max
  假设 全默认配置启动
  当 读取 retry backoff max 时
  那么 值为 10000ms

Scenario: config dsh-agent-loop maxParallelToolCalls 默认
  Test: pending_req_fr_config_config_dsh_agent_loop_maxparalleltoolcalls
  假设 插件 dsh-agent-loop 已注册
  当 读取解析后配置时
  那么 配置行 [maxParallelToolCalls:Volatile<number>:-（省略用内置 DEFAULT 10）] 的默认值按目录行实现

Scenario: config dsh-agent-loop maxParallelToolCalls 约束
  Test: pending_req_fr_config_config_dsh_agent_loop_maxparalleltoolcalls
  假设 插件 dsh-agent-loop 已注册
  当 校验与生效时
  那么 配置行 [maxParallelToolCalls:Volatile<number>:-（省略用内置 DEFAULT 10）] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-agent-loop agents 默认
  Test: pending_req_fr_config_config_dsh_agent_loop_agents
  假设 插件 dsh-agent-loop 已注册
  当 读取解析后配置时
  那么 配置行 [agents:启动时创建/恢复的 agents] 的默认值按目录行实现

Scenario: config dsh-agent-loop agents 约束
  Test: pending_req_fr_config_config_dsh_agent_loop_agents
  假设 插件 dsh-agent-loop 已注册
  当 校验与生效时
  那么 配置行 [agents:启动时创建/恢复的 agents] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-agent-instructions projectRootMarkers 默认
  Test: pending_req_fr_config_config_dsh_agent_instructions_projectrootmarkers
  假设 插件 dsh-agent-instructions 已注册
  当 读取解析后配置时
  那么 配置行 [projectRootMarkers:string[]（.git 等）] 的默认值按目录行实现

Scenario: config dsh-agent-instructions projectRootMarkers 约束
  Test: pending_req_fr_config_config_dsh_agent_instructions_projectrootmarkers
  假设 插件 dsh-agent-instructions 已注册
  当 校验与生效时
  那么 配置行 [projectRootMarkers:string[]（.git 等）] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-agent-instructions maxBytes 默认
  Test: pending_req_fr_config_config_dsh_agent_instructions_maxbytes
  假设 插件 dsh-agent-instructions 已注册
  当 读取解析后配置时
  那么 配置行 [maxBytes:number（非正/非有限禁用加载）] 的默认值按目录行实现

Scenario: config dsh-agent-instructions maxBytes 约束
  Test: pending_req_fr_config_config_dsh_agent_instructions_maxbytes
  假设 插件 dsh-agent-instructions 已注册
  当 校验与生效时
  那么 配置行 [maxBytes:number（非正/非有限禁用加载）] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-agent-instructions maxSourceBytes 默认
  Test: pending_req_fr_config_config_dsh_agent_instructions_maxsourcebytes
  假设 插件 dsh-agent-instructions 已注册
  当 读取解析后配置时
  那么 配置行 [maxSourceBytes:number（单文件上限超限忽略）] 的默认值按目录行实现

Scenario: config dsh-agent-instructions maxSourceBytes 约束
  Test: pending_req_fr_config_config_dsh_agent_instructions_maxsourcebytes
  假设 插件 dsh-agent-instructions 已注册
  当 校验与生效时
  那么 配置行 [maxSourceBytes:number（单文件上限超限忽略）] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-agent-tool-presentation / dsh-tools mode 默认
  Test: pending_req_fr_config_config_dsh_agent_tool_presentation_dsh_tools_mode
  假设 插件 dsh-agent-tool-presentation / dsh-tools 已注册
  当 读取解析后配置时
  那么 配置行 [mode:'native'|'ptc':tools 默认 native；ptc 需 ctx.ptcRuntime 且语言有 SDK renderer 否则组装失败] 的默认值按目录行实现

Scenario: config dsh-agent-tool-presentation / dsh-tools mode 约束
  Test: pending_req_fr_config_config_dsh_agent_tool_presentation_dsh_tools_mode
  假设 插件 dsh-agent-tool-presentation / dsh-tools 已注册
  当 校验与生效时
  那么 配置行 [mode:'native'|'ptc':tools 默认 native；ptc 需 ctx.ptcRuntime 且语言有 SDK renderer 否则组装失败] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-agent-tool-presentation / dsh-tools maxParallelSubCalls 默认
  Test: pending_req_fr_config_config_dsh_agent_tool_presentation_dsh_tools_maxparallelsubcalls
  假设 插件 dsh-agent-tool-presentation / dsh-tools 已注册
  当 读取解析后配置时
  那么 配置行 [maxParallelSubCalls:10（正整数，1=串行）] 的默认值按目录行实现

Scenario: config dsh-agent-tool-presentation / dsh-tools maxParallelSubCalls 约束
  Test: pending_req_fr_config_config_dsh_agent_tool_presentation_dsh_tools_maxparallelsubcalls
  假设 插件 dsh-agent-tool-presentation / dsh-tools 已注册
  当 校验与生效时
  那么 配置行 [maxParallelSubCalls:10（正整数，1=串行）] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-attachment-local dshHome 默认
  Test: pending_req_fr_config_config_dsh_attachment_local_dshhome
  假设 插件 dsh-attachment-local 已注册
  当 读取解析后配置时
  那么 配置行 [dshHome:-($DSH_HOME→~/.dsh)] 的默认值按目录行实现

Scenario: config dsh-attachment-local dshHome 约束
  Test: pending_req_fr_config_config_dsh_attachment_local_dshhome
  假设 插件 dsh-attachment-local 已注册
  当 校验与生效时
  那么 配置行 [dshHome:-($DSH_HOME→~/.dsh)] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-attachment-local maxImageBytes 默认
  Test: pending_req_fr_config_config_dsh_attachment_local_maximagebytes
  假设 插件 dsh-attachment-local 已注册
  当 读取解析后配置时
  那么 配置行 [maxImageBytes:20MiB] 的默认值按目录行实现

Scenario: config dsh-attachment-local maxImageBytes 约束
  Test: pending_req_fr_config_config_dsh_attachment_local_maximagebytes
  假设 插件 dsh-attachment-local 已注册
  当 校验与生效时
  那么 配置行 [maxImageBytes:20MiB] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-attachment-local maxImagesPerMessage 默认
  Test: pending_req_fr_config_config_dsh_attachment_local_maximagespermessage
  假设 插件 dsh-attachment-local 已注册
  当 读取解析后配置时
  那么 配置行 [maxImagesPerMessage:20] 的默认值按目录行实现

Scenario: config dsh-attachment-local maxImagesPerMessage 约束
  Test: pending_req_fr_config_config_dsh_attachment_local_maximagespermessage
  假设 插件 dsh-attachment-local 已注册
  当 校验与生效时
  那么 配置行 [maxImagesPerMessage:20] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-attachment-local maxMessageImageBytes 默认
  Test: pending_req_fr_config_config_dsh_attachment_local_maxmessageimagebytes
  假设 插件 dsh-attachment-local 已注册
  当 读取解析后配置时
  那么 配置行 [maxMessageImageBytes:200MiB] 的默认值按目录行实现

Scenario: config dsh-attachment-local maxMessageImageBytes 约束
  Test: pending_req_fr_config_config_dsh_attachment_local_maxmessageimagebytes
  假设 插件 dsh-attachment-local 已注册
  当 校验与生效时
  那么 配置行 [maxMessageImageBytes:200MiB] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-attachment-local maxImagePixels 默认
  Test: pending_req_fr_config_config_dsh_attachment_local_maximagepixels
  假设 插件 dsh-attachment-local 已注册
  当 读取解析后配置时
  那么 配置行 [maxImagePixels:64000000] 的默认值按目录行实现

Scenario: config dsh-attachment-local maxImagePixels 约束
  Test: pending_req_fr_config_config_dsh_attachment_local_maximagepixels
  假设 插件 dsh-attachment-local 已注册
  当 校验与生效时
  那么 配置行 [maxImagePixels:64000000] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-attachment-local maxImageDimension 默认
  Test: pending_req_fr_config_config_dsh_attachment_local_maximagedimension
  假设 插件 dsh-attachment-local 已注册
  当 读取解析后配置时
  那么 配置行 [maxImageDimension:8192px] 的默认值按目录行实现

Scenario: config dsh-attachment-local maxImageDimension 约束
  Test: pending_req_fr_config_config_dsh_attachment_local_maximagedimension
  假设 插件 dsh-attachment-local 已注册
  当 校验与生效时
  那么 配置行 [maxImageDimension:8192px] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-bash-local（执行器默认值真源） cwd 默认
  Test: pending_req_fr_config_config_dsh_bash_local_cwd
  假设 插件 dsh-bash-local（执行器默认值真源） 已注册
  当 读取解析后配置时
  那么 配置行 [cwd:Volatile:process.cwd()] 的默认值按目录行实现

Scenario: config dsh-bash-local（执行器默认值真源） cwd 约束
  Test: pending_req_fr_config_config_dsh_bash_local_cwd
  假设 插件 dsh-bash-local（执行器默认值真源） 已注册
  当 校验与生效时
  那么 配置行 [cwd:Volatile:process.cwd()] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-bash-local（执行器默认值真源） timeoutMs 默认
  Test: pending_req_fr_config_config_dsh_bash_local_timeoutms
  假设 插件 dsh-bash-local（执行器默认值真源） 已注册
  当 读取解析后配置时
  那么 配置行 [timeoutMs:Volatile:120000] 的默认值按目录行实现

Scenario: config dsh-bash-local（执行器默认值真源） timeoutMs 约束
  Test: pending_req_fr_config_config_dsh_bash_local_timeoutms
  假设 插件 dsh-bash-local（执行器默认值真源） 已注册
  当 校验与生效时
  那么 配置行 [timeoutMs:Volatile:120000] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-bash-local（执行器默认值真源） maxTimeoutMs 默认
  Test: pending_req_fr_config_config_dsh_bash_local_maxtimeoutms
  假设 插件 dsh-bash-local（执行器默认值真源） 已注册
  当 读取解析后配置时
  那么 配置行 [maxTimeoutMs:Volatile:600000] 的默认值按目录行实现

Scenario: config dsh-bash-local（执行器默认值真源） maxTimeoutMs 约束
  Test: pending_req_fr_config_config_dsh_bash_local_maxtimeoutms
  假设 插件 dsh-bash-local（执行器默认值真源） 已注册
  当 校验与生效时
  那么 配置行 [maxTimeoutMs:Volatile:600000] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-bash-local（执行器默认值真源） maxOutputBytes 默认
  Test: pending_req_fr_config_config_dsh_bash_local_maxoutputbytes
  假设 插件 dsh-bash-local（执行器默认值真源） 已注册
  当 读取解析后配置时
  那么 配置行 [maxOutputBytes:Volatile:64000（溢出落临时文件）] 的默认值按目录行实现

Scenario: config dsh-bash-local（执行器默认值真源） maxOutputBytes 约束
  Test: pending_req_fr_config_config_dsh_bash_local_maxoutputbytes
  假设 插件 dsh-bash-local（执行器默认值真源） 已注册
  当 校验与生效时
  那么 配置行 [maxOutputBytes:Volatile:64000（溢出落临时文件）] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-compaction-basic auto 默认
  Test: pending_req_fr_config_config_dsh_compaction_basic_auto
  假设 插件 dsh-compaction-basic 已注册
  当 读取解析后配置时
  那么 配置行 [auto:true] 的默认值按目录行实现

Scenario: config dsh-compaction-basic auto 约束
  Test: pending_req_fr_config_config_dsh_compaction_basic_auto
  假设 插件 dsh-compaction-basic 已注册
  当 校验与生效时
  那么 配置行 [auto:true] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-compaction-basic thresholdRatio 默认
  Test: pending_req_fr_config_config_dsh_compaction_basic_thresholdratio
  假设 插件 dsh-compaction-basic 已注册
  当 读取解析后配置时
  那么 配置行 [thresholdRatio:0.8] 的默认值按目录行实现

Scenario: config dsh-compaction-basic thresholdRatio 约束
  Test: pending_req_fr_config_config_dsh_compaction_basic_thresholdratio
  假设 插件 dsh-compaction-basic 已注册
  当 校验与生效时
  那么 配置行 [thresholdRatio:0.8] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-compaction-basic headroomTokens 默认
  Test: pending_req_fr_config_config_dsh_compaction_basic_headroomtokens
  假设 插件 dsh-compaction-basic 已注册
  当 读取解析后配置时
  那么 配置行 [headroomTokens:65536] 的默认值按目录行实现

Scenario: config dsh-compaction-basic headroomTokens 约束
  Test: pending_req_fr_config_config_dsh_compaction_basic_headroomtokens
  假设 插件 dsh-compaction-basic 已注册
  当 校验与生效时
  那么 配置行 [headroomTokens:65536] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-compaction-basic retainRatio 默认
  Test: pending_req_fr_config_config_dsh_compaction_basic_retainratio
  假设 插件 dsh-compaction-basic 已注册
  当 读取解析后配置时
  那么 配置行 [retainRatio:0.16（与 retainTokens 互斥）] 的默认值按目录行实现

Scenario: config dsh-compaction-basic retainRatio 约束
  Test: pending_req_fr_config_config_dsh_compaction_basic_retainratio
  假设 插件 dsh-compaction-basic 已注册
  当 校验与生效时
  那么 配置行 [retainRatio:0.16（与 retainTokens 互斥）] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-compaction-basic compactionRetries 默认
  Test: pending_req_fr_config_config_dsh_compaction_basic_compactionretries
  假设 插件 dsh-compaction-basic 已注册
  当 读取解析后配置时
  那么 配置行 [compactionRetries:1] 的默认值按目录行实现

Scenario: config dsh-compaction-basic compactionRetries 约束
  Test: pending_req_fr_config_config_dsh_compaction_basic_compactionretries
  假设 插件 dsh-compaction-basic 已注册
  当 校验与生效时
  那么 配置行 [compactionRetries:1] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-compaction-basic maxOverflowRetries 默认
  Test: pending_req_fr_config_config_dsh_compaction_basic_maxoverflowretries
  假设 插件 dsh-compaction-basic 已注册
  当 读取解析后配置时
  那么 配置行 [maxOverflowRetries:1（0 禁恢复）] 的默认值按目录行实现

Scenario: config dsh-compaction-basic maxOverflowRetries 约束
  Test: pending_req_fr_config_config_dsh_compaction_basic_maxoverflowretries
  假设 插件 dsh-compaction-basic 已注册
  当 校验与生效时
  那么 配置行 [maxOverflowRetries:1（0 禁恢复）] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-compaction-tool-result-pruner thresholdChars 默认
  Test: pending_req_fr_config_config_dsh_compaction_tool_result_pruner_thresholdchars
  假设 插件 dsh-compaction-tool-result-pruner 已注册
  当 读取解析后配置时
  那么 配置行 [thresholdChars:8192（Unicode 码点）] 的默认值按目录行实现

Scenario: config dsh-compaction-tool-result-pruner thresholdChars 约束
  Test: pending_req_fr_config_config_dsh_compaction_tool_result_pruner_thresholdchars
  假设 插件 dsh-compaction-tool-result-pruner 已注册
  当 校验与生效时
  那么 配置行 [thresholdChars:8192（Unicode 码点）] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-compaction-tool-result-pruner headChars 默认
  Test: pending_req_fr_config_config_dsh_compaction_tool_result_pruner_headchars
  假设 插件 dsh-compaction-tool-result-pruner 已注册
  当 读取解析后配置时
  那么 配置行 [headChars:4096] 的默认值按目录行实现

Scenario: config dsh-compaction-tool-result-pruner headChars 约束
  Test: pending_req_fr_config_config_dsh_compaction_tool_result_pruner_headchars
  假设 插件 dsh-compaction-tool-result-pruner 已注册
  当 校验与生效时
  那么 配置行 [headChars:4096] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-compaction-tool-result-pruner tailChars 默认
  Test: pending_req_fr_config_config_dsh_compaction_tool_result_pruner_tailchars
  假设 插件 dsh-compaction-tool-result-pruner 已注册
  当 读取解析后配置时
  那么 配置行 [tailChars:1024] 的默认值按目录行实现

Scenario: config dsh-compaction-tool-result-pruner tailChars 约束
  Test: pending_req_fr_config_config_dsh_compaction_tool_result_pruner_tailchars
  假设 插件 dsh-compaction-tool-result-pruner 已注册
  当 校验与生效时
  那么 配置行 [tailChars:1024] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-credentials-local watch 默认
  Test: pending_req_fr_config_config_dsh_credentials_local_watch
  假设 插件 dsh-credentials-local 已注册
  当 读取解析后配置时
  那么 配置行 [watch:true] 的默认值按目录行实现

Scenario: config dsh-credentials-local watch 约束
  Test: pending_req_fr_config_config_dsh_credentials_local_watch
  假设 插件 dsh-credentials-local 已注册
  当 校验与生效时
  那么 配置行 [watch:true] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-credentials-local debounceMs 默认
  Test: pending_req_fr_config_config_dsh_credentials_local_debouncems
  假设 插件 dsh-credentials-local 已注册
  当 读取解析后配置时
  那么 配置行 [debounceMs:100] 的默认值按目录行实现

Scenario: config dsh-credentials-local debounceMs 约束
  Test: pending_req_fr_config_config_dsh_credentials_local_debouncems
  假设 插件 dsh-credentials-local 已注册
  当 校验与生效时
  那么 配置行 [debounceMs:100] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-headless task 默认
  Test: pending_req_fr_config_config_dsh_headless_task
  假设 插件 dsh-headless 已注册
  当 读取解析后配置时
  那么 配置行 [task:-（省略来自 stdin）] 的默认值按目录行实现

Scenario: config dsh-headless task 约束
  Test: pending_req_fr_config_config_dsh_headless_task
  假设 插件 dsh-headless 已注册
  当 校验与生效时
  那么 配置行 [task:-（省略来自 stdin）] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-headless sessionId 默认
  Test: pending_req_fr_config_config_dsh_headless_sessionid
  假设 插件 dsh-headless 已注册
  当 读取解析后配置时
  那么 配置行 [sessionId:-（无存储会话则失败）] 的默认值按目录行实现

Scenario: config dsh-headless sessionId 约束
  Test: pending_req_fr_config_config_dsh_headless_sessionid
  假设 插件 dsh-headless 已注册
  当 校验与生效时
  那么 配置行 [sessionId:-（无存储会话则失败）] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-headless json 默认
  Test: pending_req_fr_config_config_dsh_headless_json
  假设 插件 dsh-headless 已注册
  当 读取解析后配置时
  那么 配置行 [json:false] 的默认值按目录行实现

Scenario: config dsh-headless json 约束
  Test: pending_req_fr_config_config_dsh_headless_json
  假设 插件 dsh-headless 已注册
  当 校验与生效时
  那么 配置行 [json:false] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-jobs-local maxConcurrentJobsPerOwner 默认
  Test: pending_req_fr_config_config_dsh_jobs_local_maxconcurrentjobsperowner
  假设 插件 dsh-jobs-local 已注册
  当 读取解析后配置时
  那么 配置行 [maxConcurrentJobsPerOwner:10] 的默认值按目录行实现

Scenario: config dsh-jobs-local maxConcurrentJobsPerOwner 约束
  Test: pending_req_fr_config_config_dsh_jobs_local_maxconcurrentjobsperowner
  假设 插件 dsh-jobs-local 已注册
  当 校验与生效时
  那么 配置行 [maxConcurrentJobsPerOwner:10] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-jobs-local retainBytes 默认
  Test: pending_req_fr_config_config_dsh_jobs_local_retainbytes
  假设 插件 dsh-jobs-local 已注册
  当 读取解析后配置时
  那么 配置行 [retainBytes:262144] 的默认值按目录行实现

Scenario: config dsh-jobs-local retainBytes 约束
  Test: pending_req_fr_config_config_dsh_jobs_local_retainbytes
  假设 插件 dsh-jobs-local 已注册
  当 校验与生效时
  那么 配置行 [retainBytes:262144] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-jobs-local settledRetainBytes 默认
  Test: pending_req_fr_config_config_dsh_jobs_local_settledretainbytes
  假设 插件 dsh-jobs-local 已注册
  当 读取解析后配置时
  那么 配置行 [settledRetainBytes:16384] 的默认值按目录行实现

Scenario: config dsh-jobs-local settledRetainBytes 约束
  Test: pending_req_fr_config_config_dsh_jobs_local_settledretainbytes
  假设 插件 dsh-jobs-local 已注册
  当 校验与生效时
  那么 配置行 [settledRetainBytes:16384] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-jobs-local pumpPollMs 默认
  Test: pending_req_fr_config_config_dsh_jobs_local_pumppollms
  假设 插件 dsh-jobs-local 已注册
  当 读取解析后配置时
  那么 配置行 [pumpPollMs:150] 的默认值按目录行实现

Scenario: config dsh-jobs-local pumpPollMs 约束
  Test: pending_req_fr_config_config_dsh_jobs_local_pumppollms
  假设 插件 dsh-jobs-local 已注册
  当 校验与生效时
  那么 配置行 [pumpPollMs:150] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-llm-deepseek-api-key apiKeyEnv 默认
  Test: pending_req_fr_config_config_dsh_llm_deepseek_api_key_apikeyenv
  假设 插件 dsh-llm-deepseek-api-key 已注册
  当 读取解析后配置时
  那么 配置行 [apiKeyEnv:Volatile:'DEEPSEEK_API_KEY'（每请求解析）] 的默认值按目录行实现

Scenario: config dsh-llm-deepseek-api-key apiKeyEnv 约束
  Test: pending_req_fr_config_config_dsh_llm_deepseek_api_key_apikeyenv
  假设 插件 dsh-llm-deepseek-api-key 已注册
  当 校验与生效时
  那么 配置行 [apiKeyEnv:Volatile:'DEEPSEEK_API_KEY'（每请求解析）] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-llm-pi-ai providers 默认
  Test: pending_req_fr_config_config_dsh_llm_pi_ai_providers
  假设 插件 dsh-llm-pi-ai 已注册
  当 读取解析后配置时
  那么 配置行 [providers:Volatile:Record（空=休眠）；per-provider apiKeyEnv/api/baseURL/models/compat/defaultContextWindow:262144/defaultMaxTokens:32768/defaultInput:['text']/headers/retryPolicy（normal 5 次）等] 的默认值按目录行实现

Scenario: config dsh-llm-pi-ai providers 约束
  Test: pending_req_fr_config_config_dsh_llm_pi_ai_providers
  假设 插件 dsh-llm-pi-ai 已注册
  当 校验与生效时
  那么 配置行 [providers:Volatile:Record（空=休眠）；per-provider apiKeyEnv/api/baseURL/models/compat/defaultContextWindow:262144/defaultMaxTokens:32768/defaultInput:['text']/headers/retryPolicy（normal 5 次）等] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-lsp-stdio servers 默认
  Test: pending_req_fr_config_config_dsh_lsp_stdio_servers
  假设 插件 dsh-lsp-stdio 已注册
  当 读取解析后配置时
  那么 配置行 [servers:Record 非空] 的默认值按目录行实现

Scenario: config dsh-lsp-stdio servers 约束
  Test: pending_req_fr_config_config_dsh_lsp_stdio_servers
  假设 插件 dsh-lsp-stdio 已注册
  当 校验与生效时
  那么 配置行 [servers:Record 非空] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-mcp-client（stdio 与 streamable-http 两变体） serverName 默认
  Test: pending_req_fr_config_config_dsh_mcp_client_stdio_streamable_http_servername
  假设 插件 dsh-mcp-client（stdio 与 streamable-http 两变体） 已注册
  当 读取解析后配置时
  那么 配置行 [serverName:-（[A-Za-z0-9_-]{1,32} 且唯一，工具名 mcp__<server>__<raw>）] 的默认值按目录行实现

Scenario: config dsh-mcp-client（stdio 与 streamable-http 两变体） serverName 约束
  Test: pending_req_fr_config_config_dsh_mcp_client_stdio_streamable_http_servername
  假设 插件 dsh-mcp-client（stdio 与 streamable-http 两变体） 已注册
  当 校验与生效时
  那么 配置行 [serverName:-（[A-Za-z0-9_-]{1,32} 且唯一，工具名 mcp__<server>__<raw>）] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-mcp-client（stdio 与 streamable-http 两变体） maxInstructionBytes 默认
  Test: pending_req_fr_config_config_dsh_mcp_client_stdio_streamable_http_maxinstructionbytes
  假设 插件 dsh-mcp-client（stdio 与 streamable-http 两变体） 已注册
  当 读取解析后配置时
  那么 配置行 [maxInstructionBytes:32768] 的默认值按目录行实现

Scenario: config dsh-mcp-client（stdio 与 streamable-http 两变体） maxInstructionBytes 约束
  Test: pending_req_fr_config_config_dsh_mcp_client_stdio_streamable_http_maxinstructionbytes
  假设 插件 dsh-mcp-client（stdio 与 streamable-http 两变体） 已注册
  当 校验与生效时
  那么 配置行 [maxInstructionBytes:32768] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-permission-presets presets 默认
  Test: pending_req_fr_config_config_dsh_permission_presets_presets
  假设 插件 dsh-permission-presets 已注册
  当 读取解析后配置时
  那么 配置行 [presets:Record（默认 workspace-write=workspace-write+ask、danger-full-access=danger+never；custom/auto 保留）] 的默认值按目录行实现

Scenario: config dsh-permission-presets presets 约束
  Test: pending_req_fr_config_config_dsh_permission_presets_presets
  假设 插件 dsh-permission-presets 已注册
  当 校验与生效时
  那么 配置行 [presets:Record（默认 workspace-write=workspace-write+ask、danger-full-access=danger+never；custom/auto 保留）] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-permission-presets defaultPreset 默认
  Test: pending_req_fr_config_config_dsh_permission_presets_defaultpreset
  假设 插件 dsh-permission-presets 已注册
  当 读取解析后配置时
  那么 配置行 [defaultPreset:Volatile] 的默认值按目录行实现

Scenario: config dsh-permission-presets defaultPreset 约束
  Test: pending_req_fr_config_config_dsh_permission_presets_defaultpreset
  假设 插件 dsh-permission-presets 已注册
  当 校验与生效时
  那么 配置行 [defaultPreset:Volatile] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-plan-mode section 默认
  Test: pending_req_fr_config_config_dsh_plan_mode_section
  假设 插件 dsh-plan-mode 已注册
  当 读取解析后配置时
  那么 配置行 [section:string（plan 活动期渲染 plan:policy 段）] 的默认值按目录行实现

Scenario: config dsh-plan-mode section 约束
  Test: pending_req_fr_config_config_dsh_plan_mode_section
  假设 插件 dsh-plan-mode 已注册
  当 校验与生效时
  那么 配置行 [section:string（plan 活动期渲染 plan:policy 段）] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-ptc-runtime-node launch 默认
  Test: pending_req_fr_config_config_dsh_ptc_runtime_node_launch
  假设 插件 dsh-ptc-runtime-node 已注册
  当 读取解析后配置时
  那么 配置行 [launch:node-script|embedded] 的默认值按目录行实现

Scenario: config dsh-ptc-runtime-node launch 约束
  Test: pending_req_fr_config_config_dsh_ptc_runtime_node_launch
  假设 插件 dsh-ptc-runtime-node 已注册
  当 校验与生效时
  那么 配置行 [launch:node-script|embedded] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-ptc-runtime-node executionMode 默认
  Test: pending_req_fr_config_config_dsh_ptc_runtime_node_executionmode
  假设 插件 dsh-ptc-runtime-node 已注册
  当 读取解析后配置时
  那么 配置行 [executionMode:'per-call'|'persistent-in-memory' 默认 per-call] 的默认值按目录行实现

Scenario: config dsh-ptc-runtime-node executionMode 约束
  Test: pending_req_fr_config_config_dsh_ptc_runtime_node_executionmode
  假设 插件 dsh-ptc-runtime-node 已注册
  当 校验与生效时
  那么 配置行 [executionMode:'per-call'|'persistent-in-memory' 默认 per-call] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-repeat-tool-reminder thresholds 默认
  Test: pending_req_fr_config_config_dsh_repeat_tool_reminder_thresholds
  假设 插件 dsh-repeat-tool-reminder 已注册
  当 读取解析后配置时
  那么 配置行 [thresholds:[3,5,8]] 的默认值按目录行实现

Scenario: config dsh-repeat-tool-reminder thresholds 约束
  Test: pending_req_fr_config_config_dsh_repeat_tool_reminder_thresholds
  假设 插件 dsh-repeat-tool-reminder 已注册
  当 校验与生效时
  那么 配置行 [thresholds:[3,5,8]] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-repeat-tool-reminder argumentsPreviewChars 默认
  Test: pending_req_fr_config_config_dsh_repeat_tool_reminder_argumentspreviewchars
  假设 插件 dsh-repeat-tool-reminder 已注册
  当 读取解析后配置时
  那么 配置行 [argumentsPreviewChars:500] 的默认值按目录行实现

Scenario: config dsh-repeat-tool-reminder argumentsPreviewChars 约束
  Test: pending_req_fr_config_config_dsh_repeat_tool_reminder_argumentspreviewchars
  假设 插件 dsh-repeat-tool-reminder 已注册
  当 校验与生效时
  那么 配置行 [argumentsPreviewChars:500] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-sandbox-policy mode 默认
  Test: pending_req_fr_config_config_dsh_sandbox_policy_mode
  假设 插件 dsh-sandbox-policy 已注册
  当 读取解析后配置时
  那么 配置行 [mode:'read-only'（会话起始模式，DSH_PERMISSION_MODE/FR 覆盖）] 的默认值按目录行实现

Scenario: config dsh-sandbox-policy mode 约束
  Test: pending_req_fr_config_config_dsh_sandbox_policy_mode
  假设 插件 dsh-sandbox-policy 已注册
  当 校验与生效时
  那么 配置行 [mode:'read-only'（会话起始模式，DSH_PERMISSION_MODE/FR 覆盖）] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-sandbox-policy workspaceRoot 默认
  Test: pending_req_fr_config_config_dsh_sandbox_policy_workspaceroot
  假设 插件 dsh-sandbox-policy 已注册
  当 读取解析后配置时
  那么 配置行 [workspaceRoot:process.cwd()] 的默认值按目录行实现

Scenario: config dsh-sandbox-policy workspaceRoot 约束
  Test: pending_req_fr_config_config_dsh_sandbox_policy_workspaceroot
  假设 插件 dsh-sandbox-policy 已注册
  当 校验与生效时
  那么 配置行 [workspaceRoot:process.cwd()] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-schedule deliveryHistoryDays 默认
  Test: pending_req_fr_config_config_dsh_schedule_deliveryhistorydays
  假设 插件 dsh-schedule 已注册
  当 读取解析后配置时
  那么 配置行 [deliveryHistoryDays:30] 的默认值按目录行实现

Scenario: config dsh-schedule deliveryHistoryDays 约束
  Test: pending_req_fr_config_config_dsh_schedule_deliveryhistorydays
  假设 插件 dsh-schedule 已注册
  当 校验与生效时
  那么 配置行 [deliveryHistoryDays:30] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-schedule deliveryHistoryRecords 默认
  Test: pending_req_fr_config_config_dsh_schedule_deliveryhistoryrecords
  假设 插件 dsh-schedule 已注册
  当 读取解析后配置时
  那么 配置行 [deliveryHistoryRecords:200] 的默认值按目录行实现

Scenario: config dsh-schedule deliveryHistoryRecords 约束
  Test: pending_req_fr_config_config_dsh_schedule_deliveryhistoryrecords
  假设 插件 dsh-schedule 已注册
  当 校验与生效时
  那么 配置行 [deliveryHistoryRecords:200] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-sdk-jsonrpc-server maxTokensAsSuccess 默认
  Test: pending_req_fr_config_config_dsh_sdk_jsonrpc_server_maxtokensassuccess
  假设 插件 dsh-sdk-jsonrpc-server 已注册
  当 读取解析后配置时
  那么 配置行 [maxTokensAsSuccess:-（默认 false）] 的默认值按目录行实现

Scenario: config dsh-sdk-jsonrpc-server maxTokensAsSuccess 约束
  Test: pending_req_fr_config_config_dsh_sdk_jsonrpc_server_maxtokensassuccess
  假设 插件 dsh-sdk-jsonrpc-server 已注册
  当 校验与生效时
  那么 配置行 [maxTokensAsSuccess:-（默认 false）] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-session-persistence-jsonl root 默认
  Test: pending_req_fr_config_config_dsh_session_persistence_jsonl_root
  假设 插件 dsh-session-persistence-jsonl 已注册
  当 读取解析后配置时
  那么 配置行 [root:-必填（无默认，防 cwd 漂移）] 的默认值按目录行实现

Scenario: config dsh-session-persistence-jsonl root 约束
  Test: pending_req_fr_config_config_dsh_session_persistence_jsonl_root
  假设 插件 dsh-session-persistence-jsonl 已注册
  当 校验与生效时
  那么 配置行 [root:-必填（无默认，防 cwd 漂移）] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-session-persistence-jsonl listConcurrency 默认
  Test: pending_req_fr_config_config_dsh_session_persistence_jsonl_listconcurrency
  假设 插件 dsh-session-persistence-jsonl 已注册
  当 读取解析后配置时
  那么 配置行 [listConcurrency:3] 的默认值按目录行实现

Scenario: config dsh-session-persistence-jsonl listConcurrency 约束
  Test: pending_req_fr_config_config_dsh_session_persistence_jsonl_listconcurrency
  假设 插件 dsh-session-persistence-jsonl 已注册
  当 校验与生效时
  那么 配置行 [listConcurrency:3] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-session-query-sqlite journalMode 默认
  Test: pending_req_fr_config_config_dsh_session_query_sqlite_journalmode
  假设 插件 dsh-session-query-sqlite 已注册
  当 读取解析后配置时
  那么 配置行 [journalMode:'wal'] 的默认值按目录行实现

Scenario: config dsh-session-query-sqlite journalMode 约束
  Test: pending_req_fr_config_config_dsh_session_query_sqlite_journalmode
  假设 插件 dsh-session-query-sqlite 已注册
  当 校验与生效时
  那么 配置行 [journalMode:'wal'] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-session-query-sqlite defaultLimit 默认
  Test: pending_req_fr_config_config_dsh_session_query_sqlite_defaultlimit
  假设 插件 dsh-session-query-sqlite 已注册
  当 读取解析后配置时
  那么 配置行 [defaultLimit:20] 的默认值按目录行实现

Scenario: config dsh-session-query-sqlite defaultLimit 约束
  Test: pending_req_fr_config_config_dsh_session_query_sqlite_defaultlimit
  假设 插件 dsh-session-query-sqlite 已注册
  当 校验与生效时
  那么 配置行 [defaultLimit:20] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-session-query-sqlite maxLimit 默认
  Test: pending_req_fr_config_config_dsh_session_query_sqlite_maxlimit
  假设 插件 dsh-session-query-sqlite 已注册
  当 读取解析后配置时
  那么 配置行 [maxLimit:100] 的默认值按目录行实现

Scenario: config dsh-session-query-sqlite maxLimit 约束
  Test: pending_req_fr_config_config_dsh_session_query_sqlite_maxlimit
  假设 插件 dsh-session-query-sqlite 已注册
  当 校验与生效时
  那么 配置行 [maxLimit:100] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-session-query-sqlite snippetChars 默认
  Test: pending_req_fr_config_config_dsh_session_query_sqlite_snippetchars
  假设 插件 dsh-session-query-sqlite 已注册
  当 读取解析后配置时
  那么 配置行 [snippetChars:240] 的默认值按目录行实现

Scenario: config dsh-session-query-sqlite snippetChars 约束
  Test: pending_req_fr_config_config_dsh_session_query_sqlite_snippetchars
  假设 插件 dsh-session-query-sqlite 已注册
  当 校验与生效时
  那么 配置行 [snippetChars:240] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-session-query-sqlite persistedReadConcurrency 默认
  Test: pending_req_fr_config_config_dsh_session_query_sqlite_persistedreadconcurrency
  假设 插件 dsh-session-query-sqlite 已注册
  当 读取解析后配置时
  那么 配置行 [persistedReadConcurrency:4] 的默认值按目录行实现

Scenario: config dsh-session-query-sqlite persistedReadConcurrency 约束
  Test: pending_req_fr_config_config_dsh_session_query_sqlite_persistedreadconcurrency
  假设 插件 dsh-session-query-sqlite 已注册
  当 校验与生效时
  那么 配置行 [persistedReadConcurrency:4] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-session-query-sqlite preparedSessionCacheSize 默认
  Test: pending_req_fr_config_config_dsh_session_query_sqlite_preparedsessioncachesize
  假设 插件 dsh-session-query-sqlite 已注册
  当 读取解析后配置时
  那么 配置行 [preparedSessionCacheSize:5] 的默认值按目录行实现

Scenario: config dsh-session-query-sqlite preparedSessionCacheSize 约束
  Test: pending_req_fr_config_config_dsh_session_query_sqlite_preparedsessioncachesize
  假设 插件 dsh-session-query-sqlite 已注册
  当 校验与生效时
  那么 配置行 [preparedSessionCacheSize:5] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-skill-filesystem providerName 默认
  Test: pending_req_fr_config_config_dsh_skill_filesystem_providername
  假设 插件 dsh-skill-filesystem 已注册
  当 读取解析后配置时
  那么 配置行 [providerName:'filesystem'] 的默认值按目录行实现

Scenario: config dsh-skill-filesystem providerName 约束
  Test: pending_req_fr_config_config_dsh_skill_filesystem_providername
  假设 插件 dsh-skill-filesystem 已注册
  当 校验与生效时
  那么 配置行 [providerName:'filesystem'] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-spill-local cleanupPeriodDays 默认
  Test: pending_req_fr_config_config_dsh_spill_local_cleanupperioddays
  假设 插件 dsh-spill-local 已注册
  当 读取解析后配置时
  那么 配置行 [cleanupPeriodDays:30（0 禁清理）] 的默认值按目录行实现

Scenario: config dsh-spill-local cleanupPeriodDays 约束
  Test: pending_req_fr_config_config_dsh_spill_local_cleanupperioddays
  假设 插件 dsh-spill-local 已注册
  当 校验与生效时
  那么 配置行 [cleanupPeriodDays:30（0 禁清理）] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-spill-policy maxInlineTokens 默认
  Test: pending_req_fr_config_config_dsh_spill_policy_maxinlinetokens
  假设 插件 dsh-spill-policy 已注册
  当 读取解析后配置时
  那么 配置行 [maxInlineTokens:-（省略禁保留）] 的默认值按目录行实现

Scenario: config dsh-spill-policy maxInlineTokens 约束
  Test: pending_req_fr_config_config_dsh_spill_policy_maxinlinetokens
  假设 插件 dsh-spill-policy 已注册
  当 校验与生效时
  那么 配置行 [maxInlineTokens:-（省略禁保留）] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-subagent maxActiveSubagents 默认
  Test: pending_req_fr_config_config_dsh_subagent_maxactivesubagents
  假设 插件 dsh-subagent 已注册
  当 读取解析后配置时
  那么 配置行 [maxActiveSubagents:Volatile:8] 的默认值按目录行实现

Scenario: config dsh-subagent maxActiveSubagents 约束
  Test: pending_req_fr_config_config_dsh_subagent_maxactivesubagents
  假设 插件 dsh-subagent 已注册
  当 校验与生效时
  那么 配置行 [maxActiveSubagents:Volatile:8] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-subagent maxDepth 默认
  Test: pending_req_fr_config_config_dsh_subagent_maxdepth
  假设 插件 dsh-subagent 已注册
  当 读取解析后配置时
  那么 配置行 [maxDepth:Volatile:1] 的默认值按目录行实现

Scenario: config dsh-subagent maxDepth 约束
  Test: pending_req_fr_config_config_dsh_subagent_maxdepth
  假设 插件 dsh-subagent 已注册
  当 校验与生效时
  那么 配置行 [maxDepth:Volatile:1] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-subagent-acp providerName 默认
  Test: pending_req_fr_config_config_dsh_subagent_acp_providername
  假设 插件 dsh-subagent-acp 已注册
  当 读取解析后配置时
  那么 配置行 [providerName:'acp'] 的默认值按目录行实现

Scenario: config dsh-subagent-acp providerName 约束
  Test: pending_req_fr_config_config_dsh_subagent_acp_providername
  假设 插件 dsh-subagent-acp 已注册
  当 校验与生效时
  那么 配置行 [providerName:'acp'] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-system-prompt includeHarnessIdentity 默认
  Test: pending_req_fr_config_config_dsh_system_prompt_includeharnessidentity
  假设 插件 dsh-system-prompt 已注册
  当 读取解析后配置时
  那么 配置行 [includeHarnessIdentity:true] 的默认值按目录行实现

Scenario: config dsh-system-prompt includeHarnessIdentity 约束
  Test: pending_req_fr_config_config_dsh_system_prompt_includeharnessidentity
  假设 插件 dsh-system-prompt 已注册
  当 校验与生效时
  那么 配置行 [includeHarnessIdentity:true] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-system-prompt includeRuntimeContext 默认
  Test: pending_req_fr_config_config_dsh_system_prompt_includeruntimecontext
  假设 插件 dsh-system-prompt 已注册
  当 读取解析后配置时
  那么 配置行 [includeRuntimeContext:true] 的默认值按目录行实现

Scenario: config dsh-system-prompt includeRuntimeContext 约束
  Test: pending_req_fr_config_config_dsh_system_prompt_includeruntimecontext
  假设 插件 dsh-system-prompt 已注册
  当 校验与生效时
  那么 配置行 [includeRuntimeContext:true] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-terminal-bash backendType 默认
  Test: pending_req_fr_config_config_dsh_terminal_bash_backendtype
  假设 插件 dsh-terminal-bash 已注册
  当 读取解析后配置时
  那么 配置行 [backendType:'shell'] 的默认值按目录行实现

Scenario: config dsh-terminal-bash backendType 约束
  Test: pending_req_fr_config_config_dsh_terminal_bash_backendtype
  假设 插件 dsh-terminal-bash 已注册
  当 校验与生效时
  那么 配置行 [backendType:'shell'] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-terminal-bash shellDialect 默认
  Test: pending_req_fr_config_config_dsh_terminal_bash_shelldialect
  假设 插件 dsh-terminal-bash 已注册
  当 读取解析后配置时
  那么 配置行 [shellDialect:'bash'] 的默认值按目录行实现

Scenario: config dsh-terminal-bash shellDialect 约束
  Test: pending_req_fr_config_config_dsh_terminal_bash_shelldialect
  假设 插件 dsh-terminal-bash 已注册
  当 校验与生效时
  那么 配置行 [shellDialect:'bash'] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-time-context refreshIntervalMs 默认
  Test: pending_req_fr_config_config_dsh_time_context_refreshintervalms
  假设 插件 dsh-time-context 已注册
  当 读取解析后配置时
  那么 配置行 [refreshIntervalMs:600000（0=每合格 step 注入）] 的默认值按目录行实现

Scenario: config dsh-time-context refreshIntervalMs 约束
  Test: pending_req_fr_config_config_dsh_time_context_refreshintervalms
  假设 插件 dsh-time-context 已注册
  当 校验与生效时
  那么 配置行 [refreshIntervalMs:600000（0=每合格 step 注入）] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-tool-ask-user mode 默认
  Test: pending_req_fr_config_config_dsh_tool_ask_user_mode
  假设 插件 dsh-tool-ask-user 已注册
  当 读取解析后配置时
  那么 配置行 [mode:'legacy'|'timed'（默认阻塞 legacy）] 的默认值按目录行实现

Scenario: config dsh-tool-ask-user mode 约束
  Test: pending_req_fr_config_config_dsh_tool_ask_user_mode
  假设 插件 dsh-tool-ask-user 已注册
  当 校验与生效时
  那么 配置行 [mode:'legacy'|'timed'（默认阻塞 legacy）] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-tool-ask-user timeout 默认
  Test: pending_req_fr_config_config_dsh_tool_ask_user_timeout
  假设 插件 dsh-tool-ask-user 已注册
  当 读取解析后配置时
  那么 配置行 [timeout:120s] 的默认值按目录行实现

Scenario: config dsh-tool-ask-user timeout 约束
  Test: pending_req_fr_config_config_dsh_tool_ask_user_timeout
  假设 插件 dsh-tool-ask-user 已注册
  当 校验与生效时
  那么 配置行 [timeout:120s] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-tool-bash / dsh-tool-pwsh enableRunInBackground 默认
  Test: pending_req_fr_config_config_dsh_tool_bash_dsh_tool_pwsh_enableruninbackground
  假设 插件 dsh-tool-bash / dsh-tool-pwsh 已注册
  当 读取解析后配置时
  那么 配置行 [enableRunInBackground:true] 的默认值按目录行实现

Scenario: config dsh-tool-bash / dsh-tool-pwsh enableRunInBackground 约束
  Test: pending_req_fr_config_config_dsh_tool_bash_dsh_tool_pwsh_enableruninbackground
  假设 插件 dsh-tool-bash / dsh-tool-pwsh 已注册
  当 校验与生效时
  那么 配置行 [enableRunInBackground:true] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-tool-bash / dsh-tool-pwsh promoteOnTimeout 默认
  Test: pending_req_fr_config_config_dsh_tool_bash_dsh_tool_pwsh_promoteontimeout
  假设 插件 dsh-tool-bash / dsh-tool-pwsh 已注册
  当 读取解析后配置时
  那么 配置行 [promoteOnTimeout:true（超时转后台 job 不杀；仅后台可用时生效）] 的默认值按目录行实现

Scenario: config dsh-tool-bash / dsh-tool-pwsh promoteOnTimeout 约束
  Test: pending_req_fr_config_config_dsh_tool_bash_dsh_tool_pwsh_promoteontimeout
  假设 插件 dsh-tool-bash / dsh-tool-pwsh 已注册
  当 校验与生效时
  那么 配置行 [promoteOnTimeout:true（超时转后台 job 不杀；仅后台可用时生效）] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-tool-bash-persistent / dsh-tool-pwsh-persistent backendType 默认
  Test: pending_req_fr_config_config_dsh_tool_bash_persistent_dsh_tool_pwsh_persistent_backendtype
  假设 插件 dsh-tool-bash-persistent / dsh-tool-pwsh-persistent 已注册
  当 读取解析后配置时
  那么 配置行 [backendType:'shell'] 的默认值按目录行实现

Scenario: config dsh-tool-bash-persistent / dsh-tool-pwsh-persistent backendType 约束
  Test: pending_req_fr_config_config_dsh_tool_bash_persistent_dsh_tool_pwsh_persistent_backendtype
  假设 插件 dsh-tool-bash-persistent / dsh-tool-pwsh-persistent 已注册
  当 校验与生效时
  那么 配置行 [backendType:'shell'] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-tool-bash-persistent / dsh-tool-pwsh-persistent timeoutMs 默认
  Test: pending_req_fr_config_config_dsh_tool_bash_persistent_dsh_tool_pwsh_persistent_timeoutms
  假设 插件 dsh-tool-bash-persistent / dsh-tool-pwsh-persistent 已注册
  当 读取解析后配置时
  那么 配置行 [timeoutMs:300000] 的默认值按目录行实现

Scenario: config dsh-tool-bash-persistent / dsh-tool-pwsh-persistent timeoutMs 约束
  Test: pending_req_fr_config_config_dsh_tool_bash_persistent_dsh_tool_pwsh_persistent_timeoutms
  假设 插件 dsh-tool-bash-persistent / dsh-tool-pwsh-persistent 已注册
  当 校验与生效时
  那么 配置行 [timeoutMs:300000] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-tool-bash-persistent / dsh-tool-pwsh-persistent maxOutputChars 默认
  Test: pending_req_fr_config_config_dsh_tool_bash_persistent_dsh_tool_pwsh_persistent_maxoutputchars
  假设 插件 dsh-tool-bash-persistent / dsh-tool-pwsh-persistent 已注册
  当 读取解析后配置时
  那么 配置行 [maxOutputChars:16000] 的默认值按目录行实现

Scenario: config dsh-tool-bash-persistent / dsh-tool-pwsh-persistent maxOutputChars 约束
  Test: pending_req_fr_config_config_dsh_tool_bash_persistent_dsh_tool_pwsh_persistent_maxoutputchars
  假设 插件 dsh-tool-bash-persistent / dsh-tool-pwsh-persistent 已注册
  当 校验与生效时
  那么 配置行 [maxOutputChars:16000] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-tool-fs-search sampleOverCapGlobResults 默认
  Test: pending_req_fr_config_config_dsh_tool_fs_search_sampleovercapglobresults
  假设 插件 dsh-tool-fs-search 已注册
  当 读取解析后配置时
  那么 配置行 [sampleOverCapGlobResults:必填] 的默认值按目录行实现

Scenario: config dsh-tool-fs-search sampleOverCapGlobResults 约束
  Test: pending_req_fr_config_config_dsh_tool_fs_search_sampleovercapglobresults
  假设 插件 dsh-tool-fs-search 已注册
  当 校验与生效时
  那么 配置行 [sampleOverCapGlobResults:必填] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-tool-jobs waitTimeoutMs 默认
  Test: pending_req_fr_config_config_dsh_tool_jobs_waittimeoutms
  假设 插件 dsh-tool-jobs 已注册
  当 读取解析后配置时
  那么 配置行 [waitTimeoutMs:30s] 的默认值按目录行实现

Scenario: config dsh-tool-jobs waitTimeoutMs 约束
  Test: pending_req_fr_config_config_dsh_tool_jobs_waittimeoutms
  假设 插件 dsh-tool-jobs 已注册
  当 校验与生效时
  那么 配置行 [waitTimeoutMs:30s] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-tool-jobs maxWaitTimeoutMs 默认
  Test: pending_req_fr_config_config_dsh_tool_jobs_maxwaittimeoutms
  假设 插件 dsh-tool-jobs 已注册
  当 读取解析后配置时
  那么 配置行 [maxWaitTimeoutMs:10min] 的默认值按目录行实现

Scenario: config dsh-tool-jobs maxWaitTimeoutMs 约束
  Test: pending_req_fr_config_config_dsh_tool_jobs_maxwaittimeoutms
  假设 插件 dsh-tool-jobs 已注册
  当 校验与生效时
  那么 配置行 [maxWaitTimeoutMs:10min] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-tool-jobs completionDelivery 默认
  Test: pending_req_fr_config_config_dsh_tool_jobs_completiondelivery
  假设 插件 dsh-tool-jobs 已注册
  当 读取解析后配置时
  那么 配置行 [completionDelivery:'wakeup'] 的默认值按目录行实现

Scenario: config dsh-tool-jobs completionDelivery 约束
  Test: pending_req_fr_config_config_dsh_tool_jobs_completiondelivery
  假设 插件 dsh-tool-jobs 已注册
  当 校验与生效时
  那么 配置行 [completionDelivery:'wakeup'] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-tool-lsp maxLocations 默认
  Test: pending_req_fr_config_config_dsh_tool_lsp_maxlocations
  假设 插件 dsh-tool-lsp 已注册
  当 读取解析后配置时
  那么 配置行 [maxLocations:100] 的默认值按目录行实现

Scenario: config dsh-tool-lsp maxLocations 约束
  Test: pending_req_fr_config_config_dsh_tool_lsp_maxlocations
  假设 插件 dsh-tool-lsp 已注册
  当 校验与生效时
  那么 配置行 [maxLocations:100] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-tool-lsp maxResultChars 默认
  Test: pending_req_fr_config_config_dsh_tool_lsp_maxresultchars
  假设 插件 dsh-tool-lsp 已注册
  当 读取解析后配置时
  那么 配置行 [maxResultChars:16000] 的默认值按目录行实现

Scenario: config dsh-tool-lsp maxResultChars 约束
  Test: pending_req_fr_config_config_dsh_tool_lsp_maxresultchars
  假设 插件 dsh-tool-lsp 已注册
  当 校验与生效时
  那么 配置行 [maxResultChars:16000] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-tool-lsp timeoutMs 默认
  Test: pending_req_fr_config_config_dsh_tool_lsp_timeoutms
  假设 插件 dsh-tool-lsp 已注册
  当 读取解析后配置时
  那么 配置行 [timeoutMs:60000] 的默认值按目录行实现

Scenario: config dsh-tool-lsp timeoutMs 约束
  Test: pending_req_fr_config_config_dsh_tool_lsp_timeoutms
  假设 插件 dsh-tool-lsp 已注册
  当 校验与生效时
  那么 配置行 [timeoutMs:60000] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-tool-session-query maxSearchResults 默认
  Test: pending_req_fr_config_config_dsh_tool_session_query_maxsearchresults
  假设 插件 dsh-tool-session-query 已注册
  当 读取解析后配置时
  那么 配置行 [maxSearchResults:100] 的默认值按目录行实现

Scenario: config dsh-tool-session-query maxSearchResults 约束
  Test: pending_req_fr_config_config_dsh_tool_session_query_maxsearchresults
  假设 插件 dsh-tool-session-query 已注册
  当 校验与生效时
  那么 配置行 [maxSearchResults:100] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-tool-session-query searchTimeoutMs 默认
  Test: pending_req_fr_config_config_dsh_tool_session_query_searchtimeoutms
  假设 插件 dsh-tool-session-query 已注册
  当 读取解析后配置时
  那么 配置行 [searchTimeoutMs:30000] 的默认值按目录行实现

Scenario: config dsh-tool-session-query searchTimeoutMs 约束
  Test: pending_req_fr_config_config_dsh_tool_session_query_searchtimeoutms
  假设 插件 dsh-tool-session-query 已注册
  当 校验与生效时
  那么 配置行 [searchTimeoutMs:30000] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-tool-str-replace-editor maxOutputChars 默认
  Test: pending_req_fr_config_config_dsh_tool_str_replace_editor_maxoutputchars
  假设 插件 dsh-tool-str-replace-editor 已注册
  当 读取解析后配置时
  那么 配置行 [maxOutputChars:16000] 的默认值按目录行实现

Scenario: config dsh-tool-str-replace-editor maxOutputChars 约束
  Test: pending_req_fr_config_config_dsh_tool_str_replace_editor_maxoutputchars
  假设 插件 dsh-tool-str-replace-editor 已注册
  当 校验与生效时
  那么 配置行 [maxOutputChars:16000] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-tool-subagent provider 默认
  Test: pending_req_fr_config_config_dsh_tool_subagent_provider
  假设 插件 dsh-tool-subagent 已注册
  当 读取解析后配置时
  那么 配置行 [provider:必填] 的默认值按目录行实现

Scenario: config dsh-tool-subagent provider 约束
  Test: pending_req_fr_config_config_dsh_tool_subagent_provider
  假设 插件 dsh-tool-subagent 已注册
  当 校验与生效时
  那么 配置行 [provider:必填] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-tool-subagent toolName 默认
  Test: pending_req_fr_config_config_dsh_tool_subagent_toolname
  假设 插件 dsh-tool-subagent 已注册
  当 读取解析后配置时
  那么 配置行 [toolName:'subagent'（实例名互异）] 的默认值按目录行实现

Scenario: config dsh-tool-subagent toolName 约束
  Test: pending_req_fr_config_config_dsh_tool_subagent_toolname
  假设 插件 dsh-tool-subagent 已注册
  当 校验与生效时
  那么 配置行 [toolName:'subagent'（实例名互异）] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-tool-terminal enableRunInBackground 默认
  Test: pending_req_fr_config_config_dsh_tool_terminal_enableruninbackground
  假设 插件 dsh-tool-terminal 已注册
  当 读取解析后配置时
  那么 配置行 [enableRunInBackground:true] 的默认值按目录行实现

Scenario: config dsh-tool-terminal enableRunInBackground 约束
  Test: pending_req_fr_config_config_dsh_tool_terminal_enableruninbackground
  假设 插件 dsh-tool-terminal 已注册
  当 校验与生效时
  那么 配置行 [enableRunInBackground:true] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-tool-todo allowParallelInProgress 默认
  Test: pending_req_fr_config_config_dsh_tool_todo_allowparallelinprogress
  假设 插件 dsh-tool-todo 已注册
  当 读取解析后配置时
  那么 配置行 [allowParallelInProgress:必填部署选择（true=多 in_progress；false=恰一且多标拒绝）] 的默认值按目录行实现

Scenario: config dsh-tool-todo allowParallelInProgress 约束
  Test: pending_req_fr_config_config_dsh_tool_todo_allowparallelinprogress
  假设 插件 dsh-tool-todo 已注册
  当 校验与生效时
  那么 配置行 [allowParallelInProgress:必填部署选择（true=多 in_progress；false=恰一且多标拒绝）] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-tool-web search 默认
  Test: pending_req_fr_config_config_dsh_tool_web_search
  假设 插件 dsh-tool-web 已注册
  当 读取解析后配置时
  那么 配置行 [search:true] 的默认值按目录行实现

Scenario: config dsh-tool-web search 约束
  Test: pending_req_fr_config_config_dsh_tool_web_search
  假设 插件 dsh-tool-web 已注册
  当 校验与生效时
  那么 配置行 [search:true] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-tool-web fetch 默认
  Test: pending_req_fr_config_config_dsh_tool_web_fetch
  假设 插件 dsh-tool-web 已注册
  当 读取解析后配置时
  那么 配置行 [fetch:true] 的默认值按目录行实现

Scenario: config dsh-tool-web fetch 约束
  Test: pending_req_fr_config_config_dsh_tool_web_fetch
  假设 插件 dsh-tool-web 已注册
  当 校验与生效时
  那么 配置行 [fetch:true] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-tool-web fetchTimeoutMs 默认
  Test: pending_req_fr_config_config_dsh_tool_web_fetchtimeoutms
  假设 插件 dsh-tool-web 已注册
  当 读取解析后配置时
  那么 配置行 [fetchTimeoutMs:30000] 的默认值按目录行实现

Scenario: config dsh-tool-web fetchTimeoutMs 约束
  Test: pending_req_fr_config_config_dsh_tool_web_fetchtimeoutms
  假设 插件 dsh-tool-web 已注册
  当 校验与生效时
  那么 配置行 [fetchTimeoutMs:30000] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-tool-web searchTimeoutMs 默认
  Test: pending_req_fr_config_config_dsh_tool_web_searchtimeoutms
  假设 插件 dsh-tool-web 已注册
  当 读取解析后配置时
  那么 配置行 [searchTimeoutMs:30000] 的默认值按目录行实现

Scenario: config dsh-tool-web searchTimeoutMs 约束
  Test: pending_req_fr_config_config_dsh_tool_web_searchtimeoutms
  假设 插件 dsh-tool-web 已注册
  当 校验与生效时
  那么 配置行 [searchTimeoutMs:30000] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-tool-web fetchMaxOutputChars 默认
  Test: pending_req_fr_config_config_dsh_tool_web_fetchmaxoutputchars
  假设 插件 dsh-tool-web 已注册
  当 读取解析后配置时
  那么 配置行 [fetchMaxOutputChars:200000] 的默认值按目录行实现

Scenario: config dsh-tool-web fetchMaxOutputChars 约束
  Test: pending_req_fr_config_config_dsh_tool_web_fetchmaxoutputchars
  假设 插件 dsh-tool-web 已注册
  当 校验与生效时
  那么 配置行 [fetchMaxOutputChars:200000] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-web-deepseek-api / dsh-web-fetch-http count 默认
  Test: pending_req_fr_config_config_dsh_web_deepseek_api_dsh_web_fetch_http_count
  假设 插件 dsh-web-deepseek-api / dsh-web-fetch-http 已注册
  当 读取解析后配置时
  那么 配置行 [count:10（1..50）] 的默认值按目录行实现

Scenario: config dsh-web-deepseek-api / dsh-web-fetch-http count 约束
  Test: pending_req_fr_config_config_dsh_web_deepseek_api_dsh_web_fetch_http_count
  假设 插件 dsh-web-deepseek-api / dsh-web-fetch-http 已注册
  当 校验与生效时
  那么 配置行 [count:10（1..50）] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: config dsh-web-deepseek-api / dsh-web-fetch-http sendOriginalQuestion 默认
  Test: pending_req_fr_config_config_dsh_web_deepseek_api_dsh_web_fetch_http_sendoriginalquestion
  假设 插件 dsh-web-deepseek-api / dsh-web-fetch-http 已注册
  当 读取解析后配置时
  那么 配置行 [sendOriginalQuestion:true] 的默认值按目录行实现

Scenario: config dsh-web-deepseek-api / dsh-web-fetch-http sendOriginalQuestion 约束
  Test: pending_req_fr_config_config_dsh_web_deepseek_api_dsh_web_fetch_http_sendoriginalquestion
  假设 插件 dsh-web-deepseek-api / dsh-web-fetch-http 已注册
  当 校验与生效时
  那么 配置行 [sendOriginalQuestion:true] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Scenario: 必需 id 失败拒启动与其余继续
  Test: pending_req_fr_config_id
  假设 agent-loop 激活失败、另一非必需工具插件同时失败
  当 启动结算时
  那么 以 StartupError 拒启动并列出失败与等待清单（含 agent-loop），而非必需者仅单条警告

Scenario: 缺注入服务 pending
  Test: pending_req_fr_config_pending
  假设 某可选插件声明的注入服务未由任何插件提供
  当 装载时
  那么 该插件 fiber 置 pending 容忍不中止装配；同情形发生在必需 id 上则致命

Scenario: 整行替换非键级合并
  Test: config_override_replaces_wholesale
  假设 后层补丁行与前层同 id 但仅给出部分 config 键
  当 展开装配时
  那么 该行整体以后层版本生效，前层同 id 行的其余 config 键不参与合并

Scenario: env 缺省表达式映射
  Test: env_default_chain_matches_dsh_semantics
  假设 FR_PERMISSION_MODE 分别设置与未设置
  当 求值受限子集的 env 缺省表达式时
  那么 结果与 dsh !!js process.env.DSH_PERMISSION_MODE ?? 'workspace-write' 逐值一致

Scenario: 三层优先序
  Test: layered_three_layer_precedence_and_no_override
  假设 同名变量同时出现在进程环境、项目 .env 与家目录 .env
  当 fr-config 启动解析时
  那么 进程 env 值胜出，其次项目 .env，最后家目录 .env

Scenario: 已设名不被覆盖
  Test: layered_three_layer_precedence_and_no_override
  假设 某变量已由进程环境设定且项目 .env 试图赋不同值
  当 解析 .env 时
  那么 项目 .env 的值被忽略、不替换既有值

Scenario: bootstrap 专名拒绝与代理例外
  Test: pending_req_fr_config_bootstrap
  假设 项目 .env 试图设 FR_SESSIONS_ROOT 与 PATH，家目录 .env 试图设 HTTPS_PROXY
  当 解析各层时
  那么 前两者在任何 .env 层均拒绝（FR_ 前缀与 bootstrap 专名）；家目录层的 HTTPS_PROXY 依例外接受，项目层设代理四键仍拒绝

Scenario: 任一层违例两文件均不应用
  Test: layered_atomic_rejection_applies_neither_file
  假设 项目 .env 含合法变量，家目录 .env 含 FR_ 前缀违例名
  当 启动解析分层环境时
  那么 启动终止报错，且项目 .env 的合法值也不应用

Scenario: 读错误非 ENOENT 警告跳层
  Test: layer_missing_file_is_silent_and_read_error_warns_then_skips
  假设 某 .env 路径存在但不可读（如为目录）
  当 解析该层时
  那么 stderr 输出一行警告，该层按空层跳过，启动继续

Scenario: 专名检查大小写不敏感
  Test: bootstrap_names_and_prefixes_case_insensitive
  假设 项目 .env 以小写 path= 设置变量
  当 bootstrap 校验时
  那么 该行拒绝，启动终止

Scenario: cwd 即家目录时家层不读
  Test: layered_cwd_home_dedup_reads_directory_once
  假设 进程 cwd 与 FR_HOME 解析为同一路径
  当 启动解析分层环境时
  那么 家目录层不读取，仅项目层生效一次

Scenario: materialize 与环境快照双轨
  Test: pending_req_fr_config_materialize
  假设 项目 .env 设未被进程环境定义的变量 X
  当 分层解析完成后
  那么 进程 env 可读到 X，且返回的快照按 process > project-env > user-env 层序提供逐名来源查询

Scenario: env 解析方言对齐 Node util.parseEnv
  Test: parser_matches_node_parse_env_official_fixture
  假设 .env 含 # 注释行、单引号与双引号值、双引号内转义与跨行多行值
  当 解析该文件时
  那么 各形态结果与 Node util.parseEnv 方言逐值一致（键值去包裹、转义展开、多行保留）

Scenario: 块标量字符串含 tag 形文本逐字装载
  Test: block_scalar_tag_shaped_text_mounts_verbatim
  假设 patch 行 config 含块标量字符串，其内容行形如 key: !!js v
  当 解析该 patch 文档时
  那么 字符串值逐字为该文本内容，不翻转不求值

Scenario: 引号多行标量含 tag 形文本逐字装载
  Test: quoted_multiline_tag_shaped_text_mounts_verbatim
  假设 patch 行 config 含引号多行标量，其内容行形如 key: !!js v
  当 解析该 patch 文档时
  那么 字符串值逐字为该文本内容，不翻转不求值

Scenario: 识别位外结构值位 fail-closed（flow 式）
  Test: flow_style_js_tag_is_rejected_fail_closed
  假设 !!js 出现在 flow 式映射值位
  当 解析 patch 文档时
  那么 整个文档被拒绝，错误含 outside a recognized value position 文案

Scenario: 异形键结构值位真实标签 fail-closed
  Test: exotic_structural_key_js_tag_fails_closed
  假设 异形键（含 `.` 等非常规字符的键）的结构值位携带真实 `!!js` 标签
  当 解析 patch 文档时
  那么 整个文档被拒绝，错误含 outside a recognized value position 文案

Scenario: dump 内容行单叹号 js 逐字且整篇再 parse 等于挂载
  Test: dump_content_js_shaped_lines_survive_round_trip
  假设 某行字符串值内容含形如 key: !js v 的行，且同树另有真实 !!js 表达式
  当 render_dump 输出时
  那么 内容行保持单叹号逐字，表达式行打印 !!js，输出经结构感知解析等于合成树

## Questions

- Source trace: doc/prd.md#6.1: 装配 = 有序 bundle 栈；按键覆盖合并语义与装载文件布局（2026-10-22 Q1-Q5 勘误后）, doc/prd.md#6.2: 编译产物为 dougong loader 文档；"分层、行覆盖、YAML 在 dougong 中一概不存在——它们全部是 fr-config 的职责", doc/prd.md#6.3: schema 真源为 fr-schema 声明式描述树（单一源派生 validate 与 to_json_schema；装配期查无 schema 跳过、失败进 failures 不阻塞；双侧校验）；fr dump-config / dump-config-schema, doc/prd.md#6.4: 关键默认值表, confidence: high（PRD 第 6 节字段级规格，dougong 事实已一手核实）, doc/dsh-source-notes.md: 源码级技术关键点（file:line 证据）, dsh app-boot 装载栈（2026-10-22 Q1-Q5 裁决依据）：profile.ts:37-103,169-195,740-806；profile-context.ts:63-85；index.ts:520-593, dsh .env 分层（2026-10-22 Q11-Q14 裁决依据）：app-boot src/index.ts:114-259（loadLayeredEnv:238-259 / readEnvLayer:193-225 / BOOTSTRAP_NAMES:136-157 / BOOTSTRAP_PREFIXES:160 / HOME_LAYER_PROXY_NAMES:169）；util/launch-environment/src/index.ts:16-137（快照 get/getFrom 信任序与 ctx 槽）；util/home-paths/src/index.ts:76-121（resolveRoot / resolveDshHome）
- Replace pending test selectors with real test names before lifecycle verification.
