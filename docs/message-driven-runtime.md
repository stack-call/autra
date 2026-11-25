# 消息驱动：状态推进与 Effect 执行的解耦

> **状态**：设计提案。当前实现是“持久化边界 + 协程内调用”，不是完整的消息驱动。
> **基线**：`mvp-v1`，包含 Coordinator 拆分、GlobalScheduler、RecoverySupervisor、
> EngineState snapshot + patch。
> **相关**：[Runtime vNext](runtime-vnext.md)、[运行时演进](runtime-evolution.md)、
> [执行模型](execution-model.md)、[持久化](durability.md)、
> [本地运行时稳定性](local-runtime-stability.md)。

## 1. 结论

消息驱动的价值不在于“增加线程”或“提高并发”，而在于改变**依赖关系**：

> Run 的状态推进不再依赖 Effect 的执行结果通过函数调用返回。

当前 Autra 已经有一半基础：Effect 有身份、有状态、有 outbox、有 claim、有 lease、
有结果记录。缺的是最后一步——**Coordinator 不再等待 Executor**。

目标形态：

```text
Event Inbox -> Stepper -> Effect Outbox
      ^                        |
      |                        v
      +---- Executor Pool <----+
```

- Coordinator 只消费事件、推进状态、提交事实、产出 Effect。
- Executor 只领取 Effect、执行、把结果变成事件。
- 两者之间是持久化消息，不是函数返回值。
- Run 成为**逻辑状态机**，协程只是驱动它的一种资源。

## 2. 当前实现的耦合点

### 2.1 已经解耦的部分

| 能力 | 现状 |
|---|---|
| Effect 身份 | `EffectId`、`AttemptId`、claim id、fencing token |
| Effect 生命周期 | `Recorded → AwaitingApproval → Authorized → Dispatched → Committed/Failed/OutcomeUnknown` |
| 持久化工作队列 | `effect_outbox` + claim + lease + 续租 |
| 结果持久化 | `record_claimed` 校验 claim 后写入 Effect 与 Attempt |
| 恢复 | `resume`、`reconcile`、`RecoverySupervisor` |
| 资源准入 | `GlobalScheduler` 的 Model / Tool / Process 池 |

也就是说：**Effect 作为“消息”已经存在，而且比普通内存 channel 更强**——它可持久化、
可去重、可对账。

### 2.2 仍然耦合的部分

在 `engine/coordinator/mod.rs` 的 `drive` 里，执行仍然是内联的：

```rust
let batch = EffectExecutor::new(self)
    .execute_tool_batch(...)
    .await?;
for outcome in batch.outcomes {
    results.tool_result(outcome);
}

match EffectExecutor::new(self)
    .execute_model(&effect_id, &request, &mut context)
    .await
{
    Ok(ModelStep::Output(output)) => results.model_result(output),
    ...
}
```

即使是 journaled 路径，也是 Coordinator 自己 claim、自己 dispatch、自己 await：

```text
commit_transition -> outbox 行
drive -> OutboxDispatcher::run_once_for_run_with(...).await
drive -> sync_effect -> 入队 ModelResult/ToolResult
```

所以现在的结构是：

```text
Run 协程
  ├── 状态推进（串行）
  ├── Effect 领取（串行）
  ├── Effect 执行（可并行，但仍在同一个调用栈上等待）
  └── 结果回填（串行）
```

**控制流耦合**：Coordinator 必须知道 Executor 怎么调用、什么时候返回、如何把返回值变成事件。

这不影响正确性，但限制了四件事：

1. 新事件（用户中途指令）无法在 Effect 执行期间进入同一个事件入口。
2. Executor 无法独立部署、独立扩容、独立崩溃。
3. Run 无法卸载协程后等待外部事件。
4. 多个 Run 的执行无法由统一的执行侧调度，只能由各自的协程发起。

## 3. 目标模型

### 3.1 两种独立执行单元

```text
Coordinator（每个 Run 一个，逻辑 Actor）
  输入：AcceptedEvent
  输出：EffectIntent（已提交）+ 状态变更

Executor（全局池，按资源分池）
  输入：已授权、已领取的 Effect
  输出：EffectOutcome（持久化后成为事件）
```

### 3.2 消息契约

| 消息 | 方向 | 持久化 | 说明 |
|---|---|---|---|
| `AcceptedEvent` | 外部 → Coordinator | 是（`run_events`） | 用户输入、模型结果、工具结果、取消 |
| `EffectIntent` | Coordinator → Executor | 是（`effects` + `effect_outbox`） | 已授权、待执行 |
| `EffectOutcome` | Executor → Coordinator | 是（`effects` + Attempt） | 执行事实，再转成事件 |

三者都已有承载结构，不需要新的运行时类型。

### 3.3 Coordinator 循环（目标）

