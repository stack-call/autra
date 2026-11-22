# Autra 扩展系统的安全与一致性设计

> **状态**：设计评审 + 当前实现说明。P0 项已落地；P1/P2 仍为待办。
> **基线**：`mvp-v1`，当前最新提交见仓库历史。
> **定位**：让 Autra 保持 Capability-Oriented、Contract-Driven，而不是追求通用动态插件平台。

## 0. 一句话原则

**扩展可以改变 Agent 的行为，但不能改变 Runtime 的执行规则；扩展可以提出操作，但不能拥有操作的最终授权与提交权。**

具体落到三个词：

```text
Proposal → Validation → Commit
```

- 扩展负责 **Proposal**
- Runtime 负责 **Validation** 和 **Commit**
- Middleware 修改 Proposal 之前的内容
- Lifecycle 观察 Commit 及其后续执行产生的事实

这种架构保证的是**受控执行路径的语义一致性**，不等于可以安全运行任意恶意原生代码。不可信扩展仍然需要真正的权限隔离。

---

## 1. 当前已落地

| 项 | 状态 | 证据 |
|---|---|---|
| Transition Validator | ✅ | `engine/validator.rs`，ADR-0056；含 event/phase、disposition、intent 与预算一致性 |
| 初始化状态校验 | ✅ | `validator::validate_initialized`；`initialize` 不能改身份/预算/phase |
| Plan 按 Run 隔离 | ✅ | `PlanStore`、`Agent::plan_for(&RunId)`，ADR-0055 |
| Lifecycle 拆分与时序 | ✅ | `on_effect_proposed` / `committed` / `authorized` / `rejected` / `resolved`；committed 在 `commit_transition` 之后 |
| 先提交后应用状态 | ✅ | `drive` 先 `commit_transition`，成功后才 `state = next` |
| 失败/终态版本化提交 | ✅ | `commit_terminal` 使用 `expected_version` CAS，ADR-0057 |
| EffectExecutor 提取 | ✅ | 工具/模型执行控制移入 `engine/executor.rs` |
| 审批参数摘要绑定 | ✅ | `digest_intent` 绑定到 `ApprovalRequest` 与 `EffectRecord` |
| ReAct 状态版本拒绝 | ✅ | 异构/更高 revision/损坏状态返回 `StepError`，不回退为空历史 |
| SQLite WAL / busy timeout / schema version | ✅ | `configure_connection` |
| 执行 Middleware 链 | ❌ | 仅有 `ModelRequestTransform`，设计见 `runtime-evolution.md` |
| ObserverSet | ❌ | 当前 `AgentBuilder::observer` 仍是单插槽 |
| Registry 冲突检测 / Build Preflight | ❌ | `ToolRegistry::register` 仍是 `insert` |
| 版本迁移 | ⚠️ 部分 | 不兼容会拒绝；尚无迁移函数 |
| Sandbox 后端 | ❌ | `SandboxBackend` 只有契约 |

---

## 2. 三种扩展机制的权限

| 机制 | 能读取 | 能修改 | 绝对不能做 |
|---|---|---|---|
| **Capability** | 相关执行上下文 | 自己的内部状态、输出或提案 | 直接提交 RunState，绕过权限执行 |
| **Middleware** | 某阶段的受限输入 | 尚未提交的 Draft | 修改已提交 Effect、扩大权限 |
| **Lifecycle Hook** | 已发生事件的只读快照 | 自己的观测状态 | 修改执行结果、控制提交与审批 |

Tool 确实可能修改文件或调用外部 API，但必须在其执行权限受控的前提下，通过正常 Effect 流程调用。对同进程原生 Rust 扩展，这是 API 契约，不是针对恶意代码的强制进程隔离。

### 执行模型

```text
Harness / Capability
  Context · Model · Stepper · Skills

Middleware
  仅转换尚未提交的请求

TransitionProposal / EffectIntent
  行为层提交执行提案

Trusted Runtime
  Validate Transition / Scope / Budget
  Atomic Commit / Journal
  Policy / Approval
  Dispatcher / Result Recording

Lifecycle Hooks
  Runtime 在对应事实成立后发布只读事件
```

