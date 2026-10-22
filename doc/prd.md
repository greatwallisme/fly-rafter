# Fly Rafter（飞椽）产品需求文档

| 项 | 值 |
|---|---|
| 版本 | v1.1（2026-10-21 源码勘误与补遗） |
| 日期 | 2026-10-21 |
| 状态 | 定稿（需求访谈完成，全部决策经用户逐项确认） |
| 交付口径 | 本文档为唯一需求真源；实现阶段除本文档显式标注“实现时核对”的条目外，不得引入未记录的行为 |
| 上游参照 | deepseek-harness v0.2.1-rc.2（下称 dsh），本地 `~/project/reference/cordis/deepseek-harness` |
| 基座 | dougong 0.2.0（下称“内核”），本地 `~/project/dougong`，crates.io 已发布 |

---

## 1. 产品概述

### 1.1 定位

Fly Rafter（飞椽，`fr`）是一个以 dougong 组合内核为基座的 AI agent harness 系统，行为与协议 1:1 对标 deepseek-harness。它以**全部组件均为插件**的形态交付：agent 主循环、工具、权限、会话存储、模型接入、各入口界面，无一例外都是插件；宿主二进制 `fr` 只承担两件事——声明式装配（dougong loader + reconcile）与事件泵。

### 1.2 名称

- 产品名：Fly Rafter（中文：飞椽）
- 二进制/命令名：`fr`
- 家目录：`~/.fly-rafter/`（结构与 dsh 的 `~/.dsh/` 同构：`sessions/`、`storages/`、`profiles/`）

### 1.3 目标用户

使用终端的开发者（交互 TUI 与一次性 headless 为主），以及通过 ACP / JSON-RPC 协议接入的自动化调用方。

### 1.4 设计原则

1. **协议即会话日志**（承袭 dsh）：模型可见即已记录；每次 LLM 请求必须能从会话日志确定性重建（`deriveMessages()`）。请求消息从日志投影并冻结，不经内存数组直传。
2. **一切皆插件**（dougong 语义承袭 cordis）：服务贡献、事件监听、可逆 effect、响应式依赖收敛。dougong 是 cordis 的 Rust 复现，dsh 本身构建在 vendored cordis 上——因此 fly-rafter 的结构映射是天然的：**dougong 替代 cordis，fly-rafter 插件集替代 dsh 的 packages**。插件装载形态仅 dylib 与 wasm 两种（4.3，2026-10-22 用户裁决）：第一方插件全部为 C-ABI dylib，wasm 仅承载第三方沙箱扩展；fr 装配不含任何 builtin 形态。
3. **1:1 口径 = 行为与协议对齐**：功能集合、工具名称与语义、提示词文本、配置格式、会话格式逐项对齐原版；内部结构按 dougong 插件模型重造，不镜像原版 TypeScript 模块划分。
4. **fail-closed**：审批链无应答者、沙箱不可用、依赖未声明——一律拒绝，绝不静默放行。
5. **质量门零容忍**：fmt / clippy（-D warnings）/ test / doc 四关全绿，不接收任何警告与错误。

### 1.5 非目标

- Electron 桌面端
- 语音输入、Agent Teams、浏览器/计算机使用、workflow/ralph/present/worktree/webhook、Claude Code 与 Codex hooks 桥、遥测（OTel、匿名 ID、会话日志回传）、HMR 热重载、插件市场（详见第 19 节）
- dsh 私有 wire 扩展（`dsh_session_log` 回传等，见 19.3）
- 会话格式 v0–v3 迁移链（只实现 v4）
- 与 dsh 产生的会话文件互操作（格式结构 1:1，但 harness 标识不同，不承诺互读）

---

## 2. 参照物与权威规格

本 PRD 在“协议契约级”条目上直接引用 dsh 仓库文件作为行为权威规格，不重复抄录。实现时遇到本文档未展开的细节，以这些文件为准：

| 领域 | 权威文件（dsh 仓库内路径） |
|---|---|
| 总体架构与 turn 伪代码 | `docs/architecture.md` |
| 全部工具 schema | `docs/tool-catalog.md`（由脚本从真实上下文生成并验证） |
| 全部插件配置 | `docs/config-catalog.md`（同上） |
| 默认装配与默认值 | `packages/bundle/base/cordis.patch.yml`（529 行） |
| 会话事件全集 | `packages/core/session/src/types.ts` |
| 工具执行管线 | `docs/tool-execution-pipeline.md`、`packages/core/tools/src/index.ts` |
| 权限审批 | `packages/interaction/user-approval/src/index.ts`（fail-closed 契约 ：55/:62/:283-291，已一手核实） |
| 子系统规格 | `docs/subsystems/*.md`、`docs/agent-lifecycle.md`、`docs/event-producer-consumer.md` |
| headless 形态 | `packages/bundle/headless/`（cordis.patch.yml、src/runner-internals.ts、src/startup-internals.ts） |
| SDK 协议 | `packages/sdk/protocol/src/types.ts`、`packages/sdk/protocol/src/transport.ts`、`packages/sdk/server/src/server.ts` |
| ACP 面 | `packages/acp/acp/README.md`（Protocol contract 表） |
| PTC | `docs/subsystems/ptc-runtime.md`、`packages/core/tools/src/ptc.ts` |
| 内核语义 | dougong `doc/research/dougong-design.md`（D4–D15、R1–R4）、`skills/dougong-plugin-dev/SKILL.md` |
| 源码精读笔记 | 本仓库 `doc/dsh-source-notes.md`（四模块 file:line 证据；正文各节标注 dsh 文件行号的细节均出于此） |

---

## 3. 术语表

| 术语 | 含义 |
|---|---|
| turn / step | 一次完整回合 / 回合内的一个请求-工具执行周期（与 dsh 语义一致） |
| SessionEvent | 会话日志中的追加式事实记录，判别联合 |
| surface / replace-op | 会话“表面”（模型与 UI 看到的视图）；压缩以 replace-op 重写表面并引用源事件 |
| waterfall | dougong 五种事件分发之一：continuation 链，监听器不拉 `Next` 即否决整条链 |
| realm | dougong 隔离域：域内 provide 不外泄，注入先域内后回退全局默认——承载“每会话能力集” |
| Invoker | dougong 动态通道：`call(method: u32, payload: Vec<u8>)`，跨 dylib/wasm 边界的服务形态 |
| seam | 可替换的服务缝（如 `llm`、`shell`、`sandbox`），插件向其提供实现 |
| PTC | Programmatic Tool Calling，`run_code` 工具的程序化调用形态 |
| strip-only | Node 原生 TypeScript 类型擦除执行模式，仅接受可擦除语法 |

---

## 4. 总体架构

### 4.1 内核映射

| dsh（TypeScript） | fly-rafter（Rust） |
|---|---|
| vendored cordis 内核 | dougong 0.2.0（git 依赖；本机开发期可叠 `[patch]` path 覆盖；内核改动在 dougong 仓库进行并随里程碑发版 crates.io） |
| cordis.patch.yml 分层补丁（cordis **原生**能力） | dougong **不提供**分层补丁——其 loader 只接受整份 JSON 文档、整文档替换 + 幂等收敛（dougong 设计裁决 D9，`dougong-loader/src/lib.rs`）。分层 patch 语义（有序栈、按 id 整行覆盖）由 **fr-config 在 fr 层重建**（6.2），编译为 dougong loader JSON 文档后经 `Reconciler::replace + apply` 收敛 |
| 插件向共享 Context 贡献服务 | `ctx.provide` / `ctx.require` / `Injection` 声明 |
| 五种事件分发（emit/parallel/serial/waterfall/bail） | `ctx.emit/parallel/serial/bail/waterfall`（同名同语义） |
| 可逆 effect / HMR unwind | dougong effect/LIFO（HMR 裁剪，但可逆性仍是卸载正确性的基础） |
| profile = bundle 有序栈 | 装配文档（第 6 节）经编译器展开为 dougong `Reconciler` 期望态 |

直接复用的内核能力：waterfall 否决实现策略 guard（审批）；realm 实现每会话能力集；Invoker 动态通道实现跨 dylib/wasm 边界服务；`internal/update` waterfall 作为未来热更新的预留扩展点（本期不暴露）。

### 4.2 宿主形态

`fr` 宿主二进制仅含：

1. **装配器**：解析 `fr.patch.yml` 有序分层 → 按 id 整行覆盖展开为单份装配 → 编译为 dougong loader `Config`（source 仅 dylib/wasm 两类；loader 的 builtin 表在 fr 装配中从不使用；dougong 本体无 YAML/分层/行覆盖概念，见 4.1、6.2）→ `Reconciler::replace + apply` 幂等收敛；
2. **事件泵**：驱动 tokio runtime、转发入口信号（headless 任务 / TUI 按键 / ACP·SDK 协议帧）到对应入口插件的监听器；
3. **入口分发**：按子命令装配对应入口 profile（headless / tui / acp / sdk / web）。

**agent 主循环本身是插件**（决策 3）：`agent-loop` 插件 inject `llm`、`tools`、`session-persistence`、`system-prompt` 等服务，监听输入事件驱动 turn/step；宿主对循环零感知。任一入口插件只做“把外部输入变成 inbox 消息、把会话事件投影成外部输出”。

### 4.3 插件形态与信任边界（2026-10-22 用户裁决：废除 builtin 形态）

插件仅两种装载形态，共用一份语义契约（dougong 五步契约：声明 → 激活 → 服务 → 监听 → 释放，跨边界经 descriptor + C-ABI 导出表表达）：

| 形态 | 用途 |
|---|---|
| C-ABI dylib（cdylib，内核家族仅依赖 dougong-abi） | **fly-rafter 全部第一方插件 + 第三方可信扩展**。第一方组件（agent 主循环、工具、审批、沙箱、会话、三 provider、各入口等）无一例外为 dylib 工件，经 loader 文档 `{"kind":"dylib","path"}` 按名解析装载；代码镜像进程内永不卸载（dougong D5/R1，teardown-only），换版本即重启进程。形态铁律（dougong dylib 参考）：插件 crate 从内核家族只依赖 `dougong-abi`，框架语义全部宿主侧（inverted API）；inject/provide 来自插件 descriptor，不在装载文档声明 |
| wasm 组件（wasm32-wasip2） | 第三方沙箱扩展（第一方不使用）；零能力 WASI，fuel/epoch/内存帽；经 WIT world `dougong:kernel/host` 与宿主交互。“零能力”的精确含义（dougong `dougong-wasm/src/engine.rs:84-88`，已一手核实）：wasip2 guest 必然声明 `wasi:io` 导入，宿主 linker 为其挂载**零能力 WasiCtx 的惰性定义**——能力面为零，导入声明不为零；WIT world 本身不导入任何 WASI 能力。零能力 WASI 无网络与文件系统，llm provider、mcp、web、session 落盘等组件在 wasm 内无法工作（除非宿主逐调用代理，成本不可接受）——这是第一方全 dylib 的硬约束 |

第一方全 dylib 的裁决依据（2026-10-22）：同一信任域内 wasm 只增加序列化与宿主代理成本；零能力 WASI 使需要网络/落盘的第一方组件在 wasm 内无法工作。loader 的 `builtin` 表在 fr 装配中**从不使用**（该内核能力保留只是 dougong 平台事实，不构成 fr 需求）。

全部插件（第一方与第三方、dylib 与 wasm）经 Invoker 通道接入服务：harness 定义跨边界契约 = 事件名 + JSON 载荷 schema + 方法号常量表（模式承袭 dougong `greeter` 示例：descriptor 声明 + 能力自检）。

### 4.4 服务与事件契约总则

- 服务缝名与事件名**与 dsh 逐字一致**（`llm`、`tools`、`fs`、`shell`、`terminals`、`subprocess`、`sandbox`、`web`、`lsp`、`jobs`、`subagents`、`goals`、`session-title`、`session-persistence`、`spill-store`、`mcp-resources`；事件 `agent/pre-step`、`agent/request`、`agent/request-error`、`agent/assistant-stream`、`agent/turn-stopping`、`tools/pre-execute`、`tools/execute`、`tools/post-execute`、`tools/pre-execute` 的 ask 语义等）。
- 会话事件类型名与字段**与 dsh 逐字一致**（第 7 节）。
- 仅品牌级标识按第 5 节适配表替换。
- **Invoker 消费统一经 `ctx.require::<Invoker>`，禁止 `ctx.get`**（实现约束）：require 带声明检查，与内核“未声明依赖即拒绝”的能力基线一致；跨 dylib/wasm 边界的服务调用一律走此通道（dougong `invoke.rs`，已一手核实）。
- **跨边界载荷形状（2026-10-22 裁决随附）**：跨 dylib/wasm 边界的一切服务与事件载荷为 JSON 字节（Invoker 通道，method u32 选择子）；共享 wire 形状由纯 serde 库 `fr-wire` 定义，各插件工件各链一份副本（仅共享形状定义，不跨边界传指针/状态）；会话内热路径（assistant-stream chunk）按第 22 节对策批量分发。

