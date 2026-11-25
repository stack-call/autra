//! A small terminal UI for a local chat session.

use std::io;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;

use async_trait::async_trait;
use autra::{
    ApprovalDecision, ApprovalRequest, Approver, RunEvent, RunOptions, Session, SessionStore,
};
use autra_core::{CancelFlag, HistoryItem, RunId, SessionId};
use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Position};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::{Frame, Terminal};

#[derive(Clone)]
pub struct TuiConfig {
    pub workspace: String,
    pub model: String,
    pub read_only: bool,
    pub auto_approve: bool,
    /// When set, the TUI opens this persisted session and saves every turn.
    pub session: Option<SessionBinding>,
}

/// A persisted session the TUI should restore and continue.
#[derive(Clone)]
pub struct SessionBinding {
    pub store: Arc<dyn SessionStore>,
    pub session_id: SessionId,
}

pub enum UiMessage {
    Event(RunEvent),
    Approval(ApprovalRequest, Sender<ApprovalDecision>),
    Finished {
        session: Box<Session>,
        result: Result<(), String>,
    },
}

#[derive(Clone)]
pub struct TuiApprover {
    sender: Sender<UiMessage>,
}

impl TuiApprover {
    pub fn new(sender: Sender<UiMessage>) -> Self {
        Self { sender }
    }
}

#[async_trait]
impl Approver for TuiApprover {
    async fn approve(&self, request: &ApprovalRequest) -> ApprovalDecision {
        let (decision_sender, decision_receiver) = std::sync::mpsc::channel();
        if self
            .sender
            .send(UiMessage::Approval(request.clone(), decision_sender))
            .is_err()
        {
            return ApprovalDecision::Rejected;
        }
        decision_receiver
            .recv()
            .unwrap_or(ApprovalDecision::Rejected)
    }
}

struct ChatLine {
    style: Style,
    text: String,
}

struct State {
    session: Option<Session>,
    session_label: Option<String>,
    turn_index: u32,
    config: TuiConfig,
    sender: Sender<UiMessage>,
    lines: Vec<ChatLine>,
    streaming: Option<String>,
    input: String,
    cursor: usize,
    history: Vec<String>,
    history_index: Option<usize>,
    status: String,
    running: bool,
    scroll_back: usize,
    approval: std::collections::VecDeque<(ApprovalRequest, Sender<ApprovalDecision>)>,
    should_quit: bool,
    cancel: Option<CancelFlag>,
    /// Wrapped committed lines, rebuilt only when the width changes or new
    /// messages arrive. Streaming text is wrapped separately each frame.
    wrapped: Vec<Line<'static>>,
    wrapped_width: usize,
    wrapped_count: usize,
}

impl State {
    fn new(session: Session, config: TuiConfig, sender: Sender<UiMessage>) -> Self {
        let history = session.history().to_vec();
        let session_label = config
            .session
            .as_ref()
            .map(|binding| binding.session_id.to_string());
        let turn_index = session.turn_index();
        let mut lines = history_lines(&history);
        if !lines.is_empty() {
            lines.push(ChatLine {
                style: Style::default().fg(Color::DarkGray),
                text: format!("[session] restored {turn_index} turn(s)"),
            });
        }
        Self {
            session: Some(session),
            session_label,
            turn_index,
            config,
            sender,
            lines,
            streaming: None,
            input: String::new(),
            cursor: 0,
            history: Vec::new(),
            history_index: None,
            status: "idle".to_owned(),
            running: false,
            scroll_back: 0,
            approval: std::collections::VecDeque::new(),
            should_quit: false,
            cancel: None,
            wrapped: Vec::new(),
            wrapped_width: 0,
            wrapped_count: 0,
        }
    }

    fn push(&mut self, style: Style, text: impl Into<String>) {
        self.lines.push(ChatLine {
            style,
            text: text.into(),
        });
        self.scroll_back = 0;
    }

    fn handle_message(&mut self, message: UiMessage) {
        match message {
            UiMessage::Event(event) => self.handle_event(event),
            UiMessage::Approval(request, sender) => {
                self.approval.push_back((request, sender));
                self.status = "awaiting approval".to_owned();
            }
            UiMessage::Finished { session, result } => {
                self.running = false;
                self.cancel = None;
                self.turn_index = session.turn_index();
                self.session = Some(*session);
                self.status = "idle".to_owned();
                if let Err(error) = result {
                    self.push(
                        Style::default().fg(Color::Red),
                        format!("run failed: {error}"),
                    );
                }
            }
        }
    }

