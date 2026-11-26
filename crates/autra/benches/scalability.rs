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
use metered::{Counts, Latencies, MeteredJournal};
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
    release: Arc<[watch::Receiver<bool>]>,
    batch_size: usize,
    progress: Arc<Progress>,
}
impl Gate {
    async fn wait(&self, index: usize) {
        self.release[index / self.batch_size]
            .clone()
            .wait_for(|released| *released)
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
            self.gate.wait(index).await;
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
            self.0.wait(index).await;
        }
        ApprovalDecision::Approved
    }
}
// CPU time is summed across threads. Working set is the Windows RSS measure.
#[cfg(not(windows))]
struct Process {
    clock_ticks: Option<f64>,
}
#[cfg(not(windows))]
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
#[cfg(windows)]
struct Process;

#[cfg(windows)]
impl Process {
    fn new() -> Self {
        Self
    }

    fn cpu_seconds(&self) -> Option<f64> {
        use windows_sys::Win32::{
            Foundation::FILETIME,
            System::Threading::{GetCurrentProcess, GetProcessTimes},
        };
        let [mut created, mut exited, mut kernel, mut user] = [FILETIME::default(); 4];
        // The pseudo-handle requires no cleanup; all output pointers are valid.
        let ok = unsafe {
            GetProcessTimes(
                GetCurrentProcess(),
                &mut created,
                &mut exited,
                &mut kernel,
                &mut user,
            )
        };
        let ticks = |t: FILETIME| (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime);
        (ok != 0).then(|| (ticks(kernel) + ticks(user)) as f64 / 10_000_000.0)
    }

    fn status(&self, key: &str) -> Option<u64> {
        use windows_sys::Win32::System::{
            ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS},
            Threading::GetCurrentProcess,
        };
        let mut counters = PROCESS_MEMORY_COUNTERS::default();
        let size = std::mem::size_of_val(&counters) as u32;
        // The counter buffer matches the size expected by the API.
        let ok = unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, size) };
        if ok == 0 {
            return None;
        }
        let bytes = match key {
            "VmRSS:" => counters.WorkingSetSize,
            "VmHWM:" => counters.PeakWorkingSetSize,
            _ => return None,
        };
        Some(bytes as u64 / 1024)
    }
}

