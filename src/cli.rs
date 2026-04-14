use std::{net::SocketAddr, path::PathBuf};

use anyhow::{Context, bail};
use axum::serve;
use clap::{Parser, Subcommand};
use tokio::net::TcpListener;
use uuid::Uuid;

use crate::{
    api,
    config::AppConfig,
    model::{
        ChatRespondRequest, CreateAudioEntryRequest, CreateTextEntryRequest, PatchMemoryRequest,
        SearchMemoriesRequest, empty_object,
    },
    service::AppService,
};

#[derive(Debug, Parser)]
#[command(name = "ancilla")]
#[command(about = "Personal LLM memory system")]
pub struct Cli {
    #[arg(long, global = true)]
    pub data_file: Option<PathBuf>,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    Server {
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

pub async fn run(cli: Cli, config: AppConfig) -> anyhow::Result<()> {
    let data_file = cli.data_file.clone().unwrap_or(config.data_file.clone());
    let snapshot_path = if config.database_url.is_some() {
        None
    } else {
        Some(data_file)
    };
    let service =
        AppService::load_with_config(snapshot_path, config.database_url.clone(), &config).await?;
    match cli.command {
        Command::Server { bind } => run_server(service, &bind).await,
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
    fn cli_parses_capture_and_server_commands() {
        let capture = Cli::parse_from([
            "ancilla",
            "--data-file",
            "/tmp/state.json",
            "capture",
            "--text",
            "I prefer Rust.",
        ]);
        assert!(matches!(capture.command, Command::Capture { .. }));
        assert_eq!(capture.data_file, Some(PathBuf::from("/tmp/state.json")));

        let server = Cli::parse_from(["ancilla", "server", "--bind", "127.0.0.1:4000"]);
        match server.command {
            Command::Server { bind } => assert_eq!(bind, "127.0.0.1:4000"),
            _ => panic!("expected server command"),
        }
    }
}
