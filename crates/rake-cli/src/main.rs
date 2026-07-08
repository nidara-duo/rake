mod cmd;
mod util;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    cmd::start().await
}