    fn handle_event(&mut self, event: RunEvent) {
        match event {
            RunEvent::RunStarted { run_id } => {
                self.status = format!("run {run_id}");
                self.push(
                    Style::default().fg(Color::DarkGray),
                    format!("run {run_id} started"),
                );
            }
            RunEvent::RunResumed { run_id } => {
                self.status = format!("run {run_id}");
                self.push(
                    Style::default().fg(Color::DarkGray),
                    format!("run {run_id} resumed"),
                );
            }
            RunEvent::ModelCallStarted { turn } => {
                self.status = format!("thinking (turn {turn})");
            }
            RunEvent::ModelDelta { text } => {
                self.streaming
                    .get_or_insert_with(String::new)
                    .push_str(&text);
                self.scroll_back = 0;
            }
            RunEvent::ModelResult { text, tool_calls } => {
                let text = self.streaming.take().or(text);
                if let Some(text) = text {
                    if !text.is_empty() {
                        self.push(Style::default().fg(Color::Green), text);
                    }
                }
                for call in tool_calls {
                    self.push(
                        Style::default().fg(Color::Yellow),
                        format!("[tool] requested {} ({})", call.name, call.call_id),
                    );
                }
            }
            RunEvent::ModelUsage {
                prompt_tokens,
                completion_tokens,
                total_tokens,
            } => {
                self.push(
                    Style::default().fg(Color::DarkGray),
                    format!(
                        "[usage] prompt={prompt_tokens} completion={completion_tokens} total={total_tokens}"
                    ),
                );
            }
            RunEvent::ApprovalRequested {
                tool_name, call_id, ..
            } => {
                self.push(
                    Style::default().fg(Color::DarkGray),
                    format!("[approval] {tool_name} ({call_id})"),
                );
            }
            RunEvent::ApprovalDecided { call_id, approved } => {
                self.push(
                    Style::default().fg(Color::DarkGray),
                    format!(
                        "[approval] {call_id} {}",
                        if approved { "approved" } else { "rejected" }
                    ),
                );
            }
            RunEvent::ToolCallStarted { call_id, name } => {
                self.push(
                    Style::default().fg(Color::Yellow),
                    format!("[tool] {name} started ({call_id})"),
                );
            }
            RunEvent::ToolResult {
                name,
                is_error,
                outcome_unknown,
                content,
                ..
            } => {
                let style = if outcome_unknown {
                    Style::default().fg(Color::Magenta)
                } else if is_error {
                    Style::default().fg(Color::Red)
                } else {
                    Style::default().fg(Color::Yellow)
                };
                let status = if outcome_unknown {
                    "unknown"
                } else if is_error {
                    "error"
                } else {
                    "ok"
                };
                self.push(style, format!("[tool] {name} {status}: {content}"));
            }
            RunEvent::RunCompleted { answer } => {
                if let Some(text) = self.streaming.take() {
                    if !text.is_empty() {
                        self.push(Style::default().fg(Color::Green), text);
                    }
                }
                self.status = "completed".to_owned();
                self.push(
                    Style::default().fg(Color::DarkGray),
                    format!("[run] completed: {}", answer.unwrap_or_default()),
                );
            }
            RunEvent::RunCancelled => {
                self.status = "cancelled".to_owned();
                self.push(Style::default().fg(Color::Yellow), "[run] cancelled");
            }
            RunEvent::RunFailed { message } => {
                self.status = "failed".to_owned();
                self.push(
                    Style::default().fg(Color::Red),
                    format!("[run] failed: {message}"),
                );
            }
        }
    }
}

