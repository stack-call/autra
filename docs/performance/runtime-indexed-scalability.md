# Indexed runtime scalability on Windows

The indexed pooled runtime completed 53 of 54 measured cases after repeating
three attempts that ran out of temporary disk space. At 10,000 Runs, all nine
pooled SQLite cases completed. The 5,000-Run mixed SQLite case that hit
`database is locked` completed 4,999 of 5,000 Runs. The lock error remains
in the measured totals.

## Method

Runs exercise the normal Agent, Coordinator, Executor and Journal with synthetic
in-process model and tool implementations. Each successful Run completes two
model effects and one tool effect. Memory and SQLite are measured separately at
1,000, 5,000 and 10,000 concurrent Runs. One case runs per fresh process, with
four Tokio workers, 32 blocking threads and quotas of 100 Model and 100 Tool
effects. The host was Windows 11 with an i5-11400H, 12 logical processors and
approximately 16 GiB of RAM. Production machine and workload limits depend on
the actual model, tool and environment latency.

- Pooled covers `idle`, `mixed` and `throughput` with three measured repeats.
  The idle case blocks on approval. The mixed case gates 100 tool Effects
  and blocks the rest on approval. A three-second steady window starts after all Runs reach the
  gate. Gated cases time the drain from the first release. The throughput
  case runs without gates and times the whole lifecycle.
- The inline control runs `idle` and `throughput` on both Journals and at all
  three Run counts. Both modes use the same quotas and data contracts.
- The paced-release comparison repeats the pooled SQLite 10,000-Run idle and
  mixed cases, separately releasing 50 Runs every 250 ms. Drain time includes
  the time spent releasing batches.
- The benchmark records Windows process CPU time and working set. Writer wait
  is time to acquire the SQLite writer lock. Writer hold includes all work
  performed under that lock. Transition queue delay measures wait for Tokio's
  blocking pool. Renewal lag is measured from the scheduled renewal due time.
  A dash indicates the timer was never sampled. SQLite lock errors count
  errors reported by Runs. Raw records include the sample counts, maxima and
  separate start and completion times for renewal lag.

The initial measurements ran at `21bb4e1`. The retries, resumed inline control
and paced-release cases ran at `f5f9237`. The follow-up revision changes
recovery scanning, subprocess environment filtering and database directory
cleanup. The workload, indexed Outbox, execution modes and latency probes are
the same. The temporary database moved from C: to E: after three SQLite
10,000-Run cases encountered a full disk on the third repeat. Their raw
records are retained. Tables set those host failures aside and use the
replacement attempts for those three cases. The inline sequence was interrupted
after its first eight results and resumed with the outstanding cases.

The previous baseline ran on a different Linux host. Measuring the improvement
attributable to the index requires a same-host baseline.

## Interpretation

Inline completed 36/36 cases. Paced release completed 6/6 cases. Across the
three matrices, 539,999 of 540,000 Runs completed in 95 fully successful cases
and one case with a SQLite lock error. No measured case reported lease loss.

At 10,000 Runs, both modes completed all throughput repeats. Pooled SQLite
had lower throughput and higher idle cost than the inline control on this
host. Inline remains the default.

| Journal | Mode | Throughput (Run/s) | Idle CPU (cores) | Idle RSS (MiB) | Idle Journal API calls/s |
| --- | --- | ---: | ---: | ---: | ---: |
| Memory | Inline | 5,934 | 0.00 | 335.3 | 9,964 |
| Memory | Pooled | 4,742 | 0.08 | 385.0 | 29,940 |
| SQLite | Inline | 291 | 0.11 | 259.8 | 9,494 |
| SQLite | Pooled | 210 | 0.48 | 304.3 | 29,879 |

The paced comparison traded longer end-to-end drains for lower tail latency in
Run renewal. The renewal column is the median of the three approximate p95
upper bounds from the benchmark histograms.

| Pooled SQLite, 10,000 Runs | Release | Complete cases | Drain (s) | Renewal lag p95 (s) |
| --- | --- | ---: | ---: | ---: |
| Idle | All | 3/3 | 33.13 | 18.71 |
| Idle | 50 per 250 ms | 3/3 | 52.19 | 1.05 |
| Mixed | All | 3/3 | 35.26 | 4.19 |
| Mixed | 50 per 250 ms | 3/3 | 52.15 | 1.05 |

The results support testing bounded admission for concentrated completions.
The remaining SQLite lock error and the difference between writer wait and
renewal lag call for finer diagnostics before changing worker topology. The
shared-pool comparison should use the same workload and report both successful
completion and renewal tail latency.

## Raw results

- [Pooled](2026-11-26-indexed-pooled.jsonl)
- [Inline control](2026-11-26-indexed-inline.jsonl)
- [Paced SQLite release](2026-11-26-indexed-batched.jsonl)

Regenerate the tables from the repository root.

```sh
python scripts/summarize_scalability.py --exclude-disk-full \
  docs/performance/2026-11-26-indexed-pooled.jsonl \
  docs/performance/2026-11-26-indexed-inline.jsonl \
  docs/performance/2026-11-26-indexed-batched.jsonl
```