Runtime 不会因为安装了 Middleware、Hook 或自定义 Stepper，就改变状态提交和授权的基本规则。

---

## 3. Middleware：只允许修改 Draft

现有 `ModelRequestTransform` 是应保留的设计：

```rust
fn transform(
    &self,
    ctx: &ModelBuildContext,
    draft: ModelRequestDraft,
) -> Result<ModelRequestDraft, ExtensionError>;
```

`ModelRequestDraft` 只包含系统提示、消息和工具描述；它在生成 `CallModel` Effect 之后、Coordinator 提交之前执行。

三条约束：

1. **只接收最小权限数据。** 不要传 `&mut AgentState`、`&mut RuntimeContext` 或 `&mut ExecutionScope`，否则 Middleware 会逐步变成第二个 Runtime。
2. **失败不能静默跳过。** 敏感信息过滤 Middleware 失败后若继续发送原请求，会造成数据泄露。保持 `Result` 语义，让请求停止并走正常失败路径。
3. **所有转换必须发生在提交之前。** 未来的 ToolInput Middleware 也必须是：

```text
Transform → Validate → Commit → Authorize → Dispatch
```

不能在审批之后修改参数。若 Middleware 引入时间、随机数或外部数据，恢复时必须使用已提交的最终请求，而不是重新运行转换逻辑生成另一份请求。

当前不建议支持可任意短路、重试、多次调用 `next()` 的通用 Middleware。

---

## 4. Lifecycle Hooks：观察事实，不参与事务

当前 `ObserverHook` 已包含：

```rust
fn on_effect_proposed(&self, run_id: &RunId, effect: &EffectIntent) {}
fn on_effect_committed(&self, run_id: &RunId, effect: &EffectRecord) {}
fn on_effect_authorized(&self, run_id: &RunId, effect: &EffectRecord) {}
fn on_effect_rejected(&self, run_id: &RunId, effect: &EffectRecord) {}
fn on_effect_resolved(&self, run_id: &RunId, effect: &EffectRecord) {}
fn on_run_started(&self, run_id: &RunId) {}
fn on_model_request(&self, run_id: &RunId, turn: u32) {}
fn on_run_finished(&self, run_id: &RunId, phase: &AgentPhase) {}
```

`on_effect_committed` 已放在 `commit_transition` 成功之后。

### 错误与正确时序

错误：

```text
on_effect_committed() → journal.commit() → Commit Failed
```

Hook 已宣称提交成功，但事实没有发生。

正确：

```text
journal.commit() → Commit Succeeded → on_effect_committed()
```

### 行为契约

- Hook 接收只读事件快照。
- Hook 不返回状态变更或授权决定。
- 普通 Hook 的失败不应回滚已提交事务。
- Observer 的耗时和异常不能无限阻塞 Coordinator。
- 必须可靠交付的事件通过 Journal/Outbox，而不是普通回调。

同步 Rust Hook 仍可能阻塞、Panic，或通过自己持有的外部资源产生副作用。**只读参数不等于执行隔离。** 普通 Lifecycle Hook 应被定义为 best-effort 可观测性机制；可靠审计需要独立的持久化事件消费者。

---

## 5. Capability：真正需要强化的是 Stepper

`AgentStepper::transition` 返回：

```rust
pub struct TransitionProposal {
    pub next: AgentState,
    pub intents: Vec<EffectIntent>,
    pub disposition: Disposition,
}
```

`AgentState` 包含公开可修改的 `run_id`、`budget`、`phase`、`parent_run_id`、`engine_state`。如果 Runtime 只覆盖 `state_version`，自定义 Stepper 就可能改变身份、扩张预算或非法跃迁。

### 已实现的 TransitionValidator

Runtime 在记录 Effect 或派发之前统一校验：