impl State {
    fn handle_key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }
        if !self.approval.is_empty() {
            let decision = match key.code {
                KeyCode::Char('y' | 'Y') => Some(ApprovalDecision::Approved),
                KeyCode::Char('n' | 'N') | KeyCode::Esc => Some(ApprovalDecision::Rejected),
                _ => None,
            };
            if let Some(decision) = decision {
                if let Some((_, sender)) = self.approval.pop_front() {
                    let _ = sender.send(decision);
                }
                self.status = if self.approval.is_empty() {
                    "running".to_owned()
                } else {
                    "awaiting approval".to_owned()
                };
            }
            return;
        }
        match key.code {
            KeyCode::Esc => {
                if self.running {
                    if let Some(cancel) = &self.cancel {
                        cancel.cancel();
                        self.status = "cancelling".to_owned();
                    }
                } else {
                    self.should_quit = true;
                }
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true;
            }
            KeyCode::Enter => self.submit(),
            KeyCode::Backspace => self.backspace(),
            KeyCode::Delete => self.delete_forward(),
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => {
                self.cursor = (self.cursor + 1).min(self.input.chars().count());
            }
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.input.chars().count(),
            KeyCode::Up => {
                if !self.recall_history(-1) {
                    self.scroll_back = self.scroll_back.saturating_add(1);
                }
            }
            KeyCode::Down => {
                if !self.recall_history(1) {
                    self.scroll_back = self.scroll_back.saturating_sub(1);
                }
            }
            KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.insert_char(character);
            }
            KeyCode::PageUp => self.scroll_back = self.scroll_back.saturating_add(10),
            KeyCode::PageDown => self.scroll_back = self.scroll_back.saturating_sub(10),
            _ => {}
        }
    }

    fn byte_index(&self, char_index: usize) -> usize {
        self.input
            .char_indices()
            .nth(char_index)
            .map(|(index, _)| index)
            .unwrap_or(self.input.len())
    }

    fn insert_char(&mut self, character: char) {
        let index = self.byte_index(self.cursor);
        self.input.insert(index, character);
        self.cursor += 1;
        self.history_index = None;
    }

    fn insert_text(&mut self, text: &str) {
        for character in text.chars().filter(|character| *character != '\r') {
            self.insert_char(character);
        }
    }

    fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let start = self.byte_index(self.cursor - 1);
        let end = self.byte_index(self.cursor);
        self.input.replace_range(start..end, "");
        self.cursor -= 1;
    }

    fn delete_forward(&mut self) {
        if self.cursor >= self.input.chars().count() {
            return;
        }
        let start = self.byte_index(self.cursor);
        let end = self.byte_index(self.cursor + 1);
        self.input.replace_range(start..end, "");
    }

    /// Recall shell-style input history. Returns false when there is nothing
    /// to recall, so the caller can fall back to scrolling.
    fn recall_history(&mut self, direction: isize) -> bool {
        if self.history.is_empty() {
            return false;
        }
        if direction < 0 {
            let index = match self.history_index {
                Some(index) => index.saturating_sub(1),
                None => self.history.len() - 1,
            };
            self.history_index = Some(index);
            self.input = self.history[index].clone();
        } else {
            match self.history_index {
                Some(index) if index + 1 < self.history.len() => {
                    self.history_index = Some(index + 1);
                    self.input = self.history[index + 1].clone();
                }
                Some(_) => {
                    self.history_index = None;
                    self.input.clear();
                }
                None => return false,
            }
        }
        self.cursor = self.input.chars().count();
        true
    }

    fn submit(&mut self) {
        if self.running || !self.approval.is_empty() {
            return;
        }
        let prompt = self.input.trim().to_owned();
        if prompt.is_empty() {
            return;
        }
        self.input.clear();
        self.cursor = 0;
        self.history_index = None;
        self.history.push(prompt.clone());
        if let Some(command) = parse_command(&prompt) {
            self.handle_command(command);
            return;
        }
        self.start_task(AgentTask::Turn(prompt));
    }

    fn handle_command(&mut self, command: TuiCommand) {
        match command {
            TuiCommand::Resume(run_id) => {
                let run_id = run_id.or_else(|| {
                    self.session
                        .as_ref()
                        .and_then(|session| session.last_run_id().cloned())
                });
                match run_id {
                    Some(run_id) => self.start_task(AgentTask::Resume(run_id)),
                    None => self.push(
                        Style::default().fg(Color::Red),
                        "usage: /resume <run-id> (this session has no recorded run)",
                    ),
                }
            }
            TuiCommand::Session => {
                let Some(session) = &self.session else {
                    self.status = "session busy".to_owned();
                    return;
                };
                let last_run = session
                    .last_run_id()
                    .map(|id| id.to_string())
                    .unwrap_or_else(|| "none".to_owned());
                self.push(
                    Style::default().fg(Color::DarkGray),
                    format!(
                        "[session] {} | turns {} | last run {last_run}",
                        session.session_id(),
                        session.turn_index()
                    ),
                );
            }
            TuiCommand::Help => self.push(
                Style::default().fg(Color::DarkGray),
                "commands: /resume [run-id] continue a journaled Run | /session show id and turns | /help",
            ),
        }
    }

    fn start_task(&mut self, task: AgentTask) {
        if self.running || !self.approval.is_empty() {
            return;
        }
        let Some(mut session) = self.session.take() else {
            self.status = "session busy".to_owned();
            return;
        };
        match &task {
            AgentTask::Turn(prompt) => {
                self.push(Style::default().fg(Color::Cyan), format!("> {prompt}"));
            }
            AgentTask::Resume(run_id) => {
                self.push(
                    Style::default().fg(Color::Cyan),
                    format!("/resume {run_id}"),
                );
            }
        }
        self.running = true;
        self.status = "running".to_owned();
        self.scroll_back = 0;
        let cancel = CancelFlag::new();
        self.cancel = Some(cancel.clone());
        let timeout = if self.config.model == "local echo" {
            None
        } else {
            Some(Duration::from_secs(120))
        };
        let options = RunOptions {
            cancel,
            effect_timeout: timeout,
            ..RunOptions::default()
        };
        let sender = self.sender.clone();
        std::thread::spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    let _ = sender.send(UiMessage::Finished {
                        session: Box::new(session),
                        result: Err(format!("cannot start runtime: {error}")),
                    });
                    return;
                }
            };
            runtime.block_on(async move {
                let event_sender = sender.clone();
                let mut forward = move |event: &RunEvent| {
                    let _ = event_sender.send(UiMessage::Event(event.clone()));
                };
                let result = match task {
                    AgentTask::Turn(prompt) => session
                        .run_turn_with_options(&prompt, options, &mut forward)
                        .await
                        .map(|_| ()),
                    AgentTask::Resume(run_id) => session
                        .agent()
                        .resume_with_options(&run_id, options, &mut forward)
                        .await
                        .map(|_| ()),
                }
                .map_err(|error| error.to_string());
                let _ = sender.send(UiMessage::Finished {
                    session: Box::new(session),
                    result,
                });
            });
        });
    }
}

