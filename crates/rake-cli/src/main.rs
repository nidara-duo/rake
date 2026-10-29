mod cmd;
mod util;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    if std::env::var_os("RAKE_LOG").is_some() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::builder()
                    .with_env_var("RAKE_LOG")
                    .from_env_lossy(),
            )
            .with_writer(std::io::stderr)
            .try_init();
    }

    // A binary renamed aside by a previous replacement is still on disk; this is the
    // first moment its lock is guaranteed to be released, so clear it before anything
    // else. Cheap, silent, and never fatal.
    rake_core::infra::self_replace::sweep_stale_artifacts();

    cmd::start().await
}
