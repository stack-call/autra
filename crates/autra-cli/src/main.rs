use std::process::ExitCode;

use autra_core::{ModelOutput, ToolCallRequest};
use autra_harness::{
    EchoModel, EchoTool, FakeModel, ListFilesTool, LocalWorkspace, ReadFileTool, ToolRegistry,
};
use autra_runtime::{Agent, RunEvent, RunOptions};

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None | Some("-h" | "--help" | "help") => {
            print_usage();
            ExitCode::SUCCESS
        }
        Some("run") => run_command(&args[1..]).await,
        Some("demo") => demo_command(args.get(1).map(String::as_str).unwrap_or(".")).await,
        Some("tools") => tools_command(),
        Some(other) => {
            eprintln!("unknown command `{other}`");
            print_usage();
            ExitCode::FAILURE
        }
    }
}

async fn run_command(args: &[String]) -> ExitCode {
    let prompt = args.join(" ");
    if prompt.trim().is_empty() {
        eprintln!("usage: autractl run <prompt>");
        return ExitCode::FAILURE;
    }

    let agent = match Agent::builder("cli-agent")
        .instructions("Answer the user concisely.")
        .model(EchoModel)
        .build()
    {
        Ok(agent) => agent,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    run_agent(agent, &prompt).await
}

async fn demo_command(root: &str) -> ExitCode {
    let workspace = match LocalWorkspace::open(root) {
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
            text: Some("Demo complete: listed the workspace and read README.md.".to_owned()),
            tool_calls: Vec::new(),
        },
    ]);
    let agent = match Agent::builder("demo-agent")
        .instructions("Inspect the workspace using the available tools.")
        .model(model)
        .tool(ListFilesTool::new(workspace.clone()))
        .tool(ReadFileTool::new(workspace))
        .build()
    {
        Ok(agent) => agent,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    run_agent(agent, "Inspect this project.").await
}

fn tools_command() -> ExitCode {
    let workspace = match LocalWorkspace::open(".") {
        Ok(workspace) => workspace,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    let registry = builtin_registry(workspace);
    for spec in registry.specs() {
        println!("{}\t{}", spec.name, spec.description);
    }
    ExitCode::SUCCESS
}

fn builtin_registry(workspace: LocalWorkspace) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register(EchoTool);
    registry.register(ListFilesTool::new(workspace.clone()));
    registry.register(ReadFileTool::new(workspace));
    registry
}

async fn run_agent(agent: Agent, prompt: &str) -> ExitCode {
    match agent
        .run_with_options(prompt, RunOptions::default(), print_event)
        .await
    {
        Ok(outcome) => {
            println!();
            println!(
                "status: {:?}, turns: {}, tool calls: {}",
                outcome.status(),
                outcome.turns_used(),
                outcome.tool_calls_used()
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("run failed: {error}");
            ExitCode::FAILURE
        }
    }
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

fn print_usage() {
    println!(
        "autractl — Autra local agent CLI\n\nUSAGE:\n  autractl run <prompt>\n  autractl demo [path]\n  autractl tools\n  autractl --help"
    );
}
