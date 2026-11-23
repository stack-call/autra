# Autra Runtime：简化架构设计 RFC

> **状态**：Proposed（设计建议，非现有实现说明）  
> **基线**：`stack-call/autra` 的 `mvp-v1` 分支  
> **目的**：拆分运行循环与真实 I/O，保留 Durable 能力，不提前建设分布式调度平台。  
> **核心决策**：`RunLoop + EffectExecutor + 可选 Journal`。
> **相关**：[`runtime-vnext.md`](runtime-vnext.md) 给出了更完整的
> Coordinator / Scheduler / Executor / Journal / Recovery 目标架构与迁移路线。

## 1. 一句话设计

**Harness 决定下一步做什么；RunLoop 推进并提交 Agent 状态；EffectExecutor 负责受控执行；Dispatcher/Provider/Tool 完成实际 I/O；Journal 为需要恢复的运行保存事实。**

这里的「拆分」是职责拆分，不代表必须拆成多个进程、crate 或 Trait。

```text
                         Agent / AgentBuilder
                                  |
                                  v
                       +---------------------+
                       |     RunLoop         |
                       | Event -> Stepper    |
                       | -> Prepare/Commit   |
                       | -> Result -> Event  |
                       +----------+----------+
                           ^      |
              Stepper API  |      | committed effects
                           v      v
                    +---------+  +-----------------------+
                    | Harness |  |    EffectExecutor     |
                    |  ReAct  |  | policy / approval     |
                    +---------+  | timeout / concurrency |
                                 | attempt / dispatch    |
                                 +-----------+-----------+
                                             |
                                             v
                                 +-----------------------+
                                 |    LocalDispatcher    |
                                 | ModelProvider / Tool  |
                                 +-----------------------+

          Journal / Recovery（可选）：与 RunLoop 和 EffectExecutor 协作，
          持久化状态、Effect/Attempt、执行结果，进行安全恢复。
```

## 2. 为什么只保留这些组件？

| 真实需求 | 最小增加的机制 | 暂时不需要 |
|---|---|---|
| ReAct 能持续调用模型和工具 | `RunLoop` + `Dispatcher` | Scheduler、任务队列 |
| 模型决策不应直接变成执行授权 | `EffectExecutor` 内的 Policy / Approval Gate | 独立授权微服务 |
| 多个工具可以并发 | `EffectExecutor` 内的有界并发 | 全局公平调度器 |
| Run 崩溃后可恢复 | `Journal`、Effect/Attempt 记录、Recovery | Remote Worker |
| 多进程竞争同一 Run | Store 的版本检查 / CAS、必要时 Lease | 通用分布式编排平台 |
| 多机执行、跨 Run 公平性 | **未来**增加 Scheduler / Worker / Result Inbox | 当前核心路径不依赖它们 |

**抽象准入原则：**只有当一个组件具有独立的变化原因、独立的不变量，或显著减少耦合时，才把它独立出来。不要为了让 `drive()` 看起来短，就创建只负责转发的组件。

## 3. 各组件的明确边界

### 3.1 RunLoop —— 推进一个 Run

负责：

- 读取并排序当前 Run 的 `AcceptedEvent`；
- 调用 `AgentStepper::transition(state, event)`；
- 验证提案、构造 EffectRecord；
- **Prepare → Commit → Apply**：在提交成功之后才更新内存中的权威 State；
- 将已提交 Effect 交给 Executor；
- 将已确认的 ModelResult / ToolResult 转为下一条事件；
- 识别完成、失败、取消及合法等待状态。

不负责：

- 直接调用 `ModelProvider`、`Tool::execute`；
- 亲自实现工具审批、超时或并发任务队列；
- 通过 Lifecycle Hook 修改 AgentState。

**注意**：RunLoop 可以 `await executor.execute(...)`。等待抽象执行接口并不等于混合了实际执行职责；应避免的是它直接管理工具实例、Tokio JoinSet、Claim/Lease 等细节。

### 3.2 EffectExecutor —— 执行控制

负责：

- 确认 Effect 已经由 RunLoop 接受，并满足必要的提交规则；
- ToolPolicy、审批协调、预算、取消；
- 控制 Timeout、有界并发和 Attempt 的记录；
- 调用现有 `Dispatcher`；
- 记录或返回标准化的执行结果；
- 根据 EffectClass 和历史 Attempt 判断能否安全重试，**不能在 Journal 不知情时自行重试**。

不负责：

- 决定 ReAct 的下一步；
- 修改 AgentState 或伪造 AcceptedEvent；
- 赋予自己新的权限；
- 实现所有 Model/Tool 的业务逻辑。

### 3.3 Dispatcher —— 真实 I/O 适配

继续保留当前的 `Dispatcher` / `LocalDispatcher`：

