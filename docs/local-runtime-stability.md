# 本地运行时稳定性：无沙箱加固设计

> **状态**：设计评审；P0 项已实现（见 3.1），P1/P2 待实现。  
> **基线**：`mvp-v1`。  
> **范围**：单机、单用户、本地进程、无沙箱。  
> **相关**：[执行模型](execution-model.md)、[持久化](durability.md)、
> [工具安全](tool-security.md)、[运行时演进](runtime-evolution.md)、
> ADR-0056、ADR-0057。

## 0. 结论

本地无沙箱的稳定性，难点不在“把命令跑起来”，而在四个边界：

1. **所有权**：同一时刻谁拥有一个 Run / Effect；
2. **时间**：租约、超时、取消、审批 TTL 之间的约束关系；
3. **进程**：命令的进程树、输出、环境、资源与强制终止；
4. **文件**：原子写、路径约束与并发写冲突。

在这四条边界上，最危险的不是进程崩溃，而是**没有崩溃却重复执行了非幂等副作用**，
或者**两个执行者同时接管同一个 Run**。本设计把这两类问题列为 P0。

---

## 1. 无沙箱的承诺边界

本地模式以**当前用户权限**运行，不是安全沙箱。

必须明确：

- 模型输出是不可信输入；
- 仓库里的 `build.rs`、npm scripts、git hooks、`.env` 都可能执行任意代码；
- `run_command` 可以通过 shell、绝对路径、符号链接访问 workspace 之外的路径；
- 网络请求可以把工作区内容发出去；
- `policy` / `approval` 是**人工确认机制**，不是权限隔离；
- `LocalWorkspace` 的路径校验是**便利性约束**，不是 jail；
- 审批中的 `actor` 目前只是声明值，未验证身份。

因此本地稳定性目标是：

> 保证同一条外部副作用最多执行一次；失败可解释；取消可到达；文件不半写；
> 多个本地进程不互相踩；所有增长有上限。

**非目标**：多租户、远程 worker、分布式调度、强隔离沙箱。这些需要
`ExecutionHost` / `SandboxBackend` 的独立设计，不在本文范围。

---

## 2. 六条不变量

| 不变量 | 破坏方式 | 设计规则 |
|---|---|---|
| 每个 Effect 同时只有一个 owner | 两个进程或 daemon 同时 dispatch 同一 effect | claim + lease 续租 + 每次写入 fencing |
| 非幂等 Effect 未知时绝不自动重试 | 超时/崩溃后盲目重跑 `run_command` | `OutcomeUnknown` 必须人工处理或走 reconcile 白名单 |
| 取消能到达模型、工具和子进程 | 取消只阻止下一个 effect | 取消原因 + 截止时间 + 强制杀进程树 |
| 文件写入原子且可检测冲突 | 写到一半崩溃、并行写同一文件 | temp + fsync + rename；按路径锁或内容 hash CAS |
| 恢复只接管无主 Run | daemon 把正在运行的 Run 再 resume 一遍 | 心跳/租约；只接管租约过期的 orphan |
| 所有增长有上限 | journal、输出、事件、子进程无限增长 | 输出上限、运行上限、retention/GC |

---

## 3. 当前实现基线

