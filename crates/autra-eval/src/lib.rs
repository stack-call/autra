//! Task-level evaluation for Autra agents.
//!
//! Evaluation measures Agent outcomes. Runtime correctness (duplicate effects,
//! orphan tasks, recovery) is covered by the runtime fault tests instead.

use autra::Agent;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvalTask {
    pub name: String,
    pub input: String,
    pub expected_contains: String,
}

impl EvalTask {
    pub fn new(
        name: impl Into<String>,
        input: impl Into<String>,
        expected_contains: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            input: input.into(),
            expected_contains: expected_contains.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvalResult {
    pub name: String,
    pub passed: bool,
    pub answer: Option<String>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct EvalReport {
    pub results: Vec<EvalResult>,
}

impl EvalReport {
    pub fn passed(&self) -> usize {
        self.results.iter().filter(|result| result.passed).count()
    }

    pub fn failed(&self) -> usize {
        self.results.len() - self.passed()
    }

    pub fn pass_rate(&self) -> f64 {
        if self.results.is_empty() {
            0.0
        } else {
            self.passed() as f64 / self.results.len() as f64
        }
    }
}

pub async fn run_tasks(agent: &Agent, tasks: &[EvalTask]) -> EvalReport {
    let mut results = Vec::with_capacity(tasks.len());
    for task in tasks {
        match agent.run(&task.input).await {
            Ok(outcome) => {
                let answer = outcome.answer().map(str::to_owned);
                let passed = answer
                    .as_deref()
                    .is_some_and(|answer| answer.contains(&task.expected_contains));
                results.push(EvalResult {
                    name: task.name.clone(),
                    passed,
                    answer,
                    error: None,
                });
            }
            Err(error) => results.push(EvalResult {
                name: task.name.clone(),
                passed: false,
                answer: None,
                error: Some(error.to_string()),
            }),
        }
    }
    EvalReport { results }
}
