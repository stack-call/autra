#!/usr/bin/env python3
"""Run scalability cases sequentially in fresh processes; preserve raw JSONL."""
import argparse
import json
import os
from pathlib import Path
import platform
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--runs", type=int, nargs="+", default=[1000, 5000, 10000])
    parser.add_argument("--backends", nargs="+", choices=["memory", "sqlite"], default=["memory", "sqlite"])
    parser.add_argument("--scenarios", nargs="+", choices=["idle", "mixed", "throughput"], default=["idle", "mixed", "throughput"])
    parser.add_argument("--repeats", type=int, default=3)
    parser.add_argument("--sample-secs", type=float, default=3)
    parser.add_argument("--active", type=int, default=100)
    parser.add_argument("--inline", action="store_true", help="Run the inline control instead of pooled execution")
    args = parser.parse_args()
    if args.repeats < 1 or any(n < 1 for n in args.runs):
        parser.error("runs and repeats must be positive")
    # Cargo emits the exact executable; do not select an arbitrary stale binary.
    build = subprocess.run(["cargo", "bench", "-p", "autra", "--bench", "scalability", "--no-run", "--message-format=json"], cwd=ROOT, check=True, text=True, stdout=subprocess.PIPE)
    artifacts = [json.loads(line) for line in build.stdout.splitlines() if line.startswith("{")]
    executables = [a["executable"] for a in artifacts if a.get("reason") == "compiler-artifact" and a.get("target", {}).get("name") == "scalability" and a.get("executable")]
    executable = executables[-1]
    args.output.parent.mkdir(parents=True, exist_ok=True)
    metadata = {
        "kind": "metadata", "runtime_revision": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(),
        "worktree_dirty": bool(subprocess.check_output(["git", "status", "--porcelain"], cwd=ROOT)),
        "rustc": subprocess.check_output(["rustc", "--version"], text=True).strip(),
        "platform": platform.platform(), "visible_cpus": os.cpu_count(),
        "command": sys.argv, "query_metric": "Journal API calls; excludes ApprovalStore; not SQL statement count",
    }
    # Refuse to overwrite existing measurements accidentally.
    with args.output.open("x", encoding="utf-8") as output:
        output.write(json.dumps(metadata) + "\n")
        output.flush()
        for repeat in range(1, args.repeats + 1):
            for backend in args.backends:
                for runs in args.runs:
                    for scenario in args.scenarios:
                        command = [executable, "--runs", str(runs), "--backend", backend, "--scenario", scenario, "--active", str(args.active), "--sample-secs", str(args.sample_secs), "--pooled", str(not args.inline).lower()]
                        print(f"repeat={repeat} backend={backend} runs={runs} scenario={scenario} pooled={not args.inline}", flush=True)
                        result = subprocess.run(command, cwd=ROOT, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=420)
                        if result.returncode:
                            output.write(json.dumps({"kind": "failure", "repeat": repeat, "backend": backend, "runs": runs, "scenario": scenario, "pooled": not args.inline, "returncode": result.returncode, "stderr": result.stderr}) + "\n")
                            output.flush()
                            raise RuntimeError(result.stderr)
                        row = json.loads(result.stdout)
                        row["repeat"] = repeat
                        output.write(json.dumps(row) + "\n")
                        output.flush()
                        print(json.dumps(row), flush=True)


if __name__ == "__main__":
    main()
