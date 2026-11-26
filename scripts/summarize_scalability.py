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


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("files", nargs="+", type=Path)
    args = parser.parse_args()
    groups = defaultdict(list)
    seen = set()
    for path in args.files:
        for line in path.read_text(encoding="utf-8").splitlines():
            row = json.loads(line)
            if row.get("kind") == "metadata":
                continue
            group = (row["pooled"], row["backend"], row["runs"], row["scenario"])
            identity = (*group, row["repeat"])
            if identity in seen:
                raise ValueError(f"duplicate case: {identity}")
            seen.add(identity)
            groups[group].append(row)
    print("### Waiting scenarios\n")
    print("Medians of available steady samples, including samples whose subsequent drain failed. CPU cores are process CPU seconds / elapsed seconds.\n")
    print("| Mode | Journal | Runs | Scenario | Steady samples | CPU cores | RSS MiB | Journal API calls/s | Fully completed |")
    print("|---|---|---:|---|---:|---:|---:|---:|---:|")
    for (pooled, backend, runs, scenario), attempts in sorted(groups.items(), reverse=True):
        if scenario == "throughput":
            continue
        samples = []
        for attempt in attempts:
            if attempt.get("steady"):
                samples.append(attempt["steady"])
            elif attempt.get("kind") == "failure":
                samples.extend(d["steady"] for d in attempt.get("diagnostics", []) if d.get("steady"))
        completed = sum(r.get("kind") != "failure" for r in attempts)
        print(f"| {'pooled' if pooled else 'inline'} | {backend} | {runs:,} | {scenario} | {len(samples)}/{len(attempts)} | {metric(samples, 'cpu_percent_one_core', 100, 2)} | {metric(samples, 'rss_kib', 1024)} | {metric(samples, 'journal_calls_per_second', decimals=0)} | {completed}/{len(attempts)} |")
    print("\n### Full lifecycle throughput\n")
    print("Successful-case medians; every Run completes two model Effects and one tool Effect. These are synthetic providers, not external API throughput.\n")
    print("| Journal | Runs | Fully completed | Run/s | Effect/s | Run/s min–max |")
    print("|---|---:|---:|---:|---:|---:|")
    for (_, backend, runs, scenario), attempts in sorted(groups.items()):
        if scenario != "throughput":
            continue
        success = [r for r in attempts if r.get("kind") != "failure"]
        values = [r["completed_runs_per_second"] for r in success]
        bounds = f"{min(values):,.0f}–{max(values):,.0f}" if values else "—"
        print(f"| {backend} | {runs:,} | {len(success)}/{len(attempts)} | {metric(success, 'completed_runs_per_second', decimals=0)} | {metric(success, 'completed_runs_per_second', 1/3, 0)} | {bounds} |")
    failures = [(key, r) for key, attempts in groups.items() for r in attempts if r.get("kind") == "failure"]
    print(f"\nTotal: {len(seen)-len(failures)} successful cases, {len(failures)} failed cases.\n")
    if failures:
        print("### Failed attempts\n")
        print("| Mode | Journal | Runs | Scenario | Repeat | Observed error |")
        print("|---|---|---:|---|---:|---|")
        for (pooled, backend, runs, scenario), row in sorted(failures, key=lambda item: (*item[0], item[1]["repeat"])):
            stderr = row["stderr"]
            error = "Run lease lost" if "lease was lost" in stderr else "database is locked" if "database is locked" in stderr else "See raw failure record"
            print(f"| {'pooled' if pooled else 'inline'} | {backend} | {runs:,} | {scenario} | {row['repeat']} | {error} |")


if __name__ == "__main__":
    main()
