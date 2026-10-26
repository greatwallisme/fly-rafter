spec: task
name: "模型接入层与三 provider"
tags: [requirements, generated-draft, llm, provider]
satisfies: [REQ-FR-LLM]
---

## Intent

模型接入是 harness 的动力源：统一的 LlmAdapter seam 与流协议（StreamChunk 七种），第一天交付三个 provider 插件，且不带任何 dsh 私有 wire 扩展。本合同覆盖 REQ-FR-LLM 全域；切片顺序先 seam 后 provider：当前切片交付 fr-llm 纯行为库（装配结算/错误分类学/重试策略/请求投影/attribution/api-key/assistant-stream 打包行为/prepare_call 代际绑定），三个 provider crate 的协议面（SSE 解析/Files API/凭据）随后续切片逐场景绑定。

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
- 其余 provider 适配语义 MUST：适配层永不中途 throw，错误以 usage + finish(kind error/aborted) 事件收尾；toolcall_end 将已解析参数 stringify 回原始串（与 deepseek 逐段拼接同契约）；toolUpdate 能力差异按 provider 固定：anthropic → in-history、openai-completions → addition-only、responses 系（openai-responses/azure-openai-responses/openai-codex-responses）在 compat 声明 supportsAdditionalTools 或 supportsToolSearch 时 → addition-only、否则无（2026-10-25 Q59-a 勘误对齐 dsh 真源，dsh llm-pi-ai/src/stream.ts:202-227、models.ts:75-92）。
- 三 provider 形态与命名 MUST（2026-10-25 Q56A/Q57B/Q58 裁决，PRD 决策 37）：三 crate 平切维持（fr-llm-deepseek/fr-llm-openai/fr-llm-anthropic；1:1 口径=行为与协议对齐，不镜像 dsh TS 包结构——dsh 真实形态为 llm-deepseek 直连 + llm-pi-ai 库背书多路由）；fr-llm-openai MUST 实现 openai-completions 与 openai-responses 两协议，fr-llm-anthropic 实现 anthropic-messages；fr-llm-deepseek 保留 dsh 自有 DEFAULT_MODELS 内置目录（2 条目逐字），fr-llm-openai 与 fr-llm-anthropic 第一天 MUST NOT 内置模型 catalog——路由 models 全手声明、请求引用未声明模型即报错（fail-closed，19.2 条 20）；attribution User-Agent MUST 为 `fly-rafter/<version> (+<url>)` 格式（version 取 CARGO_PKG_VERSION；url = 'https://github.com/greatwallisme/fly-rafter'，2026-10-25 用户给定，去 .git 对齐 dsh attribution.ts:43 主页形态）且缺省不可抑制；重放信封 response.kind：deepseek 侧 'deepseek-messages' 逐字保留、openai 族 'fr-openai'、anthropic 族 'fr-anthropic'、不可用重放降级哨兵 'fr-foreign'（adapter 私有 durable 元数据值域=自有命名，非会话 v4 结构字段）；fr-llm-deepseek Files 上传文件名前缀逐字 'dsh-'（同 endpoint+key 命名空间与 dsh 共享上传池，配额回收互通，用户裁决接受），upload-index 路径 <FR_HOME>/llm-deepseek/files-v3.json（formatVersion 3）。
- fr-llm 定位与形态（PRD 4.5 :167 条款执行，2026-10-25 返工裁定）：fr-llm 为 **cdylib+rlib 双输出插件工件**（PRD 4.5 「crates/ 下除 fr、fr-config、fr-wire 外全部为 cdylib 插件工件，内核家族仅依赖 dougong-abi，五步契约经 descriptor 表达」）——cdylib 面为 plugin_shell（descriptor `provide: ["llm"]`，即 fr-agent-loop PRD :336 `inject llm` 的服务提供者；宿主不含逻辑铁律下该服务只能由本工件承载）；rlib 面为行为库，供三个 provider 工件与 fr-agent-loop 工件静态链接副本（同 fr-wire 「各插件工件链接副本，指针不跨界」规则），wire 形状复用 fr-wire 不重复定义。`llm` 服务的 Invoker 方法号表与载荷 schema（PRD :131 契约框架）由 provider 与 agent-loop 消费切片细化；Q44 出站织物裁决（决策 36，Phase 15 前）管出站通路细节，不改变本工件归属。
- LlmAdapter trait：唯一必须实现 `fn stream(&self, options: GenerateOptions) -> Pin<Box<dyn Stream<Item = StreamChunk> + Send>>`（futures-core Stream 装箱，对齐 dsh AsyncIterable<StreamChunk>）；默认方法七件 provider_info/provider_retry_policy/image_request_pricing/image_request_target/list_models/resolve_model/prepare_call 对齐 dsh llm/src/index.ts:208-303（2026-10-25 Q62B：list_models/resolve_model/prepare_call 三件按 AFIT async 声明对齐 dsh Promise 面，trait dyn 不兼容的代价由后续 runtime 注册表切片以泛型/装箱面承载）。
- PreparedAdapterCall 代际绑定：非 Clone（代际句柄不可复制）；内部一次性派发门（重复派发 INVALID_PREPARED_CALL）；派发时逐字段比较 LlmCallConfig（call_config_equals 含 stop 逐元素）不匹配即 INVALID_PREPARED_CALL。
- AGENT_LOOP_REQUESTS WeakSet 对象身份标记（dsh call-config.ts:13）映射为 LlmCallConfig 的 `agent_loop: bool` 字段——Rust 值语义等价映射，内部面非协议。
- LlmError 构造校验（dsh error.ts/index.ts:96-131）：message 与 code 非空、status 100..=599、provider_retry_after_ms 正有限、request_id 非空、offload_images 仅随 IMAGE_OFFLOAD_REQUIRED；错误码常量集为共享词汇（AUTH/QUOTA/RATE_LIMIT/CONTEXT_WINDOW_EXCEEDED/SERVER/TIMEOUT/TRANSPORT/EMPTY_RESPONSE/INVALID_CREDENTIAL/IMAGE_OFFLOAD_REQUIRED 等）。
- attribution：APP_IDENTITY 常量（product fly-rafter / version env!("CARGO_PKG_VERSION") / url https://github.com/greatwallisme/fly-rafter）；user_agent() 渲染 `product/version (+url)`；headers() 产出小写 `user-agent` 头，无任何抑制路径。
- assistant-stream 行为（wire record 形状已在 fr-wire）：Accumulator 打包规则（同型相邻+同 index+tool-call 同 id+name 有无一致+时间差可逆才入 run；id 空串/name 空串不打包；block-start/end、usage、finish 恒 raw）；expand 为 durable 边界验证路径（exact keys、dt.length==members-1、时间保持安全整数）。
- 2026-10-25 Q61A（用户裁决）：`AssistantStreamRecord` 的 exact-keys 由 fr-wire serde `deny_unknown_fields` 在解析期承载（等价 dsh assistant-stream.ts:530-534 exactKeys）；Allowed 增列 crates/fr-wire/src/stream.rs 单文件。
- 2026-10-25 Q63A（用户裁决）：Accumulator 对未知 chunk kind（`StreamChunk::Other`）落 raw record 无损保留——系 fr-wire merge-extensible 既有裁决的下游扩展语义，非 dsh 偏差，不入 19.2。
- 2026-10-26 Q66-B（用户裁决）：凭据诊断三条文案（MISSING_CREDENTIAL 一条 + INVALID_CREDENTIAL 两臂）由 dsh 的 credentials-service/web-Models-page 设施引用改为 fly-rafter configuration 自有措辞——19.2 条 21；错误码与触发语义逐字不变。
- 2026-10-26 Q67-B（用户裁决）：deepSeekFileScope 采纳 sha2 crate（0.10.9）——Q65-A 零新增外部 crate 姿态的首个记录例外（PRD 决策 40）；Allowed 增列根 Cargo.toml 与 Cargo.lock（依赖边授权）。

