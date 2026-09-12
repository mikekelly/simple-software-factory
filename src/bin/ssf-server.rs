#[tokio::main]
async fn main() -> anyhow::Result<()> {
    ssf::server_main().await
}