| 关注点 | 现状 | 缺口 |
|---|---|---|
| 原子 transition commit | 已有：state + event + effects + outbox 同事务，`state_version` CAS | — |
| 终态提交 | 已有：ADR-0057，终态使用带版本的 CAS 提交 | 需覆盖所有 Effect 写入，而不只是终态 |
| transition 校验 | 已有：`engine/validator.rs` 校验 phase、event、disposition、intent、预算 | 可继续补 Effect 状态机自身的合法迁移表 |
| 模型取消 | 已有：`await_with_cancel` 中断模型 future | 取消原因、超时分类未统一 |
| 工具取消 | 已有：`RunCommandTool` 读取 `ToolContext.cancel`，Unix 进程组 SIGTERM→SIGKILL | Windows Job Object 仍待实现 |
| Claim | 已有：`claim_id = pid + 计数器`；dispatch 写入统一走 `record_claimed`，非 owner 写入被拒绝 | — |
| Lease | 已有：默认 60 秒，执行中按 1/3 TTL 续租，过期可回收 | 进程长时间暂停下的重复执行仍需故障注入验证 |
| Run ownership | 已有：`run_leases` + 心跳；resume/reconcile 先 claim；`AttemptId`/worker/fencing 元数据 | 跨进程代际协议仍待故障注入验证 |
| 取消 | 已有：持久化 `CancelRequest`、`autractl cancel`、lease 轮询与恢复前检查 | 取消原因/截止时间语义仍可继续细化 |
| 恢复 daemon | 已有：`autractl daemon`；服务租约单实例、跳过活跃 owner、校验 Agent profile（含 provider/model） | 跨机器配置一致性仍待设计 |
| Journal | 已有：WAL、`busy_timeout=5s`、`synchronous=NORMAL`、schema version、cancel/lease 表、`autractl gc`、`doctor`、单 Writer + 有界写队列 + Reader Pool | 容量与锁竞争仍需压测 |
| 文件写入 | 已有：同目录 temp + fsync + rename、拒绝 symlink、pre-image 校验、进程内写锁 + OS 文件锁 | Windows Job Object/资源上限仍未实现 |
| 命令执行 | 已有：环境变量脱敏、输出上限、超时、取消、Unix 进程组、Windows `taskkill /T` | Windows Job Object 与资源限制仍未实现 |
| 审批 | 已有：参数 digest、policy revision、TTL、actor、持久化复用 | actor 未验证；审批绑定的是文本而非结构化命令 |
| Host/Sandbox | 已有：`ExecutionHost` / `SandboxBackend` 契约 | 无适配器；harness 工具绕过 Host 直接调用 OS |
| 上下文/Token | 已有：字符预算、provider token usage 统计 | 成本预算、模型级限流仍缺 |

## 3.1 P0 实施状态

| 项 | 状态 | 实现要点 |
|---|---|---|
| P0-1 Effect ownership | 已实现 | claim 默认 60 秒并续租；`record_claimed` 校验 claim；`mark_dispatched`/终态写入均 fencing |
| P0-2 Recovery daemon | 已实现 | Run lease + 心跳；daemon 服务租约保证单实例；跳过有活跃 owner 的 Run；恢复校验 Agent profile hash |
| P0-3 工具取消与进程树 | 已实现（Windows best-effort） | `run_command` 接入 `CancelFlag`；Unix 进程组 SIGTERM→SIGKILL；Windows 保留 `taskkill /T`，Job Object 仍是后续项 |
| P0-4 原子写与冲突 | 已实现 | 同目录 temp + fsync + rename；最终路径拒绝 symlink；`edit_file` 校验 pre-image；进程内写锁 + `.autra/workspace.lock` OS 文件锁 |
| ReAct 历史复制 | 已优化 | `fork_state` 避免复制旧 engine payload；stepper 按 `state_version` 复用解码状态；每次转换只序列化一次 |

仍待完成：

- Windows Job Object 与更完整的内存/进程数限制；
- 跨进程/恢复场景的崩溃与租约故障注入矩阵；
- effect 查询与更细的审计/导出；
- `ResumeRequested` / `CancelRequested` 仍会复制完整 `AgentState`。

---

## 4. P0：会静默破坏用户数据的问题

### P0-1 Effect ownership、lease 续租与 fencing

**状态：已实现。** 下面保留原始问题描述，便于理解动机。

**现象**

- 原始问题：`DEFAULT_CLAIM_LEASE` 为 30 秒；CLI 的 effect timeout 为 120 秒，
  `run_command` 最长 300 秒，模型流式调用也可能超过 30 秒。
