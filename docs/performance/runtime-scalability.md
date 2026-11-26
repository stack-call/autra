# Runtime scalability baseline

This measures the implementation from PR #1, before any shared-worker or
notification-frequency change. Production runtime code is unchanged.

## Method

The `scalability` bench runs real Agent, Coordinator, owned Executor and Journal
code with deterministic in-process model and tool implementations. All Runs
share one Agent definition and its scheduler; this does not allocate 10,000
distinct Agent configurations or large conversation histories. Each Run
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
  --output pooled.jsonl --repeats 3 --continue-on-error
python scripts/benchmark_scalability.py \
  --inline --scenarios idle --output inline.jsonl --repeats 3 --continue-on-error
```

Individual case:

```sh
cargo bench -p autra --bench scalability -- \
  --runs 10000 --backend memory --scenario mixed \
  --active 100 --sample-secs 3 --pooled true
```

The runner builds once, launches cases sequentially, flushes one JSON record
per case and refuses to overwrite existing output. With `--continue-on-error`,
failed cases are retained while the remaining cases run; the runner still
exits nonzero if any case failed. Phase diagnostics preserve steady samples
even when subsequent completion fails. The runtime revision,
toolchain, operating system and command are recorded in the first line.

## Results

All 72 attempts are complete: 67 succeeded and five failed. Every one of the
24 case configurations was attempted three times. The pooled matrix has 54
attempts, including all five failures; all 18 inline control attempts succeeded.
Raw data is kept in:

- `2026-11-26-pooled.jsonl`: initial first-round cases, stopping at the first failure;
- `2026-11-26-pooled-tail.jsonl`: the remaining two first-round cases;
- `2026-11-26-pooled-repeats.jsonl`: complete second and third rounds;
- `2026-11-26-inline.jsonl`: the three-round inline waiting control.

The initial harness did not emit a steady sample when completion failed. Later
cases added phase diagnostics, outside the measurement window, to preserve
those samples. Workload, counters and production runtime were unchanged. This
explains the missing first-round SQLite 10,000-Run idle sample; its failed
completion remains counted.

The production runtime baseline is `de10f6a8b60ff7bf7ee266fd44134152f526ad86`.
Benchmark revisions are recorded in each JSONL header. Measurements used Rust
1.100.0, Linux 6.18.57, glibc 2.41, an AMD EPYC 9V74 host and nine visible
logical CPUs. Effective CPU quota was not available; visible CPUs are not a
capacity guarantee. These are descriptive samples from one environment.

## Interpretation

The data supports optimizing empty polling before introducing a larger worker
architecture. In the fully waiting pooled case, Inbox reads are zero because
Coordinators await approval. Outbox peeks and cancellation reads dominate
Journal traffic despite no executable work. The inline control retains the
same number of live Runs, yet removes the empty Executor loop. Its residual
roughly one Journal cancellation read per Run per second comes from lease
monitoring.

For 10,000 fully waiting Memory Runs, the pooled median was 1.90 CPU cores,
344.8 MiB RSS and 912,200 Journal API calls/s. Inline medians were 0.03 cores,
303.7 MiB and 9,996 calls/s. For SQLite at that size, waiting medians were
1.56 cores and 83,048 calls/s in pooled mode, compared with 0.13 cores and
9,995 calls/s inline. All three SQLite inline cases subsequently completed,
while all three pooled idle cases failed during concentrated completion.
This strengthens the case for reducing polling, but the different modes also
have different execution/commit paths; it is not a controlled experiment of
only one timer or proof of the lock-failure root cause.

This does not isolate the allocation cost of a Tokio Task: pooled mode also
adds channels, worker context and Inbox data. RSS differences describe whole
execution modes. The observed thread counts remain bounded, rather than
tracking the Run count. A shared pool could reduce resource overhead, but
changing Task count alone would not remove Coordinator Inbox polling or Run
lease monitoring.

The SQLite failures are an additional completion/load concern. At 10,000
Runs, all three fully waiting cases reached the wait gate but failed during
concentrated completion with `coordinator run lease was lost`. In round two,
readiness took 15.82 seconds, the steady sample took 3.00 seconds, and the
process exited after 85.90 seconds. The mixed case failed once in three
attempts; the fast throughput case failed once with `database is locked`.
Successful throughput medians must be read alongside these failure counts.

The owner check rejected a commit without a live lease, preserving the fencing
boundary. These runs do not establish a root cause for delayed renewal or
SQLite lock failures. They also do not prove a shared Worker Pool would fix
those errors. Profile writer wait times, synchronous Journal operations on
Tokio workers, renewal delay and Outbox query plans before selecting changes.

Recommended next work:

1. Notify Executor and Coordinator after durable publication, with a bounded,
   low-frequency Journal sweep for missed notifications and recovery. Cover
   both 20ms loops; measure again with the same harness.
2. Protect control-plane progress under burst load: measure lease-renewal
   delay and writer contention, retain admission/backpressure, and investigate
   the observed SQLite failures without weakening ownership checks.
3. Introduce shared bounded workers when the target requires many resident
   Runs and the remaining resource cost warrants it. Approval parking
   (ADR-0060) is a separate way to release resident Coordinator resources.
4. Before cross-node high availability, persist a resolvable Environment
   descriptor/catalog and implement provider reattachment. EffectRecord
   already stores the chosen identity; the Sandbox provider's in-memory
   handle mapping is insufficient for restart. This benchmark uses no Sandbox
   and makes no claim about remote reconnection.

## Measurement tables

Regenerate these tables from the checked-in raw data:

```sh
python scripts/summarize_scalability.py \
  docs/performance/2026-11-26-pooled.jsonl \
  docs/performance/2026-11-26-pooled-tail.jsonl \
  docs/performance/2026-11-26-pooled-repeats.jsonl \
  docs/performance/2026-11-26-inline.jsonl