### 4.5 Workspace 布局（实现基线）

```
fly-rafter/
  Cargo.toml            # workspace, resolver 3, edition 2024
  rust-toolchain.toml   # 与 dougong 对齐钉死（1.99.1 补丁级）
  crates/
    fr                  # 宿主：装配器 + 事件泵 + 入口分发（bin）
    fr-config           # [宿主库] fr.patch.yml 分层解析/校验/编译为 loader JSON；配置 schema 声明框架；.env 分层
    fr-wire             # [宿主无关库] 共享 wire 形状（纯 serde 类型，各插件工件链接副本，指针不跨界）
    fr-session          # SessionEvent 全集、JSONL+zstd 持久化（v4）、surface/replace-op、deriveMessages、fork
    fr-agent-loop       # agent 主循环插件
    fr-tools            # 工具注册表、schema、执行管线、guards
    fr-tools-builtin    # 内置工具集（第 17 节全部；"builtin" 指工具集归属，非插件形态）
    fr-ptc              # run_code：node 子进程 wire 协议与桥接
    fr-system-prompt    # 有序 section 组装、{{var}}、AGENTS.md
    fr-approval         # ApprovalService + 权限预设
    fr-sandbox          # 沙箱策略（read-only/workspace-write/danger-full-access）与执行体
    fr-compaction       # 压缩、剪枝、图片卸载、spill
    fr-llm              # LlmAdapter seam、StreamChunk、请求派生
    fr-llm-deepseek     # DeepSeek 官方（Anthropic Messages 协议）
    fr-llm-openai       # OpenAI-compatible（可配 base-url）
    fr-llm-anthropic    # Anthropic 官方
    fr-mcp              # MCP 客户端（stdio + Streamable HTTP）
    fr-subagent         # 同进程 spawn/fork（子 realm）
    fr-subagent-acp     # ACP 委派桥（与 fr-acp 共享 ACP 客户端代码）
    fr-commands         # 斜杠命令框架 + 内置命令
    fr-shell            # bash 执行 seam + 子进程/作业管理（dougong 缺口）
    fr-headless         # headless 入口插件
    fr-tui              # TUI 入口插件（ratatui 纯投影层）
    fr-acp              # ACP 服务器入口插件
    fr-sdk              # JSON-RPC 服务器入口插件
```

依赖方向：`crates/` 下除 `fr`、`fr-config`、`fr-wire` 外**全部为 cdylib 插件工件**（内核家族仅依赖 `dougong-abi`，五步契约经 descriptor 表达）；`fr` 只依赖 `fr-config` 与入口插件（经 dylib 工件装载）；任何领域 crate 不得依赖入口 crate；fr-config/fr-wire 为普通 rlib；插件之间只经服务缝与事件解耦。

---

## 5. 命名与环境适配总表

| dsh 原值 | fly-rafter 值 | 性质 |
|---|---|---|
| `dsh` 命令 | `fr` | 品牌 |
| `~/.dsh/` | `~/.fly-rafter/` | 品牌 |
| `DSH_PERMISSION_MODE` | `FR_PERMISSION_MODE` | 品牌 |
| `dshHomePath()` | `FR_HOME`（默认 `~/.fly-rafter`） | 品牌 |
| 身份句 `You are an AI agent powered by DeepSeek Harness.` | `You are an AI agent powered by Fly Rafter.` | 品牌唯一文本改动；语义与默认值不变：`include-harness-identity` 默认 true、`persona-prefix` 默认 `''`（键名适配见配置键行） |
| SDK `serverInfo.name = 'deepseek-harness-sdk-runtime'` | `'fly-rafter-sdk-runtime'` | 品牌（wire 稳定，一经发布不改） |
| ACP `agentInfo = 'deepseek-harness-acp/0.0.1'` | `'fly-rafter-acp/0.0.1'` | 品牌 |
| stderr 前缀 `dsh: reasoning:` / `dsh: <code>: <message>` | `fr: reasoning:` / `fr: <code>: <message>` | 品牌 |
| `cordis.patch.yml` | `fr.patch.yml`（行格式逐字同构） | 品牌 + 一处语义适配（见下） |
| patch 行内 `!!js` 表达式 | 受限声明式条件子集（2026-10-22 勘误 Q3A）：`env.NAME ?? 默认值` 链、相等比较（`==`/`===`）、`platform === 'linux'|'macos'|'windows'` 与平台三元、裸 env 值读取、字面量、`profile-context` 布尔（dsh `ctx.get('profileContext')` 的 fr 等价物：入口/profile 上下文存在性，支持 `!` 取反）；**不支持任意 JS** | 有意适配（Rust 无 JS 求值器）；dsh 实际用法盘点 22 处（base 12、web-app 3、headless 1；子集外：sdk-app `getBuiltinModule` 路径解析类 6 处不做——其对象 office/node 捆绑路径属桌面打包面，19.1 已裁；出处 bundle patches 全量盘点 + `vendor/include/src/index.ts:57-127`） |
| 插件配置键 camelCase（dsh schemastery 键，如 `includeHarnessIdentity`、`timeoutMs`） | 一律转 kebab-case（`include-harness-identity`、`timeout-ms`），Rust 侧经 serde rename 对接；不承诺与 dsh 配置文件互换 | 有意适配（用户裁决 2026-10-21，见 19.2） |

除上表外，一切事件名、服务名、工具名、配置键、提示词文本逐字 1:1。

### 5.1 命名规范（Rust 风格，硬约束）

- **Rust 代码标识符**：变量/函数/字段一律 snake_case，类型与 trait 一律 UpperCamelCase，常量一律 SCREAMING_SNAKE_CASE；**禁止 TS 风格 camelCase 标识符**（如 `allowList`、`sessionId` 不得作为 Rust 字段名——与 dsh 的 JSON 键同名时，Rust 侧字段用 snake_case 并以 serde rename 对接）
- **fly-rafter 自有的用户可见命名**：CLI flag、配置键、crate 名用 kebab-case（如 `allow-list`、`permission-mode`、`fr-session`）；环境变量用 SCREAMING_SNAKE（`FR_` 前缀）
- **dsh 原有键名的处理**：服务名、事件名、会话事件字段、工具 schema（含工具参数名）、SDK/ACP wire 参数属于逐字对齐的协议内容，保持 dsh 原拼写，不适用本条；**插件配置键不豁免**——dsh 的 camelCase 配置键一律转 kebab-case（见第 5 节适配表），Rust 侧以 serde rename 对接协议/存储原值；引用 dsh 原键名的对照文字保持原样

---

## 6. 装配与配置系统

### 6.1 分层模型（与 dsh 一致）

装配 = 有序 bundle 栈；bundle = `fr.patch.yml` 补丁行序列。层次自底向上：

1. 入口默认 bundle（headless/tui/acp/sdk/web 各自的内置清单，角色同 dsh `packages/bundle/*`）
2. profile bundle 栈（`fr --profile <name>`，`fr <name>` 为缩写）
3. profile patch
4. 家目录 patch（`<FR_HOME>/fr.patch.yml`，2026-10-22 Q1 勘误：原括号 `~/.fly-rafter/profiles/` 与 dsh 真源 home 根不符）
5. `--patch <file>` 命令行补丁

**装载文件语义（2026-10-22 Q1-Q5 裁决，对齐 dsh app-boot `profile.ts` / `profile-context.ts` / `index.ts`）**：profile = `<FR_HOME>/profiles/<name>/` 目录，清单 `profile.yml`（`bundles:` 列表仅引用宿主内置命名 bundle，未知名 → skipped 警告非致命）；profile patch = `<profile>/fr.patch.yml`（可选，缺文件 = 空层）；家目录 patch = `<FR_HOME>/fr.patch.yml`（可选，缺文件 = 空层）；`--patch` 为必需文件，缺文件即启动失败；任何 patch 解析错误 fail-closed 终止启动；profile 名校验拒绝空名、含 `/` 或 `\`、`.`、`..`、`node_modules`；已知入口名的 profile 首次使用自动初始化（清单 + 空白 patch 模板，绝不覆盖既有文件；fr 模板 bundles 为空列表——入口默认 bundle 恒在层 1，不重复挂载，dsh 模板 bundles 的职责由层 1 等价承担），未知名报错；profile 根文件 = `<profile>/fr.yml`（2026-10-22 Q23A 裁决，dsh `cordis.yml` 品牌映射）：注释头 + 空 entry list，每次装载（boot 与 dump 共用同一 prepare 路径）**无条件重写**——装配树全为补丁层组合、根文件恒为空基线，用户编辑对象是 `fr.patch.yml` 而非本文件（dsh 同构语义：根文件只为给 dump 与 boot 提供同一个可读 base 锚点）。入口默认 bundle 以嵌入 YAML 文本常量进 fr-config，与用户 patch 走同一解析路径；M1 为最小 headless 集（仅装 M1 存在的 crate、config 值逐条对齐 6.4 表），完整 dsh 品牌映射抽取随后续里程碑。

**合并语义（2026-10-22 勘误对齐 dsh 源码 `vendor/include/src/index.ts:57-127`，Q1/Q2 裁决）**：行内 `id`/`name` 是**匹配器**；其余键**逐键覆盖**（`config` 作为单键整体替换、不深合并；只写 `disabled` 的覆盖行仅翻转 disabled、其余键不动）；覆盖行未命中 id → 警告并跳过（不报错、不致命）；`name` 不匹配 → 警告并跳过；行序 = 首次插入序（覆盖不改变行位置）；`insert` 行向装配根插入新行（Q2：fr 不做 `group` 嵌套，仅根级插入）；后续层可覆盖先前层插入的行。

### 6.2 补丁行格式

```yaml
- insert:                    # 插入组：向装配根插入新行（fr 无 group 嵌套）；定义行一律经 insert 落地
    - id: llm-primary
      name: fr-llm-deepseek  # 插件名（经解析规则映射为 dylib/wasm 产物，见下）
      config:
        model: deepseek-flash
    - id: headless-startup
      name: fr-headless
      config:
        tools-mode: !!js "process.env.FR_TOOLS_MODE"
- id: approval-policy        # 纯覆盖行示例：仅翻转 disabled，不动 name/config（dsh base patch 即整体一个大 insert，后续层按键覆盖）
  disabled: true
```

编译产物为 dougong loader 文档：

```json
{ "plugins": [ { "name": "llm-primary", "source": {"kind": "dylib", "path": "<libdir>/libfr_llm_deepseek.so"},
                 "config": {"model": "deepseek-flash"}, "isolate": null } ] }
