# Autra Runtime vNext：模块化单体架构提案

> **状态**：单机模块化骨架已实现（见“实现进展”）；多进程、PostgreSQL、
> Redis 和完整 Attempt/Fencing 协议仍是设计提案。
> **基线**：`mvp-v1`，包含本地稳定性 P0（Run/Service lease、claim fencing、进程取消、原子写）。
> **范围**：Local-first、单机优先、预留多进程与多机扩展。
>
> **实现进展**：单机模块化骨架已落地。
> `engine/coordinator/` 负责 Run 推进、TransitionCommitter、AuthorizationGate
> 和 ResultCollector；`scheduler/` 提供 Model/Tool/Process 全局 FIFO Semaphore（加权公平与有界等待队列仍后续）；
> `engine/recovery/supervisor.rs` 提供带服务租约的 `RecoverySupervisor`；
> `EffectExecutor` 保留。跨进程 Worker、PostgreSQL、Redis、AttemptId/Fencing
> 和 SQLite 读写分离仍属于后续阶段。
> **相关**：[运行时演进](runtime-evolution.md)、[简化架构 RFC](autra-runtime-simplified-rfc.md)、
> [本地运行时稳定性](local-runtime-stability.md)、[执行模型](execution-model.md)、
> [持久化](durability.md)、ADR-0058。

## 1. 结论

Autra 不需要一次性变成分布式平台。推荐目标是**模块化单体（Modular Monolith）**：

- 逻辑上把 Run 状态推进、Effect 执行、资源调度、持久化和故障恢复五种职责分开；
- 物理上仍然是一个 Rust 进程 + SQLite；
- 每个职责有独立的不变量和测试边界；
- 后端（SQLite/PostgreSQL、本地/远程 Worker、Outbox/消息队列）可替换；
- `Run`、`Event`、`Effect`、`Attempt`、`Authorization`、`Recovery` 的语义不随部署形态改变。

目标组件：

```text
Agent API / Session / CLI / TUI
              |
        RunCoordinator
   Stepper -> TransitionCommitter -> AuthorizationGate
              |                         |
        ResultCollector            Outbox
              |
     Durable Journal (SQLite / future PostgreSQL)
              |
   GlobalScheduler + WorkDispatcher
        |                   |
  Model Executor     Tool / Process Executor
              |
     EffectOutcome -> Journal -> RunCoordinator

RecoverySupervisor（旁路）
RunHub / Metrics / Tracing（旁路）
```

**最重要的取舍：先成为可靠的模块化单体，再成为分布式平台。**

## 2. 当前基线

**vNext 单机骨架状态**

| 组件 | 状态 |
|---|---|
| RunCoordinator | 已实现：`engine/coordinator/mod.rs` 的 run/drive |
| TransitionCommitter | 已实现：`coordinator/transition.rs` |
| AuthorizationGate | 已实现：`coordinator/authorization.rs` |
| ResultCollector | 已实现：`coordinator/result.rs` 的事件队列 |
| GlobalScheduler | 已实现本地版本：`scheduler/mod.rs`，Model/Tool/Process Semaphore |
| EffectExecutor | 已实现：`engine/executor.rs` |
| RecoverySupervisor | 已实现：`engine/recovery/supervisor.rs`，服务租约 + 周期扫描 |
| Journal/Outbox | 已有，SQLite 单 Writer；读写分离与 GC 未实现 |
| 多进程 Worker / PostgreSQL / Redis | 未实现，属于阶段 3/4 |

已经在本地单机模式中具备、并且应当保留的：

| 能力 | 当前实现 |
|---|---|
| 纯状态转换 | `autra-core` 的 `AgentState`、`Event`、`Effect`、`AgentStepper` |
| ReAct 行为 | `autra-harness` 的 `ReActStepper`，历史存在 `engine_state` |
| 原子提交 | `commit_transition`：state + event + effects + Authorized outbox 同事务，`state_version` CAS |
| 终态提交 | `commit_terminal`，带版本 CAS |
| Effect 账本 | `EffectRecord`、`EffectAttempt`、状态机、结果与错误 |
| 执行边界 | `EffectExecutor`、`Dispatcher`、`OutboxDispatcher` |
| 全局调度 | `GlobalScheduler` 的 Model/Tool/Process FIFO Semaphore |
| 审批 | Policy、ApprovalRequest/Record、digest、revision、TTL、持久化复用 |
| 恢复 | `resume`、`reconcile`、`outbox`、`RecoverySupervisor`、`autractl daemon` |
| 观察 | `RunHub`、streaming、`ObserverHook`、tracing |
| 本地稳定性 P0 | Run/Service lease、claim 续租与 fencing、活跃 owner 跳过、daemon 单实例、Agent profile 校验、命令树取消、原子写 |
| 约束 | `autra-testkit` 的 crate 依赖方向测试 |

