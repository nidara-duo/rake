use std::sync::{Arc, RwLock};

use rake_domain::config::Config;

use crate::Result;
use crate::config_resolver;
use crate::event::EventBus;
use crate::infra::env::{EnvService, WindowsEnvService};
use crate::infra::http::{HttpClient, ReqwestClient};

#[derive(Clone)]
pub struct Session {
    inner: Arc<SessionInner>,
}

struct SessionInner {
    config: Config,
    event_bus: EventBus,
    http_client: Box<dyn HttpClient>,
    env_service: Box<dyn EnvService>,
    state_lock: RwLock<()>,
}

impl Session {
    pub async fn new() -> Result<Self> {
        let config = config_resolver::resolve_config()?;
        let event_bus = EventBus::new();
        let http_client = Box::new(ReqwestClient::new(
            config.proxy.as_deref(),
            Some("Rake/0.1.0 (+https://github.com/username/rake)"),
        )?);
        let env_service = Box::new(WindowsEnvService::new());

        Ok(Self {
            inner: Arc::new(SessionInner {
                config,
                event_bus,
                http_client,
                env_service,
                state_lock: RwLock::new(()),
            }),
        })
    }

    pub fn config(&self) -> &Config {
        &self.inner.config
    }

    pub fn event_bus(&self) -> &EventBus {
        &self.inner.event_bus
    }

    pub fn http_client(&self) -> &dyn HttpClient {
        self.inner.http_client.as_ref()
    }

    pub fn env_service(&self) -> &dyn EnvService {
        self.inner.env_service.as_ref()
    }

    pub fn read_lock(&self) -> Result<std::sync::RwLockReadGuard<'_, ()>> {
        self.inner
            .state_lock
            .read()
            .map_err(|_| crate::Error::Custom("state lock poisoned".into()))
    }

    pub fn write_lock(&self) -> Result<std::sync::RwLockWriteGuard<'_, ()>> {
        self.inner
            .state_lock
            .write()
            .map_err(|_| crate::Error::Custom("state lock poisoned".into()))
    }
}
