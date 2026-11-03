通知队列被当成了“事实缓存”而不是“信号”

从你给的结构上，至少有这条路径：

- worker/producer 调用 `notify()`，把 “chunk-ready / tool-settled / phase change” 追加到某个共享集合
- driver 的主循环/等待函数读这个集合，发现非空就退出等待
- 但这里并没有保证“读到之后一定会消费对应事实”，而是继续沿用旧状态/旧数据

这意味着：
- 队列里有信号，不代表真实状态已经被消费
- 真实状态的消费和 wakeup 发生在不同路径
- wakeup 条件和业务状态被解耦了

这就造成了一个非常典型的 bug 形式：

- notify A 进入队列
- wait_step 检测到队列非空，立刻返回
- 但 driver 之后没有把 A 真正消费掉，或者 A 只是“被看见了”，而不是“被处理了”
- 下次循环再读到队列还是非空，因而继续返回
- 结果：忙循环，或者无界增长的 notification backlog

这不是“加上限就能解”的问题。因为即使加了上限，队列依然可能持续被非空状态占住，等待逻辑仍然会不断立即返回，除非有真正的 drain 语义。

二、这类代码应该怎么设计，才是正确的

通知在这个系统里应该只表示：
- “某个异步条件已经发生”
- “需要重新检查是否有业务状态更新”

而不是：
- “我必须保留所有历史通知，后面再慢慢处理”

所以正确的设计应当是：

- 共享状态只有一个：当前通知版本号 / generation / last_event_id
- `notify()` 不追加事件对象，而是做：
  - lock
  - pending_generation += 1
  - wake flag = true
  - unlock
- `wait_step()` 不看“队列非空”，而看：
  - 当前 observed_generation < pending_generation
  - 或者实际 data source 有新结果
- 处理完后再更新 observed_generation
- 如果处理没来得及，必须保证后续等待会重新检查，而不是基于 stale queue

伪代码可以是这样：

- shared:
  - mutex
  - pending_epoch: u64
  - observed_epoch: u64
- notify:
  - lock
  - pending_epoch += 1
  - condvar.notify_one()
  - unlock
- wait_step:
  - lock
  - while observed_epoch == pending_epoch and !has_actual_work() {
      condvar.wait(lock)
    }
  - observed_epoch = pending_epoch
  - unlock
  - process_work()

这一版的关键不是“容量”，而是：
- 通知是单点状态，不是消息列表
- 只是一个信号，真正事实来自数据源
- wakeup 不是消费结果，消费结果在驱动状态机里发生

三、为什么“加上限”可能更糟

如果你直接给 `VecDeque` 加上限并丢弃超限元素，问题不是“没治理”，而是“引入了语义丢失”：

- 许多通知不是普通日志，是执行状态变化
- 丢掉它们会导致 driver 认为“不再有更新”
- 但底层数据源其实已经变了
- 最终出现“看起来 idle，其实有实际 work 未处理”的状态错位

尤其对于这类系统，notify 往往仅是“有新成果/新状态需要重新看一眼”的信号，而不是业务本身。丢信号不等于不处理工作；它往往会直接造成更难定位的状态机错乱。

所以结论很明确：

- “限长 + 丢老事件” 不是修复
- “通知改成版本/epoch + 只保留 one bit 的 pending wakeup + 正确 drain” 才是修复

四、status-check race 也必须被标记为高优先级问题

你说的 `maintenance_begin` 和 turn 启动之间的竞争点是成立的，但要更精准地表述：

不只是“维护请求和 driver 没同一把锁”，而是“状态机 transition 不是 atomic 的”。

也就是说，代码上容易出现这种模式：

- 读状态：`status == Idle`
- 进入一个临界区前做判断
- 另一个线程也认为自己有资格推进
- 两者都通过判断，然后都转到“运行”状态或“维护”状态

这类 bug 的后果不是“偶发错误”，而是状态机被 double-advance。关键修法不是“加锁”这么笼统，而是：

- 所有 state transition 必须在同一把 mutex 下做
- 或使用 CAS/compare-and-swap/enum + version
- 也就是说，状态判断和状态更新必须原子

例如：

- 不要写：
  - if state == Idle { state = Running }
- 要写：
  - lock
  - if state == Idle { state = Running; unlock; return true }
  - else { unlock; return false }

这才是能证实的修法。

五、并行工具数为 0 的问题，确实值得保留，但它属于“输入校验”

这个问题值得提，但要精确地表述成：

- `max_parallel_tool_calls` 作为配置值，必须遵守 `>0` 的 invariant
- 如果它被配置成 0，当前调度器会进入“永远不消费任何工具”的状态，进而可能造成：
  - group 空跑
  - no progress
  - wakeup loops
  - watchdog timeout
- 因此，正确修法不是在调度器里临时兜底，而是在构造/解析配置时强制校验

也就是：
- 在 driver 初始化/构造时做 `assert max_parallel_tool_calls > 0`
- 或者在 config parser 把 0 限定为 1
- 同时必须写一个单元测试：`zero_parallel_limit_is_rejected`

不要把这件事说成“可能无穷循环”，要说成“配置契约被破坏，调度逻辑没有保护该契约”。这更清晰，也更符合代码审查的要求。

六、审查建议的最终版

1. 通知语义错误
- 现状：通知被当作缓存队列，而不是“pending wakeup”信号
- 风险：stale notification、busy loop、无界增长
- 修正：用单一 epoch/flag 表示“是否有更新”，不保留所有通知记录