- Run / parent / Session / Turn 身份不可变
- `state_version` 必须从 expected 精确 +1
- 预算上限不可变；使用计数单调且不超限
- 已初始化的 `engine_id` 不可中途切换
- phase 必须沿合法状态图
- `Failed` 必须带 `last_error`

`initialize()` 之后也会校验：身份、预算、`state_version`、`Ready` phase 不能被初始化修改。

初始化失败返回 `RunError::Protocol`，不会进入 Effect 提交。

### 长期方向

长期可让 Stepper 只返回 Engine 自己拥有的状态变更，把 Run 身份、权限和部分预算字段完全收归 Runtime。当前先增加 Validator 更容易落地。

---

## 6. Tool 的边界

Tool 是真正产生外部副作用的能力，不能只依赖工具自己声明的 `ToolContract`。

```text
Tool → EffectIntent → Host Policy → Approval → Dispatcher → Tool.execute()
```

不要把 Tool 注册等同于 Tool 授权。

`PolicyChain` 已实现限制性合成；审批记录绑定参数摘要和 Policy Revision，这些应保留。

两个前提：

1. 普通扩展不能替换或降低 Host 的权限基线。
2. Tool 代码不可信时，必须使用独立受限执行环境；同进程 Trait 无法阻止它直接访问进程权限。

### 信任边界

| 扩展类型 | 可信要求 | 推荐执行方式 |
|---|---|---|
| 原生 Rust Tool / Engine | 可信代码 | 同进程 Trait |
| 内部受控工具 | 可信执行端 | Local Dispatcher |
| 第三方远程 Tool / MCP | 限权、可审计 | Gateway / Tool Host |
| 不可信代码执行 | 不可信 | OS / 容器级 Sandbox |

`SandboxBackend` 契约已存在，但还没有具体 Backend。

---

## 7. Effect 身份与恢复语义

```text
1 EffectIntent      固定工具名称和参数
2 Commit            原子记录状态与待执行 Effect
3 Authorize         对已提交的操作做授权判断
4 Dispatch          使用同一份已授权参数执行
5 Record Result     记录成功、失败或结果未知
6 Resume            依据已记录状态继续执行
```

### 不变量 A：提交什么，就执行什么

审批绑定的参数摘要必须与最终 Dispatcher 使用的请求一致。Middleware、Hook 或 Tool Wrapper 不能在审批后更改实际请求。

### 不变量 B：扩展不能自行决定重试

外部支付 API 发出后崩溃，Runtime 无法确认支付是否成功。此时状态应是 `OutcomeUnknown`，而不是让 Middleware 自动重新调用。

只有确认幂等性，或使用外部系统认可的幂等键和对账机制，才可以安全重试。Outbox 也不能单凭本地 Journal 保证任意外部副作用的 exactly-once。

### 不变量 C：恢复必须基于已提交事实

恢复时不能重新运行会生成不同 Tool 参数的 Middleware，然后用新请求继续旧 Effect。

当前 Journal、Outbox 和 `OutcomeUnknown` 已提供基础，应继续强化约束，而不是新增执行系统。

---

## 8. API 层：用接口权限而不是权限管理器

| 级别 | 可注册 | 允许控制 |
|---|---|---|
| Agent Capability | Tool、Model、Context、Skill | Agent 功能与行为 |
| Behavior Extension | Middleware、Stepper | 未提交请求和执行提案 |
| Lifecycle Extension | ObserverHook | 只读执行事件 |
| Trusted Runtime Adapter | Dispatcher、Journal、Host | 真实 I/O、持久化和执行环境 |

前三类通过普通 `AgentBuilder` 提供：

```rust
Agent::builder("assistant")
    .model(provider)
    .tool(MyTool)
    .middleware(MyTransform)
    .observer(MyObserver)
    .build()?;
```

第四类继续支持，但作为显式高级接口，调用者需承担信任责任和协议约束；替换 Dispatcher 或 Journal 应通过契约测试证明其满足 Effect 身份、持久化、取消和恢复语义。

