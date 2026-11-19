use std::process::ExitCode;

use autra_harness::{
    EchoModel, EchoTool, FakeModel, HistoryItem, ListFilesTool, LocalWorkspace, ModelOutput,
    ReadFileTool, ToolCallRequest, ToolRegistry,
};
use autra_runtime::{Agent, RunError, RunEvent, RunOptions, RunOutcome};

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None | Some("-h" | "--help" | "help") => {
            println!("{}", usage());
            ExitCode::SUCCESS
        }
        Some("run") => run_command(&args[1..]).await,
        Some("demo") => demo_command().await,
        Some("tools") => tools_command(),
        Some(other) => {
            eprintln!("unknown command `{other}`");
            eprintln!("{}", usage());
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

    match execute(agent, &prompt).await {
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
    let outcome = match execute(agent, "Inspect this project.").await {
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

async fn execute(agent: Agent, prompt: &str) -> Result<RunOutcome, RunError> {
    agent
        .run_with_options(prompt, RunOptions::default(), print_event)
        .await
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
    "autractl — Autra local agent CLI\n\nUSAGE:\n  autractl run <prompt>\n  autractl demo\n  autractl tools\n  autractl --help"
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
