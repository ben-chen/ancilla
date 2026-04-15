use std::{fs, path::PathBuf};

use anyhow::bail;
use serde::{Deserialize, Serialize};

use crate::config::{
    InitConfigResult, discover_user_config_file, env_var, init_user_config, load_toml_path,
    merge_optional_string, redact_secret, user_config_file,
};

const CLIENT_CONFIG_DIR_NAME: &str = "ancilla";
const CLIENT_CONFIG_FILE_NAME: &str = "client.toml";
const LEGACY_CLIENT_CONFIG_DIR_NAME: &str = "ancilla-client";
const LEGACY_CLIENT_CONFIG_FILE_NAME: &str = "config.toml";
const DEFAULT_BASE_URL: &str = "http://127.0.0.1:3000";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ClientConfig {
    pub base_url: String,
    pub basic_auth_username: Option<String>,
    pub basic_auth_password: Option<String>,
    pub selected_chat_model_id: Option<String>,
    pub selected_gate_model_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct EffectiveClientConfigView {
    pub user_config_file: Option<PathBuf>,
    pub user_config_file_exists: bool,
    pub base_url: String,
    pub basic_auth_enabled: bool,
    pub basic_auth_username: Option<String>,
    pub basic_auth_password: Option<String>,
    pub selected_chat_model_id: Option<String>,
    pub selected_gate_model_id: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
struct FileConfig {
    pub base_url: Option<String>,
    pub basic_auth_username: Option<String>,
    pub basic_auth_password: Option<String>,
    pub selected_chat_model_id: Option<String>,
    pub selected_gate_model_id: Option<String>,
}

impl ClientConfig {
    pub fn load() -> anyhow::Result<Self> {
        let mut config = Self::defaults();
        if let Some(file_config) = load_file_config()? {
            config.apply_file_config(file_config)?;
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
            CLIENT_CONFIG_DIR_NAME,
            CLIENT_CONFIG_FILE_NAME,
            default_user_config_contents(),
            force,
        )
    }

    pub fn save(&self) -> anyhow::Result<PathBuf> {
        let path = user_config_file(CLIENT_CONFIG_DIR_NAME, CLIENT_CONFIG_FILE_NAME)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let body = toml::to_string_pretty(&FileConfig {
            base_url: Some(self.base_url.clone()),
            basic_auth_username: self.basic_auth_username.clone(),
            basic_auth_password: self.basic_auth_password.clone(),
            selected_chat_model_id: self.selected_chat_model_id.clone(),
            selected_gate_model_id: self.selected_gate_model_id.clone(),
        })?;
        fs::write(&path, body)?;
        Ok(path)
    }

    pub fn to_view(&self) -> EffectiveClientConfigView {
        let user_config_file = discover_client_config_file()
            .or_else(|| user_config_file(CLIENT_CONFIG_DIR_NAME, CLIENT_CONFIG_FILE_NAME).ok());
        EffectiveClientConfigView {
            user_config_file_exists: user_config_file.as_ref().is_some_and(|path| path.exists()),
            user_config_file,
            base_url: self.base_url.clone(),
            basic_auth_enabled: self.basic_auth_username.is_some()
                && self.basic_auth_password.is_some(),
            basic_auth_username: self.basic_auth_username.clone(),
            basic_auth_password: self.basic_auth_password.as_deref().map(redact_secret),
            selected_chat_model_id: self.selected_chat_model_id.clone(),
            selected_gate_model_id: self.selected_gate_model_id.clone(),
        }
    }

    fn defaults() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_string(),
            basic_auth_username: None,
            basic_auth_password: None,
            selected_chat_model_id: None,
            selected_gate_model_id: None,
        }
    }

    fn apply_file_config(&mut self, file_config: FileConfig) -> anyhow::Result<()> {
        if let Some(base_url) =
            merge_optional_string(Some(self.base_url.clone()), file_config.base_url)
        {
            self.base_url = normalize_base_url(&base_url)?;
        }
        self.basic_auth_username = merge_optional_string(
            self.basic_auth_username.take(),
            file_config.basic_auth_username,
        );
        self.basic_auth_password = merge_optional_string(
            self.basic_auth_password.take(),
            file_config.basic_auth_password,
        );
        self.selected_chat_model_id = merge_optional_string(
            self.selected_chat_model_id.take(),
            file_config.selected_chat_model_id,
        );
        self.selected_gate_model_id = merge_optional_string(
            self.selected_gate_model_id.take(),
            file_config.selected_gate_model_id,
        );
        self.validate()?;
        Ok(())
    }

    fn apply_env_overrides(&mut self) -> anyhow::Result<()> {
        if let Some(value) = env_var("ANCILLA_CLIENT_BASE_URL") {
            self.base_url = normalize_base_url(&value)?;
        }
        if let Some(value) = env_var("ANCILLA_CLIENT_BASIC_AUTH_USERNAME") {
            self.basic_auth_username = Some(value);
        }
        if let Some(value) = env_var("ANCILLA_CLIENT_BASIC_AUTH_PASSWORD") {
            self.basic_auth_password = Some(value);
        }
        if let Some(value) = env_var("ANCILLA_CLIENT_CHAT_MODEL_ID") {
            self.selected_chat_model_id = Some(value);
        }
        if let Some(value) = env_var("ANCILLA_CLIENT_GATE_MODEL_ID") {
            self.selected_gate_model_id = Some(value);
        }
        self.validate()?;
        Ok(())
    }

    fn validate(&self) -> anyhow::Result<()> {
        match (
            self.basic_auth_username.as_deref(),
            self.basic_auth_password.as_deref(),
        ) {
            (Some(_), Some(_)) | (None, None) => Ok(()),
            (Some(_), None) => bail!(
                "client basic auth password is missing; set ANCILLA_CLIENT_BASIC_AUTH_PASSWORD or basic_auth_password"
            ),
            (None, Some(_)) => bail!(
                "client basic auth username is missing; set ANCILLA_CLIENT_BASIC_AUTH_USERNAME or basic_auth_username"
            ),
        }
    }
}