- Lease 到期后，另一个 dispatcher / daemon 可以重新 claim 同一行。
- `record()` 与 `mark_dispatched()` 写入 Effect 记录时不校验 claim id；
  只有 `complete_outbox` / `release_outbox` 校验。

**危险**

过期 owner 仍然可以把执行结果写回 journal，新 owner 也可能已经执行了同一 Effect。
对于 `run_command`、`write_file`、外部 API 调用，这意味着**重复副作用**。
`state_version` CAS 只能防止状态互相覆盖，不能防止外部动作重复发生。

**设计**

引入显式的 Effect lease：

```text
Lease {
    effect_id,
    run_id,
    owner_id,        // 进程/实例身份
    claim_id,
    attempt,         // 第几次 claim
    deadline,
}
```

规则：

1. claim 的 TTL 必须大于该 Effect 的最大执行时间（模型 + 工具 + 杀进程宽限）。
2. 执行期间按 TTL/3 续租；续租失败立即取消执行，并标记 `OutcomeUnknown`。
3. 所有 Effect 写入都带 `(claim_id, attempt)`；不匹配则拒绝，不能“最后写入者获胜”。
4. 终态提交对 `(attempt, state_version)` 做 CAS。
5. 租约丢失时，不允许普通失败处理；必须走 unknown / reconcile 路径。
6. `record()` 的接口需要拆分：`record_claimed(...)` 与只读的 `load(...)`。

**验收**

- 一个执行超过 lease 的模型/工具不会被第二个 dispatcher 重复执行。
- 两个进程同时 claim 同一 Effect，只有一个能写入终态。
- 杀掉 owner 后，新 owner 能接管，且旧 owner 的写入被 fencing 拒绝。
- 长于 lease 的命令测试必须覆盖 Windows 与 Unix。

### P0-2 Recovery daemon 的接管语义

**状态：已实现。** daemon 服务租约、Run 心跳、活跃 owner 跳过与 profile 校验均已落地。

**现象**

- `recover_once` 扫描 `list_run_ids()`，对所有
  `AwaitingTool` / `AwaitingModel` 的 Run 直接 `resume`。
- 正常执行中的 Run 恰好会持久化为 `AwaitingModel`（模型调用进行中）。
- daemon 没有单实例锁、没有 Run 心跳、没有 owner 判断。
- daemon 使用固定的 Agent 配置与 `RejectApprovals`。

**危险**

用户在前台运行一个 Run，后台 daemon 可能在 5 秒后把它再 resume 一遍。
即使状态提交被 CAS 挡住，模型/工具的外部动作可能已经发生。
两个 daemon 同时运行会放大这个问题。

**设计**

1. **Run ownership / heartbeat**：活跃 owner 定期刷新 `owner_id + deadline`；
   daemon 只接管没有 owner 或租约已过期的 Run。
2. **单实例 daemon**：文件锁或 OS 互斥；已有实例时拒绝启动或等待；
   优雅关闭时释放锁和心跳。
3. **只接管 orphan**：不接管活跃 Run，不接管终态 Run。
4. **配置身份**：在 Run 上记录 `AgentProfile` 的 hash（模型、instructions、
   工具集合、policy revision、engine id）。恢复时配置不一致必须拒绝并隔离，
   不能用一个默认 Agent 去恢复另一个 Agent 的 Run。
5. **审批策略**：后台默认不自动审批；遇到 `AwaitingApproval` 停在原地；
   自动审批只能显式开启。`RejectApprovals` 会把本来可以等待的 Effect 直接拒绝，
   需要区分“等待”和“拒绝”。
6. **退避与隔离**：单个 Run 失败不阻塞整轮；失败 Run 指数退避并记录原因；
   每轮有数量上限和确定性顺序。

**验收**

- 前台 Run 运行时启动 daemon，不会重复 dispatch。
- 两个 daemon 同时启动，只有一个获得所有权。
- 配置 hash 不一致时拒绝恢复并给出明确原因。
- 需要审批的 Effect 在 daemon 下保持 `AwaitingApproval`，不会被隐式拒绝。

