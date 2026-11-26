//! Environment affinity and quotas, orthogonal to global resource pools.
use super::{AcquireError, FairPool, GlobalScheduler, ResourceKind, SchedulerPermit};
use autra_core::{CancelFlag, EnvironmentAffinity, ExecutionRequirements, RunId};
use autra_execution::{EnvironmentError, EnvironmentProvider, ExecutionEnvironment};
use std::sync::Arc;
pub(super) struct EnvironmentSlot {
    environment: Arc<dyn ExecutionEnvironment>,
    pool: Arc<FairPool>,
}
#[derive(Debug)]
pub enum ScheduleError {
    Resource(AcquireError),
    Environment(EnvironmentError),
}
impl std::fmt::Display for ScheduleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Resource(e) => write!(f, "resource admission failed: {e:?}"),
            Self::Environment(e) => e.fmt(f),
        }
    }
}
impl std::error::Error for ScheduleError {}
pub struct EffectPermit {
    pub environment: Option<Arc<dyn ExecutionEnvironment>>,
    _resource: SchedulerPermit,
    _environment: Option<SchedulerPermit>,
}
fn compatible(env: &dyn ExecutionEnvironment, req: &ExecutionRequirements) -> bool {
    let c = env.capabilities();
    env.protocol_version() == autra_execution::ENVIRONMENT_PROTOCOL_VERSION
        && (!req.file_api || c.file_api)
        && (!req.command_execution || c.command_execution)
}
fn unavailable(message: impl Into<String>) -> ScheduleError {
    ScheduleError::Environment(EnvironmentError::Failed(message.into()))
}
impl GlobalScheduler {
    pub fn with_environment_provider(mut self, provider: Arc<dyn EnvironmentProvider>) -> Self {
        self.environment_provider = Some(provider);
        self
    }
    pub fn register_environment(
        &self,
        environment: Arc<dyn ExecutionEnvironment>,
        concurrency: usize,
    ) -> Result<(), ScheduleError> {
        let mut es = self.environments.lock().expect("environment lock poisoned");
        if es.contains_key(environment.id()) {
            return Err(unavailable("environment identity already registered"));
        }
        es.insert(
            environment.id().to_owned(),
            EnvironmentSlot {
                environment,
                pool: Arc::new(FairPool::new(concurrency, self.max_waiters)),
            },
        );
        Ok(())
    }
    async fn find_environment(
        &self,
        id: &str,
        fallback: Option<&Arc<dyn ExecutionEnvironment>>,
    ) -> Result<Arc<dyn ExecutionEnvironment>, ScheduleError> {
        if let Some(env) = self
            .environments
            .lock()
            .expect("environment lock poisoned")
            .get(id)
            .map(|s| s.environment.clone())
        {
            return Ok(env);
        }
        if let Some(env) = fallback.filter(|e| e.id() == id) {
            return Ok(env.clone());
        }
        match &self.environment_provider {
            Some(p) => {
                let env = p.acquire(id).await.map_err(ScheduleError::Environment)?;
                if env.id() != id {
                    return Err(unavailable(
                        "provider returned a different environment identity",
                    ));
                }
                Ok(env)
            }
            None => Err(unavailable(format!(
                "required environment `{id}` is unavailable"
            ))),
        }
    }
    pub async fn acquire_effect(
        &self,
        resource: ResourceKind,
        run: &RunId,
        cancel: Option<&CancelFlag>,
        req: &ExecutionRequirements,
        fallback: Option<Arc<dyn ExecutionEnvironment>>,
    ) -> Result<EffectPermit, ScheduleError> {
        let environment = match &req.environment {
            None if !req.file_api && !req.command_execution => None,
            None => return Err(unavailable("environment capabilities require an affinity")),
            Some(EnvironmentAffinity::Required(id)) => {
                Some(self.find_environment(id, fallback.as_ref()).await?)
            }
            affinity => {
                let preferred = match affinity {
                    Some(EnvironmentAffinity::Preferred(id)) => self
                        .find_environment(id, fallback.as_ref())
                        .await
                        .ok()
                        .filter(|e| compatible(e.as_ref(), req)),
                    _ => None,
                };
                Some(
                    preferred
                        .or_else(|| {
                            self.environments
                                .lock()
                                .expect("environment lock poisoned")
                                .values()
                                .map(|s| s.environment.clone())
                                .find(|e| compatible(e.as_ref(), req))
                        })
                        .or_else(|| fallback.filter(|e| compatible(e.as_ref(), req)))
                        .ok_or_else(|| unavailable("no compatible execution environment"))?,
                )
            }
        };
        let environment_permit = if let Some(env) = &environment {
            if !compatible(env.as_ref(), req) {
                return Err(unavailable(format!(
                    "environment `{}` lacks required capabilities",
                    env.id()
                )));
            }
            let pool = {
                let mut es = self.environments.lock().expect("environment lock poisoned");
                es.entry(env.id().to_owned())
                    .or_insert_with(|| EnvironmentSlot {
                        environment: env.clone(),
                        pool: Arc::new(FairPool::new(self.environment_limit, self.max_waiters)),
                    })
                    .pool
                    .clone()
            };
            Some(
                pool.acquire(run.clone(), cancel)
                    .await
                    .map_err(ScheduleError::Resource)?,
            )
        } else {
            None
        };
        let permit = self
            .acquire(resource, run, cancel)
            .await
            .map_err(ScheduleError::Resource)?;
        Ok(EffectPermit {
            environment,
            _resource: permit,
            _environment: environment_permit,
        })
    }
}
