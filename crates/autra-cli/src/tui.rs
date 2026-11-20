//! A small terminal UI for a local chat session.

use std::io;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;

use async_trait::async_trait;
use autra::{Agent, ApprovalDecision, ApprovalRequest, Approver, RunEvent, RunOptions};
use autra_core::{CancelFlag, HistoryItem};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
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
}

pub enum UiMessage {
    Event(RunEvent),
    Approval(ApprovalRequest, Sender<ApprovalDecision>),
    Finished(Result<Vec<HistoryItem>, String>),
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
    agent: Arc<Agent>,
    config: TuiConfig,
    sender: Sender<UiMessage>,
    lines: Vec<ChatLine>,
    streaming: Option<String>,
    input: String,
    status: String,
    running: bool,
    scroll_back: usize,
    approval: Option<(ApprovalRequest, Sender<ApprovalDecision>)>,
    should_quit: bool,
    cancel: Option<CancelFlag>,
    history: Vec<HistoryItem>,
}

impl State {
    fn new(agent: Arc<Agent>, config: TuiConfig, sender: Sender<UiMessage>) -> Self {
        Self {
            agent,
            config,
            sender,
            lines: Vec::new(),
            streaming: None,
            input: String::new(),
            status: "idle".to_owned(),
            running: false,
            scroll_back: 0,
            approval: None,
            should_quit: false,
            cancel: None,
            history: Vec::new(),
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
                self.approval = Some((request, sender));
                self.status = "awaiting approval".to_owned();
            }
            UiMessage::Finished(result) => {
                self.running = false;
                self.cancel = None;
                self.status = "idle".to_owned();
                match result {
                    Ok(history) => self.history = history,
                    Err(error) => self.push(
                        Style::default().fg(Color::Red),
                        format!("run failed: {error}"),
                    ),
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
        if let Some((_, decision)) = &self.approval {
            match key.code {
                KeyCode::Char('y' | 'Y') => {
                    let _ = decision.send(ApprovalDecision::Approved);
                    self.approval = None;
                    self.status = "running".to_owned();
                }
                KeyCode::Char('n' | 'N') | KeyCode::Esc => {
                    let _ = decision.send(ApprovalDecision::Rejected);
                    self.approval = None;
                    self.status = "running".to_owned();
                }
                _ => {}
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
            KeyCode::Backspace => {
                self.input.pop();
            }
            KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.input.push(character);
            }
            KeyCode::PageUp => self.scroll_back = self.scroll_back.saturating_add(10),
            KeyCode::PageDown => self.scroll_back = self.scroll_back.saturating_sub(10),
            KeyCode::Up => self.scroll_back = self.scroll_back.saturating_add(1),
            KeyCode::Down => self.scroll_back = self.scroll_back.saturating_sub(1),
            _ => {}
        }
    }

    fn submit(&mut self) {
        if self.running || self.approval.is_some() {
            return;
        }
        let prompt = self.input.trim().to_owned();
        if prompt.is_empty() {
            return;
        }
        self.input.clear();
        self.push(Style::default().fg(Color::Cyan), format!("> {prompt}"));
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
        let agent = self.agent.clone();
        let sender = self.sender.clone();
        let history = self.history.clone();
        std::thread::spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    let _ = sender.send(UiMessage::Finished(Err(format!(
                        "cannot start runtime: {error}"
                    ))));
                    return;
                }
            };
            runtime.block_on(async move {
                let event_sender = sender.clone();
                let result = agent
                    .run_with_history(&history, &prompt, options, move |event| {
                        let _ = event_sender.send(UiMessage::Event(event.clone()));
                    })
                    .await;
                let message = match result {
                    Ok(outcome) => UiMessage::Finished(Ok(autra::react_history(outcome.state()))),
                    Err(error) => UiMessage::Finished(Err(error.to_string())),
                };
                let _ = sender.send(message);
            });
        });
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

fn build_lines(state: &State, width: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for message in &state.lines {
        for part in wrap_text(&message.text, width) {
            lines.push(Line::from(Span::styled(part, message.style)));
        }
    }
    if let Some(stream) = &state.streaming {
        for part in wrap_text(stream, width) {
            lines.push(Line::from(Span::styled(
                part,
                Style::default().fg(Color::Green),
            )));
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
        let header = format!(
            " Autra | {} | {} | {} ",
            self.config.workspace, self.config.model, modes
        );
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
        let content = build_lines(self, width);
        let view = inner.height as usize;
        let range = visible_range(content.len(), view, self.scroll_back);
        let visible: Vec<Line> = content[range].to_vec();
        frame.render_widget(Paragraph::new(visible).block(conversation), chunks[1]);

        if let Some((request, _)) = &self.approval {
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
                let x = chunks[2].x + 3 + self.input.chars().count() as u16;
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
            "Enter send | Esc quit | PgUp/PgDn scroll"
        };
        frame.render_widget(
            Paragraph::new(format!(" {} | {}", self.status, hint))
                .style(Style::default().fg(Color::DarkGray)),
            chunks[3],
        );
    }
}

pub fn run(
    agent: Arc<Agent>,
    config: TuiConfig,
    sender: Sender<UiMessage>,
    receiver: Receiver<UiMessage>,
) -> io::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;
    let mut state = State::new(agent, config, sender);
    let result = event_loop(&mut terminal, &mut state, receiver);
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
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
            if let Event::Key(key) = event::read()? {
                state.handle_key(key);
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
    use autra_core::{EffectId, RunId, ToolCallRequest};
    use autra_harness::EchoModel;

    fn state() -> State {
        let agent = Agent::builder("tui").model(EchoModel).build().unwrap();
        let (sender, _receiver) = std::sync::mpsc::channel();
        State::new(
            Arc::new(agent),
            TuiConfig {
                workspace: ".".to_owned(),
                model: "local echo".to_owned(),
                read_only: false,
                auto_approve: false,
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
        assert!(state.approval.is_none());
    }
}
