//! Real Agent/Coordinator/Executor benchmark; no production scheduling changes.
//! Each invocation runs one case in a fresh process and emits one JSON record.
#[path = "scalability/metered.rs"]
mod metered;
use async_trait::async_trait;
use autra::{Agent, MemoryJournal, ModelError, ModelProvider, SchedulerLimits};
use autra_core::{EffectClass, HistoryItem, ModelCallSpec, ModelOutput, ToolCallRequest, ToolSpec};
use autra_durable_sqlite::SqliteEffectJournal;
use autra_execution::{
    ApprovalDecision, ApprovalRequest, Approver, DurableTransitionStore, Tool, ToolError,
};
use metered::{Counts, MeteredJournal};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};
use tokio::sync::watch;

#[derive(Default)]
struct Progress {
    ready: AtomicUsize,
    models: AtomicUsize,
    tools: AtomicUsize,
}
#[derive(Clone)]
struct Gate {
    active: usize,
    release: watch::Receiver<bool>,
    progress: Arc<Progress>,
}
impl Gate {
    async fn wait(&self) {
        self.release
            .clone()
            .wait_for(|open| *open)
            .await
            .expect("release sender lost");
    }
}
struct Model(Arc<Progress>);
#[async_trait]
impl ModelProvider for Model {
    async fn complete(&self, request: &ModelCallSpec) -> Result<ModelOutput, ModelError> {
        self.0.models.fetch_add(1, Ordering::Relaxed);
        let done = request
            .messages
            .iter()
            .any(|m| matches!(m, HistoryItem::Tool(_)));
        let index = request
            .messages
            .iter()
            .find_map(|m| match m {
                HistoryItem::User(s) => s.parse::<usize>().ok(),
                _ => None,
            })
            .expect("numeric input");
        Ok(ModelOutput {
            text: done.then(|| "done".into()),
            usage: None,
            tool_calls: if done {
                vec![]
            } else {
                vec![ToolCallRequest {
                    call_id: format!("call-{index}"),
                    name: "bench_tool".into(),
                    arguments: index.to_string(),
                }]
            },
        })
    }
}
struct BenchTool {
    gate: Gate,
    waiting: bool,
}
#[async_trait]
impl Tool for BenchTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "bench_tool".into(),
            description: "benchmark-only I/O gate".into(),
            input_schema: r#"{"type":"integer"}"#.into(),
        }
    }
    fn effect_class(&self) -> EffectClass {
        EffectClass::ReadOnly
    }
    fn requires_approval(&self) -> bool {
        self.waiting
    }
    async fn call(&self, arguments: &str) -> Result<String, ToolError> {
        let index: usize = arguments.parse().expect("numeric tool input");
        self.gate.progress.tools.fetch_add(1, Ordering::Relaxed);
        if self.waiting {
            if index < self.gate.active {
                self.gate.progress.ready.fetch_add(1, Ordering::Release);
            }
            self.gate.wait().await;
        }
        Ok("ok".into())
    }
}
struct Approval(Gate);
#[async_trait]
impl Approver for Approval {
    async fn approve(&self, request: &ApprovalRequest) -> ApprovalDecision {
        let index: usize = request.arguments.parse().expect("numeric approval input");
        if index >= self.0.active {
            self.0.progress.ready.fetch_add(1, Ordering::Release);
            self.0.wait().await;
        }
        ApprovalDecision::Approved
    }
}
// Linux /proc is optional. CPU% means percent of one CPU, not machine capacity.
struct Process {
    clock_ticks: Option<f64>,
}
impl Process {
    fn new() -> Self {
        let clock_ticks = std::process::Command::new("getconf")
            .arg("CLK_TCK")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .and_then(|s| s.trim().parse::<f64>().ok())
            .filter(|n| *n > 0.0);
        Self { clock_ticks }
    }
    fn cpu_seconds(&self) -> Option<f64> {
        let s = std::fs::read_to_string("/proc/self/stat").ok()?;
        let fields: Vec<_> = s.rsplit_once(')')?.1.split_whitespace().collect();
        let user = fields.get(11)?.parse::<u64>().ok()?;
        let system = fields.get(12)?.parse::<u64>().ok()?;
        Some((user + system) as f64 / self.clock_ticks?)
    }
    fn status(&self, key: &str) -> Option<u64> {
        std::fs::read_to_string("/proc/self/status")
            .ok()?
            .lines()
            .find_map(|s| {
                s.strip_prefix(key)
                    .and_then(|s| s.split_whitespace().next()?.parse().ok())
            })
    }
}
struct Config {
    runs: usize,
    active: usize,
    seconds: f64,
    backend: String,
    scenario: String,
    pooled: bool,
}
impl Config {
    fn parse() -> Self {
        let mut values = BTreeMap::new();
        let mut args = std::env::args().skip(1);
        while let Some(key) = args.next() {
            if key == "--bench" {
                continue;
            }
            if key == "--help" {
                println!(
                    "--runs N --backend memory|sqlite --scenario idle|mixed|throughput --active N --sample-secs N --pooled true|false"
                );
                std::process::exit(0);
            }
            assert!(
                [
                    "--runs",
                    "--backend",
                    "--scenario",
                    "--active",
                    "--sample-secs",
                    "--pooled"
                ]
                .contains(&key.as_str()),
                "unknown option: {key}"
            );
            values.insert(key, args.next().expect("missing option value"));
        }
        let get =
            |key: &str, fallback: &str| values.get(key).cloned().unwrap_or_else(|| fallback.into());
        let runs = get("--runs", "1000").parse::<usize>().unwrap();
        let scenario = get("--scenario", "idle");
        let active = if scenario == "mixed" {
            get("--active", "100").parse::<usize>().unwrap().min(runs)
        } else {
            0
        };
        let c = Self {
            runs,
            active,
            scenario,
            backend: get("--backend", "memory"),
            seconds: get("--sample-secs", "3").parse().unwrap(),
            pooled: get("--pooled", "true").parse().unwrap(),
        };
        assert!(c.runs > 0 && c.seconds.is_finite() && c.seconds > 0.0);
        assert!(["memory", "sqlite"].contains(&c.backend.as_str()));
        assert!(["idle", "mixed", "throughput"].contains(&c.scenario.as_str()));
        c
    }
}
async fn run(config: Config, process: Process) {
    let directory = tempfile::tempdir().unwrap();
    let inner: Arc<dyn DurableTransitionStore> = match config.backend.as_str() {
        "memory" => Arc::new(MemoryJournal::new()),
        _ => Arc::new(SqliteEffectJournal::open(directory.path().join("bench.db")).unwrap()),
    };
    let counts = Arc::new(Counts::default());
    let progress = Arc::new(Progress::default());
    let (release, receiver) = watch::channel(false);
    let waiting = config.scenario != "throughput";
    let gate = Gate {
        active: config.active,
        release: receiver,
        progress: progress.clone(),
    };
    let agent = Arc::new(
        Agent::builder("scalability")
            .model(Model(progress.clone()))
            .tool(BenchTool {
                gate: gate.clone(),
                waiting,
            })
            .approver(Approval(gate))
            .journal(MeteredJournal {
                inner,
                counts: counts.clone(),
            })
            .scheduler_limits(
                SchedulerLimits::new(100, 100, 8).with_max_waiters(config.runs.max(1024)),
            )
            .pooled_execution(config.pooled)
            .build()
            .unwrap(),
    );
    let rss_before = process.status("VmRSS:");
    let start = Instant::now();
    let cpu_start = process.cpu_seconds();
    let mut tasks = tokio::task::JoinSet::new();
    for index in 0..config.runs {
        let agent = agent.clone();
        tasks.spawn(async move { agent.run(&index.to_string()).await });
    }
    let mut steady = serde_json::Value::Null;
    let setup_seconds;
    let finish_started;
    if waiting {
        tokio::time::timeout(Duration::from_secs(180), async {
            loop {
                if progress.ready.load(Ordering::Acquire) == config.runs {
                    break;
                }
                // Surface a failed Run during startup instead of waiting for timeout.
                if let Some(result) = tasks.try_join_next() {
                    panic!("Run ended before measurement: {result:?}");
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("Runs did not all reach the measurement gate within 180s");
        setup_seconds = start.elapsed().as_secs_f64();
        assert_eq!(progress.tools.load(Ordering::Relaxed), config.active);
        let before = counts.snapshot();
        let cpu = process.cpu_seconds();
        let sample = Instant::now();
        tokio::time::sleep(Duration::from_secs_f64(config.seconds)).await;
        let seconds = sample.elapsed().as_secs_f64();
        let calls = counts.snapshot().since(before);
        let cpu_seconds = process.cpu_seconds().zip(cpu).map(|(a, b)| a - b);
        steady = json!({"seconds": seconds, "cpu_seconds": cpu_seconds,
            "cpu_percent_one_core": cpu_seconds.map(|s| s/seconds*100.0),
            "rss_kib": process.status("VmRSS:"), "threads": process.status("Threads:"),
            "journal_calls": calls, "journal_calls_per_second": calls.total as f64/seconds,
            "tools_executing": progress.tools.load(Ordering::Relaxed),
            "runs_waiting_approval": config.runs-config.active});
        finish_started = Instant::now();
        release.send(true).unwrap();
    } else {
        setup_seconds = 0.0;
        finish_started = start;
    }
    tokio::time::timeout(Duration::from_secs(180), async {
        let mut completed = 0;
        while let Some(outcome) = tasks.join_next().await {
            let outcome = outcome.expect("Run task panicked").expect("Run failed");
            assert_eq!(outcome.answer(), Some("done"));
            completed += 1;
        }
        assert_eq!(completed, config.runs);
    })
    .await
    .expect("Runs did not complete within 180s");
    let finish_seconds = finish_started.elapsed().as_secs_f64();
    assert_eq!(progress.models.load(Ordering::Relaxed), config.runs * 2);
    assert_eq!(progress.tools.load(Ordering::Relaxed), config.runs);
    println!(
        "{}",
        json!({
            "backend": config.backend, "scenario": config.scenario, "runs": config.runs,
            "pooled": config.pooled, "active": config.active, "tokio_workers": 4,
            "max_blocking_threads": 32, "model_limit": 100, "tool_limit": 100,
            "rss_before_kib": rss_before, "peak_rss_kib": process.status("VmHWM:"),
            "setup_seconds": setup_seconds, "steady": steady,
            "finish_seconds": finish_seconds, "completed_runs_per_second": config.runs as f64/finish_seconds,
            "cpu_total_seconds": process.cpu_seconds().zip(cpu_start).map(|(a,b)| a-b),
            "elapsed_seconds": start.elapsed().as_secs_f64(), "journal_calls_total": counts.snapshot(),
            "model_effects": progress.models.load(Ordering::Relaxed), "tool_effects": progress.tools.load(Ordering::Relaxed),
        })
    );
}
fn main() {
    let config = Config::parse();
    let process = Process::new();
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .max_blocking_threads(32)
        .enable_all()
        .build()
        .unwrap()
        .block_on(run(config, process));
}
