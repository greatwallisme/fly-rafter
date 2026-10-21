spec: task
name: "模型接入层与三 provider"
tags: [requirements, generated-draft, llm, provider]
satisfies: [REQ-FR-LLM]
---

## Intent

模型接入是 harness 的动力源。fly-rafter 需要统一的 LlmAdapter seam 与流协议（StreamChunk 七种），第一天交付三个 provider 插件，且不带任何 dsh 私有 wire 扩展。

## Decisions

- Generated draft from KLL requirement artifact; human review must confirm boundaries and test selectors before implementation.
- LlmAdapter trait MUST（Rust 方法名按 PRD 5.1 用 snake_case）：唯一必须实现 `stream(options) -> StreamChunk 流`；另有 provider_info / resolve_model / prepare_call / list_models / provider_retry_policy / image_request_pricing / image_request_target（dsh 源码勘误：PRD 曾漏 imageRequestTarget [llm/src/index.ts:248-250]）；prepare_call 支持代际绑定防竞态。
- GenerateOptions MUST 为 Rust 结构体（snake_case 字段）：provider/model/messages/tools/tool_history/temperature/max_tokens/stop/signal/session_id/purpose（`compaction` | `session-title`）。
- StreamChunk MUST 为七种：block-start / text-delta / reasoning-delta / tool-call-delta / block-end / usage / finish（finish 携带重放信封）；block 索引关联交错 delta；tool arguments 保持原始 JSON 字符串直到 tool/call 事件 unparsed 存储；usage MUST 在 terminal finish 之前。
- 三个 provider 插件 MUST 第一天交付：fr-llm-deepseek（默认，Anthropic Messages 协议，base-url `https://api.deepseek.com/anthropic`，头 `anthropic-version: 2023-06-01`，含 Files API 与图片，SSE 解析，不带任何 dsh 私有 wire 扩展——无 dsh_session_log / dsh_plugin_packages / x-deepseek-harness-*）；fr-llm-openai（OpenAI-compatible，base-url/model/key 全可配）；fr-llm-anthropic（Anthropic 官方端点）。
- 鉴权经环境变量/配置；headless 与 SDK 形态 MUST NOT 挂交互式凭证库（对齐 dsh headless 禁用 credentials 的做法）。
- 装配结算 MUST：已 close 索引上的 delta 忽略、重复 close 忽略、无 finish 默认 stop、缺 id 或 name 兜底；max-tokens 丢弃全部 tool-call 块且 envelope 同步过滤；中断仅保留非空白 text 与 reasoning；ReplayEnvelope 为 {response, blocks}，丢块同位置过滤、长度不等整体丢弃（dsh assembler.ts:49-189）。
- assistant/message 内嵌流持久化打包 MUST 为四 record 类：text / reasoning / tool-call-chunks / chunk；时间为 dt 差分；id/name 不全的 tool-call-delta 落 raw record；block-start/end、usage、finish 恒 raw record（dsh assistant-stream.ts:107-190）。
- prepare_call 代际绑定 MUST：PreparedAdapterCall 冻结元数据；stream 一次性，二次派发报 INVALID_PREPARED_CALL；callConfigEquals 防跨代 capability 漂移（dsh llm/src/index.ts:956-1020）。
- 重试策略 MUST：mode normal（maxRetries 5、retryableCodes 五类）| always；backoff 500ms 起步、上限 10s、jitter 0.1 对称抖动；执行方为独立 retry 插件挂 agent 失败步扩展点（dsh retry-policy.ts:211-262）。
- fr-llm-deepseek 协议细则 MUST：messagesApiRoot 强制补全 /v1 后缀；SSE 经事件解析器，帧 event 名与 body.type 不一致 → 错误码 MALFORMED_RESPONSE；message_stop 三重校验（stop_reason 合法 + 全部块 closed + tool arguments 为合法 JSON）；stop_reason 按映射表转 finish kind；usage 自 message_start 起底、后续 delta 覆盖。
- 其余 provider 适配语义 MUST：适配层永不中途 throw，错误以 usage + finish(kind error/aborted) 事件收尾；toolcall_end 将已解析参数 stringify 回原始串（与 deepseek 逐段拼接同契约）；toolUpdate 能力差异按 provider 固定：anthropic → in-history、openai-completions → addition-only、responses 系无（dsh llm-pi-ai/src/stream.ts:202-227、models.ts:80-87）。

## Boundaries