struct Config {
    runs: usize,
    active: usize,
    seconds: f64,
    backend: String,
    scenario: String,
    pooled: bool,
    release_batch: usize,
    release_interval: Duration,
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
                    "--runs N --backend memory|sqlite --scenario idle|mixed|throughput --active N --sample-secs N --pooled true|false --release-batch N --release-interval-ms N"
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
                    "--pooled",
                    "--release-batch",
                    "--release-interval-ms"
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
            release_batch: get("--release-batch", "0").parse().unwrap(),
            release_interval: Duration::from_millis(
                get("--release-interval-ms", "10").parse().unwrap(),
            ),
        };
        assert!(c.runs > 0 && c.seconds.is_finite() && c.seconds > 0.0);
        assert!(["memory", "sqlite"].contains(&c.backend.as_str()));
        assert!(["idle", "mixed", "throughput"].contains(&c.scenario.as_str()));
        c
    }
}
async fn run(
    config: Config,
    process: Process,
    latencies: Arc<Latencies>,
    directory: &std::path::Path,
) {
    let inner: Arc<dyn DurableTransitionStore> = match config.backend.as_str() {
        "memory" => Arc::new(MemoryJournal::new()),
        _ => Arc::new(SqliteEffectJournal::open(directory.join("bench.db")).unwrap()),
    };
    let counts = Arc::new(Counts::default());
    let progress = Arc::new(Progress::default());
    let waiting = config.scenario != "throughput";
    let batch_size = if config.release_batch == 0 {
        config.runs
    } else {
        config.release_batch
    };
    let (release, receivers): (Vec<_>, Vec<_>) = (0..config.runs.div_ceil(batch_size))
        .map(|_| watch::channel(false))
        .unzip();
    let gate = Gate {
        active: config.active,
        release: receivers.into(),
        batch_size,
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
    let mut early_exit = None;
    let mut reached_gate = !waiting;
    let setup_seconds;
    let finish_started;
    if waiting {
        reached_gate = tokio::time::timeout(Duration::from_secs(180), async {
            loop {
                if progress.ready.load(Ordering::Acquire) == config.runs {
                    return true;
                }
                if let Some(result) = tasks.try_join_next() {
                    early_exit = Some(result);
                    return false;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or(false);
        setup_seconds = start.elapsed().as_secs_f64();
        if reached_gate {
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
            eprintln!(
                "{}",
                json!({"stage":"steady", "setup_seconds":setup_seconds, "steady":steady})
            );
        }
        finish_started = Instant::now();
        let interval = config.release_interval;
        tokio::spawn(async move {
            for (index, gate) in release.into_iter().enumerate() {
                if index > 0 {
                    tokio::time::sleep(interval).await;
                }
                gate.send_replace(true);
            }
        });
    } else {
        setup_seconds = 0.0;
        finish_started = start;
    }
    let mut completed = 0;
    let mut errors = BTreeMap::new();
    let drained = tokio::time::timeout(Duration::from_secs(180), async {
        loop {
            let result = match early_exit.take() {
                Some(result) => Some(result),
                None => tasks.join_next().await,
            };
            let Some(result) = result else {
                break;
            };
            let error = match result {
                Ok(Ok(outcome)) if outcome.answer() == Some("done") => {
                    completed += 1;
                    continue;
                }
                Ok(Ok(outcome)) => format!("unexpected outcome: {:?}", outcome.status()),
                Ok(Err(error)) => error.to_string(),
                Err(error) => error.to_string(),
            };
            let category = if error.contains("database is locked")
                || error.contains("database table is locked")
            {
                "sqlite_locked"
            } else if error.contains("database or disk is full") {
                "sqlite_disk_full"
            } else if error.contains("lease was lost") {
                "lease_lost"
            } else if error.contains("queue is full") {
                "queue_full"
            } else {
                "other"
            };
            let (count, _example) = errors.entry(category).or_insert((0, error));
            *count += 1;
        }
    })
    .await
    .is_ok();
    let succeeded = reached_gate && completed == config.runs;
    if succeeded {
        assert_eq!(progress.models.load(Ordering::Relaxed), config.runs * 2);
        assert_eq!(progress.tools.load(Ordering::Relaxed), config.runs);
    }
    let finish_seconds = finish_started.elapsed().as_secs_f64();
    println!(
        "{}",
        json!({
            "backend": config.backend, "scenario": config.scenario, "runs": config.runs,
            "pooled": config.pooled, "active": config.active, "tokio_workers": 4,
            "max_blocking_threads": 32, "model_limit": 100, "tool_limit": 100,
            "rss_before_kib": rss_before, "peak_rss_kib": process.status("VmHWM:"),
            "setup_seconds": setup_seconds, "steady": steady,
            "finish_seconds": finish_seconds,
            "release_batch": config.release_batch,
            "release_interval_ms": config.release_interval.as_millis(),
            "completed_runs": completed, "successful": succeeded, "drained": drained,
            "reached_gate": reached_gate,
            "failed_runs": errors.values().map(|(count, _)| count).sum::<usize>(),
            "run_errors": errors,
            "completed_runs_per_second": succeeded.then(|| completed as f64/finish_seconds),
            "latencies": latencies.snapshot(),
            "cpu_total_seconds": process.cpu_seconds().zip(cpu_start).map(|(a,b)| a-b),
            "elapsed_seconds": start.elapsed().as_secs_f64(), "journal_calls_total": counts.snapshot(),
            "model_effects": progress.models.load(Ordering::Relaxed), "tool_effects": progress.tools.load(Ordering::Relaxed),
        })
    );
}
fn main() {
    let config = Config::parse();
    let process = Process::new();
    let latencies = Arc::new(Latencies::default());
    metered::install(latencies.clone());
    let directory = tempfile::Builder::new()
        .prefix("autra-scalability-")
        .tempdir()
        .unwrap();
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .max_blocking_threads(32)
        .enable_all()
        .build()
        .unwrap()
        .block_on(run(config, process, latencies, directory.path()));
    // Tokio must release detached workers and their SQLite connections first.
    directory
        .close()
        .expect("remove benchmark database directory");
}