```

**产物解析规则**（编译期定形态）：行 `name` 含路径分隔符或以 `.so`/`.wasm` 结尾 → 按字面路径（2026-10-22 Q6 勘误：insert 行 name 以 `./`/`../` 开头者，装载期 MUST 锚定到所在补丁文件目录转绝对——对齐 dsh `anchorInsertedPluginNames` index.ts:550-561；覆盖行 name 为匹配器，不解析不锚定；无 `./`/`../` 前缀的含分隔符 name 保持字面路径）；否则按 slug（`-`→`_`）解析：在 `FR_PLUGIN_PATH` 各目录（冒号分隔、序即优先级）→ 安装库目录 `<prefix>/lib/fr/` 中依次查找 `lib<slug>.so`（dylib）与 `<slug>.wasm`（wasm），先命中者定形态；两处都未命中 → 该行进入 `Report::failures`（fail-closed，按 6.4 启动失败策略处理）。编译产物 MUST NOT 出现 `builtin` source。

`inject/provide` 不在文档声明——来自插件 descriptor（dougong 铁律）。`isolate` 列承载 realm：会话级装配在会话 realm 内放置同名插件。

**实现边界（dougong 侧事实，已亲自核实源码）**：dougong loader 仅支持上述扁平 JSON 文档（`Config::parse` 只验 JSON 形状、名字唯一非空、config 为对象或 null，`dougong-loader/src/lib.rs:71-126`），reconcile 结果只取决于最终文档、与到达路径无关（`replace` + `apply`，diff 驱动 insert/update/retire；isolate 变化 = retire + re-insert）。**分层、行覆盖、YAML 在 dougong 中一概不存在**——它们全部是 fr-config 的职责，且编译产物必须是上述合法 loader 文档；`Report{inserted, updated, retired, unchanged, failures}` 与失败不阻塞其余槽的语义直接沿用内核。dougong `harness_demo.rs` 五阶段（Pending 停等 → 全量收敛 → realm 解析 → 单行 config 热换 → 删行精确 retire）是 fr-config 行为对齐的参考实现。

### 6.3 配置 schema 校验（dougong 缺口，fr-schema 补齐）

dougong 只验 JSON 形状（`require_object`）。fr 的 config schema 真源在**宿主侧**、以**声明式描述树为单一真源**（2026-10-22 Q7 裁决，等价 dsh schemastery 的校验+投影双重角色）：每插件的 config schema 为 fr-schema 描述符（受限子集，同构 dsh native graph 骨架 type/meta/dict/list/value；M1 面 = string/number/integer/boolean/object/array/dict，meta 注解仅 default 与 description；子集外声明在描述符层即不可构造——fail-closed）。同一声明派生两侧能力：`validate`（装配期与激活期共用）与 `to_json_schema`（JSON Schema 2020-12 关键词面，对齐 dsh `ConfigJsonSchemaObject`，config-schema/types.ts:7-26）；插件 config 的 Rust 类型 + serde 解码仅为消费便利层，非校验权威（dsh 同为单一声明源：schemastery 对象同供校验与 dump 投影）。

schema 集中声明于 **fr-schema 库**（2026-10-22 Q8 裁决：独立 crate，纯声明+校验+投影、无 IO；宿主直链，插件工件静态链接同一 crate 作激活期再校验的链接副本），fr-config 依赖它做装配期校验：按插件 slug 查注册表，**查无 schema 跳过该行校验**（2026-10-22 Q9 裁决，对齐 dsh absent 标注 config-schema/types.ts:42——第三方工件宿主侧本无其 schema，不构成装载障碍）；有 schema 且校验失败 → `Report::failures` 不阻塞其他槽；插件工件在激活体顶部用链接副本对 config **再校验**（双侧校验），激活期校验失败按插件装载失败 fail-closed。`fr dump-config` 输出合成装配树（YAML 文本逐字节同型对齐 dsh `--dump-config`，2026-10-22 Q18-Q20 裁决：`# == <源文件>[, patched by <层>…]` 分隔注释按连续同源行分组、`!!js` 表达式逐字打印不求值、快照算法 = 层前缀各做单次 applyEntryPatches 同参展平、整篇可再 parse 且等于同参启动挂载结果；错误三连 `fr: failed to read config` / `fr: failed to parse config` / `fr: config <path> must be a top-level YAML array of entries`），`fr dump-config-schema` 输出合成装配的 JSON Schema 2020-12 文档（对齐 dsh `--dump-config-schema`：`$schema` 2020-12 + 扩展键定名 **`x-fr`**（Q18A，对齐 dsh `x-cordis`：{profile, complete, entries, diagnostics, patchSchema}）+ entry 五态 schema/partial/absent/unsupported/error + diagnostics 逐条 stderr（`fr: <level>: [<path>] <message>`（无 path 时 `fr: <level>: <message>`））+ complete 为假时退出码 1）；三个 dump 态互斥且拒收 app 位置参数；`--default-only` 为恢复诊断（Q19A，对齐 dsh `--dump-default-config`）：仅 bundle 层、不读不解析 profile patch 与家目录 patch（坏 patch 文件亦可 dump）、不接受 `--patch`；`from-default-profile`（Q20A，对齐 dsh `--from-default-profile`）= 以出厂模板**替换**自定义 profile 后装载（区别于首用初始化绝不覆盖），dump 与 boot 共用，CLI 旗标宿主期落地、数据面 API 预留。这两个目录文档在 fly-rafter 同样由真实装配生成并验证——承袭“文档即生成物”。

### 6.4 关键默认值（继承自 dsh base patch，品牌名替换后）

| 项 | 默认值 |
|---|---|
| 默认模型 | provider `deepseek-official`（Anthropic 协议适配），model `deepseek-flash` |
| 沙箱模式 | `FR_PERMISSION_MODE ?? 'workspace-write'`，workspace root = 进程启动 cwd |
| 审批策略 | 非 danger-full-access 时 `'ask'` |
| 会话存储 | `<FR_HOME>/sessions`；storages `<FR_HOME>/storages` |
| 会话标题 | 无 LLM 时回退 5 词/40 字节（上限 80 字节）；LLM 标题 targetWords 5（CJK 10）、maxInput 4096 字节、超时 60s |
| bash 工具 | `timeout-ms` 60000（装配生效值：bash-local schema 缺省 120000、上限 600000，base patch bash-sandbox 行覆盖为 60000——两层并存均复现） |
| 重复工具提醒 | 阈值 `[3, 5, 8]` |
| glob | `sample-over-cap-glob-results: false` |
| todo | `allow-parallel-in-progress: true` |
6.4 之外的默认装配事实（dsh base cordis.patch.yml，品牌映射后同型）：默认装配树 90 行插件 id 集合为 `fr dump-config` 同型 diff 锚点；另有 agent-instructions maxBytes 65536、session-projection-cache 200 事件/5000ms、web searchProvider=deepseek-official 与 fetchProvider=http、tool-web fetch:true 与 searchTimeoutMs 60000、repeat argumentsPreviewChars 500、标题 maxOutputTokens 4096、tools 默认 mode native、storage-domain backend json、permission presets 三档表（read-only/workspace-write/danger-full-access 对应沙箱与审批组合）。

**.env 分层（2026-10-22 用户裁决纳入 fr-config 范围，Q11A/Q12A/Q13A/Q14A 定稿；对齐 dsh app-boot index.ts:238-259 品牌映射后）**：

- 层序与原子性：环境解析次序 = 继承进程 env > 项目 `.env`（自 cwd）> 家目录 `.env`（`<FR_HOME>/.env`）；两文件先全部解析并校验、后统一应用（任一层 bootstrap 违例 → 两文件均不应用，dsh index.ts:244-251）；`cwd == FR_HOME` 时家目录层不读（去重）。
- 双轨机制：应用序 project → user，逐名仅在更高层未设时 materialize 进进程 env（Rust 侧 `set_var` 为 unsafe，仅限启动期单线程、任何线程/异步运行时启动前调用），使 `!!js` env 表达式与子进程 environ 可见；同时返回不可变环境快照（层序 process > project-env > user-env 的逐名来源查询 `get`/`get_from`；消费方在后续里程碑接线，dsh launch-environment 包同构）。`.env` 中已设名 MUST NOT 覆盖更高层已存在的值。
- bootstrap 专名（任何 `.env` 层拒绝、大小写不敏感）：49 个精确名 = `PATH`、`HOME`、`USERPROFILE`、`SHELL`、`NODE_OPTIONS`、`NODE_PATH`、`NODE_EXTRA_CA_CERTS`、`LD_PRELOAD`、`LD_LIBRARY_PATH`、`LD_AUDIT`、`BASH_ENV`、`ENV`、`SHELLOPTS`、`BASHOPTS`、`PERL5OPT`、`PERL5LIB`、`PYTHONSTARTUP`、`PYTHONPATH`、`RUBYOPT`、`RUBYLIB`、`JAVA_TOOL_OPTIONS`、`_JAVA_OPTIONS`、`JDK_JAVA_OPTIONS`、`PYTHONHOME`、`GIT_SSH`、`GIT_SSH_COMMAND`、`GIT_EXTERNAL_DIFF`、`GIT_PAGER`、`GIT_EDITOR`、`GIT_ASKPASS`、`SSH_ASKPASS`、`GIT_CONFIG_GLOBAL`、`GIT_CONFIG_SYSTEM`、`GIT_CONFIG_COUNT`、`EDITOR`、`VISUAL`、`PAGER`、`BROWSER`、`DEEPSEEK_BASE_URL`、`DEEPSEEK_SEARCH_BASE_URL`、`SSL_CERT_FILE`、`SSL_CERT_DIR`、`HTTP_PROXY`、`HTTPS_PROXY`、`ALL_PROXY`、`NO_PROXY`、`REQUESTS_CA_BUNDLE`、`CURL_CA_BUNDLE`、`NODE_TLS_REJECT_UNAUTHORIZED`（dsh index.ts:136-157 逐字；`DEEPSEEK_*` 保留原名——provider 名 deepseek-official 未变）；前缀 = `FR_`、`XDG_`、`DYLD_`、`BASH_FUNC_`。
- 家目录层例外：仅 `HTTP_PROXY` / `HTTPS_PROXY` / `ALL_PROXY` / `NO_PROXY` 四键可设；项目层设代理名仍拒绝（警告文案与 dsh 同型：指向 `<FR_HOME>/.env` 的第二出路）。
- 失败语义：bootstrap 违例 = 该层解析即报错 → 启动终止（fail-closed）；读错误非 ENOENT = stderr 一行警告 + 该层按空层跳过（非致命）；ENOENT = 静默空层。
- 解析语法：Node `util.parseEnv` 方言（# 注释、单/双引号值、双引号内转义与跨行多行值），fr 自实现对齐、以 Node 官方源码为钉测蓝本（Q14A）；dsh 单层 `loadEnv` 变体仅测试夹具使用，fr 不实现（负向钉）。
- home 解析（D0）：fr-config `resolve_home` = 显式 configured > `$FR_HOME`（trim 后空白视为未设）> `~/.fly-rafter`；`~`/`~/` 前缀展开后归一（对齐 dsh home-paths resolveRoot）。

---

## 7. 会话系统

### 7.1 SessionEvent（判别联合，字段与 dsh `packages/core/session/src/types.ts` 逐字一致）

公共字段：`type`、`seq`、`time`、`data`、`ignorable?`、`surfaceOp?`、`sourceEventSeqs?`。

durable 事件全集：

| type | data 要点 |
|---|---|
| `turn/start` / `turn/end` | end 携带 reason：`completed / max-tokens / aborted / error / blocked` |
| `step/start` / `step/end` | |
| `user/message` / `developer/message` / `system/message` | 完整消息；system/message 承载提示词 surface 节点流 |
| `assistant/message` | 内嵌完整带时间戳原始 stream + usage + interrupted 标记 |
| `assistant/attempt` | 失败/重试尝试（不进模型历史） |
| `tool/call` | name + **原始未解析 arguments JSON 字符串** + callId |
| `tool/result` | content + isError + error{name, code, reason} + meta |
| `request/header` | EpochHeader：config + tools + adapterDefaults（reason: initial/resume/change/series） |
| `request/context` | |
| `session/end-seed` | |
| 扩展事件 | `tool/ptc-dispatch-start` / `tool/ptc-dispatch`（parentCallId/subCallId 配对）、`approval/policy` / `approval/asked` / `approval/decided`（插件可声明合并扩展，机制同 dsh） |

消息角色：System / Developer / User / Assistant / ToolResult，均带 MessageSource 判别。ContentBlock：text、reasoning、image、file、tool-call、tool-addition、tool-removal。
assistant/message 内嵌完整带时间戳原始 stream：以四类 record（text-chunks/reasoning-chunks/tool-call-chunks/chunk）打包，dt 存差分时间；id/name 不全的 tool-call-delta 落 raw chunk，block-start/block-end/usage/finish 恒 raw（dsh assistant-stream.ts:107-190）。assistant/attempt 仅存 {turn,step,stream}。approval 与 ptc 五类扩展事件为 log-only：持久化但永不进模型可见表面。usage 跨 attempt 累加，任一 attempt 无样本则该 step 省略 usage（dsh json-stream.ts:266-286）。

**未知必需事件必须拒绝重建**（`ignorable: true` 语义照搬）。

### 7.2 持久化（只实现 v4）