## Boundaries

### Allowed Changes
- crates/fr-llm/**
- crates/fr-llm-deepseek/**
- crates/fr-llm-openai/**
- crates/fr-llm-anthropic/**
- crates/fr-wire/src/stream.rs
- /Cargo.toml
- /Cargo.lock

### Forbidden
- Do not weaken or remove the source requirement clauses.
- Do not mark this generated draft complete until each `Test:` selector names a real test.
- 任何领域 crate 不得依赖入口 crate（fr-headless/fr-tui/fr-acp/fr-sdk）
- 不得修改本合约 satisfies 指向的 knowledge/requirements 条款来让验证通过
- 不得引入 PRD 19.1 排除清单条目

## Completion Criteria

### Rule: adapter-contract — adapter 契约与代际绑定

场景: prepare_call 代际绑定防竞态
  测试: prepare_call_rejects_stale_generation
  假设 同一 session 连续两次 prepare_call
  当 持过期代际句柄发起调用时
  那么 过期句柄被拒

场景: 二次派发拒绝
  测试: prepared_call_single_dispatch
  假设 同一 PreparedAdapterCall 的 stream 已被消费
  当 再次派发时
  那么 报 INVALID_PREPARED_CALL；callConfig 跨代 capability 不匹配亦被拒

场景: purpose 标记传递
  测试: generate_options_purpose_values
  假设 压缩引擎与标题生成分别发起请求
  当 构造 GenerateOptions 时
  那么 purpose 分别为 compaction 与 session-title

### Rule: stream-protocol — 流协议形状与契约

场景: chunk block-start 字段
  测试: stream_chunk_wire_fields_pin_dsh_types
  假设 适配器流式输出
  当 产生 block-start chunk 时
  那么 字段与 dsh types.ts 470 行定义逐字一致

场景: chunk text-delta 字段
  测试: stream_chunk_wire_fields_pin_dsh_types
  假设 适配器流式输出
  当 产生 text-delta chunk 时
  那么 字段与 dsh types.ts 470 行定义逐字一致

场景: chunk reasoning-delta 字段
  测试: stream_chunk_wire_fields_pin_dsh_types
  假设 适配器流式输出
  当 产生 reasoning-delta chunk 时
  那么 字段与 dsh types.ts 470 行定义逐字一致

场景: chunk tool-call-delta 字段
  测试: stream_chunk_wire_fields_pin_dsh_types
  假设 适配器流式输出
  当 产生 tool-call-delta chunk 时
  那么 字段与 dsh types.ts 470 行定义逐字一致

场景: chunk block-end 字段
  测试: stream_chunk_wire_fields_pin_dsh_types
  假设 适配器流式输出
  当 产生 block-end chunk 时
  那么 字段与 dsh types.ts 470 行定义逐字一致

场景: chunk usage 字段
  测试: stream_chunk_wire_fields_pin_dsh_types
  假设 适配器流式输出
  当 产生 usage chunk 时
  那么 字段与 dsh types.ts 470 行定义逐字一致

场景: chunk finish 字段
  测试: stream_chunk_wire_fields_pin_dsh_types
  假设 适配器流式输出
  当 产生 finish chunk 时
  那么 字段与 dsh types.ts 470 行定义逐字一致

场景: usage 强制先于 finish
  测试: assembled_usage_precedes_finish
  假设 正常结算流
  当 消费 chunk 序列时
  那么 usage 出现在 terminal finish 之前且其后无任何 chunk

场景: usage 先于 terminal finish
  测试: assembled_usage_precedes_finish
  假设 一次流式请求正常结算
  当 消费 StreamChunk 流时
  那么 usage chunk 出现在 terminal finish 之前

场景: tool arguments 原始串保持
  测试: assembler_tool_arguments_raw_roundtrip
  假设 流中 tool-call-delta 交错到达
  当 拼接 arguments 时
  那么 拼接结果与 provider 原始 JSON 字符串逐字节一致，tool/call 事件按 unparsed 存储

### Rule: assembly-settlement — 装配结算

场景: 重复 close 忽略
  测试: assembler_ignores_repeated_close
  假设 同一 block 索引收到两次 block-end
  当 装配时
  那么 首次生效后续忽略

场景: delta 落在已关闭块
  测试: assembler_ignores_delta_after_close
  假设 delta 的目标块已 block-end
  当 装配时
  那么 该 delta 被忽略

场景: 缺 finish 默认 stop
  测试: assembler_default_finish_is_stop
  假设 流结束但无 finish chunk
  当 装配时
  那么 默认 finish kind 为 stop

场景: max-tokens 与中断的块处理
  测试: assembler_max_tokens_and_interrupt
  假设 流分别以 max-tokens 结束与中途中断
  当 装配结算时
  那么 max-tokens 丢弃全部 tool-call 块且 envelope 同步过滤；中断仅保留非空白 text 与 reasoning

### Rule: stream-record-packing — 内嵌流持久化打包

场景: 内嵌流四 record 打包
  测试: assistant_stream_packs_runs_and_raw_chunks
  假设 一次成功 step 的 StreamChunk 全集
  当 打包进 assistant/message 时
  那么 text 与 reasoning 与 tool-call-chunks 与 chunk 四类 record、dt 差分时间、id/name 不全的 tool-call-delta 落 raw、block-start/end 与 usage 与 finish 恒 raw

### Rule: retry-policy — 重试策略数值

场景: normal 模式五次退避
  测试: retry_policy_defaults_and_local_delay
  假设 retryable 错误连续发生
  当 重试结算时
  那么 最多 5 次、backoff 500ms 起步 10s 封顶、jitter 0.1 对称抖动；always 模式不限次数

场景: 诊断文案与校验顺序逐字钉死
  测试: diagnostics_pin_dsh_texts
  假设 用户在 provider 配置中写非法 retryPolicy
  当 resolveRetryPolicy 拒绝时
  那么 八条消息与 dsh retry-policy.ts:128-193 逐字同文（含 .backoff 路径中段）、normal 臂校验顺序 maxRetries 先于 retryableCodes 先于 backoff

### Rule: request-projection — 请求投影

场景: addition-only 投影省略非活动工具
  测试: project_tool_updates_addition_only_drops_inactive
  假设 工具 A 激活后被移除且路由声明 addition-only
  当 投影请求工具声明时
  那么 A 不在声明列表且其历史移除消息被剥除；in-history 路由同输入保留声明与移除消息

场景: in-history 投影保留移除历史
  测试: project_tool_updates_in_history_keeps_removals
  假设 工具 A 激活后被移除且路由声明 in-history
  当 投影请求历史时
  那么 A 的定义仍在声明列表（禁用不弃定义）且移除消息保留

场景: 图片超预算 fail-closed
  测试: required_image_offload_counts_not_evicts
  假设 保留图片的字节与计数表示超出路由预算
  当 计算所需卸载量时
  那么 返回最老出现计数且不自行卸载任何图片，adapter 以 IMAGE_OFFLOAD_REQUIRED 携带该计数失败

### Rule: error-taxonomy — 错误分类学与凭据校验

场景: LlmError 构造校验拒绝非法字段
  测试: llm_error_construction_validates_fields
  假设 分别以空 message、空 code、status 99、status 600、负 delay、非有限 delay、空 request_id 构造
  当 构造 LlmError 时
  那么 七种情形均被拒（构造校验五项：message/code 非空、status 100..=599 整数、provider_retry_after_ms 正有限、request_id 非空）；合法构造保留全部字段——offload_images 与码的配对是类型文档约束非构造校验，其正整数校验属 adapter-failure 快照验证路径（dsh index.ts:96-131、adapter-failure.ts:79）

场景: 上下文溢出与配额措辞识别
  测试: context_window_and_quota_wording_detected
  假设 错误文本分别为上下文溢出五措辞族与配额五措辞族及无关文本
  当 识别函数判定时
  那么 两族各自命中且无关文本不命中

场景: API key 规范化
  测试: normalize_api_key_trims_and_rejects
  假设 分别提供带空白合法 key、空白 key、含非法字符 key
  当 规范化时
  那么 首者去空白返回原值、次者报 empty、末者报 illegalCharacters

### Rule: attribution — 产品归因头

场景: attribution 头格式与不可抑制
  测试: attribution_headers_pin_identity
  假设 任意 provider 发起请求
  当 构造 User-Agent 时
  那么 值为 fly-rafter/<version> (+https://github.com/greatwallisme/fly-rafter) 格式、version 取 crate 元数据，且不存在不携带 attribution 的请求路径

### Rule: provider-deepseek — deepseek 协议面（provider 切片绑定）

场景: deepseek 无私有 wire 扩展
  测试: request_headers_pin_dsh_order_without_harness
  假设 使用 fr-llm-deepseek 发起请求
  当 检查发出的 HTTP 请求时
  那么 目标为 base-url 加 anthropic-version 头，且无 dsh_session_log、dsh_plugin_packages 与 x-deepseek-harness 头

场景: /v1 补全与 SSE 帧校验
  测试: v1_path_completion_and_sse_mismatch
  假设 配置根地址缺 /v1 且录制帧 event 名与 body.type 不一致
  当 构造请求与解析 SSE 时
  那么 请求路径强制补全 /v1，不一致帧报 MALFORMED_RESPONSE

场景: message_stop 三重校验
  测试: message_stop_settlement_arms
  假设 stop_reason 非法、或存在未闭合块、或 tool arguments 非合法 JSON
  当 message_stop 校验时
  那么 三种情形均拒绝；正常时 stop_reason 按映射表转 finish kind、usage 以 message_start 起底被 delta 覆盖

场景: 重放信封 kind 命名
  测试: replay_envelope_pins_dsh_shape
  假设 三 provider 各自产出重放信封
  当 读取 response.kind 时
  那么 deepseek 侧为 'deepseek-messages'、openai 族为 'fr-openai'、anthropic 族为 'fr-anthropic'；不可用重放降级走 'fr-foreign' 哨兵

场景: Files 前缀与索引路径逐字
  测试: parse_index_validates_and_names
  假设 fr-llm-deepseek 经 Files API 上传一张请求图片
  当 检查 provider 端文件名与本地索引时
  那么 文件名以 'dsh-' 开头、索引位于 <FR_HOME>/llm-deepseek/files-v3.json 且 formatVersion 为 3

### Rule: provider-openai — openai 协议面（provider 切片绑定）

场景: openai base-url 覆盖
  测试: pending_req_fr_llm_openai_base_url
  假设 配置了自定义 base-url
  当 fr-llm-openai 发起请求时
  那么 请求发往配置端点而非硬编码地址

场景: openai 路由未声明模型报错
  测试: pending_req_fr_llm_openai
  假设 fr-llm-openai 路由未声明 models
  当 请求引用任一模型 id 时
  那么 请求被拒且错误指名未声明模型，不回退任何内置目录

### Rule: provider-anthropic — anthropic 协议面（provider 切片绑定）

场景: anthropic 官方端点
  测试: pending_req_fr_llm_anthropic
  假设 使用 fr-llm-anthropic
  当 发起请求时
  那么 请求发往 Anthropic 官方端点且协议头正确

### Rule: provider-shared — 三 provider 共有面（provider 切片绑定）

场景: headless 与 SDK 无交互凭证库
  测试: resolve_api_key_pins_dsh_texts
  假设 headless 或 SDK 形态启动
  当 检查装配与凭证读取时
  那么 仅从环境变量与配置读取，交互式凭证库插件不在装配中

场景: dsh 录制 SSE 回放产出相同 chunk 序
  测试: dsh_recorded_sse_replays_chunks
  假设 以 dsh 真实响应录制的 SSE fixture 为输入
  当 三 provider 的 SSE 解析器回放该 fixture 时
  那么 产出的 StreamChunk 序列与 dsh 解析结果一致

场景: 采样参数透传 wire
  测试: projects_replay_degrade_and_request_controls
  假设 GenerateOptions 携带 temperature、max_tokens 与 stop
  当 provider 构造协议请求时
  那么 三者按协议字段透传

场景: 错误不中途抛出
  测试: pending_req_fr_llm_fail_settle
  假设 上游连接中途失败
  当 适配层结算时
  那么 发 usage 与 finish(kind error) 收尾而非 panic；toolcall_end 参数串与 provider 原始串逐字节一致

场景: toolUpdate 能力差异
  测试: pending_req_fr_llm_toolupdate
  假设 同一工具更新分别路由到 anthropic 与 openai-completions 与 responses 系模型（后者 compat 带 supportsAdditionalTools 与不带两种）
  当 应用更新时
  那么 anthropic 为 in-history 追加、openai-completions 为 addition-only、带 compat 标记的 responses 系为 addition-only、不带标记的 responses 系不支持（拒新更新）

## Questions

- Source trace: doc/prd.md#13.1: LlmAdapter seam、StreamChunk 七种、usage 先于 finish、prepare_call 代际绑定, doc/prd.md#13.2: 三 provider 表；deepseek 无私有 wire 扩展（19.3）, doc/prd.md#19.3: 仅标准 Anthropic Messages 协议, confidence: high, doc/dsh-source-notes.md: 源码级技术关键点（file:line 证据）, doc/prd.md#13.2（2026-10-25 Q56A/Q57B/Q58 裁决段）+ PRD 决策 37 + 19.2 条 20：三 crate 平切/catalog 缺位/attribution/重放 kind/Files 前缀与索引路径, Q58-1a url 已给定回填（2026-10-25，去 .git 对齐 dsh 主页形态）；Q59-a 已勘误落条款——本块无悬置
- 本切片（fr-llm seam 库）已绑定 19 个真实测试名（覆盖 22 个场景）；provider 协议面 14 场景保持 pending，由后续 provider 切片绑定。
