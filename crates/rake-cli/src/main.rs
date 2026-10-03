mod cmd;
mod util;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // A binary renamed aside by a previous replacement is still on disk; this is the
    // first moment its lock is guaranteed to be released, so clear it before anything
    // else. Cheap, silent, and never fatal.
    rake_core::infra::self_replace::sweep_stale_artifacts();

    cmd::start().await
}
