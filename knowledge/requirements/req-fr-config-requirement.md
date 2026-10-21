---
kind: requirement
id: REQ-FR-CONFIG
title: "装配与配置系统"
status: accepted
liveness: auto
tags: [config, assembly]
---

## Problem

dsh 依赖 cordis 原生分层补丁能力装配插件；dougong loader 只接受整份 JSON 文档，无分层、行覆盖、YAML 概念。fly-rafter 必须在 fr 层重建分层装配语义，同时保持 dougong reconcile 的幂等收敛与失败不阻塞语义。

## Requirements

- [REQ-FR-CONFIG-1] fr-config MUST 解析 fr.patch.yml 有序分层装配（自底向上：入口默认 bundle → profile bundle 栈 → profile patch → 家目录 patch → `--patch` 命令行补丁），后行按 id 整行替换前行（含 config 与 disabled）。
- [REQ-FR-CONFIG-2] fr-config MUST 将展开后的装配编译为 dougong loader JSON 文档（builtin/dylib/wasm source、config 对象、isolate），经 `Reconciler::replace + apply` 幂等收敛；编译产物 MUST 为 dougong `Config::parse` 可接受的合法文档。
- [REQ-FR-CONFIG-3] fr-config MUST 在装配期对每行 config 依据插件导出的 ConfigSchema 校验；单行校验失败进入 `Report::failures`，MUST NOT 阻塞其余槽位。
- [REQ-FR-CONFIG-4] patch 行内条件表达式 MUST 限于受限声明式子集（`env.NAME` 存在性/相等比较、linux/macos/windows 平台三元、字面量）；MUST NOT 支持任意 JS 求值。
- [REQ-FR-CONFIG-5] fr MUST 提供 `fr dump-config` 与 `fr dump-config-schema`，内容由真实装配生成（文档即生成物）。
- [REQ-FR-CONFIG-6] fr MUST 提供关键默认值：默认模型 provider `deepseek-official`、model `deepseek-flash`；`FR_PERMISSION_MODE ?? workspace-write`（workspace root = 进程启动 cwd）；非 danger-full-access 时审批策略 `'ask'`；会话存储 `<FR_HOME>/sessions`、storages `<FR_HOME>/storages`；bash `timeout-ms` 60000；重复工具提醒阈值 [3, 5, 8]；`sample-over-cap-glob-results: false`；`allow-parallel-in-progress: true`；会话标题默认：无 LLM 时回退 5 词/40 字节（上限 80 字节），LLM 标题 targetWords 5（CJK 10）、maxInput 4096 字节、超时 60s。
- [REQ-FR-CONFIG-7] fr 宿主二进制 MUST 仅含装配器、事件泵、入口分发三件事：事件泵驱动 tokio runtime，把入口信号（headless 任务 / TUI 按键 / ACP·SDK 协议帧）转发到对应入口插件监听器；按子命令装配对应入口 profile（headless/tui/acp/sdk/web）；宿主对 agent 循环零感知。FR_HOME 默认 `~/.fly-rafter`，家目录结构与 dsh `~/.dsh/` 同构（sessions/storages/profiles）。
- [REQ-FR-CONFIG-8] 插件三形态 MUST 共用 dougong 五步契约：全部第一方插件为进程内 builtin（经 loader builtin 表按名装载）；C-ABI dylib 为第三方可信扩展（代码镜像进程内永不卸载，换版本即重启进程）；wasm 组件（wasm32-wasip2）为第三方沙箱扩展（零能力 WASI：wasip2 guest 必然声明 wasi:io 导入而宿主挂载零能力 WasiCtx 的惰性定义，能力面为零、导入声明不为零；fuel/epoch/内存帽；经 WIT world `dougong:kernel/host` 交互）。装配层 MUST 按 source kind 显式声明每行插件形态（fr.patch.yml 的 name → 编译产物 source.kind: builtin/dylib/wasm），fr-config MUST 按信任边界校验：第一方插件 MUST 为 builtin，dylib 仅第三方可信扩展、wasm 仅第三方沙箱扩展，违规行进入 failures。
- [REQ-FR-CONFIG-9] 第三方工具插件 MUST 经 Invoker 通道接入：跨边界契约 = 事件名 + JSON 载荷 schema + 方法号常量表（descriptor 声明 + 能力自检）；isolate 列 MUST 承载 realm（会话级装配在会话 realm 内放置同名插件，域内 provide 不外泄、注入先域内后回退全局默认）；`internal/update` waterfall 仅作热更新预留扩展点，本期 MUST NOT 暴露。