- Model：调用 `ModelProvider`，支持流式增量输出；
- Tool：通过 `ToolRegistry` 调用具体 `Tool`；
- 把底层错误映射为执行结果。

**不在第一版重新设计一套 `EffectRunner` Trait。** 如果未来出现本地/远程两种实质不同的执行后端，再提升抽象。

### 3.4 Journal / Recovery —— 可选可靠性

- **Ephemeral**：使用内存 State、事件队列和现有 Dispatcher；进程退出不保证恢复。
- **Durable**：使用 `DurableTransitionStore` 与 Effect Journal，原子记录 State、AcceptedEvent、Effect 与适用的 Outbox，记录 Attempt/Outcome；恢复时先核对 Effect，而不是重放所有 I/O。
- 外部服务的副作用与本地事务无法自动原子化；超时、进程崩溃可能是 `OutcomeUnknown`。

保留已有 `OutboxDispatcher` 作为 Durable 交付实现；当前阶段不要求将它拆成 Scheduler + Delivery + Runner + Inbox 四层。若将来外部 Worker 独立于 RunLoop 异步返回结果，**届时**才必须设计可靠的结果收据、去重及原子消费协议。

## 4. 主执行流程

```text
RunStarted / ModelResult / ToolResult
                  |
                  v
             RunLoop
                  |
         Stepper.transition()
                  |
          TransitionProposal
                  |
          prepare_transition()
                  |
         commit_transition()
          /                 \
       failed             success
         |                   |
    不派发 Effect       apply committed State
                             |
                         terminal?
                         /      \
                       yes      no
                        |        |
                    RunOutcome   v
                          EffectExecutor
                           |       |
                        policy   approval
                           \       /
                              v
                         Dispatcher
                              |
                         Model / Tool
                              |
                    verified EffectOutcome
                              |
                       next AcceptedEvent
                              |
                              +----> RunLoop
```

`await` 等待审批时，简单 Local 模式可以保持阻塞当前 Run 的方式；若需要跨进程或长时间审批恢复，应将等待状态和授权决定持久化，不能依赖活着的 Future。

## 5. 精简接口草案

以下 Rust 为**概念伪代码**，不承诺和 `mvp-v1` 当前类型完全一致。

```rust
pub struct RunLoop {
    stepper: Arc<dyn AgentStepper>,
    executor: Arc<EffectExecutor>,
    journal: Option<Arc<dyn DurableTransitionStore>>,
}

impl RunLoop {
    pub async fn run(
        &self,
        mut state: AgentState,
        mut events: VecDeque<AcceptedEvent>,
    ) -> Result<RunOutcome, RunError> {
        loop {
            let event = events.pop_front().ok_or(RunError::NoEvent)?;
            let proposal = self.stepper.transition(&state, &event)?;

            // 内部纯准备，不修改权威 state，不启动外部 I/O。
            let prepared = prepare_transition(&state, &event, proposal)?;

            // Durable 模式执行原子提交；本地模式验证版本并接纳。
            let committed = commit_transition(self.journal.as_deref(), prepared)?;
            state = committed.state;
            emit_committed_events(&committed);

            if committed.is_terminal() {
                return Ok(outcome_from(&state));
            }

            // 可能返回“等待审批”等非终态结果；不能假设总有下一事件。
            match self.executor.execute(committed.effects).await? {
                ExecuteStep::Results(results) => {
                    // Durable 模式先记录结果，再接纳后续状态机事件。
                    events.extend(to_accepted_events(results)?);
                }
                ExecuteStep::Waiting => {
                    return Err(RunError::Suspended); // 示例：正式 API 可返回 RunHandle
                }
            }
        }
    }
}

pub struct EffectExecutor {
    dispatcher: Arc<dyn Dispatcher>,
    tools: ToolRegistry,
    policy: PolicyChain,
    approver: Option<Arc<dyn Approver>>,
    max_parallel_tool_calls: usize,
    // 与 Journal 协作记录审批、Attempt 和结果。
}
```

**为什么不再增加 Service / Delivery / Runner 三层？** 目前 Model/Tool 的调度和执行后端只有少量固定实现；新增三次转发会让调用路径更长，但不一定减少任何独立变化。需要远程执行时再提取真正的 Executor/Delivery 边界。

## 6. 两条不可妥协的可靠性规则

### 6.1 先提交，再执行

```text
Stepper Proposal
    -> PreparedTransition
    -> Store Commit (success)
    -> Committed Effect
    -> Authorization / Dispatch
```

如果 Store 提交失败，不得派发对应 Effect；内存中的权威 State 也不得提前换成未提交版本。终态、错误和取消路径要明确版本化提交规则。

### 6.2 不能假装 Exactly-once

假设 `send_email` 已经发出，但 Runtime 崩溃、没收到返回值。恢复时不能简单重试。

