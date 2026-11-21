use autra_core::{HistoryItem, ModelCallSpec, ToolSpec};
use thiserror::Error;

const BUDGET_MARKER: &str = "\n[context budget]";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillSummary {
    pub name: String,
    pub description: String,
}

#[derive(Clone, Debug)]
pub struct ContextRequest<'a> {
    pub instructions: Option<&'a str>,
    pub history: &'a [HistoryItem],
    pub tools: &'a [ToolSpec],
}

#[derive(Debug, Error)]
#[error("{message}")]
pub struct ContextError {
    pub message: String,
}

impl ContextError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// Builds the model input view. It never changes authoritative execution state.
pub trait ContextBuilder: Send + Sync {
    fn build(&self, request: ContextRequest<'_>) -> Result<ModelCallSpec, ContextError>;
}

#[derive(Clone, Debug, Default)]
pub struct SimpleContextBuilder {
    skills: Vec<SkillSummary>,
    max_chars: Option<usize>,
}

impl SimpleContextBuilder {
    pub fn new(skills: Vec<SkillSummary>) -> Self {
        Self {
            skills,
            max_chars: None,
        }
    }

    pub fn with_max_chars(mut self, max_chars: usize) -> Self {
        self.max_chars = Some(max_chars);
        self
    }
}

impl ContextBuilder for SimpleContextBuilder {
    fn build(&self, request: ContextRequest<'_>) -> Result<ModelCallSpec, ContextError> {
        let mut system = request.instructions.unwrap_or_default().to_owned();
        if !self.skills.is_empty() {
            if !system.is_empty() {
                system.push_str("\n\n");
            }
            system.push_str("Available skills (call `load_skill` with a name):\n");
            for skill in &self.skills {
                system.push_str(&format!("- {}: {}\n", skill.name, skill.description));
            }
        }

        let mut messages = request.history.to_vec();
        if let Some(max_chars) = self.max_chars {
            let base = system.chars().count() + tool_chars(request.tools);
            if base >= max_chars {
                return Err(ContextError::new(format!(
                    "context budget {max_chars} cannot fit the system prompt and tool schemas ({base} chars)"
                )));
            }
            compact_content(&mut messages, max_chars - base)?;
        }

        Ok(ModelCallSpec {
            system: (!system.is_empty()).then_some(system),
            messages,
            tools: request.tools.to_vec(),
        })
    }
}

fn tool_chars(tools: &[ToolSpec]) -> usize {
    tools
        .iter()
        .map(|tool| {
            tool.name.chars().count()
                + tool.description.chars().count()
                + tool.input_schema.chars().count()
        })
        .sum()
}

fn compact_content(messages: &mut [HistoryItem], max_chars: usize) -> Result<(), ContextError> {
    let mut iterations = 0;
    while content_len(messages) > max_chars {
        iterations += 1;
        if iterations > 10_000 {
            break;
        }
        let excess = content_len(messages) - max_chars;
        if !shrink_largest(messages, excess) {
            return Err(ContextError::new(format!(
                "context budget exceeded: {} chars > {max_chars}",
                content_len(messages)
            )));
        }
    }
    Ok(())
}

/// Shrink the largest truncatable item by at least `excess` characters.
fn shrink_largest(messages: &mut [HistoryItem], excess: usize) -> bool {
    let mut best: Option<(usize, usize)> = None;
    for (index, item) in messages.iter().enumerate() {
        let length = match item {
            HistoryItem::User(text) if !text.contains(BUDGET_MARKER) => text.chars().count(),
            HistoryItem::Assistant {
                text: Some(text), ..
            } if !text.contains(BUDGET_MARKER) => text.chars().count(),
            HistoryItem::Tool(outcome) if !outcome.content.contains(BUDGET_MARKER) => {
                outcome.content.chars().count()
            }
            _ => 0,
        };
        if length > best.map_or(0, |(_, current)| current) {
            best = Some((index, length));
        }
    }
    let Some((index, length)) = best else {
        return false;
    };
    let target = length
        .saturating_sub(excess)
        .max(BUDGET_MARKER.chars().count());
    match &mut messages[index] {
        HistoryItem::User(text) => truncate_to(text, target),
        HistoryItem::Assistant {
            text: Some(text), ..
        } => truncate_to(text, target),
        HistoryItem::Tool(outcome) => truncate_to(&mut outcome.content, target),
        _ => return false,
    }
    true
}

fn truncate_to(text: &mut String, max_chars: usize) {
    if text.chars().count() <= max_chars {
        return;
    }
    let keep = max_chars.saturating_sub(BUDGET_MARKER.chars().count());
    let mut truncated: String = text.chars().take(keep).collect();
    truncated.push_str(BUDGET_MARKER);
    *text = truncated;
}

fn content_len(messages: &[HistoryItem]) -> usize {
    messages
        .iter()
        .map(|item| match item {
            HistoryItem::User(text) => text.chars().count(),
            HistoryItem::Assistant { text, tool_calls } => {
                text.as_deref()
                    .map(|text| text.chars().count())
                    .unwrap_or(0)
                    + tool_calls
                        .iter()
                        .map(|call| call.arguments.chars().count())
                        .sum::<usize>()
            }
            HistoryItem::Tool(outcome) => outcome.content.chars().count(),
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::{ContextBuilder, ContextRequest, SimpleContextBuilder, SkillSummary};
    use autra_core::{HistoryItem, ToolOutcome};

    #[test]
    fn includes_instructions_tools_and_skill_index() {
        let builder = SimpleContextBuilder::new(vec![SkillSummary {
            name: "rust".to_owned(),
            description: "Rust guidance".to_owned(),
        }]);
        let history = vec![HistoryItem::User("hello".to_owned())];

        let request = builder
            .build(ContextRequest {
                instructions: Some("be brief"),
                history: &history,
                tools: &[],
            })
            .unwrap();

        let system = request.system.unwrap();
        assert!(system.contains("be brief"));
        assert!(system.contains("Available skills"));
        assert!(system.contains("rust: Rust guidance"));
        assert_eq!(request.messages.len(), 1);
    }

    #[test]
    fn budget_truncates_content_but_keeps_tool_items() {
        let builder = SimpleContextBuilder::default().with_max_chars(64);
        let history = vec![
            HistoryItem::User("keep me".to_owned()),
            HistoryItem::Tool(ToolOutcome {
                call_id: "call-1".to_owned(),
                name: "read_file".to_owned(),
                content: "a".repeat(2000),
                is_error: false,
                outcome_unknown: false,
            }),
        ];

        let request = builder
            .build(ContextRequest {
                instructions: None,
                history: &history,
                tools: &[],
            })
            .unwrap();

        assert_eq!(request.messages.len(), 2);
        assert!(matches!(request.messages[0], HistoryItem::User(_)));
        match &request.messages[1] {
            HistoryItem::Tool(outcome) => {
                assert_eq!(outcome.call_id, "call-1");
                assert!(outcome.content.contains("[context budget]"));
            }
            other => panic!("expected a tool item, got {other:?}"),
        }
    }

    #[test]
    fn a_long_user_message_is_truncated_to_fit() {
        let builder = SimpleContextBuilder::default().with_max_chars(64);
        let history = vec![HistoryItem::User("a".repeat(2000))];

        let request = builder
            .build(ContextRequest {
                instructions: None,
                history: &history,
                tools: &[],
            })
            .unwrap();

        match &request.messages[0] {
            HistoryItem::User(text) => {
                assert!(text.chars().count() <= 64);
                assert!(text.contains("[context budget]"));
            }
            other => panic!("expected a user item, got {other:?}"),
        }
    }

    #[test]
    fn the_system_prompt_counts_toward_the_budget() {
        let builder = SimpleContextBuilder::default().with_max_chars(8);
        let history = vec![HistoryItem::User("hello".to_owned())];

        let error = builder
            .build(ContextRequest {
                instructions: Some("instructions that do not fit"),
                history: &history,
                tools: &[],
            })
            .unwrap_err();

        assert!(error.to_string().contains("cannot fit"));
    }
}