```text
loop {
    event = inbox.next()                 // 持久化事件或内存事件
    proposal = stepper.transition(state, event)
    validate(proposal)
    commit(state, event, effects, engine_delta)
    apply(proposal.next)
    publish(proposal.intents)            // 写入 outbox，不执行
    if disposition != Continue { return }
}
```

关键差异：**没有 `await execute()`**。

### 3.4 Executor 循环（目标）

```text
loop {
    claim = scheduler.reserve(resource)   // 先拿资源许可
    entry = journal.claim_one(claim)      // 再领取少量 Effect
    outcome = execute(entry)              // 带续租
    journal.record_claimed(outcome)
    notify(run_id)                        // 唤醒 Coordinator
}
```

注意顺序：**先许可后领取**。否则 Effect 会在内存队列里等待，租约可能先过期。

### 3.5 事件入口

Coordinator 需要一个统一入口，至少支持：

- 执行结果（模型/工具）
- 外部取消请求
- 用户中途输入
- 审批决定
- 恢复唤醒

这个入口在单进程内可以是一个按 `run_id` 分区的队列；在持久化模式下，它应该由
`run_events` + 一个“未消费事件”标记来实现，而不是纯内存 channel。

## 4. 四个场景的实际收益

### 4.1 Agent 运行中收到新指令

当前：Run 协程在 `await` 模型调用，外部输入只能靠额外机制（cancel flag、select）插入。

消息驱动：Coordinator 的事件入口天然统一，新指令可以作为事件排队，按状态机规则
处理（例如在 `AwaitingModel` 时标记为“下一轮输入”，在 `AwaitingTool` 时排队）。

**收益不是“更快”，而是“规则集中”**：所有输入走同一个入口，同一条状态机路径。

### 4.2 多 Run 共享 Executor

当前：每个 Run 自己发起执行，GlobalScheduler 只做准入。

消息驱动：Executor 池从 outbox 领取工作，所有 Run 共用同一执行侧。

这让以下能力变得自然：

- 全局优先级（不只是并发上限）
- 按租户/会话的配额
- 负载均衡
- Executor 独立重启

### 4.3 Run 休眠

这是长生命周期 Agent 最关键的能力。

当前：Run 与一个持续存在的 future 绑定；暂停只能靠持久化 + 进程退出 + `resume`。

消息驱动：

```text
提交审批请求
  -> 持久化状态
  -> 卸载 Coordinator
  -> 审批事件到达
  -> 重新创建 Coordinator
  -> 从 Journal 重建状态继续
```

**Run 不再必须与协程同生命周期。** 审批等待、外部回调、长延迟任务都可以这样处理。

前提是：事件队列必须持久化，且有“事件已被消费”的记录，否则唤醒会重复或丢失。

### 4.4 分布式演进

```text
机器 A：Coordinator
          |
    Durable Queue
          |
机器 B：Model Executor
机器 C：Tool Executor
机器 D：Sandbox Executor
```

Coordinator 的状态转换逻辑基本不变，替换的是消息传输与执行基础设施。
但分布式仍然需要：租约、fencing、去重、幂等、对账、故障恢复——这些不会因为
“改成消息”而自动获得。

## 5. 线程与协程模型对比

| 对象 | 当前 | 消息驱动后 |
|---|---|---|
| OS 线程 | Tokio worker | 不变 |
| Run | 调用方 future / `tokio::spawn` | 逻辑 Actor，可卸载 |
| Coordinator | Run 协程内部 | 独立 Task |
| Executor | Run 内联调用 | 独立 Worker Task / 池 |
| 通信 | 函数调用 + `await` | 持久化消息 |
| 状态修改 | 串行 | 仍然串行（不变量） |
| Run 生命周期 | 与调用方绑定 | 可独立管理 |
| 崩溃恢复 | 依赖 Journal + resume | 同样依赖，但唤醒是事件 |

## 6. 必须保持的不变量

改造不能破坏这些：

1. **一个 Run 同时只有一个有效推进者**（Run lease + fencing）。
2. **状态提交先于执行**，执行结果先持久化再消费。
3. **非幂等 Effect 未知时不自动重试**。
4. **取消由显式请求决定**，不靠进程消失推断。
5. **Effect 只能被执行一次**（同一 attempt 内），重试产生新 Attempt。
6. **事件顺序**：同一 Run 的 accepted event 序号严格递增。
7. **审批等待不占用执行槽**。

## 7. 关键设计问题

### 7.1 事件去重与顺序

执行结果可能重复投递（重试、恢复、网络重放）。需要：

- 稳定的 `effect_id + attempt_id` 作为结果身份
- `run_events` 的唯一约束（已有 `(run_id, seq)`）
- 结果 → 事件的映射必须幂等：同一 attempt 只产生一个事件

