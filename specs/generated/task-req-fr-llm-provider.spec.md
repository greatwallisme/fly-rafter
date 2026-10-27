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
- 其余 provider 适配语义 MUST：适配层永不中途 throw，错误以 usage + finish(kind error/aborted) 事件收尾；toolcall_end 将已解析参数 stringify 回原始串（与 deepseek 逐段拼接同契约）；toolUpdate 能力三 provider 均无声明（2026-10-27 A2 裁决勘误 anthropic 臂）：anthropic 的 in-history 需 supportsMidConvoSystemMessages+supportsMidConvoToolChanges 双 compat 旗标，ANTHROPIC_COMPAT_GATE 双 withhold（catalog.ts:292-307）=手声明路由不可配，dsh 侧可达性全靠 pi-ai 内置 catalog（data/anthropic.json 实证），fr 按 19.2 条 20 无 catalog → in-history/延迟装载/双占位符（DeferredToolPlaceholder+__pi_deferred_placeholder__）/tool_addition·tool_removal wire 块/inline-tools beta 结构性不可达；fr-llm-openai 两协议维持 2026-10-26 Q74-B 裁决（COMPLETIONS/RESPONSES_COMPAT_GATE 双 withhold，catalog.ts:273-274/286-287，detectCompat 恒 false，conversationUpdates 恒返回空，llm-pi-ai models.ts:75-92）——三 provider 一律基线折叠语义（system 更新折入 leading system、工具全集每请求重发，transcript.ts:108-120），延迟装载不可用（原 Q59-a 勘误引证 stream.ts:202-227 系 toStreamChunks 的 error→finish 段，与 toolUpdate 无关）。
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
- 2026-10-26 Q68-C/Q69-B/Q70-B/Q71-B/Q72-A（用户裁决，PRD 决策 41）：切片 4 = fr-llm-deepseek 网络执行面——Q69-B 生产 HTTP 载体 ureq 3.5（默认 rustls）阻塞式内置 crate（插件自有线程阻塞读 → channel → Stream 适配；dsh fetch 注入缝保留钉测）；Q70-B dsh-timeout 移植为独立 crate fr-timeout（Allowed 增列 crates/fr-timeout/**）；Q71-B request-image 归一化管线本切片实现于 fr-llm-deepseek（image 0.25 + libwebp-sys method=0 对齐 WEBP_ENCODING_EFFORT=0；wire 可见面逐字、像素级字节差异同编解码器内部差不入 19.2）；Q72-A request-extensions 全量移植（no-op 默认）。 Q73-A（用户裁决）适配器执行面错误通道：stream() 终结前 emit Other('fr-error' 记录，Q58-2 自有命名判例同构) 后终止，runtime 注册表切片消费映射。
- 2026-10-26 Q74-B（用户裁决，PRD 决策 42）：toolUpdate 合约修订——fr-llm-openai 两协议（openai-completions/openai-responses）均无 toolUpdate 能力声明，对齐 dsh hand-declared 真源（COMPLETIONS/RESPONSES_COMPAT_GATE 旗标双 withhold + detectCompat 恒 false → conversationUpdates 恒返回空）；原 Q59-a 勘误引证有误（llm-pi-ai/src/stream.ts:202-227 系 toStreamChunks error→finish 段）；「其余 provider 适配语义」条款与「toolUpdate 能力差异」场景已按本裁决修订；fr-llm-anthropic 的 in-history 行本次不动（其真源读在彼切片进行）。
- 2026-10-26 切片 5 批 0 推定追认（用户裁决，PRD 决策 42）：D5 api 值域仅 openai-completions/openai-responses（azure/codex 不入）；D6 transport/websocket 配置键省略；D7 cost 恒零（NO_COST 同构）；D8 constrained-sampling/grammar tools 不移植（dsh 双重不可达，strict mode 保留可配）；D9 discovery/login/settings-directory/registry 面全排除（dsh settings 生态，M1 headless 无此面）；D10 auth=env-only（api-key-env 命名可配、值仅来自进程环境；命名缺失 MISSING_CREDENTIAL 按 Q66-B 措辞判例；未命名无 key → pi-ai 'No API key for provider: X' 逐字；D10c=a 文件型密钥源不做）；D10b 路由形态=providers dict 多路由（dsh 真源同构，每 route 独立 api/baseURL/models/compat/retry-policy/api-key-env 全套 profile 键）；D11 pi SDK 层重试不移植（dsh 恒 maxRetries:0 单次网络尝试，重试由 recovery 层按 providerRetryPolicy 执行——fr-llm retry_policy.rs 已实现，DEFAULT_RETRYABLE_CODES 含 TRANSPORT/TIMEOUT）；D12 重放信封结构逐字（pi-ai v2 blocks 三型签名）+ kind 'fr-openai'/'fr-foreign'（:25 既裁细化）；D13 User-Agent 单一 attribution（getPiUserAgent 被 dsh requestHeaders 覆盖既证）；D14 maxTokens 恒发送+context 钳制链移植（buildBaseOptions + estimate 3.5 chars/token + −4096 余量）。
- 2026-10-26 批 0 真源勘误备案：fr-llm-openai 真源为双层——llm-pi-ai（dsh 壳，4265 行/14 文件）+ pi-ai npm 包 1.1.0（协议本体，vendor 于 ~/project/reference/vendor/earendil-pi-ai-1.1.0，sha256 6caab33c…）+ dsh 补丁 73 行（移除 6 处流中 parseStreamingJson——工具参数仅 toolcall_end 时解析，delta 全程原始串）；fr 移植面约 6000+ 行 TS（壳核心+pi-ai 双协议+utils）；测试夹具资产：llm-pi-ai tests 7429 行 + dsh snapshots/session/pi-ai-*。
- 2026-10-27 切片 6 批 0 裁决（用户 A1/A2 通过 + D15=(a) OAuth stealth 值检测面 1:1 移植（sk-ant-oat 值检测切 Bearer/claude-cli UA/x-app: cli/Claude Code system 首块/工具名 17 词表双向归一/claude-code+oauth 两 beta）+ D16-D24/C1 追认；PRD 决策 45）：A1 采样参数 Then 增 anthropic 臂（透传 temperature/max_tokens、拒绝 stop UNSUPPORTED_OPTION）；A2 toolUpdate 勘误——fr-llm-anthropic 的 in-history 结构性不可达（双 withhold 旗标 + Q57-B 无 catalog），三 provider 一律基线折叠，延迟装载/双占位符/inline-tools beta/tool_addition·tool_removal wire 块不实现（死臂注释载明真源）；D16 pi-ai provider-auth env 链（AUTH_TOKEN/OAUTH_TOKEN/API_KEY/federation 五 env，catalog 路由专属）不移植，鉴权=api-key-env 单路缺省名 ANTHROPIC_API_KEY、无 key 报 'No API key for provider: anthropic' 逐字（D10 同构）；D17 github-copilot 分支不移植；D18 retryProviderRequest=maxRetries 0 单次直通+signal aborted→'Request aborted' 映射（重试归 recovery 层，D11 同构）；D19 X-Stainless 头族对发（19.2 条 23 增 anthropic 臂：X-Stainless-Timeout 恒发秒截断缺省 '600' 与 openai 条件发系 SDK 真源差异；Retry-Count 恒 0）；D20 会话亲和头恒不发（双 withhold+非 openrouter 默认 false），sessionId 无 wire 面；D21 cacheRetention 全语义移植（short 缺省/long→ttl '1h' 条件 supportsLongCacheRetention/none 不发；system+末 tool+末消息末块三点位）；D22 thinking 可达臂全移植（budget 链+adaptive=forceAdaptiveThinking offer+effort 五级 low..max+thinkingBudgets profile 键+off→disabled 条件 thinkingLevelMap.off!==null+interleaved-thinking beta+fine-grained beta 条件默认不发），不可达臂死臂注释（managed-effort/server-side-fallback/inline-tools）；D23 strict tools（supportsStrictTools offer 默认 false+11 关键字+minItems 0/1+format 白名单 10+legacy input_schema 归一）；D24 显式 anthropic-beta 头覆写路径保留；C1 协议无关件复制进 crate（slice 5 先例）+cdylib/rlib 双输出。批 0 真源备案：三层=壳+pi-ai anthropic-messages.ts 1639 行+@anthropic-ai/sdk 0.129.0（vendor sha256 dafba13…，无 dsh 补丁）；betas 为头非体（SDK resources/beta/messages/messages.ts:91/:126）；URL 恒 /v1/messages?beta=true；非 2xx 在 SDK fetch 层抛 APIError（'${status} ${error.message}' 文案族）。

## Boundaries

### Allowed Changes
- crates/fr-llm/**
- crates/fr-llm-deepseek/**
- crates/fr-llm-openai/**
- crates/fr-llm-anthropic/**
- crates/fr-wire/src/stream.rs
- crates/fr-timeout/**
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
  测试: openai_base_url_overrides_endpoint
  假设 配置了自定义 base-url
  当 fr-llm-openai 发起请求时
  那么 请求发往配置端点而非硬编码地址

场景: openai 路由未声明模型报错
  测试: openai_undeclared_model_refuses_without_catalog
  假设 fr-llm-openai 路由未声明 models
  当 请求引用任一模型 id 时
  那么 请求被拒且错误指名未声明模型，不回退任何内置目录

### Rule: provider-anthropic — anthropic 协议面（provider 切片绑定）

场景: anthropic 官方端点
  测试: official_endpoint_pins_path_and_header_order
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
  那么 fr-llm-deepseek 三者按协议字段透传（stop→stop_sequences）；fr-llm-openai 透传 temperature 与 max_tokens（D14 恒发送）并拒绝 stop（UNSUPPORTED_OPTION，openai 族协议无 stop 字段——2026-10-27 裁决 2A 勘误：pi-ai StreamOptions 无 stop 键，原「三者透传」与真源相悖）；fr-llm-anthropic 透传 temperature 与 max_tokens 并拒绝 stop（UNSUPPORTED_OPTION 'fr-llm-anthropic does not support GenerateOptions.stop'——dsh 壳层对全部 pi-ai 协议统一拒绝 adapter.ts:360-362，2026-10-27 A1 裁决增臂）

场景: 错误不中途抛出
  测试: openai_fail_settle_emits_usage_and_error_finish
  假设 上游连接中途失败
  当 适配层结算时
  那么 发 usage 与 finish(kind error) 收尾而非 panic；toolcall_end 参数串与 provider 原始串逐字节一致

场景: toolUpdate 能力差异
  测试: toolupdate_absent_baseline_folding
  假设 同一工具更新分别路由到 anthropic 与 fr-llm-openai 两协议模型（2026-10-26 Q74-B + 2026-10-27 A2 修订）
  当 应用更新时
  那么 三 provider 均无 toolUpdate 能力——anthropic 的 in-history 需双 compat 旗标（ANTHROPIC_COMPAT_GATE withhold，手声明路由不可配）叠加内置 catalog，fr 无 catalog 结构性不可达（19.2 条 20）；fr-llm-openai 维持 Q74-B 无能力判定；不产生中途工具变更历史块，走基线折叠语义（工具全集每请求重发），延迟装载不可用

## Questions

- Source trace: doc/prd.md#13.1: LlmAdapter seam、StreamChunk 七种、usage 先于 finish、prepare_call 代际绑定, doc/prd.md#13.2: 三 provider 表；deepseek 无私有 wire 扩展（19.3）, doc/prd.md#19.3: 仅标准 Anthropic Messages 协议, confidence: high, doc/dsh-source-notes.md: 源码级技术关键点（file:line 证据）, doc/prd.md#13.2（2026-10-25 Q56A/Q57B/Q58 裁决段）+ PRD 决策 37 + 19.2 条 20：三 crate 平切/catalog 缺位/attribution/重放 kind/Files 前缀与索引路径, Q58-1a url 已给定回填（2026-10-25，去 .git 对齐 dsh 主页形态）；Q59-a 已勘误落条款——本块无悬置
- 本切片（fr-llm seam 库）已绑定 19 个真实测试名（覆盖 22 个场景）；provider 协议面 14 场景保持 pending，由后续 provider 切片绑定。
