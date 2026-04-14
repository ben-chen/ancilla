use std::{env, path::PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AppConfig {
    pub app_env: String,
    pub data_file: PathBuf,
    pub database_url: Option<String>,
    pub aws_region: String,
    pub aws_profile: Option<String>,
    pub bedrock_chat_model_id: Option<String>,
    pub bedrock_chat_max_tokens: i32,
    pub bedrock_chat_temperature: f32,
    pub accept_client_embeddings: bool,
    pub accept_client_transcripts: bool,
    pub local_embed_model: String,
    pub local_context_embed_model: String,
    pub local_embed_device: String,
}

impl AppConfig {
    pub fn from_env() -> Self {
        Self {
            app_env: env_var("ANCILLA_APP_ENV").unwrap_or_else(|| "development".to_string()),
            data_file: env_var("ANCILLA_DATA_FILE")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(".ancilla/state.json")),
            database_url: env_var("DATABASE_URL"),
            aws_region: env_var("AWS_REGION")
                .or_else(|| env_var("AWS_DEFAULT_REGION"))
                .unwrap_or_else(|| "us-west-2".to_string()),
            aws_profile: env_var("AWS_PROFILE"),
            bedrock_chat_model_id: env_var("BEDROCK_CHAT_MODEL_ID"),
            bedrock_chat_max_tokens: env_i32("BEDROCK_CHAT_MAX_TOKENS", 800),
            bedrock_chat_temperature: env_f32("BEDROCK_CHAT_TEMPERATURE", 0.2),
            accept_client_embeddings: env_bool("ANCILLA_ACCEPT_CLIENT_EMBEDDINGS", true),
            accept_client_transcripts: env_bool("ANCILLA_ACCEPT_CLIENT_TRANSCRIPTS", true),
            local_embed_model: env_var("ANCILLA_LOCAL_EMBED_MODEL")
                .unwrap_or_else(|| "perplexity-ai/pplx-embed-v1-0.6b".to_string()),
            local_context_embed_model: env_var("ANCILLA_LOCAL_CONTEXT_EMBED_MODEL")
                .unwrap_or_else(|| "perplexity-ai/pplx-embed-context-v1-0.6b".to_string()),
            local_embed_device: env_var("ANCILLA_LOCAL_EMBED_DEVICE")
                .unwrap_or_else(|| "auto".to_string()),
        }
    }
}

fn env_var(key: &str) -> Option<String> {
    env::var(key).ok().filter(|value| !value.trim().is_empty())
}

fn env_bool(key: &str, default: bool) -> bool {
    env_var(key)
        .map(|value| {
            matches!(
                value.to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(default)
}

fn env_i32(key: &str, default: i32) -> i32 {
    env_var(key)
        .and_then(|value| value.parse::<i32>().ok())
        .unwrap_or(default)
}

fn env_f32(key: &str, default: f32) -> f32 {
    env_var(key)
        .and_then(|value| value.parse::<f32>().ok())
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use std::{env, sync::Mutex};

    use super::*;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn config_loads_defaults() {
        let _guard = ENV_LOCK.lock().unwrap();
        for key in [
            "ANCILLA_APP_ENV",
            "ANCILLA_DATA_FILE",
            "DATABASE_URL",
            "AWS_REGION",
            "AWS_DEFAULT_REGION",
            "AWS_PROFILE",
            "BEDROCK_CHAT_MODEL_ID",
            "BEDROCK_CHAT_MAX_TOKENS",
            "BEDROCK_CHAT_TEMPERATURE",
            "ANCILLA_ACCEPT_CLIENT_EMBEDDINGS",
            "ANCILLA_ACCEPT_CLIENT_TRANSCRIPTS",
            "ANCILLA_LOCAL_EMBED_MODEL",
            "ANCILLA_LOCAL_CONTEXT_EMBED_MODEL",
            "ANCILLA_LOCAL_EMBED_DEVICE",
        ] {
            unsafe { env::remove_var(key) };
        }

        let config = AppConfig::from_env();
        assert_eq!(config.app_env, "development");
        assert_eq!(config.data_file, PathBuf::from(".ancilla/state.json"));
        assert_eq!(config.aws_region, "us-west-2");
        assert!(config.accept_client_embeddings);
        assert!(config.accept_client_transcripts);
        assert_eq!(config.local_embed_device, "auto");
    }

    #[test]
    fn config_loads_overrides() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe {
            env::set_var("ANCILLA_DATA_FILE", "/tmp/custom.json");
            env::set_var("AWS_REGION", "eu-west-1");
            env::set_var("AWS_PROFILE", "ancilla-dev");
            env::set_var("BEDROCK_CHAT_MAX_TOKENS", "512");
            env::set_var("BEDROCK_CHAT_TEMPERATURE", "0.1");
            env::set_var("ANCILLA_ACCEPT_CLIENT_EMBEDDINGS", "false");
            env::set_var("ANCILLA_LOCAL_EMBED_DEVICE", "mps");
        }

        let config = AppConfig::from_env();
        assert_eq!(config.data_file, PathBuf::from("/tmp/custom.json"));
        assert_eq!(config.aws_region, "eu-west-1");
        assert_eq!(config.aws_profile.as_deref(), Some("ancilla-dev"));
        assert_eq!(config.bedrock_chat_max_tokens, 512);
        assert_eq!(config.bedrock_chat_temperature, 0.1);
        assert!(!config.accept_client_embeddings);
        assert_eq!(config.local_embed_device, "mps");

        for key in [
            "ANCILLA_DATA_FILE",
            "AWS_REGION",
            "AWS_PROFILE",
            "BEDROCK_CHAT_MAX_TOKENS",
            "BEDROCK_CHAT_TEMPERATURE",
            "ANCILLA_ACCEPT_CLIENT_EMBEDDINGS",
            "ANCILLA_LOCAL_EMBED_DEVICE",
        ] {
            unsafe { env::remove_var(key) };
        }
    }
}