```

The first failed idle case lacks its steady sample; subsequent failed cases
include phase samples. A failed case is never included in successful throughput
medians. No failed attempts are dropped from completion counts.

### Waiting scenarios

Medians of available steady samples, including samples whose subsequent drain failed. CPU cores are process CPU seconds / elapsed seconds.

| Mode | Journal | Runs | Scenario | Steady samples | CPU cores | RSS MiB | Journal API calls/s | Fully completed |
|---|---|---:|---|---:|---:|---:|---:|---:|
| pooled | sqlite | 10,000 | mixed | 3/3 | 1.59 | 268.1 | 71,604 | 2/3 |
| pooled | sqlite | 10,000 | idle | 2/3 | 1.56 | 267.2 | 83,048 | 0/3 |
| pooled | sqlite | 5,000 | mixed | 3/3 | 1.57 | 140.5 | 66,077 | 3/3 |
| pooled | sqlite | 5,000 | idle | 3/3 | 1.54 | 139.3 | 81,107 | 3/3 |
| pooled | sqlite | 1,000 | mixed | 3/3 | 1.58 | 37.6 | 63,699 | 3/3 |
| pooled | sqlite | 1,000 | idle | 3/3 | 1.42 | 36.9 | 72,678 | 3/3 |
| pooled | memory | 10,000 | mixed | 3/3 | 2.07 | 345.6 | 899,571 | 3/3 |
| pooled | memory | 10,000 | idle | 3/3 | 1.90 | 344.8 | 912,200 | 3/3 |
| pooled | memory | 5,000 | mixed | 3/3 | 0.96 | 176.0 | 461,128 | 3/3 |
| pooled | memory | 5,000 | idle | 3/3 | 0.86 | 175.3 | 466,073 | 3/3 |
| pooled | memory | 1,000 | mixed | 3/3 | 0.26 | 39.8 | 90,213 | 3/3 |
| pooled | memory | 1,000 | idle | 3/3 | 0.24 | 39.0 | 93,747 | 3/3 |
| inline | sqlite | 10,000 | idle | 3/3 | 0.13 | 233.4 | 9,995 | 3/3 |
| inline | sqlite | 5,000 | idle | 3/3 | 0.09 | 121.9 | 4,998 | 3/3 |
| inline | sqlite | 1,000 | idle | 3/3 | 0.04 | 33.0 | 1,000 | 3/3 |
| inline | memory | 10,000 | idle | 3/3 | 0.03 | 303.7 | 9,996 | 3/3 |
| inline | memory | 5,000 | idle | 3/3 | 0.03 | 154.5 | 4,998 | 3/3 |
| inline | memory | 1,000 | idle | 3/3 | 0.02 | 34.5 | 1,000 | 3/3 |

### Full lifecycle throughput

Successful-case medians; every Run completes two model Effects and one tool Effect. These are synthetic providers, not external API throughput.

| Journal | Runs | Fully completed | Run/s | Effect/s | Run/s min–max |
|---|---:|---:|---:|---:|---:|
| memory | 1,000 | 3/3 | 4,023 | 12,069 | 3,837–4,294 |
| memory | 5,000 | 3/3 | 2,044 | 6,132 | 1,980–2,151 |
| memory | 10,000 | 3/3 | 1,602 | 4,807 | 1,484–1,638 |
| sqlite | 1,000 | 3/3 | 458 | 1,375 | 415–466 |
| sqlite | 5,000 | 3/3 | 286 | 857 | 284–301 |
| sqlite | 10,000 | 2/3 | 239 | 717 | 233–245 |

Total: 67 successful cases, 5 failed cases.

### Failed attempts

| Mode | Journal | Runs | Scenario | Repeat | Observed error |
|---|---|---:|---|---:|---|
| pooled | sqlite | 10,000 | idle | 1 | Run lease lost |
| pooled | sqlite | 10,000 | idle | 2 | Run lease lost |
| pooled | sqlite | 10,000 | idle | 3 | Run lease lost |
| pooled | sqlite | 10,000 | mixed | 2 | Run lease lost |
| pooled | sqlite | 10,000 | throughput | 1 | database is locked |

## Validation

- `cargo fmt --all -- --check` passed.
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` passed.
- `cargo test --workspace --all-features` passed all 372 tests.
- Benchmark implementation CI passed on both Linux and Windows.
- All 72 stress attempts and their five overload failures are retained; stress
  failures are distinct from the passing regression suite above.
- Production `src/` files are unchanged from the refactor baseline.