- 文件：`session.v4.jsonl`，可选 zstd 压缩（`session.v4.jsonl.zstd`）；世代命名，**永不改名/删除已发布文件**
- 写路径 = 编码 → 校验 → 独占发布后继文件（link+unlink 禁 rename、逐级目录 fsync、append 失败 truncate 回滚、zstd 撕裂尾恢复完整记录）；SessionHeader：version/id/createdAt/isSeeded/delegationDepth 加可选 cwd/parentSession/origin/agentPreset（无 seedLength 字段且显式拒绝；继承切点在最后一个 tagged `session/end-seed`；dsh session-persistence-jsonl/src/format.ts:113-118、core/session/src/index.ts:98-100）
- 不实现 v0→v3 迁移链（决策）；读到非 v4 文件报明确错误
- 物理不变量：首发=临时文件（wx，0600，fsync）→ link()+unlink 独占发布（禁 rename，EEXIST 互斥）→ 逐级目录 fsync；append 失败必须 truncate 回滚原 size 再抛；zstd 撕裂尾中已刷盘完整记录须恢复重写；写租约（目录内核锁）串行化并发写；文件名世代与头 version 不一致报错，storedVersion 大于当前版本报 unsupported（dsh session-persistence-jsonl/src/index.ts:1282-1461）
- fr 裁剪对照：dsh 有完整 v0→v4 相邻迁移链；fr 只实现 v4，验收对照不得以 dsh 迁移行为为准

### 7.3 surface 与 replace-op

压缩通过 `{op: 'replace', startSeq, endSeq}` 重写表面，新内容引用 `sourceEventSeqs`；旧内容永久留在日志中，保证确定性重放。system prompt 以 `system/message` 节点流存在于表面，支持 in-history 增量追加（缓存友好）与 first-node 归一化两条路由；空渲染清空活跃尾部 system 节点且 head 被 replace 为空文本（head 亦占一节点，dsh runtime-context.ts:104-110）。
surface 硬约束（dsh surface.ts）：仅 system/developer/user/assistant message 与 tool/result 五类入表面，非表面事件携带 surfaceOp/sourceEventSeqs 即抛错；replace 的 startSeq/endSeq 为表面节点 seq 双闭区间且必须早于当前事件 seq；sourceEventSeqs 必须非空、无重复、全部更小、覆盖全部被遮蔽节点，assistant/message 禁止携带；tool/result 替换只许改 content（深相等比较时双方 content 置 null）；node-0 为 system/message 时只许 system/message 恰好单节点重写；replaceGeneration 与 contentGeneration 双计数；sourceEventSeqs 落盘为折叠编码（连续 run ≥3 折叠为 [start,end]，seq-ranges.ts:18-67）。

### 7.4 deriveMessages

每次请求前从日志投影消息数组并 deepFreeze：按 request/header 世代 + 表面 + 工具增删（developer/message）重建；`tool/ptc-dispatch*` 一律忽略（子调用不进模型历史）。**冻结请求必须可逐字节重现**——这是对照测试的锚点。
投影细则：空 assistant（承载 max-tokens usage）投影为 null；缓存按 contentGeneration 变化整表重建、纯尾增长只投影新节点（dsh session/src/index.ts:984-1005）。request/header canonical 形态：空 tools 省略字段；adapterDefaults 仅在 reasoningEffort/maxTokens 之一显式为 true 时保留；相等比较对 tool schema 用 JSON.stringify（顺序敏感）——顺序不同即视为 change 落新 header（dsh request-header.ts:21-52）。

### 7.5 fork

`session/fork`：以 sourceEventSeqs 前缀派生新会话（机制同 dsh `fork.ts`）；子代理与 fork 会话不可被 `--session-id` / `session/resume` 采纳。
实现要点：该门禁在 dsh 的 core/session 内不存在（host 层 assertAdoptable 才有），fr 必须在 resume 入口自行实现四拒：preset 不匹配、subagent/fork、无 cwd、cwd 与当前不一致，另加 live Agent 占用拒绝与写租约后二次复查（dsh headless index.ts:212-292）。

### 7.6 abort 补偿

abort 时为未派发的工具调用补合成错误 `tool/result`，保证重放合法（同 dsh ToolCallRecovery 语义）。
两级补偿精确形态：调度器跳过 = ABORTED_BEFORE_DISPATCH（name AbortError，文案 'Error: tool call aborted before dispatch'）；步级/崩溃恢复 = TOOL_NOT_STARTED（未启动）或 TOOL_OUTCOME_UNKNOWN（已启动结果未知，name ToolNotStartedError/ToolOutcomeUnknownError，文案取 CLOSER_TEXT 精确常量）；恢复事件 seq 从 last.seq+1 连续、time 复用最后真实事件（保证确定性）；step 抛错先补 recovery.results 再重抛，finally 无条件补 step/end（dsh tool-calls.ts:250-260、repair.ts:15-197）。

---

## 8. Agent 主循环（插件）

### 8.1 插件形态

`fr-agent-loop`（dylib 工件）：inject `llm`、`tools`、`system-prompt`、`session-persistence`；provide `agent` 服务（`send / steer / inject / followup` 入 inbox）。每会话一个循环插件 fiber，置于该会话 realm；循环实现仅存在于 fr-agent-loop 工件，宿主代码不含循环逻辑。

### 8.2 控制流（与 dsh `agent-loop/src/agent.ts` 逐步对齐）

1. 输入入 inbox → 唤醒 driver → `while (turn())`
2. **turn()**：append `turn/start` → 循环 `preStep()`
3. **preStep()**：inbox.claim → `system-prompt.assemble()`（sections + contexts + tools）→ 派发 `agent/pre-step` **waterfall**（监听者可改写/拒绝输入；默认把 runtime-context 快照并入 messages）→ 拒绝或空输入直接收尾 turn
4. **step()**：渲染 prompt → `agent/request` waterfall 提出 LlmCallConfig → `llm.prepareCall` 绑定适配器（无适配器容忍中间件路由）→ system prompt 提交 surface（in-history / incapable 路由）→ append `user/message` → 按需 append `request/header`、工具增删 `developer/message`、`request/context` → `deriveMessages()` 冻结请求 → 发起流，chunk 实时 emit `agent/assistant-stream`
5. **流结算**：error/aborted → append `assistant/attempt` + `agent/request-error` waterfall 决定重试或抛错；正常 → append `assistant/message`；`finish.kind == 'max-tokens'` 粘滞结束 step；无 tool-call → completed；有 → 执行工具
6. **工具执行**：按 executionMode 分组（exclusive 串行屏障 / parallel 有界滚动池），逐组走 `tools.execute` 管线（第 9 节）；`concludeTurn` 类调用终结 turn
7. **收尾**：`step/end`；有 next-step 输入（steer/工具结果上下文）继续下一 step；否则 `agent/turn-stopping`（serial）→ `turn/end`；队列有 pending 开新 turn
状态机细则（dsh agent.ts）：相位 idle/maintenance/running，status 仅跨相位发 agent/status。preStep 空输入三分支：waterfall 拒绝→turnEnds={blocked}；step 0 空→completed（占首边界不花模型调用）；turnEnds 已设后空→静默 break 沿用原原因。max-tokens 粘滞不得被 completed 降级；turn-stopping 仅在 turnEnds 已设且 nextStep 队列空时派发；每 turn 换全新 AbortController 使旧信号闩失效。inbox claim：一次性取走全部 next-step、next-turn 至多 1 条；splice 规范化且 message id 跨队列唯一，每次变更加 agent/inbox/spliced。重试结算：error/aborted→append assistant/attempt→agent/request-error waterfall（默认 undefined）→非 retry 抛 LlmError；user/message 仅首次尝试 admitted；NO_ADAPTER 容忍中间件路由（config=proposal 直发），缺 provider/model 直接报错。

### 8.3 失败恢复

step 抛错时补齐缺失 `tool/result`（7.6）；适配器级重试策略由 provider 插件经 `provider_retry_policy` 声明。
provider_retry_policy 结构：mode 'normal'（maxRetries 默认 5，retryableCodes 默认 [EMPTY_RESPONSE, RATE_LIMIT, SERVER, TIMEOUT, TRANSPORT]）| 'always'；backoff initialDelayMs 500 / maxDelayMs 10000 / jitterRatio 0.1 对称抖动；执行方是 llm-retry 插件挂 agent 失败步扩展点，LLM 核心不自执行（dsh retry-policy.ts:222-262,211-215）。

---

## 9. 工具系统

### 9.1 注册与 schema

`tools` 服务（ToolRegistry）：`register`（名称、JSON Schema、executionMode、presentation 元数据、说明 section order 1000–3000）；MCP 工具以 raw JSON Schema 动态注册。schema 类型推断与呈现元模型对齐 dsh `packages/core/tools/src/schema.ts`、`presentation.ts`。

### 9.2 执行管线（对齐 dsh `tools.execute`）

```
createExecution（参数 JSON 快照/冻结）
→ tools/pre-execute waterfall        # allow / deny / {kind:'ask'} 交 ApprovalService
→ guards（单调串，防重复调用风暴）
→ tools/execute waterfall
→ 工具体
→ tools/post-execute
tool/call 在 prepare（pre-execute）之前追加，tool/result 由 contiguous 游标按模型序在 finalize 后提交并 cite callSeq（dsh tool-calls.ts:146-168）
管线细则（dsh tools/src/index.ts）：ptc 坍缩调用（模型直接调用的 run_code 专属工具）在策略管线之前终结（UNKNOWN_TOOL 附"只能经 run_code"路由文本，完全绕过 pre-execute/guards/post-execute）；guards 仅在 pre-execute allow 后求值（global 层先、agent 链 farthest-first，返回首个拒绝、单调不可翻案），guard 拒绝走 post-result 且仍过 post-execute；post-execute 默认 accept，block→isError+feedback，value 与 content 双替换抛 TypeError，失败结果禁 value 替换；finalize 快照+冻结 exec 后再 emit tools/result；成功结果须过 output schema 校验，全部失败归 INVALID_TOOL_OUTPUT。
```

### 9.3 调度

`exclusive` 组成串行屏障；`parallel` 有界滚动池。concludeTurn 语义调用终结 turn；abort 为未开始调用补合成错误结果。
调度细则：isConcurrencySafe 仅严格返回 true 才归 parallel，未声明/异常/非 true 一律 exclusive（fail-closed）；exclusive 组 = 仅首个调用（屏障），其余 parallel 排队；parallel 池上限 = agentLoop.config.maxParallelToolCalls（默认 10，volatile 可调）；每次启动前重读 executionMode（lazy 重分类，registry 变化可形成新屏障）；concludeTurn 仅成功结果可携带（failure 类型不可）。PTC 侧镜像调度：单驱动 lane——先提交队首 settled（含 post-execute）再按容量启动，仅 body 阶段并发、策略阶段全序（dsh tool-calls.ts:86-213、ptc.ts:447-522）。

### 9.4 run_code / PTC（保留，执行基底 = node 子进程，全链路 1:1）