当前 `ResultCollector` 只在内存里做取消去重，跨恢复去重还没实现。

### 7.2 唤醒机制

Coordinator 卸载后，需要被“有新事件”唤醒。两种实现：

| 方案 | 说明 | 代价 |
|---|---|---|
| 进程内唤醒 | 内存通知 + 持久化事件表 | 单进程有效 |
| 轮询 | 定期扫描未消费事件 | 简单，但有延迟 |
| 通知服务 | 数据库 notify / 队列 | 需要额外基础设施 |

单机阶段建议：**持久化事件表 + 进程内通知**，轮询作为兜底。

### 7.3 背压

消息驱动容易掩盖背压问题：

- 事件队列无限增长
- outbox 无限增长
- Executor 领取过快导致租约堆积

需要：每个 Run 的待处理事件上限、全局 outbox 上限、Executor 领取批量上限。

### 7.4 取消

取消要成为一等事件：

```text
CancelRequested（持久化）
  -> Coordinator 消费
  -> 标记未派发 Effect
  -> 等待已派发 Effect 的结果或超时
  -> Cancelled
```

当前已有持久化 cancel request 和 lease 轮询，但还没有“事件化”的取消路径。

### 7.5 审批

审批是外部事件，不应该占用执行槽。消息驱动下它天然是：

```text
Effect -> AwaitingApproval -> 持久化
      -> 审批事件 -> Authorized -> outbox
```

### 7.6 分布式额外成本

消息驱动不等于分布式正确。多进程/多机仍然需要：

- Worker identity 与 fencing
- 幂等键与结果查询
- 重复消息处理
- 租约与心跳
- 故障注入测试

## 8. 迁移路径

不推翻现有架构，分四步。

### 第一步：抽出 Executor 循环（单进程）

- 新增 `ExecutorPool`：按资源池领取 outbox 中的 Effect
- Coordinator 不再调用 `EffectExecutor`，只提交 outbox
- 结果写入后，通过事件入口唤醒 Coordinator
- 保留现有直接路径作为 fallback

验收：现有全部测试通过；`drive` 中不再有 `execute_tool_batch` / `execute_model` 调用。

### 第二步：统一事件入口

- 新增 `RunInbox`：按 `run_id` 分区
- 所有事件（结果、取消、审批、用户输入）走同一入口
- 取消从 flag 轮询改为事件

验收：取消、审批、结果在同一入口下行为一致；新增“运行中用户输入”测试。

### 第三步：Run 可卸载

- Coordinator 在 `AwaitingApproval` / 长等待时释放协程
- 事件到达时重建 Coordinator，从 Journal 恢复
- 增加“事件已消费”标记，避免重复唤醒

验收：审批等待期间进程重启，事件到达后正确继续；无重复事件。

### 第四步：多进程

- Executor 池跨进程
- Worker identity、fencing、租约
- 故障注入矩阵

验收：多个 Executor 竞争同一 Effect 不破坏状态；过期 Worker 不能提交。

## 9. 什么时候不该做

- 只做普通 ReAct Agent、单机、短任务：当前架构已经足够。
- 没有持久化事件与唤醒机制前，强行“消息驱动”会退化成内存队列，失去恢复能力。
- 没有故障注入前，多进程 Executor 会放大重复执行风险。

## 10. 测试与验收

1. **行为等价**：同一批脚本化模型响应，消息驱动与当前路径产生相同的 `RunOutcome` 与 Effect 记录。
2. **事件去重**：重复投递同一 attempt 的结果只产生一个事件。
3. **顺序**：并行工具结果乱序到达，accepted history 仍按调用顺序。
4. **唤醒**：卸载后事件到达能恢复；无事件时不重复唤醒。
5. **取消**：取消事件在 `AwaitingModel` / `AwaitingTool` / `AwaitingApproval` 下行为正确。
6. **背压**：事件队列与 outbox 达到上限时给出明确错误，而不是无限增长。
7. **多 Executor**：两个 Executor 竞争同一 Effect，只有一个执行，另一个得到 `AlreadyResolved`。
8. **崩溃**：在提交后、执行前、执行后分别崩溃，恢复语义正确。

## 11. ADR 待办

1. Coordinator 与 Executor 的消息契约与持久化边界。
2. 事件入口、去重与唤醒协议。
3. Run 卸载与重建的生命周期。
4. Executor 池的领取顺序、背压与公平性。
5. 取消事件化与 `CancelRequested` / `Cancelled` 的语义。

## 12. 一句话

当前 Autra 已经把 Effect 做成了持久化消息，但 Coordinator 仍然在同一个调用栈里等待执行。
消息驱动的下一步不是“加线程”，而是**让 Coordinator 只消费事件、只提交事实，把执行
交给独立的 Executor 循环**，从而让 Run 成为逻辑状态机，而不是一个必须持续存在的协程。
