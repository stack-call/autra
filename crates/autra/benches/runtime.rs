//! Runtime benchmarks. `harness = false` so the suite runs without extra
//! dependencies: `cargo bench -p autra`.
//!
//! Reports:
//! 1. per-step latency for a single Run with a growing tool chain;
//! 2. journal file growth for the same Run;
//! 3. throughput for concurrent Runs sharing one Agent.

use std::sync::Arc;
use std::time::{Duration, Instant};

use autra::{Agent, MemoryJournal, RunOutcome};
use autra_core::{ModelOutput, ToolCallRequest};
use autra_durable_sqlite::SqliteEffectJournal;
use autra_harness::{EchoModel, EchoTool, FakeModel};

fn tool_chain(steps: usize) -> FakeModel {
    let mut outputs = Vec::with_capacity(steps + 1);
    for index in 0..steps {
        outputs.push(ModelOutput {
            usage: None,
            text: None,
            tool_calls: vec![ToolCallRequest {
                call_id: format!("call-{index}"),
                name: "echo".to_owned(),
                arguments: format!(r#"{{"text":"step-{index}"}}"#),
            }],
        });
    }
    outputs.push(ModelOutput {
        usage: None,
        text: Some("done".to_owned()),
        tool_calls: Vec::new(),
    });
    FakeModel::new(outputs)
}

fn chain_agent(steps: usize, journal: Option<SqliteEffectJournal>) -> Agent {
    chain_agent_with(steps, journal, false)
}

fn chain_agent_with(steps: usize, journal: Option<SqliteEffectJournal>, pooled: bool) -> Agent {
    let builder = Agent::builder("bench")
        .model(tool_chain(steps))
        .tool(EchoTool)
        .max_turns(steps as u32 + 2)
        .max_tool_calls(steps as u32 + 2)
        .pooled_execution(pooled);
    let builder = match journal {
        Some(journal) => builder.journal(journal),
        None => builder,
    };
    builder.build().unwrap()
}

fn summarize(name: &str, steps: usize, elapsed: Duration, bytes: Option<u64>) {
    let per_step = elapsed / steps as u32;
    match bytes {
        Some(bytes) => println!(
            "{name:<28} steps={steps:<4} total={:>8.2?} per_step={:>8.2?} journal={:>7} KiB",
            elapsed,
            per_step,
            bytes / 1024
        ),
        None => println!(
            "{name:<28} steps={steps:<4} total={:>8.2?} per_step={:>8.2?}",
            elapsed, per_step
        ),
    }
}

async fn single_run_in_memory(steps: usize) -> Duration {
    let agent = chain_agent(steps, None);
    let started = Instant::now();
    let outcome: RunOutcome = agent.run("go").await.unwrap();
    assert_eq!(outcome.answer(), Some("done"));
    started.elapsed()
}

async fn single_run_with_journal(steps: usize, pooled: bool) -> (Duration, u64) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bench.db");
    let journal = SqliteEffectJournal::open(&path).unwrap();
    let agent = chain_agent_with(steps, Some(journal), pooled);
    let started = Instant::now();
    let outcome = agent.run("go").await.unwrap();
    assert_eq!(outcome.answer(), Some("done"));
    let elapsed = started.elapsed();
    let bytes = std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
    (elapsed, bytes)
}

async fn concurrent_runs(runs: usize) -> Duration {
    let agent = Arc::new(
        Agent::builder("bench-concurrent")
            .model(EchoModel)
            .build()
            .unwrap(),
    );
    let started = Instant::now();
    let mut tasks = tokio::task::JoinSet::new();
    for index in 0..runs {
        let agent = agent.clone();
        tasks.spawn(async move { agent.run(&format!("run-{index}")).await });
    }
    while let Some(result) = tasks.join_next().await {
        result.unwrap().unwrap();
    }
    started.elapsed()
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    println!("== single Run, in-memory journal ==");
    for steps in [10usize, 50, 100] {
        let elapsed = single_run_in_memory(steps).await;
        summarize("memory", steps, elapsed, None);
    }

    println!("== single Run, SQLite journal (inline) ==");
    for steps in [10usize, 50, 100] {
        let (elapsed, bytes) = single_run_with_journal(steps, false).await;
        summarize("sqlite-inline", steps, elapsed, Some(bytes));
    }

    println!("== single Run, SQLite journal (pooled executor) ==");
    for steps in [10usize, 50, 100] {
        let (elapsed, bytes) = single_run_with_journal(steps, true).await;
        summarize("sqlite-pooled", steps, elapsed, Some(bytes));
    }

    println!("== concurrent Runs (echo model) ==");
    for runs in [10usize, 100] {
        let elapsed = concurrent_runs(runs).await;
        println!(
            "{:<28} runs={runs:<4} total={:>8.2?} per_run={:>8.2?}",
            "concurrent",
            elapsed,
            elapsed / runs as u32
        );
    }

    let _ = MemoryJournal::new();
}
