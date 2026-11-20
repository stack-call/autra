use std::io::{self, Write};
use std::process::ExitCode;

use autra_harness::{
    EchoModel, EchoTool, FakeModel, HistoryItem, ListFilesTool, LocalWorkspace, ModelOutput,
    ReadFileTool, SearchTextTool, ToolCallRequest, ToolRegistry, WriteFileTool,
};
use autra_openai::OpenAiCompatibleModel;
use autra_runtime::{
    Agent, ApprovalDecision, ApprovalRequest, Approver, RunCommandTool, RunError, RunEvent,
    RunOptions, RunOutcome, Session,
};

#[tokio::main]
async fn main() -> ExitCode {
    init_tracing();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None | Some("-h" | "--help" | "help") => {
            println!("{}", usage());
            ExitCode::SUCCESS
        }
        Some("run") => run_command(&args[1..]).await,
        Some("chat") => chat_command(&args[1..]).await,
        Some("demo") => demo_command().await,
        Some("tools") => tools_command(),
        Some(other) => {
            eprintln!("unknown command `{other}`");
            eprintln!("{}", usage());
            ExitCode::FAILURE
        }
    }
}

struct Flags {
    model: Option<String>,
    workspace: Option<String>,
    auto_approve: bool,
    read_only: bool,
    rest: Vec<String>,
}

const WORKSPACE_INSTRUCTIONS: &str = "\
You are a coding agent working inside one workspace.
Inspect the workspace with list_files, search_text and read_file before changing it.
Use write_file to create or replace files; missing parent directories are created.
Use run_command to build, test or inspect the project. Keep changes focused, stay
inside the workspace, and when you finish summarize what changed and what you verified.";

