mod cmd;
mod internal;
mod util;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Cleanup of a locked binary runs before session setup: it must work regardless of
    // configuration, and it deliberately skips tracing and any other startup work.
    let args: Vec<String> = std::env::args().collect();
    if internal::dispatch(&args) {
        return Ok(());
    }

    cmd::start().await
}
