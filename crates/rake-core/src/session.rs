use std::sync::{Arc, RwLock};

use rake_domain::config::Config;

use crate::Result;
use crate::config_resolver;
use crate::event::EventBus;
use crate::infra::env::{EnvService, WindowsEnvService};
use crate::infra::git::GitService;
use crate::infra::git_libgit2::Git;
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
    git_service: Box<dyn GitService>,
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
        let git_service = Box::new(Git::new());

        Ok(Self {
            inner: Arc::new(SessionInner {
                config,
                event_bus,
                http_client,
                env_service,
                git_service,
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

    pub fn git_service(&self) -> &dyn GitService {
        self.inner.git_service.as_ref()
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

#[cfg(test)]
impl Session {
    pub fn from_config(config: Config) -> Self {
        Self {
            inner: Arc::new(SessionInner {
                config,
                event_bus: EventBus::new(),
                http_client: Box::new(
                    ReqwestClient::new(None, Some("test")).expect("build reqwest client"),
                ),
                env_service: Box::new(WindowsEnvService::new()),
                git_service: Box::new(Git::new()),
                state_lock: RwLock::new(()),
            }),
        }
    }

    /// A session whose environment writes are recorded instead of performed.
    ///
    /// [`Session::from_config`] installs the real `WindowsEnvService`, so any operation
    /// that sets or removes an environment variable — `uninstall` does, for every
    /// `env_set` key — would write to the actual HKCU\Environment. Tests use this so
    /// they cannot touch the user's environment.
    ///
    /// Note this does not cover PATH: `add_user_path`/`remove_user_path` are free
    /// functions, deliberately outside the trait. They only write when the entry is
    /// actually present, so a temp directory that was never added is a safe no-op.
    pub fn from_config_recording_env(config: Config) -> (Self, RecordingEnvService) {
        let recorder = RecordingEnvService::default();
        let handle = recorder.clone();
        let session = Self {
            inner: Arc::new(SessionInner {
                config,
                event_bus: EventBus::new(),
                http_client: Box::new(
                    ReqwestClient::new(None, Some("test")).expect("build reqwest client"),
                ),
                env_service: Box::new(handle),
                git_service: Box::new(Git::new()),
                state_lock: RwLock::new(()),
            }),
        };
        (session, recorder)
    }
}

/// Test double for [`EnvService`]: records what would have been written.
///
/// Cloneable and sharing one set of recorders, so a test can hold on to the handle
/// after the session has been handed to the code under test.
#[cfg(test)]
#[derive(Debug, Default, Clone)]
pub struct RecordingEnvService {
    pub set: Arc<std::sync::Mutex<Vec<(String, String)>>>,
    pub removed: Arc<std::sync::Mutex<Vec<String>>>,
}

#[cfg(test)]
#[async_trait::async_trait]
impl EnvService for RecordingEnvService {
    fn set_env(&self, key: &str, value: &str) -> Result<()> {
        self.set
            .lock()
            .expect("recorder poisoned")
            .push((key.to_owned(), value.to_owned()));
        Ok(())
    }

    fn remove_env(&self, key: &str) -> Result<()> {
        self.removed
            .lock()
            .expect("recorder poisoned")
            .push(key.to_owned());
        Ok(())
    }
}