fn load_file_config() -> anyhow::Result<Option<FileConfig>> {
    let Some(path) = discover_client_config_file() else {
        return Ok(None);
    };
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(load_toml_path(&path)?))
}

fn discover_client_config_file() -> Option<PathBuf> {
    let preferred = discover_user_config_file(CLIENT_CONFIG_DIR_NAME, CLIENT_CONFIG_FILE_NAME);
    if preferred.as_ref().is_some_and(|path| path.exists()) {
        return preferred;
    }

    let legacy = discover_user_config_file(
        LEGACY_CLIENT_CONFIG_DIR_NAME,
        LEGACY_CLIENT_CONFIG_FILE_NAME,
    );
    if legacy.as_ref().is_some_and(|path| path.exists()) {
        return legacy;
    }

    preferred.or(legacy)
}

pub fn normalize_base_url(value: &str) -> anyhow::Result<String> {
    let trimmed = value.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        bail!("client base URL cannot be empty")
    }
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        return Ok(trimmed.to_string());
    }
    Ok(format!("http://{trimmed}"))
}

fn default_user_config_contents() -> &'static str {
    r#"# Ancilla client config
# ANCILLA_CLIENT_BASE_URL overrides this value.

base_url = "http://127.0.0.1:3000"
# basic_auth_username = "ancilla"
# basic_auth_password = "replace-me"
# selected_chat_model_id = "moonshotai.kimi-k2.5"
# selected_gate_model_id = "us.anthropic.claude-haiku-4-5-20251001-v1:0"
"#
}

#[cfg(test)]
mod tests {
    use std::env;

    use tempfile::tempdir;

    use super::*;
    use crate::config::test_env_lock;

    #[test]
    fn client_config_loads_defaults() {
        let _guard = test_env_lock().lock().unwrap();
        clear_env();

        let config = ClientConfig::from_env().unwrap();
        assert_eq!(config.base_url, "http://127.0.0.1:3000");
        assert!(config.basic_auth_username.is_none());
        assert!(config.basic_auth_password.is_none());
        assert!(config.selected_chat_model_id.is_none());
        assert!(config.selected_gate_model_id.is_none());
    }

    #[test]
    fn client_config_loads_env_override() {
        let _guard = test_env_lock().lock().unwrap();
        clear_env();
        unsafe {
            env::set_var("ANCILLA_CLIENT_BASE_URL", "16.146.111.110:3000");
            env::set_var("ANCILLA_CLIENT_BASIC_AUTH_USERNAME", "ancilla");
            env::set_var("ANCILLA_CLIENT_BASIC_AUTH_PASSWORD", "secret-value");
            env::set_var("ANCILLA_CLIENT_CHAT_MODEL_ID", "moonshotai.kimi-k2.5");
            env::set_var(
                "ANCILLA_CLIENT_GATE_MODEL_ID",
                "us.anthropic.claude-haiku-4-5-20251001-v1:0",
            );
        }

        let config = ClientConfig::from_env().unwrap();
        assert_eq!(config.base_url, "http://16.146.111.110:3000");
        assert_eq!(config.basic_auth_username.as_deref(), Some("ancilla"));
        assert_eq!(config.basic_auth_password.as_deref(), Some("secret-value"));
        assert_eq!(
            config.selected_chat_model_id.as_deref(),
            Some("moonshotai.kimi-k2.5")
        );
        assert_eq!(
            config.selected_gate_model_id.as_deref(),
            Some("us.anthropic.claude-haiku-4-5-20251001-v1:0")
        );
    }