仍然缺的、本文要解决的：

| 缺口 | 影响 |
|---|---|
| Scheduler 只有 FIFO Semaphore | 有界等待队列、429 退避和加权公平尚未实现 |
| `Attempt` 已记录，但没有独立的 `AttemptId` / `fencing_token` 协议 | 多进程接管时难以证明“谁有权提交” |
| 取消意图没有独立的持久化记录 | 崩溃恢复只能看到未完成 Run，不能可靠区分用户取消与执行者失联 |
| ResultCollector 目前只负责事件队列 | 跨恢复的去重、结果重放和全局排序尚未实现 |
| SQLite 是单个 `Arc<Mutex<Connection>>` | 读写路径未分离，长事务会阻塞其他 Run |
| 没有崩溃/租约过期/fsync 失败的故障注入矩阵 | 无法证明恢复不会重复外部副作用 |
| 没有 PostgreSQL / 远程 Worker / 消息队列 | 多机扩展缺少后端契约 |
| 没有 Sandbox | 不可信命令仍以当前用户权限执行 |

## 3. 六条设计原则

| 原则 | 解决的问题 |
|---|---|
| 一个 Run 同时只有一个有效状态推进者 | 防止并发事件互相覆盖 |
| 多个 Run 可以并发 | 提高系统吞吐量，不让任务互相等待 |
| 同一个 Run 可以并行执行独立 Effect | 提高模型和工具调用效率 |
| Journal 是执行事实的权威来源 | 崩溃后知道系统已经接受了什么 |
| Scheduler 不改变 Agent 决策 | 防止资源调度逻辑侵入状态机 |
| Worker 可替换，但 Run 身份不变 | 允许进程重启和故障接管 |

第三条需要强调：**Run 串行推进状态，不等于所有工具必须串行执行。**
模型提出读取 A、B、C，三个读取可以并发；但把结果变成合法事件、推进 Run 状态，仍由 Coordinator 串行完成。

## 4. 目标组件与职责

### 4.1 RunCoordinator

**职责**：一个 Run 的唯一有效逻辑推进者。

它只回答：

> 对于这个 Run，已经接受了什么事件？下一步状态是什么？什么时候可以继续？

它接收 Event，调用 Stepper，产生 Proposal，交给 TransitionCommitter，提交成功后再安排后续工作。
它不直接持有数据库连接、不发起 HTTP、不执行 Shell、不等待工具完成。

**为什么从 `LocalEngine` 中抽出**：现在 `drive()` 同时做事件循环、状态转换、提交、执行调度、取消、Streaming 和完成处理。继续往这里加全局限流、多 Worker 和恢复策略，会让正确性难以验证。

**非目标**：Coordinator 不决定全局资源分配，也不决定哪个 Worker 执行 Effect。

### 4.2 TransitionCommitter

**职责**：把 Stepper 的 Proposal 变成持久事实。

```text
Stepper -> Proposal
        -> TransitionCommitter
        -> validate old version / identity / budget / intent
        -> atomic Journal commit
        -> Apply new in-memory state
```

固定规则：**Prepare → Commit → Apply**。

- Prepare：计算新状态和要产生的 Effect。
- Commit：持久化状态、事件、Effect、Outbox。
- Apply：提交成功后才更新内存中的权威状态。

**为什么单独抽**：状态正确性最关键的是提交边界。数据库失败时，不能出现“内存认为成功、Journal 没有保存”。现有 `commit_transition` 已经具备原子性，应复用而不是重写。

### 4.3 AuthorizationGate

**职责**：所有可能产生外部副作用的 Effect 必须经过可信授权检查。

```text
Effect -> Policy: Deny / Ask / Allow
       -> Ask: AwaitingApproval
       -> persisted decision + digest / revision / TTL
       -> Authorized -> Outbox
```

