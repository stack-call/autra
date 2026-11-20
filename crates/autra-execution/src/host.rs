//! Execution host capability contract.

pub const HOST_PROTOCOL_VERSION: u32 = 1;

/// What a host guarantees. Unsupported operations must be reported as typed
/// errors rather than silently downgraded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostCapabilities {
    pub protocol_version: u32,
    pub durable_wait: bool,
    pub crash_recovery: bool,
    pub effect_reconciliation: bool,
    pub workspace_snapshot: bool,
}

impl Default for HostCapabilities {
    fn default() -> Self {
        Self {
            protocol_version: HOST_PROTOCOL_VERSION,
            durable_wait: false,
            crash_recovery: false,
            effect_reconciliation: false,
            workspace_snapshot: false,
        }
    }
}

pub trait ExecutionHost: Send + Sync {
    fn capabilities(&self) -> HostCapabilities;
}

/// The default in-process host. It owns no durable guarantee.
#[derive(Clone, Copy, Debug, Default)]
pub struct LocalHost;

impl ExecutionHost for LocalHost {
    fn capabilities(&self) -> HostCapabilities {
        HostCapabilities::default()
    }
}
