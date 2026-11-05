# Task Plan: 基于 doc/prd.md 生成 agent-spec 合约 → M1 headless 实现

## Goal
将 doc/prd.md（fly-rafter 唯一需求真源）转化为 agent-spec 合约体系（已完成，1193 场景门禁全绿）；随后按 PRD 第 20 节实现 M1（headless）：约 20 个 crate、六条验收标准，质量门四关全绿。

## Phases（Phase 18 18b d 当前进行中）

### Phase 18b d — runner 集成
**Status:** complete（commit fb10a2e:lifecycle 32/32 全过 0 败 0 跳 quality 1.0;四关 receipt=0;e2e 进程级 5 场景全绿）

1. fr-schema registry 增 fr-llm-deepseek rail 声明（provider 默认 deepseek-official/model 默认 deepseek-flash/reasoning-effort 候选 off-low-high-max/base-url；additional_properties=true）+ 字符串默认读取助手
2. fr 宿主：compile_stage 烘 facts "model" 段（组合后 llm 行 config + 声明默认）；exit_watcher in-process 监听插件（听 headless/exit {code}→通道）；serve_until_shutdown select 信号 vs 退出通道；Cargo += fr-schema
3. fr-headless plugin_shell：descriptor inject [agent,session-persistence,session-query]+listen 四事件；on_event 解码入 RunnerBus；ShellState 扩展；ADOPT_FACTS 臂一次性 spawn runner 线程
4. fr-headless runner.rs：run() 移植（trim 判空/task/stdin、resolveAgent 双检+四拒+live 拒+re-check、whenIdle 事件镜像等待、followup、waitForChildren 抑制循环、flush 追平判定、summarize、exit 映射、fail 双流）；Cargo += uuid
5. 测试：runner 单测（scripted fake host）+壳监听/spawn 钉测
6. mock e2e（crates/fr/tests 进程级：TcpListener SSE+patch base-url+DEEPSEEK_API_KEY+fr bin）
7. 四关全绿 → spec 场景绑定（含新建 uncured_agent_error_fails_the_run）→ lifecycle/guard/explain/stamp 七步收口

### Phase 18 尾-22 切片队列（2026-11-05 裁决定稿：Q1A/Q2B/Q3A/Q4A/Q5 无修订）

| 切片 | 内容 | 状态 |
|---|---|---|
| 18c | fr-llm-retry 新 crate（决策 47-2A）+偿还 provider_retry | complete（lifecycle 29/31：29 过 0 败 2 skip=fiber_realm/header_reason 旧欠；四关 62 组 0 败；e2e 7/7） |
| 18d | fr-working-directory 抽移+装配行兑现（47-3A） | pending |
|  | （Q3A：permission-presets 竞窗排查随 18e 同期） | |
| 18e | fiber_realm+竞窗排查（47-1A+Q3A） | pending |
| 18f | M1 小账清零片（header_reason+孤儿批核对+abort 补偿集成场景；Q1A） | pending |
| 19 | fr-subagent | pending |
| 20 | fr-mcp | pending |
| 21 | fr-ptc | pending |
| 22 | M1 收官片（对拍+批绑+标题治理 Q4A+文档评审+清零核验） | pending |

### Phase 18c — fr-llm-retry
**Status:** complete（lifecycle 29/31 全过 0 败 2 旧欠 skip；四关 receipt：fmt/clippy 0/test 62 组 0 败 EXIT=0/doc ✓；e2e 7/7 serial；stamp 29/31）

1. 勘察 ✓（findings「18c 开工勘察」）
2. 合同 ✓（两 Decision 行+Allowed 扩边+场景重绑/新增；lint Quality 100%）
3. 实现 ✓（fr-llm-retry 全 crate+driver 两段载体+wait_retry_backoff+fr-schema 空 rail；e2e 排障：from_secs_f64 毫秒当秒单位 bug/zstd 日志解码/cdylib 工件陈旧三教训锚 findings）
4. 测试 ✓（fr-llm-retry 11+agent-loop +2+e2e +2 全绿；选择器对齐：spec:99→exhaust e2e 合成场景、spec:105→driver cancel 同名）
5. 收口：四关 ✓（62 组 0 败 EXIT=0;fmt/clippy/doc ✓;host_boot 期望清单随 fr-llm-retry 解析更新）→ lifecycle 跑动中 → explain/stamp/提交

## Next Step
**18c 完成（stamp 29/31、guard 判例记录、四关 62 组 0 败）。下一步=18d fr-working-directory 抽移+装配行兑现。队列：18e（含竞窗排查）→18f→19 subagent→20 mcp→21 ptc→22 收官。**

## Errors Encountered
| Error | Attempt | Resolution |
|-------|---------|------------|
| (历史) Edit/Write 尾部退化高频 | 多次 | 小片段写入+写后即验+python 锚定替换 |