**为什么不能放在 Worker**：Worker 是执行者，不应拥有修改授权规则的能力。远程 Worker 收到一条队列消息，不等于取得执行权限。
执行前仍要检查操作身份、授权版本、Effect 状态和当前可执行性。

**等待审批不占用执行槽**：`AwaitingApproval` 是外部事件等待，不是 Worker 忙碌状态。

### 4.4 GlobalScheduler + WorkDispatcher

**职责**：决定“现在运行什么”，但不修改 AgentState。

至少区分三类资源：

| 资源池 | 控制内容 |
|---|---|
| Model | 模型并发、供应商配额、429 退避 |
| Tool | 普通工具并发 |
| Process | Shell 子进程、CPU/内存较重操作 |

Scheduler 回答：

> 当前有多少可用执行资源？哪个 Effect 应该先执行？

它还需要跨 Run 公平性，避免一个产生大量 Effect 的 Run 长期占满名额。

**初期实现**：Tokio `Semaphore` + 有界队列 + 简单的加权公平/轮转即可，不需要 Redis。
当前已落地 FIFO `Semaphore`；等待队列上限和加权公平仍未实现。

**关键细节**：不要先领取大量 Effect 再让它们在内存队列里等待。应先获得资源许可，再领取少量即将执行的 Effect；执行中若可能超过租约，必须续租。

**为什么不能只靠 `max_parallel_tool_calls`**：那是单个 Run 的局部限制。100 个 Run 各允许 5 个工具，合计可能产生 500 个并行调用。

### 4.5 EffectExecutor

**职责**：执行已经授权、已经取得执行权的 Effect。

```text
ExecutionRequest {
    effect_id,
    attempt_id,
    execution context,
    timeout,
    cancellation,
    lease / fencing information,
}
```

通过 Dispatcher 路由到：

```text
CallModel  -> ModelProvider
CallTool   -> ToolRegistry
run_command -> 受控进程 / Sandbox Adapter
```

**为什么 Executor 不直接修改 Run 状态**：并行工具完成顺序不同。如果每个工具都能直接改 RunState，状态竞争会变得复杂。
执行者只报告结果，由 Coordinator 决定如何接受。

### 4.6 ResultCollector

**职责**：收集并行结果，稳定事件身份，去重、排序并决定何时可以推进。

需要区分三件事：

1. 实时完成事件（B → C → A 的进度顺序）；
2. 已持久化的执行结果；
3. 反馈给模型的最终顺序（必须与调用 ID 对应，按 ReAct 约定组织）。

**为什么单独抽**：未来远程 Worker 的结果可能延迟、重复或在恢复期间重新出现。去重和排序不应分散在 Dispatcher、Stepper 和 Worker 中。

### 4.7 Durable Journal + Outbox

**职责**：唯一权威执行事实来源。

保留现有原子协议：

```text
commit_transition:
  - 校验 state_version
  - 保存新 State
  - 追加 Event
  - 保存 EffectRecord
  - 写入 Authorized Outbox
```

不要为了并发优化把这四类写入拆到不同存储系统。

逻辑数据模型：

| 数据 | 作用 |
|---|---|
| RunRecord | Run 当前状态、版本、归属 |
| EventRecord | 已接受事件与序号，审计、恢复、重复检测 |
| EffectRecord | 一个逻辑操作 |
| EffectAttempt | 一次物理执行尝试 |
| OutboxRecord | 等待派发的已授权工作 |
| RunLease / WorkerLease | 执行者、租约、fencing 版本 |
| ApprovalRecord | 批准内容、身份、有效期 |
| CancelRequest | 显式取消意图与原因 |

**Effect 与 Attempt 必须分开**：

```text
Effect E100
  ├── Attempt 1: OutcomeUnknown
  └── Attempt 2: 对账确认安全后重试
```

重试不应生成一个新的、不相关的 Effect。

**当前进展**：Run/Service lease、claim 续租与 claim-id fencing 已实现；还缺独立 `AttemptId`、`fencing_token`、持久化 `CancelRequest`，以及 journal retention/GC。

### 4.8 RecoverySupervisor

**职责**：旁路扫描未完成 Run，复用 Coordinator 和 Journal 协议，不实现第二套 Run Loop。

判断顺序：

```text
未完成 Run
  -> 终态 / 取消请求 / 执行权
  -> 终态或已取消：不重新启动正常执行
  -> 检查 Lease 与 Effect 状态
  -> 可安全继续：重新派发或接受已有结果
  -> OutcomeUnknown：先对账，必要时人工处理
```