enum AgentTask {
    Turn(String),
    Resume(RunId),
}

enum TuiCommand {
    Resume(Option<RunId>),
    Session,
    Help,
}

fn parse_command(input: &str) -> Option<TuiCommand> {
    let mut parts = input.split_whitespace();
    match parts.next()? {
        "/resume" => Some(TuiCommand::Resume(parts.next().map(RunId::new))),
        "/session" => Some(TuiCommand::Session),
        "/help" => Some(TuiCommand::Help),
        _ => None,
    }
}

fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    for raw in text.split('\n') {
        if raw.is_empty() {
            lines.push(String::new());
            continue;
        }
        let characters: Vec<char> = raw.chars().collect();
        for chunk in characters.chunks(width) {
            lines.push(chunk.iter().collect());
        }
    }
    lines
}

fn visible_range(total: usize, view: usize, scroll_back: usize) -> std::ops::Range<usize> {
    let end = total.saturating_sub(scroll_back.min(total));
    let start = end.saturating_sub(view);
    start..end
}

fn wrap_chat_lines(messages: &[ChatLine], width: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for message in messages {
        for part in wrap_text(&message.text, width) {
            lines.push(Line::from(Span::styled(part, message.style)));
        }
    }
    lines
}

impl State {
    /// Refresh the wrapped-line cache and return only the visible window, so a
    /// long conversation does not get re-wrapped or cloned every frame.
    fn visible_lines(
        &mut self,
        width: usize,
        view: usize,
        scroll_back: usize,
    ) -> Vec<Line<'static>> {
        if width != self.wrapped_width || self.lines.len() < self.wrapped_count {
            self.wrapped = wrap_chat_lines(&self.lines, width);
            self.wrapped_width = width;
            self.wrapped_count = self.lines.len();
        } else if self.lines.len() > self.wrapped_count {
            let fresh = wrap_chat_lines(&self.lines[self.wrapped_count..], width);
            self.wrapped.extend(fresh);
            self.wrapped_count = self.lines.len();
        }
        let tail: Vec<Line<'static>> = self
            .streaming
            .as_deref()
            .map(|stream| {
                wrap_text(stream, width)
                    .into_iter()
                    .map(|part| Line::from(Span::styled(part, Style::default().fg(Color::Green))))
                    .collect()
            })
            .unwrap_or_default();
        let total = self.wrapped.len() + tail.len();
        let range = visible_range(total, view, scroll_back);
        let mut visible = Vec::with_capacity(range.len());
        for index in range {
            if index < self.wrapped.len() {
                visible.push(self.wrapped[index].clone());
            } else {
                visible.push(tail[index - self.wrapped.len()].clone());
            }
        }
        visible
    }
}