2. wait-step 与 real work 的状态脱钩
- 现状：队列非空 => 立刻返回，但实际 work 未被消费
- 风险：持续重入、重复处理、CPU 100%
- 修正：把“返回等待”和“拉取/处理 work”拆成两步，且处理完后更新观察版本

3. 状态机 transition 非原子
- 现状：检查 + 切换状态分离
- 风险：double advance、维护窗口/turn 并发同进
- 修正：状态判断和状态切换必须在同一把锁或 CAS 中完成

4. 配置不约束
- 现状：`max_parallel_tool_calls` 允许 0/负值
- 风险：调度器出现 no-progress
- 修正：在配置入口强制校验，且测试覆盖

最后一句话总结：

这份代码的关键问题，不在于“是否给队列加上限”，而在于“通知和状态更新、状态判断和状态切换之间，语义与同步边界没有被严格划分”。一旦边界不清，任何看起来像“优化”的改动都可能变成更隐蔽的状态机错误。


# 其他的 3 类更“藏得深”的问题。它们不是“看起来有点怪”，而是会直接导致状态机、工具调度和恢复逻辑出错。

1. generation / phase 边界上的重入问题

最容易忽略的是：生成、工具执行、维护窗口这些阶段都共享同一个驱动循环，但它们的边界常常不是严格串行的。

典型风险是：
- 一个 generation 还没真正收敛，另一个线程已经把 next generation 标记为可启动
- 或者 chunk-ready 还没把前一个结果沉底，工具回调又在同一轮触发状态推进
- 然后 phase 读数和实际输出流不再一致，出现“已经进入下一阶段，但上一阶段 still has pending tool result”的状态

这类问题的本质不是“代码写得差”，而是“状态推进和流数据消费没有共同的版本边界”。

修法要点：
- 所有 phase transition 必须绑定 generation/version
- 任何通知都必须带 generation 或 tool call id
- 只有匹配当前 generation 的事件才允许推进状态
- 旧 generation 的事件必须被丢弃或归并，不得改变新 generation 状态

如果代码里有 “只看队列非空/只看某个 bool” 的判断，这个问题就非常可能存在。

2. tool completion 结果的去重/重复提交问题

工具回调最容易出现“同一个事件被提交两次”的问题，尤其是：
- stream chunk ready 和 tool settle 都可能触发同一个最终状态更新
- 重试/补偿/恢复路径重新投递同一工具结果
- 事件在某个一半的状态下被重放，导致同一个 tool call id 被多次 commit

这种问题通常不会直接崩溃，但会导致：
- 同一 tool call 被多次计数
- 结果被重复写回 session/state
- next group fill 看到错误的 in-flight 数量
- 最后出现“工具看起来执行了两次，但只输出一次”或者反过来

这里最关键的 invariant 是：
- tool call result 只允许一次 commit
- commit key 必须是 call id + generation + attempt number
- 任何重放/恢复事件必须先做 idempotent check

如果没有这个，后续的并行上下文会变得非常脆弱。

3. 恢复/重放路径的幂等性缺失

这点经常在框架型代码里被忽视。你现在这个 loop 很像“有高层状态机 + 外部事件流 + 可能从 checkpoint 恢复”的设计。

如果恢复时：
- 事件重放不做去重
- notifier 里残留的旧唤醒信号没有清理
- pending tool slot 状态没有同步到 checkpoint
那么恢复后可能出现：
- driver 以为某个工具还在 flight，其实已经结束
- 生成结果重复触发
- 维护窗口/active task 被错误恢复
- 用户操作事件被重复执行

这东西的危害比单纯内存泄漏更大，因为它会造成“逻辑错误复现”。尤其是 checkpoint/replay 场景里，状态恢复和 live stream 之间极容易出现分叉。

所以我会把恢复逻辑单独列为一类问题：
- 任何从外部源恢复的状态必须是 idempotent
- notify/pending/wakeup 这类状态必须在恢复时重新归一
- 旧事件也要在恢复时清理，不可直接重放到 live loop 中

4. wait 条件和 wakeup 条件没有区分“有工作”与“已处理”

这个问题我觉得很重要，而且经常被误判为“队列上限问题”。

很多代码会写成：
- there are notifications => return
- loop again until no notifications
但真正应该是：
- there are notifications OR underlying data indicates new state => evaluate state machine

也就是说，`wait_step()` 不能把“唤醒”当成“事实已处理”。它只是触发重新评估，不是业务完成信号。

如果这里没有区分两者，就会出现：
- 反复唤醒
- 反复重入
- 只处理了信号，没有处理数据
- task 被唤醒但不推进状态

这也是“为什么加上限不够”背后的本质：问题不在于 queue too large，而在于信号和状态没有独立语义。

5. 取消语义和超时语义不够清晰

我还会看一眼 cancel / timeout / stop 这几条路径。因为这类循环系统如果 cancel 语义不完整，最容易出现“信号已经发出，但宿主层还在继续推进”的状态错位。

常见问题：
- cancel is set but still processing old generation
- timeout wakes loop but does not flush pending tool callbacks
- stop state is set before pending notifications are drained
- final event arrives after stop and mutates state anyway

这类问题产生的症状通常是“偶发卡死”“结果不一致”“最后一个工具结果被吞了”。

结论

如果把前面 4 个问题压成一句话：

- “通知被当成事实”“状态判定和状态切换不原子”“tool result 无幂等性”“恢复/重放和 live 事件没有统一版本边界”