**取消必须由显式命令决定，不能靠进程消失来猜测。**
需要区分 `CancelRequested` 和最终 `Cancelled`：执行中的工具可能无法立即停止，外部操作也可能已经发生。

### 4.9 RunHub / Metrics / Tracing

旁路观察，不参与决策。提供进度订阅、资源消耗、Effect 结果分布、审批等待、租约回收等指标。

## 5. 一次任务如何运行

示例：读取三个文件，定位登录 Bug，修改代码，运行测试。

### A. 创建 Run

```text
Session S1 -> Turn T1 -> Run R100 -> RunStarted
```

Session 是持续对话，Turn 是一轮交互，Run 是一次独立执行。
恢复 R100 时不应创建 R101。

### B. 状态推进与持久提交

```text
Coordinator
  -> Stepper.transition(RunStarted)
  -> Proposal { AwaitingModel, [CallModel] }
  -> TransitionCommitter
  -> Journal 原子提交
```

提交成功后，模型 Effect 才是系统接受的执行意图。

### C. 调度模型请求

```text
Authorized Effect
  -> Scheduler 获取 Model permit
  -> Dispatcher 按协议领取 Effect
  -> Executor 调用模型
```

不要先领取大量 Effect 再排队；租约可能在真正执行前过期。

### D. 模型提出并行工具

```text
AwaitingTool
  ├── Read A
  ├── Read B
  └── Read C
        |
   ResultCollector
        |
   ToolResult（按调用 ID 对应）
        |
   Stepper
```

最后一个需要的工具结果被接受后，才推进下一次模型调用。

### E. 需要审批

```text
edit_file -> AuthorizationGate -> AwaitingApproval
```

Effect 不进入可执行 Outbox。用户批准后，重新校验请求内容、digest、revision 和 TTL，再变为 `Authorized`。
等待审批不占用 Worker 执行槽。

### F. 完成与两个持久化边界

```text
EffectOutcome -> Journal 保存结果
             -> AcceptedEvent
             -> Stepper
             -> RunCompleted
             -> Journal Commit
             -> RunHub 通知
```

必须区分：

1. Effect 结果已经保存；
2. Run 状态机已经消费该结果并推进。

如果恰好在两者之间崩溃，恢复时必须能找到尚未消费的结果，并且只能消费一次。
需要稳定的 `effect_id / attempt_id / event_id`、唯一约束和状态版本校验。

## 6. Journal 与 SQLite

### 6.1 保持原子事务

不要为了并发把 State、Event、Effect、Outbox 拆到不同存储。现有事务协议是可靠性的基础。

### 6.2 SQLite 连接模型

目标形态：

```text
Journal API
  ├── Read Path  -> Reader Pool
  └── Write Path -> Single Writer -> SQLite WAL
```

设计原因：

- WAL 允许读写重叠；
- 单 Writer 减少应用内部写竞争；
- 短事务降低锁持有时间；
- 有界写队列提供背压；
- 同步 SQLite 操作放到阻塞线程，避免卡住 Tokio worker。

但**先压测再改造连接模型**。如果瓶颈实际是“每次领取 Effect 都加载整个 Run 的历史”，先优化 SQL 往往比新增 Reader Pool 更有效。

单 Writer 只是单进程优化；多进程访问同一数据库仍需要跨进程事务和执行权协议。

### 6.3 结果接受协议

Effect 结果持久化与 Run 状态消费是两个边界。建议最终形态：

```text
EffectAttempt -> Outcome stored (effect_id + attempt_id)
             -> ResultAccepted event (stable event_id)
             -> Coordinator consumes exactly once
```

当前已有 `commit_transition` 和 event seq 唯一约束，可以在此基础上扩展，不需要重写。

## 7. 多 Worker 路线

### 阶段 1：多个 Run 并发，Executor 在同一进程

```text
Coordinator R1/R2/R3
        |
  GlobalScheduler
        |
 Executor 1/2/3
        |
   SQLite Journal
```

所有组件在一个 Tokio Runtime 内。先验证“状态与执行分离”是否正确，而不是网络分发。

### 阶段 2：多个进程参与执行

```text
Coordinator
  -> Durable Journal
  -> Work Dispatcher
  -> Worker A/B/C
```

需要正式协议：