    #[test]
    fn client_config_loads_file_values() {
        let _guard = test_env_lock().lock().unwrap();
        clear_env();
        let temp_dir = tempdir().unwrap();
        let config_dir = temp_dir.path().join(CLIENT_CONFIG_DIR_NAME);
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join(CLIENT_CONFIG_FILE_NAME),
            r#"
base_url = "https://example.com:3000/"
basic_auth_username = "ancilla"
basic_auth_password = "from-file-secret"
selected_chat_model_id = "moonshotai.kimi-k2.5"
selected_gate_model_id = "us.anthropic.claude-haiku-4-5-20251001-v1:0"
"#,
        )
        .unwrap();
        unsafe {
            env::set_var("XDG_CONFIG_HOME", temp_dir.path());
        }

        let config = ClientConfig::load().unwrap();
        assert_eq!(config.base_url, "https://example.com:3000");
        assert_eq!(config.basic_auth_username.as_deref(), Some("ancilla"));
        assert_eq!(
            config.basic_auth_password.as_deref(),
            Some("from-file-secret")
        );
        assert_eq!(
            config.selected_chat_model_id.as_deref(),
            Some("moonshotai.kimi-k2.5")
        );
        assert_eq!(
            config.selected_gate_model_id.as_deref(),
            Some("us.anthropic.claude-haiku-4-5-20251001-v1:0")
        );
    }

    #[test]
    fn client_config_falls_back_to_legacy_path() {
        let _guard = test_env_lock().lock().unwrap();
        clear_env();
        let temp_dir = tempdir().unwrap();
        let config_dir = temp_dir.path().join(LEGACY_CLIENT_CONFIG_DIR_NAME);
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join(LEGACY_CLIENT_CONFIG_FILE_NAME),
            r#"
base_url = "http://legacy.example:3000"
"#,
        )
        .unwrap();
        unsafe {
            env::set_var("XDG_CONFIG_HOME", temp_dir.path());
        }

        let config = ClientConfig::load().unwrap();
        assert_eq!(config.base_url, "http://legacy.example:3000");
    }

    #[test]
    fn client_config_init_user_config_creates_scaffold() {
        let _guard = test_env_lock().lock().unwrap();
        clear_env();
        let temp_dir = tempdir().unwrap();
        unsafe {
            env::set_var("XDG_CONFIG_HOME", temp_dir.path());
        }

        let result = ClientConfig::init_user_config(false).unwrap();
        assert!(result.created_dir);
        assert!(result.created_file);
        let contents = std::fs::read_to_string(result.config_file).unwrap();
        assert!(contents.contains("base_url"));
        assert!(!contents.contains("database_url"));
        assert!(contents.contains("selected_chat_model_id"));
    }

    #[test]
    fn client_config_rejects_half_configured_basic_auth() {
        let _guard = test_env_lock().lock().unwrap();
        clear_env();
        unsafe {
            env::set_var("ANCILLA_CLIENT_BASIC_AUTH_USERNAME", "ancilla");
        }

        assert!(ClientConfig::from_env().is_err());
    }

    #[test]
    fn normalize_base_url_adds_http_scheme() {
        assert_eq!(
            normalize_base_url("16.146.111.110:3000").unwrap(),
            "http://16.146.111.110:3000"
        );
    }

    #[test]
    fn normalize_base_url_trims_trailing_slash() {
        assert_eq!(
            normalize_base_url("https://example.com:3000/").unwrap(),
            "https://example.com:3000"
        );
    }

    fn clear_env() {
        for key in [
            "ANCILLA_CLIENT_BASE_URL",
            "ANCILLA_CLIENT_BASIC_AUTH_USERNAME",
            "ANCILLA_CLIENT_BASIC_AUTH_PASSWORD",
            "ANCILLA_CLIENT_CHAT_MODEL_ID",
            "ANCILLA_CLIENT_GATE_MODEL_ID",
            "XDG_CONFIG_HOME",
        ] {
            unsafe { env::remove_var(key) };
        }
    }
}
