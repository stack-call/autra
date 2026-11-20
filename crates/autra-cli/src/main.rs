use std::process::ExitCode;

use autra_harness::{
    EchoModel, EchoTool, FakeModel, HistoryItem, ListFilesTool, LocalWorkspace, ModelOutput,
    ReadFileTool, ToolCallRequest, ToolRegistry,
};
use autra_openai::OpenAiCompatibleModel;
use autra_runtime::{Agent, RunError, RunEvent, RunOptions, RunOutcome, Session};

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

async fn run_command(args: &[String]) -> ExitCode {
    let (model_name, prompt_parts) = match parse_flags(args) {
        Ok(parsed) => parsed,
        Err(()) => return ExitCode::FAILURE,
    };
    let prompt = prompt_parts.join(" ");
    if prompt.trim().is_empty() {
        eprintln!("usage: autractl run [--model <name>] <prompt>");
        return ExitCode::FAILURE;
    }

    let agent = match build_agent(model_name.as_deref()) {
        Ok(agent) => agent,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    let options = options_for_model(model_name.as_deref());

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
    let (model_name, rest) = match parse_flags(args) {
        Ok(parsed) => parsed,
        Err(()) => return ExitCode::FAILURE,
    };
    if !rest.is_empty() {
        eprintln!("usage: autractl chat [--model <name>]");
        return ExitCode::FAILURE;
    }

    let agent = match build_agent(model_name.as_deref()) {
        Ok(agent) => agent,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    let mut session = Session::new(agent);
    let options = options_for_model(model_name.as_deref());
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

fn parse_flags(args: &[String]) -> Result<(Option<String>, Vec<String>), ()> {
    let mut model_name = None;
    let mut rest = Vec::new();
    let mut parsing_flags = true;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if parsing_flags && arg == "--" {
            parsing_flags = false;
            continue;
        }
        if parsing_flags && arg == "--model" {
            match iter.next() {
                Some(name) if !name.is_empty() => model_name = Some(name.clone()),
                _ => {
                    eprintln!("usage: autractl run [--model <name>] <prompt>");
                    return Err(());
                }
            }
        } else {
            rest.push(arg.clone());
        }
    }
    Ok((model_name, rest))
}

fn build_agent(model_name: Option<&str>) -> Result<Agent, String> {
    let builder = Agent::builder("cli-agent").instructions("Answer the user concisely.");
    let builder = match model_name {
        Some(name) => {
            builder.model(OpenAiCompatibleModel::from_env(name).map_err(|error| error.to_string())?)
        }
        None => builder.model(EchoModel),
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