```rust
struct ExecutionLease {
    effect_id: EffectId,
    attempt_id: AttemptId,
    worker_id: WorkerId,
    fencing_token: u64,
    expires_at: Timestamp,
}
```

执行步骤：

```text
Worker 领取 Effect
  -> Journal 校验可执行状态
  -> 原子产生 Attempt + Lease
  -> Worker 执行外部操作
  -> 携带 Attempt ID 和 Fencing Token 提交结果
  -> Journal 校验后持久化
```

**为什么不能只依赖数据库 Mutex**：数据库保护不了外部系统。Worker A 失去租约后仍可能调用外部 API，而 Worker B 已经接管。
Fencing 能阻止过期 Worker 提交当前数据库状态，但不能撤销已经发出的请求。因此还需要幂等键、结果查询、Sandbox 或 Reconcile。

### 阶段 3：多机与 PostgreSQL

- PostgreSQL 作为共享 Journal 后端；
- 远程 Worker 协议；
- Workspace/Sandbox 隔离；
- 只有在分发压力确实需要时，才加入 Redis Streams / RabbitMQ / NATS。

## 8. 为什么不现在加入 Redis

当前链路：

```text
Run 状态提交 -> Effect 写入 Outbox -> 本地 Dispatcher 领取 -> 执行
```

已经有持久化基础。加入 Redis 后多出：

- Outbox Relay；
- 发布重试；
- 重复消息去重；
- 消费者 ACK；
- Redis 故障恢复；
- 消息保留与清理。

它提高通知、分发和远程接入能力，但不会消除 SQLite 写事务限制。
**消息队列是可替换的工作通知机制，不应成为 Effect 状态的唯一权威来源。**

| 阶段 | 存储 | 分发 |
|---|---|---|
| 当前单机 | SQLite | Outbox + 本地 Dispatcher |
| 单机高并发 | 优化后的 SQLite | GlobalScheduler + 本地 Executor |
| 多机初期 | PostgreSQL | 数据库工作队列 |
| 多机复杂调度 | PostgreSQL | 可选 Redis Streams / RabbitMQ / NATS |

## 9. 代码组织

保持现有 crate 边界，不创建大量新 crate。目标逻辑结构：

```text
crates/
├── autra-core/
│   ├── state
│   ├── event
│   ├── effect
│   └── stepper
├── autra-execution/
│   ├── journal
│   ├── dispatcher
│   ├── policy
│   └── host
├── autra-harness/
│   ├── react
│   ├── context
│   └── middleware
├── autra-runtime/
│   ├── coordinator/
│   │   ├── mod.rs
│   │   ├── transition.rs
│   │   ├── authorization.rs
│   │   └── result.rs
│   ├── scheduler/
│   │   ├── mod.rs
│   │   ├── limits.rs
│   │   └── queue.rs
│   ├── executor/
│   │   ├── mod.rs
│   │   └── dispatcher.rs
│   ├── recovery/
│   │   ├── supervisor.rs
│   │   └── reconcile.rs
│   ├── child/
│   ├── hub/
│   └── engine.rs
├── autra-durable-sqlite/
│   ├── journal
│   ├── outbox
│   └── connections
├── autra/
│   ├── agent
│   └── session
└── autra-cli/
```

这是逻辑目标，不要求一次性重命名所有文件。
不要重复实现已有的 `EffectExecutor`、审批或恢复代码。

**架构纪律：Runtime 不能直接依赖 Harness 的 ReAct 实现。**
Core 只认识通用状态和 Stepper；Harness 提供 ReActStepper，未来也可以提供其他行为模型；由上层 `autra` Facade 组装。

## 10. 迁移路线

每轮都保持现有测试通过。

### 第一轮：拆 LocalEngine，保持行为不变（已完成）

`engine/coordinator/` 已包含 run/drive、TransitionCommitter、AuthorizationGate
和 ResultCollector；`EffectExecutor` 保留。

从 `drive()` 逐步拆出：

- TransitionCommitter
- AuthorizationGate
- ResultCollector

继续复用现有 `EffectExecutor`。

**验收**：原子提交、审批、工具顺序、流式输出、ChildRun、恢复相关测试全部通过。

### 第二轮：加入 GlobalScheduler（本地版本已完成）

已实现 Model / Tool / Process 三类全局 `Semaphore`，并接入直接执行路径、
outbox 派发、恢复与 reconcile。有界队列、429 退避、加权公平和 SQLite
读写压测仍未完成。