- 程序 = TypeScript async 函数体，**仅可擦除语法**；由全新 Node 子进程以原生 strip-only 模式执行
- wire：4 字节 BE u32 长度前缀 + JSON 体（无换行帧）；硬数值对齐 dsh config.ts:31-44（timeout 120s、max 600s、maxOutputBytes 64MiB、maxOldGenerationSizeMb 512、maxMessageBytes 128MiB、maxPendingCalls 128、graceMs 3s）、有界写队列、流式 stdout、崩溃分类（8 类精确触发条件见 dsh index.ts:200-339）；实现于 fr-ptc（tokio + 子进程）
- 能力面：工具以全局 async 函数注入（`await tools.name(args)`），参数/返回值必须 lossless JSON；`console` 为保留 log 槽，输出计入共享字节预算，超限 → `output-limit`
- 边界：程序不读会话状态，文件系统访问按沙箱策略执行（非绝对禁止，dsh ptc-runtime-node/src/index.ts:48）；进程在沙箱策略执行世界中启动（11 节）；**沙箱 seam 不表达网络限制**（如实承袭 dsh 事实）；`timeout-ms` 预算 → `'timeout'`
- 失败分类全集：`exception | timeout | abort | worker-exit | invalid-output | output-limit | protocol | sandbox-unavailable`，全部 fail-closed
- 绑定名校验：binding global 须 `^[A-Za-z_][A-Za-z0-9_]*$`，拒 ECMAScript 与 Python 保留字并集及 console、__dsh_main__、__builtins__、__name__、__debug__；error member 名另拒 name/message/stack/args/with_traceback/add_note 与 dunder；`__proto__` 不拒绝——以 null-prototype 宿主加 defineProperty 注入普通 own key（dsh bindings.ts:7-53、ptc.ts:686-694）
- 声明生成：从当前注册工具 schema 生成 TS 绑定声明文本，经系统提示词 `TOOLS_SDK` section（order 5000）下发模型；`run_code` 为 registry 保留传输（`mode: ptc`），不参与可过滤能力层
- 子调度：回流完整管线（含审批升级 approveEscalation），append `tool/ptc-dispatch-start|dispatch`；`max-parallel-sub-calls` 默认 10，submission-ordered；沙箱策略切换中止在跑程序
- `execution-mode: persistent-in-memory`（opt-in）：每 state owner 一个受限持久 REPL 进程
- node 缺失：该工具调用 fail-closed 报错，系统其余部分零影响；Python 后端不在范围（19.1）
- 语言 flavor 仅 TypeScript（对齐 `TYPESCRIPT_FUNCTION_FLAVOR` 文本）
- 实现细则：程序包成 `async function __dsh_program__(){...}` 后交 Node 原生 stripTypeScriptTypes（非可擦除语法直接抛错归类 exception），child 侧 AsyncFunction('use strict') 求值。声明生成：工具按 name 字典序、键非裸标识符用 JSON 引号、JSDoc 单行折叠并转义 `*/`、additionalProperties 非 false 追加 Record、敌意 schema 降级 unknown 不抛。子调度：dispatch-start 于真正获得槽位时追加；tool/ptc-dispatch 经 tools/ptc-dispatch-log waterfall（听众抛错 contained 回退原文）；背压 logWork 超过 maxParallel 时 commit 等待；abort 排队未启动者 abandon 不记日志。审批升级 approveEscalation：permissions/justification 成对非空；同模式免审、须严格更宽（目标仅 workspace-write 与 danger-full-access）、rejected/cancelled/unavailable 全抛、allowed-once 仅授本次。persistent-in-memory：每 state owner 一进程、单并发、换沙箱策略须先 release（dsh ptc 各文件）

---

## 10. 系统提示词组装

- 有序 PromptSection / Context 注册 + `{{var}}` 插值 + `tool-order`（对齐 dsh 的 toolOrder，`system-prompt/src/index.ts`）
- SECTION_ORDERS：`HARNESS_IDENTITY: -1000` → `TOOLS_SDK: 5000` → … → `9900`；CONTEXT_ORDERS：`WORKING_DIRECTORY: 100`、`SANDBOX_POLICY: 110`、`APPROVAL_POLICY: 115`（SECTION_ORDERS 全表 32 项在 system-prompt/src/index.ts:131、CONTEXT_ORDERS 全表 7 项在 :170，以源文件为权威）
- 身份句：`You are an AI agent powered by Fly Rafter.`（`include-harness-identity` 默认 true；`persona-prefix` 默认 `''`）
- **其余全部 section 文本逐字取自 dsh**（工具说明、plan-mode 全文、压缩引擎提示词等）；计划模式文案以 base patch `plan-mode` 行为准
- 项目指令：AGENTS.md/CLAUDE.md（含 local 变体）加载后渲染为 user 消息加 system-reminder 帧注入（非 system section；变更与删除以 Updated/removed 语义再注入，dsh agent-instructions/render.ts:315-321）；各工具说明 section 由工具插件注册（order 1000–3000）
- dsh 默认提示词极少（身份句一句 + 工具自带 section + plan-mode + 压缩提示词），fly-rafter 同构
- 组装确定性细则：section 注册 order 升序、同 order 按 name code-unit 序、重名抛错、scoped 同名遮蔽 global。toolOrder：保留标记 `<unlisted-tools>` 恰好出现一次、未列出工具按字典序插入 rest 位、工具 provider 返回保留名即抛。`{{var}}` 变量名限 `^[a-z][a-z0-9_]*$`，畸形/未注册/未定义抛错并指明所在 section，替换值不二次扫描，interpolate:false 保留字面。complete section 至多一个，waterfall 返回后恢复为唯一 prompt section；refreshContext 只重跑 context provider。AGENTS.md 机制：候选 AGENTS.md/CLAUDE.md 加 local 变体，projectRoot 自 cwd 向上找 .git marker；单文件上限 1MiB 超限整文件跳过，maxBytes 渲染预算二分截断到恰好适配，同组去重保留最早；文件变更/删除以 Updated/removed 语义再注入，fs 工具触碰嵌套指令文件触发再投影（dsh system-prompt/src/index.ts、agent-instructions/src）

---

## 11. 权限与沙箱

### 11.1 审批（ApprovalService，对齐 dsh `user-approval`）

- 会话策略 `ApprovalPolicy = 'ask' | 'never'`（默认 ask；danger-full-access 权限模式下 never；`never` 对一切 ask 自动拒绝，dsh `user-approval/src/index.ts:139`）；持久化为 `approval/policy` 事件（log-only 不进模型 transcript，最后一条生效），切换时注入 user-message 通知模型（文案逐字 `The approval policy changed from "x" to "y" (changed by the user).`，dsh user-approval/src/index.ts:184-195）
- `request()` 契约：必须处于 open turn 内（审计对 `approval/asked` + `approval/decided` 必须 turn-enclosed）；应答链（UI answerer）缺席/失败 → **fail-closed `'unavailable'`**，越权返回值一律归一化为 `'unavailable'`（dsh `user-approval/src/index.ts:62` 契约文档句、:55 结果词表 `['allowed-once','rejected','cancelled','unavailable']`、:283-291 兜底与归一化，已一手核实）；唯一授予值 `'allowed-once'`，无持久 allow-list
- 集成：`tools/pre-execute` waterfall 返回 `{kind:'ask'}` → serviceAsk；沙箱升级（如 bash 的 `sandbox_permissions`）走同一 seam
- 实现细则：approval/policy 事件 log-only 不进 transcript，`source:'delegation'` 标记委派种子 override；setPolicy 同值 no-op；hasOpenTurn 从日志尾倒扫 turn/start|turn/end；'never' 在 dispatch 之前由服务自身判 rejected（防 prepend listener 绕过）；结果 waterfall 以 Promise.then 先入链（同步 throw 与异步失败同路 contained）；abort 竞速先赢 resolve('cancelled')，晚到答案丢弃；ApprovalRequestId 每请求 fresh randomUUID；effectivePolicy = 会话 override ?? config.policy ?? 'ask'（dsh user-approval/src/index.ts）
- answerer 由入口提供：TUI 审批卡（18.2）；headless 无 answerer → 一律 fail-closed

### 11.2 沙箱策略（对齐 dsh `sandbox-policy`）

| 预设 | 沙箱 | 审批 |
|---|---|---|
| `read-only` | read-only | ask |
| `workspace-write`（默认） | workspace-write，root = 进程 cwd | ask |
| `danger-full-access` | danger-full-access | never |

`FR_PERMISSION_MODE` 可覆盖。每次请求把解析后的策略文本写入 runtime-context 快照（完整当前值跟随保留历史之后，保缓存安全）。执行体后端链：Linux bubblewrap → Landlock，macOS Seatbelt，Windows ACL restricted-token；不可用即 fail-closed（`sandbox-unavailable`）。文件系统 fence 与观察策略对齐 dsh `fs-sandbox` / `fs-observation-policy`。

---

## 12. 上下文管理与压缩

- 契约与默认后端对齐 dsh `compaction` / `compaction-basic`（summarizer 提示词逐字引用其 `summarizer.ts:33`）
- 工具结果剪枝：`threshold-chars 8192 / head 4096 / tail 1024`
- 图片卸载（`compaction-image-offload` 语义）；溢出存储 spill：`max-inline-tokens 12500`
- `/compact` 命令（16 节）触发；压缩以 surface replace-op 落地（7.3）
- 自动触发：上下文达到 dsh compaction 契约触发条件时无需 /compact 自动压缩；session-projection-cache 200 事件/5000ms 与 session-checkpoint-policy、token-meter 随 base 装配（dsh cordis.patch.yml）

---

## 13. 模型接入层

### 13.1 seam 与流协议

`LlmAdapter` trait（对齐 dsh；Rust 方法名按 5.1 用 snake_case）：唯一必须实现 `stream(options: GenerateOptions) -> StreamChunk 流`；另有 `provider_info / resolve_model / prepare_call / list_models / provider_retry_policy / image_request_pricing / image_request_target`（除 stream 外全部有默认实现；dsh llm/src/index.ts:208-303）。`GenerateOptions`（Rust 结构体，snake_case 字段）：provider/model/messages/tools/tool_history/temperature/max_tokens/stop/signal/session_id/purpose(`compaction` | `session-title`)。`prepare_call()` 支持代际绑定防竞态。

**StreamChunk 七种**（字段对齐 dsh `llm/src/types.ts:470`）：`block-start / text-delta / reasoning-delta / tool-call-delta / block-end / usage / finish`（finish 携带重放信封）；block 索引关联交错 delta；tool arguments 保持原始 JSON 字符串直到 `tool/call` 事件也 unparsed 存储；**usage 必须在 terminal finish 之前**。
装配与结算细则：落在已 block-end 索引上的 delta 忽略、重复 close 忽略、delta-only 协议兜底建块、tool-call 缺 id/name 兜底 `call-<index>`；无 finish 默认 `{kind:'stop'}`；max-tokens 丢弃全部 tool-call 块且 ReplayEnvelope.blocks 同步过滤（两输出永不分歧）；中断只保留有非空白内容的 text/reasoning，tool-call 一律省略；ReplayEnvelope 丢块同位置过滤、长度不等整体丢弃。prepare_call 代际绑定：PreparedAdapterCall 冻结模型元数据与派发入口，stream 一次性（二次派发 INVALID_PREPARED_CALL）并校验 callConfigEquals。toolUpdate 能力差异：anthropic-messages→in-history、openai-completions→addition-only、responses 系无（dsh assembler.ts、llm/src/index.ts:956-1020、models.ts:80-87）。

### 13.2 三个 provider 插件（第一天交付）

| 插件 | 协议 | 说明 |
|---|---|---|
| `fr-llm-deepseek`（默认） | Anthropic Messages | base-url `https://api.deepseek.com/anthropic`，头 `anthropic-version: 2023-06-01`；含 Files API 与图片；SSE 解析。**不带任何 dsh 私有 wire 扩展**（无 `dsh_session_log` / `dsh_plugin_packages` / `x-deepseek-harness-*`，决策 19.3） |
| `fr-llm-openai` | OpenAI-compatible | base-url/model/key 全可配，覆盖任意兼容端点 |
| `fr-llm-anthropic` | Anthropic Messages | Anthropic 官方端点 |
协议细则：deepseek messagesApiRoot 强制补 `/v1`；SSE 经 eventsource-parser，帧 event 与 body.type 不一致→MALFORMED_RESPONSE；message_stop 校验必须有 stop_reason、全部块 closed、空内容+stop→EMPTY_RESPONSE、非 max-tokens 时逐条校验 tool-call arguments 为合法 JSON 对象；stop_reason 映射 end_turn/stop_sequence→stop、tool_use→tool-calls、max_tokens→max-tokens；usage message_start 起底、message_delta 覆盖式更新。pi-ai 形态永不中途 throw（错误事件→usage+error/aborted finish），toolcall_end 把已解析参数 JSON.stringify 回原始字符串（与 deepseek 逐段拼接同契约）。错误分类：401/403→AUTH、quota/402→QUOTA、429→RATE_LIMIT、上下文溢出→CONTEXT_WINDOW_EXCEEDED、5xx→SERVER、retry-after 解析为 providerRetryAfterMs（dsh llm-deepseek/src、llm-pi-ai/src）。

鉴权：环境变量 / 配置（headless 与 SDK 形态不挂交互式凭证库——对齐 dsh headless 禁用 `credentials` 的做法；TUI 可后续在配置层扩展）。

---

## 14. MCP 客户端

- transport：stdio 与 Streamable HTTP（对齐 dsh `mcp-client/connection.ts`、`transport.ts`）
- 服务器配置含命名空间；工具以 raw JSON Schema 动态注册进 `tools` 注册表
- 资源工具 3 件：`list_mcp_resources` / `read_mcp_resource` / `list_mcp_resource_templates`

---

## 15. 子代理系统

| 能力 | 说明 |
|---|---|
| 进程内 spawn | `subagent` / `subagent_fork`：子 agent + 子会话置于子 realm；`list_subagent_models`、`send_message`、`interrupt_agent`、`list_agents` 配套（语义对齐 dsh subagent 包） |
| ACP 委派桥 | `subagent-acp`：把任务委派给外部 ACP agent（复用 fr-acp 的 ACP 客户端代码） |
| 不做 | claude-code / codex 委派桥（决策） |

