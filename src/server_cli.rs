use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use anyhow::{Context, bail};
use axum::serve;
use clap::{Parser, Subcommand};
use tokio::net::TcpListener;
use uuid::Uuid;

use crate::{
    api,
    bedrock::build_chat_backend,
    model::{
        ChatRespondRequest, CreateAudioEntryRequest, CreateTextEntryRequest, PatchMemoryRequest,
        SearchMemoriesRequest, empty_object,
    },
    server_config::ServerConfig,
    service::AppService,
};

#[derive(Debug, Parser)]
#[command(name = "ancilla-server")]
#[command(about = "Ancilla server and admin CLI")]
pub struct Cli {
    #[arg(long, global = true)]
    pub data_file: Option<PathBuf>,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    Serve {
        #[arg(long, default_value = "127.0.0.1:3000")]
        bind: String,
    },
    Capture {
        #[arg(long)]
        text: Option<String>,
        #[arg(long)]
        audio_asset: Option<String>,
        #[arg(long)]
        transcript: Option<String>,
        #[arg(long, default_value = "UTC")]
        timezone: String,
        #[arg(long, default_value = "cli")]
        source_app: String,
    },
    Ask {
        message: String,
    },
    InitConfig {
        #[arg(long)]
        force: bool,
    },
    ShowConfig {
        #[arg(long)]
        show_secrets: bool,
    },
    Timeline,
    Review,
    Search {
        query: String,
    },
    Forget {
        id: Uuid,
    },
    PatchMemory {
        id: Uuid,
        #[arg(long)]
        display_text: Option<String>,
    },
}

pub async fn run(cli: Cli) -> anyhow::Result<()> {
    let Cli { data_file, command } = cli;
    match command {
        Command::InitConfig { force } => {
            return print_json(&ServerConfig::init_user_config(force)?);
        }
        Command::ShowConfig { show_secrets } => {
            let mut config = ServerConfig::load()?;
            if let Some(data_file) = data_file {
                config.data_file = data_file;
            }
            return print_json(&config.to_view(show_secrets));
        }
        Command::Serve { .. }
        | Command::Capture { .. }
        | Command::Ask { .. }
        | Command::Timeline
        | Command::Review
        | Command::Search { .. }
        | Command::Forget { .. }
        | Command::PatchMemory { .. } => {}
    }

    let config = ServerConfig::load()?;
    let data_file = data_file.unwrap_or(config.data_file.clone());
    let snapshot_path = if config.database_url.is_some() {
        None
    } else {
        Some(data_file)
    };
    let chat_backend: Arc<dyn crate::bedrock::ChatCompletionBackend> =
        build_chat_backend(&config).await?;
    let service = AppService::load_with_chat_backend(
        snapshot_path,
        config.database_url.clone(),
        chat_backend,
    )
    .await?;

    match command {
        Command::Serve { bind } => run_server(service, &bind).await,
        Command::Capture {
            text,
            audio_asset,
            transcript,
            timezone,
            source_app,
        } => {
            if let Some(text) = text {
                print_json(
                    &service
                        .create_text_entry(CreateTextEntryRequest {
                            raw_text: text,
                            captured_at: None,
                            timezone: Some(timezone),
                            source_app: Some(source_app),
                            prepared_artifacts: Vec::new(),
                            prepared_memories: Vec::new(),
                            metadata: empty_object(),
                        })
                        .await?,
                )
            } else if let Some(audio_asset) = audio_asset {
                print_json(
                    &service
                        .create_audio_entry(CreateAudioEntryRequest {
                            asset_ref: audio_asset,
                            transcript_text: transcript,
                            captured_at: None,
                            timezone: Some(timezone),
                            source_app: Some(source_app),
                            prepared_artifacts: Vec::new(),
                            prepared_memories: Vec::new(),
                            metadata: empty_object(),
                        })
                        .await?,
                )
            } else {
                bail!("either --text or --audio-asset is required");
            }
        }
        Command::Ask { message } => print_json(
            &service
                .chat_respond(ChatRespondRequest {
                    message,
                    model_id: None,
                    recent_turns: Vec::new(),
                    recent_context: None,
                    active_thread_id: None,
                    focus_from: None,
                    focus_to: None,
                    query_embedding: None,
                })
                .await?,
        ),
        Command::Timeline => print_json(&service.list_timeline().await),
        Command::Review => print_json(&service.review_memories().await),
        Command::Search { query } => print_json(
            &service
                .search_memories(SearchMemoriesRequest {
                    query,
                    recent_context: None,
                    focus_from: None,
                    focus_to: None,
                    active_thread_id: None,
                    kind: None,
                    subtype: None,
                    query_embedding: None,
                    limit: Some(10),
                })
                .await?,
        ),
        Command::Forget { id } => print_json(&service.delete_memory(id).await?),
        Command::PatchMemory { id, display_text } => print_json(
            &service
                .patch_memory(
                    id,
                    PatchMemoryRequest {
                        display_text,
                        retrieval_text: None,
                        attrs: None,
                        valid_to: None,
                        confidence: None,
                        salience: None,
                        state: None,
                        thread_id: None,
                    },
                )
                .await?,
        ),
        Command::InitConfig { .. } | Command::ShowConfig { .. } => {
            unreachable!("config command handled before service load")
        }
    }
}

async fn run_server(service: AppService, bind: &str) -> anyhow::Result<()> {
    let addr: SocketAddr = bind
        .parse()
        .with_context(|| format!("invalid bind address {bind}"))?;
    let listener = TcpListener::bind(addr).await?;
    serve(listener, api::router(service)).await?;
    Ok(())
}

fn print_json(value: &impl serde::Serialize) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[test]
    fn cli_parses_server_commands() {
        let capture = Cli::parse_from([
            "ancilla-server",
            "--data-file",
            "/tmp/state.json",
            "capture",
            "--text",
            "I prefer Rust.",
        ]);
        assert!(matches!(capture.command, Command::Capture { .. }));
        assert_eq!(capture.data_file, Some(PathBuf::from("/tmp/state.json")));

        let serve = Cli::parse_from(["ancilla-server", "serve", "--bind", "127.0.0.1:4000"]);
        match serve.command {
            Command::Serve { bind } => assert_eq!(bind, "127.0.0.1:4000"),
            _ => panic!("expected serve command"),
        }

        let init_config = Cli::parse_from(["ancilla-server", "init-config", "--force"]);
        match init_config.command {
            Command::InitConfig { force } => assert!(force),
            _ => panic!("expected init-config command"),
        }

        let show_config = Cli::parse_from(["ancilla-server", "show-config", "--show-secrets"]);
        match show_config.command {
            Command::ShowConfig { show_secrets } => assert!(show_secrets),
            _ => panic!("expected show-config command"),
        }
    }
}
