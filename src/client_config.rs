use std::path::PathBuf;

use anyhow::bail;
use serde::{Deserialize, Serialize};

use crate::config::{
    InitConfigResult, discover_user_config_file, env_var, init_user_config, load_toml_file,
    merge_optional_string,
};

const CLIENT_CONFIG_APP_NAME: &str = "ancilla-client";
const CLIENT_CONFIG_FILE_NAME: &str = "config.toml";
const DEFAULT_BASE_URL: &str = "http://127.0.0.1:3000";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ClientConfig {
    pub base_url: String,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct EffectiveClientConfigView {
    pub user_config_file: Option<PathBuf>,
    pub user_config_file_exists: bool,
    pub base_url: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
struct FileConfig {
    pub base_url: Option<String>,
}

impl ClientConfig {
    pub fn load() -> anyhow::Result<Self> {
        let mut config = Self::defaults();
        if let Some(file_config) =
            load_toml_file::<FileConfig>(CLIENT_CONFIG_APP_NAME, CLIENT_CONFIG_FILE_NAME)?
        {
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
            CLIENT_CONFIG_APP_NAME,
            CLIENT_CONFIG_FILE_NAME,
            default_user_config_contents(),
            force,
        )
    }

    pub fn to_view(&self) -> EffectiveClientConfigView {
        let user_config_file =
            discover_user_config_file(CLIENT_CONFIG_APP_NAME, CLIENT_CONFIG_FILE_NAME);
        EffectiveClientConfigView {
            user_config_file_exists: user_config_file.as_ref().is_some_and(|path| path.exists()),
            user_config_file,
            base_url: self.base_url.clone(),
        }
    }

    fn defaults() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_string(),
        }
    }

    fn apply_file_config(&mut self, file_config: FileConfig) -> anyhow::Result<()> {
        if let Some(base_url) =
            merge_optional_string(Some(self.base_url.clone()), file_config.base_url)
        {
            self.base_url = normalize_base_url(&base_url)?;
        }
        Ok(())
    }

    fn apply_env_overrides(&mut self) -> anyhow::Result<()> {
        if let Some(value) = env_var("ANCILLA_CLIENT_BASE_URL") {
            self.base_url = normalize_base_url(&value)?;
        }
        Ok(())
    }
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
    fn client_config_loads_defaults() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_env();

        let config = ClientConfig::from_env().unwrap();
        assert_eq!(config.base_url, "http://127.0.0.1:3000");
    }

    #[test]
    fn client_config_loads_env_override() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_env();
        unsafe {
            env::set_var("ANCILLA_CLIENT_BASE_URL", "16.146.111.110:3000");
        }

        let config = ClientConfig::from_env().unwrap();
        assert_eq!(config.base_url, "http://16.146.111.110:3000");
    }

    #[test]
    fn client_config_loads_file_values() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_env();
        let temp_dir = tempdir().unwrap();
        let config_dir = temp_dir.path().join(CLIENT_CONFIG_APP_NAME);
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join(CLIENT_CONFIG_FILE_NAME),
            r#"
base_url = "https://example.com:3000/"
"#,
        )
        .unwrap();
        unsafe {
            env::set_var("XDG_CONFIG_HOME", temp_dir.path());
        }

        let config = ClientConfig::load().unwrap();
        assert_eq!(config.base_url, "https://example.com:3000");
    }

    #[test]
    fn client_config_init_user_config_creates_scaffold() {
        let _guard = ENV_LOCK.lock().unwrap();
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
        for key in ["ANCILLA_CLIENT_BASE_URL", "XDG_CONFIG_HOME"] {
            unsafe { env::remove_var(key) };
        }
    }
}