---

## 16. 斜杠命令框架

- `commands` 服务：注册/分发/补全元数据；交互入口（TUI）消费；headless 不适用（任务文本非命令输入）
- 内置命令：`/compact`（触发压缩）、`/goal`、`/feedback`（对齐 dsh `command-compact` / `command-goal` / `command-feedback`）

---

## 17. 内置工具规格

**参数级 schema 权威来源：dsh `docs/tool-catalog.md`**（本节给清单与关键语义，实现时逐工具对照 catalog 冻结参数表，并以同一生成脚本思路产出 fly-rafter 的 tool-catalog 供 diff 验证）。

| 组 | 工具 | 关键语义 |
|---|---|---|
| shell | `bash`、`pwsh`、persistent `bash`/`pwsh` | 沙箱执行、`run_in_background`；persistent 版为 PTY 持久会话（opt-in）；`timeout-ms` 装配默认 60000（bash-local 缺省 120000 上限 600000，patch 覆盖） |
| 文件 | `read`、`write`、`edit`、`read_image`、`glob`、`grep`、`str_replace_editor` | `grep` 经内置 ripgrep 子进程（解析 `PATH` 上的 `rg`，`FR_RIPGREP` 可指认；打包期捆绑）；`str_replace_editor` opt-in 替代编辑器 |
| web | `web_search`、`web_fetch` | provider 可换（deepseek-official / exa / perplexity / http 适配位） |
| 任务状态 | `todo_write`；`create_goal`/`get_goal`/`update_goal`；`job_kill`/`job_list`/`job_output`；`schedule_create/delete/list/update` | todo 允许并行进行中项；jobs 为后台作业管理；schedule 为 cron 定时 |
| 交互 | `ask_user_question`、`exit_plan_mode | ask 渲染为结构化选项（TUI 18.2）；headless 下 ask 类工具 fail-closed |
| 会话查询 | `session_search`、`session_trace`、`session_event_read`、`session_event_search`、`session_event_trace` | 查询本机会话库 |
| PTY | `terminal_open/read/send/signal/list/close` | opt-in 六件套 |
| 技能 | `skill`、`load_workspace_dependencies` | |
| 代码智能 | `lsp` | |
| 目录 | `working_directory` | |
| 内省管理 | `cordis_inspect_list`、`cordis_inspect_query`、`plugin_manager` | 插件内省与运行时管理；**plugin_market 不做** |

---

## 18. 入口规格

### 18.1 headless（字段级，M1）

**装配**：不挂任何 Host/HTTP/Web 运行时；禁用交互式凭证插件；无 UI answerer。

**CLI**：

```
fr headless [TASK] [--session-id <id>] [--json] [--help]
```

- `TASK` 缺省或为裸 `-` 时：从 stdin 读一次整段任务文本（逐 chunk 收集）
- 无 TASK 且 stdin 为 TTY → usage error（exit 非 0）
- 给了 TASK 就不读 stdin；不解析任何交互命令

**输出**：

- 默认：仅最终答案文本到 stdout；provider reasoning delta 实时流到 stderr（`fr: reasoning:` 标题）；错误 `fr: <code>: <message>` 到 stderr；exit 0 = 完成，1 = 中止或出错
- `--json`：stdout 为**换行分隔 JSON 事件流**：
  - 首事件 `{"type":"session","sessionId":...}`；末事件 `{"type":"final", ...}`（无损最终答案）
  - 中间事件 `status` / `text` / `thinking` / `tool_call` / `tool_result`
  - `text`/`thinking` 从**已提交** assistant message 投影（重试/丢弃 attempt 不出现；按 step 提交而非逐 token）
  - 截断规则：除 `final` 外所有字符串/对象值封顶 8 KiB（标 `truncated:true`），单行封顶 32 KiB，嵌套 64 层截断
  - 运行错误输出 `error` 事件且无 `final`

**审批**：无 answerer，一切 ask → fail-closed `'unavailable'`（模型收到拒绝结果）；`FR_PERMISSION_MODE=danger-full-access` 时策略为 never，工具直接放行。

**多轮**：单次调用内无交互 follow-up；`--session-id <id>` 采纳已持久化会话继续（不存在的 id 直接失败；拒绝采纳子代理/fork 会话与 agent preset 不匹配的会话）。默认新 `session-<uuid>`。
输出与续用细则：--json 首事件含 cwd 字段且在订阅前写出；final 事件不经截断（无损豁免）；截断二级降级梯（整行超限→仅标量字段+truncated:true→仍超→只剩 {type,truncated:true}）；行限 32767（32KiB 扣换行）；深度 64 记 [truncated: depth]。`-` 必须是唯一任务参数；语法错误在 --json 下也发 error 事件再 exit 1；--session-id trim 后判空但原串交付（空白属身份）；headless 提交后等待根会话与全部托管子孙结束（子代理收尾提前唤醒以 seq 未变且无子判定）；tool_result 仅在 surfaceOp==='append' 时投影；错误汇总取 durable log 最后一段非空 assistant text 加 turn/end reason（dsh startup.ts、index.ts、json-stream.ts）。

### 18.2 TUI（字段级，M2）

**技术栈**：ratatui 0.29（`unstable-rendered-line-info`）+ crossterm 0.28 + pulldown-cmark 0.10 + unicode-width 0.2。TUI crate 为**纯投影层**：订阅类型化会话/领域事件，`Model::apply(event)` fold 进状态，`draw()` 只读渲染；事件循环在 `fr-tui` 入口侧：`spawn_blocking` 轮询线程 → tokio mpsc → 主循环 `tokio::select!`（按键 rx / 事件 rx / 审批信号 / 问题信号 / 补全 debounce 120ms / 100ms 重绘 ticker / shutdown watch）。通信全走 channel（事件 unbounded mpsc；审批/问题 park-resume gate；watch 关停）。

**布局（band-stack 单列，无侧栏）**：自上而下 header(1) / 分隔线(1) / 内容时间线（弹性）/ docked 卡区 / activity 行 / 输入上分隔线 / 输入编辑框 / 输入下分隔线 / 排队提示行（有队列才占位）/ help 行 / footer 统计行。输入框随 wrap 实测行数增长（clamp MAX_INPUT_ROWS）；footer 贪心打包占多行；手动滚动暂停 auto-follow，滚回底部自动恢复；`max_scroll` 每帧实测；极矮终端（<8 行）按序降级（排队行/help/activity/输入框）；渲染异常降级为 notice 不 panic。docked 卡不遮时间线；审批/问题等 modal 才是 overlay。

**键位表**：

| 键 | 行为 |
|---|---|
| Enter | 发送 |
| Ctrl+Enter / Alt+Enter / Ctrl+J / 行尾 `\`+Enter | 插入换行（多行一等公民） |
| Backspace/Delete、←→、Home/End、Ctrl+A/E/U | 编辑（readline 语义） |
| ↑/↓ | 光标在首/末行时翻 prompt 历史（带 draft 保护） |
| Ctrl+C | running 时中断；idle 时双击退出（3 秒窗口） |
| Esc | running 时中断；modal 中关 modal |
| PageUp/PageDown / 鼠标滚轮 | 时间线翻页 ±5 行 / 滚动 |
| Shift+Tab | 循环 permission mode（read-only → workspace-write → danger-full-access） |
| Ctrl+O / Ctrl+T / Ctrl+R | 展开最新工具卡 / 任务面板 / 子代理实时视图 |
| `?`（输入为空时） | 帮助 overlay |
| bracketed paste | 整段粘贴（IME 安全） |
| kitty keyboard protocol | 启动时探测 CSI u，支持则区分 Press/Release 与 Ctrl+Enter |

**审批卡**（ApprovalDialog）：浮动 modal，`tool:`/`kind:` 元信息 + 命令全文（手动 wrap，尾部必须可见，超长 `… +N more lines` 标注）；三选项 `Yes, proceed (y)` / `Yes, and don't ask again this session (a)` / `No, don't run it (n)`；**y/a/n 单键直判**，Up/Down+Enter 导航；选中行 `>` 标记反色。modal 独占键盘，Esc/Ctrl+C 永远可退出。

**ask_user_question 渲染**：全屏对话框；数字 1-9 直选、Space 多选切换、Enter 激活、Tab/Shift+Tab 翻问题页、Esc 拒绝、常驻 Other 自由输入行；单问题单选选中即提交，多问题结束进 review 页。

**渲染**：markdown 经 pulldown-cmark → styled Lines；**仅 4 语义色 + bold/italic/underline**（不与终端主题冲突）；表格宽度自适应收缩 + 单元格内 wrap；流式 holdback（未闭合表格整段扣住）；标题去 `#`；行内 code 着色；**无语法高亮**。Palette：accent/warn/error/add/remove/user_bg/ok/dim + 按工具族着色；ColorMode 协商 None/16/256/TrueColor，尊重 `NO_COLOR`/`TERM=dumb`/tmux。diff 渲染（unified，add/remove 色）。工具卡三态：queued 静态 `◌` / running 闪烁 / 终态 `· 1.5s` 耗时。交互输出全走 stderr，stdout 留给数据。

**终端生命周期**：TerminalGuard RAII（raw mode + alternate screen + bracketed paste + kitty 探测）；信号处理；SSH/tmux/CI 降级。

**代码搬运（决策 16）**：从 `~/project/shadow/crates/shadow-tui` 拷入四块零耦合模块并保留出处头注释——`src/render/markdown.rs`（1440 行）、`src/render/diff.rs`（289 行）、`src/complete.rs`（310 行 fuzzy 补全）、`src/env/`（约 1000 行：TermContext/TerminalGuard/kitty/信号）。其余（band-stack、Model::apply、键路由、审批卡、问题对话框）按上述规格为 fly-rafter 重写，禁止依赖 shadow-tui crate 本体。

**斜杠与补全**：`/` 命令补全与 `@` 文件补全，fuzzy 评分 + 120ms debounce。

### 18.3 ACP 服务器（协议契约级，M3）

- 遵循 **ACP v1**（agentclientprotocol.com；对齐 dsh 依赖 `@agentclientprotocol/sdk@1.4.0` 行为）；stdio JSON-RPC，stdout 只承载协议流量；一连接多会话并行
- `agentInfo`：`fly-rafter-acp/0.0.1`
- 方法面（对齐 dsh `packages/acp/acp/README.md` Protocol contract）：`initialize`（通告 v1 + session/list/resume/close + Streamable HTTP MCP）、`authenticate`（直接成功）、`session/new`（新建持久会话；workspace + stdio/HTTP MCP 先校验后发布；返回完整配置选项态）、`session/list`（持久可恢复根会话，newest-first 分页，默认页大小 100）、`session/resume`（恢复非活动持久会话，不重放旧更新）、`session/close`（静默取消 → 排干更新 → 子孙处置 → 持久化 flush → 处置该会话 scope）、`session/set_config_option`（`model` / `reasoning_effort`，返回完整结果态）、`session/prompt`（每会话同时仅一个 in-flight prompt；text/resource-link/受支持图片；settlement 等根 + 子孙全部完成）、`session/cancel` 与 `$/cancel_request`（未知 id no-op）
- 下行 `session/update`（每会话串行）：已提交助手消息/thought、通用工具生命周期、配置变化、context 用量；**不含**原始 provider delta/重试/专有呈现。`session/request_permission`：一次性 allow/reject，客户端可自动应答
- 映射：一个 ACP session = 一个持久 agent+session；模型选择在 prompt 准入时快照并 pin 整个 turn
- 明确不支持（与 dsh 一致）：`session/load`、删除、fork、额外目录、SSE/ACP-transport MCP、modes、commands、plans、terminals、客户端文件系统操作、elicitation
- 配置：`provider` / `model` / `session-list-page-size`（默认 100）
- 生命周期细则：image 能力条件通告（attachment 支持栅格式且模型声明 image 输入），audio/embeddedContext 恒 false；resume 要求 cwd realpath 物理等同、拒绝活跃/activating 会话、恢复优先日志记录的路由；list 过滤活跃/子代理/fork/cwd 异常项，createdAt 降序同刻按 id 字节序，keyset 游标 base64url 且强校验 canonical；prompt 准入期两次 agent 存活校验、图片未通告即 invalid、严格 canonical base64；模型 pin：准入时 snapshot→按 messageId 存 pendingSelections→claimed 时 pinTurn（turnSelection 优先于 selected）→turn/end releaseTurn；更新经 outputTail promise 链每会话串行，交付失败仅记 outputError 不阻断；settlement 循环 race(whenIdle, cancelled)→waitForChildren→outputTail→seq 未变才退；config id 仅 'model'/'reasoning_effort'，未知 id 报错（dsh acp/src/index.ts、session.ts、model-control.ts）

