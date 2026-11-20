use std::io::{self, Write};
use std::process::ExitCode;
use std::sync::Arc;

use autra_core::{EffectId, RunId, SessionId};
use autra_durable_sqlite::{SqliteEffectJournal, SqliteSessionStore};

use autra::{
    Agent, ApprovalDecision, ApprovalRecord, ApprovalRequest, ApprovalStore, Approver,
    DEFAULT_APPROVAL_TTL, EffectJournal, LOCAL_POLICY_REVISION, RulePolicy, RunError, RunEvent,
    RunOptions, RunOutcome, Session, SessionStore,
};
use autra_harness::{
    EchoModel, EchoTool, EditFileTool, FakeModel, GitDiffTool, GitStatusTool, HistoryItem,
    ListFilesTool, LocalWorkspace, ModelOutput, ReadFileTool, RunCommandTool, SearchTextTool,
    ToolCallRequest, ToolRegistry, WriteFileTool,
};
use autra_openai::OpenAiCompatibleModel;

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
        Some("resume") => resume_command(&args[1..]).await,
        Some("outbox") => outbox_command(&args[1..]).await,
        Some("snapshot") => snapshot_command(&args[1..]),
        Some("effects") => effects_command(&args[1..]),
        Some("reconcile") => reconcile_command(&args[1..]).await,
        Some("approve") => approval_command(&args[1..], ApprovalDecision::Approved),
        Some("reject") => approval_command(&args[1..], ApprovalDecision::Rejected),
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
    journal: Option<String>,
    session: Option<String>,
    policy: Option<String>,
    actor: Option<String>,
    unknown_only: bool,
    local: bool,
    rest: Vec<String>,
}

const WORKSPACE_INSTRUCTIONS: &str = "\
You are a coding agent working inside one workspace.
Inspect the workspace with list_files, search_text, read_file, git_status and git_diff.
Use edit_file for targeted changes and write_file for new files; missing parent
directories are created. Use run_command to build, test or inspect the project.
Check git_diff after editing. Keep changes focused, stay inside the workspace,
and when you finish summarize what changed and what you verified.";

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
    if flags.session.is_some() && flags.journal.is_none() {
        eprintln!("--session requires --journal <path>");
        return ExitCode::FAILURE;
    }
    let mut session = match &flags.journal {
        Some(path) => {
            let store: Arc<dyn SessionStore> = match SqliteSessionStore::open(path) {
                Ok(store) => Arc::new(store),
                Err(error) => {
                    eprintln!("{error}");
                    return ExitCode::FAILURE;
                }
            };
            let session_id = flags
                .session
                .as_ref()
                .map(SessionId::new)
                .unwrap_or_else(SessionId::fresh);
            eprintln!("[session] {session_id}");
            match Session::open(agent, store, session_id) {
                Ok(session) => session,
                Err(error) => {
                    eprintln!("{error}");
                    return ExitCode::FAILURE;
                }
            }
        }
        None => Session::new(agent),
    };
    let options = options_for_model(flags.model.as_deref());
    let stdin = std::io::stdin();
    let mut line = String::new();
    let mut printer = EventPrinter::default();

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
            .run_turn_with_options(input, options.clone(), |event| printer.print(event))
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

