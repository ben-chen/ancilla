use ancilla::cli::{Cli, run};
use clap::Parser;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let config = ancilla::config::AppConfig::from_env();
    run(cli, config).await
}