### 18.4 SDK（协议契约级，M4）

**传输**：换行分隔 JSON-RPC 2.0 over stdio。帧规则（对齐 dsh `protocol/src/transport.ts`）：有 `id`+`method` 为请求，仅 `id` 为响应，仅 `method` 为通知；malformed 行忽略；handler 失败变错误帧。错误码：handler 缺失 `-32601`（仅 transport 层）；handler 抛错与未知方法 `-32603`（SDK server 恒装 handler，未知方法实得 -32603；dsh transport.ts:226-237、server.ts:374-375）；对端错误透传 wire code + data。

**请求方法**：

| 方法 | 参数 | 结果 |
|---|---|---|
| `initialize` | `cwd, provider, model, reasoningEffort?, maxTokens?` | `serverInfo: {name: 'fly-rafter-sdk-runtime', version}` |
| `session/prompt` | `sessionId`（未知 id 惰性创建）、`contentBlocks: (ContentBlock \| ImageBlock)[]`；图片块 `{type:'image', data: base64, mimeType: png/jpeg/webp/gif}` 经 durable attachment store 准入 | `messageId`（入队回执） |
| `session/resume` | `sessionId`（不存在或被占用则拒绝；仅恢复不入队 prompt） | `{}` |
| `session/wait` | `sessionId`（等该会话及其托管子孙全部结束；从不创建） | `{}` |
| `session/working-directory/get` | `sessionId` | `cwd` |
| `session/working-directory/set` | `sessionId, path`（相对基于当前目录） | `cwd` |
| `shutdown` | 无 | `{}`（进程级；协议内无 session/close） |

**下行通知**：`session.event` `{sessionId, event: SessionEvent}`（运行时内所有会话含子孙的完整事件逐条实时下发）；`session.status` `{sessionId, status: 'idle'|'running'}`；`subagent.started` `{parentSessionId, childSessionId}`；`subagent.finished` `{provider, agentId, parentSessionId, childSessionId, status: 'ok'|'error', stopReason, lastAssistantMessage?}`。

**不提供官方客户端 SDK**（2026-10-21 用户裁决）：协议即契约，调用方按需自行封装；不交付 Python SDK。
服务器细则：请求 id 形如 `req_<uuid>`；params 非对象（数组/标量）坍缩为 `{}`；subagent.finished 仅 info.local 子女；status 映射 completed→ok、max-tokens 仅 maxTokensAsSuccess 才 ok（默认 false）；initialize 校验 reasoningEffort 非空串与 maxTokens 正安全整数，无适配器时仅 provider=='deepseek-official' 允许惰性挂载并预检路由；wait 从不创建会话、退出有 failure 则抛；shutdown 单飞：等全部在途创建→清 map→逆序 pop disposers→drainDescendants→并行 dispose→失败聚合（dsh sdk/server/src/server.ts）。

### 18.5 Web（能力清单级，M5；动工前另出详案附录）

能力清单（对齐 dsh web profile 的用户可见面）：本地 HTTP 服务器默认 `127.0.0.1:3080`；会话列表与续用；对话时间线（流式 markdown、工具卡、reasoning）；附件上传/拖拽/粘贴（图片草稿预览）；审批与 ask_user_question 界面；斜杠命令；plan mode 与 goal 面板；会话 fork；设置。技术栈在详案附录定（Rust 侧服务器 + 前端框架选型），本 PRD 不约束实现细节。

---

## 19. 明确排除与有意偏差记录

### 19.1 排除清单（用户确认裁剪）

| 项 | 原版位置（dsh） |
|---|---|
| 语音输入全家（voice-input bundle、speech-to-text 及转写 provider） | `packages/experimental/voice-input-*`、`packages/speech-to-text/` 等 |
| Agent Teams 9 工具（spawn_teammate/team_task_*/wait_agent） | `packages/experimental/` |
| 浏览器/计算机使用（browser-use、computer-use、stagehand 6 工具） | `packages/browser-use/`、`packages/computer-use/` |
| workflow、ralph、present、create_worktree、webhook | 对应包 |
| hooks 桥（Claude Code / Codex hooks.json） | `packages/experimental/hooks-*` |
| 遥测（OTel 导出、匿名用户 ID、`dsh_session_log` 回传） | `packages/telemetry/`、`packages/identity/`、`session-log-deepseek` |
| HMR 热重载、插件市场（plugin_market） | `packages/boot/hmr/`、plugin_market 工具 |
| Electron 桌面端 | `apps/desktop*` |
| PTC Python 后端（实验） | `packages/experimental/ptc-runtime-python` |
| Python SDK 客户端（协议消费者自行封装） | `python/` |
| 会话格式迁移链 v0→v3 | `packages/session/session-format*` |
| 子代理 claude-code / codex 委派桥 | `subagent-claude-code`、`subagent-codex` |

### 19.2 有意偏差（与 1:1 口径的关系声明）

1. `!!js` patch 表达式 → 受限声明式条件子集（第 5 节；dsh 中该语法的真实用法盘点 22 处见第 5 节适配表——env 三元与裸 env 读、profile 门控、sdk-app 路径解析类；fr 子集覆盖前三类并以 profile-context 布尔承载门控，2026-10-22 Q3A 勘误）
2. 品牌/命名替换（第 5 节表）——文本级唯一改动为身份句
3. 插件配置键由 dsh 的 camelCase 转为 kebab-case（用户裁决 2026-10-21，见第 5 节适配表与 5.1）：不承诺与 dsh 配置文件互换；协议 wire 层（会话事件、工具 schema、SDK/ACP 参数）不受影响，仍逐字 1:1
4. 除上述各条与 19.1 排除项外，**不存在其他已知偏差**；实现中发现新的不可避免偏差必须先记录到本节并经用户确认
5. PRD 勘误与源码补遗（2026-10-21 源码核对）：正文 12 处与 dsh 源码不符已按源码修正（会话头 seedLength、tool/call 时序、绑定名规则、PTC wire 帧、程序 fs 访问、CONTEXT_ORDERS 行号、AGENTS.md 注入形态、image_request_target、SDK 未知方法错误码、空渲染语义、审批切换文案、独占发布细节）；另将 dsh 源码级技术关键点（默认装配树锚点、surface 硬约束、两级补偿、header 判定算法、缓存友好路由、管线与调度细则、PTC 实现细则、组装确定性、审批细则、流装配结算、三 provider 协议细则、入口输出细则、ACP 与 SDK 生命周期细则）并入正文各节。全部依据见 doc/dsh-source-notes.md（file:line 证据）
6. 装配语义勘误（2026-10-22 Q1-Q3 裁决）：6.1 "按 id 整行替换"勘误为 dsh 实际的**按键覆盖**语义（id/name 匹配器、逐键覆盖、config 整键替换不深合并、未命中 id 或 name 不匹配均警告跳过、行序=首次插入序）；行模型增补 `insert`（根级插入，不做 group——base patch 未用、dougong 无组概念）；`!!js` 子集按 bundle patches 全量盘点（22 处）扩充：env `??` 默认链、相等比较、平台三元、裸 env 值、字面量、profile-context 布尔（dsh `ctx.get('profileContext')` 的 fr 等价物）；"dsh 中该语法仅用于平台门控与 env 读取"的原表述勘误；sdk-app `getBuiltinModule` 路径解析类 6 处不做（对象属桌面打包面，19.1 已裁）。依据 `vendor/include/src/index.ts:57-127`
7. 装载文件布局勘误与裁决（2026-10-22 Q1-Q5）：6.1 家目录 patch 括号路径 `~/.fly-rafter/profiles/` 勘误为 `<FR_HOME>/fr.patch.yml`（dsh 真源为 home 根 cordis.patch.yml，profile-context.ts:78）；profile 清单定为 `<profile>/profile.yml`（`bundles:` 仅引用宿主内置命名 bundle，未知名 skipped 警告非致命——dsh 的 npm 包栈与 pnpm 机制在 fr 无对应物，插件市场已裁）；profile 模板自动初始化纳入 M1（1:1 dsh 首用 init：绝不覆盖既有文件，未知名报错）；入口默认 bundle 为嵌入 YAML 常量、与用户 patch 同一解析路径（dsh bundle 与用户 patch 同 schema，app-boot index.ts:268）；M1 默认 bundle 为最小 headless 集，完整品牌映射抽取后置。依据 `packages/boot/app-boot/src/{profile.ts,profile-context.ts,index.ts}`
8. insert 行相对路径锚定勘误（2026-10-22 Q6，代码评审发现）：6.2「按字面路径」缺 dsh 的锚定语义——dsh 在补丁解析期把 insert 行 name 为 `./`/`../` 相对路径者锚定到补丁文件所在目录（anchorInsertedPluginNames，app-boot index.ts:550-561；覆盖行 name 为匹配器不解析）；fr 对齐实现于装载期，无此锚定则相对路径落成 CWD 相对而偏离 dsh
9. 配置 schema 真源形态勘误（2026-10-22 Q7-Q9 裁决）：6.3 原「Rust 类型 + serde + 校验函数」措辞修正为**声明式描述树单一真源**（fr-schema 描述符等价 schemastery 校验+投影角色；Rust 类型 + serde 降为消费便利层）——对齐 dsh 单一声明源机制（schemastery 对象同供校验与 dump 投影，app-boot config-schema/types.ts）；schema 库独立为 fr-schema crate（宿主与插件工件链接同一 crate 双侧校验）；装配期查无 schema 的行跳过校验（dsh absent 标注语义，types.ts:42），第三方工件宿主侧无 schema 不构成装载障碍
10. .env 分层规格勘误与定稿（2026-10-22 Q11-Q14 裁决）：6.4 原「NODE_* 前缀」概称勘误——dsh 无 NODE_ 前缀，bootstrap 名单实为 49 精确名（app-boot index.ts:136-157 逐字；DEEPSEEK_BASE_URL/DEEPSEEK_SEARCH_BASE_URL 保留原名，provider 名 deepseek-official 未变）+ FR_/XDG_/DYLD_/BASH_FUNC_ 四前缀（DYLD_、BASH_FUNC_ 为非品牌前缀，原文漏记）；补钉失败语义五点（违例即终止、两文件先全解析校验再统一应用的原子序、非 ENOENT 读错误警告跳层非致命、ENOENT 静默、大小写不敏感检查）与 cwd==home 家层去重；应用机制定为双轨 = materialize 进进程 env（unsafe set_var 启动期单线程）+ 不可变环境快照（Q13A）；`.env` 解析语法定为 Node util.parseEnv 方言自实现对齐（Q14A，dotenvy 载体否决——变量扩展等超集语义漂移）；home 解析 = FR_HOME（空白未设）> ~/.fly-rafter（D0，fr-config resolve_home）。依据 `packages/boot/app-boot/src/index.ts:114-259`、`packages/util/{launch-environment,home-paths}`
11. dump 三态与品牌定名（2026-10-22 Q18-Q20 裁决）：x- 扩展键定名 `x-fr`（对齐 dsh `x-cordis`）；`fr dump-config` 输出为 YAML 带分层注释逐字节同型（`# ==` 分隔、`!!js` 逐字不求值、前缀快照单次 applyEntryPatches 算法）；`--default-only` 恢复诊断纳入（对齐 dsh `--dump-default-config`，仅 bundle 层、不读不解析用户层、禁 `--patch`）；`from-default-profile` 出厂模板替换纳入（对齐 dsh `--from-default-profile`，dump 与 boot 共用，CLI 旗标宿主期落地）；三 dump 态互斥且拒收 app 位置参数。依据 `apps/cli/src/{args,dump-config,dump-config-schema}.ts`、`packages/boot/app-boot/src/index.ts:635-`、`packages/boot/app-boot/tests/config-dump.spec.ts`
12. `!!js` 方言结构值位 fail-closed 范围（2026-10-22 Q25B 定稿，行为先于本条存在）：识别值位（行首 `key: `/`- ` 前缀后）之外的结构值位（flow 式 `{p: !!js x}`、异形键 `a.b: !!js v`）fr 拒绝整个文档（错误含 "outside a recognized value position"），dsh 的 js-yaml 自定义 type 在任意值位接受——该偏差自 Q3A 子集定稿起存在，Q25B 重申保留（静默丢标签求字面量比拒绝更危险）；字符串值内容中 tag 形文本不是结构值位，Q25B 起逐字装载、不拒绝也不翻转（块标量、引号多行、普通多行标量同理）