fn history_lines(history: &[HistoryItem]) -> Vec<ChatLine> {
    let mut lines = Vec::new();
    for item in history {
        match item {
            HistoryItem::User(text) => lines.push(ChatLine {
                style: Style::default().fg(Color::Cyan),
                text: format!("> {text}"),
            }),
            HistoryItem::Assistant { text, tool_calls } => {
                if let Some(text) = text {
                    if !text.is_empty() {
                        lines.push(ChatLine {
                            style: Style::default().fg(Color::Green),
                            text: text.clone(),
                        });
                    }
                }
                for call in tool_calls {
                    lines.push(ChatLine {
                        style: Style::default().fg(Color::Yellow),
                        text: format!("[tool] requested {} ({})", call.name, call.call_id),
                    });
                }
            }
            HistoryItem::Tool(outcome) => {
                let (style, status) = if outcome.outcome_unknown {
                    (Style::default().fg(Color::Magenta), "unknown")
                } else if outcome.is_error {
                    (Style::default().fg(Color::Red), "error")
                } else {
                    (Style::default().fg(Color::Yellow), "ok")
                };
                lines.push(ChatLine {
                    style,
                    text: format!("[tool] {} {status}: {}", outcome.name, outcome.content),
                });
            }
        }
    }
    lines
}

fn truncate(text: &str, max: usize) -> String {
    let mut shortened: String = text.chars().take(max).collect();
    if text.chars().count() > max {
        shortened.push('…');
    }
    shortened
}

impl State {
    fn draw(&mut self, frame: &mut Frame) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Min(4),
                Constraint::Length(3),
                Constraint::Length(1),
            ])
            .split(frame.area());

        let modes = if self.config.read_only {
            "read-only"
        } else if self.config.auto_approve {
            "auto-approve"
        } else {
            "approvals"
        };
        let mut header = format!(
            " Autra | {} | {} | {}",
            self.config.workspace, self.config.model, modes
        );
        if let Some(label) = &self.session_label {
            header.push_str(&format!(" | {label} turn {}", self.turn_index));
        }
        header.push(' ');
        frame.render_widget(
            Paragraph::new(header).style(
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            chunks[0],
        );

        let conversation = Block::default().borders(Borders::ALL).title("Conversation");
        let inner = conversation.inner(chunks[1]);
        let width = inner.width as usize;
        let view = inner.height as usize;
        let visible = self.visible_lines(width, view, self.scroll_back);
        frame.render_widget(Paragraph::new(visible).block(conversation), chunks[1]);

        if let Some((request, _)) = self.approval.front() {
            let preview = request.preview.as_deref().unwrap_or("");
            let text = format!(
                "Approve `{}` {}? [y/N]  {}",
                request.tool_name,
                request.call_id,
                truncate(preview, 160)
            );
            frame.render_widget(
                Paragraph::new(text)
                    .block(Block::default().borders(Borders::ALL).title("Approval"))
                    .style(Style::default().fg(Color::Yellow)),
                chunks[2],
            );
        } else {
            let text = format!("> {}", self.input);
            frame.render_widget(
                Paragraph::new(text).block(Block::default().borders(Borders::ALL).title("Input")),
                chunks[2],
            );
            if !self.running {
                let x = chunks[2].x + 3 + self.cursor as u16;
                let y = chunks[2].y + 1;
                frame.set_cursor_position(Position::new(
                    x.min(chunks[2].right().saturating_sub(2)),
                    y,
                ));
            }
        }

        let hint = if self.running {
            "Esc cancel | Ctrl+C quit | PgUp/PgDn scroll"
        } else {
            "Enter send | Up/Down history | /resume /session /help | Esc quit | PgUp/PgDn scroll"
        };
        frame.render_widget(
            Paragraph::new(format!(" {} | {}", self.status, hint))
                .style(Style::default().fg(Color::DarkGray)),
            chunks[3],
        );
    }
}

