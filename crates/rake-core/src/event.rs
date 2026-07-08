use flume::{Receiver, Sender, bounded};
use rake_domain::package::PackageIdent;

const BUS_CAPACITY: usize = 256;

#[derive(Debug, Clone)]
pub enum BucketState {
    Started,
    Succeeded,
    Failed(String),
}

#[derive(Debug, Clone)]
pub enum Event {
    BucketSyncProgress { name: String, state: BucketState },
    BucketSyncDone,

    DownloadCached(PackageIdent),
    DownloadProgress(DownloadProgress),
    DownloadStart(PackageIdent),
    DownloadDone,
    DownloadError(PackageIdent, String),

    IntegrityCheckStart,
    IntegrityCheckDone,

    CommitStart(PackageIdent, String),
    CommitProgress(String),
    CommitDone(PackageIdent),

    UpdateStart(PackageIdent, String, String),
    UpdateProgress(String),
    UpdateDone(PackageIdent),

    NeedConfirm(ConfirmRequest),
    NeedCandidateSelect(Vec<String>),
}

#[derive(Debug, Clone)]
pub struct DownloadProgress {
    pub ident: PackageIdent,
    pub url: String,
    pub filename: String,
    pub total_bytes: u64,
    pub downloaded_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct ConfirmRequest {
    pub install: Vec<String>,
    pub upgrade: Vec<String>,
    pub replace: Vec<String>,
    pub remove: Vec<String>,
    pub total_download_size: u64,
    pub estimated: bool,
}

#[derive(Debug, Clone)]
pub struct EventBus {
    core_tx: Sender<Event>,
    core_rx: Receiver<Event>,
    cli_tx: Sender<Event>,
    cli_rx: Receiver<Event>,
}

impl EventBus {
    pub fn new() -> Self {
        let (core_tx, core_rx) = bounded(BUS_CAPACITY);
        let (cli_tx, cli_rx) = bounded(BUS_CAPACITY);
        Self {
            core_tx,
            core_rx,
            cli_tx,
            cli_rx,
        }
    }

    pub fn core_sender(&self) -> Sender<Event> {
        self.core_tx.clone()
    }

    pub fn core_receiver(&self) -> Receiver<Event> {
        self.core_rx.clone()
    }

    pub fn cli_sender(&self) -> Sender<Event> {
        self.cli_tx.clone()
    }

    pub fn cli_receiver(&self) -> Receiver<Event> {
        self.cli_rx.clone()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}