Rust 类型系统能限制数据形状，但不能保证任意实现者正确执行协议，更不能隔离同进程恶意代码。因此除了接口设计，还必须有 Runtime 校验与契约测试。

---

## 9. 待办与优先级

| 优先级 | 改动 | 完成标准 | 状态 |
|---|---|---|---|
| P0 | Transition Validator | 恶意 Stepper 无法改 RunId、扩预算、提交非法状态 | ✅ |
| P0 | 初始状态与 Run 隔离 | `initialize()` 不能破坏身份；并发 Run 不共享可变状态 | ✅（Plan 已隔离） |
| P0 | Effect 参数不可变约束 | 审批、记录、执行使用同一份请求 | ✅ 基本满足，待补回归测试 |
| P1 | ObserverSet 与失败隔离 | 多 Observer 组合；普通 Hook 故障不影响权威提交 | 未做 |
| P1 | Registry 校验 | 同名覆盖、协议不兼容在 Build 时明确报错 | 未做 |
| P1 | 恢复兼容性测试 | 已提交 Effect 不因升级或重新构造请求而改变 | 部分 |
| P1 | Build Preflight | 重名、版本、能力、依赖在 `AgentBuilder::build` 阶段失败 | 未做 |
| P2 | 新的 Middleware 类型 | 只有真实场景出现时再添加 | 未做 |
| P2 | MCP / Sandbox / Remote Dispatcher | 新能力不改 Core，权限边界可验证 | 未做 |
| P3 | Extension Package 生态 | 可组合复用，静态组合层而非动态平台 | 未做 |

---

## 10. 扩展安全回归测试

建议覆盖：

- 恶意 Stepper 改变 RunId / parent / session / turn
- Stepper 扩张预算上限或回滚计数
- Stepper 非法 phase 跃迁或从终态离开
- `initialize()` 改变身份、预算或 phase
- 失败 Middleware 不应发送原请求
- Panic Observer 不应破坏已提交事务
- 重复 Tool / Skill 名称注册
- 审批后请求篡改
- 崩溃恢复使用已提交请求
- 非幂等操作出现未知结果时不得自动重试
- Journal 提交失败时不派发 Effect（已有 `FailingJournal` 测试）

最重要的标准不是“某个扩展正常运行”，而是：

> **无论扩展如何返回错误、修改允许修改的数据或产生非法提案，都无法让 Runtime 提交一条违反核心不变量的状态转换。**

---

## 11. 不建议引入

- 通用 `ExtensionContext`，让所有扩展共享 Runtime 能力。
- 所有执行阶段统一的 `before/after/around` Hook。
- 可以绕过 Dispatcher 的 `next()` 型执行拦截器。
- 可任意修改 `AgentState` 的 Lifecycle。
- 当前阶段没有实际需求的 `PluginManager` 和 `ServiceRegistry`。
- 让 ExtensionRegistry 变成随处可调用的全局 Service Locator。

---

## 12. 最终取舍

- **Capability**：可扩展能力。允许新增 Tool、Model、Context、Stepper，但所有执行提案必须受 Runtime 检查。
- **Middleware**：可扩展变换。只处理指定阶段的 Draft，不接触权威状态和已提交 Effect。
- **Lifecycle**：可扩展观察。观察 Runtime 发布的事实，不参与提交、审批和执行决策。
- **Runtime**：不可被普通扩展绕过的权威机制。统一管理 State、Budget、Scope、Effect、Commit、Approval、Dispatch 和 Recovery。

`Proposal → Validation → Commit` 是 Autra 最小安全扩展架构的核心。

---

## 参考

- [`adr/0055-plans-are-run-scoped.md`](adr/0055-plans-are-run-scoped.md)
- [`adr/0056-transition-proposals-are-validated.md`](adr/0056-transition-proposals-are-validated.md)
- [`runtime-evolution.md`](runtime-evolution.md)
- [`extensions.md`](extensions.md)
- [`execution-model.md`](execution-model.md)
- [`durability.md`](durability.md)