async fn resume_command(args: &[String]) -> ExitCode {
    let flags = match parse_flags(args, "resume") {
        Ok(flags) => flags,
        Err(()) => return ExitCode::FAILURE,
    };
    let Some(run_id) = flags.rest.first() else {
        eprintln!(
            "usage: autractl resume --journal <path> --model <name> [--workspace <dir>] <run-id>"
        );
        return ExitCode::FAILURE;
    };
    if flags.rest.len() > 1 {
        eprintln!(
            "usage: autractl resume --journal <path> --model <name> [--workspace <dir>] <run-id>"
        );
        return ExitCode::FAILURE;
    }
    if flags.journal.is_none() {
        eprintln!("resume requires --journal <path>");
        return ExitCode::FAILURE;
    }
    if flags.model.is_none() && !flags.local {
        eprintln!(
            "resume requires --model <name> or --local: the journal does not store the provider"
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
    let mut printer = EventPrinter::default();

    match agent
        .resume_with_options(&RunId::new(run_id.clone()), options, |event| {
            printer.print(event)
        })
        .await
    {
        Ok(outcome) => {
            print_status(&outcome);
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("resume failed: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn outbox_command(args: &[String]) -> ExitCode {
    let flags = match parse_flags(args, "outbox") {
        Ok(flags) => flags,
        Err(()) => return ExitCode::FAILURE,
    };
    if !flags.rest.is_empty() {
        eprintln!("usage: autractl outbox --journal <path> --model <name> [--workspace <dir>]");
        return ExitCode::FAILURE;
    }
    if flags.journal.is_none() {
        eprintln!("outbox requires --journal <path>");
        return ExitCode::FAILURE;
    }
    if flags.model.is_none() && !flags.local {
        eprintln!(
            "outbox requires --model <name> or --local: model effects need the run's provider"
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

    match agent.drain_outbox(100).await {
        Ok(results) => {
            for result in &results {
                println!("[outbox] {} {:?}", result.effect_id, result.outcome);
            }
            println!("outbox: dispatched {} due effect(s)", results.len());
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("outbox drain failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn load_rule_policy(path: &str) -> Result<RulePolicy, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("cannot read policy `{path}`: {error}"))?;
    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|error| format!("cannot parse policy `{path}`: {error}"))?;
    let mut policy = RulePolicy::new(format!("file:{path}"));
    for name in string_list(&value, "allow_tools")? {
        policy = policy.allow_tool(name);
    }
    for name in string_list(&value, "ask_tools")? {
        policy = policy.ask_tool(name);
    }
    for name in string_list(&value, "deny_tools")? {
        policy = policy.deny_tool(name);
    }
    for pattern in string_list(&value, "deny_commands")? {
        policy = policy.deny_command(pattern);
    }
    Ok(policy)
}

fn string_list(value: &serde_json::Value, key: &str) -> Result<Vec<String>, String> {
    match value.get(key) {
        None | Some(serde_json::Value::Null) => Ok(Vec::new()),
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| format!("policy `{key}` must contain strings"))
            })
            .collect(),
        Some(_) => Err(format!("policy `{key}` must be an array")),
    }
}

fn effects_command(args: &[String]) -> ExitCode {
    let flags = match parse_flags(args, "effects") {
        Ok(flags) => flags,
        Err(()) => return ExitCode::FAILURE,
    };
    let Some(run_id) = flags.rest.first() else {
        eprintln!("usage: autractl effects --journal <path> [--unknown] <run-id>");
        return ExitCode::FAILURE;
    };
    let Some(path) = flags.journal.as_ref() else {
        eprintln!("effects requires --journal <path>");
        return ExitCode::FAILURE;
    };
    let journal = match SqliteEffectJournal::open(path) {
        Ok(journal) => journal,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    let run_id = RunId::new(run_id.clone());
    let mut effects = match journal.load(&run_id) {
        Ok(effects) => effects,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    if flags.unknown_only {
        effects.retain(|effect| effect.status == autra_core::EffectStatus::OutcomeUnknown);
    }
    if effects.is_empty() {
        println!("no effects");
        return ExitCode::SUCCESS;
    }
    for effect in effects {
        let tool = effect.tool_name.as_deref().unwrap_or("-");
        let error = effect
            .error
            .as_deref()
            .map(|error| format!(" error={error}"))
            .unwrap_or_default();
        let reconciled = if effect.reconciliations.is_empty() {
            String::new()
        } else {
            format!(" reconciled={}", effect.reconciliations.len())
        };
        println!(
            "{} {:?} {} {:?} attempts={}{}{}",
            effect.id,
            effect.kind,
            tool,
            effect.status,
            effect.attempts.len(),
            error,
            reconciled
        );
    }
    ExitCode::SUCCESS
}

async fn reconcile_command(args: &[String]) -> ExitCode {
    let flags = match parse_flags(args, "reconcile") {
        Ok(flags) => flags,
        Err(()) => return ExitCode::FAILURE,
    };
    let Some(run_id) = flags.rest.first() else {
        eprintln!(
            "usage: autractl reconcile --journal <path> [--model <name>] [--workspace <dir>] <run-id>"
        );
        return ExitCode::FAILURE;
    };
    if flags.journal.is_none() {
        eprintln!("reconcile requires --journal <path>");
        return ExitCode::FAILURE;
    }
    let agent = match build_agent(&flags) {
        Ok(agent) => agent,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    let run_id = RunId::new(run_id.clone());
    let retry_models = flags.model.is_some();
    match agent.reconcile_with(&run_id, retry_models).await {
        Ok(report) => {
            println!(
                "reconcile: retried {} effect(s), unresolved {}",
                report.retried.len(),
                report.unresolved.len()
            );
            for unresolved in &report.unresolved {
                println!(
                    "[unresolved] {}: {}",
                    unresolved.effect_id, unresolved.reason
                );
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("reconcile failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn approval_command(args: &[String], decision: ApprovalDecision) -> ExitCode {
    let command = match decision {
        ApprovalDecision::Approved => "approve",
        ApprovalDecision::Rejected => "reject",
    };
    let flags = match parse_flags(args, command) {
        Ok(flags) => flags,
        Err(()) => return ExitCode::FAILURE,
    };
    if flags.rest.len() != 2 {
        eprintln!(
            "usage: autractl {command} --journal <path> [--policy <path>] [--actor <name>] <run-id> <effect-id>"
        );
        return ExitCode::FAILURE;
    }
    let Some(path) = flags.journal.as_ref() else {
        eprintln!("{command} requires --journal <path>");
        return ExitCode::FAILURE;
    };
    let run_id = RunId::new(flags.rest[0].clone());
    let effect_id = EffectId::new(flags.rest[1].clone());
    let journal = match SqliteEffectJournal::open(path) {
        Ok(journal) => journal,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    let effects = match journal.load(&run_id) {
        Ok(effects) => effects,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    let Some(effect) = effects.iter().find(|effect| effect.id == effect_id) else {
        eprintln!("effect `{effect_id}` was not found in run `{run_id}`");
        return ExitCode::FAILURE;
    };
    let revision = match flags.policy.as_ref() {
        Some(policy_path) => format!("file:{policy_path}"),
        None => LOCAL_POLICY_REVISION.to_owned(),
    };
    let Some(record) = ApprovalRecord::from_effect(
        effect,
        &run_id,
        decision,
        flags.actor.clone(),
        revision,
        Some(DEFAULT_APPROVAL_TTL),
    ) else {
        eprintln!("effect `{effect_id}` is not a tool call");
        return ExitCode::FAILURE;
    };
    match journal.save_approval(&record) {
        Ok(()) => {
            println!("{command}d effect `{effect_id}` for run `{run_id}`");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn snapshot_command(args: &[String]) -> ExitCode {
    let flags = match parse_flags(args, "snapshot") {
        Ok(flags) => flags,
        Err(()) => return ExitCode::FAILURE,
    };
    let Some(run_id) = flags.rest.first() else {
        eprintln!("usage: autractl snapshot --journal <path> <run-id>");
        return ExitCode::FAILURE;
    };
    let Some(path) = flags.journal.as_ref() else {
        eprintln!("snapshot requires --journal <path>");
        return ExitCode::FAILURE;
    };
    let journal = match SqliteEffectJournal::open(path) {
        Ok(journal) => journal,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    let run_id = RunId::new(run_id.clone());
    let state = match journal.load_run(&run_id) {
        Ok(Some(state)) => state,
        Ok(None) => {
            eprintln!("run `{run_id}` was not found in the journal");
            return ExitCode::FAILURE;
        }
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    let (events, effects) = match (journal.load_events(&run_id), journal.load(&run_id)) {
        (Ok(events), Ok(effects)) => (events, effects),
        (Err(error), _) | (_, Err(error)) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    let last_seq = events.iter().map(|event| event.seq).max().unwrap_or(0);
    let snapshot = autra::RunSnapshot::from_state(&state, last_seq, effects.len());
    match serde_json::to_string_pretty(&snapshot) {
        Ok(json) => {
            println!("{json}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("cannot encode snapshot: {error}");
            ExitCode::FAILURE
        }
    }
}

fn parse_flags(args: &[String], command: &str) -> Result<Flags, ()> {
    let usage = || {
        eprintln!(
            "usage: autractl {command} [--workspace <dir>] [--model <name>] [--journal <path>] [--yes] [--read-only]"
        );
    };
    let mut flags = Flags {
        model: None,
        workspace: None,
        auto_approve: false,
        read_only: false,
        journal: None,
        session: None,
        policy: None,
        actor: None,
        unknown_only: false,
        local: false,
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
            "--journal" | "-j" => match iter.next() {
                Some(path) if !path.is_empty() => flags.journal = Some(path.clone()),
                _ => {
                    usage();
                    return Err(());
                }
            },
            "--policy" => match iter.next() {
                Some(path) if !path.is_empty() => flags.policy = Some(path.clone()),
                _ => {
                    usage();
                    return Err(());
                }
            },
            "--actor" => match iter.next() {
                Some(actor) if !actor.is_empty() => flags.actor = Some(actor.clone()),
                _ => {
                    usage();
                    return Err(());
                }
            },
            "--session" => match iter.next() {
                Some(id) if !id.is_empty() => flags.session = Some(id.clone()),
                _ => {
                    usage();
                    return Err(());
                }
            },
            "--unknown" => flags.unknown_only = true,
            "--local" => flags.local = true,
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
    if flags.local && flags.model.is_some() {
        return Err("--local and --model cannot be combined".to_owned());
    }
    let root = flags.workspace.as_deref().unwrap_or(".");
    let workspace = LocalWorkspace::open(root).map_err(|error| error.to_string())?;
    let mut tools = ToolRegistry::new();
    tools.register(ListFilesTool::new(workspace.clone()));
    tools.register(ReadFileTool::new(workspace.clone()));
    tools.register(SearchTextTool::new(workspace.clone()));
    tools.register(GitStatusTool::new(workspace.clone()));
    tools.register(GitDiffTool::new(workspace.clone()));
    if flags.read_only {
        eprintln!("[workspace] {root} (read-only)");
    } else {
        tools.register(WriteFileTool::new(workspace.clone()));
        tools.register(EditFileTool::new(workspace.clone()));
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
    let builder = match &flags.policy {
        Some(path) => builder.policy(load_rule_policy(path)?),
        None => builder,
    };
    let builder = match &flags.actor {
        Some(actor) => builder.actor(actor.clone()),
        None => builder,
    };
    let builder = match &flags.journal {
        Some(path) => {
            builder.journal(SqliteEffectJournal::open(path).map_err(|error| error.to_string())?)
        }
        None => builder,
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
        eprintln!("[approval] {} ({})", request.tool_name, request.call_id);
        match &request.preview {
            Some(preview) => eprintln!("{preview}"),
            None => eprintln!("arguments: {}", request.arguments),
        }
        eprint!("allow? [y/N] ");
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
        autra_harness::react_history(outcome.state()).iter().any(
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
    registry.register(EditFileTool::new(workspace.clone()));
    registry.register(GitStatusTool::new(workspace.clone()));
    registry.register(GitDiffTool::new(workspace.clone()));
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
    let mut printer = EventPrinter::default();
    agent
        .run_with_options(prompt, options, |event| printer.print(event))
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

#[derive(Default)]
struct EventPrinter {
    streamed: bool,
}

impl EventPrinter {
    fn print(&mut self, event: &RunEvent) {
        match event {
            RunEvent::RunStarted { run_id } => println!("[run] {run_id} started"),
            RunEvent::RunResumed { run_id } => println!("[run] {run_id} resumed"),
            RunEvent::ModelCallStarted { turn } => println!("[model] turn {turn}"),
            RunEvent::ModelDelta { text } => {
                print!("{text}");
                let _ = io::stdout().flush();
                self.streamed = true;
            }
            RunEvent::ModelResult { text, tool_calls } => {
                if self.streamed {
                    println!();
                    self.streamed = false;
                } else if let Some(text) = text {
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
}

fn preview(text: &str) -> String {
    let mut preview: String = text.chars().take(400).collect();
    if text.chars().count() > 400 {
        preview.push('…');
    }
    preview
}

fn usage() -> &'static str {
    r#"autractl - Autra local workspace agent

USAGE:
  autractl run [--workspace <dir>] [--model <name>] [--journal <path>] [--policy <path>] [--actor <name>] [--yes] [--read-only] <prompt>
  autractl chat [--workspace <dir>] [--model <name>] [--journal <path>] [--session <id>] [--policy <path>] [--actor <name>] [--yes] [--read-only]
  autractl resume --journal <path> (--model <name> | --local) [--workspace <dir>] [--policy <path>] [--actor <name>] <run-id>
  autractl outbox --journal <path> (--model <name> | --local) [--workspace <dir>]
  autractl snapshot --journal <path> <run-id>
  autractl effects --journal <path> [--unknown] <run-id>
  autractl reconcile --journal <path> [--model <name>] [--workspace <dir>] [--policy <path>] <run-id>
  autractl approve|reject --journal <path> [--policy <path>] [--actor <name>] <run-id> <effect-id>
  autractl demo
  autractl tools
  autractl --help

OPTIONS:
  --workspace <dir>  workspace root (default ".")
  --model <name>     OpenAI-compatible model; omit for the local echo model
  --local            use the built-in local echo model explicitly
  --journal <path>   persist runs, events, effects, approvals and sessions to SQLite
  --session <id>     named chat session (requires --journal)
  --policy <path>    JSON tool/command policy
  --actor <name>     operator identity recorded on approvals
  --yes              auto-approve gated tools
  --read-only        only expose read-only tools"#
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
