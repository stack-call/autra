#!/usr/bin/env python3
"""Summarize raw scalability cases without hiding failed attempts."""
import argparse
from collections import defaultdict
import json
from pathlib import Path
from statistics import median


def metric(rows, key, divisor=1, decimals=1):
    values = [row[key] / divisor for row in rows if row.get(key) is not None]
    return f"{median(values):,.{decimals}f}" if values else "—"


def release_mode(batch, interval):
    return f"{batch:,} / {interval} ms" if batch else "all"


def case_key(row, batch_default, interval_default):
    return (
        row["pooled"], row["backend"], row["runs"], row["scenario"],
        row.get("release_batch", batch_default),
        row.get("release_interval_ms", interval_default),
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("files", nargs="+", type=Path)
    args = parser.parse_args()
    groups = defaultdict(list)
    seen = set()
    for path in args.files:
        batch = interval = 0
        for line in path.read_text(encoding="utf-8").splitlines():
            row = json.loads(line)
            if row.get("kind") == "metadata":
                batch = row.get("release_batch", 0)
                interval = row.get("release_interval_ms", 0)
                continue
            group = case_key(row, batch, interval)
            if not group[-2]:
                group = (*group[:-1], 0)
            identity = (*group, row["repeat"])
            if identity in seen:
                raise ValueError(f"duplicate case: {identity}")
            seen.add(identity)
            groups[group].append(row)
    print("### Waiting scenarios\n")
    print("Medians of available steady samples, including samples whose subsequent drain failed. CPU cores are process CPU seconds / elapsed seconds.\n")
    print("| Mode | Journal | Runs | Scenario | Release | Steady samples | CPU cores | RSS MiB | Journal API calls/s | Fully completed |")
    print("|---|---|---:|---|---|---:|---:|---:|---:|---:|")
    for (pooled, backend, runs, scenario, batch, interval), attempts in sorted(groups.items(), reverse=True):
        if scenario == "throughput":
            continue
        samples = []
        for attempt in attempts:
            if attempt.get("steady"):
                samples.append(attempt["steady"])
            elif attempt.get("kind") == "failure":
                samples.extend(d["steady"] for d in attempt.get("diagnostics", []) if d.get("steady"))
        completed = sum(r.get("kind") != "failure" for r in attempts)
        print(f"| {'pooled' if pooled else 'inline'} | {backend} | {runs:,} | {scenario} | {release_mode(batch, interval)} | {len(samples)}/{len(attempts)} | {metric(samples, 'cpu_percent_one_core', 100, 2)} | {metric(samples, 'rss_kib', 1024)} | {metric(samples, 'journal_calls_per_second', decimals=0)} | {completed}/{len(attempts)} |")
    print("\n### Full lifecycle throughput\n")
    print("Successful-case medians. Every completed Run executes two model Effects and one tool Effect. These are synthetic providers, not external API throughput.\n")
    print("| Mode | Journal | Runs | Fully completed | Run/s | Effect/s | Run/s min–max |")
    print("|---|---|---:|---:|---:|---:|---:|")
    for (pooled, backend, runs, scenario, _, _), attempts in sorted(groups.items()):
        if scenario != "throughput":
            continue
        success = [r for r in attempts if r.get("kind") != "failure"]
        values = [r["completed_runs_per_second"] for r in success]
        bounds = f"{min(values):,.0f}–{max(values):,.0f}" if values else "—"
        print(f"| {'pooled' if pooled else 'inline'} | {backend} | {runs:,} | {len(success)}/{len(attempts)} | {metric(success, 'completed_runs_per_second', decimals=0)} | {metric(success, 'completed_runs_per_second', 1/3, 0)} | {bounds} |")
    failures = [(key, r) for key, attempts in groups.items() for r in attempts if r.get("kind") == "failure"]
    print(f"\nTotal: {len(seen)-len(failures)} successful cases, {len(failures)} failed cases.\n")
    if any("latencies" in row for attempts in groups.values() for row in attempts):
        print("\n### Queue and lease timing\n")
        print("Values are medians of the per-attempt p95 upper bounds in milliseconds. A dash indicates no samples.")
        print("\n| Mode | Journal | Runs | Scenario | Release | Completed Runs | Transition queue | SQLite wait | SQLite writer hold | Renewal lag | SQLite lock errors |")
        print("|---|---|---:|---|---|---:|---:|---:|---:|---:|---:|")
        for (pooled, backend, runs, scenario, batch, interval), attempts in sorted(groups.items()):
            if not any("latencies" in row for row in attempts):
                continue
            keys = ("transition_queue_wait_us", "sqlite_writer_wait_us", "sqlite_writer_hold_us", "renewal_end_lag_us")
            timings = [
                metric([row.get("latencies", {}).get(key, {}) for row in attempts], "p95_upper", 1000, 3)
                for key in keys
            ]
            completed = sum(row.get("completed_runs", 0) for row in attempts)
            locked = sum(row.get("run_errors", {}).get("sqlite_locked", [0])[0] for row in attempts)
            total = runs * len(attempts)
            print(f"| {'pooled' if pooled else 'inline'} | {backend} | {runs:,} | {scenario} | {release_mode(batch, interval)} | {completed:,}/{total:,} | {' | '.join(timings)} | {locked:,} |")
    if failures:
        print("\n### Failed attempts\n")
        print("| Mode | Journal | Runs | Scenario | Release | Repeat | Observed error |")
        print("|---|---|---:|---|---|---:|---|")
        for (pooled, backend, runs, scenario, batch, interval), row in sorted(failures, key=lambda item: (*item[0], item[1]["repeat"])):
            stderr = row.get("stderr", "")
            run_errors = row.get("run_errors", {})
            if run_errors:
                error = ", ".join(f"{kind} ({count:,})" for kind, (count, _) in sorted(run_errors.items()))
            elif not row.get("drained", True):
                error = "Drain timed out"
            elif not row.get("reached_gate", True):
                error = "Readiness gate never reached"
            elif "lease was lost" in stderr:
                error = "Run lease lost"
            elif "database is locked" in stderr:
                error = "database is locked"
            else:
                error = "See raw failure record"
            print(f"| {'pooled' if pooled else 'inline'} | {backend} | {runs:,} | {scenario} | {release_mode(batch, interval)} | {row['repeat']} | {error} |")


if __name__ == "__main__":
    main()