### P0-3 工具取消与进程树

**状态：已实现，Windows 仍为 best-effort。** Unix 使用进程组；Windows Job Object 仍需后续补齐。

**现象**

- `RunCommandTool` 通过 `cmd /C` 或 `sh -c` 执行字符串命令。
- 有超时、输出上限、环境脱敏、`kill_on_drop`，Windows 上使用 `taskkill /T`。
- 不读取 `ToolContext.cancel`；Unix 上没有进程组，`child.kill()` 只杀直接子进程。

**危险**

- 用户取消后命令继续运行，可能继续修改工作区。
- 孙进程成为孤儿，继续占用端口、写文件或发网络请求。
- 超时/取消后的“未知”状态不统一，恢复时可能被当成普通失败重试。

**设计**

1. 引入 `CommandSpec`（program、argv、cwd、env、stdin、timeout、output_limit），
   命令执行不再以裸字符串为契约；需要 shell 时显式声明，并使用固定 shell、
   禁用 profile（如 `sh -c` 非登录、PowerShell `-NoProfile`）。
2. 进程树：
   - Unix：`setsid` 建进程组，先 `SIGTERM`，宽限期后 `SIGKILL`，`killpg` 整组；
   - Windows：Job Object + `KILL_ON_JOB_CLOSE`，`taskkill /T /F` 仅作兜底；
   - 终止后必须 `wait` 回收，避免僵尸进程。
3. `CancelFlag` 到达工具层；取消原因区分 User / Parent / Timeout / Shutdown / LeaseLost。
4. 强制终止后标记 `OutcomeUnknown`，除非能证明动作未发生。
5. 限制：输出上限、超时、进程数、内存（Job Object / rlimit）、磁盘可用空间。
6. 环境：保留现有 secret 过滤，进一步改为 allowlist；固定 `cwd`；
   stdin 关闭；避免继承 agent 的临时目录和代理变量。
7. 长驻/后台进程：检测到不退出时给出明确错误；agent 退出时清理 job/进程组。

**验收**

- 取消一个 `sh -c 'sleep 100 & sleep 100'` 能杀掉整棵树。
- 超时后工作区不再被继续修改。
- agent 进程被强杀后，子进程不残留（Windows Job Object / Unix 进程组）。
- 取消、超时、spawn 失败分别产生可区分的事件与状态。

### P0-4 原子文件写入与写冲突

**状态：已实现。** 进程内写锁与 OS 文件锁共同串行化跨进程写入。

**现象**

- `write_file` 使用 `std::fs::write`，崩溃会留下截断文件。
- `resolve_for_write` 先 canonicalize 父目录，再 `parent.join(file_name)`；
  最终路径的符号链接替换存在 TOCTOU。
- 并行工具批次可以同时写同一文件。

**危险**

半写文件会破坏仓库；TOCTOU 可以把写入引到 workspace 外；
并行写会让最后一次写入静默覆盖前一次，且 journal 无法解释。

**设计**

1. `WorkspaceFs` 抽象：
   - `write_atomic(path, content, expected_hash)`；
   - `edit(path, expected_hash, old, new)`；
   - 所有写入走同一实现，harness 不直接调用 `std::fs`。
2. 原子写：同目录临时文件 → `fsync` → `rename`；
   Windows 使用 `ReplaceFileW` / `MoveFileEx`；Unix 追加父目录 `fsync`；
   保留权限位，拒绝写入符号链接/junction。
3. 路径校验：canonicalize 根目录；拒绝绝对路径和 `..`；
   打开文件后用句柄再次校验最终路径；对最终组件拒绝符号链接。
4. 冲突检测：`edit_file` 的审批 digest 绑定**原始内容 hash**；
   执行时内容变化则失败，而不是盲目替换。
