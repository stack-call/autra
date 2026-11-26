#!/usr/bin/env python3
"""Run scalability cases sequentially in fresh processes; preserve raw JSONL."""
import argparse
import json
import os
from pathlib import Path
import platform
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--runs", type=int, nargs="+", default=[1000, 5000, 10000])
    parser.add_argument("--backends", nargs="+", choices=["memory", "sqlite"], default=["memory", "sqlite"])
    parser.add_argument("--scenarios", nargs="+", choices=["idle", "mixed", "throughput"], default=["idle", "mixed", "throughput"])
    parser.add_argument("--repeats", type=int, default=3)
    parser.add_argument("--repeat-start", type=int, default=1)
    parser.add_argument("--continue-on-error", action="store_true")
    parser.add_argument("--sample-secs", type=float, default=3)
    parser.add_argument("--active", type=int, default=100)
    parser.add_argument("--release-batch", type=int, default=0, help="Number of waiting Runs released per batch; 0 releases all")
    parser.add_argument("--release-interval-ms", type=int, default=10)
    parser.add_argument("--inline", action="store_true", help="Run the inline control instead of pooled execution")
    args = parser.parse_args()
    if args.repeats < 1 or args.repeat_start < 1 or any(n < 1 for n in args.runs):
        parser.error("runs and repeats must be positive")
    if args.release_batch < 0 or args.release_interval_ms < 0:
        parser.error("release batch and interval must be non-negative")
    # Cargo emits the exact executable; do not select an arbitrary stale binary.
    build = subprocess.run(["cargo", "bench", "-p", "autra", "--bench", "scalability", "--no-run", "--message-format=json"], cwd=ROOT, check=True, text=True, stdout=subprocess.PIPE)
    artifacts = [json.loads(line) for line in build.stdout.splitlines() if line.startswith("{")]
    executables = [a["executable"] for a in artifacts if a.get("reason") == "compiler-artifact" and a.get("target", {}).get("name") == "scalability" and a.get("executable")]
    executable = executables[-1]
    args.output.parent.mkdir(parents=True, exist_ok=True)
    metadata = {
        "kind": "metadata", "release_batch": args.release_batch,
        "release_interval_ms": args.release_interval_ms, "runtime_revision": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(),
        "worktree_dirty": bool(subprocess.check_output(["git", "status", "--porcelain"], cwd=ROOT)),
        "rustc": subprocess.check_output(["rustc", "--version"], text=True).strip(),
        "platform": platform.platform(), "visible_cpus": os.cpu_count(),
        "command": sys.argv, "query_metric": "Journal API calls; excludes ApprovalStore; not SQL statement count",
    }
    # Refuse to overwrite existing measurements accidentally.
    failures = 0
    with args.output.open("x", encoding="utf-8") as output:
        output.write(json.dumps(metadata) + "\n")
        output.flush()
        for repeat in range(args.repeat_start, args.repeat_start + args.repeats):
            for backend in args.backends:
                for runs in args.runs:
                    for scenario in args.scenarios:
                        command = [executable, "--runs", str(runs), "--backend", backend, "--scenario", scenario, "--active", str(args.active), "--sample-secs", str(args.sample_secs), "--pooled", str(not args.inline).lower(), "--release-batch", str(args.release_batch), "--release-interval-ms", str(args.release_interval_ms)]
                        print(f"repeat={repeat} backend={backend} runs={runs} scenario={scenario} pooled={not args.inline}", flush=True)
                        started = time.monotonic()
                        try:
                            result = subprocess.run(command, cwd=ROOT, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=420)
                        except subprocess.TimeoutExpired as error:
                            result = subprocess.CompletedProcess(command, 124, "", "Process timed out after 420s\n" + str(error.stderr or ""))
                        wall_seconds = time.monotonic() - started
                        if result.returncode:
                            # Key order is unspecified; accept any JSON stage record.
                            diagnostics = [json.loads(line) for line in result.stderr.splitlines() if line.startswith("{") and "stage" in json.loads(line)]
                            output.write(json.dumps({"wall_seconds": wall_seconds, "diagnostics": diagnostics, "kind": "failure", "repeat": repeat, "backend": backend, "runs": runs, "scenario": scenario, "pooled": not args.inline, "returncode": result.returncode, "stderr": result.stderr}) + "\n")
                            output.flush()
                            failures += 1
                            print(f"FAILED after {wall_seconds:.2f}s: {result.stderr}", flush=True)
                            if not args.continue_on_error:
                                raise RuntimeError(result.stderr)
                            continue
                        row = json.loads(result.stdout)
                        row["repeat"] = repeat
                        if not row["successful"]:
                            row["kind"] = "failure"
                            failures += 1
                        output.write(json.dumps(row) + "\n")
                        output.flush()
                        print(json.dumps(row), flush=True)
                        if not row["successful"] and not args.continue_on_error:
                            raise SystemExit("benchmark case failed; raw result retained")

    if failures:
        raise SystemExit(f"{failures} benchmark cases failed; raw failures retained")


if __name__ == "__main__":
    main()