### 19.3 wire 层声明

与 DeepSeek 官方 API 的交互仅使用标准 Anthropic Messages 协议。不实现 `dsh_session_log` 会话日志回传、`dsh_plugin_packages` 上报与 `x-deepseek-harness-*` 头。官方 API 在无这些扩展时正常工作，仅无 DeepSeek 服务端会话持久化。

---

## 20. 里程碑与验收

| 里程碑 | 范围 | 验收标准 |
|---|---|---|
| **M1 headless** | workspace 骨架与质量门；fr-config 分层编译；fr-session（v4 全量）；fr-agent-loop；fr-tools 管线 + 第 17 节全部工具（`subagent-acp` 桥除外）；fr-approval/fr-sandbox；fr-compaction；fr-system-prompt；三 provider；fr-mcp；fr-ptc；进程内子代理 spawn/fork；fr-headless（以上全部插件以 dylib 工件经真实装载走通） | 1. `fr headless "任务"` 走通完整 turn 周期并产出 v4 会话文件；2. `--json` 事件流通过 schema 校验；3. 同一任务在 fly-rafter 与 dsh（同模型同参数）产出的会话日志结构逐事件同型（工具名/事件类型/字段名 diff 为空）；4. 审批 fail-closed、`--session-id` 续用、abort 补偿各有集成测试；5. `fr dump-config` / `dump-config-schema` 可用；6. 质量门四关全绿 |
| **M2 TUI** | fr-tui 全量（18.2） | 1. 18.2 键位表逐项有 TestBackend 测试；2. 审批卡 y/a/n、问题对话框数字直选实测通过；3. 流式 markdown 与 holdback 渲染测试；4. `tmux capture-pane` 实机验证脚本入库；5. 四搬运模块保留出处头注释 |
| **M3 ACP** | fr-acp + subagent-acp 桥 | 1. 以标准 ACP 客户端完成 initialize/new/prompt/update/permission/close 全流程；2. session/list 分页与 resume 语义测试；3. 委派桥端到端用例 |
| **M4 SDK** | fr-sdk | 1. 协议帧/错误码/通知逐条符合 18.4 表；2. `session/wait` 覆盖子孙会话 |
| **M5 web** | 详案附录先行，评审后再动工 | 附录另行评审 |

每个里程碑收尾：更新生成式文档（tool-catalog / config-catalog 等价物）并与 dsh 同名目录 diff 评审。

---

## 21. 测试与质量策略

- **质量门**：`cargo fmt --check`、`cargo clippy -- -D warnings`（lint 配置与 dougong workspace 对齐：rust lints + clippy all/pedantic/nursery/cargo 及选定 restriction 项，见 dougong `Cargo.toml` `[workspace.lints]`）、`cargo test`、`cargo doc`（无警告）——四关全绿才可合并；工具链钉死与 dougong 同步（1.99.1 补丁级，含 wasm32-wasip2 与 musl target）
- **测试组织**：单测随 crate；集成测试放 `crates/*/tests/`；临时测试文件即写即删
- **对照测试（1:1 锚点）**：golden session 回放——构造 v4 会话文件，断言 `deriveMessages()` 重建的冻结请求与 dsh 同场景一致；`--json` 事件流与 SDK/ACP 帧各有 wire 一致性测试
- **TUI**：`TestBackend` 全量渲染断言 + tmux capture-pane 实机验证
- **测试本质**：用例以暴露缺陷为目标，客观分析结果；禁止为绿灯放宽 lint 或断言
- **文档即生成物**：tool-catalog / config-catalog 等价物由脚本从真实装配生成并校验，防文档漂移

---

## 22. 风险与对策

| 风险 | 对策 |
|---|---|
| node 运行时依赖（仅 PTC） | fail-closed + 明确报错；CI 装 node 覆盖 PTC 测试；文档声明可选依赖 |
| bubblewrap 缺失（Linux 沙箱） | 后端链降级 Landlock；两者皆缺 → `sandbox-unavailable` fail-closed，不静默放行 |
| ripgrep 分发 | 运行时解析 PATH / `FR_RIPGREP`；打包期捆绑；缺失时 `grep` 工具明确报错 |
| dsh 源码漂移（上游更新） | 本 PRD 锚定 v0.2.1-rc.2；参照物路径固定；升级为显式决策 |
| 全插件化下循环性能（事件分发开销；插件边界跨越全部为 C-ABI Invoker 通道，JSON 字节序列化） | 会话内热路径（assistant-stream chunk）批量分发为硬性要求；基准测试进 M1 验收 |
| 提示词文本抄录错误 | 提示词全部以脚本从 dsh 源文件抽取生成常量，禁止手抄 |

---

## 23. 决策记录（2026-10-21 访谈确认）

1. 基座 dougong 0.2.0，git 依赖（本机可叠 path 覆盖），内核改动随里程碑发版
2. 字面全插件：agent 主循环也是插件；宿主 = 装配器 + 事件泵
3. 1:1 口径 = 行为与协议对齐；内部结构按 dougong 重造
4. 身份句换 Fly Rafter，其余提示词逐字保留
5. 命令 `fr`，家目录 `~/.fly-rafter/`
6. PRD 中文、工程级详尽、分层深度（headless/TUI 字段级；acp/sdk 契约级；web 能力级）
7. 功能清单：核心子系统全保留；子代理仅 ACP 桥；工具组 12–21 全保留；内省管理裁 plugin_market；实验/边缘/遥测/HMR/插件市场全裁
8. run_code/PTC 保留，node 子进程执行 TypeScript，全链路 1:1
9. 入口顺序 headless → TUI → acp → sdk → web；desktop 裁剪
10. headless 一次性任务 + `--json` 事件流 + 审批 fail-closed + `--session-id` 续用
11. TUI：ratatui + crossterm，shadow 范式 + 四模块搬运
12. 三 provider 第一天交付；无私有 wire 扩展
13. 会话格式只实现 v4
14. Python SDK 裁剪：SDK 仅 JSON-RPC 服务器入口，客户端由调用方自行封装
15. 命名硬约束（Rust 风格，禁 TS 风格）：代码标识符 snake_case，自有用户可见命名 kebab-case；dsh camelCase 插件配置键转 kebab-case（19.2 条 3），协议 wire 层保持逐字 1:1
16. 插件仅 dylib 与 wasm 两形态（2026-10-22 用户裁决）：第一方组件全部为 C-ABI dylib 工件（内核家族仅依赖 dougong-abi），wasm 仅承载第三方沙箱扩展；loader builtin 表在 fr 装配中从不使用；fr-config 为宿主直链库（非插件，先于装载存在）；宿主进程内面 = dougong 内核 + dougong-loader + fr-config + 宿主壳（CLI 解析/事件泵/入口分发）；跨边界载荷一律 Invoker 通道 JSON 字节，共享形状经 fr-wire 纯 serde 库按副本共享
17. 装配合并语义对齐 dsh 源码（2026-10-22 Q1-Q3 裁决）：按键覆盖（id/name 匹配器、逐键覆盖、config 整键替换、missed warn-skip、首次插入序）；`insert` 根级插入、不做 group；`!!js` 子集 = env `??` 默认链、相等比较（`==`/`===`）、平台三元、裸 env 值读取、字面量、profile-context 布尔（含 `!` 取反）
18. 装载文件布局（2026-10-22 Q1-Q5 裁决）：家目录 patch = `<FR_HOME>/fr.patch.yml`（对齐 dsh home 根）；profile 清单 = `<profile>/profile.yml`，bundles 仅引用宿主内置命名 bundle、未知名 skipped 非致命；profile 模板自动初始化纳入 M1（首用建清单+空白 patch，不覆盖既有；fr 模板 bundles 为空——入口默认 bundle 恒在层 1）；入口默认 bundle 为嵌入 YAML 常量走同一解析路径；M1 默认 bundle 为最小 headless 集（完整品牌映射抽取后置）
19. insert 行相对路径锚定对齐 dsh（2026-10-22 Q6 裁决）：装载期把各文件层 insert 行 name 以 `./`/`../` 开头者锚定到补丁文件目录转绝对（词法归一，不做符号链接解析）；入口内置 bundle 全为 slug 名不受影响；覆盖行 name 不锚定；无 `./`/`../` 前缀的含分隔符 name 保持字面路径
20. 配置 schema 框架（2026-10-22 Q7-Q10 裁决）：声明式描述树为单一真源（fr-schema：validate 与 to_json_schema 同源派生，子集外声明不可构造）；fr-schema 独立 crate，fr-config 依赖其做装配期校验（slug 查注册表、查无跳过、失败进 failures 不阻塞），插件工件静态链接做激活期再校验；dump-config-schema 产物为 JSON Schema 2020-12 文档；M1 框架以自造描述符钉测交付，真实 schema 逐插件 crate 落地时挂注册表
21. .env 分层机制（2026-10-22 Q11-Q14 裁决）：bootstrap 名单 = dsh 49 精确名逐字（DEEPSEEK_* 保留原名）+ FR_/XDG_/DYLD_/BASH_FUNC_ 前缀，大小写不敏感检查；失败语义五点对齐 dsh（违例终止 / 两文件原子先校验后应用 / 非 ENOENT 读错误警告跳层 / ENOENT 静默 / cwd==home 家层去重）；应用 = materialize（unsafe set_var 启动期单线程）+ 不可变环境快照双轨（快照消费方后置）；解析器 = fr-config 自实现对齐 Node util.parseEnv（否决 dotenvy 超集漂移）；fr-config 增 resolve_home（FR_HOME 空白未设 > ~/.fly-rafter，D0）
22. dump 数据面规格（2026-10-22 Q18-Q20 裁决）：dump-config-schema 输出扩展键 = `x-fr`；dump-config 输出 YAML 分层注释逐字节同型（`# ==` 分隔注释、`!!js` 逐字、前缀快照单次 applyEntryPatches）；`--default-only` 恢复诊断与 `from-default-profile` 出厂模板替换纳入规格（CLI 旗标宿主期落地，数据面先行）
23. dump base 锚点与离线工具口径（2026-10-22 Q23A/Q24A 裁决）：profile 布局增根文件 `fr.yml`（dsh `cordis.yml` 品牌映射），每次装载无条件重写，boot 与 dump 共用同参 base，错误三连落在该文件读/解析上；dump 离线工具 = dsh structural-trust 逐字移植——层文件 shape-only 解析（顶层 YAML 数组 + 每项映射即过，任意键保留、`!!js` raw），raw compose 完整移植 `applyEntryPatches`（group 递归索引、insert 带 id 指向 group、五条 dsh 警告文案原样、`%C` = JSON.stringify），boot 严格解析不动；fr 与 dsh 组合结果仅在合法 fr 文件上有定义，越格式文件 dump 仍可出树（dsh dump 同构诊断面）
24. `!!js` 方言结构感知保真（2026-10-22 Q25B 裁决）：`!!js` 归一化从行锚定正则改写升级为结构感知差分解析——raw 解析为字符串内容真值（serde_yaml_ng 对 `!!` 二级句柄 tag 一律静默丢弃，既证）、翻转解析为标签位置真值、剩余 `!!js` 以守卫 tag `!fr-js-position-guard` 检出识别位外结构值位 fail-closed（文案不变）；字符串内容中 tag 形文本逐字装载（19.2 条 12）；dump 输出侧同步废除内容行误翻——`!!js` 标签行照常双叹号、内容行 `!js` 逐字（双序列化行差分，异常回退正则），dump == boot 对任意字符串成立
25. dump-config-schema 文档体 = 公共骨架（2026-10-22 Q26B 裁决）：x-fr 五键与 entry 五态照载（决策 22），文档 `$defs` 取公共骨架、include/group carrier 面裁剪（19.2 条 13）；diagnostics stderr 格式勘误为 `fr: <level>: [<path>] <message>`（无 path 时 `fr: <level>: <message>`，dsh dump-config-schema.ts:44-46 实证 level 冒号后带空格、path 方括号前带空格）