**验收**：同时启动 100 个 Run，严格遵守配置的模型/工具并发上限，不产生无界等待队列。

### 第三轮：完善 Durable Recovery 与故障测试（部分完成）

`RecoverySupervisor` 已从 CLI 提取到 runtime，带服务租约并复用 `recover_once`。
持久化取消语义、Attempt 身份、Worker Lease/Fencing 代际和崩溃注入仍未完成。

**验收**：杀死进程后，已确认结果不丢失；用户取消不被当成普通崩溃恢复；非幂等未知操作不会自动重复。

### 第四轮：按真实需求支持多进程与多机

先验证单机多进程的领取和执行权协议，再提供 PostgreSQL 后端、远程 Worker 和工作环境隔离。最后根据性能数据决定 Redis。

**验收**：多个 Worker 竞争 Effect 不破坏状态；过期 Worker 不能提交当前执行权下的结果；非安全重试必须先对账。

**安全前置**：对不可信命令开放多机/远程执行前，必须先有真正的 Sandbox。Sandbox 不应等到分布式阶段结束。

## 11. 保留与修改

| 组件 | 决策 | 原因 |
|---|---|---|
| `autra-core` | 保留 | 状态机与 I/O 已解耦 |
| `ReActStepper` | 保留 | 行为逻辑不应被调度架构影响 |
| `EffectIntent / Record / Attempt` | 保留并增强 | 可靠执行协议的核心 |
| `LocalEngine` | 渐进拆分 | 职责过于集中 |
| `EffectExecutor` | 保留 | 已有实际执行边界 |
| `OutboxDispatcher` | 保留并增强 | 已具备持久化工作领取能力 |
| SQLite Journal | 保留并优化 | 单机轻量、部署简单 |
| RunHub | 保留 | 进程内观察仍有价值 |
| GlobalScheduler | 新增 | 当前缺少跨 Run 资源限制 |
| RecoverySupervisor | 增强现有 daemon | 长期安全运行需要 |
| Redis | 暂不引入 | 目前缺少必须使用它的证据 |
| PostgreSQL | 预留后端 | 为多机共享状态准备 |
| Remote Worker | 后续实现 | 先完善执行权与隔离协议 |

## 12. 最终架构原则

> **Autra Runtime 是一个由事件驱动的持久化 Agent 执行引擎。**
>
> Core 负责纯状态转换；Coordinator 负责 Run 的串行状态推进；Journal 负责原子提交和权威执行记录；Scheduler 负责全局资源分配；Executor 负责模型和工具的并发执行；RecoverySupervisor 负责中断后的安全恢复。
>
> 系统默认采用单进程 SQLite 模式，未来可以替换持久化和任务分发后端，但不能改变 Run、Event、Effect、Attempt、Authorization 和 Recovery 的基本执行语义。

## 13. ADR 待办与开放问题

建议后续形成 ADR：

1. Coordinator 的职责边界与 `LocalEngine` 拆分顺序。
2. TransitionCommitter 的提交协议和 Apply 时机。
3. GlobalScheduler 的资源池、公平性和领取顺序。
4. AttemptId / fencing token 与结果接受协议。
5. 持久化取消请求与 `CancelRequested` / `Cancelled` 语义。
6. ResultCollector 的去重、排序与恢复重放。
7. SQLite 读写连接模型与多进程写入协议。
8. PostgreSQL/远程 Worker 的后端契约。
9. Sandbox 作为不可信执行的前置条件。

开放问题：

- 第一轮拆分是否保持 `LocalEngine` 作为薄封装，还是直接迁移到 `coordinator/`？
- Scheduler 的公平性按 Run、Session 还是租户计算？
- 审批等待是否需要一个独立的持久化队列和唤醒机制？
- SQLite Reader Pool 的收益是否足以覆盖连接管理复杂度？
- 多进程阶段是否先做单机多进程，还是直接进入 PostgreSQL？
- 取消请求的持久化粒度：Run 级、Effect 级，还是两者都要？

---

**实施第一步**：在保持 `LocalEngine` 现有行为不变的前提下，先完成
`RunCoordinator → TransitionCommitter → EffectExecutor → ResultCollector`
之间的职责边界和测试，再加入 GlobalScheduler。
不要一次性重构整个 Runtime。