- `ReadOnly`、具备明确幂等条件的 Effect：可按策略重试；
- `NonIdempotent` / `Unknown` 且外部结果未知：标记 `OutcomeUnknown`，等待核对或人工处理；
- 重试应保留同一 `EffectId`，为每次真实派发记录 Attempt；
- 版本检查防止旧 Coordinator 覆盖新 State，但本身不等于阻止所有外部重复副作用。

## 7. Lifecycle、Middleware、Policy 放在哪里？

| 机制 | 位置 | 能力边界 |
|---|---|---|
| `ModelRequestTransform` | Harness | 修改**尚未提交**的模型请求 |
| `RunLifecycle` | RunLoop | 观察 RunStarted / TransitionCommitted / RunFinished |
| `EffectLifecycle` | EffectExecutor / Journal | 观察 EffectCommitted / AttemptStarted / EffectResolved |
| `ExecutionMiddleware`（Wrap，可后加） | Dispatcher 执行边界 | Tracing、Metrics、受控 I/O 包裹 |
| `ToolPolicy` / `Approval` | EffectExecutor 的可信 Gate | 可以拒绝执行；不能由普通 Middleware 绕过 |

Lifecycle 是事实观察，Wrap 是执行拦截，Policy 是不可绕过的授权约束。`EffectCommitted` 必须在 Store 提交成功后发出；需要可靠投递的业务通知应单独使用事务性记录/Outbox，而不是普通回调。

## 8. 最小目录调整

```text
autra-runtime/src/
├── engine/
│   ├── mod.rs             # LocalEngine 对外 API 与组装
│   ├── run_loop.rs        # 事件驱动、Stepper、状态提交
│   └── recovery.rs        # 恢复入口与 Effect 核对
├── execution/
│   ├── mod.rs
│   ├── executor.rs        # Policy、Approval、Attempt、并发
│   └── approval.rs
├── dispatcher.rs          # 保留现有 Dispatcher / LocalDispatcher / Outbox 实现
├── journal.rs
├── lifecycle.rs
├── child.rs
└── hub.rs
```

目录是建议，不要求立刻创建全部文件；尤其无需为每个小模块增加一个公开 Trait。

## 9. 从当前 `mvp-v1` 迁移

**P0 — 测试现有语义。** 固定 ReAct Model→Tool→Model、并发 Tool 结果关联、审批、取消、Journal 失败、恢复路径的行为。

**P1 — 统一提交边界。** 将状态转换改为 `Prepare → Commit → Apply`；收敛正常完成、失败和取消的提交语义；提交后才发布 `EffectCommitted`。

**P2 — 提取 EffectExecutor。** 从 `drive()` 和 `execute_tool_batch()` 提取执行控制；继续复用已有 `LocalDispatcher`、`OutboxDispatcher`，不改变 Core/Harness API。

**P3 — 减轻 RunLoop。** 让 `drive()` 只组织状态推进、提交、等待执行结果和重新接纳事件；将工具并发和审批细节移出。

**P4 — 按需求扩展。** 只有明确出现多机 Worker、全局配额或独立结果回传时，才设计 Scheduler、Result Inbox、Remote Executor 等能力。

### 必须通过的验收测试

- Commit 失败时不派发 Effect，State 未被错误接纳；
- 同一 Run 的过期版本提交被拒绝；
- 未审批或被拒绝的 Tool 不被执行；
- 并发 Tool 的 `call_id` 与结果对应正确；
- Model Streaming 的 Delta 不作为已完成的 ModelResult；
- 非幂等操作出现未知结果时不会盲目重试；
- Resume 不丢失 `engine_state`，也不会重复接受同一已处理事件；
- Lifecycle 的 Committed 通知不会先于真正的存储提交。

## 10. 非目标与未来演化

本阶段**不建设**独立 Scheduler 服务、远程 Worker、通用 DAG/Graph Engine、Durable Result Inbox 服务、复杂的多层 EffectRunner 框架，也不承诺操作系统级沙箱或任意外部副作用 Exactly-once。

如果未来需要把执行卸载到其他进程，再进一步演化：

```text
RunLoop -> Durable Outbox -> Scheduler -> Remote Executor
   ^                                           |
   +------------ Durable Result Inbox <-------+
```

该图是**远期方案**，不是当前最小 Runtime 的必经路径。

## 11. 设计结论

**当前版本保持三件事：**

1. `RunLoop`：AgentState 的权威推进和 Transition 提交。
2. `EffectExecutor`：授权、审批、超时、并发及执行结果控制。
3. `Journal / Recovery`（可选）：持久化、Effect/Attempt 记录与安全恢复。

实际 Model/Tool 调用继续复用现有 `Dispatcher`；不为了命名整齐而提前增加 Scheduler、Delivery、Runner、Inbox 四层。先保护执行不变量，再用真实需求推动下一次拆分。
