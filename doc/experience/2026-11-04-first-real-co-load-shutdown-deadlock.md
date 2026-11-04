# 经验：首次全量真共载的关停挂死排查与修复（2026-11-04，Phase 18a）

> 适用：任何「多 cdylib 插件工件首次真装载 + 多线程插件关停」场景；本文以
> fly-rafter 18a 的实证为底，方法与模式可直接复用。

## 1. 事件链（一句话版）

20 个第一方 dylib 工件首次真共载，boot 阶段连环暴露五枚只有真装配才可见的
跨 crate 缺陷（幽灵 schema 约束、描述符声明环、非标准注册方言、工件壳缺
服务带、内核锁序倒置），boot 打通后关停面再暴露「teardown join 系」三枚
通道生命周期缺陷——每一枚的表象都是挂死，根因各不相同。

## 2. 方法论沉淀

### 2.1 单测全绿 ≠ 系统可用

桩驱动的单测绕过了真实边界：wire 方法号漂移、通道生命周期、声明环、
schema 方言差异全部在桩上不可见。**只有真实共载集成测试（真工件、真
内核、真服务闭包）能暴露这一层**。集成测试需要真实 cdylib 工件——
`cargo test` 不产 cdylib（实证），四关门流程必须前置
`cargo build --workspace`，测试对缺失工件 fail-loud 而非静默跳过。

### 2.2 挂死排查的正确顺序（gdb 前置，别猜）

1. **先拿到用户态线程栈再动手**。`ps -T`（线程图 + wchan）只能告诉你
   「哪些线程停在 futex」；**gdb `thread apply all bt` 才能告诉你每条线程
   卡在哪一行业务代码**。本轮前半段在无 gdb 状态下推理浪费了大量轮次，
   gdb 可用后一次附加直接命中（Thread N 停在
   `dougong_dylib…apply…join` → 立刻定位到具体 crate 的 teardown 行号）。
2. 附加对象要选对：`pgrep -f` 会匹配到 bash 包装进程，用
   `readlink /proc/<pid>/exe` 按可执行路径过滤。
3. 挂死复现要区分「进程面」（有看门狗兜底，永远能退）与「库/测试面」
   （无兜底，真挂）——两边都要验，别只看进程退出码。

### 2.3 三类关停挂死的判别签名（gdb 栈对照）

| 栈签名 | 根因类 | 修法 |
| --- | --- | --- |
| runtime 线程停在 `Thread::join`，被 join 的 worker 停在 condvar/recv，且唤醒条件依赖「某个句柄先 drop」 | **通道生命周期自锁**：worker 自持的 Arc 里克隆了 sender，drop 状态侧拷贝永远关不掉通道 | teardown 显式 close（`Option<Sender>` 置 None / close flag），把「唤醒 worker」与「drop 状态」解耦 |
| runtime 线程停在 `Thread::join`，worker 停在同一把实例锁的等待队列 | **disposer/worker 锁序倒置**（内核边界） | 门先翻、锁后取（公平队列自然排水）；见 dougong 0.5.1 `teardown_requirements.md` |
| worker 停在 condvar/nanosleep，但其 close/shutdown 面从未被调用 | **teardown 缺收口调用**（Drop 链不覆盖、或句柄在 state 里被 move 后未触达） | teardown 里 join 之前显式 shutdown/close；insert 后必须 rg 验证落盘 |

### 2.4 通道生命周期铁律

「drop 最后一个 sender → recv 返回」只在 **sender 无共享克隆** 时成立。
只要 sender 被装进 worker 也持有的 Arc，drop-based close 就失效。两条铁律：

- 多线程消费者的通道：**close 必须是显式方法**（`Option<Sender>` 置 None
  / 原子 flag），不能依赖 Drop 链；
- 评审 worker spawn 点时，先画「谁持有 sender 拷贝」清单，再写 join。

### 2.5 async 超时在同步 join 面前不可靠

tokio 的 `timeout(...)` 需要 runtime 线程能被轮询；一个同步
`Thread::join` 卡住 runtime 线程后，**所有 async 侧的兜底（超时/看门狗
task）全部失效**。进程面兜底必须用 OS 线程硬定时器（dsh
`setTimeout(forceExit)` 同形）：`std::thread::sleep` + `process::exit`。
库/进程分层：库面 shutdown 返回码不退出；进程面才持看门狗并退出。

### 2.6 测试环境隔离：插件激活读的是进程 env

插件工件激活期从 **进程环境**烘焙根目录（`FR_HOME` 等）。测试若只把 home
作为函数参数传给装配面、不设 env，插件会解析到**真实用户 home**——
污染真实数据 + 跨测试磁盘冲突（如 session-1 already exists）。测试的
TempHome 必须**单点**设置 env（ edition 2024 下 `set_var` 是 unsafe，
SAFETY 注释写明 runner 串行性前提）。

## 3. 修复清单（对照提交）

- dougong 0.5.1（399a103）：teardown quiescence——门先翻、锁后取、
  require/桥预锁门、桥锁内门补 UB 洞、dispatch 早门；需求规格
  `teardown_requirements.md` 先行，回归钉测双向实证（旧码挂/新码绿）。
- fr-tools-builtin：notice 通道 `Option<Sender>` + `close_tasks()`
  （worker 自持 Arc 的 sender 克隆失效化）；schedule runtime 增加
  shutdown 信号臂，teardown join 前显式触发。
- fr-storage：`job_tx` 改 `Option<Sender>`，close 握手后显式丢弃。
- fr 测试：TempHome 单点设 `FR_HOME`；boot 稳定性 5/5；SIGINT 130 /
  SIGTERM 0；六集成测试全绿（16s，无挂死）。

## 4. 交付纪律红线（本轮用户明令）

- 并发修复禁轮询/睡眠补丁——解法必须从同步原语的**公平性与等待者语义**
  推导（tokio Mutex 公平队列 + 完整门 = 正解；try_lock+sleep 是遮羞布）。
- 跨仓变更按**其文档惯例**走：dougong 是 requirements 规格先行
  （`*-requirements.md`），handoff 是发布前件。
- 每次插入/替换后立即验证落盘（本轮多次 replace 静默未命中，靠 rg 复查
  兜住）。