### Allowed Changes
- crates/fr-llm/**
- crates/fr-llm-deepseek/**
- crates/fr-llm-openai/**
- crates/fr-llm-anthropic/**

### Forbidden
- Do not weaken or remove the source requirement clauses.
- Do not mark this generated draft complete until each `Test:` selector names a real test.
- 任何领域 crate 不得依赖入口 crate（fr-headless/fr-tui/fr-acp/fr-sdk）
- 不得修改本合约 satisfies 指向的 knowledge/requirements 条款来让验证通过
- 不得引入 PRD 19.1 排除清单条目

## Completion Criteria

Scenario: prepare_call 代际绑定防竞态
  Test: pending_req_fr_llm_prepare_call
  假设 同一 session 连续两次 prepare_call
  当 持过期代际句柄发起调用时
  那么 过期句柄被拒

Scenario: purpose 标记传递
  Test: pending_req_fr_llm_purpose
  假设 压缩引擎与标题生成分别发起请求
  当 构造 GenerateOptions 时
  那么 purpose 分别为 compaction 与 session-title

Scenario: usage 先于 terminal finish
  Test: pending_req_fr_llm_usage_terminal_finish
  假设 一次流式请求正常结算
  当 消费 StreamChunk 流时
  那么 usage chunk 出现在 terminal finish 之前

Scenario: tool arguments 原始串保持
  Test: pending_req_fr_llm_tool_arguments
  假设 流中 tool-call-delta 交错到达
  当 拼接 arguments 时
  那么 拼接结果与 provider 原始 JSON 字符串逐字节一致，tool/call 事件按 unparsed 存储

Scenario: deepseek 无私有 wire 扩展
  Test: pending_req_fr_llm_deepseek_wire
  假设 使用 fr-llm-deepseek 发起请求
  当 检查发出的 HTTP 请求时
  那么 目标为 base-url 加 anthropic-version 头，且无 dsh_session_log、dsh_plugin_packages 与 x-deepseek-harness 头

Scenario: openai base-url 覆盖
  Test: pending_req_fr_llm_openai_base_url
  假设 配置了自定义 base-url
  当 fr-llm-openai 发起请求时
  那么 请求发往配置端点而非硬编码地址

Scenario: anthropic 官方端点
  Test: pending_req_fr_llm_anthropic
  假设 使用 fr-llm-anthropic
  当 发起请求时
  那么 请求发往 Anthropic 官方端点且协议头正确

Scenario: headless 与 SDK 无交互凭证库
  Test: pending_req_fr_llm_headless_sdk
  假设 headless 或 SDK 形态启动
  当 检查装配与凭证读取时
  那么 仅从环境变量与配置读取，交互式凭证库插件不在装配中

Scenario: dsh 录制 SSE 回放产出相同 chunk 序
  Test: pending_req_fr_llm_dsh_sse_chunk
  假设 以 dsh 真实响应录制的 SSE fixture 为输入
  当 三 provider 的 SSE 解析器回放该 fixture 时
  那么 产出的 StreamChunk 序列与 dsh 解析结果一致

Scenario: 采样参数透传 wire
  Test: pending_req_fr_llm_wire
  假设 GenerateOptions 携带 temperature、max_tokens 与 stop
  当 provider 构造协议请求时
  那么 三者按协议字段透传

Scenario: max-tokens 与中断的块处理
  Test: pending_req_fr_llm_max_tokens
  假设 流分别以 max-tokens 结束与中途中断
  当 装配结算时
  那么 max-tokens 丢弃全部 tool-call 块且 envelope 同步过滤；中断仅保留非空白 text 与 reasoning

Scenario: chunk block-start 字段
  Test: pending_req_fr_llm_chunk_block_start
  假设 适配器流式输出
  当 产生 block-start chunk 时
  那么 字段与 dsh types.ts 470 行定义逐字一致

Scenario: chunk text-delta 字段
  Test: pending_req_fr_llm_chunk_text_delta
  假设 适配器流式输出
  当 产生 text-delta chunk 时
  那么 字段与 dsh types.ts 470 行定义逐字一致

Scenario: chunk reasoning-delta 字段
  Test: pending_req_fr_llm_chunk_reasoning_delta
  假设 适配器流式输出
  当 产生 reasoning-delta chunk 时
  那么 字段与 dsh types.ts 470 行定义逐字一致

Scenario: chunk tool-call-delta 字段
  Test: pending_req_fr_llm_chunk_tool_call_delta
  假设 适配器流式输出
  当 产生 tool-call-delta chunk 时
  那么 字段与 dsh types.ts 470 行定义逐字一致

Scenario: chunk block-end 字段
  Test: pending_req_fr_llm_chunk_block_end
  假设 适配器流式输出
  当 产生 block-end chunk 时
  那么 字段与 dsh types.ts 470 行定义逐字一致

Scenario: chunk usage 字段
  Test: pending_req_fr_llm_chunk_usage
  假设 适配器流式输出
  当 产生 usage chunk 时
  那么 字段与 dsh types.ts 470 行定义逐字一致

Scenario: chunk finish 字段
  Test: pending_req_fr_llm_chunk_finish
  假设 适配器流式输出
  当 产生 finish chunk 时
  那么 字段与 dsh types.ts 470 行定义逐字一致

Scenario: usage 强制先于 finish
  Test: pending_req_fr_llm_usage_finish
  假设 正常结算流
  当 消费 chunk 序列时
  那么 usage 出现在 terminal finish 之前且其后无任何 chunk

Scenario: 重复 close 忽略
  Test: pending_req_fr_llm_close
  假设 同一 block 索引收到两次 block-end
  当 装配时
  那么 首次生效后续忽略

Scenario: delta 落在已关闭块
  Test: pending_req_fr_llm_delta
  假设 delta 的目标块已 block-end
  当 装配时
  那么 该 delta 被忽略

Scenario: 缺 finish 默认 stop
  Test: pending_req_fr_llm_finish_stop
  假设 流结束但无 finish chunk
  当 装配时
  那么 默认 finish kind 为 stop

Scenario: 内嵌流四 record 打包
  Test: pending_req_fr_llm_record
  假设 一次成功 step 的 StreamChunk 全集
  当 打包进 assistant/message 时
  那么 text 与 reasoning 与 tool-call-chunks 与 chunk 四类 record、dt 差分时间、id/name 不全的 tool-call-delta 落 raw、block-start/end 与 usage 与 finish 恒 raw

Scenario: 二次派发拒绝
  Test: pending_req_fr_llm_requirement
  假设 同一 PreparedAdapterCall 的 stream 已被消费
  当 再次派发时
  那么 报 INVALID_PREPARED_CALL；callConfig 跨代 capability 不匹配亦被拒

Scenario: normal 模式五次退避
  Test: pending_req_fr_llm_normal
  假设 retryable 错误连续发生
  当 重试结算时
  那么 最多 5 次、backoff 500ms 起步 10s 封顶、jitter 0.1 对称抖动；always 模式不限次数

Scenario: /v1 补全与 SSE 帧校验
  Test: pending_req_fr_llm_v1_sse
  假设 配置根地址缺 /v1 且录制帧 event 名与 body.type 不一致
  当 构造请求与解析 SSE 时
  那么 请求路径强制补全 /v1，不一致帧报 MALFORMED_RESPONSE

Scenario: message_stop 三重校验
  Test: pending_req_fr_llm_message_stop
  假设 stop_reason 非法、或存在未闭合块、或 tool arguments 非合法 JSON
  当 message_stop 校验时
  那么 三种情形均拒绝；正常时 stop_reason 按映射表转 finish kind、usage 以 message_start 起底被 delta 覆盖

Scenario: 错误不中途抛出
  Test: pending_req_fr_llm_requirement
  假设 上游连接中途失败
  当 适配层结算时
  那么 发 usage 与 finish(kind error) 收尾而非 panic；toolcall_end 参数串与 provider 原始串逐字节一致

Scenario: toolUpdate 能力差异
  Test: pending_req_fr_llm_toolupdate
  假设 同一工具更新分别路由到 anthropic 与 openai-completions 与 responses 系模型
  当 应用更新时
  那么 分别为 in-history 追加、addition-only、不支持（拒新更新）

## Questions

- Source trace: doc/prd.md#13.1: LlmAdapter seam、StreamChunk 七种、usage 先于 finish、prepare_call 代际绑定, doc/prd.md#13.2: 三 provider 表；deepseek 无私有 wire 扩展（19.3）, doc/prd.md#19.3: 仅标准 Anthropic Messages 协议, confidence: high, doc/dsh-source-notes.md: 源码级技术关键点（file:line 证据）
- Replace pending test selectors with real test names before lifecycle verification.
