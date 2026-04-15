use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::config::{
    InitConfigResult, discover_user_config_file, env_bool, env_f32, env_i32, env_var,
    init_user_config, load_toml_file, merge_optional_path, merge_optional_string,
    redact_database_url,
};
use crate::model::{ChatModelOption, ChatModelsResponse, ChatThinkingMode};

const SERVER_CONFIG_APP_NAME: &str = "ancilla-server";
const SERVER_CONFIG_FILE_NAME: &str = "config.toml";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ServerConfig {
    pub app_env: String,
    pub data_file: PathBuf,
    pub database_url: Option<String>,
    pub embedder_base_url: Option<String>,
    pub embedder_timeout_seconds: i32,
    pub aws_region: String,
    pub aws_profile: Option<String>,
    pub aws_config_file: Option<PathBuf>,
    pub aws_shared_credentials_file: Option<PathBuf>,
    pub bedrock_chat_model_id: Option<String>,
    pub chat_models: Vec<ChatModelOption>,
    pub bedrock_chat_max_tokens: i32,
    pub bedrock_chat_temperature: f32,
    pub accept_client_embeddings: bool,
    pub accept_client_transcripts: bool,
    pub local_embed_model: String,
    pub local_context_embed_model: String,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct EffectiveServerConfigView {
    pub user_config_file: Option<PathBuf>,
    pub user_config_file_exists: bool,
    pub app_env: String,
    pub data_backend: String,
    pub data_file: PathBuf,
    pub database_url: Option<String>,
    pub embedder_base_url: Option<String>,
    pub embedder_timeout_seconds: i32,
    pub aws_region: String,
    pub aws_profile: Option<String>,
    pub aws_config_file: Option<PathBuf>,
    pub aws_config_file_exists: Option<bool>,
    pub aws_shared_credentials_file: Option<PathBuf>,
    pub aws_shared_credentials_file_exists: Option<bool>,
    pub chat_backend: String,
    pub bedrock_chat_model_id: Option<String>,
    pub chat_models: Vec<ChatModelOption>,
    pub bedrock_chat_max_tokens: i32,
    pub bedrock_chat_temperature: f32,
    pub accept_client_embeddings: bool,
    pub accept_client_transcripts: bool,
    pub local_embed_model: String,
    pub local_context_embed_model: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
struct FileConfig {
    pub app_env: Option<String>,
    pub data_file: Option<PathBuf>,
    pub database_url: Option<String>,
    pub embedder_base_url: Option<String>,
    pub embedder_timeout_seconds: Option<i32>,
    pub aws_region: Option<String>,
    pub aws_profile: Option<String>,
    pub aws_config_file: Option<PathBuf>,
    pub aws_shared_credentials_file: Option<PathBuf>,
    pub bedrock_chat_model_id: Option<String>,
    pub chat_models: Option<Vec<ChatModelOption>>,
    pub bedrock_chat_max_tokens: Option<i32>,
    pub bedrock_chat_temperature: Option<f32>,
    pub accept_client_embeddings: Option<bool>,
    pub accept_client_transcripts: Option<bool>,
    pub local_embed_model: Option<String>,
    pub local_context_embed_model: Option<String>,
}

impl ServerConfig {
    pub fn load() -> anyhow::Result<Self> {
        let mut config = Self::defaults();
        if let Some(file_config) =
            load_toml_file::<FileConfig>(SERVER_CONFIG_APP_NAME, SERVER_CONFIG_FILE_NAME)?
        {
            config.apply_file_config(file_config);
        }
        config.apply_env_overrides()?;
        Ok(config)
    }

    pub fn from_env() -> anyhow::Result<Self> {
        let mut config = Self::defaults();
        config.apply_env_overrides()?;
        Ok(config)
    }

    pub fn init_user_config(force: bool) -> anyhow::Result<InitConfigResult> {
        init_user_config(
            SERVER_CONFIG_APP_NAME,
            SERVER_CONFIG_FILE_NAME,
            default_user_config_contents(),
            force,
        )
    }

    pub fn to_view(&self, show_secrets: bool) -> EffectiveServerConfigView {
        let user_config_file =
            discover_user_config_file(SERVER_CONFIG_APP_NAME, SERVER_CONFIG_FILE_NAME);
        let chat_models = self.chat_models_response();
        EffectiveServerConfigView {
            user_config_file_exists: user_config_file.as_ref().is_some_and(|path| path.exists()),
            user_config_file,
            app_env: self.app_env.clone(),
            data_backend: if self.database_url.is_some() {
                "postgres".to_string()
            } else {
                "json".to_string()
            },
            data_file: self.data_file.clone(),
            database_url: self.database_url.as_deref().map(|url| {
                if show_secrets {
                    url.to_string()
                } else {
                    redact_database_url(url)
                }
            }),
            embedder_base_url: self.embedder_base_url.clone(),
            embedder_timeout_seconds: self.embedder_timeout_seconds,
            aws_region: self.aws_region.clone(),
            aws_profile: self.aws_profile.clone(),
            aws_config_file: self.aws_config_file.clone(),
            aws_config_file_exists: self.aws_config_file.as_ref().map(|path| path.exists()),
            aws_shared_credentials_file: self.aws_shared_credentials_file.clone(),
            aws_shared_credentials_file_exists: self
                .aws_shared_credentials_file
                .as_ref()
                .map(|path| path.exists()),
            chat_backend: chat_models.backend,
            bedrock_chat_model_id: chat_models.default_model_id,
            chat_models: chat_models.models,
            bedrock_chat_max_tokens: self.bedrock_chat_max_tokens,
            bedrock_chat_temperature: self.bedrock_chat_temperature,
            accept_client_embeddings: self.accept_client_embeddings,
            accept_client_transcripts: self.accept_client_transcripts,
            local_embed_model: self.local_embed_model.clone(),
            local_context_embed_model: self.local_context_embed_model.clone(),
        }
    }

    fn defaults() -> Self {
        Self {
            app_env: "development".to_string(),
            data_file: PathBuf::from(".ancilla/state.json"),
            database_url: None,
            embedder_base_url: None,
            embedder_timeout_seconds: 30,
            aws_region: "us-west-2".to_string(),
            aws_profile: None,
            aws_config_file: None,
            aws_shared_credentials_file: None,
            bedrock_chat_model_id: None,
            chat_models: Vec::new(),
            bedrock_chat_max_tokens: 800,
            bedrock_chat_temperature: 0.2,
            accept_client_embeddings: true,
            accept_client_transcripts: true,
            local_embed_model: "perplexity-ai/pplx-embed-v1-0.6b".to_string(),
            local_context_embed_model: "perplexity-ai/pplx-embed-context-v1-0.6b".to_string(),
        }
    }

    fn apply_file_config(&mut self, file_config: FileConfig) {
        if let Some(value) = crate::config::normalize_string(file_config.app_env) {
            self.app_env = value;
        }
        if let Some(value) = file_config
            .data_file
            .filter(|value| !value.as_os_str().is_empty())
        {
            self.data_file = value;
        }
        self.database_url =
            merge_optional_string(self.database_url.take(), file_config.database_url);
        self.embedder_base_url =
            merge_optional_string(self.embedder_base_url.take(), file_config.embedder_base_url);
        if let Some(value) = file_config.embedder_timeout_seconds {
            self.embedder_timeout_seconds = value.max(1);
        }
        if let Some(value) = crate::config::normalize_string(file_config.aws_region) {
            self.aws_region = value;
        }
        self.aws_profile = merge_optional_string(self.aws_profile.take(), file_config.aws_profile);
        self.aws_config_file =
            merge_optional_path(self.aws_config_file.take(), file_config.aws_config_file);
        self.aws_shared_credentials_file = merge_optional_path(
            self.aws_shared_credentials_file.take(),
            file_config.aws_shared_credentials_file,
        );
        self.bedrock_chat_model_id = merge_optional_string(
            self.bedrock_chat_model_id.take(),
            file_config.bedrock_chat_model_id,
        )
        .map(|model_id| canonicalize_chat_model_id(&model_id));
        if let Some(chat_models) = file_config.chat_models {
            self.chat_models = normalize_chat_models(chat_models);
        }
        if let Some(value) = file_config.bedrock_chat_max_tokens {
            self.bedrock_chat_max_tokens = value;
        }
        if let Some(value) = file_config.bedrock_chat_temperature {
            self.bedrock_chat_temperature = value;
        }
        if let Some(value) = file_config.accept_client_embeddings {
            self.accept_client_embeddings = value;
        }
        if let Some(value) = file_config.accept_client_transcripts {
            self.accept_client_transcripts = value;
        }
        if let Some(value) = crate::config::normalize_string(file_config.local_embed_model) {
            self.local_embed_model = value;
        }
        if let Some(value) = crate::config::normalize_string(file_config.local_context_embed_model)
        {
            self.local_context_embed_model = value;
        }
    }

    fn apply_env_overrides(&mut self) -> anyhow::Result<()> {
        if let Some(value) =
            env_var("ANCILLA_SERVER_APP_ENV").or_else(|| env_var("ANCILLA_APP_ENV"))
        {
            self.app_env = value;
        }
        if let Some(value) =
            env_var("ANCILLA_SERVER_DATA_FILE").or_else(|| env_var("ANCILLA_DATA_FILE"))
        {
            self.data_file = PathBuf::from(value);
        }
        if let Some(value) =
            env_var("ANCILLA_SERVER_DATABASE_URL").or_else(|| env_var("DATABASE_URL"))
        {
            self.database_url = Some(value);
        }
        if let Some(value) = env_var("ANCILLA_SERVER_EMBEDDER_BASE_URL")
            .or_else(|| env_var("ANCILLA_EMBEDDER_BASE_URL"))
        {
            self.embedder_base_url = Some(value);
        }
        self.embedder_timeout_seconds = env_i32(
            "ANCILLA_SERVER_EMBEDDER_TIMEOUT_SECONDS",
            env_i32(
                "ANCILLA_EMBEDDER_TIMEOUT_SECONDS",
                self.embedder_timeout_seconds,
            ),
        )
        .max(1);
        if let Some(value) = env_var("ANCILLA_SERVER_AWS_REGION")
            .or_else(|| env_var("AWS_REGION"))
            .or_else(|| env_var("AWS_DEFAULT_REGION"))
        {
            self.aws_region = value;
        }
        if let Some(value) =
            env_var("ANCILLA_SERVER_AWS_PROFILE").or_else(|| env_var("AWS_PROFILE"))
        {
            self.aws_profile = Some(value);
        }
        if let Some(value) =
            env_var("ANCILLA_SERVER_AWS_CONFIG_FILE").or_else(|| env_var("AWS_CONFIG_FILE"))
        {
            self.aws_config_file = Some(PathBuf::from(value));
        }
        if let Some(value) = env_var("ANCILLA_SERVER_AWS_SHARED_CREDENTIALS_FILE")
            .or_else(|| env_var("AWS_SHARED_CREDENTIALS_FILE"))
        {
            self.aws_shared_credentials_file = Some(PathBuf::from(value));
        }
        if let Some(value) = env_var("ANCILLA_SERVER_BEDROCK_CHAT_MODEL_ID")
            .or_else(|| env_var("BEDROCK_CHAT_MODEL_ID"))
        {
            self.bedrock_chat_model_id = Some(canonicalize_chat_model_id(&value));
        }
        if let Some(value) = env_var("ANCILLA_SERVER_BEDROCK_CHAT_MODELS_JSON")
            .or_else(|| env_var("BEDROCK_CHAT_MODELS_JSON"))
        {
            self.chat_models = normalize_chat_models(
                serde_json::from_str::<Vec<ChatModelOption>>(&value).map_err(|error| {
                    anyhow::anyhow!("failed to parse chat models JSON: {error}")
                })?,
            );
        }
        self.bedrock_chat_max_tokens = env_i32(
            "ANCILLA_SERVER_BEDROCK_CHAT_MAX_TOKENS",
            env_i32("BEDROCK_CHAT_MAX_TOKENS", self.bedrock_chat_max_tokens),
        );
        self.bedrock_chat_temperature = env_f32(
            "ANCILLA_SERVER_BEDROCK_CHAT_TEMPERATURE",
            env_f32("BEDROCK_CHAT_TEMPERATURE", self.bedrock_chat_temperature),
        );
        self.accept_client_embeddings = env_bool(
            "ANCILLA_SERVER_ACCEPT_CLIENT_EMBEDDINGS",
            env_bool(
                "ANCILLA_ACCEPT_CLIENT_EMBEDDINGS",
                self.accept_client_embeddings,
            ),
        );
        self.accept_client_transcripts = env_bool(
            "ANCILLA_SERVER_ACCEPT_CLIENT_TRANSCRIPTS",
            env_bool(
                "ANCILLA_ACCEPT_CLIENT_TRANSCRIPTS",
                self.accept_client_transcripts,
            ),
        );
        if let Some(value) = env_var("ANCILLA_SERVER_LOCAL_EMBED_MODEL")
            .or_else(|| env_var("ANCILLA_LOCAL_EMBED_MODEL"))
        {
            self.local_embed_model = value;
        }
        if let Some(value) = env_var("ANCILLA_SERVER_LOCAL_CONTEXT_EMBED_MODEL")
            .or_else(|| env_var("ANCILLA_LOCAL_CONTEXT_EMBED_MODEL"))
        {
            self.local_context_embed_model = value;
        }
        Ok(())
    }

    pub fn chat_models_response(&self) -> ChatModelsResponse {
        let mut models = if self.chat_models.is_empty() {
            self.bedrock_chat_model_id
                .as_deref()
                .map(synthesized_chat_model)
                .into_iter()
                .collect()
        } else {
            self.chat_models.clone()
        };

        let default_model_id = self
            .bedrock_chat_model_id
            .clone()
            .or_else(|| models.first().map(|model| model.model_id.clone()));

        if let Some(default_model_id) = default_model_id.as_deref()
            && models
                .iter()
                .all(|model| model.model_id != default_model_id)
        {
            models.insert(0, synthesized_chat_model(default_model_id));
        }

        ChatModelsResponse {
            backend: if default_model_id.is_some() {
                "bedrock".to_string()
            } else {
                "synthetic".to_string()
            },
            default_model_id,
            models,
        }
    }
}

fn normalize_chat_models(chat_models: Vec<ChatModelOption>) -> Vec<ChatModelOption> {
    chat_models
        .into_iter()
        .filter_map(|model| {
            let model_id = crate::config::normalize_string(Some(model.model_id))?;
            Some(ChatModelOption {
                label: crate::config::normalize_string(Some(model.label))?,
                model_id: canonicalize_chat_model_id(&model_id),
                description: crate::config::normalize_string(model.description),
                thinking_mode: model.thinking_mode,
                thinking_effort: model.thinking_effort,
                thinking_budget_tokens: model.thinking_budget_tokens,
            })
        })
        .collect()
}

fn synthesized_chat_model(model_id: &str) -> ChatModelOption {
    let model_id = canonicalize_chat_model_id(model_id);
    match model_id.as_str() {
        "us.anthropic.claude-opus-4-6-v1" | "global.anthropic.claude-opus-4-6-v1" => {
            ChatModelOption {
                label: "Claude Opus 4.6".to_string(),
                model_id,
                description: Some("Deepest reasoning".to_string()),
                thinking_mode: Some(ChatThinkingMode::Adaptive),
                thinking_effort: None,
                thinking_budget_tokens: None,
            }
        }
        "us.anthropic.claude-sonnet-4-6" | "global.anthropic.claude-sonnet-4-6" => {
            ChatModelOption {
                label: "Claude Sonnet 4.6".to_string(),
                model_id,
                description: Some("Balanced reasoning and speed".to_string()),
                thinking_mode: Some(ChatThinkingMode::Adaptive),
                thinking_effort: None,
                thinking_budget_tokens: None,
            }
        }
        "us.anthropic.claude-haiku-4-5-20251001-v1:0"
        | "global.anthropic.claude-haiku-4-5-20251001-v1:0" => ChatModelOption {
            label: "Claude Haiku 4.5".to_string(),
            model_id,
            description: Some("Fastest responses".to_string()),
            thinking_mode: None,
            thinking_effort: None,
            thinking_budget_tokens: None,
        },
        _ => ChatModelOption {
            label: model_id.to_string(),
            model_id,
            description: None,
            thinking_mode: None,
            thinking_effort: None,
            thinking_budget_tokens: None,
        },
    }
}

fn canonicalize_chat_model_id(model_id: &str) -> String {
    match model_id {
        "anthropic.claude-opus-4-6-v1" => "us.anthropic.claude-opus-4-6-v1".to_string(),
        "anthropic.claude-sonnet-4-6" => "us.anthropic.claude-sonnet-4-6".to_string(),
        "anthropic.claude-haiku-4-5-20251001-v1:0" => {
            "us.anthropic.claude-haiku-4-5-20251001-v1:0".to_string()
        }
        _ => model_id.to_string(),
    }
}

fn default_user_config_contents() -> &'static str {
    r#"# Ancilla server config
# Server-specific env vars override values in this file.
# Standard envs like DATABASE_URL, AWS_REGION, and BEDROCK_CHAT_MODEL_ID also work.
# Set BEDROCK_CHAT_MODELS_JSON to configure the model picker over env.

app_env = "development"
data_file = ".ancilla/state.json"
# database_url = "postgres://user:password@host:5432/ancilla?sslmode=require"
# embedder_base_url = "http://10.42.0.50:4000"
# embedder_timeout_seconds = 30
aws_region = "us-west-2"
# aws_profile = "ancilla-dev"
# aws_config_file = "~/path/to/ancilla/.aws/config"
# aws_shared_credentials_file = "~/path/to/ancilla/.aws/credentials"
# bedrock_chat_model_id = "us.anthropic.claude-opus-4-6-v1"
#
# [[chat_models]]
# label = "Claude Opus 4.6"
# model_id = "us.anthropic.claude-opus-4-6-v1"
# description = "Deepest reasoning"
# thinking_mode = "adaptive"
#
# [[chat_models]]
# label = "Claude Sonnet 4.6"
# model_id = "us.anthropic.claude-sonnet-4-6"
# description = "Balanced reasoning and speed"
# thinking_mode = "adaptive"
#
# [[chat_models]]
# label = "Claude Haiku 4.5"
# model_id = "us.anthropic.claude-haiku-4-5-20251001-v1:0"
# description = "Fastest responses"
bedrock_chat_max_tokens = 800
bedrock_chat_temperature = 0.2
accept_client_embeddings = true
accept_client_transcripts = true
local_embed_model = "perplexity-ai/pplx-embed-v1-0.6b"
local_context_embed_model = "perplexity-ai/pplx-embed-context-v1-0.6b"
"#
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::sync::Mutex;

    use tempfile::tempdir;

    use super::*;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn server_config_loads_defaults() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_env();

        let config = ServerConfig::from_env().unwrap();
        assert_eq!(config.app_env, "development");
        assert_eq!(config.data_file, PathBuf::from(".ancilla/state.json"));
        assert_eq!(config.embedder_timeout_seconds, 30);
        assert_eq!(config.aws_region, "us-west-2");
        assert!(config.accept_client_embeddings);
        assert!(config.accept_client_transcripts);
    }

    #[test]
    fn server_config_loads_env_overrides() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_env();
        unsafe {
            env::set_var("ANCILLA_SERVER_DATA_FILE", "/tmp/custom.json");
            env::set_var("ANCILLA_SERVER_EMBEDDER_BASE_URL", "http://10.42.0.50:4000");
            env::set_var("ANCILLA_SERVER_EMBEDDER_TIMEOUT_SECONDS", "45");
            env::set_var("ANCILLA_SERVER_AWS_REGION", "eu-west-1");
            env::set_var("ANCILLA_SERVER_AWS_PROFILE", "ancilla-dev");
            env::set_var("ANCILLA_SERVER_AWS_CONFIG_FILE", "/tmp/project/.aws/config");
            env::set_var(
                "ANCILLA_SERVER_AWS_SHARED_CREDENTIALS_FILE",
                "/tmp/project/.aws/credentials",
            );
            env::set_var("ANCILLA_SERVER_BEDROCK_CHAT_MAX_TOKENS", "512");
            env::set_var("ANCILLA_SERVER_BEDROCK_CHAT_TEMPERATURE", "0.1");
            env::set_var("ANCILLA_SERVER_ACCEPT_CLIENT_EMBEDDINGS", "false");
        }

        let config = ServerConfig::from_env().unwrap();
        assert_eq!(config.data_file, PathBuf::from("/tmp/custom.json"));
        assert_eq!(
            config.embedder_base_url.as_deref(),
            Some("http://10.42.0.50:4000")
        );
        assert_eq!(config.embedder_timeout_seconds, 45);
        assert_eq!(config.aws_region, "eu-west-1");
        assert_eq!(config.aws_profile.as_deref(), Some("ancilla-dev"));
        assert_eq!(
            config.aws_config_file,
            Some(PathBuf::from("/tmp/project/.aws/config"))
        );
        assert_eq!(
            config.aws_shared_credentials_file,
            Some(PathBuf::from("/tmp/project/.aws/credentials"))
        );
        assert_eq!(config.bedrock_chat_max_tokens, 512);
        assert_eq!(config.bedrock_chat_temperature, 0.1);
        assert!(!config.accept_client_embeddings);
    }

    #[test]
    fn server_config_loads_file_values() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_env();
        let temp_dir = tempdir().unwrap();
        let config_dir = temp_dir.path().join(SERVER_CONFIG_APP_NAME);
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join(SERVER_CONFIG_FILE_NAME),
            r#"
