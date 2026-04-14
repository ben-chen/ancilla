use clap::{Parser, Subcommand};

use crate::{client, client_config::ClientConfig};

#[derive(Debug, Parser)]
#[command(name = "ancilla-client")]
#[command(about = "Ancilla remote ratatui client")]
pub struct Cli {
    #[arg(long, global = true)]
    pub base_url: Option<String>,
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    InitConfig {
        #[arg(long)]
        force: bool,
    },
    ShowConfig,
}

pub async fn run(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Some(Command::InitConfig { force }) => {
            println!(
                "{}",
                serde_json::to_string_pretty(&ClientConfig::init_user_config(force)?)?
            );
            Ok(())
        }
        Some(Command::ShowConfig) => {
            let config = ClientConfig::load()?;
            println!("{}", serde_json::to_string_pretty(&config.to_view())?);
            Ok(())
        }
        None => {
            let config = ClientConfig::load()?;
            client::run(cli.base_url, &config).await
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[test]
    fn cli_parses_client_commands() {
        let run = Cli::parse_from(["ancilla-client", "--base-url", "http://example.com"]);
        assert!(run.command.is_none());
        assert_eq!(run.base_url.as_deref(), Some("http://example.com"));

        let init_config = Cli::parse_from(["ancilla-client", "init-config", "--force"]);
        match init_config.command {
            Some(Command::InitConfig { force }) => assert!(force),
            _ => panic!("expected init-config command"),
        }

        let show_config = Cli::parse_from(["ancilla-client", "show-config"]);
        match show_config.command {
            Some(Command::ShowConfig) => {}
            _ => panic!("expected show-config command"),
        }
    }
}