5. 并行写：同一路径串行化（进程内 per-path lock），或使用内容 hash 的
   compare-and-swap；跨进程写冲突必须显式报错。
6. 编码：保留 BOM/CRLF；拒绝二进制；限制单文件大小；写入前检查磁盘空间。
7. Git：禁止交互式命令；设置 `GIT_TERMINAL_PROMPT=0`、`GIT_OPTIONAL_LOCKS=0`
   等；避免并行写 index；不自动 commit/push。

**验收**

- 模拟写入中途崩溃，文件要么是旧内容，要么是新内容，不存在半截。
- 符号链接替换测试无法把写入引到 workspace 外。
- 两个并行写同一路径，一个成功一个得到明确冲突错误。
- `edit_file` 在内容已变化时拒绝执行。

---

## 5. P1：可运维与可证明

### P1-1 超时、取消与未知的统一分类

需要分别定义：

- 模型：连接超时、首字节/idle 超时、总超时；
- 工具：执行超时、终止宽限、等待回收上限；
- Run：总墙钟 deadline；
- 审批：TTL；
- Lease：TTL 与续租周期。

规则：

- 只要外部动作**可能已经发生**，失败就是 `OutcomeUnknown`；
- `OutcomeUnknown` 只有 `ReadOnly` 或明确可对账的 Effect 能自动重试；
- 取消导致的未执行是 `CancelledBeforeDispatch`，不是 `Failed`；
- 取消不能发生在 journal 提交中途；提交前后的边界必须明确；
- 所有超时与取消都要有事件、原因和日志。

### P1-2 SQLite 并发、迁移与 journal 增长

已有 WAL、`busy_timeout`、`synchronous=NORMAL`、`user_version`。仍需：

- 每个进程一个写连接，事务尽量短，**不要把事务跨 await 持有**；
- 多进程竞争时显式重试 + 抖动，不依赖 SQLite 锁承担业务语义；
- 关键终态提交考虑提高同步级别；
- 迁移前备份；遇到更高 schema 版本拒绝打开；
- retention/GC：已完成 Run 的事件、快照、过期审批、旧 outbox 行；
- journal 中可能含工具参数/输出，写入前做 secret 脱敏；
- GC 不能破坏可恢复性：只清理已终态且超过保留期的 Run。

### P1-3 审批绑定结构化命令

- `deny_commands` 子串匹配不是安全边界，容易被引号、变量展开、别名、PowerShell 绕过；
- 审批请求应绑定 `CommandSpec` 全字段（program、argv、cwd、env、timeout），
  预览展示同一份 spec，执行时不再重新解析；
- digest 覆盖 spec，避免“批准的是 A，执行的是 B”；
- `--yes` 默认只覆盖 `ReadOnly` / `Idempotent`；`NonIdempotent` 使用独立开关；
- actor 目前是声明值；如要作为审计依据，需要外部身份来源或明确标注“未验证”。

### P1-4 可观测性、故障注入与 Windows 覆盖

- 日志字段：`run_id`、`effect_id`、`call_id`、`turn`、`attempt`、`claim_id`；
- 指标：run 时长、Effect 结果分布、重试/未知/取消次数、审批等待、lease 回收；
- `autractl doctor`：检查 journal、schema、workspace 权限、shell、磁盘、daemon 实例；
- 故障注入：commit 前/后、外部 Effect 前/后、SQLite busy、spawn 失败、fsync 失败、
  owner 被杀、lease 过期；
- 多进程并发测试；长于 lease 的 Effect 测试；
- Windows CI：路径、编码、Job Object、CRLF、文件锁。

---

## 6. P2：资源与长期扩展

- 资源限制：内存、进程数、CPU、磁盘；Windows Job Object / Unix rlimit；
- 网络策略：无沙箱下无法真正阻断，只能提示、记录或提供离线模式；
- `CommandRunner` / `WorkspaceFs` / `ProcessSupervisor` 落地，为未来 sandbox 留接口；
- UI/文档明确“无沙箱、同用户权限”，不把 workspace 约束说成安全隔离。