database_url = "postgres://file"
data_file = "/tmp/from-file.json"
embedder_base_url = "http://10.42.0.77:4000"
embedder_timeout_seconds = 55
aws_profile = "ancilla-dev"
aws_config_file = "~/workspace/ancilla/.aws/config"
aws_shared_credentials_file = "~/workspace/ancilla/.aws/credentials"
bedrock_chat_temperature = 0.6
"#,
        )
        .unwrap();
        unsafe {
            env::set_var("XDG_CONFIG_HOME", temp_dir.path());
        }

        let config = ServerConfig::load().unwrap();
        assert_eq!(config.database_url.as_deref(), Some("postgres://file"));
        assert_eq!(config.data_file, PathBuf::from("/tmp/from-file.json"));
        assert_eq!(
            config.embedder_base_url.as_deref(),
            Some("http://10.42.0.77:4000")
        );
        assert_eq!(config.embedder_timeout_seconds, 55);
        assert_eq!(config.aws_profile.as_deref(), Some("ancilla-dev"));
        assert_eq!(config.bedrock_chat_temperature, 0.6);
    }

    #[test]
    fn server_config_init_user_config_creates_scaffold() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_env();
        let temp_dir = tempdir().unwrap();
        unsafe {
            env::set_var("XDG_CONFIG_HOME", temp_dir.path());
        }

        let result = ServerConfig::init_user_config(false).unwrap();
        assert!(result.created_dir);
        assert!(result.created_file);
        let contents = std::fs::read_to_string(result.config_file).unwrap();
        assert!(contents.contains("database_url"));
        assert!(!contents.contains("service_base_url"));
    }

    #[test]
    fn server_config_view_redacts_database_url_by_default() {
        let config = ServerConfig {
            app_env: "development".to_string(),
            data_file: PathBuf::from(".ancilla/state.json"),
            database_url: Some(
                "postgres://ancilla:supersecret@example.com:5432/ancilla?sslmode=require"
                    .to_string(),
            ),
            embedder_base_url: Some("http://10.42.0.50:4000".to_string()),
            embedder_timeout_seconds: 30,
            aws_region: "us-west-2".to_string(),
            aws_profile: Some("ancilla-dev".to_string()),
            aws_config_file: Some(PathBuf::from("/tmp/project/.aws/config")),
            aws_shared_credentials_file: Some(PathBuf::from("/tmp/project/.aws/credentials")),
            bedrock_chat_model_id: Some("us.anthropic.claude-opus-4-6-v1".to_string()),
            chat_models: vec![ChatModelOption {
                label: "Claude Opus 4.6".to_string(),
                model_id: "us.anthropic.claude-opus-4-6-v1".to_string(),
                description: Some("Deepest reasoning".to_string()),
                thinking_mode: Some(ChatThinkingMode::Adaptive),
                thinking_effort: None,
                thinking_budget_tokens: None,
            }],
            bedrock_chat_max_tokens: 800,
            bedrock_chat_temperature: 0.2,
            accept_client_embeddings: true,
            accept_client_transcripts: true,
            local_embed_model: "embed".to_string(),
            local_context_embed_model: "context".to_string(),
        };

        let view = config.to_view(false);
        assert_eq!(view.data_backend, "postgres");
        assert_eq!(view.chat_backend, "bedrock");
        assert_eq!(view.chat_models.len(), 1);
        assert_eq!(
            view.embedder_base_url.as_deref(),
            Some("http://10.42.0.50:4000")
        );
        assert_eq!(
            view.database_url.as_deref(),
            Some("postgres://ancilla:***@example.com:5432/ancilla?sslmode=require")
        );
    }

    #[test]
    fn server_config_parses_chat_models_json_env() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_env();
        unsafe {
            env::set_var(
                "ANCILLA_SERVER_BEDROCK_CHAT_MODELS_JSON",
                r#"[{"label":"Claude Opus 4.6","model_id":"us.anthropic.claude-opus-4-6-v1","thinking_mode":"adaptive"},{"label":"Claude Haiku 4.5","model_id":"us.anthropic.claude-haiku-4-5-20251001-v1:0"}]"#,
            );
        }

        let config = ServerConfig::from_env().unwrap();
        assert_eq!(config.chat_models.len(), 2);
        assert_eq!(config.chat_models[0].label, "Claude Opus 4.6");
        assert_eq!(
            config.chat_models[0].thinking_mode,
            Some(ChatThinkingMode::Adaptive)
        );
    }

    fn clear_env() {
        for key in [
            "ANCILLA_SERVER_APP_ENV",
            "ANCILLA_SERVER_DATA_FILE",
            "ANCILLA_SERVER_DATABASE_URL",
            "ANCILLA_SERVER_EMBEDDER_BASE_URL",
            "ANCILLA_SERVER_EMBEDDER_TIMEOUT_SECONDS",
            "ANCILLA_SERVER_AWS_REGION",
            "ANCILLA_SERVER_AWS_PROFILE",
            "ANCILLA_SERVER_AWS_CONFIG_FILE",
            "ANCILLA_SERVER_AWS_SHARED_CREDENTIALS_FILE",
            "ANCILLA_SERVER_BEDROCK_CHAT_MODEL_ID",
            "ANCILLA_SERVER_BEDROCK_CHAT_MODELS_JSON",
            "ANCILLA_SERVER_BEDROCK_CHAT_MAX_TOKENS",
            "ANCILLA_SERVER_BEDROCK_CHAT_TEMPERATURE",
            "ANCILLA_SERVER_ACCEPT_CLIENT_EMBEDDINGS",
            "ANCILLA_SERVER_ACCEPT_CLIENT_TRANSCRIPTS",
            "ANCILLA_SERVER_LOCAL_EMBED_MODEL",
            "ANCILLA_SERVER_LOCAL_CONTEXT_EMBED_MODEL",
            "ANCILLA_APP_ENV",
            "ANCILLA_DATA_FILE",
            "DATABASE_URL",
            "ANCILLA_EMBEDDER_BASE_URL",
            "ANCILLA_EMBEDDER_TIMEOUT_SECONDS",
            "AWS_REGION",
            "AWS_DEFAULT_REGION",
            "AWS_PROFILE",
            "AWS_CONFIG_FILE",
            "AWS_SHARED_CREDENTIALS_FILE",
            "BEDROCK_CHAT_MODEL_ID",
            "BEDROCK_CHAT_MODELS_JSON",
            "BEDROCK_CHAT_MAX_TOKENS",
            "BEDROCK_CHAT_TEMPERATURE",
            "ANCILLA_ACCEPT_CLIENT_EMBEDDINGS",
            "ANCILLA_ACCEPT_CLIENT_TRANSCRIPTS",
            "ANCILLA_LOCAL_EMBED_MODEL",
            "ANCILLA_LOCAL_CONTEXT_EMBED_MODEL",
            "XDG_CONFIG_HOME",
        ] {
            unsafe { env::remove_var(key) };
        }
    }
}