async fn run_command(args: &[String]) -> ExitCode {
    let flags = match parse_flags(args, "run") {
        Ok(flags) => flags,
        Err(()) => return ExitCode::FAILURE,
    };
    let prompt = flags.rest.join(" ");
    if prompt.trim().is_empty() {
        eprintln!(
            "usage: autractl run [--workspace <dir>] [--model <name>] [--yes] [--read-only] <prompt>"
        );
        return ExitCode::FAILURE;
    }

    let agent = match build_agent(&flags) {
        Ok(agent) => agent,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    let options = options_for_model(flags.model.as_deref());

    match execute(agent, &prompt, options).await {
        Ok(outcome) => {
            print_status(&outcome);
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("run failed: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn chat_command(args: &[String]) -> ExitCode {
    let flags = match parse_flags(args, "chat") {
        Ok(flags) => flags,
        Err(()) => return ExitCode::FAILURE,
    };
    if !flags.rest.is_empty() {
        eprintln!(
            "usage: autractl chat [--workspace <dir>] [--model <name>] [--yes] [--read-only]"
        );
        return ExitCode::FAILURE;
    }

    let agent = match build_agent(&flags) {
        Ok(agent) => agent,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    let mut session = Session::new(agent);
    let options = options_for_model(flags.model.as_deref());
    let stdin = std::io::stdin();
    let mut line = String::new();

    loop {
        line.clear();
        match stdin.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(error) => {
                eprintln!("cannot read input: {error}");
                return ExitCode::FAILURE;
            }
        }
        let input = line.trim();
        if input.is_empty() {
            continue;
        }
        match session
            .run_turn_with_options(input, options.clone(), print_event)
            .await
        {
            Ok(outcome) => print_status(&outcome),
            Err(error) => {
                eprintln!("run failed: {error}");
                return ExitCode::FAILURE;
            }
        }
    }

    ExitCode::SUCCESS
}

fn parse_flags(args: &[String], command: &str) -> Result<Flags, ()> {
    let usage = || {
        eprintln!(
            "usage: autractl {command} [--workspace <dir>] [--model <name>] [--yes] [--read-only]"
        );
    };
    let mut flags = Flags {
        model: None,
        workspace: None,
        auto_approve: false,
        read_only: false,
        rest: Vec::new(),
    };
    let mut parsing_flags = true;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if parsing_flags && arg == "--" {
            parsing_flags = false;
            continue;
        }
        if !parsing_flags || !arg.starts_with('-') {
            flags.rest.push(arg.clone());
            continue;
        }
        match arg.as_str() {
            "--model" | "-m" => match iter.next() {
                Some(name) if !name.is_empty() => flags.model = Some(name.clone()),
                _ => {
                    usage();
                    return Err(());
                }
            },
            "--workspace" | "-C" => match iter.next() {
                Some(path) if !path.is_empty() => flags.workspace = Some(path.clone()),
                _ => {
                    usage();
                    return Err(());
                }
            },
            "--yes" | "-y" => flags.auto_approve = true,
            "--read-only" => flags.read_only = true,
            other => {
                eprintln!("unknown option `{other}`");
                usage();
                return Err(());
            }
        }
    }
    Ok(flags)
}

fn build_agent(flags: &Flags) -> Result<Agent, String> {
    let root = flags.workspace.as_deref().unwrap_or(".");
    let workspace = LocalWorkspace::open(root).map_err(|error| error.to_string())?;
    let mut tools = ToolRegistry::new();
    tools.register(ListFilesTool::new(workspace.clone()));
    tools.register(ReadFileTool::new(workspace.clone()));
    tools.register(SearchTextTool::new(workspace.clone()));
    if flags.read_only {
        eprintln!("[workspace] {root} (read-only)");
    } else {
        tools.register(WriteFileTool::new(workspace.clone()));
        tools.register(RunCommandTool::new(workspace));
        eprintln!("[workspace] {root}");
    }
    let builder = Agent::builder("autractl")
        .instructions(WORKSPACE_INSTRUCTIONS)
        .tools(tools)
        .max_turns(24)
        .max_tool_calls(64)
        .max_parallel_tool_calls(4);
    let builder = match flags.model.as_deref() {
        Some(name) => {
            builder.model(OpenAiCompatibleModel::from_env(name).map_err(|error| error.to_string())?)
        }
        None => builder.model(EchoModel),
    };
    let builder = if flags.auto_approve {
        builder.approver(AutoApprove)
    } else {
        builder.approver(InteractiveApprover)
    };
    builder.build().map_err(|error| error.to_string())
}

fn options_for_model(model_name: Option<&str>) -> RunOptions {
    if model_name.is_some() {
        RunOptions {
            effect_timeout: Some(std::time::Duration::from_secs(120)),
            ..RunOptions::default()
        }
    } else {
        RunOptions::default()
    }
}

struct AutoApprove;

#[async_trait::async_trait]
impl Approver for AutoApprove {
    async fn approve(&self, _request: &ApprovalRequest) -> ApprovalDecision {
        ApprovalDecision::Approved
    }
}

struct InteractiveApprover;

#[async_trait::async_trait]
impl Approver for InteractiveApprover {
    async fn approve(&self, request: &ApprovalRequest) -> ApprovalDecision {
        eprint!("[approval] run `{}`? [y/N] ", request.tool_name);
        let _ = io::stderr().flush();
        let mut answer = String::new();
        match io::stdin().read_line(&mut answer) {
            Ok(_) => {
                let answer = answer.trim().to_lowercase();
                if answer == "y" || answer == "yes" {
                    ApprovalDecision::Approved
                } else {
                    ApprovalDecision::Rejected
                }
            }
            Err(_) => ApprovalDecision::Rejected,
        }
    }
}

async fn demo_command() -> ExitCode {
    let workspace = match LocalWorkspace::open(".") {
        Ok(workspace) => workspace,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    let model = FakeModel::new(vec![
        ModelOutput {
            text: None,
            tool_calls: vec![ToolCallRequest {
                call_id: "demo-1".to_owned(),
                name: "list_files".to_owned(),
                arguments: r#"{"path":"."}"#.to_owned(),
            }],
        },
        ModelOutput {
            text: None,
            tool_calls: vec![ToolCallRequest {
                call_id: "demo-2".to_owned(),
                name: "read_file".to_owned(),
                arguments: r#"{"path":"README.md"}"#.to_owned(),
            }],
        },
        ModelOutput {
            text: Some("Demo run finished.".to_owned()),
            tool_calls: Vec::new(),
        },
    ]);
    let agent = match Agent::builder("demo-agent")
        .instructions("Inspect the workspace using the available tools.")
        .model(model)
        .tools(builtin_registry(workspace))
        .build()
    {
        Ok(agent) => agent,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    let outcome = match execute(agent, "Inspect this project.", RunOptions::default()).await {
        Ok(outcome) => outcome,
        Err(error) => {
            eprintln!("demo failed: {error}");
            return ExitCode::FAILURE;
        }
    };

    print_status(&outcome);
    if demo_tools_succeeded(&outcome) {
        println!("demo: listed the workspace and read README.md");
        ExitCode::SUCCESS
    } else {
        eprintln!("demo failed: the list-files/read-README sequence did not complete");
        ExitCode::FAILURE
    }
}

fn demo_tools_succeeded(outcome: &RunOutcome) -> bool {
    let succeeded = |name: &str| {
        outcome.state().history.iter().any(
            |item| matches!(item, HistoryItem::Tool(tool) if tool.name == name && !tool.is_error),
        )
    };
    succeeded("list_files") && succeeded("read_file")
}

fn tools_command() -> ExitCode {
    let workspace = match LocalWorkspace::open(".") {
        Ok(workspace) => workspace,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    let mut registry = builtin_registry(workspace.clone());
    registry.register(SearchTextTool::new(workspace.clone()));
    registry.register(RunCommandTool::new(workspace));
    for spec in registry.specs() {
        println!("{}\t{}", spec.name, spec.description);
    }
    ExitCode::SUCCESS
}

fn builtin_registry(workspace: LocalWorkspace) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register(EchoTool);
    registry.register(ListFilesTool::new(workspace.clone()));
    registry.register(WriteFileTool::new(workspace.clone()));
    registry.register(ReadFileTool::new(workspace));
    registry
}

async fn execute(agent: Agent, prompt: &str, options: RunOptions) -> Result<RunOutcome, RunError> {
    agent.run_with_options(prompt, options, print_event).await
}

fn print_status(outcome: &RunOutcome) {
    println!();
    println!(
        "status: {:?}, turns: {}, tool calls: {}",
        outcome.status(),
        outcome.turns_used(),
        outcome.tool_calls_used()
    );
}

fn print_event(event: &RunEvent) {
    match event {
        RunEvent::RunStarted { run_id } => println!("[run] {run_id} started"),
        RunEvent::ModelCallStarted { turn } => println!("[model] turn {turn}"),
        RunEvent::ModelResult { text, tool_calls } => {
            if let Some(text) = text {
                println!("[model] {}", preview(text));
            }
            for call in tool_calls {
                println!("[model] tool call: {} ({})", call.name, call.call_id);
            }
        }
        RunEvent::ApprovalRequested {
            call_id,
            tool_name,
            argument_digest,
        } => {
            println!("[approval] {tool_name} ({call_id}) digest {argument_digest}");
        }
        RunEvent::ApprovalDecided { call_id, approved } => {
            println!(
                "[approval] {call_id} {}",
                if *approved { "approved" } else { "rejected" }
            );
        }
        RunEvent::ToolCallStarted { call_id, name } => {
            println!("[tool] {name} ({call_id}) started");
        }
        RunEvent::ToolResult {
            name,
            is_error,
            outcome_unknown,
            content,
            ..
        } => {
            let status = if *outcome_unknown {
                "unknown"
            } else if *is_error {
                "error"
            } else {
                "ok"
            };
            println!("[tool] {name} {status}: {}", preview(content));
        }
        RunEvent::RunCompleted { answer } => {
            println!(
                "[run] completed: {}",
                answer
                    .as_deref()
                    .map(preview)
                    .unwrap_or_else(|| "(no answer)".to_owned())
            );
        }
        RunEvent::RunCancelled => println!("[run] cancelled"),
        RunEvent::RunFailed { message } => eprintln!("[run] failed: {message}"),
    }
}

fn preview(text: &str) -> String {
    let mut preview: String = text.chars().take(400).collect();
    if text.chars().count() > 400 {
        preview.push('…');
    }
    preview
}

fn usage() -> &'static str {
    "autractl — Autra local agent CLI\n\nUSAGE:\n  autractl run [--model <name>] <prompt>\n  autractl chat [--model <name>]\n  autractl demo\n  autractl tools\n  autractl --help"
}

fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"));
    let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
}

#[cfg(test)]
mod tests {
    use super::preview;

    #[test]
    fn preview_truncates_at_400_chars() {
        let long = "a".repeat(401);

        let truncated = preview(&long);

        assert_eq!(truncated.chars().count(), 401);
        assert!(truncated.ends_with('…'));
        assert_eq!(preview("short"), "short");
    }
}
