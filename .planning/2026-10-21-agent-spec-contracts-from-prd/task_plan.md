# Task Plan: 基于 doc/prd.md 生成 agent-spec 合约 → M1 headless 实现

## Goal
将 doc/prd.md（fly-rafter 唯一需求真源）转化为 agent-spec 合约体系（已完成，1193 场景门禁全绿）；随后按 PRD 第 20 节实现 M1（headless）：约 20 个 crate、六条验收标准，质量门四关全绿。

## Phases（Phase 18 18b d 当前进行中）

### Phase 18b d — runner 集成
**Status:** in_progress（实现+门绿,收口=.lifecycle+stamp+commit）

1. fr-schema registry 增 fr-llm-deepseek rail 声明（provider 默认 deepseek-official/model 默认 deepseek-flash/reasoning-effort 候选 off-low-high-max/base-url；additional_properties=true）+ 字符串默认读取助手
2. fr 宿主：compile_stage 烘 facts "model" 段（组合后 llm 行 config + 声明默认）；exit_watcher in-process 监听插件（听 headless/exit {code}→通道）；serve_until_shutdown select 信号 vs 退出通道；Cargo += fr-schema
3. fr-headless plugin_shell：descriptor inject [agent,session-persistence,session-query]+listen 四事件；on_event 解码入 RunnerBus；ShellState 扩展；ADOPT_FACTS 臂一次性 spawn runner 线程
4. fr-headless runner.rs：run() 移植（trim 判空/task/stdin、resolveAgent 双检+四拒+live 拒+re-check、whenIdle 事件镜像等待、followup、waitForChildren 抑制循环、flush 追平判定、summarize、exit 映射、fail 双流）；Cargo += uuid
5. 测试：runner 单测（scripted fake host）+壳监听/spawn 钉测
6. mock e2e（crates/fr/tests 进程级：TcpListener SSE+patch base-url+DEEPSEEK_API_KEY+fr bin）
7. 四关全绿 → spec 场景绑定（含新建 uncured_agent_error_fails_the_run）→ lifecycle/guard/explain/stamp 七步收口

## Next Step
**18b d 实现全绿收口中：四关 receipt=0（fmt/clippy/test 61 组 0 败/doc）；e2e 进程级 5 场景+runner 18 测全绿；spec 25 选择器绑定；两项用户裁决 A/A 落地。lifecycle 后台长跑中（pid 699907,32 场景×逐测 cargo 慢）——完成后 guard(判例跳)+stamp+提交。**

## Errors Encountered
| Error | Attempt | Resolution |
|-------|---------|------------|
| (历史) Edit/Write 尾部退化高频 | 多次 | 小片段写入+写后即验+python 锚定替换 |
