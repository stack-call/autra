//! Local execution host and re-exported host contracts.

pub use autra_execution::host::{ExecutionHost, HOST_PROTOCOL_VERSION, HostCapabilities};

/// The default in-process host. It owns no durable guarantee.
#[derive(Clone, Copy, Debug, Default)]
pub struct LocalHost;

impl ExecutionHost for LocalHost {
    fn capabilities(&self) -> HostCapabilities {
        HostCapabilities::default()
    }
}
