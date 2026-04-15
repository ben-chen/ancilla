use std::{
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, bail};
use serde::{Serialize, de::DeserializeOwned};

#[cfg(test)]
use std::sync::Mutex;

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct InitConfigResult {
    pub config_dir: PathBuf,
    pub config_file: PathBuf,
    pub created_dir: bool,
    pub created_file: bool,
    pub overwritten_file: bool,
}

pub fn user_config_dir(app_name: &str) -> anyhow::Result<PathBuf> {
    if let Some(path) = discover_user_config_dir(app_name) {
        return Ok(path);
    }
    bail!("could not determine config directory; set XDG_CONFIG_HOME or HOME")
}

pub fn user_config_file(app_name: &str, file_name: &str) -> anyhow::Result<PathBuf> {
    Ok(user_config_dir(app_name)?.join(file_name))
}

pub fn init_user_config(
    app_name: &str,
    file_name: &str,
    contents: &str,
    force: bool,
) -> anyhow::Result<InitConfigResult> {
    let config_dir = user_config_dir(app_name)?;
    let config_file = user_config_file(app_name, file_name)?;
    let created_dir = if config_dir.exists() {
        false
    } else {
        fs::create_dir_all(&config_dir).with_context(|| {
            format!("failed to create config directory {}", config_dir.display())
        })?;
        true
    };

    let file_exists = config_file.exists();
    let overwritten_file = file_exists && force;
    let created_file = if file_exists && !force {
        false
    } else {
        fs::write(&config_file, contents)
            .with_context(|| format!("failed to write config file {}", config_file.display()))?;
        true
    };

    Ok(InitConfigResult {
        config_dir,
        config_file,
        created_dir,
        created_file,
        overwritten_file,
    })
}

pub fn discover_user_config_dir(app_name: &str) -> Option<PathBuf> {
    if let Some(path) = env_var("XDG_CONFIG_HOME") {
        return Some(PathBuf::from(path).join(app_name));
    }
    env_var("HOME").map(|home| PathBuf::from(home).join(".config").join(app_name))
}

pub fn discover_user_config_file(app_name: &str, file_name: &str) -> Option<PathBuf> {
    discover_user_config_dir(app_name).map(|path| path.join(file_name))
}

pub fn load_toml_file<T>(app_name: &str, file_name: &str) -> anyhow::Result<Option<T>>
where
    T: DeserializeOwned,
{
    let Some(path) = discover_user_config_file(app_name, file_name) else {
        return Ok(None);
    };
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(load_toml_path(&path)?))
}

pub fn load_toml_path<T>(path: &Path) -> anyhow::Result<T>
where
    T: DeserializeOwned,
{
    let contents = fs::read_to_string(path)
        .with_context(|| format!("failed to read config file {}", path.display()))?;
    toml::from_str::<T>(&contents)
        .with_context(|| format!("failed to parse config file {}", path.display()))
}

pub(crate) fn env_var(key: &str) -> Option<String> {
    env::var(key).ok().filter(|value| !value.trim().is_empty())
}

pub(crate) fn env_bool(key: &str, default: bool) -> bool {
    env_var(key)
        .map(|value| {
            matches!(
                value.to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(default)
}

pub(crate) fn env_i32(key: &str, default: i32) -> i32 {
    env_var(key)
        .and_then(|value| value.parse::<i32>().ok())
        .unwrap_or(default)
}

pub(crate) fn env_f32(key: &str, default: f32) -> f32 {
    env_var(key)
        .and_then(|value| value.parse::<f32>().ok())
        .unwrap_or(default)
}

pub(crate) fn normalize_string(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

pub(crate) fn merge_optional_string(
    current: Option<String>,
    incoming: Option<String>,
) -> Option<String> {
    normalize_string(incoming).or(current)
}

pub(crate) fn merge_optional_path(
    current: Option<PathBuf>,
    incoming: Option<PathBuf>,
) -> Option<PathBuf> {
    incoming
        .filter(|value| !value.as_os_str().is_empty())
        .or(current)
}

pub(crate) fn redact_database_url(value: &str) -> String {
    let Some(scheme_idx) = value.find("://") else {
        return "***".to_string();
    };
    let scheme_end = scheme_idx + 3;
    let Some(at_idx) = value[scheme_end..].find('@') else {
        return "***".to_string();
    };
    let at_idx = scheme_end + at_idx;
    let credentials = &value[scheme_end..at_idx];
    let suffix = &value[at_idx..];
    if let Some(colon_idx) = credentials.find(':') {
        let username = &credentials[..colon_idx];
        format!("{}{}:***{}", &value[..scheme_end], username, suffix)
    } else {
        format!("{}***{}", &value[..scheme_end], suffix)
    }
}

pub(crate) fn redact_secret(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.len() <= 12 {
        return "***".to_string();
    }
    format!("{}...{}", &trimmed[..12], &trimmed[trimmed.len() - 4..])
}

#[cfg(test)]
static TEST_ENV_LOCK: Mutex<()> = Mutex::new(());

#[cfg(test)]
pub(crate) fn test_env_lock() -> &'static Mutex<()> {
    &TEST_ENV_LOCK
}
