# Fly Rafter 候选需求块（KLL 导入前草稿）

| 项 | 值 |
|---|---|
| 来源 | doc/prd.md v1.0（2026-10-21 定稿） |
| 状态 | 已确认（2026-10-21，用户裁决 1A 2B）：20 块导入 KLL；REQ-FR-WEB 延后至 M5 详案附录定稿后补导入（草稿存 doc/requirements-deferred-web.md） |
| 用途 | `agent-spec requirements import --from doc/requirements-intake.md --out knowledge/requirements` 的唯一输入 |
| 图例 | 置信度记录于各块 Source Trace 条目；依赖为 REQ 块 id；场景用中文 DSL（解析器双语支持） |

<!-- agent-spec:requirement id=REQ-FR-CONFIG title="装配与配置系统" tags=config,assembly source=doc/prd.md -->
## Problem

dsh 依赖 cordis 原生分层补丁能力装配插件；dougong loader 只接受整份 JSON 文档，无分层、行覆盖、YAML 概念。fly-rafter 必须在 fr 层重建分层装配语义，同时保持 dougong reconcile 的幂等收敛与失败不阻塞语义。

## Requirements

- [REQ-FR-CONFIG-1] fr-config MUST 解析 fr.patch.yml 有序分层装配（自底向上：入口默认 bundle → profile bundle 栈 → profile patch → 家目录 patch → `--patch` 命令行补丁），合并语义 MUST 对齐 dsh `applyEntryPatches`（2026-10-22 Q1/Q2 裁决，vendor/include/src/index.ts:57-127）：行内 id/name 为匹配器，其余键**逐键覆盖**（config 整键替换不深合并；只携 disabled 的覆盖行仅翻转 disabled）；覆盖行未命中 id 或 name 不匹配 → 警告跳过（不致命、不进 failures）；行序 = 首次插入序；`insert` 行 MUST 向装配根插入新行（fr 不做 group 嵌套），后续层 MUST 能覆盖先前层插入的行。装载文件布局 MUST（2026-10-22 Q1/Q2/Q3 裁决，对齐 dsh app-boot profile.ts / profile-context.ts / index.ts）：profile = `<FR_HOME>/profiles/<name>/`，名字校验拒绝空名、含 `/` 或 `\`、`.`、`..`、`node_modules`；清单 `profile.yml` 的 `bundles:` 仅引用宿主内置命名 bundle，未知名 → skipped 警告非致命；profile patch = `<profile>/fr.patch.yml`（可选，缺文件 = 空层）；家目录 patch = `<FR_HOME>/fr.patch.yml`（可选，缺文件 = 空层）；`--patch` 为必需文件、缺文件报错终止；任何 patch 解析错误 fail-closed；已知入口名的 profile 首次使用自动初始化（清单 + 空白 patch 模板，绝不覆盖既有文件；fr 模板 bundles 为空列表——入口默认 bundle 恒在层 1，不重复挂载），未知名报错。patch 装载诊断文案 MUST 逐字对齐 dsh parsePatchList 族（2026-10-24 Q52A 裁决，app-boot index.ts:520-590 实证）：读失败 `fr: failed to read patches <path>: <原因>`（--patch 层 `fr: failed to read overlay <path>: <原因>`）、解析失败 `fr: failed to parse <label> <path>: <原因>`、非序列文档 `fr: <label> <path> must be a top-level YAML array of loader patch entries`、非映射行 `fr: <label> entry <N> in <path> must be a mapping (a loader patch entry)`（N 1-based；label：可选/家目录层与内嵌 bundle 资产 patches、--patch 层 overlay）；错误变体裸输出（Io/Profile/PatchParse 无 fr 包装前缀）；`--from-default-profile` 未伴随 profile 时 MUST fail-closed 拒绝装载（2026-10-24 Q51A 裁决：`--from-default-profile requires --profile`，dsh resolveBoot 在 CLI 层先行拒绝故 dsh 无此库态，文案为 fr 自有面）。清单与 profile 名诊断文案 MUST 逐字对齐 dsh（2026-10-24 Q53A 裁决，app-boot profile.ts:617/622/173/798 实证）：读失败 `fr: failed to read profile manifest <path>: <原因>`、非对象 `fr: profile manifest <path> must hold a JSON object`、非法名 `fr: invalid profile name "<name>"`（JSON.stringify 引号形）、未知 profile `fr: profile "<name>" does not exist`；:798 尾部补救命令后缀不移植（fr M1 无 plugin 命令，PRD 19.2 条 19）；清单解析错误文案系引擎自有（dsh V8 SyntaxError / fr YAML 解析器文案），同为裸抛结构不可字节对齐（同条）。
- [REQ-FR-CONFIG-2] fr-config MUST 将展开后的装配编译为 dougong loader JSON 文档（source.kind 仅 dylib/wasm：行 name 经产物解析规则映射——字面路径，或 `FR_PLUGIN_PATH` 各目录与安装库目录 `<prefix>/lib/fr/` 下按 slug（`-`→`_`）先查 `lib<slug>.so`（dylib）再查 `<slug>.wasm`（wasm），先命中定形态，未命中进入 failures；config 对象、isolate），经 `Reconciler::replace + apply` 幂等收敛；编译产物 MUST 为 dougong `Config::parse` 可接受的合法文档，且 MUST NOT 出现 builtin source。insert 行 name 为 `./`/`../` 开头的相对路径 MUST 在装载期锚定到所在补丁文件目录转绝对（2026-10-22 Q6 裁决，对齐 dsh anchorInsertedPluginNames index.ts:550-561，词法归一不做符号链接解析）；覆盖行 name 为匹配器 MUST NOT 锚定；无 `./`/`../` 前缀的含分隔符 name 保持字面路径。
- [REQ-FR-CONFIG-3] fr 的 config schema 以 fr-schema 声明式描述树为单一真源（2026-10-22 Q7 裁决，等价 dsh schemastery 校验+投影双重角色）：每插件 schema 为 fr-schema 描述符（受限子集，M1 面 = string/number/integer/boolean/object/array/dict；meta 注解仅 default 与 description；子集外声明不可构造——fail-closed），同一声明派生 `validate` 与 `to_json_schema`（JSON Schema 2020-12 关键词面，对齐 dsh ConfigJsonSchemaObject）。fr-config MUST 在装配期按插件 slug 查 fr-schema 注册表对每行 config 校验：查无 schema MUST 跳过该行校验（2026-10-22 Q9 裁决，对齐 dsh absent 标注；第三方字面路径工件宿主侧本无其 schema，不构成装载障碍）；有 schema 且单行校验失败进入 `Report::failures`，MUST NOT 阻塞其余槽位。插件工件 MUST 在激活体顶部以 fr-schema 链接副本对 config 再校验（双侧校验），激活期校验失败按插件装载失败 fail-closed。
- [REQ-FR-CONFIG-4] patch 行内条件表达式（`!!js` 标记或 `!!js ` 前缀字符串）MUST 限于受限声明式子集（2026-10-22 Q3A 勘误扩充）：`env.NAME ?? 默认值` 链、相等比较（`==`/`===`）、`platform === 'linux'|'macos'|'windows'` 与平台三元、裸 env 值读取、字面量、`profile-context` 布尔（dsh `ctx.get('profileContext')` 的 fr 等价物：入口/profile 上下文存在性，支持 `!` 取反）；MUST NOT 支持任意 JS 求值；子集外表达式（如 `getBuiltinModule` 类）MUST 求值失败 fail-closed（patch 文档整体拒绝，启动终止——对齐 dsh 用户层 patch 错误抛出语义）。
- [REQ-FR-CONFIG-5] fr MUST 提供 `fr dump-config` 与 `fr dump-config-schema`，内容由真实装配生成（文档即生成物）。`fr dump-config` MUST 输出合成装配树 YAML 文本、逐字节同型对齐 dsh `--dump-config`（2026-10-22 Q18-Q20 裁决，apps/cli/src/dump-config.ts + app-boot index.ts:635- 实证）：`# == <源文件>[, patched by <层>…]` 分隔注释按连续同源行分组；`!!js` 表达式逐字打印不求值；快照算法 = 层前缀各做单次 applyEntryPatches 同参展平（分层调用会让 group config 替换的子行被后续层命中，boot 不会）；整篇可再 parse 且等于同参启动挂载结果；层序 = bundle 层（label=包名）→ profile patch（label=路径，存在才入）→ 家目录 patch → `--patch` overlays（label=绝对路径）；错误三连 `fr: failed to read config` / `fr: failed to parse config` / `fr: config <path> must be a top-level YAML array of entries`。`fr dump-config-schema` MUST 输出 ConfigSchemaDump 等价物：`$schema` 2020-12 + `x-fr` 扩展键（Q18A，对齐 dsh `x-cordis`：{profile, complete, entries, diagnostics, patchSchema:'#/$defs/patchList'}）+ entry 五态 schema/partial/absent/unsupported/error + diagnostics 逐条 stderr（`fr: <level>: [<path>] <message>`（无 path 时 `fr: <level>: <message>`））+ complete 为假时退出码 1。三 dump 态互斥且拒收 app 位置参数；`--default-only`（Q19A，对齐 dsh `--dump-default-config`）为恢复诊断：仅 bundle 层、不读不解析 profile patch 与家目录 patch（坏 patch 文件亦可 dump）、不接受 `--patch`。`from-default-profile`（Q20A，对齐 dsh `--from-default-profile`）= 以出厂模板**替换**自定义 profile 后装载（区别于首用初始化绝不覆盖），dump 与 boot 共用；CLI 已随宿主切片落地（2026-10-23 Q30B）：子命令形态 `fr dump-config` / `fr dump-config-schema`；dsh 三旗标互斥在子命令形态结构性成立（文案不移植）；子命令自首个无法识别参数起整体视为 app 内参拒收（`error: config dumps take no app arguments, got <逐参数 JSON 串列>`，含 `-h`）；参数校验错误族逐字：`error: --profile <name> is required` / `error: --profile needs a name` / `error: select a profile only once` / `error: --patch needs a path` / `error: --from-default-profile needs a name`；`--default-only` 与 `--patch` 同给报 `error: --default-only prints the bundle layers and takes no --patch`；dump-config-schema stdout 恰单份 JSON 加换行、diagnostics 逐条 stderr、complete 假退出码 1；dump-config 致命错误（错误三连）以单行 stderr 诊断与退出码 1 终止；根调用 `-V/--version` 版本退出码 0、`-h/--help` 用法退出码 0、其余根调用用法错误退出码 1（fr 自有 launcher 面，M1 宿主全量时扩展入口模式）。
- [REQ-FR-CONFIG-6] fr MUST 提供关键默认值：默认模型 provider `deepseek-official`、model `deepseek-flash`；`FR_PERMISSION_MODE ?? workspace-write`（workspace root = 进程启动 cwd）；非 danger-full-access 时审批策略 `'ask'`；会话存储 `<FR_HOME>/sessions`、storages `<FR_HOME>/storages`；bash `timeout-ms` 60000；重复工具提醒阈值 [3, 5, 8]；`sample-over-cap-glob-results: false`；`allow-parallel-in-progress: true`；会话标题默认：无 LLM 时回退 5 词/40 字节（上限 80 字节），LLM 标题 targetWords 5（CJK 10）、maxInput 4096 字节、超时 60s。
- [REQ-FR-CONFIG-7] fr 宿主二进制 MUST 仅含装配器、事件泵、入口分发三件事：事件泵驱动 tokio runtime，把入口信号（headless 任务 / TUI 按键 / ACP·SDK 协议帧）转发到对应入口插件监听器；按子命令装配对应入口 profile（headless/tui/acp/sdk/web）；宿主对 agent 循环零感知。FR_HOME 默认 `~/.fly-rafter`，家目录结构与 dsh `~/.dsh/` 同构（sessions/storages/profiles）。
- [REQ-FR-CONFIG-8] 插件仅 dylib 与 wasm 两形态（2026-10-22 用户裁决，废除 builtin），MUST 共用 dougong 五步契约（声明 → 激活 → 服务 → 监听 → 释放，跨边界经 descriptor + C-ABI 导出表表达）：全部第一方插件 MUST 为 C-ABI dylib 工件（cdylib、内核家族仅依赖 dougong-abi、代码镜像进程内永不卸载，换版本即重启进程）；wasm 组件（wasm32-wasip2）仅为第三方沙箱扩展（零能力 WASI：wasip2 guest 必然声明 wasi:io 导入而宿主挂载零能力 WasiCtx 的惰性定义，能力面为零、导入声明不为零；fuel/epoch/内存帽；经 WIT world `dougong:kernel/host` 交互；零能力 WASI 无网络与文件系统，第一方组件 MUST NOT 以 wasm 承载）。fr-config MUST 按信任边界校验每行：第一方插件名 MUST 解析为 dylib source（解析命中 wasm 产物即为违规），wasm 仅当产物为第三方 `.wasm`；违规行进入 failures；loader builtin 表 MUST NOT 出现在 fr 装配产物中。
- [REQ-FR-CONFIG-9] 第三方工具插件 MUST 经 Invoker 通道接入：跨边界契约 = 事件名 + JSON 载荷 schema + 方法号常量表（descriptor 声明 + 能力自检）；isolate 列 MUST 承载 realm（会话级装配在会话 realm 内放置同名插件，域内 provide 不外泄、注入先域内后回退全局默认）；`internal/update` waterfall 仅作热更新预留扩展点，本期 MUST NOT 暴露。

- [REQ-FR-CONFIG-10] 默认装配树 MUST 与 dsh base cordis.patch.yml 插件 id 集合同型（品牌映射后），并冻结 PRD 6.4 未记载默认值：agent-instructions maxBytes 65536、projection-cache 200 事件 5000ms、web searchProvider deepseek-official 与 fetchProvider http、repeat argumentsPreviewChars 500、标题 maxOutputTokens 4096、tools 默认 native、storage-domain backend json（dsh cordis.patch.yml 各行）。
- [REQ-FR-CONFIG-11] 启动失败策略 MUST（对齐 dsh required-id 审计 [app-boot index.ts:944-961,1092-1114]）：仅 bootstrap 入口 Include 与必需插件 id 集（fr 对应 agent-loop 与当前子命令入口插件）激活失败时以 StartupError 拒启动（错误列出失败与等待清单）；其余插件失败一律单条警告、兄弟继续；注入服务缺失 → 插件 fiber 置 pending 容忍（其 id 属必需集则致命）；导入失败记录错误后不留实例、不中止装配；patch 未知 id 警告跳过永不致命；覆盖行语义 MUST 为按键覆盖（2026-10-22 Q1 勘误，对齐 dsh applyEntryPatches），patch 行内仅其余键作用于该行自身。
- [REQ-FR-CONFIG-12] patch 行内 env 缺省表达式 MUST 以受限子集（CONFIG-4）表达 dsh !!js 形态的对应语义：`env.FR_PERMISSION_MODE ?? workspace-write` 与 `danger-full-access` 的相等比较求值结果 MUST 与 dsh `process.env.DSH_PERMISSION_MODE ?? 'workspace-write'` 逐值一致（dsh cordis.patch.yml:246,262）。
- [REQ-FR-CONFIG-13] .env 分层 MUST（2026-10-22 用户裁决纳入，Q11A/Q12A/Q13A/Q14A 定稿；bootstrap 49 名全表以 PRD 6.4 为真源）：fr-config 启动期解析环境 = 继承进程 env > 项目 `.env`（自 cwd）> 家目录 `.env`（`<FR_HOME>/.env`）；两文件 MUST 先全部解析校验、后统一应用（任一层 bootstrap 违例则两文件均不应用）；`cwd == FR_HOME` 时家目录层 MUST NOT 重复读取；`.env` 中已设名 MUST NOT 覆盖更高层已存在的值（应用序 project → user，逐名仅在更高层未设时 materialize 进程 env）；bootstrap 专名在任何 `.env` 层拒绝（大小写不敏感）：49 个精确名 = dsh BOOTSTRAP_NAMES（app-boot index.ts:136-157）逐字（DEEPSEEK_BASE_URL 与 DEEPSEEK_SEARCH_BASE_URL 保留原名）+ 四前缀 FR_ / XDG_ / DYLD_ / BASH_FUNC_（DSH_ 品牌映射）；家目录层例外仅 HTTP_PROXY / HTTPS_PROXY / ALL_PROXY / NO_PROXY 四键（项目层设代理名仍拒绝）；bootstrap 违例 MUST 终止启动（fail-closed）；`.env` 读错误非 ENOENT = stderr 一行警告 + 该层按空层跳过（非致命）；ENOENT = 静默空层；机制为双轨 = materialize 进程 env（unsafe set_var 启动期单线程）+ 返回不可变环境快照（层序 process > project-env > user-env 的 get / get_from 逐名来源查询，消费方后续里程碑接线）；`.env` 解析语法 MUST 对齐 Node util.parseEnv 方言（自实现，以 Node 官方源码为钉测蓝本）；home 解析 = 显式 configured > `$FR_HOME`（trim 后空白视为未设）> `~/.fly-rafter`（~ 前缀展开后归一）；dsh 单层 loadEnv 变体仅测试夹具使用，fr 不实现（负向钉）
- [REQ-FR-CONFIG-14] `!!js` 方言解析与 dump 输出 MUST 结构感知（2026-10-22 Q25B 裁决）：字符串值内容中形如 `key: !!js v` 的文本（块标量、引号多行标量、普通多行标量）MUST 逐字装载，不翻转为单叹号、不作为表达式求值；识别值位（行首 `key: ` 或 `- ` 前缀后）的 `!!js` 照旧归一为表达式求值；识别位之外的结构值位（flow 式、异形键）MUST fail-closed 拒绝且错误含 `outside a recognized value position` 文案；`dump-config` 输出中字符串内容行形如 `key: !js v` MUST 保持单叹号逐字、真实 `!!js` 标签行照常打印 `!!js`，整篇再 parse 等于同参挂载结果（dump == boot 对任意字符串成立）
## Scenarios

Rule: REQ-FR-CONFIG-1 — 分层装配与按键覆盖

场景: 覆盖行仅翻转 disabled
  假设 覆盖行仅携带 id 与 disabled:true 且未携 config 或 name
  当 合并到已存在的同名 id 行时
  那么 仅该行 disabled 变为 true，name 与 config 保持不变

场景: 覆盖行整键替换 config
  假设 覆盖行携带新 config 对象
  当 合并到已存在 id 行时
  那么 该行 config 整体替换为新对象（不深合并，旧键不保留）

场景: 覆盖行未命中 id 警告跳过
  假设 覆盖行的 id 在装配中不存在
  当 合并时
  那么 发出单条警告并跳过该行，不进 failures、不致命

场景: 覆盖行 name 不匹配警告跳过
  假设 覆盖行 id 命中但 name 与既有行不同
  当 合并时
  那么 发出单条警告并跳过该行，不致命

场景: 装配层次自底向上展开
  假设 入口默认、profile、家目录 patch 与 --patch 各定义了同一 id
  当 展开装配时
  那么 按入口默认 → profile → 家目录 patch → --patch 顺序覆盖，最终值来自最后一层

场景: insert 行向根插入并保持首插序
  假设 某 bundle 层以 insert 行插入两个新 id
  当 合并完成后
  那么 新行按插入序追加于行列表尾，后续层可按 id 覆盖这些插入行

场景: profile 清单 bundle 栈按清单序叠加
  假设 profile.yml 的 bundles 列表为 [a, b] 且两者均为内置命名 bundle
  当 装载该 profile 时
  那么 bundle 层按清单序（先列者先应用）叠加于入口默认 bundle 之后、profile patch 之前

场景: profile 清单未知名 skipped 警告非致命
  假设 profile.yml bundles 引用了不存在的 bundle 名
  当 装载该 profile 时
  那么 该名进入 skipped 列表并附原因，其余 bundle 正常装载，不致命

场景: profile 名校验拒绝非法名
  假设 profile 名为空、含 / 或 \、等于 . 或 ..、或为 node_modules
  当 解析 profile 引用时
  那么 拒绝并报错，不产生文件系统副作用，文案逐字 `fr: invalid profile name "<name>"`（对齐 dsh profile.ts:173，JSON.stringify 引号形）

场景: profile 模板首用自动初始化
  假设 FR_HOME/profiles/headless 目录不存在
  当 首次以 profile 名 headless 装载时
  那么 自动创建目录、profile.yml（bundles 为空列表）与空白 fr.patch.yml 模板，绝不覆盖既有文件

场景: 未知 profile 名报错
  假设 profile 名无内置模板且对应目录不存在
  当 装载该 profile 时
  那么 报错指明 profile 不存在并终止启动，文案逐字 `fr: profile "<name>" does not exist`（dsh profile.ts:798 前段；补救命令后缀不移植，PRD 19.2 条 19）

场景: profile 清单读失败与非对象文案逐字对齐
  假设 profile 清单文件不可读（如为目录）或解析结果非映射
  当 装载该 profile 时
  那么 分别以单行错误终止启动：`fr: failed to read profile manifest <path>: <原因>`、`fr: profile manifest <path> must hold a JSON object`（对齐 dsh readProfileManifest profile.ts:617/622）

场景: 可选 patch 缺文件为空层
  假设 profile patch 与家目录 patch 文件均不存在
  当 装载时
  那么 两处均按空层处理，不报错

场景: --patch 缺文件报错
  假设 --patch 给出的路径不存在
  当 装载 overlays 时
  那么 报错并终止启动

场景: patch 解析错误 fail-closed
  假设 任一层 patch 文件存在解析错误（行 schema 违例或子集外表达式）
  当 装载时
  那么 报错并终止启动，不产生部分装配产物

场景: 可选 patch 读失败文案逐字对齐
  假设 profile patch 或家目录 patch 路径存在但不可读（如为目录）
  当 装载时
  那么 以单行错误终止启动：`fr: failed to read patches <path>: <原因>`（对齐 dsh loadOptionalPatches index.ts:526）

场景: --patch 读失败文案逐字对齐
  假设 --patch 给出的路径不可读或不存在
  当 装载 overlays 时
  那么 以单行错误终止启动：`fr: failed to read overlay <path>: <原因>`（对齐 dsh loadOverlayPatches index.ts:545）

场景: patch 文档形态错误文案逐字对齐
  假设 patch 文档为非序列、YAML 语法错误或含非映射行
  当 装载时
  那么 分别以单行错误终止启动：`fr: <label> <path> must be a top-level YAML array of loader patch entries`、`fr: failed to parse <label> <path>: <原因>`、`fr: <label> entry <N> in <path> must be a mapping (a loader patch entry)`（label 按层 patches/overlay，对齐 dsh parsePatchList index.ts:582-589）

场景: from-default-profile 缺 profile 拒绝装载
  假设 装载调用给出 from-default-profile 模板但未选 profile
  当 装载时
  那么 fail-closed 拒绝：`--from-default-profile requires --profile`（2026-10-24 Q51A 裁决；dsh resolveBoot 在 CLI 层先拒，库层同拒为 fr 自有面）

Rule: REQ-FR-CONFIG-2 — 编译为 loader 文档

场景: 编译产物为合法 loader 文档
  假设 一次完整装配展开完成
  当 编译并执行 Reconciler::replace 加 apply 时
  那么 编译产物通过 dougong Config::parse，每行含 source/config/isolate

场景: insert 行相对路径锚定到补丁文件目录
  假设 某补丁文件以 insert 行插入 name 为 ./ext/libfoo.so 与 ../sib/bar.wasm 的插件
  当 装载该补丁层时
  那么 两个 name 锚定为该补丁文件目录下的绝对路径（../ 逐级上折），后续编译按字面路径解析

场景: 覆盖行 name 与无前缀相对 name 不锚定
  假设 覆盖行 name 为 ./matcher 或 insert 行 name 为 plugins/foo.so（无 ./ ../ 前缀）
  当 装载该补丁层时
  那么 两处 name 保持原样（匹配器不锚定；无前缀含分隔符 name 保持字面路径）

Rule: REQ-FR-CONFIG-3 — schema 校验不阻塞

场景: 单行 config 校验失败不阻塞其余槽位
  假设 某插件行的 config 违反其 ConfigSchema
  当 执行装配收敛时
  那么 该行进入 failures，其余插件正常装载

场景: 查无 schema 的行跳过校验
  假设 某行插件名在 fr-schema 注册表中无声明（如第三方字面路径工件）
  当 执行装配收敛时
  那么 该行 config 不校验、照常装载，不进 failures（dsh absent 语义）

场景: 同一声明派生校验与 JSON Schema 投影
  假设 fr-schema 描述符声明了候选枚举 string 字段与嵌套对象结构
  当 分别执行 validate 与 to_json_schema 时
  那么 两者出自同一声明：validate 拒绝枚举外与结构违例值，投影产出 2020-12 关键词面 JSON Schema

场景: 激活期链接副本再校验 fail-closed
  假设 插件工件激活体收到违反其 schema 的 config
  当 激活体顶部以 fr-schema 链接副本再校验时
  那么 校验失败按插件装载失败 fail-closed，服务不激活

Rule: REQ-FR-CONFIG-4 — 受限条件子集

场景: 超出受限子集的条件表达式 fail-closed
  假设 patch 行使用任意 JS 表达式（如 getBuiltinModule 路径解析类）
  当 解析 fr.patch.yml 求值该表达式时
  那么 求值失败进 failures 并按启动失败策略处理

场景: env 默认链与相等比较逐值求值
  假设 patch 行含 env.FR_PERMISSION_MODE ?? workspace-write 与 danger-full-access 相等比较
  当 分别在 FR_PERMISSION_MODE 设为 danger-full-access、其他值与未设置下求值时
  那么 求值结果与 dsh process.env.DSH_PERMISSION_MODE ?? 'workspace-write' 逐值一致

场景: 裸 env 值读取作为配置值
  假设 patch 行 config 值为裸 env.FR_TOOLS_MODE 表达式
  当 求值时
  那么 env 存在时取其字符串值，不存在时取 null

场景: profile-context 布尔门控
  假设 patch 行 disabled 为 profile-context 取反布尔（dsh !ctx.get('profileContext') 的 fr 等价物）
  当 分别在有与无入口 profile 上下文时求值
  那么 有上下文时 disabled=false，无上下文时 disabled=true

场景: 平台三元按字面语义求值
  假设 patch 行含 linux 平台三元
  当 在不同平台断言下求值时
  那么 按平台真值选择分支，无 JS 求值器介入

Rule: REQ-FR-CONFIG-5 — dump 命令即生成物

场景: dump 命令输出来自真实装配
  当 运行 fr dump-config 与 fr dump-config-schema 时
  那么 分别输出合成后插件树与全部插件 config schema

场景: dump 输出注释分层逐字同型
  假设 bundle、profile patch、家目录 patch 与 --patch 多层修改同一行并插入新行
  当 运行 fr dump-config 时
  那么 输出 YAML 文本中 # == 源文件[, patched by 层…] 分隔注释按连续同源行分组，!!js 表达式逐字未求值，整篇可再 parse 且等于同参启动挂载结果

场景: default-only 恢复诊断不读不解析用户层
  假设 profile 的 fr.patch.yml 含语法损坏内容且以 --default-only 调用
  当 运行 fr dump-config 时
  那么 仅输出 bundle 层，损坏文件未读未解析且不报错

场景: default-only 不接受 --patch
  假设 --default-only 与 --patch 同时给出
  当 解析调用时
  那么 报错终止

场景: x-fr 扩展键与 entry 五态
  假设 运行 fr dump-config-schema
  当 检查输出文档时
  那么 $schema 为 2020-12，含 x-fr 的 profile/complete/entries/diagnostics/patchSchema 五键，entry status 为五态之一

场景: complete 为假退出码 1
  假设 装配存在 error 态 entry
  当 运行 fr dump-config-schema 时
  那么 diagnostics 含该条且进程退出码为 1

场景: 注册表命中投影 configN
  假设 装配行 name 命中 fr-schema 注册表
  当 运行 fr dump-config-schema 时
  那么 该 entry status 为 schema 且 configRef 指向 $defs/configN，其 $defs 内容与同一声明的 to_json_schema 投影一致，且投影为 dsh 全脸（非必需位 type 并 'null'、必需位附加 not:{type:'null'}、根与嵌套值位 anyOf loaderExpression 包裹）

场景: 同名异 schema 投影分裂告警
  假设 同一插件名的两行经 schema_of 命中不同声明
  当 运行 fr dump-config-schema 时
  那么 config0/config1 按发现序分裂，ambiguity 警告入 diagnostics，complete 为假

场景: 查无 schema 标注 absent
  假设 装配行 name 未命中 fr-schema 注册表
  当 运行 fr dump-config-schema 时
  那么 该 entry status 为 absent 且 configRef 为 #/$defs/unknownConfig

场景: from-default-profile 以出厂模板替换
  假设 自定义 profile 已存在且携带 from-default-profile 指向 headless
  当 数据面装载该 profile 时
  那么 出厂模板替换既有 profile 内容后装载（非首用初始化的绝不覆盖语义）

场景: dump 拒收 app 位置参数与未知旗标
  假设 dump 子命令调用携带位置参数或未知旗标（含 -h）
  当 解析参数时
  那么 stderr 输出 error: config dumps take no app arguments, got <逐参数 JSON 串列>，退出码 1，stdout 无输出

场景: dump 参数校验错误族
  假设 dump 子命令分别缺 --profile、--profile 为空值、--profile 重复、--patch 空值或 --from-default-profile 空值
  当 解析参数时
  那么 依次输出 error: --profile <name> is required、error: --profile needs a name、error: select a profile only once、error: --patch needs a path、error: --from-default-profile needs a name，均以退出码 1 终止

场景: dump-config-schema 流面分流
  假设 运行 fr dump-config-schema 且装配完整
  当 输出时
  那么 stdout 恰为单份 JSON 文档加一个换行，diagnostics 逐条 stderr（fr: <level>: [<path>] <message>，无 path 时无方括号段），退出码 0

场景: dump 致命错误单行诊断退出码 1
  假设 --patch 指向无法读取或无法解析的文件
  当 运行 fr dump-config 时
  那么 stderr 输出含对应错误三连文案的单行诊断，退出码 1，stdout 无输出

场景: dump 之外的根调用面
  假设 fr 以 -V/--version、-h/--help、无子命令或未知子命令调用
  当 解析根调用时
  那么 -V/--version 输出版本号退出码 0，-h/--help 输出用法退出码 0，其余以用法错误行退出码 1 拒绝

Rule: REQ-FR-CONFIG-6 — 关键默认值

场景: 未配置时关键默认值生效
  假设 全默认配置启动
  当 读取解析后的配置时
  那么 provider 为 deepseek-official、model 为 deepseek-flash、权限模式为 workspace-write、bash timeout-ms 为 60000

场景: FR_PERMISSION_MODE 覆盖默认权限模式
  假设 FR_PERMISSION_MODE 为 read-only
  当 解析权限模式时
  那么 生效 read-only 而非默认 workspace-write

Rule: REQ-FR-CONFIG-7 — 宿主三件事

场景: 宿主按子命令装配对应入口 profile
  假设 调用 fr headless
  当 装配时
  那么 装配 headless 入口 profile，宿主二进制不含 agent 循环实现

场景: 事件泵转发入口信号
  假设 headless 任务、TUI 按键或 ACP/SDK 协议帧到达
  当 事件泵运转时
  那么 信号被转发到对应入口插件的监听器

Rule: REQ-FR-CONFIG-8 — 两形态与信任校验

场景: 第一方名解析为 wasm 被拒
  假设 patch 行 name 为 fr-session 且解析命中 <slug>.wasm 产物
  当 fr-config 信任边界校验时
  那么 该行进入 failures 且不装载

场景: dylib 镜像永不卸载
  假设 已加载一个第三方 dylib 插件
  当 变更其补丁行并 reconcile 时
  那么 代码镜像保持驻留不卸载，换版本要求重启进程

场景: 第一方 dylib 工件缺失启动失败
  假设 patch 行 name 为 fr-session 且解析路径不存在对应 .so
  当 装配收敛时
  那么 该行进入 failures 并按启动失败策略处理，其余槽不受影响

场景: 装配产物不含 builtin source
  假设 默认 headless 装配展开完成
  当 检查编译产物全部 source.kind 时
  那么 全部为 dylib，无任何 builtin 条目

Rule: REQ-FR-CONFIG-9 — Invoker 接入与 realm

场景: 会话 realm 域内解析不外泄
  假设 会话 realm 内放置了同名插件
  当 会话内 require 该服务时
  那么 先命中域内实现，域内 provide 不外泄到全局默认

场景: 插件经 Invoker 通道接入
  当 插件（第一方 dylib 或第三方 wasm）跨边界提供服务时
  那么 经事件名加 JSON 载荷 schema 加方法号常量表的 Invoker 契约接入，且 descriptor 声明与能力自检通过

Rule: REQ-FR-CONFIG-1 — profile 机制

场景: profile 选择与缩写
  假设 定义了名为 work 的 profile
  当 分别以 --profile work 与位置参数缩写 fr work 启动时
  两种方式装配出相同 profile bundle 栈

Rule: REQ-FR-CONFIG-6 — 默认值全表

场景: 全表默认值逐项生效
  假设 全默认配置启动
  当 逐项读取会话存储路径、会话标题参数（无 LLM 回退 5 词 40 字节上限 80、LLM targetWords 5 CJK 10 maxInput 4096 超时 60s）、重复工具提醒阈值、glob 与 todo 配置时
  那么 全部与 PRD 6.4 表一致

场景: dump-config 插件树与 dsh base patch 同型
  假设 默认装配导出 fr dump-config
  当 与 dsh packages/bundle/base/cordis.patch.yml 展开的插件树同型比对时
  那么 插件名集合与默认配置逐行同型（品牌键除外）

Rule: REQ-FR-CONFIG-1 — disabled 行语义

场景: disabled 行使插件不装载
  假设 后行将某 id 行置 disabled 为 true
  当 收敛时
  那么 该插件不装载且编译产物不含该行实例

Rule: REQ-FR-CONFIG-2 — dylib 与 wasm source 装载

场景: dylib 与 wasm source 行装载
  假设 装配分别包含第一方 dylib 行与第三方 wasm 行
  当 收敛时
  那么 两种 source 按各自形态装载且能力边界生效

Rule: REQ-FR-CONFIG-2 — isolate 变更与幂等

场景: isolate 变更触发 retire 加重插
  假设 某行的 isolate 列发生变化
  当 reconcile 时
  那么 该插件先 retire 再重插，其余行不受影响

场景: 重复收敛产出 unchanged
  假设 同一文档连续两次收敛
  当 第二次收敛时
  那么 Report 仅含 unchanged，无 insert/update/retire

Rule: REQ-FR-CONFIG-10 — 默认装配树同型

场景: 装配树名单 diff
  假设 以默认装配导出 dump-config
  当 与 base cordis.patch.yml 插件 id 集合按品牌映射比对时
  那么 集合一致且未记载默认值逐项命中

Rule: REQ-FR-CONFIG-6 — 默认值全表枚举（枚举生成）

场景: 默认值 provider
  假设 全默认配置启动
  当 读取 provider 时
  那么 值为 deepseek-official

场景: 默认值 model
  假设 全默认配置启动
  当 读取 model 时
  那么 值为 deepseek-flash

场景: 默认值 permission-mode
  假设 全默认配置启动
  当 读取 permission-mode 时
  那么 值为 workspace-write

场景: 默认值 approval-policy
  假设 全默认配置启动
  当 读取 approval-policy 时
  那么 值为 ask

场景: 默认值 bash timeout-ms
  假设 全默认配置启动
  当 读取 bash timeout-ms 时
  那么 值为 60000

场景: 默认值 repeat-thresholds
  假设 全默认配置启动
  当 读取 repeat-thresholds 时
  那么 值为 [3,5,8]

场景: 默认值 argumentsPreviewChars
  假设 全默认配置启动
  当 读取 argumentsPreviewChars 时
  那么 值为 500

场景: 默认值 sample-over-cap-glob-results
  假设 全默认配置启动
  当 读取 sample-over-cap-glob-results 时
  那么 值为 false

场景: 默认值 allow-parallel-in-progress
  假设 全默认配置启动
  当 读取 allow-parallel-in-progress 时
  那么 值为 true

场景: 默认值 pruner threshold
  假设 全默认配置启动
  当 读取 pruner threshold 时
  那么 值为 8192

场景: 默认值 pruner head
  假设 全默认配置启动
  当 读取 pruner head 时
  那么 值为 4096

场景: 默认值 pruner tail
  假设 全默认配置启动
  当 读取 pruner tail 时
  那么 值为 1024

场景: 默认值 spill max-inline-tokens
  假设 全默认配置启动
  当 读取 spill max-inline-tokens 时
  那么 值为 12500

场景: 默认值 标题回退
  假设 全默认配置启动
  当 读取 标题回退 时
  那么 值为 5 词 40 字节上限 80

场景: 默认值 LLM 标题 targetWords
  假设 全默认配置启动
  当 读取 LLM 标题 targetWords 时
  那么 值为 5（CJK 10）

场景: 默认值 LLM 标题 maxInput
  假设 全默认配置启动
  当 读取 LLM 标题 maxInput 时
  那么 值为 4096 字节

场景: 默认值 LLM 标题 timeout
  假设 全默认配置启动
  当 读取 LLM 标题 timeout 时
  那么 值为 60s

场景: 默认值 LLM 标题 maxOutputTokens
  假设 全默认配置启动
  当 读取 LLM 标题 maxOutputTokens 时
  那么 值为 4096

场景: 默认值 agent-instructions maxBytes
  假设 全默认配置启动
  当 读取 agent-instructions maxBytes 时
  那么 值为 65536

场景: 默认值 projection-cache
  假设 全默认配置启动
  当 读取 projection-cache 时
  那么 值为 200 事件 5000ms

场景: 默认值 web searchProvider
  假设 全默认配置启动
  当 读取 web searchProvider 时
  那么 值为 deepseek-official

场景: 默认值 web fetchProvider
  假设 全默认配置启动
  当 读取 web fetchProvider 时
  那么 值为 http

场景: 默认值 tool-web searchTimeoutMs
  假设 全默认配置启动
  当 读取 tool-web searchTimeoutMs 时
  那么 值为 60000

场景: 默认值 tools 默认 mode
  假设 全默认配置启动
  当 读取 tools 默认 mode 时
  那么 值为 native

场景: 默认值 storage-domain backend
  假设 全默认配置启动
  当 读取 storage-domain backend 时
  那么 值为 json

场景: 默认值 PTC execution-mode
  假设 全默认配置启动
  当 读取 PTC execution-mode 时
  那么 值为 per-call

场景: 默认值 PTC timeoutMs
  假设 全默认配置启动
  当 读取 PTC timeoutMs 时
  那么 值为 120000

场景: 默认值 PTC maxTimeoutMs
  假设 全默认配置启动
  当 读取 PTC maxTimeoutMs 时
  那么 值为 600000

场景: 默认值 PTC maxOutputBytes
  假设 全默认配置启动
  当 读取 PTC maxOutputBytes 时
  那么 值为 64MiB

场景: 默认值 PTC maxOldGenerationSizeMb
  假设 全默认配置启动
  当 读取 PTC maxOldGenerationSizeMb 时
  那么 值为 512

场景: 默认值 PTC maxMessageBytes
  假设 全默认配置启动
  当 读取 PTC maxMessageBytes 时
  那么 值为 128MiB

场景: 默认值 PTC maxPendingCalls
  假设 全默认配置启动
  当 读取 PTC maxPendingCalls 时
  那么 值为 128

场景: 默认值 PTC graceMs
  假设 全默认配置启动
  当 读取 PTC graceMs 时
  那么 值为 3000

场景: 默认值 max-parallel-sub-calls
  假设 全默认配置启动
  当 读取 max-parallel-sub-calls 时
  那么 值为 10

场景: 默认值 maxParallelToolCalls
  假设 全默认配置启动
  当 读取 maxParallelToolCalls 时
  那么 值为 10

场景: 默认值 ACP session-list-page-size
  假设 全默认配置启动
  当 读取 ACP session-list-page-size 时
  那么 值为 100

场景: 默认值 retry maxRetries
  假设 全默认配置启动
  当 读取 retry maxRetries 时
  那么 值为 5

场景: 默认值 retry backoff initial
  假设 全默认配置启动
  当 读取 retry backoff initial 时
  那么 值为 500ms

场景: 默认值 retry backoff max
  假设 全默认配置启动
  当 读取 retry backoff max 时
  那么 值为 10000ms

Rule: REQ-FR-CONFIG-6 — 插件 config 目录枚举（catalog 生成）

场景: config dsh-agent-loop maxParallelToolCalls 默认
  假设 插件 dsh-agent-loop 已注册
  当 读取解析后配置时
  那么 配置行 [maxParallelToolCalls:Volatile<number>:-（省略用内置 DEFAULT 10）] 的默认值按目录行实现

场景: config dsh-agent-loop maxParallelToolCalls 约束
  假设 插件 dsh-agent-loop 已注册
  当 校验与生效时
  那么 配置行 [maxParallelToolCalls:Volatile<number>:-（省略用内置 DEFAULT 10）] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-agent-loop agents 默认
  假设 插件 dsh-agent-loop 已注册
  当 读取解析后配置时
  那么 配置行 [agents:启动时创建/恢复的 agents] 的默认值按目录行实现

场景: config dsh-agent-loop agents 约束
  假设 插件 dsh-agent-loop 已注册
  当 校验与生效时
  那么 配置行 [agents:启动时创建/恢复的 agents] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-agent-instructions projectRootMarkers 默认
  假设 插件 dsh-agent-instructions 已注册
  当 读取解析后配置时
  那么 配置行 [projectRootMarkers:string[]（.git 等）] 的默认值按目录行实现

场景: config dsh-agent-instructions projectRootMarkers 约束
  假设 插件 dsh-agent-instructions 已注册
  当 校验与生效时
  那么 配置行 [projectRootMarkers:string[]（.git 等）] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-agent-instructions maxBytes 默认
  假设 插件 dsh-agent-instructions 已注册
  当 读取解析后配置时
  那么 配置行 [maxBytes:number（非正/非有限禁用加载）] 的默认值按目录行实现

场景: config dsh-agent-instructions maxBytes 约束
  假设 插件 dsh-agent-instructions 已注册
  当 校验与生效时
  那么 配置行 [maxBytes:number（非正/非有限禁用加载）] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-agent-instructions maxSourceBytes 默认
  假设 插件 dsh-agent-instructions 已注册
  当 读取解析后配置时
  那么 配置行 [maxSourceBytes:number（单文件上限超限忽略）] 的默认值按目录行实现

场景: config dsh-agent-instructions maxSourceBytes 约束
  假设 插件 dsh-agent-instructions 已注册
  当 校验与生效时
  那么 配置行 [maxSourceBytes:number（单文件上限超限忽略）] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-agent-tool-presentation / dsh-tools mode 默认
  假设 插件 dsh-agent-tool-presentation / dsh-tools 已注册
  当 读取解析后配置时
  那么 配置行 [mode:'native'|'ptc':tools 默认 native；ptc 需 ctx.ptcRuntime 且语言有 SDK renderer 否则组装失败] 的默认值按目录行实现

场景: config dsh-agent-tool-presentation / dsh-tools mode 约束
  假设 插件 dsh-agent-tool-presentation / dsh-tools 已注册
  当 校验与生效时
  那么 配置行 [mode:'native'|'ptc':tools 默认 native；ptc 需 ctx.ptcRuntime 且语言有 SDK renderer 否则组装失败] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-agent-tool-presentation / dsh-tools maxParallelSubCalls 默认
  假设 插件 dsh-agent-tool-presentation / dsh-tools 已注册
  当 读取解析后配置时
  那么 配置行 [maxParallelSubCalls:10（正整数，1=串行）] 的默认值按目录行实现

场景: config dsh-agent-tool-presentation / dsh-tools maxParallelSubCalls 约束
  假设 插件 dsh-agent-tool-presentation / dsh-tools 已注册
  当 校验与生效时
  那么 配置行 [maxParallelSubCalls:10（正整数，1=串行）] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-attachment-local dshHome 默认
  假设 插件 dsh-attachment-local 已注册
  当 读取解析后配置时
  那么 配置行 [dshHome:-($DSH_HOME→~/.dsh)] 的默认值按目录行实现

场景: config dsh-attachment-local dshHome 约束
  假设 插件 dsh-attachment-local 已注册
  当 校验与生效时
  那么 配置行 [dshHome:-($DSH_HOME→~/.dsh)] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-attachment-local maxImageBytes 默认
  假设 插件 dsh-attachment-local 已注册
  当 读取解析后配置时
  那么 配置行 [maxImageBytes:20MiB] 的默认值按目录行实现

场景: config dsh-attachment-local maxImageBytes 约束
  假设 插件 dsh-attachment-local 已注册
  当 校验与生效时
  那么 配置行 [maxImageBytes:20MiB] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-attachment-local maxImagesPerMessage 默认
  假设 插件 dsh-attachment-local 已注册
  当 读取解析后配置时
  那么 配置行 [maxImagesPerMessage:20] 的默认值按目录行实现

场景: config dsh-attachment-local maxImagesPerMessage 约束
  假设 插件 dsh-attachment-local 已注册
  当 校验与生效时
  那么 配置行 [maxImagesPerMessage:20] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-attachment-local maxMessageImageBytes 默认
  假设 插件 dsh-attachment-local 已注册
  当 读取解析后配置时
  那么 配置行 [maxMessageImageBytes:200MiB] 的默认值按目录行实现

场景: config dsh-attachment-local maxMessageImageBytes 约束
  假设 插件 dsh-attachment-local 已注册
  当 校验与生效时
  那么 配置行 [maxMessageImageBytes:200MiB] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-attachment-local maxImagePixels 默认
  假设 插件 dsh-attachment-local 已注册
  当 读取解析后配置时
  那么 配置行 [maxImagePixels:64000000] 的默认值按目录行实现

场景: config dsh-attachment-local maxImagePixels 约束
  假设 插件 dsh-attachment-local 已注册
  当 校验与生效时
  那么 配置行 [maxImagePixels:64000000] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-attachment-local maxImageDimension 默认
  假设 插件 dsh-attachment-local 已注册
  当 读取解析后配置时
  那么 配置行 [maxImageDimension:8192px] 的默认值按目录行实现

场景: config dsh-attachment-local maxImageDimension 约束
  假设 插件 dsh-attachment-local 已注册
  当 校验与生效时
  那么 配置行 [maxImageDimension:8192px] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-bash-local（执行器默认值真源） cwd 默认
  假设 插件 dsh-bash-local（执行器默认值真源） 已注册
  当 读取解析后配置时
  那么 配置行 [cwd:Volatile:process.cwd()] 的默认值按目录行实现

场景: config dsh-bash-local（执行器默认值真源） cwd 约束
  假设 插件 dsh-bash-local（执行器默认值真源） 已注册
  当 校验与生效时
  那么 配置行 [cwd:Volatile:process.cwd()] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-bash-local（执行器默认值真源） timeoutMs 默认
  假设 插件 dsh-bash-local（执行器默认值真源） 已注册
  当 读取解析后配置时
  那么 配置行 [timeoutMs:Volatile:120000] 的默认值按目录行实现

场景: config dsh-bash-local（执行器默认值真源） timeoutMs 约束
  假设 插件 dsh-bash-local（执行器默认值真源） 已注册
  当 校验与生效时
  那么 配置行 [timeoutMs:Volatile:120000] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-bash-local（执行器默认值真源） maxTimeoutMs 默认
  假设 插件 dsh-bash-local（执行器默认值真源） 已注册
  当 读取解析后配置时
  那么 配置行 [maxTimeoutMs:Volatile:600000] 的默认值按目录行实现

场景: config dsh-bash-local（执行器默认值真源） maxTimeoutMs 约束
  假设 插件 dsh-bash-local（执行器默认值真源） 已注册
  当 校验与生效时
  那么 配置行 [maxTimeoutMs:Volatile:600000] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-bash-local（执行器默认值真源） maxOutputBytes 默认
  假设 插件 dsh-bash-local（执行器默认值真源） 已注册
  当 读取解析后配置时
  那么 配置行 [maxOutputBytes:Volatile:64000（溢出落临时文件）] 的默认值按目录行实现

场景: config dsh-bash-local（执行器默认值真源） maxOutputBytes 约束
  假设 插件 dsh-bash-local（执行器默认值真源） 已注册
  当 校验与生效时
  那么 配置行 [maxOutputBytes:Volatile:64000（溢出落临时文件）] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-compaction-basic auto 默认
  假设 插件 dsh-compaction-basic 已注册
  当 读取解析后配置时
  那么 配置行 [auto:true] 的默认值按目录行实现

场景: config dsh-compaction-basic auto 约束
  假设 插件 dsh-compaction-basic 已注册
  当 校验与生效时
  那么 配置行 [auto:true] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-compaction-basic thresholdRatio 默认
  假设 插件 dsh-compaction-basic 已注册
  当 读取解析后配置时
  那么 配置行 [thresholdRatio:0.8] 的默认值按目录行实现

场景: config dsh-compaction-basic thresholdRatio 约束
  假设 插件 dsh-compaction-basic 已注册
  当 校验与生效时
  那么 配置行 [thresholdRatio:0.8] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-compaction-basic headroomTokens 默认
  假设 插件 dsh-compaction-basic 已注册
  当 读取解析后配置时
  那么 配置行 [headroomTokens:65536] 的默认值按目录行实现

场景: config dsh-compaction-basic headroomTokens 约束
  假设 插件 dsh-compaction-basic 已注册
  当 校验与生效时
  那么 配置行 [headroomTokens:65536] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-compaction-basic retainRatio 默认
  假设 插件 dsh-compaction-basic 已注册
  当 读取解析后配置时
  那么 配置行 [retainRatio:0.16（与 retainTokens 互斥）] 的默认值按目录行实现

场景: config dsh-compaction-basic retainRatio 约束
  假设 插件 dsh-compaction-basic 已注册
  当 校验与生效时
  那么 配置行 [retainRatio:0.16（与 retainTokens 互斥）] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-compaction-basic compactionRetries 默认
  假设 插件 dsh-compaction-basic 已注册
  当 读取解析后配置时
  那么 配置行 [compactionRetries:1] 的默认值按目录行实现

场景: config dsh-compaction-basic compactionRetries 约束
  假设 插件 dsh-compaction-basic 已注册
  当 校验与生效时
  那么 配置行 [compactionRetries:1] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-compaction-basic maxOverflowRetries 默认
  假设 插件 dsh-compaction-basic 已注册
  当 读取解析后配置时
  那么 配置行 [maxOverflowRetries:1（0 禁恢复）] 的默认值按目录行实现

场景: config dsh-compaction-basic maxOverflowRetries 约束
  假设 插件 dsh-compaction-basic 已注册
  当 校验与生效时
  那么 配置行 [maxOverflowRetries:1（0 禁恢复）] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-compaction-tool-result-pruner thresholdChars 默认
  假设 插件 dsh-compaction-tool-result-pruner 已注册
  当 读取解析后配置时
  那么 配置行 [thresholdChars:8192（Unicode 码点）] 的默认值按目录行实现

场景: config dsh-compaction-tool-result-pruner thresholdChars 约束
  假设 插件 dsh-compaction-tool-result-pruner 已注册
  当 校验与生效时
  那么 配置行 [thresholdChars:8192（Unicode 码点）] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-compaction-tool-result-pruner headChars 默认
  假设 插件 dsh-compaction-tool-result-pruner 已注册
  当 读取解析后配置时
  那么 配置行 [headChars:4096] 的默认值按目录行实现

场景: config dsh-compaction-tool-result-pruner headChars 约束
  假设 插件 dsh-compaction-tool-result-pruner 已注册
  当 校验与生效时
  那么 配置行 [headChars:4096] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-compaction-tool-result-pruner tailChars 默认
  假设 插件 dsh-compaction-tool-result-pruner 已注册
  当 读取解析后配置时
  那么 配置行 [tailChars:1024] 的默认值按目录行实现

场景: config dsh-compaction-tool-result-pruner tailChars 约束
  假设 插件 dsh-compaction-tool-result-pruner 已注册
  当 校验与生效时
  那么 配置行 [tailChars:1024] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-credentials-local watch 默认
  假设 插件 dsh-credentials-local 已注册
  当 读取解析后配置时
  那么 配置行 [watch:true] 的默认值按目录行实现

场景: config dsh-credentials-local watch 约束
  假设 插件 dsh-credentials-local 已注册
  当 校验与生效时
  那么 配置行 [watch:true] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-credentials-local debounceMs 默认
  假设 插件 dsh-credentials-local 已注册
  当 读取解析后配置时
  那么 配置行 [debounceMs:100] 的默认值按目录行实现

场景: config dsh-credentials-local debounceMs 约束
  假设 插件 dsh-credentials-local 已注册
  当 校验与生效时
  那么 配置行 [debounceMs:100] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-headless task 默认
  假设 插件 dsh-headless 已注册
  当 读取解析后配置时
  那么 配置行 [task:-（省略来自 stdin）] 的默认值按目录行实现

场景: config dsh-headless task 约束
  假设 插件 dsh-headless 已注册
  当 校验与生效时
  那么 配置行 [task:-（省略来自 stdin）] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-headless sessionId 默认
  假设 插件 dsh-headless 已注册
  当 读取解析后配置时
  那么 配置行 [sessionId:-（无存储会话则失败）] 的默认值按目录行实现

场景: config dsh-headless sessionId 约束
  假设 插件 dsh-headless 已注册
  当 校验与生效时
  那么 配置行 [sessionId:-（无存储会话则失败）] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-headless json 默认
  假设 插件 dsh-headless 已注册
  当 读取解析后配置时
  那么 配置行 [json:false] 的默认值按目录行实现

场景: config dsh-headless json 约束
  假设 插件 dsh-headless 已注册
  当 校验与生效时
  那么 配置行 [json:false] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-jobs-local maxConcurrentJobsPerOwner 默认
  假设 插件 dsh-jobs-local 已注册
  当 读取解析后配置时
  那么 配置行 [maxConcurrentJobsPerOwner:10] 的默认值按目录行实现

场景: config dsh-jobs-local maxConcurrentJobsPerOwner 约束
  假设 插件 dsh-jobs-local 已注册
  当 校验与生效时
  那么 配置行 [maxConcurrentJobsPerOwner:10] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-jobs-local retainBytes 默认
  假设 插件 dsh-jobs-local 已注册
  当 读取解析后配置时
  那么 配置行 [retainBytes:262144] 的默认值按目录行实现

场景: config dsh-jobs-local retainBytes 约束
  假设 插件 dsh-jobs-local 已注册
  当 校验与生效时
  那么 配置行 [retainBytes:262144] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-jobs-local settledRetainBytes 默认
  假设 插件 dsh-jobs-local 已注册
  当 读取解析后配置时
  那么 配置行 [settledRetainBytes:16384] 的默认值按目录行实现

场景: config dsh-jobs-local settledRetainBytes 约束
  假设 插件 dsh-jobs-local 已注册
  当 校验与生效时
  那么 配置行 [settledRetainBytes:16384] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-jobs-local pumpPollMs 默认
  假设 插件 dsh-jobs-local 已注册
  当 读取解析后配置时
  那么 配置行 [pumpPollMs:150] 的默认值按目录行实现

场景: config dsh-jobs-local pumpPollMs 约束
  假设 插件 dsh-jobs-local 已注册
  当 校验与生效时
  那么 配置行 [pumpPollMs:150] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-llm-deepseek-api-key apiKeyEnv 默认
  假设 插件 dsh-llm-deepseek-api-key 已注册
  当 读取解析后配置时
  那么 配置行 [apiKeyEnv:Volatile:'DEEPSEEK_API_KEY'（每请求解析）] 的默认值按目录行实现

场景: config dsh-llm-deepseek-api-key apiKeyEnv 约束
  假设 插件 dsh-llm-deepseek-api-key 已注册
  当 校验与生效时
  那么 配置行 [apiKeyEnv:Volatile:'DEEPSEEK_API_KEY'（每请求解析）] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-llm-pi-ai providers 默认
  假设 插件 dsh-llm-pi-ai 已注册
  当 读取解析后配置时
  那么 配置行 [providers:Volatile:Record（空=休眠）；per-provider apiKeyEnv/api/baseURL/models/compat/defaultContextWindow:262144/defaultMaxTokens:32768/defaultInput:['text']/headers/retryPolicy（normal 5 次）等] 的默认值按目录行实现

场景: config dsh-llm-pi-ai providers 约束
  假设 插件 dsh-llm-pi-ai 已注册
  当 校验与生效时
  那么 配置行 [providers:Volatile:Record（空=休眠）；per-provider apiKeyEnv/api/baseURL/models/compat/defaultContextWindow:262144/defaultMaxTokens:32768/defaultInput:['text']/headers/retryPolicy（normal 5 次）等] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-lsp-stdio servers 默认
  假设 插件 dsh-lsp-stdio 已注册
  当 读取解析后配置时
  那么 配置行 [servers:Record 非空] 的默认值按目录行实现

场景: config dsh-lsp-stdio servers 约束
  假设 插件 dsh-lsp-stdio 已注册
  当 校验与生效时
  那么 配置行 [servers:Record 非空] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-mcp-client（stdio 与 streamable-http 两变体） serverName 默认
  假设 插件 dsh-mcp-client（stdio 与 streamable-http 两变体） 已注册
  当 读取解析后配置时
  那么 配置行 [serverName:-（[A-Za-z0-9_-]{1,32} 且唯一，工具名 mcp__<server>__<raw>）] 的默认值按目录行实现

场景: config dsh-mcp-client（stdio 与 streamable-http 两变体） serverName 约束
  假设 插件 dsh-mcp-client（stdio 与 streamable-http 两变体） 已注册
  当 校验与生效时
  那么 配置行 [serverName:-（[A-Za-z0-9_-]{1,32} 且唯一，工具名 mcp__<server>__<raw>）] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-mcp-client（stdio 与 streamable-http 两变体） maxInstructionBytes 默认
  假设 插件 dsh-mcp-client（stdio 与 streamable-http 两变体） 已注册
  当 读取解析后配置时
  那么 配置行 [maxInstructionBytes:32768] 的默认值按目录行实现

场景: config dsh-mcp-client（stdio 与 streamable-http 两变体） maxInstructionBytes 约束
  假设 插件 dsh-mcp-client（stdio 与 streamable-http 两变体） 已注册
  当 校验与生效时
  那么 配置行 [maxInstructionBytes:32768] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-permission-presets presets 默认
  假设 插件 dsh-permission-presets 已注册
  当 读取解析后配置时
  那么 配置行 [presets:Record（默认 workspace-write=workspace-write+ask、danger-full-access=danger+never；custom/auto 保留）] 的默认值按目录行实现

场景: config dsh-permission-presets presets 约束
  假设 插件 dsh-permission-presets 已注册
  当 校验与生效时
  那么 配置行 [presets:Record（默认 workspace-write=workspace-write+ask、danger-full-access=danger+never；custom/auto 保留）] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-permission-presets defaultPreset 默认
  假设 插件 dsh-permission-presets 已注册
  当 读取解析后配置时
  那么 配置行 [defaultPreset:Volatile] 的默认值按目录行实现

场景: config dsh-permission-presets defaultPreset 约束
  假设 插件 dsh-permission-presets 已注册
  当 校验与生效时
  那么 配置行 [defaultPreset:Volatile] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-plan-mode section 默认
  假设 插件 dsh-plan-mode 已注册
  当 读取解析后配置时
  那么 配置行 [section:string（plan 活动期渲染 plan:policy 段）] 的默认值按目录行实现

场景: config dsh-plan-mode section 约束
  假设 插件 dsh-plan-mode 已注册
  当 校验与生效时
  那么 配置行 [section:string（plan 活动期渲染 plan:policy 段）] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-ptc-runtime-node launch 默认
  假设 插件 dsh-ptc-runtime-node 已注册
  当 读取解析后配置时
  那么 配置行 [launch:node-script|embedded] 的默认值按目录行实现

场景: config dsh-ptc-runtime-node launch 约束
  假设 插件 dsh-ptc-runtime-node 已注册
  当 校验与生效时
  那么 配置行 [launch:node-script|embedded] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-ptc-runtime-node executionMode 默认
  假设 插件 dsh-ptc-runtime-node 已注册
  当 读取解析后配置时
  那么 配置行 [executionMode:'per-call'|'persistent-in-memory' 默认 per-call] 的默认值按目录行实现

场景: config dsh-ptc-runtime-node executionMode 约束
  假设 插件 dsh-ptc-runtime-node 已注册
  当 校验与生效时
  那么 配置行 [executionMode:'per-call'|'persistent-in-memory' 默认 per-call] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-repeat-tool-reminder thresholds 默认
  假设 插件 dsh-repeat-tool-reminder 已注册
  当 读取解析后配置时
  那么 配置行 [thresholds:[3,5,8]] 的默认值按目录行实现

场景: config dsh-repeat-tool-reminder thresholds 约束
  假设 插件 dsh-repeat-tool-reminder 已注册
  当 校验与生效时
  那么 配置行 [thresholds:[3,5,8]] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-repeat-tool-reminder argumentsPreviewChars 默认
  假设 插件 dsh-repeat-tool-reminder 已注册
  当 读取解析后配置时
  那么 配置行 [argumentsPreviewChars:500] 的默认值按目录行实现

场景: config dsh-repeat-tool-reminder argumentsPreviewChars 约束
  假设 插件 dsh-repeat-tool-reminder 已注册
  当 校验与生效时
  那么 配置行 [argumentsPreviewChars:500] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-sandbox-policy mode 默认
  假设 插件 dsh-sandbox-policy 已注册
  当 读取解析后配置时
  那么 配置行 [mode:'read-only'（会话起始模式，DSH_PERMISSION_MODE/FR 覆盖）] 的默认值按目录行实现

场景: config dsh-sandbox-policy mode 约束
  假设 插件 dsh-sandbox-policy 已注册
  当 校验与生效时
  那么 配置行 [mode:'read-only'（会话起始模式，DSH_PERMISSION_MODE/FR 覆盖）] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-sandbox-policy workspaceRoot 默认
  假设 插件 dsh-sandbox-policy 已注册
  当 读取解析后配置时
  那么 配置行 [workspaceRoot:process.cwd()] 的默认值按目录行实现

场景: config dsh-sandbox-policy workspaceRoot 约束
  假设 插件 dsh-sandbox-policy 已注册
  当 校验与生效时
  那么 配置行 [workspaceRoot:process.cwd()] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-schedule deliveryHistoryDays 默认
  假设 插件 dsh-schedule 已注册
  当 读取解析后配置时
  那么 配置行 [deliveryHistoryDays:30] 的默认值按目录行实现

场景: config dsh-schedule deliveryHistoryDays 约束
  假设 插件 dsh-schedule 已注册
  当 校验与生效时
  那么 配置行 [deliveryHistoryDays:30] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-schedule deliveryHistoryRecords 默认
  假设 插件 dsh-schedule 已注册
  当 读取解析后配置时
  那么 配置行 [deliveryHistoryRecords:200] 的默认值按目录行实现

场景: config dsh-schedule deliveryHistoryRecords 约束
  假设 插件 dsh-schedule 已注册
  当 校验与生效时
  那么 配置行 [deliveryHistoryRecords:200] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-sdk-jsonrpc-server maxTokensAsSuccess 默认
  假设 插件 dsh-sdk-jsonrpc-server 已注册
  当 读取解析后配置时
  那么 配置行 [maxTokensAsSuccess:-（默认 false）] 的默认值按目录行实现

场景: config dsh-sdk-jsonrpc-server maxTokensAsSuccess 约束
  假设 插件 dsh-sdk-jsonrpc-server 已注册
  当 校验与生效时
  那么 配置行 [maxTokensAsSuccess:-（默认 false）] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-session-persistence-jsonl root 默认
  假设 插件 dsh-session-persistence-jsonl 已注册
  当 读取解析后配置时
  那么 配置行 [root:-必填（无默认，防 cwd 漂移）] 的默认值按目录行实现

场景: config dsh-session-persistence-jsonl root 约束
  假设 插件 dsh-session-persistence-jsonl 已注册
  当 校验与生效时
  那么 配置行 [root:-必填（无默认，防 cwd 漂移）] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-session-persistence-jsonl listConcurrency 默认
  假设 插件 dsh-session-persistence-jsonl 已注册
  当 读取解析后配置时
  那么 配置行 [listConcurrency:3] 的默认值按目录行实现

场景: config dsh-session-persistence-jsonl listConcurrency 约束
  假设 插件 dsh-session-persistence-jsonl 已注册
  当 校验与生效时
  那么 配置行 [listConcurrency:3] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-session-query-sqlite journalMode 默认
  假设 插件 dsh-session-query-sqlite 已注册
  当 读取解析后配置时
  那么 配置行 [journalMode:'wal'] 的默认值按目录行实现

场景: config dsh-session-query-sqlite journalMode 约束
  假设 插件 dsh-session-query-sqlite 已注册
  当 校验与生效时
  那么 配置行 [journalMode:'wal'] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-session-query-sqlite defaultLimit 默认
  假设 插件 dsh-session-query-sqlite 已注册
  当 读取解析后配置时
  那么 配置行 [defaultLimit:20] 的默认值按目录行实现

场景: config dsh-session-query-sqlite defaultLimit 约束
  假设 插件 dsh-session-query-sqlite 已注册
  当 校验与生效时
  那么 配置行 [defaultLimit:20] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-session-query-sqlite maxLimit 默认
  假设 插件 dsh-session-query-sqlite 已注册
  当 读取解析后配置时
  那么 配置行 [maxLimit:100] 的默认值按目录行实现

场景: config dsh-session-query-sqlite maxLimit 约束
  假设 插件 dsh-session-query-sqlite 已注册
  当 校验与生效时
  那么 配置行 [maxLimit:100] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-session-query-sqlite snippetChars 默认
  假设 插件 dsh-session-query-sqlite 已注册
  当 读取解析后配置时
  那么 配置行 [snippetChars:240] 的默认值按目录行实现

场景: config dsh-session-query-sqlite snippetChars 约束
  假设 插件 dsh-session-query-sqlite 已注册
  当 校验与生效时
  那么 配置行 [snippetChars:240] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-session-query-sqlite persistedReadConcurrency 默认
  假设 插件 dsh-session-query-sqlite 已注册
  当 读取解析后配置时
  那么 配置行 [persistedReadConcurrency:4] 的默认值按目录行实现

场景: config dsh-session-query-sqlite persistedReadConcurrency 约束
  假设 插件 dsh-session-query-sqlite 已注册
  当 校验与生效时
  那么 配置行 [persistedReadConcurrency:4] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-session-query-sqlite preparedSessionCacheSize 默认
  假设 插件 dsh-session-query-sqlite 已注册
  当 读取解析后配置时
  那么 配置行 [preparedSessionCacheSize:5] 的默认值按目录行实现

场景: config dsh-session-query-sqlite preparedSessionCacheSize 约束
  假设 插件 dsh-session-query-sqlite 已注册
  当 校验与生效时
  那么 配置行 [preparedSessionCacheSize:5] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-skill-filesystem providerName 默认
  假设 插件 dsh-skill-filesystem 已注册
  当 读取解析后配置时
  那么 配置行 [providerName:'filesystem'] 的默认值按目录行实现

场景: config dsh-skill-filesystem providerName 约束
  假设 插件 dsh-skill-filesystem 已注册
  当 校验与生效时
  那么 配置行 [providerName:'filesystem'] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-spill-local cleanupPeriodDays 默认
  假设 插件 dsh-spill-local 已注册
  当 读取解析后配置时
  那么 配置行 [cleanupPeriodDays:30（0 禁清理）] 的默认值按目录行实现

场景: config dsh-spill-local cleanupPeriodDays 约束
  假设 插件 dsh-spill-local 已注册
  当 校验与生效时
  那么 配置行 [cleanupPeriodDays:30（0 禁清理）] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-spill-policy maxInlineTokens 默认
  假设 插件 dsh-spill-policy 已注册
  当 读取解析后配置时
  那么 配置行 [maxInlineTokens:-（省略禁保留）] 的默认值按目录行实现

场景: config dsh-spill-policy maxInlineTokens 约束
  假设 插件 dsh-spill-policy 已注册
  当 校验与生效时
  那么 配置行 [maxInlineTokens:-（省略禁保留）] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-subagent maxActiveSubagents 默认
  假设 插件 dsh-subagent 已注册
  当 读取解析后配置时
  那么 配置行 [maxActiveSubagents:Volatile:8] 的默认值按目录行实现

场景: config dsh-subagent maxActiveSubagents 约束
  假设 插件 dsh-subagent 已注册
  当 校验与生效时
  那么 配置行 [maxActiveSubagents:Volatile:8] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-subagent maxDepth 默认
  假设 插件 dsh-subagent 已注册
  当 读取解析后配置时
  那么 配置行 [maxDepth:Volatile:1] 的默认值按目录行实现

场景: config dsh-subagent maxDepth 约束
  假设 插件 dsh-subagent 已注册
  当 校验与生效时
  那么 配置行 [maxDepth:Volatile:1] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-subagent-acp providerName 默认
  假设 插件 dsh-subagent-acp 已注册
  当 读取解析后配置时
  那么 配置行 [providerName:'acp'] 的默认值按目录行实现

场景: config dsh-subagent-acp providerName 约束
  假设 插件 dsh-subagent-acp 已注册
  当 校验与生效时
  那么 配置行 [providerName:'acp'] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-system-prompt includeHarnessIdentity 默认
  假设 插件 dsh-system-prompt 已注册
  当 读取解析后配置时
  那么 配置行 [includeHarnessIdentity:true] 的默认值按目录行实现

场景: config dsh-system-prompt includeHarnessIdentity 约束
  假设 插件 dsh-system-prompt 已注册
  当 校验与生效时
  那么 配置行 [includeHarnessIdentity:true] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-system-prompt includeRuntimeContext 默认
  假设 插件 dsh-system-prompt 已注册
  当 读取解析后配置时
  那么 配置行 [includeRuntimeContext:true] 的默认值按目录行实现

场景: config dsh-system-prompt includeRuntimeContext 约束
  假设 插件 dsh-system-prompt 已注册
  当 校验与生效时
  那么 配置行 [includeRuntimeContext:true] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-terminal-bash backendType 默认
  假设 插件 dsh-terminal-bash 已注册
  当 读取解析后配置时
  那么 配置行 [backendType:'shell'] 的默认值按目录行实现

场景: config dsh-terminal-bash backendType 约束
  假设 插件 dsh-terminal-bash 已注册
  当 校验与生效时
  那么 配置行 [backendType:'shell'] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-terminal-bash shellDialect 默认
  假设 插件 dsh-terminal-bash 已注册
  当 读取解析后配置时
  那么 配置行 [shellDialect:'bash'] 的默认值按目录行实现

场景: config dsh-terminal-bash shellDialect 约束
  假设 插件 dsh-terminal-bash 已注册
  当 校验与生效时
  那么 配置行 [shellDialect:'bash'] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-time-context refreshIntervalMs 默认
  假设 插件 dsh-time-context 已注册
  当 读取解析后配置时
  那么 配置行 [refreshIntervalMs:600000（0=每合格 step 注入）] 的默认值按目录行实现

场景: config dsh-time-context refreshIntervalMs 约束
  假设 插件 dsh-time-context 已注册
  当 校验与生效时
  那么 配置行 [refreshIntervalMs:600000（0=每合格 step 注入）] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-tool-ask-user mode 默认
  假设 插件 dsh-tool-ask-user 已注册
  当 读取解析后配置时
  那么 配置行 [mode:'legacy'|'timed'（默认阻塞 legacy）] 的默认值按目录行实现

场景: config dsh-tool-ask-user mode 约束
  假设 插件 dsh-tool-ask-user 已注册
  当 校验与生效时
  那么 配置行 [mode:'legacy'|'timed'（默认阻塞 legacy）] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-tool-ask-user timeout 默认
  假设 插件 dsh-tool-ask-user 已注册
  当 读取解析后配置时
  那么 配置行 [timeout:120s] 的默认值按目录行实现

场景: config dsh-tool-ask-user timeout 约束
  假设 插件 dsh-tool-ask-user 已注册
  当 校验与生效时
  那么 配置行 [timeout:120s] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-tool-bash / dsh-tool-pwsh enableRunInBackground 默认
  假设 插件 dsh-tool-bash / dsh-tool-pwsh 已注册
  当 读取解析后配置时
  那么 配置行 [enableRunInBackground:true] 的默认值按目录行实现

场景: config dsh-tool-bash / dsh-tool-pwsh enableRunInBackground 约束
  假设 插件 dsh-tool-bash / dsh-tool-pwsh 已注册
  当 校验与生效时
  那么 配置行 [enableRunInBackground:true] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-tool-bash / dsh-tool-pwsh promoteOnTimeout 默认
  假设 插件 dsh-tool-bash / dsh-tool-pwsh 已注册
  当 读取解析后配置时
  那么 配置行 [promoteOnTimeout:true（超时转后台 job 不杀；仅后台可用时生效）] 的默认值按目录行实现

场景: config dsh-tool-bash / dsh-tool-pwsh promoteOnTimeout 约束
  假设 插件 dsh-tool-bash / dsh-tool-pwsh 已注册
  当 校验与生效时
  那么 配置行 [promoteOnTimeout:true（超时转后台 job 不杀；仅后台可用时生效）] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-tool-bash-persistent / dsh-tool-pwsh-persistent backendType 默认
  假设 插件 dsh-tool-bash-persistent / dsh-tool-pwsh-persistent 已注册
  当 读取解析后配置时
  那么 配置行 [backendType:'shell'] 的默认值按目录行实现

场景: config dsh-tool-bash-persistent / dsh-tool-pwsh-persistent backendType 约束
  假设 插件 dsh-tool-bash-persistent / dsh-tool-pwsh-persistent 已注册
  当 校验与生效时
  那么 配置行 [backendType:'shell'] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-tool-bash-persistent / dsh-tool-pwsh-persistent timeoutMs 默认
  假设 插件 dsh-tool-bash-persistent / dsh-tool-pwsh-persistent 已注册
  当 读取解析后配置时
  那么 配置行 [timeoutMs:300000] 的默认值按目录行实现

场景: config dsh-tool-bash-persistent / dsh-tool-pwsh-persistent timeoutMs 约束
  假设 插件 dsh-tool-bash-persistent / dsh-tool-pwsh-persistent 已注册
  当 校验与生效时
  那么 配置行 [timeoutMs:300000] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-tool-bash-persistent / dsh-tool-pwsh-persistent maxOutputChars 默认
  假设 插件 dsh-tool-bash-persistent / dsh-tool-pwsh-persistent 已注册
  当 读取解析后配置时
  那么 配置行 [maxOutputChars:16000] 的默认值按目录行实现

场景: config dsh-tool-bash-persistent / dsh-tool-pwsh-persistent maxOutputChars 约束
  假设 插件 dsh-tool-bash-persistent / dsh-tool-pwsh-persistent 已注册
  当 校验与生效时
  那么 配置行 [maxOutputChars:16000] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-tool-fs-search sampleOverCapGlobResults 默认
  假设 插件 dsh-tool-fs-search 已注册
  当 读取解析后配置时
  那么 配置行 [sampleOverCapGlobResults:必填] 的默认值按目录行实现

场景: config dsh-tool-fs-search sampleOverCapGlobResults 约束
  假设 插件 dsh-tool-fs-search 已注册
  当 校验与生效时
  那么 配置行 [sampleOverCapGlobResults:必填] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-tool-jobs waitTimeoutMs 默认
  假设 插件 dsh-tool-jobs 已注册
  当 读取解析后配置时
  那么 配置行 [waitTimeoutMs:30s] 的默认值按目录行实现

场景: config dsh-tool-jobs waitTimeoutMs 约束
  假设 插件 dsh-tool-jobs 已注册
  当 校验与生效时
  那么 配置行 [waitTimeoutMs:30s] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-tool-jobs maxWaitTimeoutMs 默认
  假设 插件 dsh-tool-jobs 已注册
  当 读取解析后配置时
  那么 配置行 [maxWaitTimeoutMs:10min] 的默认值按目录行实现

场景: config dsh-tool-jobs maxWaitTimeoutMs 约束
  假设 插件 dsh-tool-jobs 已注册
  当 校验与生效时
  那么 配置行 [maxWaitTimeoutMs:10min] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-tool-jobs completionDelivery 默认
  假设 插件 dsh-tool-jobs 已注册
  当 读取解析后配置时
  那么 配置行 [completionDelivery:'wakeup'] 的默认值按目录行实现

场景: config dsh-tool-jobs completionDelivery 约束
  假设 插件 dsh-tool-jobs 已注册
  当 校验与生效时
  那么 配置行 [completionDelivery:'wakeup'] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-tool-lsp maxLocations 默认
  假设 插件 dsh-tool-lsp 已注册
  当 读取解析后配置时
  那么 配置行 [maxLocations:100] 的默认值按目录行实现

场景: config dsh-tool-lsp maxLocations 约束
  假设 插件 dsh-tool-lsp 已注册
  当 校验与生效时
  那么 配置行 [maxLocations:100] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-tool-lsp maxResultChars 默认
  假设 插件 dsh-tool-lsp 已注册
  当 读取解析后配置时
  那么 配置行 [maxResultChars:16000] 的默认值按目录行实现

场景: config dsh-tool-lsp maxResultChars 约束
  假设 插件 dsh-tool-lsp 已注册
  当 校验与生效时
  那么 配置行 [maxResultChars:16000] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-tool-lsp timeoutMs 默认
  假设 插件 dsh-tool-lsp 已注册
  当 读取解析后配置时
  那么 配置行 [timeoutMs:60000] 的默认值按目录行实现

场景: config dsh-tool-lsp timeoutMs 约束
  假设 插件 dsh-tool-lsp 已注册
  当 校验与生效时
  那么 配置行 [timeoutMs:60000] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-tool-session-query maxSearchResults 默认
  假设 插件 dsh-tool-session-query 已注册
  当 读取解析后配置时
  那么 配置行 [maxSearchResults:100] 的默认值按目录行实现

场景: config dsh-tool-session-query maxSearchResults 约束
  假设 插件 dsh-tool-session-query 已注册
  当 校验与生效时
  那么 配置行 [maxSearchResults:100] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-tool-session-query searchTimeoutMs 默认
  假设 插件 dsh-tool-session-query 已注册
  当 读取解析后配置时
  那么 配置行 [searchTimeoutMs:30000] 的默认值按目录行实现

场景: config dsh-tool-session-query searchTimeoutMs 约束
  假设 插件 dsh-tool-session-query 已注册
  当 校验与生效时
  那么 配置行 [searchTimeoutMs:30000] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-tool-str-replace-editor maxOutputChars 默认
  假设 插件 dsh-tool-str-replace-editor 已注册
  当 读取解析后配置时
  那么 配置行 [maxOutputChars:16000] 的默认值按目录行实现

场景: config dsh-tool-str-replace-editor maxOutputChars 约束
  假设 插件 dsh-tool-str-replace-editor 已注册
  当 校验与生效时
  那么 配置行 [maxOutputChars:16000] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-tool-subagent provider 默认
  假设 插件 dsh-tool-subagent 已注册
  当 读取解析后配置时
  那么 配置行 [provider:必填] 的默认值按目录行实现

场景: config dsh-tool-subagent provider 约束
  假设 插件 dsh-tool-subagent 已注册
  当 校验与生效时
  那么 配置行 [provider:必填] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-tool-subagent toolName 默认
  假设 插件 dsh-tool-subagent 已注册
  当 读取解析后配置时
  那么 配置行 [toolName:'subagent'（实例名互异）] 的默认值按目录行实现

场景: config dsh-tool-subagent toolName 约束
  假设 插件 dsh-tool-subagent 已注册
  当 校验与生效时
  那么 配置行 [toolName:'subagent'（实例名互异）] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-tool-terminal enableRunInBackground 默认
  假设 插件 dsh-tool-terminal 已注册
  当 读取解析后配置时
  那么 配置行 [enableRunInBackground:true] 的默认值按目录行实现

场景: config dsh-tool-terminal enableRunInBackground 约束
  假设 插件 dsh-tool-terminal 已注册
  当 校验与生效时
  那么 配置行 [enableRunInBackground:true] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-tool-todo allowParallelInProgress 默认
  假设 插件 dsh-tool-todo 已注册
  当 读取解析后配置时
  那么 配置行 [allowParallelInProgress:必填部署选择（true=多 in_progress；false=恰一且多标拒绝）] 的默认值按目录行实现

场景: config dsh-tool-todo allowParallelInProgress 约束
  假设 插件 dsh-tool-todo 已注册
  当 校验与生效时
  那么 配置行 [allowParallelInProgress:必填部署选择（true=多 in_progress；false=恰一且多标拒绝）] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-tool-web search 默认
  假设 插件 dsh-tool-web 已注册
  当 读取解析后配置时
  那么 配置行 [search:true] 的默认值按目录行实现

场景: config dsh-tool-web search 约束
  假设 插件 dsh-tool-web 已注册
  当 校验与生效时
  那么 配置行 [search:true] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-tool-web fetch 默认
  假设 插件 dsh-tool-web 已注册
  当 读取解析后配置时
  那么 配置行 [fetch:true] 的默认值按目录行实现

场景: config dsh-tool-web fetch 约束
  假设 插件 dsh-tool-web 已注册
  当 校验与生效时
  那么 配置行 [fetch:true] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-tool-web fetchTimeoutMs 默认
  假设 插件 dsh-tool-web 已注册
  当 读取解析后配置时
  那么 配置行 [fetchTimeoutMs:30000] 的默认值按目录行实现

场景: config dsh-tool-web fetchTimeoutMs 约束
  假设 插件 dsh-tool-web 已注册
  当 校验与生效时
  那么 配置行 [fetchTimeoutMs:30000] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-tool-web searchTimeoutMs 默认
  假设 插件 dsh-tool-web 已注册
  当 读取解析后配置时
  那么 配置行 [searchTimeoutMs:30000] 的默认值按目录行实现

场景: config dsh-tool-web searchTimeoutMs 约束
  假设 插件 dsh-tool-web 已注册
  当 校验与生效时
  那么 配置行 [searchTimeoutMs:30000] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-tool-web fetchMaxOutputChars 默认
  假设 插件 dsh-tool-web 已注册
  当 读取解析后配置时
  那么 配置行 [fetchMaxOutputChars:200000] 的默认值按目录行实现

场景: config dsh-tool-web fetchMaxOutputChars 约束
  假设 插件 dsh-tool-web 已注册
  当 校验与生效时
  那么 配置行 [fetchMaxOutputChars:200000] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-web-deepseek-api / dsh-web-fetch-http count 默认
  假设 插件 dsh-web-deepseek-api / dsh-web-fetch-http 已注册
  当 读取解析后配置时
  那么 配置行 [count:10（1..50）] 的默认值按目录行实现

场景: config dsh-web-deepseek-api / dsh-web-fetch-http count 约束
  假设 插件 dsh-web-deepseek-api / dsh-web-fetch-http 已注册
  当 校验与生效时
  那么 配置行 [count:10（1..50）] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

场景: config dsh-web-deepseek-api / dsh-web-fetch-http sendOriginalQuestion 默认
  假设 插件 dsh-web-deepseek-api / dsh-web-fetch-http 已注册
  当 读取解析后配置时
  那么 配置行 [sendOriginalQuestion:true] 的默认值按目录行实现

场景: config dsh-web-deepseek-api / dsh-web-fetch-http sendOriginalQuestion 约束
  假设 插件 dsh-web-deepseek-api / dsh-web-fetch-http 已注册
  当 校验与生效时
  那么 配置行 [sendOriginalQuestion:true] 的类型与约束生效，自有键 kebab-case 转换、wire 键保持原拼写

Rule: REQ-FR-CONFIG-11 — 启动失败策略

场景: 必需 id 失败拒启动与其余继续
  假设 agent-loop 激活失败、另一非必需工具插件同时失败
  当 启动结算时
  那么 以 StartupError 拒启动并列出失败与等待清单（含 agent-loop），而非必需者仅单条警告

场景: 缺注入服务 pending
  假设 某可选插件声明的注入服务未由任何插件提供
  当 装载时
  那么 该插件 fiber 置 pending 容忍不中止装配；同情形发生在必需 id 上则致命

场景: 整行替换非键级合并
  假设 后层补丁行与前层同 id 但仅给出部分 config 键
  当 展开装配时
  那么 该行整体以后层版本生效，前层同 id 行的其余 config 键不参与合并

场景: env 缺省表达式映射
  假设 FR_PERMISSION_MODE 分别设置与未设置
  当 求值受限子集的 env 缺省表达式时
  那么 结果与 dsh !!js process.env.DSH_PERMISSION_MODE ?? 'workspace-write' 逐值一致

Rule: REQ-FR-CONFIG-13 — .env 分层

场景: 三层优先序
  假设 同名变量同时出现在进程环境、项目 .env 与家目录 .env
  当 fr-config 启动解析时
  那么 进程 env 值胜出，其次项目 .env，最后家目录 .env

场景: 已设名不被覆盖
  假设 某变量已由进程环境设定且项目 .env 试图赋不同值
  当 解析 .env 时
  那么 项目 .env 的值被忽略、不替换既有值

场景: bootstrap 专名拒绝与代理例外
  假设 项目 .env 试图设 FR_SESSIONS_ROOT 与 PATH，家目录 .env 试图设 HTTPS_PROXY
  当 解析各层时
  那么 前两者在任何 .env 层均拒绝（FR_ 前缀与 bootstrap 专名）；家目录层的 HTTPS_PROXY 依例外接受，项目层设代理四键仍拒绝
场景: 任一层违例两文件均不应用
  假设 项目 .env 含合法变量，家目录 .env 含 FR_ 前缀违例名
  当 启动解析分层环境时
  那么 启动终止报错，且项目 .env 的合法值也不应用

场景: 读错误非 ENOENT 警告跳层
  假设 某 .env 路径存在但不可读（如为目录）
  当 解析该层时
  那么 stderr 输出一行警告，该层按空层跳过，启动继续

场景: 专名检查大小写不敏感
  假设 项目 .env 以小写 path= 设置变量
  当 bootstrap 校验时
  那么 该行拒绝，启动终止

场景: cwd 即家目录时家层不读
  假设 进程 cwd 与 FR_HOME 解析为同一路径
  当 启动解析分层环境时
  那么 家目录层不读取，仅项目层生效一次

场景: materialize 与环境快照双轨
  假设 项目 .env 设未被进程环境定义的变量 X
  当 分层解析完成后
  那么 进程 env 可读到 X，且返回的快照按 process > project-env > user-env 层序提供逐名来源查询

场景: env 解析方言对齐 Node util.parseEnv
  假设 .env 含 # 注释行、单引号与双引号值、双引号内转义与跨行多行值
  当 解析该文件时
  那么 各形态结果与 Node util.parseEnv 方言逐值一致（键值去包裹、转义展开、多行保留）

Rule: REQ-FR-CONFIG-14 — `!!js` 结构感知保真

场景: 块标量字符串含 tag 形文本逐字装载
  测试: block_scalar_tag_shaped_text_mounts_verbatim
  假设 patch 行 config 含块标量字符串，其内容行形如 key: !!js v
  当 解析该 patch 文档时
  那么 字符串值逐字为该文本内容，不翻转不求值

场景: 引号多行标量含 tag 形文本逐字装载
  测试: quoted_multiline_tag_shaped_text_mounts_verbatim
  假设 patch 行 config 含引号多行标量，其内容行形如 key: !!js v
  当 解析该 patch 文档时
  那么 字符串值逐字为该文本内容，不翻转不求值

场景: 识别位外结构值位 fail-closed（flow 式）
  测试: flow_style_js_tag_is_rejected_fail_closed
  假设 !!js 出现在 flow 式映射值位
  当 解析 patch 文档时
  那么 整个文档被拒绝，错误含 outside a recognized value position 文案

场景: 异形键结构值位真实标签 fail-closed
  测试: exotic_structural_key_js_tag_fails_closed
  假设 异形键（含 `.` 等非常规字符的键）的结构值位携带真实 `!!js` 标签
  当 解析 patch 文档时
  那么 整个文档被拒绝，错误含 outside a recognized value position 文案

场景: dump 内容行单叹号 js 逐字且整篇再 parse 等于挂载
  测试: dump_content_js_shaped_lines_survive_round_trip
  假设 某行字符串值内容含形如 key: !js v 的行，且同树另有真实 !!js 表达式
  当 render_dump 输出时
  那么 内容行保持单叹号逐字，表达式行打印 !!js，输出经结构感知解析等于合成树

## Dependencies

None.

## Source Trace

- doc/prd.md#6.1: 装配 = 有序 bundle 栈；按键覆盖合并语义与装载文件布局（2026-10-22 Q1-Q5 勘误后）
- doc/prd.md#6.2: 编译产物为 dougong loader 文档；"分层、行覆盖、YAML 在 dougong 中一概不存在——它们全部是 fr-config 的职责"
- doc/prd.md#6.3: schema 真源为 fr-schema 声明式描述树（单一源派生 validate 与 to_json_schema；装配期查无 schema 跳过、失败进 failures 不阻塞；双侧校验）；fr dump-config / dump-config-schema
- doc/prd.md#6.4: 关键默认值表
- confidence: high（PRD 第 6 节字段级规格，dougong 事实已一手核实）
- doc/dsh-source-notes.md: 源码级技术关键点（file:line 证据）
- dsh app-boot 装载栈（2026-10-22 Q1-Q5 裁决依据）：profile.ts:37-103,169-195,740-806；profile-context.ts:63-85；index.ts:520-593
- dsh .env 分层（2026-10-22 Q11-Q14 裁决依据）：app-boot src/index.ts:114-259（loadLayeredEnv:238-259 / readEnvLayer:193-225 / BOOTSTRAP_NAMES:136-157 / BOOTSTRAP_PREFIXES:160 / HOME_LAYER_PROXY_NAMES:169）；util/launch-environment/src/index.ts:16-137（快照 get/getFrom 信任序与 ctx 槽）；util/home-paths/src/index.ts:76-121（resolveRoot / resolveDshHome）

## Open Questions

None.
<!-- /agent-spec:requirement -->

<!-- agent-spec:requirement id=REQ-FR-SESSION title="会话系统（v4）" tags=session,persistence source=doc/prd.md -->
## Problem

dsh 的“协议即会话日志”原则要求：模型可见即已记录，每次 LLM 请求必须能从会话日志确定性重建。fly-rafter 需要完整的 v4 会话格式：追加式 JSONL+zstd、surface/replace-op、deriveMessages、fork、abort 补偿，字段与 dsh 逐字一致。

## Requirements

- [REQ-FR-SESSION-1] fr-session MUST 实现全部 durable SessionEvent 类型与字段，与 dsh `packages/core/session/src/types.ts` 判别联合逐字一致；公共字段 type/seq/time/data/ignorable?/surfaceOp?/sourceEventSeqs?；request/header 携带 EpochHeader（config + tools + adapterDefaults，reason: initial/resume/change/series，startsSeries?: true）；turn/end reason MUST 为七变体：completed / aborted{reason: user|parent|hook{reason}|disposed} / error{LlmFailure} / max-tokens / blocked / interrupted（crash 孤儿 turn 由 resume 追加关闭，loop 永不 live 发出）/ forked（仅 fork seed 携带）（2026-10-23 勘误 E1）；user/message 的 data MUST 为 UserMessage 直载（无 {turn,step,message} 包裹，其余四类 surface 事件均 {turn,step,…} 包裹，勘误 E2）。
- [REQ-FR-SESSION-2] fr-session MUST 只实现会话格式 v4；文件为 `session.v4.jsonl`（可选 zstd 压缩 `session.v4.jsonl.zstd`）；世代命名，永不改名或删除已发布文件；读到非 v4 文件 MUST 报明确错误；写路径 = 编码 → 校验 → 独占发布后继文件。
- [REQ-FR-SESSION-3] fr-session MUST 支持 surface 与 replace-op：压缩经 `{op:'replace', startSeq, endSeq}` 重写表面，新内容引用 sourceEventSeqs，旧内容永久留存于日志。
- [REQ-FR-SESSION-4] fr-session MUST 实现 deriveMessages：每次请求前从日志按 request/header 世代 + 表面 + 工具增删 developer/message 重建消息数组并冻结；`tool/ptc-dispatch*` 一律忽略；同输入两次重建 MUST 逐字节一致。
- [REQ-FR-SESSION-5] fr-session MUST 实现 session/fork：以 sourceEventSeqs 前缀派生新会话；fork 会话与子代理会话 MUST NOT 被 `--session-id`/`session/resume` 采纳。
- [REQ-FR-SESSION-6] abort 时 MUST 为未派发的工具调用补合成错误 `tool/result`，保证重放合法。
- [REQ-FR-SESSION-7] 重建时遇到未知必需事件 MUST 拒绝重建（ignorable: true 语义照搬 dsh）。
- [REQ-FR-SESSION-8] SessionHeader 逻辑头 MUST 为 version/id/createdAt/cwd?/parentSession?/isSeeded/origin/delegationDepth?/agentPreset?（cwd、parentSession、origin、delegationDepth、agentPreset 可选；delegationDepth 缺省即 0；origin 值只允许 'subagent'）；磁盘头 MUST 恰含六必需键 type/version/id/createdAt/isSeeded/delegationDepth；MUST NOT 定义或接受 seedLength 字段（dsh session/src/types.ts:95-131、session-persistence-jsonl/src/format.ts:84-96）。
- [REQ-FR-SESSION-9] 消息角色 System/Developer/User/Assistant/ToolResult MUST 均带 MessageSource 判别；ContentBlock MUST 为全集：text、reasoning、image、file、tool-call、tool-addition、tool-removal。
- [REQ-FR-SESSION-10] system prompt MUST 以 system/message 节点流存在于表面，支持 in-history 增量追加与 first-node 归一化两条路由；空渲染 MUST 清空活跃尾部 system 节点且 head 节点以 replace 落为空文本节点（head 亦占节点，dsh agent-loop/src/runtime-context.ts:104-110）。

- [REQ-FR-SESSION-11] 编码与发布不变量 MUST：种子信封仅六键、seq 从 0 连续；首发 wx 0600 fsync 后 link+unlink 独占发布（禁 rename）并逐级目录 fsync；append 失败 truncate 回滚；zstd 撕裂尾恢复完整记录；文件名世代与头 version 不一致报错（dsh session-persistence-jsonl/src/index.ts:1282-1461）。
- [REQ-FR-SESSION-12] surface 约束 MUST：仅 system/developer/user/assistant message 与 tool/result 五类入表面；replace 区间为表面节点双闭区间且必须早于当前 seq；sourceEventSeqs 非空、无重复、全部更小、必须覆盖全部被遮蔽节点、assistant 禁带；tool/result 替换只许改 content；node-0 只许 system/message 单节点重写（dsh surface.ts:50-56,290-507）。
- [REQ-FR-SESSION-13] abort 两级补偿 MUST：调度器跳过 = ABORTED_BEFORE_DISPATCH（AbortError）；步级恢复 = TOOL_NOT_STARTED / TOOL_OUTCOME_UNKNOWN（精确文案）；恢复 seq 从 last.seq+1 连续、time 复用最后真实事件（dsh repair.ts:15-197、tool-calls.ts:250-260）。
- [REQ-FR-SESSION-14] sourceEventSeqs 落盘 MUST 为折叠编码（连续 run 长度 3 以上折叠为 [start,end]），解码校验严格递增（dsh seq-ranges.ts:18-67）。
- [REQ-FR-SESSION-15] deriveMessages 投影 MUST：contentGeneration 变化时从该节点起整表重建，纯尾增长走增量投影；空 assistant（无内容块仅承载 max-tokens usage）MUST 投影为 null（dsh session/src/index.ts:984-1005、agent-loop/src/agent.ts:686-694）。
- [REQ-FR-SESSION-16] request/header canonical MUST：canonicalHeader 的 tools 为空时整体省略；tool schema 以 JSON.stringify 形态做顺序敏感比较；工具增删仅当 baseline 存在且名字集合有差才追加 developer/message，headerSeq 仅 additions>0 时携带（dsh agent-loop/src/agent.ts:635-665、request-header.ts:21-52）。
- [REQ-FR-SESSION-17] 继承切点 MUST 为最后一个 tagged session/end-seed 所在 seq：isSeeded 头必须携带继承事件数（cut），非 isSeeded 头 cut 必须为 0（dsh session-persistence-jsonl/src/format.ts:117-127）。
- [REQ-FR-SESSION-18] 事件词表 MUST 以激活期声明聚合（2026-10-23 Q34A）：fr-session 内核类型静态 + 各插件激活期声明其拥有的持久事件类型名，fr-session 聚合为读路径已知集（语义同 dsh KNOWN_SESSION_EVENT_TYPES，known-event-types.ts:22-85）；重复声明同类型 MUST 报装配错误（fail-closed）；descriptor 自由 JSON 扩展键 MUST NOT 作为声明通道（dougong 宿主 trait 不回透 descriptor）。
- [REQ-FR-SESSION-19] plugin 记录通道 MUST（2026-10-23 Q33A）：fr-session 提供 append-record 服务入口——ignorable:true 强制、类型名 `plugin:` 小写段语法校验（语法同 dsh types.ts:525）、data 丢失无损 JSON 校验；读回 API 等价 dsh pluginRecordOf；M1 无调用方，调用方约束入合约 Boundaries。
- [REQ-FR-SESSION-20] tools 投影 MUST（2026-10-23 Q36A 补遗，dsh tool-history.ts）：request/header 进 headers 表，reason==='series' 或 startsSeries 或同名工具定义变更（JSON.stringify 比较）即重置基线；developer/message 的 tool-addition 经 headerSeq 解析定义追加进 updates；快照 = {tools: 基线, updates: [{messageId, additions}]}；active≠available 时 legacy 回退 updates: []。
- [REQ-FR-SESSION-21] 写侧坐标正数 MUST（2026-10-24 裁决 B 案，19.2 条 17）：system/message 与 developer/message 的写侧 admission 对 turn 或 step 为 0 即拒（文案 `session event <type> turn must be positive` / `session event <type> step must be positive` 四条），镜像存储侧 positivity 门（dsh assertV4SystemMessageFields / assertV4DeveloperData developer 分支的读侧族）；turn/step 编号自 1 起；turn 链单调性不入写侧（读侧专属不变量）。
- [REQ-FR-SESSION-22] 写侧 system/message 保护头 MUST（2026-10-24 裁决 B 案，19.2 条 18）：surface 非空且保护头缺席时的 system/message 写入即拒，文案与存储侧逐字一致（`system/message requires a protected first surface head`）；空 surface 首插 system/message 建立保护头，单节点 system/message 重写转移保护头——写侧折叠状态与读侧 fold_surface 语义逐点一致，seed 重放同受检。
## Scenarios

Rule: REQ-FR-SESSION-1 — v4 事件全集与 EpochHeader

场景: v4 会话写入与读取回环
  假设 一个含 turn/step/消息/工具事件的会话被持久化为 session.v4.jsonl
  当 读取该文件重建会话时
  那么 全部事件逐条还原，无丢失与变形

场景: request/header reason 枚举
  假设 同一会话经历新建、续用、配置变化与请求序列中段
  当 各情形生成 request/header 时
  那么 reason 分别为 initial、resume、change、series

场景: crash 孤儿 turn 的 interrupted 关闭
  假设 一份持久化日志的尾 turn 只有 turn/start 而无 turn/end（进程崩溃）
  当 resume 重建该会话时
  那么 追加合成 step/end（若 step 开）与 turn/end{kind: interrupted}，seq 连续、time 复用最后真实事件，且 loop 永不 live 发出 interrupted

场景: fork seed 的 forked 关闭
  假设 fork 边界落在源会话的开放 turn 内
  当 构造 fork seed 时
  那么 子会话在切点携带 tagged session/end-seed，开放 turn 以 turn/end{kind: forked} 关闭，合成 tool/result 文案取 forked CLOSER_TEXT

场景: user/message 直载形态
  假设 一条 user/message 与一条 developer/message 事件落盘
  当 读取两事件的 data 字段时
  那么 user/message 的 data 即 UserMessage 本体（无 {turn,step,message} 包裹），developer/message 的 data 为 {turn,step,message,headerSeq?}

Rule: REQ-FR-SESSION-2 — v4 文件与写路径

场景: 非 v4 文件被拒绝
  假设 存在一个 version 不等于 4 的会话文件
  当 尝试加载时
  那么 返回明确错误且不产生任何写入

场景: zstd 压缩回环
  假设 启用 zstd 压缩
  当 写出并重新加载 session.v4.jsonl.zstd 时
  那么 事件序列逐条一致

场景: 写路径校验失败不发布后继文件
  假设 写路径中编码或校验失败
  当 走写路径时
  那么 不产生后继文件且已发布文件保持不变

Rule: REQ-FR-SESSION-3 — surface 与 replace-op

场景: replace-op 后源事件永久留存
  假设 压缩以 op 为 replace、startSeq 与 endSeq 界定的区间结算
  当 重放读取该区间时
  那么 区间内原事件全部留存，表面显示引用 sourceEventSeqs 的新内容

Rule: REQ-FR-SESSION-4 — deriveMessages 确定性

场景: deriveMessages 两次重建逐字节一致
  假设 同一份会话日志
  当 两次执行 deriveMessages 时
  那么 冻结请求逐字节一致

场景: ptc-dispatch 子调用不进请求
  假设 日志含 tool/ptc-dispatch-start 与 tool/ptc-dispatch 事件
  当 deriveMessages 重建时
  那么 重建结果不含任何子调用消息

Rule: REQ-FR-SESSION-5 — fork 与续用拒绝

场景: fork 会话拒绝被续用
  假设 存在一个由 session/fork 派生的会话
  当 以 --session-id 采纳该会话时
  那么 拒绝并报明确错误

Rule: REQ-FR-SESSION-6 — abort 补偿

场景: abort 补合成 tool/result
  假设 流中断时存在未派发的工具调用
  当 abort 结算时
  那么 每个未派发调用补一条合成错误 tool/result，重放合法

Rule: REQ-FR-SESSION-7 — 未知必需事件拒绝

场景: 未知必需事件拒绝重建
  假设 日志被注入一个非 ignorable 的未知 type 事件
  当 重建时
  那么 拒绝并报明确错误

Rule: REQ-FR-SESSION-8 — SessionHeader 字段

场景: SessionHeader 逻辑九字段与磁盘六必需键
  假设 新建一个会话
  当 写出 v4 文件时
  那么 首行头恰含六必需键 type/version/id/createdAt/isSeeded/delegationDepth，逻辑头九字段齐备且 origin 仅接受 'subagent'

场景: 携带 seedLength 的头被拒绝
  假设 一个 v4 头含未知字段 seedLength
  当 加载或重建时
  那么 拒绝并报明确错误

Rule: REQ-FR-SESSION-9 — 角色与 ContentBlock

场景: 五角色与七种 ContentBlock 往返无损
  假设 会话含 System/Developer/User/Assistant/ToolResult 五种角色与 text/reasoning/image/file/tool-call/tool-addition/tool-removal 七种 block
  当 写出并重载时
  那么 角色判别与 block 类型逐条还原

Rule: REQ-FR-SESSION-10 — system surface 路由

场景: in-history 增量追加
  假设 同一请求世代内提示词增量变化
  当 提交 surface 时
  那么 以 in-history 增量追加节点，不重写既有节点

场景: 世代更替走 first-node 归一化
  假设 请求世代更替
  当 提交 surface 时
  那么 system 节点按 first-node 归一化路由重写首节点

场景: 空渲染清空活跃尾部
  假设 system prompt 渲染结果为空
  当 提交 surface 时
  那么 活跃尾部 system 节点被清空且 head 节点以 replace 落为空文本节点

Rule: REQ-FR-SESSION-1 — 扩展事件与 end-seed 回环

场景: 扩展事件与 end-seed 参与回环
  假设 会话含 approval 扩展事件、tool/ptc 扩展事件与 session/end-seed
  当 写出并重载时
  那么 全部事件逐条还原

Rule: REQ-FR-SESSION-1 — 会话标题生成

场景: LLM 标题参数生效
  假设 配置了 LLM 且会话需要标题
  当 生成标题时
  那么 请求受 targetWords 5（CJK 10）、maxInput 4096 字节与 60s 超时约束

场景: 无 LLM 回退标题
  假设 无可用 LLM
  当 生成标题时
  那么 回退 5 词或 40 字节，上限 80 字节

Rule: REQ-FR-SESSION-11 — 编码与发布不变量

场景: 独占发布与回滚
  假设 新会话首发与一次 append 失败
  当 观察发布路径时
  那么 首发走临时文件加 link 独占发布且逐级目录 fsync，append 失败时文件 truncate 回滚无半行

场景: 文件名世代与头版本不一致报错
  假设 磁盘文件名为 v4 但头内 version 为 5
  当 加载时
  那么 报错拒绝

Rule: REQ-FR-SESSION-12 — surface 硬约束

场景: 非法 replace 被拒
  假设 replace 的 sourceEventSeqs 遗漏某个被遮蔽节点或 assistant 消息携带它
  当 提交时
  那么 校验失败拒绝落盘

场景: tool/result 替换越权
  假设 对某 tool/result 的 replace 同时改动 content 以外字段
  当 提交时
  那么 拒绝

Rule: REQ-FR-SESSION-13 — 两级补偿精确形态

场景: 两级补偿错误码与文案
  假设 一次 abort 同时产生调度器跳过与步级未启动调用
  当 恢复落盘时
  那么 前者为 ABORTED_BEFORE_DISPATCH 加 AbortError，后者为 TOOL_NOT_STARTED 或 TOOL_OUTCOME_UNKNOWN 精确文案，seq 连续且 time 复用最后真实事件

Rule: REQ-FR-SESSION-14 — 序列折叠编码

场景: 折叠编码回环
  假设 sourceEventSeqs 含长度 3 以上的连续段
  当 落盘并读回时
  那么 磁盘为折叠区间而内存还原为展开序列

Rule: REQ-FR-SESSION-1 — 事件全集枚举（枚举生成）

场景: 事件 turn/start 触发
  假设 会话运行中满足条件：开启 turn 时
  当 结算时
  那么 日志追加 turn/start 事件且字段与 dsh types.ts 逐字一致

场景: 事件 turn/start 表面归属
  假设 事件 turn/start 落盘
  当 重放分类时
  那么 按类型正确判定 入表面 或 log-only

场景: 事件 turn/start 回环
  假设 含 turn/start 的会话持久化
  当 写出并重载时
  那么 事件逐字节还原

场景: 事件 turn/end 触发
  假设 会话运行中满足条件：turn 结束时携带 reason 枚举
  当 结算时
  那么 日志追加 turn/end 事件且字段与 dsh types.ts 逐字一致

场景: 事件 turn/end 表面归属
  假设 事件 turn/end 落盘
  当 重放分类时
  那么 按类型正确判定 入表面 或 log-only

场景: 事件 turn/end 回环
  假设 含 turn/end 的会话持久化
  当 写出并重载时
  那么 事件逐字节还原

场景: 事件 step/start 触发
  假设 会话运行中满足条件：step 开启时
  当 结算时
  那么 日志追加 step/start 事件且字段与 dsh types.ts 逐字一致

场景: 事件 step/start 表面归属
  假设 事件 step/start 落盘
  当 重放分类时
  那么 按类型正确判定 入表面 或 log-only

场景: 事件 step/start 回环
  假设 含 step/start 的会话持久化
  当 写出并重载时
  那么 事件逐字节还原

场景: 事件 step/end 触发
  假设 会话运行中满足条件：step 收尾时
  当 结算时
  那么 日志追加 step/end 事件且字段与 dsh types.ts 逐字一致

场景: 事件 step/end 表面归属
  假设 事件 step/end 落盘
  当 重放分类时
  那么 按类型正确判定 入表面 或 log-only

场景: 事件 step/end 回环
  假设 含 step/end 的会话持久化
  当 写出并重载时
  那么 事件逐字节还原

场景: 事件 user/message 触发
  假设 会话运行中满足条件：用户输入提交时
  当 结算时
  那么 日志追加 user/message 事件且字段与 dsh types.ts 逐字一致

场景: 事件 user/message 表面归属
  假设 事件 user/message 落盘
  当 重放分类时
  那么 按类型正确判定 入表面 或 log-only

场景: 事件 user/message 回环
  假设 含 user/message 的会话持久化
  当 写出并重载时
  那么 事件逐字节还原

场景: 事件 developer/message 触发
  假设 会话运行中满足条件：工具增删声明时
  当 结算时
  那么 日志追加 developer/message 事件且字段与 dsh types.ts 逐字一致

场景: 事件 developer/message 表面归属
  假设 事件 developer/message 落盘
  当 重放分类时
  那么 按类型正确判定 入表面 或 log-only

场景: 事件 developer/message 回环
  假设 含 developer/message 的会话持久化
  当 写出并重载时
  那么 事件逐字节还原

场景: 事件 system/message 触发
  假设 会话运行中满足条件：系统提示词入表面时
  当 结算时
  那么 日志追加 system/message 事件且字段与 dsh types.ts 逐字一致

场景: 事件 system/message 表面归属
  假设 事件 system/message 落盘
  当 重放分类时
  那么 按类型正确判定 入表面 或 log-only

场景: 事件 system/message 回环
  假设 含 system/message 的会话持久化
  当 写出并重载时
  那么 事件逐字节还原

场景: 事件 assistant/message 触发
  假设 会话运行中满足条件：成功结算时内嵌完整 stream
  当 结算时
  那么 日志追加 assistant/message 事件且字段与 dsh types.ts 逐字一致

场景: 事件 assistant/message 表面归属
  假设 事件 assistant/message 落盘
  当 重放分类时
  那么 按类型正确判定 入表面 或 log-only

场景: 事件 assistant/message 回环
  假设 含 assistant/message 的会话持久化
  当 写出并重载时
  那么 事件逐字节还原

场景: 事件 assistant/attempt 触发
  假设 会话运行中满足条件：失败尝试时且不进模型历史
  当 结算时
  那么 日志追加 assistant/attempt 事件且字段与 dsh types.ts 逐字一致

场景: 事件 assistant/attempt 表面归属
  假设 事件 assistant/attempt 落盘
  当 重放分类时
  那么 按类型正确判定 入表面 或 log-only

场景: 事件 assistant/attempt 回环
  假设 含 assistant/attempt 的会话持久化
  当 写出并重载时
  那么 事件逐字节还原

场景: 事件 tool/call 触发
  假设 会话运行中满足条件：派发前追加且参数原始未解析
  当 结算时
  那么 日志追加 tool/call 事件且字段与 dsh types.ts 逐字一致

场景: 事件 tool/call 表面归属
  假设 事件 tool/call 落盘
  当 重放分类时
  那么 按类型正确判定 入表面 或 log-only

场景: 事件 tool/call 回环
  假设 含 tool/call 的会话持久化
  当 写出并重载时
  那么 事件逐字节还原

场景: 事件 tool/result 触发
  假设 会话运行中满足条件：finalize 后按模型序提交且 cite callSeq
  当 结算时
  那么 日志追加 tool/result 事件且字段与 dsh types.ts 逐字一致

场景: 事件 tool/result 表面归属
  假设 事件 tool/result 落盘
  当 重放分类时
  那么 按类型正确判定 入表面 或 log-only

场景: 事件 tool/result 回环
  假设 含 tool/result 的会话持久化
  当 写出并重载时
  那么 事件逐字节还原

场景: 事件 request/header 触发
  假设 会话运行中满足条件：世代判定满足时
  当 结算时
  那么 日志追加 request/header 事件且字段与 dsh types.ts 逐字一致

场景: 事件 request/header 表面归属
  假设 事件 request/header 落盘
  当 重放分类时
  那么 按类型正确判定 入表面 或 log-only

场景: 事件 request/header 回环
  假设 含 request/header 的会话持久化
  当 写出并重载时
  那么 事件逐字节还原

场景: 事件 request/context 触发
  假设 会话运行中满足条件：四字段变化时
  当 结算时
  那么 日志追加 request/context 事件且字段与 dsh types.ts 逐字一致

场景: 事件 request/context 表面归属
  假设 事件 request/context 落盘
  当 重放分类时
  那么 按类型正确判定 入表面 或 log-only

场景: 事件 request/context 回环
  假设 含 request/context 的会话持久化
  当 写出并重载时
  那么 事件逐字节还原

场景: 事件 session/end-seed 触发
  假设 会话运行中满足条件：继承标记时
  当 结算时
  那么 日志追加 session/end-seed 事件且字段与 dsh types.ts 逐字一致

场景: 事件 session/end-seed 表面归属
  假设 事件 session/end-seed 落盘
  当 重放分类时
  那么 按类型正确判定 入表面 或 log-only

场景: 事件 session/end-seed 回环
  假设 含 session/end-seed 的会话持久化
  当 写出并重载时
  那么 事件逐字节还原

场景: 事件 approval/policy 触发
  假设 会话运行中满足条件：策略切换时且 log-only
  当 结算时
  那么 日志追加 approval/policy 事件且字段与 dsh types.ts 逐字一致

场景: 事件 approval/policy 表面归属
  假设 事件 approval/policy 落盘
  当 重放分类时
  那么 按类型正确判定 入表面 或 log-only

场景: 事件 approval/policy 回环
  假设 含 approval/policy 的会话持久化
  当 写出并重载时
  那么 事件逐字节还原

场景: 事件 approval/asked 触发
  假设 会话运行中满足条件：ask 发起时与 decided 配对
  当 结算时
  那么 日志追加 approval/asked 事件且字段与 dsh types.ts 逐字一致

场景: 事件 approval/asked 表面归属
  假设 事件 approval/asked 落盘
  当 重放分类时
  那么 按类型正确判定 入表面 或 log-only

场景: 事件 approval/asked 回环
  假设 含 approval/asked 的会话持久化
  当 写出并重载时
  那么 事件逐字节还原

场景: 事件 approval/decided 触发
  假设 会话运行中满足条件：应答结算时
  当 结算时
  那么 日志追加 approval/decided 事件且字段与 dsh types.ts 逐字一致

场景: 事件 approval/decided 表面归属
  假设 事件 approval/decided 落盘
  当 重放分类时
  那么 按类型正确判定 入表面 或 log-only

场景: 事件 approval/decided 回环
  假设 含 approval/decided 的会话持久化
  当 写出并重载时
  那么 事件逐字节还原

场景: 事件 tool/ptc-dispatch-start 触发
  假设 会话运行中满足条件：子调用获得槽位时
  当 结算时
  那么 日志追加 tool/ptc-dispatch-start 事件且字段与 dsh types.ts 逐字一致

场景: 事件 tool/ptc-dispatch-start 表面归属
  假设 事件 tool/ptc-dispatch-start 落盘
  当 重放分类时
  那么 按类型正确判定 入表面 或 log-only

场景: 事件 tool/ptc-dispatch-start 回环
  假设 含 tool/ptc-dispatch-start 的会话持久化
  当 写出并重载时
  那么 事件逐字节还原

场景: 事件 tool/ptc-dispatch 触发
  假设 会话运行中满足条件：子调用结算时配对
  当 结算时
  那么 日志追加 tool/ptc-dispatch 事件且字段与 dsh types.ts 逐字一致

场景: 事件 tool/ptc-dispatch 表面归属
  假设 事件 tool/ptc-dispatch 落盘
  当 重放分类时
  那么 按类型正确判定 入表面 或 log-only

场景: 事件 tool/ptc-dispatch 回环
  假设 含 tool/ptc-dispatch 的会话持久化
  当 写出并重载时
  那么 事件逐字节还原

Rule: REQ-FR-SESSION-15 — 投影细则

场景: contentGeneration 变化触发整表重建
  假设 某表面节点的 contentGeneration 发生变化
  当 deriveMessages 重建时
  那么 从该节点起整表重建而非尾增量投影

场景: 表面尾增长走增量投影
  假设 仅在日志尾部追加了新表面节点
  当 deriveMessages 重建时
  那么 复用上次投影仅增量追加新节点

场景: 空 assistant 投影为 null
  假设 某 assistant 消息无内容块仅承载 max-tokens usage
  当 deriveMessages 投影时
  那么 该 assistant 投影为 null

Rule: REQ-FR-SESSION-16 — canonical header 细则

场景: canonicalHeader 空 tools 省略
  假设 世代内工具集合为空
  当 生成 canonicalHeader 时
  那么 tools 字段整体省略

场景: tool schema 比较顺序敏感
  假设 两个同名工具的 schema JSON 键序不同
  当 headerEquals 比较时
  那么 判定为不相等且 reason 升级为 change

场景: 工具增删 developer 声明门控
  假设 工具集合相对 baseline 的名字集合无差异
  当 结算新请求世代时
  那么 不追加 developer/message 且 headerSeq 不携带；有名字集合差且 baseline 存在时追加 developer/message，headerSeq 仅 additions>0 时携带

Rule: REQ-FR-SESSION-11 — 撕裂与世代发布

场景: zstd 撕裂尾恢复
  假设 session.v4.jsonl.zstd 尾部记录被截断
  当 加载时
  那么 丢弃不完整尾记录、完整记录全部还原且会话可继续追加

场景: 后继世代 EEXIST 逐字节比对
  假设 发布后继世代时目标文件名已存在
  当 link 返回已存在时
  那么 与已有文件逐字节比对：一致则采纳，不一致则报错拒绝

Rule: REQ-FR-SESSION-12 — node-0 保护

场景: node-0 重写白名单
  假设 对 node-0 的 replace 携带非 system/message 类型的单节点
  当 提交时
  那么 拒绝；仅 system/message 单节点重写被接受

Rule: REQ-FR-SESSION-17 — end-seed 切点

场景: 继承切点取最后一个 tagged end-seed
  假设 会话含多个 session/end-seed 事件
  当 计算继承切点时
  那么 切点为最后一个 tagged session/end-seed 所在 seq 且 isSeeded 头携带该继承事件数

Rule: REQ-FR-SESSION-18 — 激活期声明事件词表

场景: 激活期声明聚合为读路径已知集
  假设 fr-approval 在激活期声明 approval/asked、approval/decided、approval/policy
  当 fr-session 聚合词表并读取含这些类型的日志时
  那么 全部类型按已知事件正常重建，与内核静态类型并集一致

场景: 重复声明同类型装配失败
  假设 两个插件在各自激活期声明了同一持久事件类型名
  当 装配进行到第二个声明时
  那么 报装配错误终止启动（fail-closed），词表不含冲突类型

Rule: REQ-FR-SESSION-19 — plugin 记录通道

场景: plugin 记录写入读取回环
  假设 插件经 append-record 入口写入类型 plugin:example/state 的记录
  当 读取该日志重建会话时
  那么 记录以 ignorable:true 保留跳过、不进模型表面，读回 API 返回原样 data

场景: 非 plugin: 语法类型拒绝
  假设 append-record 收到类型名 example/state（无 plugin: 前缀）
  当 通道校验该类型名时
  那么 拒绝写入且日志不变（fail-closed）

Rule: REQ-FR-SESSION-20 — tools 投影

场景: series 重置基线弃 prefix reuse
  假设 会话先落 baseline header 与若干 tool-addition updates，随后落 reason 为 series 的 header 且某同名工具定义已变更
  当 tools 投影消费该 header 时
  那么 基线重置为该 header 的 tools、updates 清空、declared 表重建

场景: tool-addition 经 headerSeq 解析进 updates
  假设 developer/message 携带 tool-addition 且 headerSeq 指向既有 request/header
  当 tools 投影消费该事件时
  那么 additions 从该 header 解析定义并追加进 updates，快照为 {tools: 基线, updates}

Rule: REQ-FR-SESSION-21 — 写侧坐标正数

场景: system/message turn 0 写侧即拒
  假设 活会话将追加 data.turn 为 0 的 system/message
  当 append 提交该事件时
  那么 写侧拒绝（session event system/message turn must be positive），事件不入日志

场景: developer/message step 0 写侧即拒
  假设 活会话将追加 data.step 为 0 的 developer/message
  当 append 提交该事件时
  那么 写侧拒绝（session event developer/message step must be positive），事件不入日志

场景: 正坐标 step 消息照常接受
  假设 活会话将追加 turn 与 step 均为正的 system/message 与 developer/message
  当 append 依次提交这些事件时
  那么 写侧接受并照常入日志折叠

Rule: REQ-FR-SESSION-22 — 写侧保护头

场景: 非保护头上的 system/message 写侧即拒
  假设 活会话 surface 首节点为 user/message（保护头缺席）且 surface 非空
  当 append 提交 system/message 时
  那么 写侧拒绝（system/message requires a protected first surface head），事件不入日志

场景: 空 surface 首插 system/message 建保护头
  假设 活会话 surface 为空
  当 append 提交 system/message 时
  那么 写侧接受且该节点成为保护头

场景: 保护头经单节点重写转移
  假设 保护头在位且新 system/message 以 replace 恰好只覆盖保护头节点
  当 append 提交该替换时
  那么 写侧接受且保护头转移为新节点

## Dependencies

None.

## Source Trace

- doc/prd.md#7.1: SessionEvent 判别联合，字段与 dsh types.ts 逐字一致；durable 事件全集表
- dsh packages/core/session/src/known-event-types.ts:22-85: 64 类型词表目录（构建期生成）；REQ-FR-SESSION-18 激活期声明为其 fr 运行期等价（Q34A）
- dsh packages/core/session/src/tool-history.ts: tools 投影全实现（REQ-FR-SESSION-20 真源，Q36A）
- dsh packages/core/session/src/surface.ts: live fold 的保护头不对称即 REQ-FR-SESSION-22 偏差的 dsh 侧事实（2026-10-24 裁决 B）
- dsh core/session 校验层 assertV4SystemMessageFields / assertV4DeveloperData positivity 族: REQ-FR-SESSION-21 的镜像范围真源（2026-10-24 裁决 B）
- dsh packages/core/session/src/types.ts:202-233/310-331: turn/end 七变体与 user/message 直载（E1/E2 勘误证据）
- dsh packages/core/session/src/index.ts:492-512: appendPluginRecord/pluginRecordOf（REQ-FR-SESSION-19 真源，Q33A）
- doc/prd.md#7.2: 只实现 v4；世代命名，永不改名/删除已发布文件；读到非 v4 报明确错误
- doc/prd.md#7.3: surface 与 replace-op；旧内容永久留在日志
- doc/prd.md#7.4: deriveMessages 冻结请求必须可逐字节重现——对照测试锚点
- doc/prd.md#7.5: fork 与子代理会话不可被续用
- doc/prd.md#7.6: abort 补合成 tool/result
- confidence: high（PRD 第 7 节字段级规格）
- doc/dsh-source-notes.md: 源码级技术关键点（file:line 证据）

## Open Questions

None.
<!-- /agent-spec:requirement -->

<!-- agent-spec:requirement id=REQ-FR-AGENT-LOOP title="Agent 主循环插件" tags=agent-loop,core source=doc/prd.md -->
## Problem

fly-rafter 是字面全插件系统：agent 主循环不是宿主内置，而是 dougong 插件。宿主对循环零感知，循环与基础设施只经服务缝与事件解耦。

## Requirements

- [REQ-FR-AGENT-LOOP-1] fr-agent-loop MUST 是 dougong 插件：inject llm/tools/system-prompt/session-persistence，provide `agent` 服务（send/steer/inject/followup 入 inbox）；每会话一个循环 fiber，置于该会话 realm。
- [REQ-FR-AGENT-LOOP-2] 控制流 MUST 对齐 PRD 8.2 七步：输入入 inbox → turn()（append turn/start）→ preStep（inbox.claim → system-prompt.assemble → agent/pre-step waterfall → 拒绝或空输入直接收尾 turn）→ step（agent/request waterfall 提出 LlmCallConfig → llm.prepare_call → system prompt 提交 surface → append user/message → 按需 request/header、developer/message、request/context → deriveMessages 冻结请求 → 发起流）→ 流结算（error/aborted 时 append assistant/attempt + agent/request-error waterfall 决定重试或抛错；正常 append assistant/message；finish.kind == 'max-tokens' 粘滞结束 step）→ 工具执行（按 executionMode 分组）→ 收尾（step/end → agent/turn-stopping serial → turn/end；队列有 pending 开新 turn；llm.prepare_call 在无适配器时容忍中间件路由）。
- [REQ-FR-AGENT-LOOP-3] 每次请求 MUST 从会话日志 deriveMessages 投影并冻结，不经内存数组直传。
- [REQ-FR-AGENT-LOOP-4] step 抛错时 MUST 补齐缺失 tool/result（7.6 语义）；适配器级重试策略由 provider 插件经 provider_retry_policy 声明。
- [REQ-FR-AGENT-LOOP-5] 会话内热路径（agent/assistant-stream chunk）MUST 使用批量事件分发；分发开销基准测试 MUST 纳入 M1 验收。

- [REQ-FR-AGENT-LOOP-6] request/header 发布判定 MUST：首条 initial/resume；此后仅 canonical header 变化才 change（tool schema 用 JSON.stringify 顺序敏感比较）、仅序列边界才 series；工具增删 developer/message 仅 baseline 存在且名字集合有差，headerSeq 仅 additions 大于 0 时携带（dsh agent.ts:635-665、request-header.ts:21-52）。
- [REQ-FR-AGENT-LOOP-7] 缓存友好路由 MUST：capable 模型同世代提示词增量 in-history 追加；runtime-context 以 user 消息快照追加、完整当前值跟随保留历史之后（清空文案逐字 'Current runtime context: none. Earlier runtime-context snapshots no longer apply.'）；runtime-context 快照每个变更恰好 admitted 一次，重试 MUST NOT 重复注入；组装确定性保证 prompt 前缀逐字节稳定（dsh runtime-context.ts:14-16,96-119、agent.ts:425-440）。
- [REQ-FR-AGENT-LOOP-8] inbox 语义 MUST：claim 一次性取走全部 next-step、next-turn 至多 1 条；splice 规范化、message id 跨队列唯一、每次变更加 agent/inbox/spliced（dsh inbox.ts:109-114,198-243）。
- [REQ-FR-AGENT-LOOP-9] 长任务维持 MUST：每 turn 换新 AbortController 使旧信号闩失效；max-tokens 粘滞不得被 completed 降级；turn-stopping 仅 turnEnds 已设且 nextStep 空时派发；NO_ADAPTER 容忍中间件路由；缺 provider/model 配置 MUST 直接报错（dsh agent.ts:154-161,319-397,573-610）。
- [REQ-FR-AGENT-LOOP-10] attempt 结算 MUST 三分支：流 aborted 且中断内容非空白 → assistant/message 且 interrupted:true；error/aborted → assistant/attempt 并交 agent/request-error waterfall 裁决重试或抛错；正常 → assistant/message；user/message 仅 firstAttempt 落盘，重试 MUST NOT 重复注入（dsh agent.ts:425-441,464-545）。
- [REQ-FR-AGENT-LOOP-11] turn/step 状态机 MUST：相位 idle/maintenance/running；pre-step reject 置 turnEnds={blocked}；step0 无可领取输入 → turn reason completed 且不发起模型调用；turnEnds 已设后的空步静默收尾（dsh agent.ts:154-161,298-397）。
## Scenarios

Rule: REQ-FR-AGENT-LOOP-1 — 循环插件形态与 agent 服务

场景: agent 服务四入口入 inbox
  假设 会话循环运行中
  当 调用 agent 服务的 send、steer、inject 或 followup 时
  那么 输入进入 inbox 并唤醒 driver

场景: 每会话循环 fiber 置于会话 realm
  假设 新会话启动
  当 循环 fiber 创建时
  那么 fiber 置于该会话 realm，循环实现仅存在于 fr-agent-loop dylib 工件，宿主代码不含循环实现

Rule: REQ-FR-AGENT-LOOP-2 — 七步控制流

场景: 完整 turn 无工具调用
  假设 headless 任务提交后模型返回纯文本
  当 turn 结束时
  那么 会话日志含 turn/start、step/start、user/message、assistant/message、step/end、turn/end 且 reason 为 completed

场景: 含工具调用的 turn
  假设 模型请求调用 bash 工具
  当 工具结果回填后继续下一 step
  那么 会话日志含模型序的 tool/call 与 tool/result，且无更多 tool-call 时 turn 为 completed

场景: max-tokens 粘滞结束
  假设 流结算 finish kind 为 max-tokens
  当 结算完成时
  那么 step 粘滞结束且 turn reason 为 max-tokens

场景: steer 输入排队开新 turn
  当 turn 进行中调用 steer 时
  那么 输入入 inbox，当前 turn 正常收尾后开启下一 turn

Rule: REQ-FR-AGENT-LOOP-3 — 请求从日志冻结投影

场景: 冻结请求拒绝内存改动
  假设 deriveMessages 已产出冻结请求
  当 尝试在内存中修改请求内容时
  那么 修改被拒绝，请求仍与日志投影一致

Rule: REQ-FR-AGENT-LOOP-4 — 失败恢复

场景: step 抛错补齐缺失 tool/result
  假设 step 抛错时存在未结算的工具调用
  当 失败恢复运行时
  那么 缺失的 tool/result 被补齐且重放合法

场景: provider 重试策略生效
  假设 provider 插件声明了 provider_retry_policy
  当 流错误发生时
  那么 按策略重试，超限后向循环抛错

Rule: REQ-FR-AGENT-LOOP-5 — 热路径批量分发

场景: assistant-stream 批量分发基准
  假设 会话内 assistant-stream chunk 高频到达
  当 运行 M1 验收的分发开销基准时
  那么 chunk 经批量分发送达监听者且基准达标

Rule: REQ-FR-AGENT-LOOP-2 — preStep 与收尾分支

场景: pre-step 拒绝或空输入收尾 turn
  假设 agent/pre-step waterfall 拒绝输入或 inbox 无可领取输入
  当 preStep 结算时
  那么 不发起请求，turn 直接收尾并 append turn/end

场景: turn 收尾前派发 turn-stopping
  假设 turn 即将正常结束
  当 收尾时
  那么 先以 serial 语义派发 agent/turn-stopping，再 append turn/end

Rule: REQ-FR-AGENT-LOOP-3 — 无适配器容忍

场景: 无适配器时中间件路由容忍
  假设 agent/request waterfall 提出的配置无绑定适配器
  当 调用 prepare_call 时
  那么 容忍中间件路由不报错，由后续件决定行为

Rule: REQ-FR-AGENT-LOOP-2 — inbox claim 顺序

场景: 多输入按入队顺序 claim
  假设 inbox 中排队多个输入
  当 preStep claim 时
  那么 按入队顺序消费

场景: incapable 模型走归一化路由
  假设 当前模型不支持提示词缓存增量
  当 提交 system prompt surface 时
  那么 走归一化路由整段重写首节点

Rule: REQ-FR-AGENT-LOOP-6 — header 发布判定

场景: header reason 判定
  假设 会话经历新建、工具集变更、仅序列推进三种情形
  当 发布 request/header 时
  那么 reason 分别为 initial、change、series；工具 schema 仅顺序不同的相等比较按顺序敏感判定

Rule: REQ-FR-AGENT-LOOP-7 — 缓存友好路由

场景: 策略变化不打破前缀
  假设 会话中途审批策略变化
  当 构造下一请求时
  那么 历史前缀逐字节不变，新 runtime-context 快照追加在保留历史之后

Rule: REQ-FR-AGENT-LOOP-8 — inbox 语义

场景: claim 队列语义
  假设 inbox 中有两条 next-step 与一条 next-turn 输入
  当 preStep claim 时
  那么 next-step 全部取走、next-turn 至多取一条，且变更产生 inbox/spliced 事件

Rule: REQ-FR-AGENT-LOOP-9 — 跨 turn 信号隔离

场景: 新 turn 旧信号失效
  假设 上一 turn 的 AbortController 已触发
  当 开启新 turn 时
  那么 换新 controller，旧信号上的闩不再影响新 turn

Rule: REQ-FR-AGENT-LOOP-10 — attempt 结算分支

场景: 中断保留部分内容
  假设 流 aborted 时已积累非空白文本与 reasoning 内容
  当 结算该 attempt 时
  那么 追加 assistant/message 且 interrupted:true，中断内容进入历史

场景: 重试不重复 user/message
  假设 首次 attempt 失败进入重试
  当 第二次 attempt 发起请求时
  那么 user/message 仅存在 firstAttempt 的一条，未重复注入

场景: 错误 attempt 走 waterfall 裁决
  假设 流以 error 结束
  当 结算时
  那么 追加 assistant/attempt 并交 agent/request-error waterfall 裁决，默认不重试则抛错

Rule: REQ-FR-AGENT-LOOP-11 — 状态机精确行为

场景: step0 空输入不花模型调用
  假设 turn 开启后 preStep 无可领取输入
  当 step0 结算时
  那么 turn reason 为 completed 且未发起任何模型请求

场景: pre-step reject 置 blocked
  假设 agent/pre-step waterfall 以 reject 裁决
  当 结算时
  那么 turnEnds 置为 blocked 并收尾 turn

场景: 维护相位不接受新步
  假设 循环处于 maintenance 相位
  当 请求发起新 step 时
  那么 该请求不被执行，循环仅在恢复 running 后继续

Rule: REQ-FR-AGENT-LOOP-7 — runtime-context 准入一次

场景: runtime-context 恰好 admitted 一次
  假设 某步因流错误重试
  当 重试请求组装时
  那么 本步已注入的 runtime-context 快照不重复出现

## Dependencies

- REQ-FR-SESSION
- REQ-FR-LLM
- REQ-FR-TOOLS

## Source Trace

- doc/prd.md#8.1: agent 主循环本身是插件；provide agent 服务（send/steer/inject/followup 入 inbox）
- doc/prd.md#8.2: 控制流与 dsh agent-loop/src/agent.ts 逐步对齐（七步）
- doc/prd.md#8.3: 失败恢复；provider_retry_policy
- confidence: high（PRD 第 8 节流程级规格）
- doc/dsh-source-notes.md: 源码级技术关键点（file:line 证据）

## Open Questions

None.
<!-- /agent-spec:requirement -->

<!-- agent-spec:requirement id=REQ-FR-TOOLS title="工具系统与调度" tags=tools,pipeline source=doc/prd.md -->
## Problem

工具调用是 agent 执行能力的核心。fly-rafter 需要与 dsh `tools.execute` 逐项对齐的注册、schema、管线与调度语义。

## Requirements

- [REQ-FR-TOOLS-1] ToolRegistry MUST 支持 register（名称、JSON Schema、executionMode（exclusive/parallel）、presentation 元数据、说明 section order 1000-3000）；MCP 工具以 raw JSON Schema 动态注册。
- [REQ-FR-TOOLS-2] 执行管线 MUST 按序：createExecution（参数 JSON 快照/冻结）→ tools/pre-execute waterfall（allow / deny / {kind:'ask'} 交 ApprovalService）→ guards（单调串，防重复调用风暴）→ tools/execute waterfall → 工具体 → tools/post-execute → append tool/call（模型序）+ tool/result（模型序提交）；guards 仅 pre-execute allow 时求值、跨 waterfall 单调不放宽（无 allow 回退）；guard 拒绝的调用仍 MUST 过 tools/post-execute；post-execute block → isError 结果附 feedback 文本、value/content 双替换抛 TypeError；finalize 冻结执行记录后才提交 tool/result（dsh tools/src/index.ts:1153-1162,724-732,1501-1547,1789-1828,1677-1722）。
- [REQ-FR-TOOLS-3] 调度 MUST：exclusive 调用为单调用屏障（组=该调用本身）；parallel 有界滚动池上限=agentLoop.config.maxParallelToolCalls 默认 10；批次启动前 lazy 重分类（注册表变更可产生新屏障，并行组在首个非 parallel 调用处断开）；concludeTurn 仅成功结果终结 turn、错误结果 turn 继续；abort 为未开始调用补合成错误结果（dsh agent-loop/src/tool-calls.ts:85-105,200-213、tools/src/index.ts:593）。
- [REQ-FR-TOOLS-4] schema 类型推断与呈现元模型 MUST 对齐 dsh schema.ts / presentation.ts。

- [REQ-FR-TOOLS-5] schema 子集白名单 MUST（type/oneOf/properties/required/additionalProperties/items/enum/const 加注释词，其余键拒绝）；object 类的 additionalProperties MUST 显式 boolean（缺省或非布尔拒绝）；推断规则：const 优先于 enum 优先于宽类型、integer 归 number、oneOf 并集、深度 16 上限回退 JsonValue；isConcurrencySafe 仅严格 true 才 parallel（否则 fail-closed exclusive）（dsh schema.ts:97-166,367-369、json-schema.ts:6-10）。
## Scenarios

Rule: REQ-FR-TOOLS-1 — 注册与 schema

场景: 工具注册元数据齐备
  当 注册任一内置工具时
  那么 名称、JSON Schema、executionMode、presentation 元数据与说明 section order 全部登记

Rule: REQ-FR-TOOLS-2 — 执行管线

场景: 管线事件顺序
  当 任一工具执行时
  那么 顺序为 createExecution、pre-execute、guards、execute、post-execute、tool/call 加 tool/result

场景: pre-execute deny 短路执行
  假设 tools/pre-execute waterfall 返回 deny
  当 该调用进入管线时
  那么 不执行工具体，直接产出错误 tool/result

Rule: REQ-FR-TOOLS-3 — 调度

场景: exclusive 串行屏障
  假设 模型一次请求多个 exclusive 工具调用
  当 执行时
  那么 按模型序逐个执行，无并发重叠

场景: parallel 有界滚动池
  假设 模型一次请求多个 parallel 工具调用
  当 执行时
  那么 并发数不超过有界池上限，全部完成后才结算 step

场景: concludeTurn 调用终结 turn
  假设 模型请求 concludeTurn 类调用
  当 其 tool/result 提交后
  那么 turn 终结且不再开启下一 step

Rule: REQ-FR-TOOLS-4 — schema 推断对齐

场景: schema 类型推断与 dsh 一致
  当 对相同参数对象运行类型推断时
  那么 推断产出与 dsh schema.ts 的结果一致

Rule: REQ-FR-TOOLS-2 — guards 防重复风暴

场景: 重复调用达阈值触发守卫
  假设 同一工具被重复调用达到提醒阈值
  当 守卫链结算时
  那么 按阈值档位注入提醒（阈值取自默认 [3, 5, 8]）

Rule: REQ-FR-TOOLS-3 — 混合调度组屏障

场景: exclusive 与 parallel 混合分组屏障
  假设 模型一次请求依次含 parallel 组、exclusive 组、parallel 组
  当 分组调度时
  那么 组间按模型序串行推进，exclusive 组内串行、parallel 组内有界并发

Rule: REQ-FR-TOOLS-1 — presentation 元模型

场景: presentation 推断与 dsh 一致
  当 对相同工具运行呈现元数据推断时
  那么 结果与 dsh presentation.ts 一致

Rule: REQ-FR-TOOLS-2 — 参数快照冻结

场景: 执行中参数快照不可变
  假设 createExecution 已冻结参数快照
  当 执行期间原始参数对象被修改时
  那么 工具体看到的仍为快照值

Rule: REQ-FR-TOOLS-5 — schema 白名单与推断

场景: 超集键拒绝与推断回退
  假设 工具 schema 含白名单外键、另一工具含 16 层以上嵌套
  当 注册时
  那么 超集键注册失败，深嵌套推断回退 JsonValue

场景: additionalProperties 非显式布尔拒绝
  假设 object 类 schema 的 additionalProperties 缺省或为非布尔值
  当 注册时
  那么 拒绝；仅显式 true/false 被接受

Rule: REQ-FR-TOOLS-2 — guards 求值与 post-execute 结算

场景: guards 仅 allow 时求值且单调
  假设 pre-execute 裁决为 deny 或 ask
  当 该调用进入守卫阶段时
  那么 guards 不求值；allow 路径上守卫状态跨 waterfall 单调，不允许已触发档位回退

场景: guard 拒绝仍过 post-execute
  假设 guards 拒绝了某调用
  当 结算管线时
  那么 该调用仍经过 tools/post-execute 后产出拒绝结果

场景: post-execute block 双替换抛 TypeError
  假设 tools/post-execute 裁决同时替换 value 与 content
  当 结算时
  那么 抛 TypeError；单字段 block 替换产生 isError 结果并附 feedback 文本

Rule: REQ-FR-TOOLS-3 — 池上限与惰性重分类

场景: parallel 池上限默认 10
  假设 一批 parallel 调用超过 10 个且配置未覆盖 maxParallelToolCalls
  当 调度时
  那么 同时在飞不超过 10，完成即递补

场景: 注册表变更触发惰性重分类
  假设 并行组执行中某未启动调用的 executionMode 被注册表更新为 exclusive
  当 填池扫描到该调用时
  那么 并行组在此处断开，该调用按新屏障语义执行

场景: concludeTurn 错误结果不终结 turn
  假设 concludeTurn 类调用产出错误结果
  当 其 tool/result 提交后
  那么 turn 不终结且继续下一 step；仅成功结果终结 turn

## Dependencies

- REQ-FR-SESSION
- REQ-FR-CONFIG

## Source Trace

- doc/prd.md#9.1: 注册与 schema；MCP 工具 raw JSON Schema 动态注册
- doc/prd.md#9.2: 执行管线伪码
- doc/prd.md#9.3: 调度（exclusive/parallel、concludeTurn、abort 补偿）
- confidence: high（PRD 第 9 节）
- doc/dsh-source-notes.md: 源码级技术关键点（file:line 证据）

## Open Questions

None.
<!-- /agent-spec:requirement -->

<!-- agent-spec:requirement id=REQ-FR-PTC title="run_code / PTC" tags=ptc,tools source=doc/prd.md -->
## Problem

run_code 是程序化工具调用（PTC）入口：TypeScript async 函数体（仅可擦除语法），由全新 Node 子进程以原生 strip-only 模式执行，全链路 1:1 对齐 dsh PTC 运行时。

## Requirements

- [REQ-FR-PTC-1] fr-ptc MUST 以全新 Node 子进程执行 TypeScript（strip-only 类型擦除：程序包成 async function __dsh_program__ 后经 stripTypeScriptTypes，再以 AsyncFunction('use strict') 求值）；会话状态不可读；文件系统访问 MUST 按沙箱策略执行（非禁止）；进程在沙箱策略执行世界中启动；沙箱 seam 不表达网络限制（如实承袭 dsh 事实）；语言 flavor 仅 TypeScript（对齐 TYPESCRIPT_FUNCTION_FLAVOR 文本）（dsh ptc-runtime-node/src/index.ts:30,48,200-203、bootstrap.ts:404-411）。
- [REQ-FR-PTC-2] wire 帧协议 MUST 对齐 dsh channel.ts/json-wire.ts/protocol.ts 的帧上限、有界写队列、流式 stdout、崩溃分类；实现于 fr-ptc（tokio + 子进程）。
- [REQ-FR-PTC-3] 失败分类全集 `exception | timeout | abort | worker-exit | invalid-output | output-limit | protocol | sandbox-unavailable`，全部 fail-closed；触发条件 MUST 精确：exception=strip 抛或程序抛；timeout=wall 计时器；abort=取消信号且非超时；worker-exit=先行退出/输出流失败/清理失败；invalid-output=最终值非 lossless JSON；output-limit=LogBuffer 预算/admit 溢出/ledger 超限重写；protocol=非法帧/坏 log/坏 call id（须严格递增）/未声明绑定/pending 双上限（128 个与 128MiB）；sandbox-unavailable=SandboxUnavailableError 或 runner spawn 规则（dsh ptc-runtime-node/src/index.ts:200-339、bootstrap.ts:47-96,176-182、output-ledger.ts:24-65）。
- [REQ-FR-PTC-4] 能力面：工具以全局 async 函数注入（`await tools.name(args)`），参数/返回值必须 lossless JSON；`console` 为保留 log 槽，输出计入共享字节预算，超限报 output-limit；`timeout-ms` 预算超时归类 'timeout'。
- [REQ-FR-PTC-5] 绑定名校验：可移植标识符，拒绝保留字与 `console` 槽；dunder 仅在 error 成员位拒绝（RESERVED_ERROR_MEMBERS），绑定 global 不拒 dunder；`__proto__` MUST NOT 拒绝——按普通 own key 注入（宿主 null-prototype 对象 defineProperty，child 同）（dsh bindings.ts:7-53、tools/src/ptc.ts:686-694、ptc-runtime-node/src/bootstrap.ts:313-358）。
- [REQ-FR-PTC-6] 声明生成：从当前注册工具 schema 生成 TS 绑定声明文本，经系统提示词 TOOLS_SDK section（order 5000）下发模型；工具按 name 字典序、键非裸标识符 JSON 引号、JSDoc 折叠转义 `*/`、additionalProperties!==false 追加 Record 重载、敌意 schema 降级 unknown；run_code 为 registry 保留传输（mode: ptc），不参与可过滤能力层（dsh tools/src/ts-types.ts:18-38,169-171,240-247）。
- [REQ-FR-PTC-7] 子调度：回流完整管线（含审批升级 approveEscalation），append `tool/ptc-dispatch-start` 与 `tool/ptc-dispatch`（parentCallId/subCallId 配对）；`max-parallel-sub-calls` 默认 10、submission-ordered；沙箱策略切换中止在跑程序。
- [REQ-FR-PTC-8] `execution-mode: persistent-in-memory`（opt-in）：每 state owner 一个受限持久 REPL 进程、单并发、换沙箱策略 MUST 先 release 旧进程再新建（dsh ptc-runtime-node/src/persistent.ts:69,125,334-356）。
- [REQ-FR-PTC-9] node 缺失时该工具调用 fail-closed 报错，系统其余部分零影响；Python 后端不在范围。

- [REQ-FR-PTC-10] 子调用协议 MUST：子调用 id 形如 parent 加 ptc 加序号且严格递增；pending 双上限（128 个与 128MiB）；dispatch-start 于真正获得槽位时追加；dispatch-log waterfall 听众抛错 contained 回退不传播；logWork 超过子并发上限应用背压排队；abort 时排队未启动者 abandon 不记日志（dsh ptc.ts:447-532,613-620,662、tools/src/index.ts:1331-1341）。
- [REQ-FR-PTC-11] 审批升级 MUST：permissions 与 justification 成对非空；同模式免审、升级须严格更宽（目标仅 workspace-write 与 danger-full-access）；rejected、cancelled、unavailable 一律抛错（dsh escalation.ts:41-63,165-217）。
- [REQ-FR-PTC-12] mode:ptc 下模型直接工具调用（非经 run_code 程序）MUST 在策略管线前坍缩终结：UNKNOWN_TOOL 加路由文本，绕过 pre-execute 与 guards（dsh tools/src/index.ts:1461-1482）。
- [REQ-FR-PTC-13] PTC 运行时默认值 MUST：execution-mode 'per-call'、timeoutMs 120000、maxTimeoutMs 600000、maxOutputBytes 67108864（64MiB）、maxOldGenerationSizeMb 512、maxMessageBytes 134217728（128MiB）、maxPendingCalls 128、graceMs 3000（dsh ptc-runtime-node/src/config.ts:31-44）。
## Scenarios

Rule: REQ-FR-PTC-1 — node 子进程 strip-only 执行

场景: run_code happy path
  假设 模型提交合法 TS 程序调用两个工具
  当 子进程执行完成时
  那么 返回结果为 lossless JSON，日志含 tool/ptc-dispatch-start 与 tool/ptc-dispatch

场景: 不可擦除语法 fail-closed 拒绝
  假设 程序含 strip-only 模式不可擦除的语法
  当 子进程装载程序时
  那么 fail-closed 报错且不执行程序

Rule: REQ-FR-PTC-2 — wire 帧协议

场景: 超帧上限归类 protocol
  假设 一帧超过协议帧上限
  当 帧解析时
  那么 该调用以 protocol 失败分类结算

场景: 大输出触发有界写队列背压
  假设 子进程 stdout 大量输出
  当 写队列达到上限时
  那么 应用背压，不无界缓冲

Rule: REQ-FR-PTC-3 — 失败分类全集

场景: 程序超时归类 timeout
  假设 程序超过 timeout-ms 预算
  当 预算耗尽时
  那么 失败分类为 timeout

场景: 子进程异常退出归类 worker-exit
  假设 子进程非正常退出
  当 结算时
  那么 失败分类为 worker-exit

Rule: REQ-FR-PTC-4 — 能力面与预算

场景: console 输出超预算归类 output-limit
  假设 程序 console 输出超过共享字节预算
  当 结算时
  那么 失败分类为 output-limit

场景: 非 lossless JSON 值被拒
  假设 工具参数或返回值不可 lossless JSON 化
  当 程序调用该工具时
  那么 该子调用报错

Rule: REQ-FR-PTC-5 — 绑定名校验

场景: 保留名绑定被拒
  假设 程序声明绑定名 console、__proto__、dunder 或保留字
  当 绑定名校验时
  那么 拒绝该程序

Rule: REQ-FR-PTC-6 — 声明生成

场景: TOOLS_SDK 声明覆盖全部工具
  假设 registry 已注册全部内置工具
  当 生成 TS 绑定声明文本时
  那么 声明覆盖全部注册工具 schema 且经 order 5000 的 TOOLS_SDK section 下发

Rule: REQ-FR-PTC-7 — 子调度

场景: 子调度回流完整管线并触发审批
  假设 程序内子调用命中需审批操作
  当 子调度回流管线时
  那么 走同一审批 seam，未应答则 fail-closed

场景: 子调用并发上限 submission-ordered
  假设 程序发起 11 个子调用且上限为 10
  当 并发提交时
  那么 第 11 个排队，完成一个放行一个

场景: 沙箱策略切换中止在跑程序
  假设 程序运行中权限模式切换
  当 切换结算时
  那么 在跑程序被中止

Rule: REQ-FR-PTC-8 — 持久 REPL

场景: persistent-in-memory 状态跨调用保留
  假设 execution-mode 为 persistent-in-memory 且同 state owner 连续两次调用
  当 第二次调用访问第一次的变量时
  那么 状态保留可见

Rule: REQ-FR-PTC-9 — node 缺失 fail-closed

场景: node 缺失仅该工具报错
  假设 运行环境无 node
  当 调用 run_code 时
  那么 该调用报明确错误，系统其余部分零影响

Rule: REQ-FR-PTC-6 — run_code 保留传输

场景: run_code 不参与可过滤能力层
  假设 配置了工具过滤能力
  当 应用过滤时
  那么 run_code 仍注册（mode 为 ptc 的保留传输），不可被过滤移除

Rule: REQ-FR-PTC-2 — 流式 stdout

场景: 程序 stdout 流式回传
  假设 程序持续输出 stdout
  当 程序未结束时
  那么 输出已流式回传宿主，不等待进程结束

Rule: REQ-FR-PTC-10 — 子调用协议

场景: 子调用 id 与 pending 上限
  假设 程序并发提交超过 pending 双上限
  当 host 结算时
  那么 超限以 protocol 失败，id 非严格递增亦然

Rule: REQ-FR-PTC-11 — 审批升级

场景: 升级路径判定
  假设 程序分别请求同模式、更宽模式与非目标模式升级
  当 approveEscalation 结算时
  那么 同模式免审、合法宽模式走审批、非法目标直接抛错

Rule: REQ-FR-PTC-3 — 失败分类精确触发（枚举生成）

场景: 失败分类 exception 归类
  假设 程序抛出异常或 strip 阶段抛出
  当 结算时
  那么 失败分类精确为 exception 且 fail-closed 返回

场景: 失败分类 timeout 归类
  假设 程序运行越过 wall 计时器预算
  当 结算时
  那么 失败分类精确为 timeout 且 fail-closed 返回

场景: 失败分类 abort 归类
  假设 收到取消信号且并非超时路径
  当 结算时
  那么 失败分类精确为 abort 且 fail-closed 返回

场景: 失败分类 worker-exit 归类
  假设 子进程先行退出、或输出流失败、或清理阶段失败
  当 结算时
  那么 失败分类精确为 worker-exit 且 fail-closed 返回

场景: 失败分类 invalid-output 归类
  假设 程序最终值非 lossless JSON
  当 结算时
  那么 失败分类精确为 invalid-output 且 fail-closed 返回

场景: 失败分类 output-limit 归类
  假设 LogBuffer 预算耗尽、或 admit 溢出、或 ledger 超限重写
  当 结算时
  那么 失败分类精确为 output-limit 且 fail-closed 返回

场景: 失败分类 protocol 归类
  假设 出现非法帧、坏 log、非严格递增 call id、未声明绑定或 pending 双上限
  当 结算时
  那么 失败分类精确为 protocol 且 fail-closed 返回

场景: 失败分类 sandbox-unavailable 归类
  假设 SandboxUnavailableError 或 runner spawn 规则命中
  当 结算时
  那么 失败分类精确为 sandbox-unavailable 且 fail-closed 返回

Rule: REQ-FR-PTC-5 — 绑定名与注入细则

场景: __proto__ 绑定按 own key 注入
  假设 程序声明名为 __proto__ 的绑定
  当 宿主构建绑定对象时
  那么 不拒绝，经 null-prototype 对象 defineProperty 以普通 own key 注入

场景: error 成员位 dunder 拒绝
  假设 程序在 error 成员位声明 dunder 名绑定
  当 绑定名校验时
  那么 以 RESERVED_ERROR_MEMBERS 拒绝；同名出现在绑定 global 位则放行

Rule: REQ-FR-PTC-1 — 执行形态

场景: 程序包装与严格求值
  假设 模型提交合法 TS 程序
  当 装载执行时
  那么 程序包成 async function __dsh_program__ 经 stripTypeScriptTypes 后以 AsyncFunction('use strict') 求值

场景: 程序 fs 访问按沙箱策略
  假设 程序内子调用触发文件系统访问
  当 沙箱策略结算该访问时
  那么 按策略允许或拒绝，不因 PTC 路径一律禁止

Rule: REQ-FR-PTC-13 — 运行时默认值

场景: PTC 默认值全表
  假设 部署未覆盖 ptc-runtime-node 配置
  当 读取生效配置时
  那么 execution-mode 'per-call'、timeoutMs 120000、maxTimeoutMs 600000、maxOutputBytes 67108864、maxOldGenerationSizeMb 512、maxMessageBytes 134217728、maxPendingCalls 128、graceMs 3000

Rule: REQ-FR-PTC-6 — 声明生成细则

场景: 声明文本规范化
  假设 registry 含名称字典序交错、键含非裸标识符、JSDoc 含闭合序列的工具
  当 生成 TS 声明时
  那么 工具按 name 字典序、非裸键 JSON 引号、JSDoc 转义闭合序列、additionalProperties!==false 追加 Record 重载、敌意 schema 降级 unknown

Rule: REQ-FR-PTC-8 — 释放与单并发

场景: 换策略须先 release
  假设 persistent-in-memory 进程存活且请求切换沙箱策略
  当 策略切换结算时
  那么 先 release 旧进程再新建；进程存活期间同 owner 第二并发请求被拒

Rule: REQ-FR-PTC-10 — dispatch-log 与背压

场景: dispatch-log 听众抛错 contained
  假设 dispatch-log waterfall 某听众抛错
  当 结算时
  那么 错误 contained 回退，不传播给程序或宿主

Rule: REQ-FR-PTC-12 — 坍缩调用

场景: model-direct 调用在管线前坍缩
  假设 mode:ptc 下模型直接提交 run_code 以外的工具调用
  当 结算时
  那么 在策略管线前以 UNKNOWN_TOOL 加路由文本终结，绕过 pre-execute 与 guards

## Dependencies

- REQ-FR-TOOLS
- REQ-FR-APPROVAL
- REQ-FR-SANDBOX

## Source Trace

- doc/prd.md#9.4: run_code/PTC 完整规格（wire、能力面、失败分类、子调度、persistent-in-memory、node 缺失）
- confidence: high（PRD 9.4 字段级规格）
- doc/dsh-source-notes.md: 源码级技术关键点（file:line 证据）

## Open Questions

None.
<!-- /agent-spec:requirement -->

<!-- agent-spec:requirement id=REQ-FR-PROMPT title="系统提示词组装" tags=prompt,system-prompt source=doc/prd.md -->
## Problem

提示词文本是 1:1 对齐的核心面。fly-rafter 需要有序 section/context 注册、{{var}} 插值与逐字文本保真；文本必须脚本抽取生成、禁止手抄。

## Requirements

- [REQ-FR-PROMPT-1] fr-system-prompt MUST 提供有序 PromptSection / Context 注册 + `{{var}}` 插值 + `tool-order`（对齐 dsh toolOrder）。
- [REQ-FR-PROMPT-2] SECTION_ORDERS MUST 以 dsh system-prompt/src/index.ts:131-165 为权威、CONTEXT_ORDERS 以 :170-179 为权威（勘误注记：CONTEXT_ORDERS 曾误记 :149，行号实为 :170、数值不变）；HARNESS_IDENTITY: -1000、TOOLS_SDK: 5000、STRUCTURED_OUTPUT: 9900；WORKING_DIRECTORY: 100、SANDBOX_POLICY: 110、APPROVAL_POLICY: 115。
- [REQ-FR-PROMPT-3] 身份句 MUST 为 `You are an AI agent powered by Fly Rafter.`；`include-harness-identity` 默认 true；`persona-prefix` 默认 `''`；除身份句外全部 section 文本逐字取自 dsh。
- [REQ-FR-PROMPT-4] AGENTS.md 注入 MUST 非 system section：渲染为 user 消息包裹 system-reminder 帧（首行 "Instructions from: <path>"）；变更/删除以 Updated/removed 语义再注入；fs 工具触碰嵌套指令文件触发再投影；候选 AGENTS.md/CLAUDE.md 加 local 变体、projectRoot 自 cwd 向上找 .git 标记、maxSourceBytes 1MiB 超限跳过、maxBytes 二分截断恰好适配、同组去重；各工具说明 section 由工具插件注册（order 1000-3000）（dsh context/agent-instructions/src/render.ts:255-272,315-321、config.ts,files.ts、index.ts:4-5）。
- [REQ-FR-PROMPT-5] 提示词常量 MUST 由脚本从 dsh 源文件抽取生成，禁止手抄。

- [REQ-FR-PROMPT-6] 组装确定性 MUST：同 order 按 name code-unit 序、重名抛错、scoped 遮蔽 global；complete section 至多一个、waterfall 后恢复唯一 prompt section、refreshContext 只重跑 context 节；toolOrder 保留标记 unlisted-tools 恰好出现一次、未列工具字典序插入 rest 位；变量名限小写首字母加数字下划线、畸形或未注册抛错并指明 section、interpolate:false 保留字面（dsh system-prompt/src/index.ts:196-239,248-250,290-377,582-602,653-656）。
- [REQ-FR-PROMPT-7] SECTION_ORDERS 全表 32 项数值 MUST 逐字为：HARNESS_IDENTITY -1000、DEPLOYMENT_PERSONA_PREFIX 0、PLAN_POLICY 500、TEAM_POLICY 600、PTC_ONLY 800、FILE_REFERENCE 900、TOOL_BASH 1000、TOOL_PWSH 1010、TOOL_READ 1100、TOOL_WRITE 1200、TOOL_EDIT 1300、TOOL_GLOB 1400、TOOL_GREP 1500、TOOL_JOBS 1600、TOOL_PTY 1700、TOOL_WEB_SEARCH 2000、TOOL_WEB_FETCH 2100、TOOL_LSP 2200、TOOL_SESSION_QUERY 2300、TOOL_GOAL 2400、TOOL_WORKFLOW 2600、TOOL_RALPH 2700、TOOL_SUBAGENT 2800、TOOL_REPORT 2900、TOOL_COMPUTER_USE 3000、MCP_SERVERS 3100、TOOLS_SDK 5000、DELIVERABLE_FILE_REFERENCES 9000、STRUCTURED_OUTPUT 9900、HARNESS_SOURCE 10000、WEB_SURFACE 10100、DEPLOYMENT_PERSONA_SUFFIX 10200；CONTEXT_ORDERS 7 项：WORKING_DIRECTORY 100、SANDBOX_POLICY 110、APPROVAL_POLICY 115、SUBAGENT_DELEGATION 120、TEAM_IDENTITY 125、HARNESS_SOURCE 200、WEB_SURFACE 210（dsh system-prompt/src/index.ts:131-165,170-179）。
## Scenarios

Rule: REQ-FR-PROMPT-1 — section 注册与插值

场景: 变量插值与 tool-order 重排
  假设 section 文本含占位变量且注册工具集变化
  当 组装提示词时
  那么 占位变量替换为运行时值，工具说明 section 按当前工具集与 tool-order 重排

Rule: REQ-FR-PROMPT-2 — orders 权威

场景: section 全序与 dsh 源一致
  当 组装默认装配提示词时
  那么 section 顺序符合 SECTION_ORDERS 与 CONTEXT_ORDERS（HARNESS_IDENTITY 最前、TOOLS_SDK 位于 5000），全集与 dsh 源文件一致

Rule: REQ-FR-PROMPT-3 — 身份句与开关

场景: 身份句默认渲染
  假设 include-harness-identity 保持默认 true
  当 组装提示词时
  那么 section 序列含 You are an AI agent powered by Fly Rafter.

场景: 身份句可关闭与前缀
  假设 include-harness-identity 为 false 且 persona-prefix 非空
  当 组装提示词时
  那么 无身份句 section，前缀文本按序出现在原身份句位置

Rule: REQ-FR-PROMPT-4 — AGENTS.md 注入

场景: 项目指令进入 agent-instructions
  假设 工作目录存在 AGENTS.md
  当 组装提示词时
  那么 其内容进入 agent-instructions section

Rule: REQ-FR-PROMPT-5 — 脚本抽取禁手抄

场景: 提示词常量与 dsh 源逐字一致
  当 以抽取脚本生成提示词常量并与 dsh 源文件比对时
  那么 除身份句外逐字一致

Rule: REQ-FR-PROMPT-3 — 计划模式文案

场景: plan-mode 文案与 base patch 逐字
  假设 计划模式启用
  当 组装计划模式 section 时
  那么 文案与 dsh base patch 的 plan-mode 行逐字一致

Rule: REQ-FR-PROMPT-4 — 无 AGENTS.md 负向

场景: 无 AGENTS.md 不产生空 section
  假设 工作目录无 AGENTS.md
  当 组装提示词时
  那么 不存在空的 agent-instructions section

Rule: REQ-FR-PROMPT-2 — working-directory context

场景: working-directory context 跟随 cwd
  假设 会话工作目录变更
  当 组装 runtime-context 时
  那么 WORKING_DIRECTORY context 反映新 cwd

Rule: REQ-FR-PROMPT-6 — 组装确定性

场景: 同序工具名排序与未列工具
  假设 两个 section 同 order、工具集含 toolOrder 未列出项
  当 组装时
  那么 同 order 按 name 码元序排列，未列工具按字典序插入 unlisted 标记位

Rule: REQ-FR-PROMPT-2 — SECTION_ORDERS 全表（枚举生成）

场景: section HARNESS_IDENTITY 序值
  假设 读取 SECTION_ORDERS 常量表
  当 查 HARNESS_IDENTITY 时
  那么 序值与 dsh index.ts:131 全表一致

场景: section DEPLOYMENT_PERSONA_PREFIX 序值
  假设 读取 SECTION_ORDERS 常量表
  当 查 DEPLOYMENT_PERSONA_PREFIX 时
  那么 序值与 dsh index.ts:131 全表一致

场景: section PLAN_POLICY 序值
  假设 读取 SECTION_ORDERS 常量表
  当 查 PLAN_POLICY 时
  那么 序值与 dsh index.ts:131 全表一致

场景: section TEAM_POLICY 序值
  假设 读取 SECTION_ORDERS 常量表
  当 查 TEAM_POLICY 时
  那么 序值与 dsh index.ts:131 全表一致

场景: section PTC_ONLY 序值
  假设 读取 SECTION_ORDERS 常量表
  当 查 PTC_ONLY 时
  那么 序值与 dsh index.ts:131 全表一致

场景: section FILE_REFERENCE 序值
  假设 读取 SECTION_ORDERS 常量表
  当 查 FILE_REFERENCE 时
  那么 序值与 dsh index.ts:131 全表一致

场景: section TOOL_BASH 序值
  假设 读取 SECTION_ORDERS 常量表
  当 查 TOOL_BASH 时
  那么 序值与 dsh index.ts:131 全表一致

场景: section TOOL_PWSH 序值
  假设 读取 SECTION_ORDERS 常量表
  当 查 TOOL_PWSH 时
  那么 序值与 dsh index.ts:131 全表一致

场景: section TOOL_READ 序值
  假设 读取 SECTION_ORDERS 常量表
  当 查 TOOL_READ 时
  那么 序值与 dsh index.ts:131 全表一致

场景: section TOOL_WRITE 序值
  假设 读取 SECTION_ORDERS 常量表
  当 查 TOOL_WRITE 时
  那么 序值与 dsh index.ts:131 全表一致

场景: section TOOL_EDIT 序值
  假设 读取 SECTION_ORDERS 常量表
  当 查 TOOL_EDIT 时
  那么 序值与 dsh index.ts:131 全表一致

场景: section TOOL_GLOB 序值
  假设 读取 SECTION_ORDERS 常量表
  当 查 TOOL_GLOB 时
  那么 序值与 dsh index.ts:131 全表一致

场景: section TOOL_GREP 序值
  假设 读取 SECTION_ORDERS 常量表
  当 查 TOOL_GREP 时
  那么 序值与 dsh index.ts:131 全表一致

场景: section TOOL_JOBS 序值
  假设 读取 SECTION_ORDERS 常量表
  当 查 TOOL_JOBS 时
  那么 序值与 dsh index.ts:131 全表一致

场景: section TOOL_PTY 序值
  假设 读取 SECTION_ORDERS 常量表
  当 查 TOOL_PTY 时
  那么 序值与 dsh index.ts:131 全表一致

场景: section TOOL_WEB_SEARCH 序值
  假设 读取 SECTION_ORDERS 常量表
  当 查 TOOL_WEB_SEARCH 时
  那么 序值与 dsh index.ts:131 全表一致

场景: section TOOL_WEB_FETCH 序值
  假设 读取 SECTION_ORDERS 常量表
  当 查 TOOL_WEB_FETCH 时
  那么 序值与 dsh index.ts:131 全表一致

场景: section TOOL_LSP 序值
  假设 读取 SECTION_ORDERS 常量表
  当 查 TOOL_LSP 时
  那么 序值与 dsh index.ts:131 全表一致

场景: section TOOL_SESSION_QUERY 序值
  假设 读取 SECTION_ORDERS 常量表
  当 查 TOOL_SESSION_QUERY 时
  那么 序值与 dsh index.ts:131 全表一致

场景: section TOOL_GOAL 序值
  假设 读取 SECTION_ORDERS 常量表
  当 查 TOOL_GOAL 时
  那么 序值与 dsh index.ts:131 全表一致

场景: section TOOL_WORKFLOW 序值
  假设 读取 SECTION_ORDERS 常量表
  当 查 TOOL_WORKFLOW 时
  那么 序值与 dsh index.ts:131 全表一致

场景: section TOOL_RALPH 序值
  假设 读取 SECTION_ORDERS 常量表
  当 查 TOOL_RALPH 时
  那么 序值与 dsh index.ts:131 全表一致

场景: section TOOL_SUBAGENT 序值
  假设 读取 SECTION_ORDERS 常量表
  当 查 TOOL_SUBAGENT 时
  那么 序值与 dsh index.ts:131 全表一致

场景: section TOOL_REPORT 序值
  假设 读取 SECTION_ORDERS 常量表
  当 查 TOOL_REPORT 时
  那么 序值与 dsh index.ts:131 全表一致

场景: section TOOL_COMPUTER_USE 序值
  假设 读取 SECTION_ORDERS 常量表
  当 查 TOOL_COMPUTER_USE 时
  那么 序值与 dsh index.ts:131 全表一致

场景: section MCP_SERVERS 序值
  假设 读取 SECTION_ORDERS 常量表
  当 查 MCP_SERVERS 时
  那么 序值与 dsh index.ts:131 全表一致

场景: section TOOLS_SDK 序值
  假设 读取 SECTION_ORDERS 常量表
  当 查 TOOLS_SDK 时
  那么 序值与 dsh index.ts:131 全表一致

场景: section DELIVERABLE_FILE_REFERENCES 序值
  假设 读取 SECTION_ORDERS 常量表
  当 查 DELIVERABLE_FILE_REFERENCES 时
  那么 序值与 dsh index.ts:131 全表一致

场景: section STRUCTURED_OUTPUT 序值
  假设 读取 SECTION_ORDERS 常量表
  当 查 STRUCTURED_OUTPUT 时
  那么 序值与 dsh index.ts:131 全表一致

场景: section HARNESS_SOURCE 序值
  假设 读取 SECTION_ORDERS 常量表
  当 查 HARNESS_SOURCE 时
  那么 序值与 dsh index.ts:131 全表一致

场景: section WEB_SURFACE 序值
  假设 读取 SECTION_ORDERS 常量表
  当 查 WEB_SURFACE 时
  那么 序值与 dsh index.ts:131 全表一致

场景: section DEPLOYMENT_PERSONA_SUFFIX 序值
  假设 读取 SECTION_ORDERS 常量表
  当 查 DEPLOYMENT_PERSONA_SUFFIX 时
  那么 序值与 REQ-FR-PROMPT-7 全表逐字一致

Rule: REQ-FR-PROMPT-4 — AGENTS.md 注入形态

场景: 注入为 user 消息 system-reminder 帧
  假设 工作目录存在 AGENTS.md
  当 组装提示词时
  那么 内容渲染为 user 消息、包裹 system-reminder 帧、首行 "Instructions from: <path>"，而非 system section

场景: 变更与删除再注入
  假设 会话中 AGENTS.md 被修改或删除
  当 fs 工具触碰该嵌套指令文件后组装时
  那么 以 Updated/removed 语义再注入且触发再投影

场景: 尺寸门控与去重
  假设 指令文件超 maxSourceBytes 1MiB 或同组重复
  当 加载候选 AGENTS.md/CLAUDE.md 与 local 变体时
  那么 超限整文件跳过、maxBytes 二分截断恰好适配、同组去重、projectRoot 自 cwd 向上找 .git 标记

Rule: REQ-FR-PROMPT-6 — section 唯一性与字面保留

场景: complete section 唯一
  假设 多个提供者竞逐 complete section
  当 waterfall 结算时
  那么 至多一个生效且恢复唯一 prompt section；refreshContext 仅重跑 context 节

场景: interpolate false 保留字面
  假设 section 声明 interpolate:false 且文本含 {{var}}
  当 组装时
  那么 {{var}} 按字面保留不替换

## Dependencies

- REQ-FR-TOOLS

## Source Trace

- doc/prd.md#10: 提示词组装全部要点；SECTION_ORDERS 与 CONTEXT_ORDERS 全集以 dsh 源文件 :131/:149 为权威
- doc/prd.md#22: 提示词全部以脚本从 dsh 源文件抽取生成常量，禁止手抄
- confidence: high（PRD 第 10 节）
- doc/dsh-source-notes.md: 源码级技术关键点（file:line 证据）

## Open Questions

None.
<!-- /agent-spec:requirement -->

<!-- agent-spec:requirement id=REQ-FR-APPROVAL title="权限审批服务" tags=approval,permissions source=doc/prd.md -->
## Problem

审批链是安全基线：无应答者、沙箱不可用、依赖未声明一律拒绝，绝不静默放行（fail-closed）。契约细节以 dsh user-approval 源码为权威（PRD 已一手核实行号）。

## Requirements

- [REQ-FR-APPROVAL-1] ApprovalPolicy MUST 为 'ask' | 'never'；effectivePolicy MUST 为 override??config??'ask' 优先级链；`never` 在 dispatch 前由审批服务自身判 rejected（防 listener 绕过），不产生 approval/asked 事件对（dsh user-approval/src/index.ts:139）。
- [REQ-FR-APPROVAL-2] request() MUST 处于 open turn 内；审计对 approval/asked 与 approval/decided 必须 turn-enclosed。
- [REQ-FR-APPROVAL-3] 应答链（UI answerer）缺席或失败 MUST fail-closed 'unavailable'；越权返回值一律归一化为 'unavailable'（dsh user-approval/src/index.ts:55 结果词表 ['allowed-once','rejected','cancelled','unavailable']、:283-291 兜底与归一化）；唯一授予值 'allowed-once'，无持久 allow-list。
- [REQ-FR-APPROVAL-4] 策略 MUST 持久化为 approval/policy 事件（log-only 不进 transcript、最后一条生效）；setPolicy 同值 no-op；切换时注入 user 消息，文案逐字 `The approval policy changed from "x" to "y" (changed by the user).`（x/y 为实际策略值）；abort 竞速先到者以 cancelled 结算（dsh user-approval/src/index.ts 各处）。
- [REQ-FR-APPROVAL-5] 集成：tools/pre-execute waterfall 返回 {kind:'ask'} 交 ApprovalService serviceAsk；沙箱升级（如 bash 的 sandbox_permissions）走同一 seam。

## Scenarios

Rule: REQ-FR-APPROVAL-1 — 策略语义

场景: never 策略自动拒绝
  假设 策略为 never
  当 任一工具请求审批时
  那么 自动拒绝且不产生 approval/asked 事件对

场景: 权限模式映射策略
  假设 权限模式为 danger-full-access
  当 解析审批策略时
  那么 策略为 never；其余非 danger 模式下默认 ask

Rule: REQ-FR-APPROVAL-2 — turn 封闭

场景: open turn 之外请求被拒
  假设 当前无 open turn
  当 调用 request() 时
  那么 请求被拒，不产生 approval/asked 事件

Rule: REQ-FR-APPROVAL-3 — fail-closed 与归一化

场景: 无 answerer fail-closed
  假设 headless 形态无 UI answerer
  当 工具请求审批时
  那么 结果为 unavailable，模型收到拒绝结果

场景: 词表外返回值归一化
  假设 answerer 返回结果词表之外的值
  当 审批结算时
  那么 归一化为 unavailable

场景: allowed-once 一次性授予
  假设 answerer 应答 allowed-once
  当 审批结算时
  那么 该次放行且不产生任何持久 allow-list

Rule: REQ-FR-APPROVAL-4 — 策略持久化与通知

场景: 策略切换落事件并通知模型
  假设 用户把策略切换为 never
  当 切换结算时
  那么 append approval/policy 事件（最后一条生效）且向模型注入 user-message 通知

Rule: REQ-FR-APPROVAL-5 — 统一审批 seam

场景: bash 沙箱升级走同一 seam
  假设 bash 调用携带 sandbox_permissions 升级
  当 pre-execute 返回 ask 时
  那么 走 ApprovalService 同一 seam 结算

Rule: REQ-FR-APPROVAL-3 — rejected 与 cancelled 结算

场景: rejected 与 cancelled 正常结算
  假设 answerer 分别应答 rejected 与 cancelled
  当 结算时
  那么 按词表原值结算，不归一化为 unavailable，不产生放行

Rule: REQ-FR-APPROVAL-2 — asked/decided 配对

场景: 正常 ask 产生 asked 与 decided 配对
  假设 open turn 内工具触发 ask 且获得应答
  当 结算时
  那么 依次产生 approval/asked 与 approval/decided 且二者均在 turn 内

Rule: REQ-FR-APPROVAL-2 — 请求 id 新鲜性

场景: 审批请求 id 每次新鲜
  假设 同一会话连续两次 ask
  当 生成 ApprovalRequestId 时
  那么 每次为全新 randomUUID 且不复用

Rule: REQ-FR-APPROVAL-4 — 策略切换细则

场景: 同值切换 no-op
  假设 当前策略已是 never
  当 setPolicy 再设 never 时
  那么 no-op：不落事件、不注入通知

场景: 切换文案逐字
  假设 策略从 ask 切到 never
  当 注入通知时
  那么 user 消息文本恰为 The approval policy changed from "ask" to "never" (changed by the user).

场景: abort 竞速结算
  假设 审批等待中同时发生 abort 与应答
  当 竞速结算时
  那么 先到者胜出，先到 abort 则以 cancelled 结算

场景: effectivePolicy 优先级链
  假设 同时存在 override 与 config 值
  当 解析生效策略时
  那么 override 优先于 config 优先于默认 ask

## Dependencies

- REQ-FR-SESSION

## Source Trace

- doc/prd.md#11.1: ApprovalService 契约；fail-closed 行号 :55/:62/:139/:283-291 已一手核实
- confidence: high

## Open Questions

None.
<!-- /agent-spec:requirement -->

<!-- agent-spec:requirement id=REQ-FR-SANDBOX title="沙箱策略" tags=sandbox,security source=doc/prd.md -->
## Problem

沙箱是工具执行的安全边界。fly-rafter 需要与 dsh sandbox-policy 对齐的三预设策略与 fail-closed 执行体链。

## Requirements

- [REQ-FR-SANDBOX-1] 三预设 read-only | workspace-write | danger-full-access；默认两层并存：插件 schema 缺省 read-only（fail-safe），装配层覆盖为 `FR_PERMISSION_MODE ?? workspace-write`（对齐 dsh cordis.patch.yml:246 的 !!js 表达式）；审批策略联动公式 `mode === 'danger-full-access' ? 'never' : 'ask'`；workspace root = 进程启动 cwd（无会话 cwd 时的 fallback root 由配置提供）。
- [REQ-FR-SANDBOX-2] 每次请求 MUST 把解析后的策略文本写入 runtime-context 快照（完整当前值跟随保留历史之后，保缓存安全）；文本模板 MUST 逐字：read-only 为 `Current DSH file policy: read-only. Any available operation enforced by the DSH file sandbox cannot modify files in the standing mode. Do not refuse a required modification from this policy alone: try an available tool normally and follow any denial and escalation guidance it returns.`（fr 版将其中 DSH 字样按 1:1 文本治理规则处理）；workspace-write 为 `Current DSH file policy: workspace-write. Any available operation enforced by the DSH file sandbox may modify files under the session workspace: ${JSON.stringify(policy.workspaceRoot)}. Some platform temporary areas may also be writable.`（dsh sandbox-policy/src/index.ts:44-47）。
- [REQ-FR-SANDBOX-3] 执行体后端链 MUST：Linux bubblewrap → Landlock，macOS Seatbelt，Windows ACL restricted-token；不可用即 fail-closed（`sandbox-unavailable`）。
- [REQ-FR-SANDBOX-4] 文件系统 fence 与观察策略 MUST 对齐 dsh fs-sandbox / fs-observation-policy。

## Scenarios

Rule: REQ-FR-SANDBOX-1 — 三预设与覆盖

场景: 默认 workspace-write 与 root
  假设 未设置 FR_PERMISSION_MODE
  当 解析权限模式时
  那么 生效 workspace-write 且 workspace root 为进程启动 cwd

场景: read-only 与 danger-full-access 预设
  假设 FR_PERMISSION_MODE 分别为 read-only 与 danger-full-access
  当 解析时
  那么 沙箱模式对应生效且审批按公式 mode === 'danger-full-access' ? 'never' : 'ask' 联动

场景: 两层默认并存
  假设 未设置 FR_PERMISSION_MODE 且读取插件 schema 缺省
  当 分别取插件缺省与装配生效值时
  那么 插件 schema 缺省 read-only、装配层生效 workspace-write，两层并存均须复现

Rule: REQ-FR-SANDBOX-2 — 策略文本快照

场景: 解析后策略文本进入 runtime-context
  假设 任意请求准备就绪
  当 构造 runtime-context 快照时
  那么 解析后的策略文本写入快照，且完整当前值跟随保留历史之后

Rule: REQ-FR-SANDBOX-3 — 执行体后端链

场景: bubblewrap 缺失降级 Landlock
  假设 Linux 上 bubblewrap 不可用而 Landlock 可用
  当 工具需要沙箱执行时
  那么 降级 Landlock 执行且不报 sandbox-unavailable

场景: 后端全缺失 fail-closed
  假设 Linux 上 bubblewrap 与 Landlock 均不可用
  当 工具需要沙箱执行时
  那么 报 sandbox-unavailable，不静默放行

Rule: REQ-FR-SANDBOX-4 — 文件系统 fence

场景: workspace 外写入被拒
  假设 workspace-write 模式
  当 工具尝试写 workspace 外路径时
  那么 被文件系统 fence 拒绝，观察策略与 dsh fs-sandbox 及 fs-observation-policy 一致

Rule: REQ-FR-SANDBOX-3 — 平台后端选择

场景: 按平台解析执行体后端
  假设 分别在 linux、macos、windows 目标声明下解析
  当 选择沙箱执行体时
  那么 依次命中 bubblewrap/Landlock、Seatbelt、ACL restricted-token 链

Rule: REQ-FR-SANDBOX-4 — 观察策略对齐

场景: 观察策略与 dsh 对齐
  假设 工具读取 workspace 外路径
  当 按观察策略结算时
  那么 行为与 dsh fs-observation-policy 的对照断言一致

## Dependencies

- REQ-FR-CONFIG

## Source Trace

- doc/prd.md#11.2: 三预设表、FR_PERMISSION_MODE 覆盖、执行体后端链、fail-closed
- confidence: high

## Open Questions

None.
<!-- /agent-spec:requirement -->

<!-- agent-spec:requirement id=REQ-FR-COMPACTION title="上下文管理与压缩" tags=compaction,context source=doc/prd.md -->
## Problem

长会话超出模型上下文窗口。fly-rafter 需要与 dsh compaction / compaction-basic 对齐的压缩契约：summarizer 提示词逐字、剪枝、图片卸载与 spill 溢出存储，经 surface replace-op 落地。

## Requirements

- [REQ-FR-COMPACTION-1] 压缩契约与默认后端 MUST 对齐 dsh compaction / compaction-basic；summarizer 提示词常量 COMPACTION_INSTRUCTION MUST 逐字引用 dsh summarizer.ts:32-147（9 个固定 ## 节名与合并非复制既有 <compacted-summary> 规则在内）。
- [REQ-FR-COMPACTION-2] 触发数学 MUST：thresholdTokens = floor(min(contextWindow × 0.8, messageBudget − 65536))；messageBudget = contextWindow − reservedCompletionTokens；reserved = routed requestHeader maxTokens ?? adapter defaultMaxTokens ?? 0；缺上下文窗 → 警告一次后跳过；自动触发挂 agent/pre-step，仅 measured totalTokens ≥ thresholdTokens 时触发且剪枝后复查；pre-step 压缩失败警告 `step compaction failed: <msg>; continuing the turn` 后继续 turn。
- [REQ-FR-COMPACTION-3] 摘要请求 MUST：消息组成为（表面节点 0 为 system/message 时的 system head）+ 影子区消息按表面序 + 会话工具（自 request header）+ 恰一条 user 消息全文为 COMPACTION_INSTRUCTION；摘要目标 = 配置 summarizationProvider/Model 对 > 最新 routed request header > agent options，全无报错 "no provider/model available for summarization: ..."；provider/model 必须成对设置；调用 maxTokens = config.maxTokens（默认 headroomTokens 65536）。
- [REQ-FR-COMPACTION-4] 摘要 fail-closed MUST：finish error/aborted → LlmError；max-tokens → 错误码 'MAX_TOKENS' 文案 "summarization truncated at the token cap (incomplete checkpoint)"；image 输出拒绝 UNSUPPORTED_CONTENT；空文本 → "summarization produced no text summary content"。
- [REQ-FR-COMPACTION-5] 替换消息 MUST 为 CHECKPOINT_PREAMBLE（"This is an automatically generated checkpoint condensing an earlier span..."）+ `<compacted-summary>` 包裹摘要 + 收尾标记；收缩门：框架后估算 MUST 小于影子区 tokens 否则 "summary is not smaller than the shadowed content (N estimated framed tokens >= M)"；摘要错误经 waterfall 'compaction/summary-error'（recover 默认 false）裁决，重试时重备输入。
- [REQ-FR-COMPACTION-6] 保留 MUST：retainRatio 0.16 → retainTokens = floor(messageBudget × 0.16)；retainRatio ≥ thresholdRatio 拒绝装载；resolved retainTokens ≥ thresholdTokens 拒绝装载；选区永不含 node-0 的 system/message（firstIdx=1）、自尾累积至 retainTokens、边界回退到 toolPairingBalancedBefore 使 assistant tool-call/result 对不拆分、无可压缩返回 null。
- [REQ-FR-COMPACTION-7] 溢出恢复 MUST：agent/request-error 码 CONTEXT_WINDOW_EXCEEDED → 剪枝 + retainTokens 0 强制压缩，成功 {kind:'retry'}；计数器在 idle 与每 assistant/message 重置；耗尽透传原错误；表面已收缩后压缩失败仍重试；自动压缩仍超限报 "compaction still above threshold after N compaction attempts (X estimated tokens >= threshold Y)"。
- [REQ-FR-COMPACTION-8] 压缩 MUST 以持久括弧落地：compaction/start … compaction/summary + replace-surface user/message … compaction/end；失败 close 留可检测未配对 start；摘要结算后 MUST 全表面稳定性复查。
- [REQ-FR-COMPACTION-9] 工具结果剪枝 MUST：仅触 tool/result 表面事件；度量=文本块 Unicode 码点（非文本块计 0）；total > 8192 才剪；移除段 = [4096, total−1024) 码点区间；标记仅在首个相交块插入一次，文本逐字 `\n\n[... tool result middle pruned ...]\n\n`；非文本块保序；替换结果须更小且 ≤ threshold；每次替换 append compaction/prune 影子事件 + surfaceOp replace 的 tool/result（sourceEventSeqs=[seq]）；配置校验 head+marker+tail ≤ threshold。
- [REQ-FR-COMPACTION-10] spill MUST：max-inline-tokens 12500（省略=禁用）；挂 tools/post-execute 的 accepted 结果（跳过 exec.name==='read' 与纯文本 PTC 子调用）并限界 PTC dispatch 日志；定价=文本估算 + 每图模型 adapter 图像定价；超限全文落盘（图片渲染为 `[Image: <readonlyPath>; <mediaType>; <w>x<h>. Use read_image to view it.]`）；模型面替换 = 保留头 + `\n\n[...]\n\n` + 保留尾 + footer `(Omitted N bytes.[ Omitted M images.] Full formatted result stored at: <locator>. <retrievalHint>)`；预算预留最坏 notice 后头尾对半、图片不可分、surrogate 安全切片；任一步失败警告 `spill-policy: ...; keeping the inline content` 保留内联；存储布局 `<root>/session-<sha256(sessionId) 前 12hex>/<12hex 随机>-<encodedName>`、文件 0600 目录 0700、缺省 root=OS temp 下 mkdtemp `dsh-spill-XXXXXX`；启动清扫删 mtime 超清理期（默认 30 天、0 禁用）并剪空目录。
- [REQ-FR-COMPACTION-11] /compact MUST：仅空闲 agent（busy → "manual compaction requires an idle agent with no waking queued work"）；选区 retainTokens 0；成功 → `Compacted N history items (~M tokens).`；无可压缩 → 'No compactable history yet.'；取消 → "Compaction cancelled."；错误码 busy/cancelled/changed/summary/commit/persistence 各有固定文案；落地前 ctx.sessions.flush 确保持久。

## Scenarios

Rule: REQ-FR-COMPACTION-1 — 契约与 summarizer 提示词

场景: summarizer 提示词逐字一致
  当 以脚本抽取 summarizer 提示词常量时
  那么 与 dsh summarizer.ts:32-147 逐字一致，含 9 个固定 ## 节名与合并非复制规则

场景: 摘要请求消息组成
  假设 压缩启动且表面节点 0 为 system/message
  当 构造摘要请求时
  那么 消息含 system head、影子区消息按表面序、会话工具与恰一条全文为指令的 user 消息

Rule: REQ-FR-COMPACTION-2 — 触发数学

场景: 阈值计算与触发
  假设 上下文窗 200000、routed maxTokens 8192
  当 计算 thresholdTokens 时
  那么 取 floor(min(200000×0.8, (200000−8192)−65536)) 且 measured totalTokens 达阈值即在 pre-step 触发

场景: 缺上下文窗跳过
  假设 adapter 未声明上下文窗
  当 解析目标压力时
  那么 警告一次后跳过该 provider/model，不报错中断

场景: 剪枝后复查
  假设 触发时先执行工具结果剪枝
  当 剪枝结算后
  那么 复查 totalTokens，已低于阈值则不再压缩

Rule: REQ-FR-COMPACTION-3 — 摘要目标与参数

场景: 目标三级回退
  假设 依次配置摘要对、缺省摘要对两种部署
  当 选摘要目标时
  那么 配置对优先于最新 routed header 再优先于 agent options，缺配对拒绝装载，全无报 no provider/model available for summarization

Rule: REQ-FR-COMPACTION-4 — fail-closed

场景: max-tokens 截断拒绝
  假设 摘要流以 max-tokens 结束
  当 结算时
  那么 错误码 MAX_TOKENS 文案 summarization truncated at the token cap (incomplete checkpoint)

场景: 空文本拒绝
  假设 摘要流成功但零文本
  当 结算时
  那么 报 summarization produced no text summary content

Rule: REQ-FR-COMPACTION-5 — 替换与收缩门

场景: 替换消息形状
  假设 摘要文本就绪
  当 构造替换 user 消息时
  那么 为 CHECKPOINT_PREAMBLE 加 <compacted-summary> 包裹与收尾标记

场景: 收缩门拒绝
  假设 框架后估算不小于影子区 tokens
  当 结算替换时
  那么 拒绝并报 summary is not smaller than the shadowed content (N estimated framed tokens >= M)

场景: summary-error waterfall 默认不恢复
  假设 摘要调用失败
  当 compaction/summary-error waterfall 裁决时
  那么 recover 默认 false；重试时重备输入

Rule: REQ-FR-COMPACTION-6 — 保留与选区

场景: 保留边界与配平回退
  假设 尾部累积越过 retainTokens 且下一节点是半个 tool 对
  当 选区结算时
  那么 边界回退到配平位置使 assistant tool-call/result 对不拆分，且永不含 node-0

场景: 非法保留配置拒绝装载
  假设 retainRatio ≥ thresholdRatio 或 resolved retainTokens ≥ thresholdTokens
  当 插件装载时
  那么 拒绝并报明确错误

Rule: REQ-FR-COMPACTION-7 — 溢出恢复

场景: 上下文超限恢复重试
  假设 请求失败码 CONTEXT_WINDOW_EXCEEDED
  当 恢复结算时
  那么 剪枝并以 retainTokens 0 强制压缩，成功返回 retry 且计数器随后续 assistant/message 重置

场景: 恢复耗尽透传
  假设 恢复重试次数耗尽仍失败
  当 结算时
  那么 透传原始请求错误

Rule: REQ-FR-COMPACTION-8 — 持久括弧

场景: 括弧事件序列
  假设 一次压缩成功
  当 检查日志时
  那么 含 compaction/start 与 compaction/summary、replace-surface 与 compaction/end 的完整序列，失败 close 留可检测未配对 start

场景: 全表面稳定性复查
  假设 摘要结算时表面已被并发修改
  当 复查时
  那么 检出变化并按稳定性规则处理（放弃或重试）

Rule: REQ-FR-COMPACTION-9 — 剪枝细则

场景: 剪枝度量与标记
  假设 某工具结果含 10000 码点文本与非文本块
  当 剪枝结算时
  那么 仅按文本码点计总量，超 8192 才剪，移除 [4096, total−1024) 段，标记一次逐字 \n\n[... tool result middle pruned ...]\n\n，非文本块保序

场景: 剪枝落盘形态
  假设 一次剪枝生效
  当 检查日志时
  那么 append compaction/prune 影子事件与 surfaceOp replace 的 tool/result 且 sourceEventSeqs 恰含原 seq

Rule: REQ-FR-COMPACTION-10 — spill

场景: spill 替换形状与 footer
  假设 某工具结果估算超 12500 tokens
  当 spill 结算时
  那么 全文落盘、内联替换为头加 [...] 加尾加逐字 footer (Omitted N bytes.[ Omitted M images.] Full formatted result stored at: <locator>. <retrievalHint>)，read 工具与纯文本 PTC 子调用豁免

场景: spill 存储与清扫
  假设 多会话产生 spill 文件且存在超 30 天旧文件
  当 启动清扫运行时
  那么 删除超期文件并剪空目录，路径为 session-<sha256 前 12hex> 布局、文件 0600 目录 0700

场景: spill 失败保留内联
  假设 spill 落盘或替换任一步失败
  当 结算时
  那么 警告 spill-policy: ...; keeping the inline content 并保留内联原文

Rule: REQ-FR-COMPACTION-11 — 手动压缩

场景: /compact 空闲限定与文案
  假设 agent 忙碌时执行 /compact
  当 结算时
  那么 报 manual compaction requires an idle agent with no waking queued work；空闲成功时回 Compacted N history items (~M tokens).，无历史回 No compactable history yet.

## Dependencies

- REQ-FR-SESSION
- REQ-FR-LLM

## Source Trace

- doc/prd.md#12: 契约与默认后端对齐 compaction/compaction-basic；summarizer.ts:33 逐字；剪枝参数；图片卸载；spill max-inline-tokens 12500；/compact 触发
- confidence: high

## Open Questions

None.
<!-- /agent-spec:requirement -->

<!-- agent-spec:requirement id=REQ-FR-LLM title="模型接入层与三 provider" tags=llm,provider source=doc/prd.md -->
## Problem

模型接入是 harness 的动力源。fly-rafter 需要统一的 LlmAdapter seam 与流协议（StreamChunk 七种），第一天交付三个 provider 插件，且不带任何 dsh 私有 wire 扩展。

## Requirements

- [REQ-FR-LLM-1] LlmAdapter trait MUST（Rust 方法名按 PRD 5.1 用 snake_case）：唯一必须实现 `stream(options) -> StreamChunk 流`；另有 provider_info / resolve_model / prepare_call / list_models / provider_retry_policy / image_request_pricing / image_request_target（dsh 源码勘误：PRD 曾漏 imageRequestTarget [llm/src/index.ts:248-250]）；prepare_call 支持代际绑定防竞态。
- [REQ-FR-LLM-2] GenerateOptions MUST 为 Rust 结构体（snake_case 字段）：provider/model/messages/tools/tool_history/temperature/max_tokens/stop/signal/session_id/purpose（`compaction` | `session-title`）。
- [REQ-FR-LLM-3] StreamChunk MUST 为七种：block-start / text-delta / reasoning-delta / tool-call-delta / block-end / usage / finish（finish 携带重放信封）；block 索引关联交错 delta；tool arguments 保持原始 JSON 字符串直到 tool/call 事件 unparsed 存储；usage MUST 在 terminal finish 之前。
- [REQ-FR-LLM-4] 三个 provider 插件 MUST 第一天交付：fr-llm-deepseek（默认，Anthropic Messages 协议，base-url `https://api.deepseek.com/anthropic`，头 `anthropic-version: 2023-06-01`，含 Files API 与图片，SSE 解析，不带任何 dsh 私有 wire 扩展——无 dsh_session_log / dsh_plugin_packages / x-deepseek-harness-*）；fr-llm-openai（OpenAI-compatible，base-url/model/key 全可配）；fr-llm-anthropic（Anthropic 官方端点）。
- [REQ-FR-LLM-5] 鉴权经环境变量/配置；headless 与 SDK 形态 MUST NOT 挂交互式凭证库（对齐 dsh headless 禁用 credentials 的做法）。

- [REQ-FR-LLM-6] 装配结算 MUST：已 close 索引上的 delta 忽略、重复 close 忽略、无 finish 默认 stop、缺 id 或 name 兜底；max-tokens 丢弃全部 tool-call 块且 envelope 同步过滤；中断仅保留非空白 text 与 reasoning；ReplayEnvelope 为 {response, blocks}，丢块同位置过滤、长度不等整体丢弃（dsh assembler.ts:49-189）。
- [REQ-FR-LLM-7] assistant/message 内嵌流持久化打包 MUST 为四 record 类：text / reasoning / tool-call-chunks / chunk；时间为 dt 差分；id/name 不全的 tool-call-delta 落 raw record；block-start/end、usage、finish 恒 raw record（dsh assistant-stream.ts:107-190）。
- [REQ-FR-LLM-8] prepare_call 代际绑定 MUST：PreparedAdapterCall 冻结元数据；stream 一次性，二次派发报 INVALID_PREPARED_CALL；callConfigEquals 防跨代 capability 漂移（dsh llm/src/index.ts:956-1020）。
- [REQ-FR-LLM-9] 重试策略 MUST：mode normal（maxRetries 5、retryableCodes 五类）| always；backoff 500ms 起步、上限 10s、jitter 0.1 对称抖动；执行方为独立 retry 插件挂 agent 失败步扩展点（dsh retry-policy.ts:211-262）。
- [REQ-FR-LLM-10] fr-llm-deepseek 协议细则 MUST：messagesApiRoot 强制补全 /v1 后缀；SSE 经事件解析器，帧 event 名与 body.type 不一致 → 错误码 MALFORMED_RESPONSE；message_stop 三重校验（stop_reason 合法 + 全部块 closed + tool arguments 为合法 JSON）；stop_reason 按映射表转 finish kind；usage 自 message_start 起底、后续 delta 覆盖。
- [REQ-FR-LLM-11] 其余 provider 适配语义 MUST：适配层永不中途 throw，错误以 usage + finish(kind error/aborted) 事件收尾；toolcall_end 将已解析参数 stringify 回原始串（与 deepseek 逐段拼接同契约）；toolUpdate 能力差异按 provider 固定：anthropic → in-history、openai-completions → addition-only、responses 系（openai-responses/azure-openai-responses/openai-codex-responses）在 compat 声明 supportsAdditionalTools 或 supportsToolSearch 时 → addition-only、否则无（2026-10-25 Q59-a 勘误对齐 dsh 真源，dsh llm-pi-ai/src/stream.ts:202-227、models.ts:75-92）。
- [REQ-FR-LLM-12] 三 provider 形态与命名 MUST（2026-10-25 Q56A/Q57B/Q58 裁决，PRD 决策 37）：三 crate 平切维持（fr-llm-deepseek/fr-llm-openai/fr-llm-anthropic；1:1 口径=行为与协议对齐，不镜像 dsh TS 包结构——dsh 真实形态为 llm-deepseek 直连 + llm-pi-ai 库背书多路由）；fr-llm-openai MUST 实现 openai-completions 与 openai-responses 两协议，fr-llm-anthropic 实现 anthropic-messages；fr-llm-deepseek 保留 dsh 自有 DEFAULT_MODELS 内置目录（2 条目逐字），fr-llm-openai 与 fr-llm-anthropic 第一天 MUST NOT 内置模型 catalog——路由 models 全手声明、请求引用未声明模型即报错（fail-closed，19.2 条 20）；attribution User-Agent MUST 为 `fly-rafter/<version> (+<url>)` 格式（version 取 CARGO_PKG_VERSION；url = 'https://github.com/greatwallisme/fly-rafter'，2026-10-25 用户给定，去 .git 对齐 dsh attribution.ts:43 主页形态）且缺省不可抑制；重放信封 response.kind：deepseek 侧 'deepseek-messages' 逐字保留、openai 族 'fr-openai'、anthropic 族 'fr-anthropic'、不可用重放降级哨兵 'fr-foreign'（adapter 私有 durable 元数据值域=自有命名，非会话 v4 结构字段）；fr-llm-deepseek Files 上传文件名前缀逐字 'dsh-'（同 endpoint+key 命名空间与 dsh 共享上传池，配额回收互通，用户裁决接受），upload-index 路径 <FR_HOME>/llm-deepseek/files-v3.json（formatVersion 3）。
## Scenarios

Rule: REQ-FR-LLM-1 — adapter 契约

场景: prepare_call 代际绑定防竞态
  假设 同一 session 连续两次 prepare_call
  当 持过期代际句柄发起调用时
  那么 过期句柄被拒

Rule: REQ-FR-LLM-2 — GenerateOptions

场景: purpose 标记传递
  假设 压缩引擎与标题生成分别发起请求
  当 构造 GenerateOptions 时
  那么 purpose 分别为 compaction 与 session-title

Rule: REQ-FR-LLM-3 — StreamChunk 序

场景: usage 先于 terminal finish
  假设 一次流式请求正常结算
  当 消费 StreamChunk 流时
  那么 usage chunk 出现在 terminal finish 之前

场景: tool arguments 原始串保持
  假设 流中 tool-call-delta 交错到达
  当 拼接 arguments 时
  那么 拼接结果与 provider 原始 JSON 字符串逐字节一致，tool/call 事件按 unparsed 存储

Rule: REQ-FR-LLM-4 — 三 provider

场景: deepseek 无私有 wire 扩展
  假设 使用 fr-llm-deepseek 发起请求
  当 检查发出的 HTTP 请求时
  那么 目标为 base-url 加 anthropic-version 头，且无 dsh_session_log、dsh_plugin_packages 与 x-deepseek-harness 头

场景: openai base-url 覆盖
  假设 配置了自定义 base-url
  当 fr-llm-openai 发起请求时
  那么 请求发往配置端点而非硬编码地址

场景: anthropic 官方端点
  假设 使用 fr-llm-anthropic
  当 发起请求时
  那么 请求发往 Anthropic 官方端点且协议头正确

Rule: REQ-FR-LLM-5 — 鉴权来源

场景: headless 与 SDK 无交互凭证库
  假设 headless 或 SDK 形态启动
  当 检查装配与凭证读取时
  那么 仅从环境变量与配置读取，交互式凭证库插件不在装配中

Rule: REQ-FR-LLM-4 — SSE fixture 回放

场景: dsh 录制 SSE 回放产出相同 chunk 序
  假设 以 dsh 真实响应录制的 SSE fixture 为输入
  当 三 provider 的 SSE 解析器回放该 fixture 时
  那么 产出的 StreamChunk 序列与 dsh 解析结果一致

Rule: REQ-FR-LLM-2 — 参数透传

场景: 采样参数透传 wire
  假设 GenerateOptions 携带 temperature、max_tokens 与 stop
  当 provider 构造协议请求时
  那么 三者按协议字段透传

Rule: REQ-FR-LLM-6 — 装配结算

场景: max-tokens 与中断的块处理
  假设 流分别以 max-tokens 结束与中途中断
  当 装配结算时
  那么 max-tokens 丢弃全部 tool-call 块且 envelope 同步过滤；中断仅保留非空白 text 与 reasoning

Rule: REQ-FR-LLM-3 — StreamChunk 枚举与装配（枚举生成）

场景: chunk block-start 字段
  假设 适配器流式输出
  当 产生 block-start chunk 时
  那么 字段与 dsh types.ts 470 行定义逐字一致

场景: chunk text-delta 字段
  假设 适配器流式输出
  当 产生 text-delta chunk 时
  那么 字段与 dsh types.ts 470 行定义逐字一致

场景: chunk reasoning-delta 字段
  假设 适配器流式输出
  当 产生 reasoning-delta chunk 时
  那么 字段与 dsh types.ts 470 行定义逐字一致

场景: chunk tool-call-delta 字段
  假设 适配器流式输出
  当 产生 tool-call-delta chunk 时
  那么 字段与 dsh types.ts 470 行定义逐字一致

场景: chunk block-end 字段
  假设 适配器流式输出
  当 产生 block-end chunk 时
  那么 字段与 dsh types.ts 470 行定义逐字一致

场景: chunk usage 字段
  假设 适配器流式输出
  当 产生 usage chunk 时
  那么 字段与 dsh types.ts 470 行定义逐字一致

场景: chunk finish 字段
  假设 适配器流式输出
  当 产生 finish chunk 时
  那么 字段与 dsh types.ts 470 行定义逐字一致

场景: usage 强制先于 finish
  假设 正常结算流
  当 消费 chunk 序列时
  那么 usage 出现在 terminal finish 之前且其后无任何 chunk

场景: 重复 close 忽略
  假设 同一 block 索引收到两次 block-end
  当 装配时
  那么 首次生效后续忽略

场景: delta 落在已关闭块
  假设 delta 的目标块已 block-end
  当 装配时
  那么 该 delta 被忽略

场景: 缺 finish 默认 stop
  假设 流结束但无 finish chunk
  当 装配时
  那么 默认 finish kind 为 stop

Rule: REQ-FR-LLM-7 — 持久化打包

场景: 内嵌流四 record 打包
  假设 一次成功 step 的 StreamChunk 全集
  当 打包进 assistant/message 时
  那么 text 与 reasoning 与 tool-call-chunks 与 chunk 四类 record、dt 差分时间、id/name 不全的 tool-call-delta 落 raw、block-start/end 与 usage 与 finish 恒 raw

Rule: REQ-FR-LLM-8 — prepared call 一次性

场景: 二次派发拒绝
  假设 同一 PreparedAdapterCall 的 stream 已被消费
  当 再次派发时
  那么 报 INVALID_PREPARED_CALL；callConfig 跨代 capability 不匹配亦被拒

Rule: REQ-FR-LLM-9 — 重试数值

场景: normal 模式五次退避
  假设 retryable 错误连续发生
  当 重试结算时
  那么 最多 5 次、backoff 500ms 起步 10s 封顶、jitter 0.1 对称抖动；always 模式不限次数

Rule: REQ-FR-LLM-10 — deepseek 协议细则

场景: /v1 补全与 SSE 帧校验
  假设 配置根地址缺 /v1 且录制帧 event 名与 body.type 不一致
  当 构造请求与解析 SSE 时
  那么 请求路径强制补全 /v1，不一致帧报 MALFORMED_RESPONSE

场景: message_stop 三重校验
  假设 stop_reason 非法、或存在未闭合块、或 tool arguments 非合法 JSON
  当 message_stop 校验时
  那么 三种情形均拒绝；正常时 stop_reason 按映射表转 finish kind、usage 以 message_start 起底被 delta 覆盖

Rule: REQ-FR-LLM-11 — 适配层收尾与 toolUpdate

场景: 错误不中途抛出
  假设 上游连接中途失败
  当 适配层结算时
  那么 发 usage 与 finish(kind error) 收尾而非 panic；toolcall_end 参数串与 provider 原始串逐字节一致

场景: toolUpdate 能力差异
  假设 同一工具更新分别路由到 anthropic 与 openai-completions 与 responses 系模型（后者 compat 带 supportsAdditionalTools 与不带两种）
  当 应用更新时
  那么 anthropic 为 in-history 追加、openai-completions 为 addition-only、带 compat 标记的 responses 系为 addition-only、不带标记的 responses 系不支持（拒新更新）

Rule: REQ-FR-LLM-12 — 形态与命名

场景: openai 路由未声明模型报错
  假设 fr-llm-openai 路由未声明 models
  当 请求引用任一模型 id 时
  那么 请求被拒且错误指名未声明模型，不回退任何内置目录

场景: attribution 头格式与不可抑制
  假设 任意 provider 发起请求
  当 构造 User-Agent 时
  那么 值为 fly-rafter/<version> (+https://github.com/greatwallisme/fly-rafter) 格式、version 取 crate 元数据，且不存在不携带 attribution 的请求路径

场景: 重放信封 kind 命名
  假设 三 provider 各自产出重放信封
  当 读取 response.kind 时
  那么 deepseek 侧为 'deepseek-messages'、openai 族为 'fr-openai'、anthropic 族为 'fr-anthropic'；不可用重放降级走 'fr-foreign' 哨兵

场景: Files 前缀与索引路径逐字
  假设 fr-llm-deepseek 经 Files API 上传一张请求图片
  当 检查 provider 端文件名与本地索引时
  那么 文件名以 'dsh-' 开头、索引位于 <FR_HOME>/llm-deepseek/files-v3.json 且 formatVersion 为 3

## Dependencies

- REQ-FR-CONFIG

## Source Trace

- doc/prd.md#13.1: LlmAdapter seam、StreamChunk 七种、usage 先于 finish、prepare_call 代际绑定
- doc/prd.md#13.2: 三 provider 表；deepseek 无私有 wire 扩展（19.3）
- doc/prd.md#19.3: 仅标准 Anthropic Messages 协议
- confidence: high
- doc/dsh-source-notes.md: 源码级技术关键点（file:line 证据）
- doc/prd.md#13.2（2026-10-25 Q56A/Q57B/Q58 裁决段）+ PRD 决策 37 + 19.2 条 20：三 crate 平切/catalog 缺位/attribution/重放 kind/Files 前缀与索引路径
- Q58-1a url 已给定回填（2026-10-25，去 .git 对齐 dsh 主页形态）；Q59-a 已勘误落条款——本块无悬置

## Open Questions

None.
<!-- /agent-spec:requirement -->

<!-- agent-spec:requirement id=REQ-FR-MCP title="MCP 客户端" tags=mcp,tools source=doc/prd.md -->
## Problem

MCP 是外部工具接入通道。fly-rafter 需要与 dsh mcp-client 对齐的 transport 与动态工具注册。

## Requirements

- [REQ-FR-MCP-1] fr-mcp MUST 支持 stdio 与 Streamable HTTP 两种 transport：stdio 子进程环境 = 净化父环境 + 配置 env；http 携 url/headers（对齐 dsh mcp-client/index.ts:119-142、transport.ts:22-36）。
- [REQ-FR-MCP-2] 服务器名 MUST 匹配 [A-Za-z0-9_-]{1,32} 且实例间唯一，活跃重名装载失败 "mcp-client: serverName \"X\" is already in use..."；MCP 工具 MUST 以 raw JSON Schema 动态注册进 tools 注册表：公名 `mcp__<server>__<raw>` 归一到 64 字符上限 [A-Za-z0-9_-]，有损变更追加 `_<12hex sha256("server\0raw")>`；raw 名仅上 wire tools/call；description/inputSchema 透传（description 缺省空串）、outputSchema 仅在支持时保留。
- [REQ-FR-MCP-3] 资源工具 3 件 MUST 提供：list_mcp_resources / read_mcp_resource / list_mcp_resource_templates；渲染逐字 `MCP server: <server>\n<JSON>`、blob 字段替换 `[binary resource: N base64 characters; available to programmatic callers]`；系统提示词 section 'mcp-resource-servers' 文本逐字 `## MCP resource servers\n\nUse list_mcp_resources, list_mcp_resource_templates, or read_mcp_resource with one of these names as the server argument: ["a","b"].`；未知/越权 server → `MCP resource server "X" is unavailable in this agent's scope`；资源操作共用 toolCallTimeoutMs。
- [REQ-FR-MCP-4] 生命周期 MUST：apply 阻塞于 connection.ready（首连+同步，成败都返回）；failOnStartupError 默认 false，true → 抛 `mcp-client(<server>): initial connection or tool synchronization failed`；首次同步 registrationFailure 仅 failOnStartupError 时 throw、此后一律 contain；dispose 停重连、5s 关闭确认、等在途尝试与同步链、注销全部工具。
- [REQ-FR-MCP-5] 服务器指令 MUST：捕获为 `### MCP server: <serverName>\n\n<trim 文本>`（无则空串）；超 maxInstructionBytes（默认 32768）该次尝试失败；发布为系统提示词 section `mcp:<server>`、order MCP_SERVERS、interpolate:false。
- [REQ-FR-MCP-6] 重连 MUST：默认 enabled、initialDelayMs 500、maxDelayMs 30000、maxAttempts 10；延迟 = min(maxDelayMs, initialDelayMs × 2^(失败次数−1))；预算仅在连接存活 ≥ maxDelayMs（稳定窗）后掉线才重置；超限 → 注销全部工具、清指令、日志 `giving up after 10 consecutive failed reconnect attempts — tools unregistered; reload the plugin or restart the Host to reconnect`，仅插件重载可再连；enabled=false → 工具持续失败或永不注册；失败尝试 5s 内无法确认 transport 关闭 → 停止重连；断连期间工具保持注册、资源请求抛 `mcp-client(<server>): server is disconnected`、死代在途调用作废；重连成功日志 `reconnected and re-synced tools (attempt N/M)`。
- [REQ-FR-MCP-7] 工具同步 MUST 两段式：先取完整新代（失败旧代不动）；raw 重名 → `server listed tool "X" more than once — invalid tool list`；换装先 dispose 旧代再注册新代；注册表冲突回滚该服务器全部工具并 contain（启动时 throw）；tools/list_changed 触发串行重同步（cacheMode 'refresh'）。
- [REQ-FR-MCP-8] 工具调用 MUST：toolCallTimeoutMs 默认 60000 并随执行信号；超时 → isError 结果；MCP isError 结果抛出提取文本；非法 CallToolResult → `Tool "<raw>" returned an invalid MCP result: <issues>`；task-required 工具 → `Tool "<raw>" requires task-based execution, which this bridge does not support`；非对象模型参数坍缩为 {}。
- [REQ-FR-MCP-9] 内容投影 MUST 逐字：text 块 '\n' 连接；resource_link → `Resource link: <name> (<uri>)`（缺名或 uri → `[resource link unavailable: the MCP block is missing its name or URI]`）；audio → `[audio result unsupported: <mime>; raw audio data remains available to programmatic callers]`；embedded → `[embedded resource unsupported; raw resource data remains available to programmatic callers]`；未知类型 → `[unsupported MCP content type: <type>]`；非对象块 → `[unsupported MCP content block: expected an object]`；空内容 → `(<toolName> returned no model-visible content)`；图片仅 PNG/JPEG/WebP/GIF canonical base64 且模型路由声明 image input 才准入，拒绝投影 `[image unavailable: <mime>; <reason>; raw image data remains available to programmatic callers]` 且 canonical 值保留原始数据。

## Scenarios

Rule: REQ-FR-MCP-1 — 双 transport

场景: stdio 服务器连接
  假设 配置了一个 stdio MCP 服务器
  当 建立连接并完成握手时
  那么 连接就绪且子进程环境为净化父环境加配置 env，可列举工具

场景: Streamable HTTP 服务器连接
  假设 配置了一个 Streamable HTTP MCP 服务器
  当 建立连接并完成握手时
  那么 连接就绪且请求携带配置 url 与 headers，可列举工具

Rule: REQ-FR-MCP-2 — 命名空间与动态注册

场景: 工具以命名空间前缀注册
  假设 已连接一个 MCP 服务器
  当 列举其工具时
  那么 工具以命名空间前缀按 raw JSON Schema 动态注册进 ToolRegistry

Rule: REQ-FR-MCP-3 — 资源工具

场景: 资源工具三件可用
  当 分别调用 list_mcp_resources、read_mcp_resource、list_mcp_resource_templates 时
  那么 三者均返回对应资源数据且渲染逐字 MCP server: <server> 前缀格式

场景: 资源工具越权与提示词段
  假设 agent scope 不含某服务器而调用其资源
  当 结算时
  那么 报 MCP resource server "X" is unavailable in this agent's scope，且系统提示词含逐字 mcp-resource-servers 段

Rule: REQ-FR-MCP-2 — 工具命名模式

场景: stdio 工具命名模式
  假设 已连接 stdio 服务器 server-a 且其工具名为 raw-tool
  当 注册进 ToolRegistry 时
  那么 注册名严格为 mcp__server-a__raw-tool

场景: server 名约束
  假设 服务器名含非法字符或超过 32 字符
  当 配置加载时
  那么 拒绝（须 [A-Za-z0-9_-]{1,32} 且实例间唯一）

场景: 长名有损归一
  假设 server 加 raw 拼接超 64 字符或含非法字符
  当 生成公名时
  那么 归一到合法形态并追加 _加 12hex sha256 后缀，raw 名仅在 wire tools/call 使用

Rule: REQ-FR-MCP-4 — 生命周期

场景: 启动失败双形态
  假设 首连失败且分别以 failOnStartupError false 与 true 部署
  当 apply 结算时
  那么 false 时含错返回不影响装配其余部分，true 时抛 mcp-client(<server>): initial connection or tool synchronization failed

场景: dispose 语义
  假设 插件被卸载
  当 dispose 结算时
  那么 停止重连、5s 关闭确认、等待在途尝试与同步链、注销全部工具

Rule: REQ-FR-MCP-5 — 服务器指令

场景: 指令捕获与超限
  假设 服务器返回 instructions 且长度逼近上限
  当 发布提示词段时
  那么 以逐字 ### MCP server 头发布为 mcp:<server> 段（interpolate:false），超 32768 字节该次尝试失败

Rule: REQ-FR-MCP-6 — 重连

场景: 退避与稳定窗
  假设 连续 3 次连接失败
  当 计算重试延迟时
  那么 依次 500ms、1000ms、2000ms（封顶 30000ms）；仅存活满 30000ms 后掉线才重置计数

场景: 重连耗尽
  假设 连续失败超过 10 次
  当 结算时
  那么 注销全部工具并清指令，日志 giving up after 10 consecutive failed reconnect attempts — tools unregistered; reload the plugin or restart the Host to reconnect

场景: 断连期间调用
  假设 服务器断连且重连进行中
  当 调用其工具与资源接口时
  那么 工具保持注册但调用失败，资源请求抛 mcp-client(<server>): server is disconnected，死代在途调用被作废

Rule: REQ-FR-MCP-7 — 工具同步

场景: 两段式同步与回滚
  假设 新代获取失败与注册表冲突两种故障
  当 同步结算时
  那么 取代失败旧代不动；注册冲突回滚该服务器全部工具并 contain

场景: 重复工具名拒绝
  假设 服务器列表返回同名工具两次
  当 同步时
  那么 报 server listed tool "X" more than once — invalid tool list

场景: list_changed 触发重同步
  假设 服务器发 tools/list_changed 通知
  当 处理时
  那么 串行重同步且 cacheMode 为 refresh，不打断在途调用

Rule: REQ-FR-MCP-8 — 工具调用

场景: 超时与错误投影
  假设 调用超过 60000ms 默认与服务器返回 isError 两种情形
  当 结算时
  那么 超时产生 isError 结果；isError 抛出提取文本作为错误结果

场景: 非法结果与 task 工具
  假设 服务器返回不合 schema 的 CallToolResult 与声明 task 必需的工具
  当 结算时
  那么 分别报 Tool "<raw>" returned an invalid MCP result 与 requires task-based execution 文案

场景: 非对象参数坍缩
  假设 模型对某 MCP 工具提交非对象参数
  当 调用时
  那么 参数坍缩为 {} 上 wire

Rule: REQ-FR-MCP-9 — 内容投影

场景: 特殊内容块逐字投影
  假设 结果分别含 resource_link、audio、embedded resource、未知类型、非对象块
  当 投影时
  那么 五种情形各自产出逐字占位文案，text 块以换行连接

场景: 图片准入门控
  假设 图片结果分别为合法 canonical base64 且模型支持、与模型不支持 image input
  当 投影时
  那么 前者以图像内容准入，后者投影逐字 image unavailable 占位且 canonical 值保留原始数据供程序调用方

场景: 空内容占位
  假设 工具成功返回零内容块
  当 投影时
  那么 产出 (<toolName> returned no model-visible content)

## Dependencies

- REQ-FR-TOOLS

## Source Trace

- doc/prd.md#14: transport、命名空间、动态注册、资源工具 3 件
- confidence: high

## Open Questions

None.
<!-- /agent-spec:requirement -->

<!-- agent-spec:requirement id=REQ-FR-SUBAGENT title="子代理系统" tags=subagent,agents source=doc/prd.md -->
## Problem

复杂任务需要委派子代理。fly-rafter 提供进程内 spawn/fork 与 ACP 委派桥；明确不做 claude-code / codex 委派桥（用户裁决）。

## Requirements

- [REQ-FR-SUBAGENT-1] 进程内 spawn MUST：child id = randomUUID 会话 id（external = provider 铸造、跨父跨 provider 唯一）；重复 id → `subagent "X" already exists` 码 DUPLICATE_CHILD；subagent 工具即时返回 `{kind:'activation', subagentId}` 渲染 `started subagent <id>`；父 durable log 收 subagent/catalog 分块清单记录（childId/label/mode）；生命周期边为事件 subagent/start（{runId,provider,id,local}）与 subagent/end（+stopReason,lastAssistantMessage?）作用于委派父；spawn 不共享任何对话上下文（inheritsParentContext:false、agentPreset 能力 true）。
- [REQ-FR-SUBAGENT-2] 结果送达 MUST：manager 清理后以 user 消息投递（父空闲 queue、运行中 steer）；文案族逐字 `Background subagent <id> finished and will do no further work unless you send it more.` / `…finished. It cannot receive follow-up messages.` / `…was stopped before it finished.` / `…ran out of room…` / `…declined the task.` / `…failed before it finished.`，后接 `Its closing message:`（无则 `It left no closing message.`）与可选 `Structured result: <json>`；source {kind:'subagent-settled',form:'notice',summary,senderSessionId}；stop reasons completed/aborted/error/max-tokens/refusal；可续 provider 的子初始 prompt 追加返回指引（父 id 与 send_message 指令）。
- [REQ-FR-SUBAGENT-3] fork MUST：种子 = 父日志最后一个 turn/end 为止的均衡完成 turn 前缀（in-flight turn 排除、无完成 turn 则空），创建时一次捕获为子自有持久 transcript；子继承 preset 组合与委托策略；cwd 默认父生效目录；fork agentPreset:false、inheritsParentContext:true。
- [REQ-FR-SUBAGENT-4] interrupt_agent MUST：同步输出 `interrupt requested for agent <id>` 与 {accepted:true}，不等待；取消信号下目标可继续运行至观察点；未领取 inbox、Activation 与已发布后代保留；external 目标永久停止；local 目标 cancel({kind:'user'|'parent'},{keepInbox:true})；目标不存在为 accepted no-op；UNAUTHORIZED 判四情形：非活跃祖先、self-interrupt、他父子代、非活跃后代。
- [REQ-FR-SUBAGENT-5] send_message MUST：运行中目标下 step 准入（steer）、空闲目标开新 turn（queue/followup）；渲染 `message delivered to agent <id>` 返回 {messageId}（送达确认非答案）；child→parent 投递包裹 `Agent <id> sent a message: …` source {kind:'agent-message',form:'relay',senderSessionId}；缺席 direct child 从持久层冷续；非驻留 child 向父发送拒绝 `…cannot send to parent…` UNAUTHORIZED；external child → NOT_CONTINUABLE。
- [REQ-FR-SUBAGENT-6] list_agents MUST：scope children（默认，直接子代自持久 catalog、不加载 Agent）| descendants（递归前序、行含 parent 与 depth）；one-shot 与 external 子代不列出；诊断行 {kind:'diagnostic',reason:'corrupt'|'unsupported'|'unavailable'}；status running/inactive 自活跃注册表；空渲染 `(no subagents)`；行格式 `<id> [<status>] parent=<p> depth=<d> — <label>`。
- [REQ-FR-SUBAGENT-7] 容量与深度 MUST：per-root ActivationPool 经不间断可续父链全后代共享；超限 → `subagent limit reached (active child limit: N); wait for an existing child to finish or complete this work with the current agents`；子深度 = 父 + 1、持久头为单调下界（resume 不归零）；超帽 → `subagent depth D exceeds maxDepth M`；数字 maxDepth 需 provider depthLimit 能力否则装载失败 `provider "X" cannot enforce maxDepth (no depthLimit capability) — set maxDepth: 'provider-managed'…`；到帽 agent 获模型上下文 `Delegation depth for \`subagent\`: current D, maximum M, remaining levels 0. Do not call…`。
- [REQ-FR-SUBAGENT-8] preset 门控 MUST：请求 agentPreset 需能力且 !inheritsParentContext，否则 UNSUPPORTED_CAPABILITY "does not support independent agent preset selection"；缺 agent-presets 服务 → PRESET_UNAVAILABLE；新建 append agent-preset/selected 与 subagent/descriptor 事件；省略继承父 preset；独立选 preset 的子冷续需 preset 投影否则 NOT_RESUMABLE。
- [REQ-FR-SUBAGENT-9] 委托策略 MUST：子审批钉 'never'；父沙箱 override 与 Auto/Full-access preset 以 source:'delegation' 事件捕获；子获固定上下文 "You are a delegated subagent: your permission scope was fixed…"。
- [REQ-FR-SUBAGENT-10] subagent-acp 委派桥 MUST 把任务委派给外部 ACP agent（permission 默认 reject、disposeGraceMs 配置），与 fr-acp 共享 ACP 客户端代码；MUST NOT 实现 claude-code / codex 委派桥。

## Scenarios

Rule: REQ-FR-SUBAGENT-1 — spawn 生命周期

场景: spawn 子会话 realm 隔离
  假设 主会话 spawn 一个子代理
  当 子代理运行时
  那么 子 agent 与子会话置于子 realm，工具即时返回 started subagent <id> 形态的 activation，子会话不进入模型主历史

场景: catalog 与生命周期事件
  假设 子代理自创建到结束
  当 检查父日志与事件时
  那么 父日志含 subagent/catalog 分块清单，subagent/start 与 subagent/end 事件载荷含 runId/provider/id/local 与 stopReason

场景: 重复 id 拒绝
  假设 请求的 child id 已存在
  当 创建时
  那么 报 subagent "X" already exists 码 DUPLICATE_CHILD

Rule: REQ-FR-SUBAGENT-2 — 结果送达

场景: 完成通知文案族
  假设 子代理分别以 completed、refusal、failed 结束且带收尾消息
  当 送达父会话时
  那么 对应逐字文案族出现并接 Its closing message: 与内容，source 为 subagent-settled notice

场景: 无收尾消息与结构化结果
  假设 子代理未留消息但产出结构化结果
  当 送达时
  那么 出现 It left no closing message. 与 Structured result: <json>

场景: 送达时机
  假设 父会话分别处于运行中与空闲
  当 子代理结束送达时
  那么 运行中走 steer 队列、空闲走 queue 开新轮

Rule: REQ-FR-SUBAGENT-3 — fork 语义

场景: fork 前缀派生
  假设 主会话含三个完成 turn 与一个进行中 turn 时调用 subagent_fork
  当 子会话创建时
  那么 种子为最后 turn/end 为止的均衡前缀、in-flight 排除、delegationDepth 递增，子以后续冷续重放该 transcript 而非重新 fork

场景: fork 无完成 turn
  假设 父日志无任何完成 turn
  当 fork 时
  那么 种子为空且子会话照常创建

Rule: REQ-FR-SUBAGENT-4 — interrupt_agent

场景: 中止请求不等待
  假设 子代理运行中
  当 调用 interrupt_agent 时
  那么 立即返回 interrupt requested for agent <id> 与 accepted:true；目标可运行至观察点，未领取 inbox 与已发布后代保留

场景: 非法目标四情形
  假设 分别对不存在目标、自身、他父子代、非活跃后代调用
  当 结算时
  那么 不存在为 accepted no-op，其余三类报 UNAUTHORIZED

Rule: REQ-FR-SUBAGENT-5 — send_message

场景: 送达确认非答案
  假设 子代理运行中收到 send_message
  当 结算时
  那么 渲染 message delivered to agent <id> 并返回 messageId，消息于该子下 step 准入

场景: child 到 parent 中继
  假设 子代理向父发消息
  当 投递时
  那么 以 Agent <id> sent a message: … 包裹且 source 为 agent-message relay

场景: external 与缺席目标
  假设 对 external child 发消息、缺席 direct child 发消息
  当 结算时
  那么 external 报 NOT_CONTINUABLE，缺席者自持久层冷续后送达

Rule: REQ-FR-SUBAGENT-6 — list_agents

场景: 清单行格式与空态
  假设 存在直接子代与孙代、另有 one-shot 子代
  当 分别以 children 与 descendants scope 列举时
  那么 children 仅直接子代，descendants 前序含 parent 与 depth；one-shot 不列；空时渲染 (no subagents)

场景: 诊断行与状态
  假设 catalog 存在损坏记录且部分子代活跃
  当 列举时
  那么 产出 diagnostic 行（corrupt/unsupported/unavailable）且活跃行 status 为 running

Rule: REQ-FR-SUBAGENT-7 — 容量与深度

场景: 激活池超限
  假设 活跃子代达 maxActiveSubagents 上限后再次 spawn
  当 结算时
  那么 报 subagent limit reached (active child limit: N) 文案且建议等待或收敛

场景: 深度超帽
  假设 委派深度达 maxDepth 后继续委派
  当 结算时
  那么 报 subagent depth D exceeds maxDepth M；到帽 agent 收到逐字 Delegation depth 上下文

场景: maxDepth 能力门控
  假设 provider 无 depthLimit 能力而配置数字 maxDepth
  当 装载时
  那么 失败并报 cannot enforce maxDepth (no depthLimit capability) 指引 provider-managed

Rule: REQ-FR-SUBAGENT-8 — preset 门控

场景: 独立 preset 能力校验
  假设 fork（inheritsParentContext）请求自定义 agentPreset
  当 结算时
  那么 UNSUPPORTED_CAPABILITY does not support independent agent preset selection；spawn 路径合法放行并 append agent-preset/selected 与 subagent/descriptor

场景: 冷续投影要求
  假设 独立选 preset 的子代理进程消失后冷续
  当 恢复时
  那么 preset 投影存在则恢复，缺失报 NOT_RESUMABLE

Rule: REQ-FR-SUBAGENT-9 — 委托策略

场景: 子审批与沙箱委托
  假设 父会话携带沙箱 override 与 Full-access preset 委派子代理
  当 子会话组装时
  那么 子审批策略钉 never、委托事实以 delegation 来源事件留存、子获固定委派上下文文案

Rule: REQ-FR-SUBAGENT-10 — ACP 桥与排除

场景: ACP 桥端到端委派
  假设 外部 ACP agent 进程可用
  当 通过 subagent-acp 委派任务时
  那么 任务完成结果回流主会话且权限请求默认 reject

场景: 委派桥清单仅含 ACP
  当 列举全部委派桥时
  那么 仅存在 subagent-acp，无 claude-code 与 codex 桥

## Dependencies

- REQ-FR-SESSION
- REQ-FR-AGENT-LOOP
- REQ-FR-ACP

## Source Trace

- doc/prd.md#15: 进程内 spawn/fork、配套工具、ACP 委派桥、不做 claude-code/codex
- confidence: high

## Open Questions

None.
<!-- /agent-spec:requirement -->

<!-- agent-spec:requirement id=REQ-FR-COMMANDS title="斜杠命令框架" tags=commands,interaction source=doc/prd.md -->
## Problem

交互入口需要斜杠命令。fly-rafter 提供 commands 服务与内置命令；headless 任务文本不解析命令。

## Requirements

- [REQ-FR-COMMANDS-1] 解析 MUST：正则 `^\/([a-z][a-z0-9_-]*)(?=$|[\t\n\r ])`；rawInput = 精确余串含分隔空白、永不归一；无引号/转义规则（handler 自行拆分）；非命令返回未命中且不记日志；不以 / 开头（或裸 /）的输入按普通内容交给模型。
- [REQ-FR-COMMANDS-2] 注册 MUST：名称 `^[a-z][a-z0-9_-]*$`、描述与 handler 与 hint 非空；同层重名 → `command "X" is already registered…`；scoped（每 agent）定义遮蔽 global；list() 名称序；注册表变更发 commands/change；补全元数据供交互入口消费。
- [REQ-FR-COMMANDS-3] 执行语义 MUST：命令从不发给模型；handler 前 append command/run（载荷 {commandId,name,args?,source:{kind:'user'}}，recordInput:false 时省略 args）、结算后 command/done（载荷 {commandId,kind:'success'|'error',text?,sourceEventSeq?}）；二者 log-only 不进 transcript、不占 turn；commandId = `cmd-<8 位 uuid 片>-<单调序>`；handler 抛错/中止 → command/done kind:'error' 带渲染消息后重抛；附件给无附件命令 → `/<name> does not accept attachments`；无 store → `…attachments are unavailable because no attachment store is composed`。
- [REQ-FR-COMMANDS-4] 内置命令 MUST 逐字对齐：/goal（语法 `[<objective>|clear|edit <objective>|pause|resume]`，空=显示 title/Status/Blocker/Objective/Rounds/Activation/Commands 块；已设/missing-goal/GoalError 各固定错误文案；附件在 create/edit 作 "Reference attachments for the goal objective." followup、非 create/edit 报错；clear 无 goal → "No goal to clear."）；/compact（无参强制 `Usage: /compact (no arguments)`；成功 `Compacted N history items (~M tokens).`、无历史 `No compactable history yet.`、取消 "Compaction cancelled."、错误码 busy/cancelled/changed/summary/commit/persistence 各固定文案）；/feedback（recordInput:false、空白 → "Feedback text is required. Usage: /feedback <text>"、成功 "Feedback recorded for session <id>\nAnonymous user: <id>."、append log-only feedback/record）；/permission（空参报当前 preset 与可用表、未知 → `unknown preset "X" (available: …)`、成功 `preset X`）；/plan（hint `[off|message]`，off 文案族 "Plan mode off." / "Leaving plan mode (applies from the next step)." / "Plan mode entry cancelled." / "Plan mode is already inactive."，on → "Plan mode on. Use /plan off to leave." 或 "Entering plan mode (applies from the next step).…"，非 off 消息与附件经 agent.steer 转发，off 带附件 → "Attachments cannot accompany /plan off."）。
- [REQ-FR-COMMANDS-5] /help MUST NOT 注册（宿主无 help 命令；帮助卡由交互入口自行渲染）。
- [REQ-FR-COMMANDS-6] headless MUST NOT 解析任何交互命令（任务文本非命令输入）。
- [REQ-FR-COMMANDS-7] 交互入口提交路径 MUST：未命中的类命令行 → 错误 outcome 且保留草稿与附件；命中命令恒向入口报成功（结果经 command/run 与 done 广播渲染）；本地发 command/executed 事件仅提交方可见。

## Scenarios

Rule: REQ-FR-COMMANDS-1 — 解析

场景: 正则与 rawInput 保真
  假设 输入分别为 /goal 上下文含双空格、/goal、/1bad、裸 /
  当 parseCommand 结算时
  那么 首例 name 为 goal 且 rawInput 原样含双空格；/goal 命中空参；/1bad 与裸 / 不命中且不记日志、交模型普通输入

Rule: REQ-FR-COMMANDS-2 — 注册

场景: 重名与遮蔽
  假设 同层注册重复名、另一 agent 层注册同名命令
  当 注册与列举时
  那么 同层报 already registered；scoped 层遮蔽 global；list 名称序；变更发 commands/change

Rule: REQ-FR-COMMANDS-3 — 生命周期事件

场景: run 与 done 配对
  假设 一次成功命令与一次抛错命令
  当 结算时
  那么 成功者先后产生 command/run 与 kind success 的 command/done（log-only 无 turn）；抛错者产生 kind error 后错误继续传播；commandId 形如 cmd-<8位>-<序>

场景: 附件拒绝文案
  假设 向无附件命令带附件、或在无 attachment store 部署带附件
  当 执行时
  那么 分别报 does not accept attachments 与 attachments are unavailable 逐字文案

Rule: REQ-FR-COMMANDS-4 — 内置命令

场景: /goal 语法族
  假设 依次输入空参、clear（无 goal）、非法 edit、二次创建
  当 分发时
  那么 空参渲染完整状态块；clear 无 goal 回 No goal to clear.；非法 edit 报 usage；已设 goal 报固定错误；GoalError 回固定指引文案

场景: /goal 附件规则
  假设 create 带附件与非 create 子命令带附件
  当 分发时
  那么 前者以 Reference attachments for the goal objective. followup 入队，后者报错

场景: /compact 全分支
  假设 空闲无历史、空闲有历史、带参数、压缩被取消四种输入
  当 分发时
  那么 分别回 No compactable history yet.、Compacted N history items (~M tokens).、Usage: /compact (no arguments)、Compaction cancelled.

场景: /feedback 与 /permission
  假设 /feedback 空白与有文本、/permission 空参与未知名
  当 分发时
  那么 feedback 报必填文案或成功文案并落 log-only 记录；permission 报当前 preset 表或 unknown preset 文案，合法名切换成功回 preset X

场景: /plan 文案族
  假设 依次 /plan off（活动/非活动/待进入取消）、/plan 开启消息、/plan off 带附件
  当 分发时
  那么 off 文案族按态回正确一条；非 off 消息经 agent.steer 以用户消息转发并回开启文案；off 带附件报 Attachments cannot accompany /plan off.

Rule: REQ-FR-COMMANDS-5 — 无 help 命令

场景: /help 未注册
  当 输入 /help 时
  那么 宿主注册表无 help 命令，按未知命令路径处理

Rule: REQ-FR-COMMANDS-6 — headless 不解析

场景: 任务文本不触发命令分发
  假设 任务文本以 /compact 开头
  当 headless 处理时
  那么 整段文本作为任务交给模型，不触发命令分发

Rule: REQ-FR-COMMANDS-7 — 交互入口提交

场景: 未命中保草稿
  假设 入口提交 /unknown cmd 带草稿与附件
  当 结算时
  那么 报错误 outcome 且草稿与附件保留不丢失；命中的命令恒报成功并以广播事件渲染结果

## Dependencies

- REQ-FR-COMPACTION

## Source Trace

- doc/prd.md#16: commands 服务、内置命令 3 件、headless 不适用
- confidence: high

## Open Questions

None.
<!-- /agent-spec:requirement -->

<!-- agent-spec:requirement id=REQ-FR-BUILTIN-TOOLS title="内置工具集" tags=builtin-tools,tools source=doc/prd.md -->
## Problem

内置工具是模型能力面。全部工具的参数级 schema 以 dsh docs/tool-catalog.md 为权威，需以生成式文档防漂移；plugin_market 明确不做。

## Requirements

- [REQ-FR-BUILTIN-TOOLS-1] 第 17 节工具全集 MUST 实现：shell（bash、pwsh、persistent bash/pwsh opt-in）；文件（read、write、edit、read_image、glob、grep、str_replace_editor opt-in）；web（web_search、web_fetch）；任务状态（todo_write、create_goal/get_goal/update_goal、job_kill/job_list/job_output、schedule_create/delete/list/update）；交互（ask_user_question、exit_plan_mode）；会话查询（session_search、session_trace、session_event_read、session_event_search、session_event_trace）；PTY 六件套（terminal_open/read/send/signal/list/close，opt-in）；技能（skill、load_workspace_dependencies）；代码智能（lsp）；目录（working_directory）；内省管理（cordis_inspect_list、cordis_inspect_query、plugin_manager）。
- [REQ-FR-BUILTIN-TOOLS-2] 每个工具的参数级 schema MUST 对照 dsh docs/tool-catalog.md 冻结，并以生成脚本产出 fly-rafter 的 tool-catalog 供 diff 验证。
- [REQ-FR-BUILTIN-TOOLS-3] grep 工具 MUST 经内置 ripgrep 子进程（解析 PATH 上的 rg，`FR_RIPGREP` 可指认，打包期捆绑）；缺失时 grep 工具明确报错。
- [REQ-FR-BUILTIN-TOOLS-4] ask 类工具（ask_user_question 等）在 headless 下 MUST fail-closed。
- [REQ-FR-BUILTIN-TOOLS-5] MUST NOT 实现 plugin_market。
- [REQ-FR-BUILTIN-TOOLS-6] web_search 与 web_fetch MUST 支持 provider 可换（deepseek-official / exa / perplexity / http 适配位）。

## Scenarios

Rule: REQ-FR-BUILTIN-TOOLS-1 — 工具全集

场景: 注册表含第 17 节全部工具
  当 列举 ToolRegistry 时
  那么 含 shell、文件、web、任务状态、交互、会话查询、PTY、技能、lsp、working_directory 与内省管理的全部工具名

场景: persistent shell 需 opt-in
  假设 未启用 persistent shell 配置
  当 列举默认装配的工具时
  那么 无 persistent bash 与 persistent pwsh

Rule: REQ-FR-BUILTIN-TOOLS-2 — catalog 冻结与生成

场景: 工具 schema 与 catalog diff 为空
  假设 以生成脚本从真实装配产出 tool-catalog
  当 与冻结的参数表比对时
  那么 diff 为空

Rule: REQ-FR-BUILTIN-TOOLS-3 — ripgrep 解析

场景: ripgrep 缺失明确报错
  假设 PATH 与 FR_RIPGREP 均无 rg
  当 调用 grep 工具时
  那么 返回明确错误而非静默降级

场景: FR_RIPGREP 指认生效
  假设 FR_RIPGREP 指向一个 rg 可执行文件
  当 调用 grep 工具时
  那么 使用该指认的 rg 执行

Rule: REQ-FR-BUILTIN-TOOLS-4 — ask 类 fail-closed

场景: headless 下 ask 工具 fail-closed
  假设 headless 形态调用 ask_user_question
  当 工具执行时
  那么 fail-closed 返回，模型收到拒绝结果

Rule: REQ-FR-BUILTIN-TOOLS-5 — 无 plugin_market

场景: 注册表不含 plugin_market
  当 列举全部工具与插件名时
  那么 不存在 plugin_market

Rule: REQ-FR-BUILTIN-TOOLS-6 — web provider 适配位

场景: web_search provider 可切换
  假设 web 搜索 provider 配置为 exa
  当 调用 web_search 时
  那么 请求经 exa 适配位发出

Rule: REQ-FR-BUILTIN-TOOLS-1 — str_replace_editor opt-in

场景: 替代编辑器默认不装配
  假设 未启用 str_replace_editor 配置
  当 列举默认装配工具时
  那么 无 str_replace_editor

Rule: REQ-FR-BUILTIN-TOOLS-1 — 后台作业与 todo 状态

场景: run_in_background 产生可管理作业
  假设 bash 以 run_in_background 执行
  当 作业运行时
  那么 job_list 可见、job_output 可读、job_kill 可终止

场景: todo 状态流转校验
  假设 todo 列表含 pending、in_progress 与 completed 项
  当 todo_write 提交状态变更时
  那么 非法流转被拒，合法流转写入

Rule: REQ-FR-BUILTIN-TOOLS-1 — 工具目录逐行枚举（catalog 生成）

场景: bash 调度与呈现
  假设 合约要求的行为已实现
  当 读取工具 bash 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 terminal

场景: bash 参数越界拒绝
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

场景: bash 说明 section 注册
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 bash 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

场景: bash 参数 description 形状
  假设 工具 bash 已注册
  当 校验 schema 时
  那么 参数行 [description:string:req（主动语态 5-10 词，置于 command 前）] 的名称与类型按目录行实现

场景: bash 参数 description 约束
  假设 工具 bash 已注册
  当 校验 schema 时
  那么 参数行 [description:string:req（主动语态 5-10 词，置于 command 前）] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: bash 参数 command 形状
  假设 工具 bash 已注册
  当 校验 schema 时
  那么 参数行 [command:string:req（bash -c 命令）] 的名称与类型按目录行实现

场景: bash 参数 command 约束
  假设 工具 bash 已注册
  当 校验 schema 时
  那么 参数行 [command:string:req（bash -c 命令）] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: bash 参数 timeoutMs 形状
  假设 工具 bash 已注册
  当 校验 schema 时
  那么 参数行 [timeoutMs:number:opt（执行器施加默认与上限；到期转后台 job 而非杀死）] 的名称与类型按目录行实现

场景: bash 参数 timeoutMs 约束
  假设 工具 bash 已注册
  当 校验 schema 时
  那么 参数行 [timeoutMs:number:opt（执行器施加默认与上限；到期转后台 job 而非杀死）] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: bash 参数 workdir 形状
  假设 工具 bash 已注册
  当 校验 schema 时
  那么 参数行 [workdir:string:opt（默认 session workspace）] 的名称与类型按目录行实现

场景: bash 参数 workdir 约束
  假设 工具 bash 已注册
  当 校验 schema 时
  那么 参数行 [workdir:string:opt（默认 session workspace）] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: bash 参数 run_in_background 形状
  假设 工具 bash 已注册
  当 校验 schema 时
  那么 参数行 [run_in_background:boolean:opt（无 job registry 或 enableRunInBackground:false 时不注册）] 的名称与类型按目录行实现

场景: bash 参数 run_in_background 约束
  假设 工具 bash 已注册
  当 校验 schema 时
  那么 参数行 [run_in_background:boolean:opt（无 job registry 或 enableRunInBackground:false 时不注册）] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: bash 行为 1
  假设 工具 bash 执行时
  当 达到触发条件时
  那么 每次调用全新 shell，勿用 cd 传 workdir；DSH_* 暴露 harness 事实；长输出截尾、完整输出落盘报路径；删除/移动前校验解析后绝对路径；未设变量展开为空串用 ${VAR:?} 防护；沙箱拒绝报 [sandbox: file access denied under <mode

场景: pwsh 调度与呈现
  假设 合约要求的行为已实现
  当 读取工具 pwsh 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 terminal

场景: pwsh 参数越界拒绝
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

场景: pwsh 说明 section 注册
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 pwsh 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

场景: pwsh 行为 1
  假设 工具 pwsh 执行时
  当 达到触发条件时
  那么 pwsh -Command 方言，每次新进程无持久 PTY；C:\ 原生路径与 $env:NAME；Windows 强杀 [exit code: 1] 视为中断；勿赋值 $HOME 等自动变量；mirror bash 但无沙箱控制

场景: persistent-bash 调度与呈现
  假设 合约要求的行为已实现
  当 读取工具 persistent bash 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 terminal

场景: persistent-bash 参数越界拒绝
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

场景: persistent-bash 说明 section 注册
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 persistent bash 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

场景: persistent-bash 参数 command 形状
  假设 工具 persistent bash 已注册
  当 校验 schema 时
  那么 参数行 [command:string:req] 的名称与类型按目录行实现

场景: persistent-bash 参数 command 约束
  假设 工具 persistent bash 已注册
  当 校验 schema 时
  那么 参数行 [command:string:req] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: persistent-bash 行为 1
  假设 工具 persistent bash 执行时
  当 达到触发条件时
  那么 owner 隔离持久 PTY，cwd/env 跨调用保持；config backendType 默认 shell、timeoutMs 300000、maxOutputChars 16000

场景: persistent-pwsh 调度与呈现
  假设 合约要求的行为已实现
  当 读取工具 persistent pwsh 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 terminal

场景: persistent-pwsh 参数越界拒绝
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

场景: persistent-pwsh 说明 section 注册
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 persistent pwsh 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

场景: read 调度与呈现
  假设 合约要求的行为已实现
  当 读取工具 read 的声明时
  那么 executionMode 为 parallel（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 read

场景: read 参数越界拒绝
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

场景: read 说明 section 注册
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 read 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

场景: read 参数 file_path 形状
  假设 工具 read 已注册
  当 校验 schema 时
  那么 参数行 [file_path:string:req] 的名称与类型按目录行实现

场景: read 参数 file_path 约束
  假设 工具 read 已注册
  当 校验 schema 时
  那么 参数行 [file_path:string:req] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: read 参数 offset 形状
  假设 工具 read 已注册
  当 校验 schema 时
  那么 参数行 [offset:number:opt:1（1-based 起始行）] 的名称与类型按目录行实现

场景: read 参数 offset 约束
  假设 工具 read 已注册
  当 校验 schema 时
  那么 参数行 [offset:number:opt:1（1-based 起始行）] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: read 参数 limit 形状
  假设 工具 read 已注册
  当 校验 schema 时
  那么 参数行 [limit:number:opt:2000（最大行数）] 的名称与类型按目录行实现

场景: read 参数 limit 约束
  假设 工具 read 已注册
  当 校验 schema 时
  那么 参数行 [limit:number:opt:2000（最大行数）] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: read 行为 1
  假设 工具 read 执行时
  当 达到触发条件时
  那么 返回行号内容；config readLimit（默认兼上限）/readMaxLineLength/readMaxBytes/readStreamMinSize（超限转流）；presence/absence 发 fs/observed

场景: read_image 调度与呈现
  假设 合约要求的行为已实现
  当 读取工具 read_image 的声明时
  那么 executionMode 为 parallel（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 generic

场景: read_image 参数越界拒绝
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

场景: read_image 说明 section 注册
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 read_image 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

场景: read_image 参数 file_path 形状
  假设 工具 read_image 已注册
  当 校验 schema 时
  那么 参数行 [file_path:string:req] 的名称与类型按目录行实现

场景: read_image 参数 file_path 约束
  假设 工具 read_image 已注册
  当 校验 schema 时
  那么 参数行 [file_path:string:req] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: read_image 行为 1
  假设 工具 read_image 执行时
  当 达到触发条件时
  那么 PNG/JPEG/WebP/GIF 返回图像本体，大图自动缩；无 ctx.attachments 不注册；仅精确路由模型声明 image input 才执行否则拒绝

场景: write 调度与呈现
  假设 合约要求的行为已实现
  当 读取工具 write 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 diff

场景: write 参数越界拒绝
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

场景: write 说明 section 注册
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 write 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

场景: write 参数 file_path 形状
  假设 工具 write 已注册
  当 校验 schema 时
  那么 参数行 [file_path:string:req（先于 content）] 的名称与类型按目录行实现

场景: write 参数 file_path 约束
  假设 工具 write 已注册
  当 校验 schema 时
  那么 参数行 [file_path:string:req（先于 content）] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: write 参数 content 形状
  假设 工具 write 已注册
  当 校验 schema 时
  那么 参数行 [content:string:req（完整 UTF-8）] 的名称与类型按目录行实现

场景: write 参数 content 约束
  假设 工具 write 已注册
  当 校验 schema 时
  那么 参数行 [content:string:req（完整 UTF-8）] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: write 行为 1
  假设 工具 write 执行时
  当 达到触发条件时
  那么 创建或整文件替换；发 fs/write-intent；read-before-write 由 fs-observation-policy 施加

场景: edit 调度与呈现
  假设 合约要求的行为已实现
  当 读取工具 edit 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 diff

场景: edit 参数越界拒绝
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

场景: edit 说明 section 注册
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 edit 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

场景: edit 参数 file_path 形状
  假设 工具 edit 已注册
  当 校验 schema 时
  那么 参数行 [file_path:string:req（先于 old/new）] 的名称与类型按目录行实现

场景: edit 参数 file_path 约束
  假设 工具 edit 已注册
  当 校验 schema 时
  那么 参数行 [file_path:string:req（先于 old/new）] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: edit 参数 old_string 形状
  假设 工具 edit 已注册
  当 校验 schema 时
  那么 参数行 [old_string:string:req] 的名称与类型按目录行实现

场景: edit 参数 old_string 约束
  假设 工具 edit 已注册
  当 校验 schema 时
  那么 参数行 [old_string:string:req] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: edit 参数 new_string 形状
  假设 工具 edit 已注册
  当 校验 schema 时
  那么 参数行 [new_string:string:req（空串即删除）] 的名称与类型按目录行实现

场景: edit 参数 new_string 约束
  假设 工具 edit 已注册
  当 校验 schema 时
  那么 参数行 [new_string:string:req（空串即删除）] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: edit 参数 replace_all 形状
  假设 工具 edit 已注册
  当 校验 schema 时
  那么 参数行 [replace_all:boolean:opt:false（false 时 old_string 须恰好出现一次）] 的名称与类型按目录行实现

场景: edit 参数 replace_all 约束
  假设 工具 edit 已注册
  当 校验 schema 时
  那么 参数行 [replace_all:boolean:opt:false（false 时 old_string 须恰好出现一次）] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: edit 行为 1
  假设 工具 edit 执行时
  当 达到触发条件时
  那么 UTF-8 字面替换；发 fs/edit-intent；read-before-write 同 write

场景: glob 调度与呈现
  假设 合约要求的行为已实现
  当 读取工具 glob 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 search

场景: glob 参数越界拒绝
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

场景: glob 说明 section 注册
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 glob 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

场景: glob 参数 pattern 形状
  假设 工具 glob 已注册
  当 校验 schema 时
  那么 参数行 [pattern:string:req（无分隔符按任意深度 basename 匹配，带分隔符锚定深度）] 的名称与类型按目录行实现

场景: glob 参数 pattern 约束
  假设 工具 glob 已注册
  当 校验 schema 时
  那么 参数行 [pattern:string:req（无分隔符按任意深度 basename 匹配，带分隔符锚定深度）] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: glob 参数 path 形状
  假设 工具 glob 已注册
  当 校验 schema 时
  那么 参数行 [path:string:opt（默认 session workspace）] 的名称与类型按目录行实现

场景: glob 参数 path 约束
  假设 工具 glob 已注册
  当 校验 schema 时
  那么 参数行 [path:string:opt（默认 session workspace）] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: glob 行为 1
  假设 工具 glob 执行时
  当 达到触发条件时
  那么 打包 ripgrep 经 ctx.subprocess 前台运行（永不后台 job、无 host rg 依赖、无 shell 层）；返回 ≤100 路径按 mtime 序；超限按 sampleOverCapGlobResults 顶层采样并报完整列表保存位置（spill 落盘）

场景: grep 调度与呈现
  假设 合约要求的行为已实现
  当 读取工具 grep 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 search

场景: grep 参数越界拒绝
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

场景: grep 说明 section 注册
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 grep 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

场景: grep 参数 pattern 形状
  假设 工具 grep 已注册
  当 校验 schema 时
  那么 参数行 [pattern:string:req（ripgrep 正则）] 的名称与类型按目录行实现

场景: grep 参数 pattern 约束
  假设 工具 grep 已注册
  当 校验 schema 时
  那么 参数行 [pattern:string:req（ripgrep 正则）] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: grep 参数 path 形状
  假设 工具 grep 已注册
  当 校验 schema 时
  那么 参数行 [path:string:opt] 的名称与类型按目录行实现

场景: grep 参数 path 约束
  假设 工具 grep 已注册
  当 校验 schema 时
  那么 参数行 [path:string:opt] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: grep 参数 include 形状
  假设 工具 grep 已注册
  当 校验 schema 时
  那么 参数行 [include:string:opt（单 glob，不支持否定）] 的名称与类型按目录行实现

场景: grep 参数 include 约束
  假设 工具 grep 已注册
  当 校验 schema 时
  那么 参数行 [include:string:opt（单 glob，不支持否定）] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: grep 行为 1
  假设 工具 grep 执行时
  当 达到触发条件时
  那么 匹配行+行号按文件分组 ≤250 匹配；超限报完整列表位置；config globMaxResults/grepMaxMatches/grepMaxLineBytes/searchMetaMaxBytes/rawOutputMaxBytes（溢出 SEARCH_RAW_OUTPUT_OVERFLOW

场景: str_replace_editor 调度与呈现
  假设 合约要求的行为已实现
  当 读取工具 str_replace_editor 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 diff(str_replace/create)+generic(view)

场景: str_replace_editor 参数越界拒绝
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

场景: str_replace_editor 说明 section 注册
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 str_replace_editor 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

场景: str_replace_editor 参数 command 形状
  假设 工具 str_replace_editor 已注册
  当 校验 schema 时
  那么 参数行 [command:string:req:enum[view|create|str_replace|insert]] 的名称与类型按目录行实现

场景: str_replace_editor 参数 command 约束
  假设 工具 str_replace_editor 已注册
  当 校验 schema 时
  那么 参数行 [command:string:req:enum[view|create|str_replace|insert]] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: str_replace_editor 参数 path 形状
  假设 工具 str_replace_editor 已注册
  当 校验 schema 时
  那么 参数行 [path:string:req（绝对路径）] 的名称与类型按目录行实现

场景: str_replace_editor 参数 path 约束
  假设 工具 str_replace_editor 已注册
  当 校验 schema 时
  那么 参数行 [path:string:req（绝对路径）] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: str_replace_editor 参数 file_text 形状
  假设 工具 str_replace_editor 已注册
  当 校验 schema 时
  那么 参数行 [file_text:string|null:opt（create 必填，已存在不可 create）] 的名称与类型按目录行实现

场景: str_replace_editor 参数 file_text 约束
  假设 工具 str_replace_editor 已注册
  当 校验 schema 时
  那么 参数行 [file_text:string|null:opt（create 必填，已存在不可 create）] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: str_replace_editor 参数 insert_line 形状
  假设 工具 str_replace_editor 已注册
  当 校验 schema 时
  那么 参数行 [insert_line:integer|null:opt（insert 必填）] 的名称与类型按目录行实现

场景: str_replace_editor 参数 insert_line 约束
  假设 工具 str_replace_editor 已注册
  当 校验 schema 时
  那么 参数行 [insert_line:integer|null:opt（insert 必填）] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: str_replace_editor 参数 new_str 形状
  假设 工具 str_replace_editor 已注册
  当 校验 schema 时
  那么 参数行 [new_str:string|null:opt（str_replace 可选=删除，insert 必填）] 的名称与类型按目录行实现

场景: str_replace_editor 参数 new_str 约束
  假设 工具 str_replace_editor 已注册
  当 校验 schema 时
  那么 参数行 [new_str:string|null:opt（str_replace 可选=删除，insert 必填）] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: str_replace_editor 参数 old_str 形状
  假设 工具 str_replace_editor 已注册
  当 校验 schema 时
  那么 参数行 [old_str:string|null:opt（str_replace 必填，须唯一匹配否则不替换）] 的名称与类型按目录行实现

场景: str_replace_editor 参数 old_str 约束
  假设 工具 str_replace_editor 已注册
  当 校验 schema 时
  那么 参数行 [old_str:string|null:opt（str_replace 必填，须唯一匹配否则不替换）] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: str_replace_editor 参数 view_range 形状
  假设 工具 str_replace_editor 已注册
  当 校验 schema 时
  那么 参数行 [view_range:array<integer>|null:opt（1-based，[start,-1] 到末尾）] 的名称与类型按目录行实现

场景: str_replace_editor 参数 view_range 约束
  假设 工具 str_replace_editor 已注册
  当 校验 schema 时
  那么 参数行 [view_range:array<integer>|null:opt（1-based，[start,-1] 到末尾）] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: str_replace_editor 行为 1
  假设 工具 str_replace_editor 执行时
  当 达到触发条件时
  那么 view=cat -n；目录 view 列非隐藏两层；长输出截断标 <response clipped>；未用命令的 null 占位视为省略

场景: web_search 调度与呈现
  假设 合约要求的行为已实现
  当 读取工具 web_search 的声明时
  那么 executionMode 为 parallel（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 search+web

场景: web_search 参数越界拒绝
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

场景: web_search 说明 section 注册
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 web_search 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

场景: web_search 参数 queries 形状
  假设 工具 web_search 已注册
  当 校验 schema 时
  那么 参数行 [queries:array<string>:req（1-4 条，结果合并）] 的名称与类型按目录行实现

场景: web_search 参数 queries 约束
  假设 工具 web_search 已注册
  当 校验 schema 时
  那么 参数行 [queries:array<string>:req（1-4 条，结果合并）] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: web_search 行为 1
  假设 工具 web_search 执行时
  当 达到触发条件时
  那么 可选摘要答案+源 URL 列表；config searchTimeoutMs 30000、searchMaxQueries/searchMaxResults 上限；后端 deepseek/exa/perplexity

场景: web_fetch 调度与呈现
  假设 合约要求的行为已实现
  当 读取工具 web_fetch 的声明时
  那么 executionMode 为 parallel（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 fetch+web

场景: web_fetch 参数越界拒绝
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

场景: web_fetch 说明 section 注册
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 web_fetch 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

场景: web_fetch 参数 url 形状
  假设 工具 web_fetch 已注册
  当 校验 schema 时
  那么 参数行 [url:string:req（HTTP(S)）] 的名称与类型按目录行实现

场景: web_fetch 参数 url 约束
  假设 工具 web_fetch 已注册
  当 校验 schema 时
  那么 参数行 [url:string:req（HTTP(S)）] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: web_fetch 行为 1
  假设 工具 web_fetch 执行时
  当 达到触发条件时
  那么 解码为文本；config fetchTimeoutMs 30000、fetchMaxOutputChars 200000、search/fetch 注册开关默认 true

场景: todo_write 调度与呈现
  假设 合约要求的行为已实现
  当 读取工具 todo_write 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 other

场景: todo_write 参数越界拒绝
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

场景: todo_write 说明 section 注册
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 todo_write 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

场景: todo_write 参数 todos 形状
  假设 工具 todo_write 已注册
  当 校验 schema 时
  那么 参数行 [todos:array<object>:req（完整表替换）] 的名称与类型按目录行实现

场景: todo_write 参数 todos 约束
  假设 工具 todo_write 已注册
  当 校验 schema 时
  那么 参数行 [todos:array<object>:req（完整表替换）] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: todo_write 参数 todos[].content 形状
  假设 工具 todo_write 已注册
  当 校验 schema 时
  那么 参数行 [todos[].content:string:req] 的名称与类型按目录行实现

场景: todo_write 参数 todos[].content 约束
  假设 工具 todo_write 已注册
  当 校验 schema 时
  那么 参数行 [todos[].content:string:req] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: todo_write 参数 todos[].status 形状
  假设 工具 todo_write 已注册
  当 校验 schema 时
  那么 参数行 [todos[].status:string:req:enum[pending|in_progress|completed]] 的名称与类型按目录行实现

场景: todo_write 参数 todos[].status 约束
  假设 工具 todo_write 已注册
  当 校验 schema 时
  那么 参数行 [todos[].status:string:req:enum[pending|in_progress|completed]] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: todo_write 行为 1
  假设 工具 todo_write 执行时
  当 达到触发条件时
  那么 session 持有状态；并行工作可多个 in_progress（allowParallelInProgress:true），false 则恰一活跃且多标拒绝

场景: create_goal 调度与呈现
  假设 合约要求的行为已实现
  当 读取工具 create_goal 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 other

场景: create_goal 参数越界拒绝
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

场景: create_goal 说明 section 注册
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 create_goal 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

场景: create_goal 参数 objective 形状
  假设 工具 create_goal 已注册
  当 校验 schema 时
  那么 参数行 [objective:string:req（直接人类请求推断的长时目标）] 的名称与类型按目录行实现

场景: create_goal 参数 objective 约束
  假设 工具 create_goal 已注册
  当 校验 schema 时
  那么 参数行 [objective:string:req（直接人类请求推断的长时目标）] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: create_goal 参数 max_goal_rounds 形状
  假设 工具 create_goal 已注册
  当 校验 schema 时
  那么 参数行 [max_goal_rounds:number:opt（正安全整数）] 的名称与类型按目录行实现

场景: create_goal 参数 max_goal_rounds 约束
  假设 工具 create_goal 已注册
  当 校验 schema 时
  那么 参数行 [max_goal_rounds:number:opt（正安全整数）] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: create_goal 行为 1
  假设 工具 create_goal 执行时
  当 达到触发条件时
  那么 跨自动续轮；仅长时目标

场景: get_goal 调度与呈现
  假设 合约要求的行为已实现
  当 读取工具 get_goal 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 read

场景: get_goal 参数越界拒绝
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

场景: get_goal 说明 section 注册
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 get_goal 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

场景: get_goal 行为 1
  假设 工具 get_goal 执行时
  当 达到触发条件时
  那么 读当前 goal 含 id/revision

场景: update_goal 调度与呈现
  假设 合约要求的行为已实现
  当 读取工具 update_goal 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 generic

场景: update_goal 参数越界拒绝
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

场景: update_goal 说明 section 注册
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 update_goal 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

场景: update_goal 参数 goal_id 形状
  假设 工具 update_goal 已注册
  当 校验 schema 时
  那么 参数行 [goal_id:string:req] 的名称与类型按目录行实现

场景: update_goal 参数 goal_id 约束
  假设 工具 update_goal 已注册
  当 校验 schema 时
  那么 参数行 [goal_id:string:req] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: update_goal 参数 revision 形状
  假设 工具 update_goal 已注册
  当 校验 schema 时
  那么 参数行 [revision:number:req（精确正 revision）] 的名称与类型按目录行实现

场景: update_goal 参数 revision 约束
  假设 工具 update_goal 已注册
  当 校验 schema 时
  那么 参数行 [revision:number:req（精确正 revision）] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: update_goal 参数 action 形状
  假设 工具 update_goal 已注册
  当 校验 schema 时
  那么 参数行 [action:string:req:enum[edit|pause|resume|complete|blocked]] 的名称与类型按目录行实现

场景: update_goal 参数 action 约束
  假设 工具 update_goal 已注册
  当 校验 schema 时
  那么 参数行 [action:string:req:enum[edit|pause|resume|complete|blocked]] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: update_goal 参数 objective 形状
  假设 工具 update_goal 已注册
  当 校验 schema 时
  那么 参数行 [objective:string:opt（仅 edit）] 的名称与类型按目录行实现

场景: update_goal 参数 objective 约束
  假设 工具 update_goal 已注册
  当 校验 schema 时
  那么 参数行 [objective:string:opt（仅 edit）] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: update_goal 参数 max_goal_rounds 形状
  假设 工具 update_goal 已注册
  当 校验 schema 时
  那么 参数行 [max_goal_rounds:number:opt（仅 edit）] 的名称与类型按目录行实现

场景: update_goal 参数 max_goal_rounds 约束
  假设 工具 update_goal 已注册
  当 校验 schema 时
  那么 参数行 [max_goal_rounds:number:opt（仅 edit）] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: update_goal 参数 blocked_reason 形状
  假设 工具 update_goal 已注册
  当 校验 schema 时
  那么 参数行 [blocked_reason:string:opt（仅 blocked 必填）] 的名称与类型按目录行实现

场景: update_goal 参数 blocked_reason 约束
  假设 工具 update_goal 已注册
  当 校验 schema 时
  那么 参数行 [blocked_reason:string:opt（仅 blocked 必填）] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: update_goal 行为 1
  假设 工具 update_goal 执行时
  当 达到触发条件时
  那么 edit/pause/resume 需直接人类 root 权威；blocked 在最小轮前被拒（blockedAfterConsecutiveRounds 默认 3）

场景: job_kill 调度与呈现
  假设 合约要求的行为已实现
  当 读取工具 job_kill 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 execute

场景: job_kill 参数越界拒绝
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

场景: job_kill 说明 section 注册
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 job_kill 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

场景: job_kill 参数 job_id 形状
  假设 工具 job_kill 已注册
  当 校验 schema 时
  那么 参数行 [job_id:string:req] 的名称与类型按目录行实现

场景: job_kill 参数 job_id 约束
  假设 工具 job_kill 已注册
  当 校验 schema 时
  那么 参数行 [job_id:string:req] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: job_kill 参数 reason 形状
  假设 工具 job_kill 已注册
  当 校验 schema 时
  那么 参数行 [reason:string:opt（入日志并转发）] 的名称与类型按目录行实现

场景: job_kill 参数 reason 约束
  假设 工具 job_kill 已注册
  当 校验 schema 时
  那么 参数行 [reason:string:opt（入日志并转发）] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: job_list 调度与呈现
  假设 合约要求的行为已实现
  当 读取工具 job_list 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 read

场景: job_list 参数越界拒绝
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

场景: job_list 说明 section 注册
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 job_list 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

场景: job_list 行为 1
  假设 工具 job_list 执行时
  当 达到触发条件时
  那么 列 running+finished 的 id/kind/status

场景: job_output 调度与呈现
  假设 合约要求的行为已实现
  当 读取工具 job_output 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 read

场景: job_output 参数越界拒绝
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

场景: job_output 说明 section 注册
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 job_output 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

场景: job_output 参数 job_id 形状
  假设 工具 job_output 已注册
  当 校验 schema 时
  那么 参数行 [job_id:string:req] 的名称与类型按目录行实现

场景: job_output 参数 job_id 约束
  假设 工具 job_output 已注册
  当 校验 schema 时
  那么 参数行 [job_id:string:req] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: job_output 参数 wait 形状
  假设 工具 job_output 已注册
  当 校验 schema 时
  那么 参数行 [wait:boolean:opt:false] 的名称与类型按目录行实现

场景: job_output 参数 wait 约束
  假设 工具 job_output 已注册
  当 校验 schema 时
  那么 参数行 [wait:boolean:opt:false] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: job_output 参数 timeout_ms 形状
  假设 工具 job_output 已注册
  当 校验 schema 时
  那么 参数行 [timeout_ms:number:opt（默认 30s，钳至 maxWaitTimeoutMs 10min）] 的名称与类型按目录行实现

场景: job_output 参数 timeout_ms 约束
  假设 工具 job_output 已注册
  当 校验 schema 时
  那么 参数行 [timeout_ms:number:opt（默认 30s，钳至 maxWaitTimeoutMs 10min）] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: job_output 行为 1
  假设 工具 job_output 执行时
  当 达到触发条件时
  那么 流式增量读取；wait 超时留 job 运行

场景: schedule_create 调度与呈现
  假设 合约要求的行为已实现
  当 读取工具 schedule_create 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 other

场景: schedule_create 参数越界拒绝
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

场景: schedule_create 说明 section 注册
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 schedule_create 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

场景: schedule_create 参数 prompt 形状
  假设 工具 schedule_create 已注册
  当 校验 schema 时
  那么 参数行 [prompt:string:req] 的名称与类型按目录行实现

场景: schedule_create 参数 prompt 约束
  假设 工具 schedule_create 已注册
  当 校验 schema 时
  那么 参数行 [prompt:string:req] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: schedule_create 参数 title 形状
  假设 工具 schedule_create 已注册
  当 校验 schema 时
  那么 参数行 [title:string:req（≤120 字符规则）] 的名称与类型按目录行实现

场景: schedule_create 参数 title 约束
  假设 工具 schedule_create 已注册
  当 校验 schema 时
  那么 参数行 [title:string:req（≤120 字符规则）] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: schedule_create 行为 1
  假设 工具 schedule_create 执行时
  当 达到触发条件时
  那么 时区 UTC 或 IANA；不存在本地时刻跳过、重复取较早；宕机后循环提醒只补最近一次；管理走 storage domain

场景: schedule_delete 调度与呈现
  假设 合约要求的行为已实现
  当 读取工具 schedule_delete 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 generic

场景: schedule_delete 参数越界拒绝
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

场景: schedule_delete 说明 section 注册
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 schedule_delete 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

场景: schedule_delete 参数 id 形状
  假设 工具 schedule_delete 已注册
  当 校验 schema 时
  那么 参数行 [id:string:req] 的名称与类型按目录行实现

场景: schedule_delete 参数 id 约束
  假设 工具 schedule_delete 已注册
  当 校验 schema 时
  那么 参数行 [id:string:req] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: schedule_delete 行为 1
  假设 工具 schedule_delete 执行时
  当 达到触发条件时
  那么 活动/非活动均可删，不撤回已入队

场景: schedule_list 调度与呈现
  假设 合约要求的行为已实现
  当 读取工具 schedule_list 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 read

场景: schedule_list 参数越界拒绝
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

场景: schedule_list 说明 section 注册
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 schedule_list 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

场景: schedule_list 行为 1
  假设 工具 schedule_list 执行时
  当 达到触发条件时
  那么 列当前 session 活动提醒

场景: schedule_update 调度与呈现
  假设 合约要求的行为已实现
  当 读取工具 schedule_update 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 generic

场景: schedule_update 参数越界拒绝
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

场景: schedule_update 说明 section 注册
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 schedule_update 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

场景: schedule_update 参数 id 形状
  假设 工具 schedule_update 已注册
  当 校验 schema 时
  那么 参数行 [id:string:req] 的名称与类型按目录行实现

场景: schedule_update 参数 id 约束
  假设 工具 schedule_update 已注册
  当 校验 schema 时
  那么 参数行 [id:string:req] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: schedule_update 行为 1
  假设 工具 schedule_update 执行时
  当 达到触发条件时
  那么 原地改保留 id；改相对延迟须新建；every_seconds 改动按保存时刻重对齐

场景: ask_user_question 调度与呈现
  假设 合约要求的行为已实现
  当 读取工具 ask_user_question 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 generic

场景: ask_user_question 参数越界拒绝
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

场景: ask_user_question 说明 section 注册
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 ask_user_question 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

场景: ask_user_question 参数 questions 形状
  假设 工具 ask_user_question 已注册
  当 校验 schema 时
  那么 参数行 [questions:array<object>:req（[].id:req 稳定 id、[].question:req、[].header:opt、[].options:opt（label 必填+description；推荐首位加 (Recommended)）、[].multi_select:opt:false）] 的名称与类型按目录行实现

场景: ask_user_question 参数 questions 约束
  假设 工具 ask_user_question 已注册
  当 校验 schema 时
  那么 参数行 [questions:array<object>:req（[].id:req 稳定 id、[].question:req、[].header:opt、[].options:opt（label 必填+description；推荐首位加 (Recommended)）、[].multi_select:opt:false）] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: ask_user_question 行为 1
  假设 工具 ask_user_question 执行时
  当 达到触发条件时
  那么 默认阻塞等回答；config mode:timed 启用超时+pending result（timeout 默认 120s，-1 无限阻塞）

场景: exit_plan_mode 调度与呈现
  假设 合约要求的行为已实现
  当 读取工具 exit_plan_mode 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 generic

场景: exit_plan_mode 参数越界拒绝
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

场景: exit_plan_mode 说明 section 注册
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 exit_plan_mode 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

场景: exit_plan_mode 参数 plan 形状
  假设 工具 exit_plan_mode 已注册
  当 校验 schema 时
  那么 参数行 [plan:string:req（完整 markdown，# 标题开头）] 的名称与类型按目录行实现

场景: exit_plan_mode 参数 plan 约束
  假设 工具 exit_plan_mode 已注册
  当 校验 schema 时
  那么 参数行 [plan:string:req（完整 markdown，# 标题开头）] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: exit_plan_mode 行为 1
  假设 工具 exit_plan_mode 执行时
  当 达到触发条件时
  那么 schema 常驻防 churn；plan 外 execute 拒绝；plan 内经 user-questions 呈批 approve/继续规划；批准在 step 边界记录 inactive

场景: run_code 调度与呈现
  假设 合约要求的行为已实现
  当 读取工具 run_code 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 generic（PTC）

场景: run_code 参数越界拒绝
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

场景: run_code 说明 section 注册
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 run_code 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

场景: run_code 参数 code 形状
  假设 工具 run_code 已注册
  当 校验 schema 时
  那么 参数行 [code:string:req（TypeScript 程序）] 的名称与类型按目录行实现

场景: run_code 参数 code 约束
  假设 工具 run_code 已注册
  当 校验 schema 时
  那么 参数行 [code:string:req（TypeScript 程序）] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: run_code 参数 description 形状
  假设 工具 run_code 已注册
  当 校验 schema 时
  那么 参数行 [description:string:req（5-10 词）] 的名称与类型按目录行实现

场景: run_code 参数 description 约束
  假设 工具 run_code 已注册
  当 校验 schema 时
  那么 参数行 [description:string:req（5-10 词）] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: run_code 参数 timeoutMs 形状
  假设 工具 run_code 已注册
  当 校验 schema 时
  那么 参数行 [timeoutMs:number:opt（正数，受部署上限封顶）] 的名称与类型按目录行实现

场景: run_code 参数 timeoutMs 约束
  假设 工具 run_code 已注册
  当 校验 schema 时
  那么 参数行 [timeoutMs:number:opt（正数，受部署上限封顶）] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: run_code 参数 sandbox_permissions 形状
  假设 工具 run_code 已注册
  当 校验 schema 时
  那么 参数行 [sandbox_permissions:string:opt:enum[workspace-write|danger-full-access]] 的名称与类型按目录行实现

场景: run_code 参数 sandbox_permissions 约束
  假设 工具 run_code 已注册
  当 校验 schema 时
  那么 参数行 [sandbox_permissions:string:opt:enum[workspace-write|danger-full-access]] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: run_code 参数 justification 形状
  假设 工具 run_code 已注册
  当 校验 schema 时
  那么 参数行 [justification:string:opt（用户当前请求语言）] 的名称与类型按目录行实现

场景: run_code 参数 justification 约束
  假设 工具 run_code 已注册
  当 校验 schema 时
  那么 参数行 [justification:string:opt（用户当前请求语言）] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: run_code 行为 1
  假设 工具 run_code 执行时
  当 达到触发条件时
  那么 mode:ptc 保留传输不可过滤；子调用经完整 guarded pipeline；并发安全子调用重叠至 maxParallelSubCalls（默认 10，1=串行）；图片型子工具结果 run 后附加

场景: session_search 调度与呈现
  假设 合约要求的行为已实现
  当 读取工具 session_search 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 search

场景: session_search 参数越界拒绝
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

场景: session_search 说明 section 注册
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 session_search 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

场景: session_search 参数 query 形状
  假设 工具 session_search 已注册
  当 校验 schema 时
  那么 参数行 [query:string:req] 的名称与类型按目录行实现

场景: session_search 参数 query 约束
  假设 工具 session_search 已注册
  当 校验 schema 时
  那么 参数行 [query:string:req] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: session_search 参数 availability 形状
  假设 工具 session_search 已注册
  当 校验 schema 时
  那么 参数行 [availability:enum[live|persisted]:opt（至少一选）] 的名称与类型按目录行实现

场景: session_search 参数 availability 约束
  假设 工具 session_search 已注册
  当 校验 schema 时
  那么 参数行 [availability:enum[live|persisted]:opt（至少一选）] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: session_search 行为 1
  假设 工具 session_search 执行时
  当 达到触发条件时
  那么 跨 workspace 搜先前会话每会话回最强匹配；config maxSearchResults 100、searchTimeoutMs 30000

场景: session_trace 调度与呈现
  假设 合约要求的行为已实现
  当 读取工具 session_trace 的声明时
  那么 executionMode 为 parallel（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 generic

场景: session_trace 参数越界拒绝
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

场景: session_trace 说明 section 注册
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 session_trace 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

场景: session_trace 参数 session_id 形状
  假设 工具 session_trace 已注册
  当 校验 schema 时
  那么 参数行 [session_id:opt（省略当前）] 的名称与类型按目录行实现

场景: session_trace 参数 session_id 约束
  假设 工具 session_trace 已注册
  当 校验 schema 时
  那么 参数行 [session_id:opt（省略当前）] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: session_trace 行为 1
  假设 工具 session_trace 执行时
  当 达到触发条件时
  那么 读授权谱系

场景: session_event_read 调度与呈现
  假设 合约要求的行为已实现
  当 读取工具 session_event_read 的声明时
  那么 executionMode 为 parallel（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 generic

场景: session_event_read 参数越界拒绝
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

场景: session_event_read 说明 section 注册
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 session_event_read 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

场景: session_event_read 参数 seq 形状
  假设 工具 session_event_read 已注册
  当 校验 schema 时
  那么 参数行 [seq:integer:req] 的名称与类型按目录行实现

场景: session_event_read 参数 seq 约束
  假设 工具 session_event_read 已注册
  当 校验 schema 时
  那么 参数行 [seq:integer:req] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: session_event_read 参数 session_id 形状
  假设 工具 session_event_read 已注册
  当 校验 schema 时
  那么 参数行 [session_id:opt] 的名称与类型按目录行实现

场景: session_event_read 参数 session_id 约束
  假设 工具 session_event_read 已注册
  当 校验 schema 时
  那么 参数行 [session_id:opt] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: session_event_search 调度与呈现
  假设 合约要求的行为已实现
  当 读取工具 session_event_search 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 search

场景: session_event_search 参数越界拒绝
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

场景: session_event_search 说明 section 注册
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 session_event_search 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

场景: session_event_search 参数 query 形状
  假设 工具 session_event_search 已注册
  当 校验 schema 时
  那么 参数行 [query:string:req] 的名称与类型按目录行实现

场景: session_event_search 参数 query 约束
  假设 工具 session_event_search 已注册
  当 校验 schema 时
  那么 参数行 [query:string:req] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: session_event_search 行为 1
  假设 工具 session_event_search 执行时
  当 达到触发条件时
  那么 排除执行本调用的 step

场景: session_event_trace 调度与呈现
  假设 合约要求的行为已实现
  当 读取工具 session_event_trace 的声明时
  那么 executionMode 为 parallel（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 generic

场景: session_event_trace 参数越界拒绝
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

场景: session_event_trace 说明 section 注册
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 session_event_trace 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

场景: session_event_trace 参数 seq 形状
  假设 工具 session_event_trace 已注册
  当 校验 schema 时
  那么 参数行 [seq:integer:req] 的名称与类型按目录行实现

场景: session_event_trace 参数 seq 约束
  假设 工具 session_event_trace 已注册
  当 校验 schema 时
  那么 参数行 [seq:integer:req] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: session_event_trace 参数 session_id 形状
  假设 工具 session_event_trace 已注册
  当 校验 schema 时
  那么 参数行 [session_id:opt] 的名称与类型按目录行实现

场景: session_event_trace 参数 session_id 约束
  假设 工具 session_event_trace 已注册
  当 校验 schema 时
  那么 参数行 [session_id:opt] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: session_event_trace 行为 1
  假设 工具 session_event_trace 执行时
  当 达到触发条件时
  那么 读直接替换与关系

场景: terminal_open 调度与呈现
  假设 合约要求的行为已实现
  当 读取工具 terminal_open 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 execute

场景: terminal_open 参数越界拒绝
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

场景: terminal_open 说明 section 注册
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 terminal_open 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

场景: terminal_open 参数 type 形状
  假设 工具 terminal_open 已注册
  当 校验 schema 时
  那么 参数行 [type:string:req（通常 shell）] 的名称与类型按目录行实现

场景: terminal_open 参数 type 约束
  假设 工具 terminal_open 已注册
  当 校验 schema 时
  那么 参数行 [type:string:req（通常 shell）] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: terminal_open 参数 name 形状
  假设 工具 terminal_open 已注册
  当 校验 schema 时
  那么 参数行 [name:opt] 的名称与类型按目录行实现

场景: terminal_open 参数 name 约束
  假设 工具 terminal_open 已注册
  当 校验 schema 时
  那么 参数行 [name:opt] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: terminal_open 参数 cwd 形状
  假设 工具 terminal_open 已注册
  当 校验 schema 时
  那么 参数行 [cwd:opt（非绝对按 Session 目录解析）] 的名称与类型按目录行实现

场景: terminal_open 参数 cwd 约束
  假设 工具 terminal_open 已注册
  当 校验 schema 时
  那么 参数行 [cwd:opt（非绝对按 Session 目录解析）] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: terminal_read 调度与呈现
  假设 合约要求的行为已实现
  当 读取工具 terminal_read 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 generic

场景: terminal_read 参数越界拒绝
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

场景: terminal_read 说明 section 注册
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 terminal_read 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

场景: terminal_read 参数 sessionId 形状
  假设 工具 terminal_read 已注册
  当 校验 schema 时
  那么 参数行 [sessionId:req] 的名称与类型按目录行实现

场景: terminal_read 参数 sessionId 约束
  假设 工具 terminal_read 已注册
  当 校验 schema 时
  那么 参数行 [sessionId:req] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: terminal_read 参数 offset 形状
  假设 工具 terminal_read 已注册
  当 校验 schema 时
  那么 参数行 [offset:opt:0] 的名称与类型按目录行实现

场景: terminal_read 参数 offset 约束
  假设 工具 terminal_read 已注册
  当 校验 schema 时
  那么 参数行 [offset:opt:0] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: terminal_read 参数 count 形状
  假设 工具 terminal_read 已注册
  当 校验 schema 时
  那么 参数行 [count:opt:500（后端封顶）] 的名称与类型按目录行实现

场景: terminal_read 参数 count 约束
  假设 工具 terminal_read 已注册
  当 校验 schema 时
  那么 参数行 [count:opt:500（后端封顶）] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: terminal_send 调度与呈现
  假设 合约要求的行为已实现
  当 读取工具 terminal_send 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 generic

场景: terminal_send 参数越界拒绝
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

场景: terminal_send 说明 section 注册
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 terminal_send 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

场景: terminal_send 参数 sessionId 形状
  假设 工具 terminal_send 已注册
  当 校验 schema 时
  那么 参数行 [sessionId:req] 的名称与类型按目录行实现

场景: terminal_send 参数 sessionId 约束
  假设 工具 terminal_send 已注册
  当 校验 schema 时
  那么 参数行 [sessionId:req] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: terminal_send 参数 text 形状
  假设 工具 terminal_send 已注册
  当 校验 schema 时
  那么 参数行 [text:string:req] 的名称与类型按目录行实现

场景: terminal_send 参数 text 约束
  假设 工具 terminal_send 已注册
  当 校验 schema 时
  那么 参数行 [text:string:req] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: terminal_send 参数 submit 形状
  假设 工具 terminal_send 已注册
  当 校验 schema 时
  那么 参数行 [submit:boolean:opt:true] 的名称与类型按目录行实现

场景: terminal_send 参数 submit 约束
  假设 工具 terminal_send 已注册
  当 校验 schema 时
  那么 参数行 [submit:boolean:opt:true] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: terminal_send 参数 run_in_background 形状
  假设 工具 terminal_send 已注册
  当 校验 schema 时
  那么 参数行 [run_in_background:opt（立即返回 job id）] 的名称与类型按目录行实现

场景: terminal_send 参数 run_in_background 约束
  假设 工具 terminal_send 已注册
  当 校验 schema 时
  那么 参数行 [run_in_background:opt（立即返回 job id）] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: terminal_send 行为 1
  假设 工具 terminal_send 执行时
  当 达到触发条件时
  那么 默认等 prompt/静默/超时/退出；后台经 ctx.jobs

场景: terminal_signal 调度与呈现
  假设 合约要求的行为已实现
  当 读取工具 terminal_signal 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 execute

场景: terminal_signal 参数越界拒绝
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

场景: terminal_signal 说明 section 注册
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 terminal_signal 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

场景: terminal_signal 参数 sessionId 形状
  假设 工具 terminal_signal 已注册
  当 校验 schema 时
  那么 参数行 [sessionId:req] 的名称与类型按目录行实现

场景: terminal_signal 参数 sessionId 约束
  假设 工具 terminal_signal 已注册
  当 校验 schema 时
  那么 参数行 [sessionId:req] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: terminal_signal 参数 signal 形状
  假设 工具 terminal_signal 已注册
  当 校验 schema 时
  那么 参数行 [signal:string:req:enum[SIGINT|SIGTERM|SIGKILL|SIGTSTP|SIGHUP]] 的名称与类型按目录行实现

场景: terminal_signal 参数 signal 约束
  假设 工具 terminal_signal 已注册
  当 校验 schema 时
  那么 参数行 [signal:string:req:enum[SIGINT|SIGTERM|SIGKILL|SIGTSTP|SIGHUP]] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: terminal_signal 行为 1
  假设 工具 terminal_signal 执行时
  当 达到触发条件时
  那么 shell 目标 SIGKILL 被拒用 terminal_close

场景: terminal_list 调度与呈现
  假设 合约要求的行为已实现
  当 读取工具 terminal_list 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 read

场景: terminal_list 参数越界拒绝
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

场景: terminal_list 说明 section 注册
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 terminal_list 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

场景: terminal_close 调度与呈现
  假设 合约要求的行为已实现
  当 读取工具 terminal_close 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 delete

场景: terminal_close 参数越界拒绝
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

场景: terminal_close 说明 section 注册
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 terminal_close 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

场景: terminal_close 参数 sessionId 形状
  假设 工具 terminal_close 已注册
  当 校验 schema 时
  那么 参数行 [sessionId:req] 的名称与类型按目录行实现

场景: terminal_close 参数 sessionId 约束
  假设 工具 terminal_close 已注册
  当 校验 schema 时
  那么 参数行 [sessionId:req] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: terminal_close 行为 1
  假设 工具 terminal_close 执行时
  当 达到触发条件时
  那么 等待 owned 进程树消亡

场景: skill 调度与呈现
  假设 合约要求的行为已实现
  当 读取工具 skill 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 read

场景: skill 参数越界拒绝
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

场景: skill 说明 section 注册
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 skill 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

场景: skill 参数 name 形状
  假设 工具 skill 已注册
  当 校验 schema 时
  那么 参数行 [name:string:req（精确技能名）] 的名称与类型按目录行实现

场景: skill 参数 name 约束
  假设 工具 skill 已注册
  当 校验 schema 时
  那么 参数行 [name:string:req（精确技能名）] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: load_workspace_dependencies 调度与呈现
  假设 合约要求的行为已实现
  当 读取工具 load_workspace_dependencies 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 read

场景: load_workspace_dependencies 参数越界拒绝
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

场景: load_workspace_dependencies 说明 section 注册
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 load_workspace_dependencies 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

场景: load_workspace_dependencies 行为 1
  假设 工具 load_workspace_dependencies 执行时
  当 达到触发条件时
  那么 返回捆绑 Python+库路径与版本（numpy/pandas 等）；Node/pnpm 在 payload 提供时附上

场景: lsp 调度与呈现
  假设 合约要求的行为已实现
  当 读取工具 lsp 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 generic

场景: lsp 参数越界拒绝
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

场景: lsp 说明 section 注册
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 lsp 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

场景: lsp 参数 operation 形状
  假设 工具 lsp 已注册
  当 校验 schema 时
  那么 参数行 [operation:string:req:enum[goToDefinition|findReferences|goToImplementation|hover]] 的名称与类型按目录行实现

场景: lsp 参数 operation 约束
  假设 工具 lsp 已注册
  当 校验 schema 时
  那么 参数行 [operation:string:req:enum[goToDefinition|findReferences|goToImplementation|hover]] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: lsp 行为 1
  假设 工具 lsp 执行时
  当 达到触发条件时
  那么 无 provider 返回结构化 LSP_UNAVAILABLE 不改 schema；config maxLocations 100、maxResultChars 16000、timeoutMs 60000（即 60000 归属 lsp）

场景: working_directory 调度与呈现
  假设 合约要求的行为已实现
  当 读取工具 working_directory 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 generic

场景: working_directory 参数越界拒绝
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

场景: working_directory 说明 section 注册
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 working_directory 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

场景: working_directory 参数 cd 形状
  假设 工具 working_directory 已注册
  当 校验 schema 时
  那么 参数行 [cd:string:opt（省略读当前）] 的名称与类型按目录行实现

场景: working_directory 参数 cd 约束
  假设 工具 working_directory 已注册
  当 校验 schema 时
  那么 参数行 [cd:string:opt（省略读当前）] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: working_directory 行为 1
  假设 工具 working_directory 执行时
  当 达到触发条件时
  那么 shell 与运行进程保留自身目录；origin 元数据与权限根固定

场景: cordis_inspect_list 调度与呈现
  假设 合约要求的行为已实现
  当 读取工具 cordis_inspect_list 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 generic

场景: cordis_inspect_list 参数越界拒绝
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

场景: cordis_inspect_list 说明 section 注册
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 cordis_inspect_list 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

场景: cordis_inspect_list 行为 1
  假设 工具 cordis_inspect_list 执行时
  当 达到触发条件时
  那么 列全部 Inspect Provider；写/配插件前先调用

场景: cordis_inspect_query 调度与呈现
  假设 合约要求的行为已实现
  当 读取工具 cordis_inspect_query 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 read

场景: cordis_inspect_query 参数越界拒绝
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

场景: cordis_inspect_query 说明 section 注册
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 cordis_inspect_query 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

场景: cordis_inspect_query 参数 platform 形状
  假设 工具 cordis_inspect_query 已注册
  当 校验 schema 时
  那么 参数行 [platform:string:req:enum[host|client]] 的名称与类型按目录行实现

场景: cordis_inspect_query 参数 platform 约束
  假设 工具 cordis_inspect_query 已注册
  当 校验 schema 时
  那么 参数行 [platform:string:req:enum[host|client]] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: cordis_inspect_query 参数 provider 形状
  假设 工具 cordis_inspect_query 已注册
  当 校验 schema 时
  那么 参数行 [provider:string:req（精确 ID）] 的名称与类型按目录行实现

场景: cordis_inspect_query 参数 provider 约束
  假设 工具 cordis_inspect_query 已注册
  当 校验 schema 时
  那么 参数行 [provider:string:req（精确 ID）] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: cordis_inspect_query 参数 method 形状
  假设 工具 cordis_inspect_query 已注册
  当 校验 schema 时
  那么 参数行 [method:string:req（manifest 精确方法）] 的名称与类型按目录行实现

场景: cordis_inspect_query 参数 method 约束
  假设 工具 cordis_inspect_query 已注册
  当 校验 schema 时
  那么 参数行 [method:string:req（manifest 精确方法）] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: cordis_inspect_query 参数 input 形状
  假设 工具 cordis_inspect_query 已注册
  当 校验 schema 时
  那么 参数行 [input:any:opt（满足 input schema）] 的名称与类型按目录行实现

场景: cordis_inspect_query 参数 input 约束
  假设 工具 cordis_inspect_query 已注册
  当 校验 schema 时
  那么 参数行 [input:any:opt（满足 input schema）] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: plugin_manager 调度与呈现
  假设 合约要求的行为已实现
  当 读取工具 plugin_manager 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 generic

场景: plugin_manager 参数越界拒绝
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

场景: plugin_manager 说明 section 注册
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 plugin_manager 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

场景: plugin_manager 参数 action 形状
  假设 工具 plugin_manager 已注册
  当 校验 schema 时
  那么 参数行 [action:string:req:enum[list_plugins|list_bundles|set_plugin|set_bundle|install_bundle|remove_bundle|list_version_exemptions|set_version_exemption]] 的名称与类型按目录行实现

场景: plugin_manager 参数 action 约束
  假设 工具 plugin_manager 已注册
  当 校验 schema 时
  那么 参数行 [action:string:req:enum[list_plugins|list_bundles|set_plugin|set_bundle|install_bundle|remove_bundle|list_version_exemptions|set_version_exemption]] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: plugin_manager 参数 target 形状
  假设 工具 plugin_manager 已注册
  当 校验 schema 时
  那么 参数行 [target:opt] 的名称与类型按目录行实现

场景: plugin_manager 参数 target 约束
  假设 工具 plugin_manager 已注册
  当 校验 schema 时
  那么 参数行 [target:opt] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: plugin_manager 参数 enabled 形状
  假设 工具 plugin_manager 已注册
  当 校验 schema 时
  那么 参数行 [enabled:opt（set 必填）] 的名称与类型按目录行实现

场景: plugin_manager 参数 enabled 约束
  假设 工具 plugin_manager 已注册
  当 校验 schema 时
  那么 参数行 [enabled:opt（set 必填）] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: plugin_manager 参数 runtimeVersion 形状
  假设 工具 plugin_manager 已注册
  当 校验 schema 时
  那么 参数行 [runtimeVersion:opt（精确版本）] 的名称与类型按目录行实现

场景: plugin_manager 参数 runtimeVersion 约束
  假设 工具 plugin_manager 已注册
  当 校验 schema 时
  那么 参数行 [runtimeVersion:opt（精确版本）] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: plugin_manager 参数 acceptRisk 形状
  假设 工具 plugin_manager 已注册
  当 校验 schema 时
  那么 参数行 [acceptRisk:opt] 的名称与类型按目录行实现

场景: plugin_manager 参数 acceptRisk 约束
  假设 工具 plugin_manager 已注册
  当 校验 schema 时
  那么 参数行 [acceptRisk:opt] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: plugin_manager 参数 approvedBuilds 形状
  假设 工具 plugin_manager 已注册
  当 校验 schema 时
  那么 参数行 [approvedBuilds:opt] 的名称与类型按目录行实现

场景: plugin_manager 参数 approvedBuilds 约束
  假设 工具 plugin_manager 已注册
  当 校验 schema 时
  那么 参数行 [approvedBuilds:opt] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: plugin_manager 参数 registry 形状
  假设 工具 plugin_manager 已注册
  当 校验 schema 时
  那么 参数行 [registry:opt] 的名称与类型按目录行实现

场景: plugin_manager 参数 registry 约束
  假设 工具 plugin_manager 已注册
  当 校验 schema 时
  那么 参数行 [registry:opt] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: plugin_manager 参数 offset 形状
  假设 工具 plugin_manager 已注册
  当 校验 schema 时
  那么 参数行 [offset:opt:0] 的名称与类型按目录行实现

场景: plugin_manager 参数 offset 约束
  假设 工具 plugin_manager 已注册
  当 校验 schema 时
  那么 参数行 [offset:opt:0] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: plugin_manager 参数 limit 形状
  假设 工具 plugin_manager 已注册
  当 校验 schema 时
  那么 参数行 [limit:opt:25（1..100）] 的名称与类型按目录行实现

场景: plugin_manager 参数 limit 约束
  假设 工具 plugin_manager 已注册
  当 校验 schema 时
  那么 参数行 [limit:opt:25（1..100）] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: plugin_manager 行为 1
  假设 工具 plugin_manager 执行时
  当 达到触发条件时
  那么 每动作需 danger-full-access 或本次批准；安装可执行允许的 build 脚本

场景: subagent 调度与呈现
  假设 合约要求的行为已实现
  当 读取工具 subagent 的声明时
  那么 executionMode 为 parallel（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 generic

场景: subagent 参数越界拒绝
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

场景: subagent 说明 section 注册
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 subagent 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

场景: subagent 参数 description 形状
  假设 工具 subagent 已注册
  当 校验 schema 时
  那么 参数行 [description:string:req（3-5 词）] 的名称与类型按目录行实现

场景: subagent 参数 description 约束
  假设 工具 subagent 已注册
  当 校验 schema 时
  那么 参数行 [description:string:req（3-5 词）] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: subagent 参数 prompt 形状
  假设 工具 subagent 已注册
  当 校验 schema 时
  那么 参数行 [prompt:string:req（自包含，child 不共享对话）] 的名称与类型按目录行实现

场景: subagent 参数 prompt 约束
  假设 工具 subagent 已注册
  当 校验 schema 时
  那么 参数行 [prompt:string:req（自包含，child 不共享对话）] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: subagent 参数 cwd 形状
  假设 工具 subagent 已注册
  当 校验 schema 时
  那么 参数行 [cwd:opt（相对按当前，省略继承；双方后续变更互不影响）] 的名称与类型按目录行实现

场景: subagent 参数 cwd 约束
  假设 工具 subagent 已注册
  当 校验 schema 时
  那么 参数行 [cwd:opt（相对按当前，省略继承；双方后续变更互不影响）] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: subagent 行为 1
  假设 工具 subagent 执行时
  当 达到触发条件时
  那么 立即返回 child id，完成通知；config provider 必填、maxDepth（0=禁委托或 provider-managed；省略读 Host 默认 1）、agentPreset/allowedAgentPresets（非空才暴露 agent_preset）

场景: interrupt_agent 调度与呈现
  假设 合约要求的行为已实现
  当 读取工具 interrupt_agent 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 generic

场景: interrupt_agent 参数越界拒绝
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

场景: interrupt_agent 说明 section 注册
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 interrupt_agent 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

场景: interrupt_agent 参数 agent_id 形状
  假设 工具 interrupt_agent 已注册
  当 校验 schema 时
  那么 参数行 [agent_id:string:req（直接子代或更深后代）] 的名称与类型按目录行实现

场景: interrupt_agent 参数 agent_id 约束
  假设 工具 interrupt_agent 已注册
  当 校验 schema 时
  那么 参数行 [agent_id:string:req（直接子代或更深后代）] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: interrupt_agent 行为 1
  假设 工具 interrupt_agent 执行时
  当 达到触发条件时
  那么 请求停止不等待；外部执行永久停止；其启动的 subagent 继续跑

场景: list_agents 调度与呈现
  假设 合约要求的行为已实现
  当 读取工具 list_agents 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 generic

场景: list_agents 参数越界拒绝
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

场景: list_agents 说明 section 注册
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 list_agents 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

场景: list_agents 参数 scope 形状
  假设 工具 list_agents 已注册
  当 校验 schema 时
  那么 参数行 [scope:opt:enum[children|descendants]（descendants 深>1 仅可 interrupt）] 的名称与类型按目录行实现

场景: list_agents 参数 scope 约束
  假设 工具 list_agents 已注册
  当 校验 schema 时
  那么 参数行 [scope:opt:enum[children|descendants]（descendants 深>1 仅可 interrupt）] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: send_message 调度与呈现
  假设 合约要求的行为已实现
  当 读取工具 send_message 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 generic

场景: send_message 参数越界拒绝
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

场景: send_message 说明 section 注册
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 send_message 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

场景: send_message 参数 agent_id 形状
  假设 工具 send_message 已注册
  当 校验 schema 时
  那么 参数行 [agent_id:string:req] 的名称与类型按目录行实现

场景: send_message 参数 agent_id 约束
  假设 工具 send_message 已注册
  当 校验 schema 时
  那么 参数行 [agent_id:string:req] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: send_message 参数 message 形状
  假设 工具 send_message 已注册
  当 校验 schema 时
  那么 参数行 [message:string:req] 的名称与类型按目录行实现

场景: send_message 参数 message 约束
  假设 工具 send_message 已注册
  当 校验 schema 时
  那么 参数行 [message:string:req] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: send_message 行为 1
  假设 工具 send_message 执行时
  当 达到触发条件时
  那么 工作中者最近 step 收到，空闲者开新轮；返回送达确认非答案

场景: list_subagent_models 调度与呈现
  假设 合约要求的行为已实现
  当 读取工具 list_subagent_models 的声明时
  那么 executionMode 为 exclusive（parallel 仅 isConcurrencySafe 严格 true，否则 fail-closed exclusive）且 presentation 族为 generic

场景: list_subagent_models 参数越界拒绝
  假设 提交违反该工具 schema 的参数
  当 校验运行时
  那么 校验失败返回结构化错误且不执行

场景: list_subagent_models 说明 section 注册
  假设 组装系统提示词时
  当 扫描 section 注册表
  那么 工具 list_subagent_models 的说明 section 由工具插件注册且 order 位于 1000-3000，文本从 catalog 逐字抽取

场景: list_subagent_models 参数 provider 形状
  假设 工具 list_subagent_models 已注册
  当 校验 schema 时
  那么 参数行 [provider:opt] 的名称与类型按目录行实现

场景: list_subagent_models 参数 provider 约束
  假设 工具 list_subagent_models 已注册
  当 校验 schema 时
  那么 参数行 [provider:opt] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: list_subagent_models 参数 model 形状
  假设 工具 list_subagent_models 已注册
  当 校验 schema 时
  那么 参数行 [model:opt（需 provider）] 的名称与类型按目录行实现

场景: list_subagent_models 参数 model 约束
  假设 工具 list_subagent_models 已注册
  当 校验 schema 时
  那么 参数行 [model:opt（需 provider）] 的 required、默认值与枚举约束按目录行实现并冻结对照

场景: list_subagent_models 行为 1
  假设 工具 list_subagent_models 执行时
  当 达到触发条件时
  那么 目录仅 advisory

## Dependencies

- REQ-FR-TOOLS
- REQ-FR-SANDBOX

## Source Trace

- doc/prd.md#17: 内置工具清单与关键语义；参数级 schema 权威来源 docs/tool-catalog.md
- doc/prd.md#22: ripgrep 分发对策
- confidence: high（清单级；参数级以 catalog 为权威）

## Open Questions

None.
<!-- /agent-spec:requirement -->

<!-- agent-spec:requirement id=REQ-FR-HEADLESS title="headless 入口" tags=headless,entry,cli source=doc/prd.md -->
## Problem

headless 是第一个交付入口（M1）：一次性任务、`--json` 事件流、审批 fail-closed、`--session-id` 续用。装配不挂任何 Host/HTTP/Web 运行时，禁用交互式凭证插件，无 UI answerer。

## Requirements

- [REQ-FR-HEADLESS-1] CLI MUST 为 `fr headless [TASK] [--session-id <id>] [--json] [--help]`；TASK 缺省或为裸 `-` 时从 stdin 读一次整段任务文本；无 TASK 且 stdin 为 TTY MUST 返回 usage error（exit 非 0）；给了 TASK 就不读 stdin；不解析任何交互命令。
- [REQ-FR-HEADLESS-2] 默认输出 MUST：仅最终答案文本到 stdout；provider reasoning delta 实时流到 stderr（`fr: reasoning:` 标题）；错误 `fr: <code>: <message>` 到 stderr；exit 0 = 完成，1 = 中止或出错。
- [REQ-FR-HEADLESS-3] `--json` 模式 MUST：stdout 为换行分隔 JSON 事件流；首事件 `{"type":"session","sessionId":...}`，末事件 `{"type":"final", ...}`（无损最终答案）；中间事件 status / text / thinking / tool_call / tool_result。
- [REQ-FR-HEADLESS-4] `--json` 的 text/thinking MUST 从已提交 assistant message 投影（重试/丢弃 attempt 不出现；按 step 提交而非逐 token）。
- [REQ-FR-HEADLESS-5] `--json` 截断规则 MUST：除 final 外所有字符串/对象值封顶 8 KiB（标 truncated:true），单行封顶 32 KiB，嵌套 64 层截断；运行错误输出 error 事件且无 final。
- [REQ-FR-HEADLESS-6] 审批 MUST：无 answerer，一切 ask fail-closed 'unavailable'（模型收到拒绝结果）；`FR_PERMISSION_MODE=danger-full-access` 时策略 never，工具直接放行。
- [REQ-FR-HEADLESS-7] `--session-id <id>` MUST 采纳已持久化会话继续（不存在的 id 直接失败；拒绝采纳子代理/fork 会话与 agent preset 不匹配的会话）；默认新 `session-<uuid>`。
- [REQ-FR-HEADLESS-8] 装配 MUST 不挂任何 Host/HTTP/Web 运行时；禁用交互式凭证插件；无 UI answerer。

- [REQ-FR-HEADLESS-9] 续用与输出硬细则 MUST：连字符任务参数必须唯一；任务参数非空但 trim 后为空报错；--session-id trim 后判空但原串交付（空白属身份）；--json 下语法错误也发 error 事件；采纳四拒（preset 不匹配、子代理与 fork、无 cwd、cwd 不一致）加 live Agent 拒绝加 resume 双重复查；final 文本 = durable log 最后一段非空 assistant text、exit reason 取 turn/end reason；等待托管子孙结束（子代理收尾提前唤醒时以 seq 未变且无存活子判定抑制）；--json 首事件含 cwd；字符串与 key 均封顶 8KiB、行限 32KiB 二级降级梯；usage 跨 attempt 累加且任一 attempt 无样本则整 step 省略；tool_result 仅 surfaceOp 为 append 时投影（dsh startup.ts:83-113、index.ts:73-99,212-292,375-386、json-stream.ts:16-66,132-149,266-303）。
## Scenarios

Rule: REQ-FR-HEADLESS-1 — CLI 与 stdin 规则

场景: 裸连字符从 stdin 读整段任务
  假设 TASK 为裸 - 且 stdin 提供整段任务文本
  当 运行 fr headless 加该参数时
  那么 整段文本作为任务且不再等待更多输入

场景: 给了 TASK 不读 stdin
  假设 命令行已给定 TASK
  当 运行时
  那么 stdin 被忽略且不阻塞

场景: 无 TASK 且 stdin 为 TTY 报 usage error
  假设 终端交互运行且未给 TASK
  当 执行 fr headless 时
  那么 输出 usage error 且 exit 非 0

Rule: REQ-FR-HEADLESS-2 — 默认输出

场景: 完整任务走通并产出 v4 会话
  假设 有效 provider 配置
  当 执行 fr headless 加任务时
  那么 最终答案到 stdout、产生 v4 会话文件且 exit 0

场景: 运行错误走 stderr 且 exit 1
  假设 provider 配置错误
  当 运行失败时
  那么 stderr 输出 fr 前缀错误行、exit 1 且 stdout 无答案文本

场景: reasoning 流到 stderr
  假设 模型输出含 reasoning delta
  当 默认模式运行时
  那么 reasoning 实时写入 stderr 且带 fr 前缀标题，stdout 不混入

Rule: REQ-FR-HEADLESS-3 — --json 事件流

场景: --json 首末事件定型
  假设 --json 模式运行成功
  当 解析 stdout 事件流时
  那么 首事件 type 为 session，末事件 type 为 final 且答案无损

场景: 中间事件类型齐备
  假设 任务触发工具调用
  当 解析 --json 流时
  那么 status、text、thinking、tool_call、tool_result 事件按序出现

Rule: REQ-FR-HEADLESS-4 — 已提交投影

场景: 丢弃的 attempt 不投影
  假设 流错误后重试成功
  当 解析 --json 流时
  那么 text 与 thinking 仅来自已提交 assistant message，重试尝试文本不出现

Rule: REQ-FR-HEADLESS-5 — 截断与错误事件

场景: 值截断封顶
  假设 某 tool_result 值超过 8 KiB
  当 输出该事件时
  那么 值被截断且标 truncated 为 true，单行不超过 32 KiB

场景: 运行错误输出 error 无 final
  假设 --json 模式运行失败
  当 输出事件流时
  那么 含 error 事件且无 final 事件

Rule: REQ-FR-HEADLESS-6 — 审批 fail-closed

场景: 需审批工具收到 unavailable
  假设 模型调用需审批工具且无 answerer
  当 工具进入审批时
  那么 模型收到 unavailable 拒绝结果

场景: danger-full-access 直接放行
  假设 FR_PERMISSION_MODE 为 danger-full-access
  当 工具执行时
  那么 策略为 never 且工具直接执行

Rule: REQ-FR-HEADLESS-7 --session-id 续用

场景: 续用已持久化会话
  假设 存在一个已持久化的非 fork 非 子代理会话
  当 以 --session-id 指定该 id 运行时
  那么 接续其历史继续且新事件追加到原会话

场景: 不存在的 session id 失败
  假设 --session-id 指向不存在的会话
  当 启动时
  那么 直接失败且不创建新会话

场景: 默认生成新会话 id
  假设 未指定 --session-id
  当 启动时
  那么 创建形如 session 加 uuid 的新会话

Rule: REQ-FR-HEADLESS-8 — 装配约束

场景: headless 装配无交互运行时
  假设 headless profile 装配完成
  当 检查装配产物时
  那么 无 Host/HTTP/Web 运行时插件、无交互式凭证插件、无 UI answerer

Rule: REQ-FR-HEADLESS-1 — help

场景: --help 输出用法
  当 运行 fr headless --help 时
  那么 输出用法说明且 exit 0

Rule: REQ-FR-HEADLESS-9 — 续用与输出硬细则

场景: 语法错误走 JSON 契约
  假设 --json 模式传入非法参数组合
  当 解析失败时
  那么 stdout 输出 error 事件行且 exit 1

场景: key 级截断与降级梯
  假设 事件含超长 key 与超大值
  当 boundJsonLine 处理时
  那么 key 与字符串值均封顶 8KiB，行超限依次降级为标量加 truncated 再到仅 type

场景: 采纳四拒与 live 拒绝
  假设 --session-id 分别指向 preset 不匹配、子代理、cwd 不一致或已被 live Agent 占用的会话
  当 采纳时
  那么 一律拒绝且给出对应错误

场景: trim 空任务参数报错
  假设 任务参数为连续空白非空串
  当 启动时
  那么 报错且 exit 非 0；--session-id 空白串同理判空失败但原串保留于错误上下文

场景: final 来源与提前唤醒抑制
  假设 会话日志含多段 assistant 文本且某子代理在收尾时提前唤醒主循环
  当 汇总输出时
  那么 final 取 durable log 最后一段非空 assistant text，唤醒以 seq 未变且无存活子判定抑制、继续等待

场景: usage 累加与省略
  假设 同一 step 跨两次 attempt 且其中一次无 usage 样本
  当 投影事件时
  那么 有效样本跨 attempt 累加；任一 attempt 无样本则整 step 省略 usage

场景: tool_result 仅 append 投影
  假设 某工具结果曾被压缩替换（surfaceOp 非 append）
  当 投影 --json 流时
  那么 原始结果不出现，仅 append 形态的 tool_result 投影

## Dependencies

- REQ-FR-AGENT-LOOP
- REQ-FR-SESSION
- REQ-FR-APPROVAL
- REQ-FR-CONFIG
- REQ-FR-LLM

## Source Trace

- doc/prd.md#18.1: headless 字段级规格（CLI、输出、--json、审批、多轮）
- confidence: high（PRD 18.1 字段级）
- doc/dsh-source-notes.md: 源码级技术关键点（file:line 证据）

## Open Questions

None.
<!-- /agent-spec:requirement -->

<!-- agent-spec:requirement id=REQ-FR-TUI title="TUI 入口" tags=tui,entry,ratatui source=doc/prd.md -->
## Problem

TUI 是主交互入口（M2）。为 ratatui 纯投影层：订阅会话事件 fold 进状态、只读渲染；band-stack 布局、单源键路由、modal 键盘词汇，范式承袭 shadow-tui。

## Requirements

- [REQ-FR-TUI-1] 技术栈 MUST：ratatui 0.29（unstable-rendered-line-info）+ crossterm 0.28 + pulldown-cmark 0.10 + unicode-width 0.2；fr-tui 为纯投影层：`Model::apply(event)` fold 进状态，`draw()` 只读渲染。
- [REQ-FR-TUI-2] 事件循环 MUST：spawn_blocking 轮询线程 → tokio mpsc → 主循环 `tokio::select!`（按键 rx / 事件 rx / 审批信号 / 问题信号 / 补全 debounce 120ms / 100ms 重绘 ticker / shutdown watch）；通信全走 channel。
- [REQ-FR-TUI-3] 布局 MUST 为 band-stack 单列无侧栏：header(1)/分隔线/内容时间线（弹性）/docked 卡区/activity 行/输入上分隔线/输入编辑框/输入下分隔线/排队提示行（有队列才占位）/help 行/footer 统计行；输入框随 wrap 实测行数增长（clamp MAX_INPUT_ROWS）；手动滚动暂停 auto-follow，滚回底部自动恢复；极矮终端（<8 行）按序降级（排队行/help/activity/输入框）；渲染异常降级为 notice 不 panic；docked 卡不遮时间线，仅审批/问题 modal 为 overlay；footer 贪心打包占多行；max_scroll 每帧实测。
- [REQ-FR-TUI-4] 键位表 MUST 全项生效（PRD 18.2 表）：Enter 发送；Ctrl+Enter/Alt+Enter/Ctrl+J/行尾反斜杠+Enter 插入换行；readline 编辑语义；上下方向翻 prompt 历史（带 draft 保护）；Ctrl+C running 中断、idle 双击退出（3 秒窗口）；Esc 中断或关 modal；PageUp/PageDown 翻页 ±5 行 / 鼠标滚轮滚动；Shift+Tab 循环 permission mode；Ctrl+O/T/R 展开工具卡/任务面板/子代理视图；空输入时 ? 帮助 overlay；bracketed paste 整段粘贴；kitty keyboard protocol 探测 CSI u。
- [REQ-FR-TUI-5] 审批卡 MUST：浮动 modal（tool:/kind: 元信息 + 命令全文手动 wrap、尾部必须可见、超长省略标注）；三选项 Yes(y)/Yes-and-dont-ask-this-session(a)/No(n)；y/a/n 单键直判，Up/Down+Enter 导航；选中行 `>` 标记反色；modal 独占键盘，Esc/Ctrl+C 永远可退出。
- [REQ-FR-TUI-6] ask_user_question 渲染 MUST：全屏对话框；数字 1-9 直选、Space 多选切换、Enter 激活、Tab/Shift+Tab 翻问题页、Esc 拒绝、常驻 Other 自由输入行；单问题单选选中即提交，多问题结束进 review 页。
- [REQ-FR-TUI-7] 渲染 MUST：markdown 经 pulldown-cmark → styled Lines；仅 4 语义色 + bold/italic/underline；表格宽度自适应收缩 + 单元格内 wrap；流式 holdback（未闭合表格整段扣住）；标题去 #；行内 code 着色；无语法高亮；Palette accent/warn/error/add/remove/user_bg/ok/dim + 按工具族着色；ColorMode 协商 None/16/256/TrueColor，尊重 NO_COLOR/TERM=dumb/tmux；diff 渲染 unified（add/remove 色）；工具卡三态（queued 静态 / running 闪烁 / 终态显示耗时）；交互输出全走 stderr，stdout 留给数据。
- [REQ-FR-TUI-8] 终端生命周期 MUST：TerminalGuard RAII（raw mode + alternate screen + bracketed paste + kitty 探测）；信号处理；SSH/tmux/CI 降级。
- [REQ-FR-TUI-9] 四搬运模块 MUST 保留出处头注释：自 ~/project/shadow/crates/shadow-tui 拷入 src/render/markdown.rs（1440 行）、src/render/diff.rs（289 行）、src/complete.rs（310 行 fuzzy 补全）、src/env/（约 1000 行：TermContext/TerminalGuard/kitty/信号）；其余按规格重写，禁止依赖 shadow-tui crate 本体。
- [REQ-FR-TUI-10] 斜杠与补全 MUST：`/` 命令补全与 `@` 文件补全，fuzzy 评分 + 120ms debounce；键位表逐项有 TestBackend 测试；tmux capture-pane 实机验证脚本入库。

## Scenarios

Rule: REQ-FR-TUI-1 — 纯投影层

场景: 事件 fold 进状态只读渲染
  假设 会话事件依次到达
  当 Model 逐事件 apply 且 draw 渲染时
  那么 渲染输出由事件序列唯一决定（TestBackend 断言）

Rule: REQ-FR-TUI-2 — 事件循环

场景: 多事件源单循环汇聚
  假设 按键、会话事件、审批、问题、补全 debounce、重绘 ticker 与 shutdown watch 多源并发
  当 任一源触发时
  那么 全部经主循环 select 处理且通信全走 channel

Rule: REQ-FR-TUI-3 — band-stack 布局与降级

场景: 极矮终端按序降级
  假设 终端高度小于 8 行
  当 渲染时
  那么 按排队行、help、activity、输入框顺序降级且不 panic

场景: 手动滚动暂停与恢复 auto-follow
  假设 用户滚动离开时间线底部后滚回
  当 新事件到达时
  那么 离开底部期间不跟随，回到底部后恢复自动跟随

场景: docked 卡不遮挡时间线
  当 工具卡以 docked 形式展示时
  那么 时间线完整可见，仅审批与问题 modal 为 overlay

场景: footer 贪心打包
  假设 统计项较多
  当 渲染 footer 时
  那么 统计项贪心打包占多行

Rule: REQ-FR-TUI-4 — 键位表

场景: Ctrl+C 中断运行中会话
  假设 会话 running
  当 按下 Ctrl+C
  那么 发送中断且输入恢复可编辑

场景: idle 双击 Ctrl+C 退出
  假设 空闲状态且 3 秒内第二次 Ctrl+C
  当 第二次按下时
  那么 退出 TUI

场景: 多行输入与整段粘贴
  假设 输入框有焦点
  当 Ctrl+J、行尾反斜杠加 Enter 或 bracketed paste 时
  那么 换行插入或整段粘贴生效

场景: Shift+Tab 循环权限模式
  当 连续按 Shift+Tab 时
  那么 按 read-only、workspace-write、danger-full-access 循环切换

场景: 上下方向翻 prompt 历史带 draft 保护
  假设 输入框有未发送草稿
  当 翻历史后再回到底部时
  那么 草稿内容恢复不丢失

场景: PageUp PageDown 与滚轮滚动
  当 按 PageUp/PageDown 或滚动滚轮时
  那么 时间线翻页或滚动，翻页步长 5 行

Rule: REQ-FR-TUI-5 — 审批卡

场景: y a n 单键直判
  假设 审批 modal 打开
  当 按下 y、a 或 n
  那么 立即结算对应审批选项且无需 Enter

场景: Esc 永远可退出审批 modal
  假设 审批 modal 打开
  当 按 Esc 或 Ctrl+C 时
  那么 modal 关闭且键盘焦点回归主循环

场景: 超长命令全文标注
  假设 审批对象命令超过卡高
  当 渲染审批卡时
  那么 命令全文手动 wrap、尾部必须可见且超长部分带省略标注

Rule: REQ-FR-TUI-6 — 问题对话框

场景: 数字直选与多选
  假设 ask_user_question 对话框打开
  当 数字 1-9 直选、Space 多选切换、Enter 激活时
  那么 选择生效，Esc 拒绝，Tab 与 Shift+Tab 翻问题页，Other 自由输入行常驻

Rule: REQ-FR-TUI-7 — 渲染规范

场景: 未闭合表格 holdback
  假设 流式输出中出现未闭合表格
  当 渲染时间线时
  那么 该表格整段扣住，闭合后一并输出

场景: NO_COLOR 降级
  假设 NO_COLOR 已设置
  当 渲染时
  那么 以无色彩模式输出

场景: 工具卡三态
  假设 工具卡经历 queued、running 与终态
  当 各状态渲染时
  那么 分别呈现静态、闪烁与耗时三种形态

Rule: REQ-FR-TUI-8 — 终端生命周期

场景: TerminalGuard RAII 恢复
  假设 TUI 异常退出
  当 TerminalGuard drop 时
  那么 raw mode 与 alternate screen 恢复，bracketed paste 复位

场景: CI 环境降级
  假设 运行在 CI 或非交互终端
  当 TUI 启动时
  那么 按降级路径运行或明确报错，不破坏终端

Rule: REQ-FR-TUI-9 — 搬运模块出处

场景: 四搬运模块保留出处头注释
  当 检查 markdown.rs、diff.rs、complete.rs 与 env 目录文件头时
  那么 均保留 shadow-tui 出处注释且代码不依赖 shadow-tui crate 本体

Rule: REQ-FR-TUI-10 — 斜杠与补全

场景: 命令与文件补全 debounce
  假设 输入以 / 或 @ 开头
  当 补全 debounce 120ms 到期时
  那么 按 fuzzy 评分排序输出候选

Rule: REQ-FR-TUI-4 — 键位补遗

场景: Enter 发送与 ? 帮助
  假设 输入框有内容或为空
  当 分别按 Enter 与 ? 时
  那么 有内容则发送，空输入时 ? 打开帮助 overlay

场景: Ctrl+O T R 面板
  当 分别按 Ctrl+O、Ctrl+T、Ctrl+R 时
  那么 展开最新工具卡、任务面板与子代理实时视图

场景: kitty 协议探测降级
  假设 终端不支持 kitty keyboard protocol
  当 启动探测 CSI u 失败时
  那么 回退传统按键语义且 Ctrl+Enter 降级可用

Rule: REQ-FR-TUI-1 — 对齐基线

场景: 不复刻 dsh pi-tui 交互形态
  假设 以 dsh 最后版 TUI（packages/ui/tui，上游 commit 10bb9cbf4a 已删除）的键位与布局对照 fr TUI
  当 评审 TUI 行为时
  那么 对齐对象为 PRD 18.2 自有规格（ratatui band-stack、PRD 键位表、shadow-tui 范式）；dsh TUI 键位（如 Ctrl+L 重绘、Ctrl+O 三态循环）不构成 fr 验收依据，仅会话语义层（命令解析、审批、问题、steer/followup）与 dsh 对齐

Rule: REQ-FR-TUI-4 — 键位全表枚举（枚举生成）

场景: 键位 Enter
  假设 TUI 运行中
  当 按下 Enter
  那么 行为与 PRD 18.2 键位表一致且有 TestBackend 断言

场景: 键位 Ctrl+Enter
  假设 TUI 运行中
  当 按下 Ctrl+Enter
  那么 行为与 PRD 18.2 键位表一致且有 TestBackend 断言

场景: 键位 Alt+Enter
  假设 TUI 运行中
  当 按下 Alt+Enter
  那么 行为与 PRD 18.2 键位表一致且有 TestBackend 断言

场景: 键位 Ctrl+J
  假设 TUI 运行中
  当 按下 Ctrl+J
  那么 行为与 PRD 18.2 键位表一致且有 TestBackend 断言

场景: 键位 行尾反斜杠加 Enter
  假设 TUI 运行中
  当 按下 行尾反斜杠加 Enter
  那么 行为与 PRD 18.2 键位表一致且有 TestBackend 断言

场景: 键位 Backspace
  假设 TUI 运行中
  当 按下 Backspace
  那么 行为与 PRD 18.2 键位表一致且有 TestBackend 断言

场景: 键位 Delete
  假设 TUI 运行中
  当 按下 Delete
  那么 行为与 PRD 18.2 键位表一致且有 TestBackend 断言

场景: 键位 左方向
  假设 TUI 运行中
  当 按下 左方向
  那么 行为与 PRD 18.2 键位表一致且有 TestBackend 断言

场景: 键位 右方向
  假设 TUI 运行中
  当 按下 右方向
  那么 行为与 PRD 18.2 键位表一致且有 TestBackend 断言

场景: 键位 Home
  假设 TUI 运行中
  当 按下 Home
  那么 行为与 PRD 18.2 键位表一致且有 TestBackend 断言

场景: 键位 End
  假设 TUI 运行中
  当 按下 End
  那么 行为与 PRD 18.2 键位表一致且有 TestBackend 断言

场景: 键位 Ctrl+A
  假设 TUI 运行中
  当 按下 Ctrl+A
  那么 行为与 PRD 18.2 键位表一致且有 TestBackend 断言

场景: 键位 Ctrl+E
  假设 TUI 运行中
  当 按下 Ctrl+E
  那么 行为与 PRD 18.2 键位表一致且有 TestBackend 断言

场景: 键位 Ctrl+U
  假设 TUI 运行中
  当 按下 Ctrl+U
  那么 行为与 PRD 18.2 键位表一致且有 TestBackend 断言

场景: 键位 上方向
  假设 TUI 运行中
  当 按下 上方向
  那么 行为与 PRD 18.2 键位表一致且有 TestBackend 断言

场景: 键位 下方向
  假设 TUI 运行中
  当 按下 下方向
  那么 行为与 PRD 18.2 键位表一致且有 TestBackend 断言

场景: 键位 Ctrl+C
  假设 TUI 运行中
  当 按下 Ctrl+C
  那么 行为与 PRD 18.2 键位表一致且有 TestBackend 断言

场景: 键位 Esc
  假设 TUI 运行中
  当 按下 Esc
  那么 行为与 PRD 18.2 键位表一致且有 TestBackend 断言

场景: 键位 PageUp
  假设 TUI 运行中
  当 按下 PageUp
  那么 行为与 PRD 18.2 键位表一致且有 TestBackend 断言

场景: 键位 PageDown
  假设 TUI 运行中
  当 按下 PageDown
  那么 行为与 PRD 18.2 键位表一致且有 TestBackend 断言

场景: 键位 Shift+Tab
  假设 TUI 运行中
  当 按下 Shift+Tab
  那么 行为与 PRD 18.2 键位表一致且有 TestBackend 断言

场景: 键位 Ctrl+O
  假设 TUI 运行中
  当 按下 Ctrl+O
  那么 行为与 PRD 18.2 键位表一致且有 TestBackend 断言

场景: 键位 Ctrl+T
  假设 TUI 运行中
  当 按下 Ctrl+T
  那么 行为与 PRD 18.2 键位表一致且有 TestBackend 断言

场景: 键位 Ctrl+R
  假设 TUI 运行中
  当 按下 Ctrl+R
  那么 行为与 PRD 18.2 键位表一致且有 TestBackend 断言

场景: 键位 ?
  假设 TUI 运行中
  当 按下 ?
  那么 行为与 PRD 18.2 键位表一致且有 TestBackend 断言

场景: 键位 滚轮
  假设 TUI 运行中
  当 按下 滚轮
  那么 行为与 PRD 18.2 键位表一致且有 TestBackend 断言

## Dependencies

- REQ-FR-AGENT-LOOP
- REQ-FR-APPROVAL
- REQ-FR-SESSION
- REQ-FR-COMMANDS

## Source Trace

- doc/prd.md#18.2: TUI 字段级规格（技术栈、布局、键位表、审批卡、问题对话框、渲染、终端生命周期、搬运清单、补全）
- confidence: high（PRD 18.2 字段级）

## Open Questions

None.
<!-- /agent-spec:requirement -->

<!-- agent-spec:requirement id=REQ-FR-ACP title="ACP 服务器入口" tags=acp,entry,protocol source=doc/prd.md -->
## Problem

ACP 让外部编辑器/客户端接入。fly-rafter 遵循 ACP v1（对齐 dsh @agentclientprotocol/sdk@1.4.0 行为），stdio JSON-RPC，一连接多会话并行。

## Requirements

- [REQ-FR-ACP-1] MUST 遵循 ACP v1：stdio JSON-RPC，stdout 只承载协议流量；一连接多会话并行；`agentInfo` 为 `fly-rafter-acp/0.0.1`。
- [REQ-FR-ACP-2] 方法面 MUST 对齐 dsh packages/acp/acp/README.md Protocol contract：initialize（通告 v1 + session/list/resume/close + Streamable HTTP MCP）、authenticate（直接成功）、session/new（新建持久会话；workspace 与 stdio/HTTP MCP 先校验后发布；返回完整配置选项态）、session/list（持久可恢复根会话 newest-first 分页）、session/resume（恢复非活动持久会话，不重放旧更新）、session/close（静默取消 → 排干更新 → 子孙处置 → 持久化 flush → 处置该会话 scope）、session/set_config_option（model / reasoning_effort，返回完整结果态）、session/prompt（每会话同时仅一个 in-flight prompt；text/resource-link/受支持图片；settlement 等根与子孙全部完成）、session/cancel 与 $/cancel_request（未知 id no-op）。
- [REQ-FR-ACP-3] 下行 MUST：session/update（每会话串行：已提交助手消息/thought、通用工具生命周期、配置变化、context 用量；不含原始 provider delta/重试/专有呈现）；session/request_permission（一次性 allow/reject，客户端可自动应答）。
- [REQ-FR-ACP-4] 映射 MUST：一个 ACP session = 一个持久 agent+session；模型选择在 prompt 准入时快照并 pin 整个 turn。
- [REQ-FR-ACP-5] MUST NOT 支持（与 dsh 一致）：session/load、删除、fork、额外目录、SSE/ACP-transport MCP、modes、commands、plans、terminals、客户端文件系统操作、elicitation。
- [REQ-FR-ACP-6] 配置 MUST：provider / model / `session-list-page-size`（默认 100）。

- [REQ-FR-ACP-7] 生命周期细则 MUST：image 能力条件通告且 audio 与 embeddedContext 恒 false；resume 要求 cwd realpath 物理等同并优先日志记录的路由；list 游标 base64url canonical 防伪并支持 cwd 过滤；prompt 准入期两次 agent 存活校验；config id 仅 model 与 reasoning_effort；更新经 outputTail promise 链每会话串行、交付失败仅记 outputError 不阻断；settlement 循环 race（whenIdle 对 cancelled）（dsh index.ts:180-335,479-506; session.ts:78-94,246-565）。
## Scenarios

Rule: REQ-FR-ACP-1 — 传输与 agentInfo

场景: initialize 通告与 stdout 纯净
  当 ACP 客户端执行 initialize 时
  那么 响应通告 v1 与能力面，agentInfo 为 fly-rafter-acp 斜杠版本，且 stdout 无协议外流量

Rule: REQ-FR-ACP-2 — 方法面

场景: session/new 先校验后发布
  假设 客户端携带 workspace 与 stdio/HTTP MCP 配置
  当 调用 session/new 时
  那么 先校验后发布，返回完整配置选项态

场景: 每会话仅一个 in-flight prompt
  假设 某会话已有进行中的 prompt
  当 第二个 session/prompt 到达该会话时
  那么 第二个被拒绝

场景: cancel 未知 id no-op
  当 session/cancel 或 $/cancel_request 携带未知 id 时
  那么 no-op 且不报错

场景: set_config_option 返回结果态
  当 session/set_config_option 修改 model 或 reasoning_effort 时
  那么 返回修改后的完整结果态

场景: resume 不重放旧更新
  假设 持久会话已有历史更新
  当 session/resume 时
  那么 恢复成功且不重放 resume 前的更新

场景: close 排干并处置
  假设 会话正在运行且有子代理
  当 session/close 时
  那么 静默取消、排干更新、处置子孙、flush 持久化并处置该会话 scope

Rule: REQ-FR-ACP-3 — 下行更新

场景: session/update 每会话串行
  假设 同一会话产生多个更新
  当 下行时
  那么 按序串行下发，且不含原始 provider delta 与重试细节

场景: request_permission 一次性
  假设 工具需要审批
  当 session/request_permission 下行时
  那么 客户端一次性 allow 或 reject 即结算

Rule: REQ-FR-ACP-4 — 模型 pin

场景: 模型快照 pin 整个 turn
  假设 prompt 准入时选中模型 A
  当 turn 运行中配置变化时
  那么 本 turn 仍使用模型 A

Rule: REQ-FR-ACP-5 — 不支持清单

场景: 不支持的方法被拒
  当 调用 session/load、modes、commands 或 elicitation 等不支持面时
  那么 返回不支持，不提供部分实现

Rule: REQ-FR-ACP-6 — 配置

场景: session/list 分页与页大小
  假设 存在多个持久根会话且页大小配置为默认 100
  当 调用 session/list 时
  那么 newest-first 返回且受页大小约束

Rule: REQ-FR-ACP-2 — authenticate 与多会话

场景: authenticate 直接成功
  当 客户端调用 authenticate 时
  那么 直接成功返回

场景: 一连接多会话并行
  假设 同一连接已建立两个会话且会话 A prompt 进行中
  当 会话 B 发起 prompt 时
  那么 B 正常执行，不受 A 阻塞

Rule: REQ-FR-ACP-7 — 生命周期细则

场景: resume 的 cwd 与路由
  假设 持久会话记录于另一 cwd 或记录过显式模型路由
  当 resume 时
  那么 cwd 物理不等同即拒绝，恢复后优先日志记录的路由

场景: 游标防伪
  假设 客户端提交被篡改的 list 游标
  当 session/list 分页时
  那么 canonical 校验失败即拒绝

Rule: REQ-FR-ACP-2 — 方法面枚举（枚举生成）

场景: ACP initialize 契约
  假设 标准 ACP 客户端连接
  当 调用 initialize 时
  那么 行为与 dsh packages/acp 实现一致且 stdout 无协议外流量

场景: ACP authenticate 契约
  假设 标准 ACP 客户端连接
  当 调用 authenticate 时
  那么 行为与 dsh packages/acp 实现一致且 stdout 无协议外流量

场景: ACP session/new 契约
  假设 标准 ACP 客户端连接
  当 调用 session/new 时
  那么 行为与 dsh packages/acp 实现一致且 stdout 无协议外流量

场景: ACP session/list 契约
  假设 标准 ACP 客户端连接
  当 调用 session/list 时
  那么 行为与 dsh packages/acp 实现一致且 stdout 无协议外流量

场景: ACP session/resume 契约
  假设 标准 ACP 客户端连接
  当 调用 session/resume 时
  那么 行为与 dsh packages/acp 实现一致且 stdout 无协议外流量

场景: ACP session/close 契约
  假设 标准 ACP 客户端连接
  当 调用 session/close 时
  那么 行为与 dsh packages/acp 实现一致且 stdout 无协议外流量

场景: ACP session/set_config_option 契约
  假设 标准 ACP 客户端连接
  当 调用 session/set_config_option 时
  那么 行为与 dsh packages/acp 实现一致且 stdout 无协议外流量

场景: ACP session/prompt 契约
  假设 标准 ACP 客户端连接
  当 调用 session/prompt 时
  那么 行为与 dsh packages/acp 实现一致且 stdout 无协议外流量

场景: ACP session/cancel 契约
  假设 标准 ACP 客户端连接
  当 调用 session/cancel 时
  那么 行为与 dsh packages/acp 实现一致且 stdout 无协议外流量

场景: ACP $/cancel_request 契约
  假设 标准 ACP 客户端连接
  当 调用 $/cancel_request 时
  那么 行为与 dsh packages/acp 实现一致且 stdout 无协议外流量

Rule: REQ-FR-ACP-7 — 准入与过滤细则

场景: resume 拒绝形态
  假设 目标会话分别处于活跃或 activating、为子代理会话、为 fork 会话
  当 session/resume 时
  那么 三种形态均以 invalidParams 拒绝；仅非活动持久根会话可恢复

场景: list 过滤与同刻决胜
  假设 会话集合含活跃、子代理、fork、cwd 缺失或非绝对与会话
  当 session/list 时
  那么 上述全部被过滤；createdAt 降序、同刻按 id 字节序决胜

场景: prompt 图片准入
  假设 prompt 含图片而能力未通告 image、或 base64 非 canonical
  当 准入校验时
  那么 以 invalidParams 拒绝；单 in-flight 冲突亦为 invalidParams

场景: request_permission 门控与排干
  假设 权限请求非本会话所有或缺 callId
  当 拦截结算时
  那么 不下行客户端、按 unavailable fail-closed；合法请求先 drainUpdates 再下行，选项为 allow-once 与 reject-once；configOptions 请求亦经 outputTail 串行化，未知 config id 报错

## Dependencies

- REQ-FR-AGENT-LOOP
- REQ-FR-SESSION

## Source Trace

- doc/prd.md#18.3: ACP 协议契约级规格
- confidence: high（契约级；方法字段细节以 dsh ACP README 为权威）
- doc/dsh-source-notes.md: 源码级技术关键点（file:line 证据）

## Open Questions

None.
<!-- /agent-spec:requirement -->

<!-- agent-spec:requirement id=REQ-FR-SDK title="SDK JSON-RPC 服务器入口" tags=sdk,entry,protocol source=doc/prd.md -->
## Problem

SDK 以协议即契约方式暴露运行时：换行分隔 JSON-RPC 2.0 over stdio；不提供官方客户端 SDK（含 Python SDK 裁剪，用户裁决）。

## Requirements

- [REQ-FR-SDK-1] 传输 MUST：换行分隔 JSON-RPC 2.0 over stdio；帧规则对齐 dsh protocol/src/transport.ts（有 id+method 为请求、仅 id 为响应、仅 method 为通知；malformed 行忽略；handler 失败变错误帧）；错误码 MUST（勘误同步，源码 transport.ts:226-237 + server.ts:374-375）：SDK 方法面恒有 handler，未知方法经 default handler 抛普通 Error 得 **-32603**；-32601 仅出现在 transport 层未装配 handler 时；handler 内部错误同为 -32603；对端错误透传 wire code + data。
- [REQ-FR-SDK-2] 请求方法 MUST 全集：initialize（cwd/provider/model/reasoningEffort?/maxTokens? → serverInfo: {name: 'fly-rafter-sdk-runtime', version}）；session/prompt（sessionId 未知 id 惰性创建；contentBlocks 支持文本与图片块 `{type:'image', data: base64, mimeType: png/jpeg/webp/gif}` 经 durable attachment store 准入 → messageId）；session/resume（不存在或被占用则拒绝；仅恢复不入队 prompt）；session/wait（等该会话及托管子孙全部结束；从不创建）；session/working-directory/get 与 set；shutdown（进程级；协议内无 session/close）。
- [REQ-FR-SDK-3] 下行通知 MUST 全集：session.event {sessionId, event}（运行时内所有会话含子孙的完整事件逐条实时下发）；session.status {sessionId, status: idle|running}；subagent.started {parentSessionId, childSessionId}；subagent.finished {provider, agentId, parentSessionId, childSessionId, status: ok|error, stopReason, lastAssistantMessage?}。
- [REQ-FR-SDK-4] serverInfo.name MUST 为 `fly-rafter-sdk-runtime`（wire 稳定，一经发布不改）。
- [REQ-FR-SDK-5] MUST NOT 提供官方客户端 SDK；MUST NOT 交付 Python SDK（协议即契约，调用方自行封装）。

- [REQ-FR-SDK-6] 语义细则 MUST：请求 id 形如 req 加 uuid；params 非对象坍缩为空对象；subagent.finished 仅 local 子女；max-tokens 仅 maxTokensAsSuccess 才计 ok（默认 false）；initialize 无适配器仅 deepseek-official 允许 fallback 并路由预检；wait 从不创建会话且退出有 failure 则抛；shutdown 单飞逆序 dispose（dsh server.ts:73-76,155-189,240-257,298-334,374-375）。
## Scenarios

Rule: REQ-FR-SDK-1 — 传输与错误码

场景: initialize 返回 serverInfo
  当 发送 initialize 请求时
  那么 响应含 name 为 fly-rafter-sdk-runtime 的 serverInfo 与版本号

场景: 未知方法 -32603
  当 发送未定义方法请求时
  那么 default handler 抛普通 Error，返回错误码 -32603 的错误帧（-32601 仅 transport 层未装 handler 时出现）

场景: handler 抛错 -32603
  假设 某方法 handler 内部出错
  当 该请求被处理时
  那么 返回错误码 -32603 的错误帧

场景: malformed 行忽略
  当 输入流中出现无法解析的行时
  那么 忽略该行并继续服务

Rule: REQ-FR-SDK-2 — 请求方法

场景: prompt 未知会话惰性创建
  假设 session/prompt 携带未知 sessionId
  当 处理该请求时
  那么 惰性创建会话并返回 messageId 入队回执

场景: resume 拒绝不存在与占用
  假设 目标会话不存在或正被占用
  当 session/resume 时
  那么 拒绝该请求

场景: working-directory 相对解析
  假设 session/working-directory/set 携带相对路径
  当 处理时
  那么 相对于当前工作目录解析并返回新 cwd

场景: wait 等待全部子孙
  假设 主会话 spawn 了子代理会话
  当 对主会话调用 session/wait 时
  那么 等待主会话与全部托管子孙结束才返回

场景: shutdown 进程级
  当 调用 shutdown 时
  那么 进程级关停且协议内无 session/close

Rule: REQ-FR-SDK-3 — 下行通知

场景: 事件与状态通知实时下发
  假设 运行时内有活动会话与子孙会话
  当 事件与状态变化时
  那么 session.event 逐条实时下发且含子孙会话，session.status 在 idle 与 running 间正确切换

场景: subagent 生命周期通知
  假设 主会话 spawn 子代理并结束
  当 子代理开始与结束时
  那么 下行 subagent.started 与 subagent.finished 通知

Rule: REQ-FR-SDK-5 — 无官方客户端

场景: 仓库不提供客户端 SDK 产物
  当 检查交付物清单时
  那么 不存在官方客户端 SDK 与 Python SDK 产物

Rule: REQ-FR-SDK-2 — 图片块准入

场景: 图片块经 attachment store
  假设 session/prompt 的 contentBlocks 含 png/jpeg/webp/gif 图片块
  当 处理该请求时
  那么 图片经 durable attachment store 准入并进入消息

Rule: REQ-FR-SDK-4 — serverInfo wire 稳定

场景: serverInfo 名称冻结
  假设 SDK 服务器多次版本迭代
  当 检查 serverInfo.name 常量时
  那么 恒为 fly-rafter-sdk-runtime 且 wire 层一经发布不改动

Rule: REQ-FR-SDK-6 — 语义细则

场景: local 过滤与 maxTokens 结算
  假设 子代理含非 local 来源且 turn 以 max-tokens 结束
  当 下行 subagent.finished 与状态结算时
  那么 非 local 不发 finished；max-tokens 默认计 error

Rule: REQ-FR-SDK-2 — 方法与通知枚举（枚举生成）

场景: SDK initialize 契约
  假设 JSON-RPC 客户端连接
  当 调用 initialize 时
  那么 行为与 PRD 18.4 表逐字一致

场景: SDK session/prompt 契约
  假设 JSON-RPC 客户端连接
  当 调用 session/prompt 时
  那么 行为与 PRD 18.4 表逐字一致

场景: SDK session/resume 契约
  假设 JSON-RPC 客户端连接
  当 调用 session/resume 时
  那么 行为与 PRD 18.4 表逐字一致

场景: SDK session/wait 契约
  假设 JSON-RPC 客户端连接
  当 调用 session/wait 时
  那么 行为与 PRD 18.4 表逐字一致

场景: SDK session/working-directory/get 契约
  假设 JSON-RPC 客户端连接
  当 调用 session/working-directory/get 时
  那么 行为与 PRD 18.4 表逐字一致

场景: SDK session/working-directory/set 契约
  假设 JSON-RPC 客户端连接
  当 调用 session/working-directory/set 时
  那么 行为与 PRD 18.4 表逐字一致

场景: SDK shutdown 契约
  假设 JSON-RPC 客户端连接
  当 调用 shutdown 时
  那么 行为与 PRD 18.4 表逐字一致

场景: SDK 通知 session.event
  假设 运行时内会话活动
  当 触发 session.event 时
  那么 载荷字段与 PRD 18.4 一致且实时下发

场景: SDK 通知 session.status
  假设 运行时内会话活动
  当 触发 session.status 时
  那么 载荷字段与 PRD 18.4 一致且实时下发

场景: SDK 通知 subagent.started
  假设 运行时内会话活动
  当 触发 subagent.started 时
  那么 载荷字段与 PRD 18.4 一致且实时下发

场景: SDK 通知 subagent.finished
  假设 运行时内会话活动
  当 触发 subagent.finished 时
  那么 载荷字段与 PRD 18.4 一致且实时下发

Rule: REQ-FR-SDK-6 — 校验与关停序

场景: initialize 参数校验
  假设 initialize 分别携带空串 reasoningEffort、非正或非整数 maxTokens
  当 处理时
  那么 两种情形均报错；合法值正常受理；无适配器时仅 provider 为 deepseek-official 允许惰性挂载并做路由预检

场景: wait 未知 id 报错
  假设 session/wait 携带从未存在的 sessionId
  当 处理时
  那么 报错且不创建任何会话；等待循环在 whenIdle 且子孙完成且事件序未变时才退出

场景: loader 就绪与 shutdown 退出序
  假设 初始化请求先于插件装载完成到达、随后调用 shutdown
  当 结算时
  那么 initialize 先等待 loader 就绪再处理；shutdown 先行响应，随后 flush 持久化、按逆序 dispose、失败聚合后 exit 0

## Dependencies

- REQ-FR-AGENT-LOOP
- REQ-FR-SESSION
- REQ-FR-SUBAGENT

## Source Trace

- doc/prd.md#18.4: SDK 协议契约级规格（传输、方法表、通知、不提供官方客户端）
- confidence: high（契约级）
- doc/dsh-source-notes.md: 源码级技术关键点（file:line 证据）

## Open Questions

None.
<!-- /agent-spec:requirement -->


<!-- agent-spec:requirement id=REQ-FR-QUALITY title="质量门与命名硬约束" tags=quality,naming,ci source=doc/prd.md -->
## Problem

项目质量门零容忍：fmt / clippy / test / doc 四关全绿；命名遵循 Rust 风格硬约束；1:1 对齐以对照测试锚定。

## Requirements

- [REQ-FR-QUALITY-1] 质量门 MUST 四关全绿：`cargo fmt --check`、`cargo clippy -- -D warnings`（lint 配置与 dougong workspace 对齐：rust lints + clippy all/pedantic/nursery/cargo 及选定 restriction 项）、`cargo test`、`cargo doc`（无警告）。
- [REQ-FR-QUALITY-2] 工具链 MUST 与 dougong 同步钉死（1.99.1 补丁级，含 wasm32-wasip2 与 musl target）。
- [REQ-FR-QUALITY-3] 命名 MUST：Rust 标识符 snake_case/UpperCamelCase/SCREAMING_SNAKE_CASE，禁 TS 风格 camelCase；自有用户可见命名 kebab-case（CLI flag、配置键、crate 名）；环境变量 SCREAMING_SNAKE（FR_ 前缀）；dsh 协议 wire 层（会话事件、工具 schema、SDK/ACP 参数）豁免保持逐字；dsh camelCase 插件配置键转 kebab-case（serde rename 对接）。
- [REQ-FR-QUALITY-4] 对照测试 MUST：golden session 回放——构造 v4 会话文件，断言 deriveMessages 重建的冻结请求与 dsh 同场景一致；`--json` 事件流与 SDK/ACP 帧各有 wire 一致性测试；同一任务在同模型同参数下，fly-rafter 与 dsh 产出的会话日志 MUST 逐事件同型（工具名/事件类型/字段名 diff 为空）。
- [REQ-FR-QUALITY-5] 文档即生成物 MUST：tool-catalog / config-catalog 等价物由脚本从真实装配生成并校验，防文档漂移。
- [REQ-FR-QUALITY-6] MUST NOT 以放宽 lint 或断言换取绿灯；测试用例以暴露缺陷为目标。
- [REQ-FR-QUALITY-7] 服务缝名与事件名 MUST 与 dsh 逐字一致——服务缝：llm、tools、fs、shell、terminals、subprocess、sandbox、web、lsp、jobs、subagents、goals、session-title、session-persistence、spill-store、mcp-resources；事件：agent/pre-step、agent/request、agent/request-error、agent/assistant-stream、agent/turn-stopping、tools/pre-execute、tools/execute、tools/post-execute。
- [REQ-FR-QUALITY-8] Invoker 消费 MUST 统一经 `ctx.require::<Invoker>`，MUST NOT 使用 `ctx.get`；跨 dylib/wasm 边界的服务调用 MUST 一律走此通道。
- [REQ-FR-QUALITY-9] workspace MUST 按 PRD 4.5 布局（26 crate）；依赖方向 MUST：fr 只依赖 fr-config 与各入口插件，任何领域 crate MUST NOT 依赖入口 crate，全部插件 crate MUST 依赖 dougong（基座 0.3.0 crates.io 依赖，本机可叠 path 覆盖，内核改动随里程碑发版）。

## Scenarios

Rule: REQ-FR-QUALITY-1 — 四关全绿

场景: 质量门四关通过
  假设 任意提交前
  当 运行四关命令时
  那么 fmt、clippy、test、doc 全部零警告零错误

Rule: REQ-FR-QUALITY-2 — 工具链钉死

场景: 工具链与 dougong 一致
  当 读取 rust-toolchain.toml 并与 dougong 对比时
  那么 1.99.1 补丁级一致且含 wasm32-wasip2 与 musl target

场景: lints 配置与 dougong 对齐
  当 对比 workspace lints 配置与 dougong 的 [workspace.lints] 时
  那么 rust lints 与 clippy 组合一致且未放宽

Rule: REQ-FR-QUALITY-3 — 命名硬约束

场景: 配置键 kebab-case
  假设 新增自有配置键
  当 校验键名时
  那么 键名为 kebab-case，dsh 协议 wire 字段豁免保持原拼写

场景: Rust 标识符无 camelCase
  当 对全部 crate 运行标识符扫描与 clippy 时
  那么 零 camelCase 标识符，serde rename 对接的 wire 原值除外

Rule: REQ-FR-QUALITY-4 — 对照测试锚点

场景: golden session 回放一致
  假设 构造与 dsh 同场景的 v4 会话文件
  当 deriveMessages 重建冻结请求时
  那么 与 dsh 产出的冻结请求一致

场景: 会话日志逐事件同型
  假设 同一任务在同模型同参数下分别由 fly-rafter 与 dsh 执行
  当 比对双方会话日志时
  那么 工具名、事件类型与字段名 diff 为空

场景: wire 一致性三面
  当 分别运行 --json 事件流、SDK 帧、ACP 帧的 wire 一致性测试时
  那么 三者与 dsh 基准一致

Rule: REQ-FR-QUALITY-5 — 文档即生成物

场景: 生成式文档无漂移
  当 重新生成 tool-catalog 与 config-catalog 等价物时
  那么 与入库版本 diff 为空

Rule: REQ-FR-QUALITY-6 — 禁止放宽

场景: 放宽 lint 的变更被拒绝
  假设 变更引入 lint 放宽或断言弱化
  当 质量门运行时
  那么 门禁失败且不得合并

Rule: REQ-FR-QUALITY-7 — 缝与事件名逐字

场景: 缝名与事件名清单逐字
  当 列举全部服务缝与事件名时
  那么 与 PRD 4.4 清单逐字一致

Rule: REQ-FR-QUALITY-8 — Invoker 消费约束

场景: ctx.get 消费被禁
  当 扫描全部插件 crate 源码时
  那么 不存在以 ctx.get 消费 Invoker 的用法

Rule: REQ-FR-QUALITY-9 — 依赖方向

场景: 领域 crate 依赖入口即失败
  假设 某领域 crate 引用了入口 crate
  当 运行结构检查时
  那么 检查失败，门禁不通过

## Dependencies

None.

## Source Trace

- doc/prd.md#21: 质量门、测试组织、对照测试、文档即生成物
- doc/prd.md#5.1: 命名规范硬约束
- doc/prd.md#1.4: 设计原则 5（质量门零容忍）
- confidence: high

## Open Questions

None.
<!-- /agent-spec:requirement -->

<!-- agent-spec:requirement id=REQ-FR-SCOPE title="范围排除与偏差治理" tags=scope,exclusions source=doc/prd.md -->
## Problem

PRD 第 19 节记录了用户确认的裁剪与有意偏差。为防止范围蔓延与静默偏差，以 MUST NOT 形式固化，并要求新偏差先记录后确认。

## Requirements

- [REQ-FR-SCOPE-1] MUST NOT 实现 PRD 19.1 排除清单全部条目：语音输入全家；Agent Teams 9 工具；浏览器/计算机使用 6 工具；workflow、ralph、present、create_worktree、webhook；hooks 桥（Claude Code / Codex hooks.json）；遥测（OTel、匿名用户 ID、dsh_session_log 回传）；HMR 热重载；插件市场 plugin_market；Electron 桌面端；PTC Python 后端；Python SDK 客户端；会话格式迁移链 v0→v3；子代理 claude-code / codex 委派桥。
- [REQ-FR-SCOPE-2] 有意偏差 MUST 限于 PRD 19.2 已记录清单（!!js 受限子集、品牌/命名替换、配置键 kebab-case，及 2026-10-21 源码核对产生的勘误与补遗记录条 5）；实现中发现新的不可避免偏差 MUST 先记录到 PRD 19.2 并经用户确认后才可继续。
- [REQ-FR-SCOPE-3] 与 DeepSeek 官方 API 交互 MUST 仅使用标准 Anthropic Messages 协议；MUST NOT 实现 dsh_session_log 回传、dsh_plugin_packages 上报与 x-deepseek-harness-* 头。
- [REQ-FR-SCOPE-4] dsh 源码漂移 MUST 视为显式决策：PRD 锚定 v0.2.1-rc.2，参照物路径固定。

## Scenarios

Rule: REQ-FR-SCOPE-1 — 排除清单

场景: 排除项不出现于装配
  假设 全部内置插件注册完成
  当 列举插件与工具名时
  那么 不含 PRD 19.1 排除清单任一名称

Rule: REQ-FR-SCOPE-2 — 偏差治理

场景: 新偏差先记录后实施
  假设 实现中发现新的不可避免偏差
  当 尚未记录到 PRD 19.2 并经用户确认时
  那么 该偏差不得落地

Rule: REQ-FR-SCOPE-3 — 无私有 wire

场景: 官方 API 交互无私有扩展
  当 检查全部对外 HTTP 请求与代码时
  那么 无 dsh_session_log、dsh_plugin_packages 与 x-deepseek-harness 前缀头

Rule: REQ-FR-SCOPE-4 — 参照物锚定

场景: dsh 版本锚定
  当 检查参照物路径与版本时
  那么 锚定 v0.2.1-rc.2 与固定路径，升级需显式决策

Rule: REQ-FR-SCOPE-1 — 排除工具负向枚举（catalog 生成）

场景: 排除工具 ralph 不存在
  假设 全部插件注册完成
  当 列举工具与插件名时
  那么 不存在 ralph

场景: 排除工具 workflow 不存在
  假设 全部插件注册完成
  当 列举工具与插件名时
  那么 不存在 workflow

场景: 排除工具 team_task 不存在
  假设 全部插件注册完成
  当 列举工具与插件名时
  那么 不存在 team_task

场景: 排除工具 spawn_teammate 不存在
  假设 全部插件注册完成
  当 列举工具与插件名时
  那么 不存在 spawn_teammate

场景: 排除工具 wait_agent 不存在
  假设 全部插件注册完成
  当 列举工具与插件名时
  那么 不存在 wait_agent

场景: 排除工具 stagehand 不存在
  假设 全部插件注册完成
  当 列举工具与插件名时
  那么 不存在 stagehand

场景: 排除工具 create_worktree 不存在
  假设 全部插件注册完成
  当 列举工具与插件名时
  那么 不存在 create_worktree

场景: 排除工具 present 不存在
  假设 全部插件注册完成
  当 列举工具与插件名时
  那么 不存在 present

场景: 排除工具 plugin_market 不存在
  假设 全部插件注册完成
  当 列举工具与插件名时
  那么 不存在 plugin_market

## Dependencies

None.

## Source Trace

- doc/prd.md#19.1: 排除清单（用户确认裁剪）
- doc/prd.md#19.2: 有意偏差 3 条与治理规则
- doc/prd.md#19.3: wire 层声明
- doc/prd.md#22: dsh 源码漂移对策
- confidence: high

## Open Questions

None.
<!-- /agent-spec:requirement -->
