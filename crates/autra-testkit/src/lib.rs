//! Shared contract checks for extensions. These are protocol checks, not
//! behavioural test suites for a specific adapter.

use autra_core::{AgentStepper, STEPPER_PROTOCOL_VERSION};
use autra_harness::{ModelProvider, TOOL_PROTOCOL_VERSION, Tool};
use autra_runtime::{ExecutionHost, HOST_PROTOCOL_VERSION};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ContractCheck {
    pub subject: String,
    pub failures: Vec<String>,
}

impl ContractCheck {
    pub fn is_ok(&self) -> bool {
        self.failures.is_empty()
    }

    pub fn assert_ok(&self) {
        assert!(
            self.is_ok(),
            "contract check failed for {}: {}",
            self.subject,
            self.failures.join("; ")
        );
    }
}

pub fn check_tool(tool: &dyn Tool) -> ContractCheck {
    let descriptor = tool.descriptor();
    let mut failures = Vec::new();
    if descriptor.name.is_empty() {
        failures.push("descriptor name is empty".to_owned());
    }
    if descriptor.description.is_empty() {
        failures.push("descriptor description is empty".to_owned());
    }
    if descriptor.input_schema.is_empty() {
        failures.push("descriptor input schema is empty".to_owned());
    }
    if descriptor.name != tool.spec().name {
        failures.push("descriptor and spec names differ".to_owned());
    }
    if tool.protocol_version() != TOOL_PROTOCOL_VERSION {
        failures.push(format!(
            "tool protocol version {} != {}",
            tool.protocol_version(),
            TOOL_PROTOCOL_VERSION
        ));
    }
    let _ = tool.contract();
    ContractCheck {
        subject: format!("tool `{}`", descriptor.name),
        failures,
    }
}

pub fn check_model(provider: &dyn ModelProvider) -> ContractCheck {
    let mut failures = Vec::new();
    if provider.protocol_version() != 1 {
        failures.push(format!(
            "model protocol version {} != 1",
            provider.protocol_version()
        ));
    }
    let _ = provider.capabilities();
    ContractCheck {
        subject: "model provider".to_owned(),
        failures,
    }
}

pub fn check_stepper(stepper: &dyn AgentStepper) -> ContractCheck {
    let mut failures = Vec::new();
    if stepper.protocol_version() != STEPPER_PROTOCOL_VERSION {
        failures.push(format!(
            "stepper protocol version {} != {}",
            stepper.protocol_version(),
            STEPPER_PROTOCOL_VERSION
        ));
    }
    ContractCheck {
        subject: "agent stepper".to_owned(),
        failures,
    }
}

pub fn check_host(host: &dyn ExecutionHost) -> ContractCheck {
    let capabilities = host.capabilities();
    let mut failures = Vec::new();
    if capabilities.protocol_version != HOST_PROTOCOL_VERSION {
        failures.push(format!(
            "host protocol version {} != {}",
            capabilities.protocol_version, HOST_PROTOCOL_VERSION
        ));
    }
    ContractCheck {
        subject: "execution host".to_owned(),
        failures,
    }
}
