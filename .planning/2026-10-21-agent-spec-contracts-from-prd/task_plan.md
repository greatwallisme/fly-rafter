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
| 18d | fr-working-directory 抽移+装配行兑现（47-3A） | complete（四关 63 组 2027 过；lifecycle builtin-tools 357/0/53skip、prompt 47/47；顺带根因修复 fr-llm 代理丢唤醒+rearm 双 setup；待提交） |
|  | （Q3A：permission-presets 竞窗排查随 18e 同期） | |
| 18e | fiber_realm+竞窗排查（47-1A+Q3A） | complete（fiber_realm：lifecycle 33/34、stamp 33/34、四关全绿、commit 29a21b3；Q3A：竞窗排除=on_event 回复协议确定性缺陷，两壳修复+钉测、四关全绿待提交） |
| 18f | M1 小账清零片（header_reason+孤儿批核对+abort 补偿集成场景；Q1A） | complete（dougong 0.6.1 修复发布+词表 declare 接线+两处真实装配缺口根修；e2e 8/8；stamp：headless 33/33、agent-loop 34/34、session 99/108（9 旧欠 skip）、builtin-tools 358/410（52 队列项 skip）、compaction 26/26） |
| 19 | fr-subagent | pending |
| 20 | fr-mcp | pending |
| 21 | fr-ptc | pending |
| 22 | M1 收官片（对拍+批绑+标题治理 Q4A+文档评审+清零核验） | pending |

### Phase 18d — fr-working-directory
**Status:** complete（待提交；详见 progress.md 2026-11-05 18d 完成）

1. 勘察 ✓（findings「18d 开工勘察」：实现体 16f 已全量，纯抽移+跨插件重接线）
2. 合同：builtin-tools/prompt 两 spec 扩边（Allowed += crates/fr-working-directory/**）+47-3A Decision 行 → lint 0.7
3. 实现：新 crate fr-working-directory（core/wire/outbound/plugin_shell/export，18c 判例形；DirectoryLeg 缝走 fs wire 面）+fr-tools-builtin 重接（Seams 增 set、EXECUTE 臂经 seams、删域体/注册/fold_log/describe 165→164、descriptor inject 增名）+fr-schema default-directory rail+manifest 注释+host_boot 清单
4. 测试：9 域测试+context 注册钉迁新 crate（名字不变，spec 选择器不动）；fake Seams ×7 增 set；serve 形/descriptor/注册 spec 钉新增
5. 收口：四关（fmt/clippy/test/doc）→ lifecycle（builtin-tools+prompt）→ explain/stamp/提交


**Status:** complete（lifecycle 29/31 全过 0 败 2 旧欠 skip；四关 receipt：fmt/clippy 0/test 62 组 0 败 EXIT=0/doc ✓；e2e 7/7 serial；stamp 29/31）

1. 勘察 ✓（findings「18c 开工勘察」）
2. 合同 ✓（两 Decision 行+Allowed 扩边+场景重绑/新增；lint Quality 100%）
3. 实现 ✓（fr-llm-retry 全 crate+driver 两段载体+wait_retry_backoff+fr-schema 空 rail；e2e 排障：from_secs_f64 毫秒当秒单位 bug/zstd 日志解码/cdylib 工件陈旧三教训锚 findings）
4. 测试 ✓（fr-llm-retry 11+agent-loop +2+e2e +2 全绿；选择器对齐：spec:99→exhaust e2e 合成场景、spec:105→driver cancel 同名）
5. 收口：四关 ✓（62 组 0 败 EXIT=0;fmt/clippy/doc ✓;host_boot 期望清单随 fr-llm-retry 解析更新）→ lifecycle 跑动中 → explain/stamp/提交

### Phase 18f — M1 小账清零片（Q1A）
**Status:** complete（已提交；dougong daed684+v0.6.1 已 push GitHub 并发 crates.io）

1. 勘察 ✓（19 枚 pending 全对账：headerEquals 三处分裂核实/driver reason 三态/杂 6 归属/approval 9 缺项/两级码组合测试缺/wire 面缺钉测）
2. 合同：session spec 17 枚重绑+扩边 4 crate（Q1A Decision 行）；agent-loop spec 重绑 1+扩 fr-session；builtin-tools 重绑 1；headless spec 新增 abort 补偿集成场景 → lint 0.7
3. 实现：header_equals 族归位 fr-session（canonical_header 邻）+fr-token-meter 删副本改用+driver 两处切换（log_request_header/tools_changed）；新测试 8 枚（fr-session×4：header_equals 钉测/approval 归属/approval 回环/两级码组合+replace 扩断言+reason 扩四词；fr-agent-loop×2：header reason 三态序敏/工具门控；fr-approval×1：审计对逐字键集；fr-tools-builtin×1：ask wire fail-closed）+e2e×1（SIGINT 中途打断+resume interrupted 修复）
4. 测试：全部新测绿+四关全绿
5. 收口：lifecycle（session/agent-loop/builtin-tools/headless[/compaction]）→ explain/stamp/提交

## Next Step
**18f complete 已提交。下一步=Phase 19 fr-subagent（in-process spawn/fork+五工具族+manager；63 绑定面）。**

## Errors Encountered
| Error | Attempt | Resolution |
|-------|---------|------------|
| (历史) Edit/Write 尾部退化高频 | 多次 | 小片段写入+写后即验+python 锚定替换 |