- [REQ-FR-CONFIG-10] 默认装配树 MUST 与 dsh base cordis.patch.yml 插件 id 集合同型（品牌映射后），并冻结 PRD 6.4 未记载默认值：agent-instructions maxBytes 65536、projection-cache 200 事件 5000ms、web searchProvider deepseek-official 与 fetchProvider http、repeat argumentsPreviewChars 500、标题 maxOutputTokens 4096、tools 默认 native、storage-domain backend json（dsh cordis.patch.yml 各行）。
- [REQ-FR-CONFIG-11] 启动失败策略 MUST（对齐 dsh required-id 审计 [app-boot index.ts:944-961,1092-1114]）：仅 bootstrap 入口 Include 与必需插件 id 集（fr 对应 agent-loop 与当前子命令入口插件）激活失败时以 StartupError 拒启动（错误列出失败与等待清单）；其余插件失败一律单条警告、兄弟继续；注入服务缺失 → 插件 fiber 置 pending 容忍（其 id 属必需集则致命）；导入失败记录错误后不留实例、不中止装配；patch 未知 id 警告跳过永不致命；行覆盖语义 MUST 为按 id 整行替换（非 dsh 键级合并），patch 行内仅其余键作用于该行自身。
- [REQ-FR-CONFIG-12] patch 行内 env 缺省表达式 MUST 以受限子集表达 dsh !!js 形态的对应语义：`env.FR_PERMISSION_MODE 存在取其值否则取字面量 workspace-write` 的求值结果与 dsh `process.env.DSH_PERMISSION_MODE ?? 'workspace-write'` 逐值一致（dsh cordis.patch.yml:246,262）。
- [REQ-FR-CONFIG-13] .env 分层 MUST（2026-10-22 用户裁决纳入，PRD 6.4；对齐 dsh app-boot index.ts:126-260 品牌映射后）：fr-config 启动期解析环境 = 继承进程 env > 项目 `.env`（自 cwd）> 家目录 `.env`（`<FR_HOME>/.env`）；`.env` 中已设名 MUST NOT 覆盖更高层已存在的值；bootstrap 专名在任何 `.env` 层拒绝：PATH、NODE_ 前缀、代理名、DEEPSEEK_BASE_URL、全部 FR_ 与 XDG_ 前缀；家目录层例外可设 HTTP_PROXY / HTTPS_PROXY / ALL_PROXY / NO_PROXY 四键。
## Scenarios

Rule: REQ-FR-CONFIG-1 — 分层装配与行覆盖

场景: 后行按 id 整行覆盖前行
  假设 profile patch 中存在与入口默认 bundle 相同 id 的补丁行
  当 编译装配时
  那么 该行以 profile patch 版本生效，前行配置不再出现于编译产物

场景: 装配层次自底向上展开
  假设 入口默认、profile、家目录 patch 与 --patch 各定义了同一 id
  当 展开装配时
  那么 按入口默认 → profile → 家目录 patch → --patch 顺序覆盖，最终值来自最后一层

Rule: REQ-FR-CONFIG-2 — 编译为 loader 文档

场景: 编译产物为合法 loader 文档
  假设 一次完整装配展开完成
  当 编译并执行 Reconciler::replace 加 apply 时
  那么 编译产物通过 dougong Config::parse，每行含 source/config/isolate

Rule: REQ-FR-CONFIG-3 — schema 校验不阻塞

场景: 单行 config 校验失败不阻塞其余槽位
  假设 某插件行的 config 违反其 ConfigSchema
  当 执行装配收敛时
  那么 该行进入 failures，其余插件正常装载

Rule: REQ-FR-CONFIG-4 — 受限条件子集

场景: 超出受限子集的条件表达式被拒绝
  假设 patch 行使用任意 JS 表达式
  当 解析 fr.patch.yml
  那么 返回明确解析错误并拒绝该行

场景: env 门控与平台三元按字面语义求值
  假设 patch 行含 env.FR_X 存在性门控与 linux 平台三元
  当 分别在 FR_X 设置与未设置、不同平台断言下求值时
  那么 按存在性/相等与平台真值求值，无 JS 求值器介入

Rule: REQ-FR-CONFIG-5 — dump 命令即生成物

场景: dump 命令输出来自真实装配
  当 运行 fr dump-config 与 fr dump-config-schema 时
  那么 分别输出合成后插件树与全部插件 config schema

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

Rule: REQ-FR-CONFIG-8 — 三形态与信任校验

场景: 第一方名配 wasm source 被拒
  假设 patch 行 name 为 fr-session 且 source kind 为 wasm
  当 fr-config 信任边界校验时
  那么 该行进入 failures 且不装载

场景: dylib 镜像永不卸载
  假设 已加载一个第三方 dylib 插件
  当 变更其补丁行并 reconcile 时
  那么 代码镜像保持驻留不卸载，换版本要求重启进程

Rule: REQ-FR-CONFIG-9 — Invoker 接入与 realm

场景: 会话 realm 域内解析不外泄
  假设 会话 realm 内放置了同名插件
  当 会话内 require 该服务时
  那么 先命中域内实现，域内 provide 不外泄到全局默认

场景: 第三方工具经 Invoker 通道接入
  当 第三方 wasm 工具插件接入时
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
  假设 装配分别包含 dylib 与 wasm source 的第三方行
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

## Dependencies

None.

## Source Trace

- doc/prd.md#6.1: 装配 = 有序 bundle 栈；后行按 id 整行替换前行（含 config、disabled）
- doc/prd.md#6.2: 编译产物为 dougong loader 文档；"分层、行覆盖、YAML 在 dougong 中一概不存在——它们全部是 fr-config 的职责"
- doc/prd.md#6.3: 每个插件 crate 导出 static ConfigSchema；失败进入 Report::failures 不阻塞其他槽；fr dump-config / dump-config-schema
- doc/prd.md#6.4: 关键默认值表
- confidence: high（PRD 第 6 节字段级规格，dougong 事实已一手核实）
- doc/dsh-source-notes.md: 源码级技术关键点（file:line 证据）

## Open Questions

None.
