# Runtime scalability baseline

This measures the implementation from PR #1, before any shared-worker or
notification-frequency change. Production runtime code is unchanged.

## Method

The `scalability` bench runs real Agent, Coordinator, owned Executor and Journal
code with deterministic in-process model and tool implementations. Each Run
accepts one model-generated tool call, completes that tool and accepts a final
model response: three Effects and the normal committed transitions. No paid
model provider, external network or sandbox is involved.

Cases are sequential, each in a fresh release-mode process, with four Tokio
workers, at most 32 blocking threads, Model/Tool quotas of 100 and a Process
quota of 8. The scheduler wait queue is deliberately raised to at least the
number of Runs; the default 1,024-waiter admission limit is not a promise to
accept 10,000 simultaneous Run requests.

Three scenarios:

- `idle`: all Runs wait in the existing blocking approval path. Outbox is empty
  after the initial model results. Coordinators wait on approval; their Inbox
  polling is not active. Per-Run executors still inspect Outbox.
- `mixed`: exactly 100 tools have entered an asynchronous I/O gate; the other
  Runs wait on approval. The 100 active tools hold scheduler permits; their
  Coordinators poll Inbox while they execute.
- `throughput`: approvals and I/O gates are disabled. Measure complete lifecycle
  throughput including startup, journal writes and final result acceptance.

The idle/mixed sample starts only when **every** Run has reached its expected
wait, then lasts at least three seconds. Startup and completion after releasing
all waits are reported separately. Exactly two model calls and one successful
tool call per Run are asserted. Failed Runs, missing readiness and timeouts
invalidate a case rather than contributing successful throughput.

Memory and SQLite are separate cases. SQLite uses a fresh database and the
normal WAL/NORMAL configuration; one shared SqliteEffectJournal instance with
its normal connection management is used per process. This is a single-node
baseline, not a multi-process/database contention test.

Counters wrap the Journal API and forward all methods. They count API calls,
including Outbox peeks, Inbox reads and cancellation checks. They **do not count
individual SQL statements**, internal statements within one Journal method or
ApprovalStore access. The atomic counters add some overhead. SQL tracing and
profiling are follow-up measurements if choosing database optimizations.

CPU is Linux process user+system time from `/proc/self/stat`, with CLK_TCK
queried from the host. 100% means one CPU core; it is not percent of total host
capacity. RSS and peak RSS come from `/proc/self/status` and include the whole
process: RunState, histories, Journal, channels, tasks, stacks and allocator
retention. RSS is not an isolated measurement of Executor Task memory. Each
case has a fresh process, preventing previous larger cases from retaining
allocator memory. Non-Linux process metrics are null rather than estimates.

## Reproduction

```sh
python scripts/benchmark_scalability.py \
  --output pooled.jsonl --repeats 3
python scripts/benchmark_scalability.py \
  --inline --scenarios idle --output inline.jsonl --repeats 3
```

Individual case:

```sh
cargo bench -p autra --bench scalability -- \
  --runs 10000 --backend memory --scenario mixed \
  --active 100 --sample-secs 3 --pooled true
```

The runner builds once, launches cases sequentially, flushes one JSON record
per case and refuses to overwrite existing output. The runtime revision,
toolchain, operating system and command are recorded in the first line.

## Results

Measurements are in progress. Raw completed cases are retained in
`2026-11-26-pooled.jsonl`; results and interpretation will be completed after
all cases and the inline control finish.