---

## 7. 目标架构

```text
core (sans-I/O)
  EffectClass / EffectStatus / CancelReason / TimeoutKind
        |
        v
execution (contracts)
  ModelProvider / Tool / Journal / Policy / Approval / Session
  CommandRunner / WorkspaceFs / ProcessSupervisor   <- 新增
        |
        v
runtime
  RunLoop -> EffectExecutor -> Dispatcher
  LeaseManager / RecoveryService / RunHub
        |
        v
local host (adapter)
  process groups / job objects / atomic fs / sqlite
        |
        v
harness
  ReAct / tools -> 只通过 Host 契约执行副作用
```

关键约束：

1. **core 不引入 I/O**；只定义语义和分类。
2. **harness 工具不直接调用 `std::fs` / `tokio::process`**；
   所有副作用经 `WorkspaceFs` / `CommandRunner`。
3. **runtime 不实现 OS 细节**；由 LocalHost 适配。
4. **journal 写入只有一个所有权模型**：单进程服务独占，或多进程 + lease/fencing。
   不支持“多进程但只靠 SQLite 锁”的中间态。
5. **恢复是 runtime 服务，不是第二个执行循环**：daemon 只调用与前台相同的
   `resume` / `reconcile`，不另写一套逻辑。

### 写者模型选择

| 方案 | 优点 | 代价 | 建议 |
|---|---|---|---|
| A. 单一本地服务独占 journal | 简单、无重复 owner | 需要 `serve`/daemon 进程与 IPC | 长期目标 |
| B. 多进程直接写 journal + per-run lease/fencing | 保留现有 CLI 用法 | 必须完整实现 P0-1/P0-2 | 近期可行 |

在 P0-1/P0-2 完成前，不应同时运行多个 daemon/outbox 进程写同一个 journal。

---

## 8. 实施顺序

**P0（先做）**

1. Effect ownership：lease 续租、claim fencing、所有写入校验、终态 CAS。
2. Recovery daemon：单实例、Run 心跳、只接管 orphan、配置身份校验、审批策略。
3. 工具取消与进程树：`CommandSpec`、进程组/Job Object、取消原因、强制终止。
4. 原子写与冲突：`WorkspaceFs`、temp+rename、符号链接校验、per-path 冲突检测。

**P1（可证明）**

5. 超时/取消/未知分类统一。
6. 崩溃与多进程故障注入、Windows CI。
7. journal retention/GC、metrics、doctor。
8. 审批绑定结构化命令 spec。

**P2（扩展）**

9. 资源限制。
10. Host 抽象落地，为 sandbox 留接口。
11. 网络与“无沙箱”UX 提示。

依赖关系：P1 的故障注入依赖 P0-1/P0-2；P2 的 sandbox 依赖 P0-3/P0-4 的 Host 抽象。
不要在 P0 完成前引入更多执行路径。

---

## 9. ADR 待办

1. 本地无沙箱模式的信任模型与对外承诺。
2. Effect ownership、lease 续租与写入 fencing。
3. 命令执行契约：`CommandSpec`、进程树、取消、超时、输出上限。
4. workspace 原子写与写冲突策略。
5. recovery daemon 的选举、心跳与接管规则。
6. 超时、取消、`OutcomeUnknown` 的分类学。
7. journal retention、schema 迁移与备份策略。

---

## 10. 开放问题

- 后台 daemon 是否允许自动审批 `ReadOnly` Effect，还是全部停在审批？
- 恢复时 Agent 配置不一致：拒绝恢复、隔离，还是允许显式覆盖？
- lease 策略选择“TTL > 最大执行时间”还是“短 TTL + 强制续租”？
- 默认是否禁用 shell，只允许 argv allowlist？
- journal 的默认保留期与 GC 触发方式？
- Windows 上是否强制使用 Job Object，还是保留 `taskkill` 兼容路径？
