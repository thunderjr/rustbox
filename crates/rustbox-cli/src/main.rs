#[tokio::main]
async fn main() -> anyhow::Result<()> {
    rustbox_cli::run().await
}
