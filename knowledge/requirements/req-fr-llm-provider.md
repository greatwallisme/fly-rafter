---
kind: requirement
id: REQ-FR-LLM
title: "模型接入层与三 provider"
status: accepted
liveness: auto
tags: [llm, provider]
---

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
- [REQ-FR-LLM-11] 其余 provider 适配语义 MUST：适配层永不中途 throw，错误以 usage + finish(kind error/aborted) 事件收尾；toolcall_end 将已解析参数 stringify 回原始串（与 deepseek 逐段拼接同契约）；toolUpdate 能力三 provider 均无声明（2026-10-27 A2 裁决勘误）：anthropic in-history 需双 compat 旗标（ANTHROPIC_COMPAT_GATE withhold，手声明路由不可配）+内置 catalog（fr 无，19.2 条 20）结构性不可达；fr-llm-openai 两协议维持 Q74-B 无能力判定（COMPLETIONS/RESPONSES_COMPAT_GATE 双 withhold）；一律基线折叠语义（工具全集每请求重发），延迟装载不可用（真源 llm-pi-ai models.ts:75-92、anthropic-messages.ts:1141-1142；原 Q59-a 引证 stream.ts:202-227 有误系 toStreamChunks error→finish 段）。
- [REQ-FR-LLM-12] 三 provider 形态与命名 MUST（2026-10-25 Q56A/Q57B/Q58 裁决，PRD 决策 37）：三 crate 平切维持（fr-llm-deepseek/fr-llm-openai/fr-llm-anthropic；1:1 口径=行为与协议对齐，不镜像 dsh TS 包结构——dsh 真实形态为 llm-deepseek 直连 + llm-pi-ai 库背书多路由）；fr-llm-openai MUST 实现 openai-completions 与 openai-responses 两协议，fr-llm-anthropic 实现 anthropic-messages；fr-llm-deepseek 保留 dsh 自有 DEFAULT_MODELS 内置目录（2 条目逐字），fr-llm-openai 与 fr-llm-anthropic 第一天 MUST NOT 内置模型 catalog——路由 models 全手声明、请求引用未声明模型即报错（fail-closed，19.2 条 20）；attribution User-Agent MUST 为 `fly-rafter/<version> (+<url>)` 格式（version 取 CARGO_PKG_VERSION；url = 'https://github.com/greatwallisme/fly-rafter'，2026-10-25 用户给定，去 .git 对齐 dsh attribution.ts:43 主页形态）且缺省不可抑制；重放信封 response.kind：deepseek 侧 'deepseek-messages' 逐字保留、openai 族 'fr-openai'、anthropic 族 'fr-anthropic'、不可用重放降级哨兵 'fr-foreign'（adapter 私有 durable 元数据值域=自有命名，非会话 v4 结构字段）；fr-llm-deepseek Files 上传文件名前缀逐字 'dsh-'（同 endpoint+key 命名空间与 dsh 共享上传池，配额回收互通，用户裁决接受），upload-index 路径 <FR_HOME>/llm-deepseek/files-v3.json（formatVersion 3）。
- [REQ-FR-LLM-13] 重试策略解析诊断 MUST（2026-10-25 Q60-A 裁决，PRD 决策 38）：resolveRetryPolicy/resolveBackoff 全部八条拒绝消息与 dsh retry-policy.ts:128-193 逐字同文——形 `{path}.{field} must be ...`，backoff 臂路径带 `.backoff` 中段（调用方传完整 path 前缀，对齐 dsh config.ts:500/327 调用形）；normal 臂校验顺序 maxRetries→retryableCodes→backoff 同 dsh 臂内顺序；dsh mode 臂与 validateKeys unknown-key 臂由 fr 类型层与 provider 反序列化层承载（无运行时文本，非缺口）。
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
  假设 同一工具更新分别路由到 anthropic 与 fr-llm-openai 两协议模型（2026-10-27 A2 修订）
  当 应用更新时
  那么 三 provider 均无 toolUpdate 能力——anthropic in-history 需双 compat 旗标（withhold 不可配）+内置 catalog（fr 无）结构性不可达；openai 两协议维持 Q74-B 判定；一律基线折叠（工具全集每请求重发），延迟装载不可用

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

Rule: REQ-FR-LLM-13 — 重试策略解析诊断

场景: 诊断文案与校验顺序逐字钉死
  假设 用户在 provider 配置中写非法 retryPolicy
  当 resolveRetryPolicy 拒绝时
  那么 八条消息与 dsh retry-policy.ts:128-193 逐字同文（含 .backoff 路径中段）、normal 臂校验顺序 maxRetries 先于 retryableCodes 先于 backoff

## Dependencies

- REQ-FR-CONFIG

## Source Trace

- doc/prd.md#13.1: LlmAdapter seam、StreamChunk 七种、usage 先于 finish、prepare_call 代际绑定
- doc/prd.md#13.2: 三 provider 表；deepseek 无私有 wire 扩展（19.3）
- doc/prd.md#19.3: 仅标准 Anthropic Messages 协议
- confidence: high
- doc/dsh-source-notes.md: 源码级技术关键点（file:line 证据）
- doc/prd.md#13.2（2026-10-25 Q56A/Q57B/Q58 裁决段）+ PRD 决策 37 + 19.2 条 20：三 crate 平切/catalog 缺位/attribution/重放 kind/Files 前缀与索引路径
- Q58-1a url 已给定回填（2026-10-25，去 .git 对齐 dsh 主页形态）；Q59-a 已勘误落条款；Q60-A 已落 LLM-13 与场景（retry 诊断逐字）——本块无悬置

## Open Questions

None.