pub fn run(
    agent: autra::Agent,
    config: TuiConfig,
    sender: Sender<UiMessage>,
    receiver: Receiver<UiMessage>,
) -> io::Result<()> {
    let session = match &config.session {
        Some(binding) => Session::open(agent, binding.store.clone(), binding.session_id.clone())
            .map_err(|error| io::Error::other(error.to_string()))?,
        None => Session::new(agent),
    };
    let mut state = State::new(session, config, sender);
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableBracketedPaste)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;
    let result = event_loop(&mut terminal, &mut state, receiver);
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableBracketedPaste
    )?;
    terminal.show_cursor()?;
    result
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    state: &mut State,
    receiver: Receiver<UiMessage>,
) -> io::Result<()> {
    loop {
        while let Ok(message) = receiver.try_recv() {
            state.handle_message(message);
        }
        terminal.draw(|frame| state.draw(frame))?;
        if event::poll(Duration::from_millis(50))? {
            match event::read()? {
                Event::Key(key) => state.handle_key(key),
                Event::Paste(text) => state.insert_text(&text),
                _ => {}
            }
        }
        if state.should_quit {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use autra::Agent;
    use autra_core::{EffectId, RunId, ToolCallRequest, ToolOutcome};
    use autra_harness::EchoModel;

    fn state() -> State {
        let agent = Agent::builder("tui").model(EchoModel).build().unwrap();
        let (sender, _receiver) = std::sync::mpsc::channel();
        State::new(
            Session::new(agent),
            TuiConfig {
                workspace: ".".to_owned(),
                model: "local echo".to_owned(),
                read_only: false,
                auto_approve: false,
                session: None,
            },
            sender,
        )
    }

    #[test]
    fn wrap_text_chunks_long_lines() {
        assert_eq!(wrap_text("abcdef", 4), ["abcd", "ef"]);
        assert_eq!(wrap_text("a\n\nb", 4), ["a", "", "b"]);
    }

    #[test]
    fn visible_range_is_a_tail() {
        assert_eq!(visible_range(10, 3, 0), 7..10);
        assert_eq!(visible_range(10, 3, 2), 5..8);
        assert_eq!(visible_range(2, 5, 0), 0..2);
        assert_eq!(visible_range(2, 5, 9), 0..0);
    }

    #[test]
    fn deltas_become_one_assistant_message() {
        let mut state = state();
        state.handle_event(RunEvent::ModelDelta {
            text: "he".to_owned(),
        });
        state.handle_event(RunEvent::ModelDelta {
            text: "llo".to_owned(),
        });
        assert!(state.lines.is_empty());
        state.handle_event(RunEvent::ModelResult {
            text: Some("hello".to_owned()),
            tool_calls: Vec::new(),
        });
        assert_eq!(state.lines.len(), 1);
        assert_eq!(state.lines[0].text, "hello");
        assert!(state.streaming.is_none());
    }

    #[test]
    fn approving_with_y_sends_approved() {
        let mut state = state();
        let (sender, receiver) = std::sync::mpsc::channel();
        let request = ApprovalRequest::new(
            &RunId::new("run"),
            &EffectId::fresh(),
            &ToolCallRequest {
                call_id: "call".to_owned(),
                name: "write_file".to_owned(),
                arguments: "{}".to_owned(),
            },
        );
        state.handle_message(UiMessage::Approval(request, sender));

        state.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));

        assert_eq!(receiver.recv().unwrap(), ApprovalDecision::Approved);
        assert!(state.approval.is_empty());
    }

    #[test]
    fn queued_approvals_are_decided_one_at_a_time() {
        let mut state = state();
        let (first_sender, first_receiver) = std::sync::mpsc::channel();
        let (second_sender, second_receiver) = std::sync::mpsc::channel();
        let request = |call_id: &str| {
            ApprovalRequest::new(
                &RunId::new("run"),
                &EffectId::fresh(),
                &ToolCallRequest {
                    call_id: call_id.to_owned(),
                    name: "write_file".to_owned(),
                    arguments: "{}".to_owned(),
                },
            )
        };
        state.handle_message(UiMessage::Approval(request("call-1"), first_sender));
        state.handle_message(UiMessage::Approval(request("call-2"), second_sender));

        state.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
        assert_eq!(first_receiver.recv().unwrap(), ApprovalDecision::Approved);
        assert_eq!(state.approval.len(), 1);

        state.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
        assert_eq!(second_receiver.recv().unwrap(), ApprovalDecision::Rejected);
        assert!(state.approval.is_empty());
    }

    #[test]
    fn restored_history_renders_each_item_kind() {
        let history = vec![
            HistoryItem::User("hi".to_owned()),
            HistoryItem::Assistant {
                text: Some("hello".to_owned()),
                tool_calls: vec![ToolCallRequest {
                    call_id: "call-1".to_owned(),
                    name: "echo".to_owned(),
                    arguments: "{}".to_owned(),
                }],
            },
            HistoryItem::Tool(ToolOutcome {
                call_id: "call-1".to_owned(),
                name: "echo".to_owned(),
                content: "hi".to_owned(),
                is_error: false,
                outcome_unknown: false,
            }),
        ];

        let lines = history_lines(&history);

        assert_eq!(lines[0].text, "> hi");
        assert_eq!(lines[1].text, "hello");
        assert_eq!(lines[2].text, "[tool] requested echo (call-1)");
        assert_eq!(lines[3].text, "[tool] echo ok: hi");
    }

    #[test]
    fn an_empty_session_starts_without_restored_lines() {
        let state = state();

        assert!(state.lines.is_empty());
        assert_eq!(state.turn_index, 0);
        assert!(state.session.is_some());
    }

    #[test]
    fn commands_are_parsed_before_a_turn() {
        match parse_command("/resume run-1") {
            Some(TuiCommand::Resume(Some(run_id))) => {
                assert_eq!(run_id.to_string(), "run-1");
            }
            _ => panic!("expected /resume with a run id"),
        }
        assert!(matches!(
            parse_command("/resume"),
            Some(TuiCommand::Resume(None))
        ));
        assert!(matches!(
            parse_command("/session"),
            Some(TuiCommand::Session)
        ));
        assert!(matches!(parse_command("/help"), Some(TuiCommand::Help)));
        assert!(parse_command("hello").is_none());
    }

    #[test]
    fn the_session_command_reports_id_and_turn_count() {
        let mut state = state();
        state.input = "/session".to_owned();

        state.submit();

        let line = state.lines.last().unwrap();
        assert!(line.text.starts_with("[session]"));
        assert!(line.text.contains("turns 0"));
        assert!(line.text.contains("last run none"));
    }

    #[test]
    fn resume_without_a_recorded_run_explains_the_usage() {
        let mut state = state();
        state.input = "/resume".to_owned();

        state.submit();

        assert!(state.lines.last().unwrap().text.contains("usage: /resume"));
    }

    #[test]
    fn input_editing_uses_the_cursor() {
        let mut state = state();
        for character in "ac".chars() {
            state.handle_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
        }
        state.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        state.handle_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE));
        assert_eq!(state.input, "abc");
        assert_eq!(state.cursor, 2);

        state.handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        state.handle_key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));
        assert_eq!(state.input, "bc");
        assert_eq!(state.cursor, 0);

        state.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        state.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(state.input, "b");
    }

    #[test]
    fn history_recalls_previous_input() {
        let mut state = state();
        state.history = vec!["first".to_owned(), "second".to_owned()];

        state.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(state.input, "second");
        state.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(state.input, "first");
        state.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(state.input, "second");
        state.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(state.input, "");
    }

    #[test]
    fn paste_inserts_text_at_the_cursor() {
        let mut state = state();

        state.insert_text("你好\r\nworld");

        assert_eq!(state.input, "你好\nworld");
        assert_eq!(state.cursor, 8);
    }
}
