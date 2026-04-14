use ancilla::server_cli::{Cli, run};
use clap::Parser;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    run(Cli::parse()).await
}
