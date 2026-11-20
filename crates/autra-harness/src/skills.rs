use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use autra_core::{EffectClass, ToolSpec};
use serde::Deserialize;

use crate::context::SkillSummary;
use crate::tools::{Tool, ToolError};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub instructions: String,
}

impl Skill {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        instructions: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            instructions: instructions.into(),
        }
    }
}

#[derive(Clone, Default)]
pub struct SkillRegistry {
    skills: BTreeMap<String, Skill>,
}

impl SkillRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, skill: Skill) -> &mut Self {
        self.skills.insert(skill.name.clone(), skill);
        self
    }

    pub fn index(&self) -> Vec<SkillSummary> {
        self.skills
            .values()
            .map(|skill| SkillSummary {
                name: skill.name.clone(),
                description: skill.description.clone(),
            })
            .collect()
    }

    pub fn get(&self, name: &str) -> Option<&Skill> {
        self.skills.get(name)
    }

    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }
}

/// Loads a named skill's instructions. Loading does not grant permissions.
pub struct LoadSkillTool {
    skills: Arc<SkillRegistry>,
}

impl LoadSkillTool {
    pub fn new(skills: Arc<SkillRegistry>) -> Self {
        Self { skills }
    }
}

#[derive(Deserialize)]
struct LoadSkillArgs {
    name: String,
}

const LOAD_SKILL_SCHEMA: &str = r#"{"type":"object","properties":{"name":{"type":"string","description":"Skill name from the available skills list"}},"required":["name"]}"#;

#[async_trait]
impl Tool for LoadSkillTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "load_skill".to_owned(),
            description: "Load the full instructions for a named skill.".to_owned(),
            input_schema: LOAD_SKILL_SCHEMA.to_owned(),
        }
    }

    fn effect_class(&self) -> EffectClass {
        EffectClass::ReadOnly
    }

    async fn call(&self, arguments: &str) -> Result<String, ToolError> {
        let args: LoadSkillArgs = serde_json::from_str(arguments)
            .map_err(|error| ToolError::new(format!("invalid load_skill arguments: {error}")))?;
        let skill = self
            .skills
            .get(&args.name)
            .ok_or_else(|| ToolError::new(format!("unknown skill `{}`", args.name)))?;
        Ok(format!("# Skill: {}\n\n{}", skill.name, skill.instructions))
    }
}

#[cfg(test)]
mod tests {
    use super::{LoadSkillTool, Skill, SkillRegistry};
    use crate::tools::Tool;
    use std::sync::Arc;

    #[tokio::test]
    async fn loads_registered_skill_instructions() {
        let mut registry = SkillRegistry::new();
        registry.register(Skill::new("rust", "Rust guidance", "Run cargo fmt."));
        let tool = LoadSkillTool::new(Arc::new(registry));

        let output = tool.call(r#"{"name":"rust"}"#).await.unwrap();

        assert!(output.contains("Run cargo fmt."));
    }

    #[tokio::test]
    async fn unknown_skill_is_an_error() {
        let tool = LoadSkillTool::new(Arc::new(SkillRegistry::new()));

        let error = tool.call(r#"{"name":"missing"}"#).await.unwrap_err();

        assert!(error.to_string().contains("unknown skill"));
    }
}
