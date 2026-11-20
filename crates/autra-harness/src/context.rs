use autra_core::{HistoryItem, ModelCallSpec, ToolSpec};
use thiserror::Error;

const TRUNCATE_TO: usize = 512;

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
            compact_content(&mut messages, max_chars);
        }

        Ok(ModelCallSpec {
            system: (!system.is_empty()).then_some(system),
            messages,
            tools: request.tools.to_vec(),
        })
    }
}

fn compact_content(messages: &mut [HistoryItem], max_chars: usize) {
    while content_len(messages) > max_chars {
        let mut changed = false;
        for item in messages.iter_mut() {
            if let HistoryItem::Tool(outcome) = item {
                if outcome.content.chars().count() > TRUNCATE_TO
                    && !outcome.content.contains("[context budget]")
                {
                    truncate(&mut outcome.content);
                    changed = true;
                    break;
                }
            }
        }
        if !changed {
            for item in messages.iter_mut() {
                if let HistoryItem::Assistant {
                    text: Some(text), ..
                } = item
                {
                    if text.chars().count() > TRUNCATE_TO && !text.contains("[context budget]") {
                        truncate(text);
                        changed = true;
                        break;
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }
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

fn truncate(text: &mut String) {
    let mut truncated: String = text.chars().take(TRUNCATE_TO).collect();
    truncated.push_str("\n[context budget]");
    *text = truncated;
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
}
