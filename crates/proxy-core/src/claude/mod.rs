use crate::{
    config::DEFAULT_PORT,
    error::{ProxyError, Result},
};
use chrono::Utc;
use serde_json::{json, Map, Value};
use std::{
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};

pub const MANAGED_ENV_KEYS: &[&str] = &[
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_MODEL",
    "ANTHROPIC_SMALL_FAST_MODEL",
    "CLAUDE_CODE_AUTO_COMPACT_WINDOW",
    "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
    "CLAUDE_CODE_DISABLE_NONSTREAMING_FALLBACK",
];

#[derive(Debug, Clone)]
pub struct ClaudeSettingsOptions {
    pub port: u16,
    pub model: String,
    pub small_fast_model: String,
    pub auto_compact_window: u32,
}

impl Default for ClaudeSettingsOptions {
    fn default() -> Self {
        Self {
            port: DEFAULT_PORT,
            model: "gpt-5.4[1m]".into(),
            small_fast_model: "gpt-5.4-mini[1m]".into(),
            auto_compact_window: 272_000,
        }
    }
}

#[derive(Debug, Clone)]
pub struct InstallResult {
    pub settings_path: PathBuf,
    pub backup_path: Option<PathBuf>,
}

pub fn default_settings_path() -> Result<PathBuf> {
    let home = dirs::home_dir()
        .ok_or_else(|| ProxyError::Config("cannot locate home directory".into()))?;
    Ok(home.join(".claude").join("settings.json"))
}

pub fn install_settings(path: &Path, options: &ClaudeSettingsOptions) -> Result<InstallResult> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let backup_path = backup_existing(path)?;
    let mut settings = read_settings(path)?;
    merge_env(&mut settings, managed_env(options));
    write_pretty(path, &settings)?;
    Ok(InstallResult {
        settings_path: path.to_path_buf(),
        backup_path,
    })
}

pub fn restore_latest_backup(path: &Path) -> Result<Option<PathBuf>> {
    let Some(parent) = path.parent() else {
        return Ok(None);
    };
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| ProxyError::Config("settings path has no filename".into()))?;
    let prefix = format!("{file_name}.backup-");
    let mut backups = fs::read_dir(parent)?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|candidate| {
            candidate
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(&prefix))
        })
        .collect::<Vec<_>>();
    backups.sort();
    let Some(latest) = backups.pop() else {
        return Ok(None);
    };
    fs::copy(&latest, path)?;
    Ok(Some(latest))
}

pub fn managed_env(options: &ClaudeSettingsOptions) -> Map<String, Value> {
    let mut env = Map::new();
    env.insert(
        "ANTHROPIC_BASE_URL".into(),
        Value::String(format!("http://127.0.0.1:{}", options.port)),
    );
    env.insert(
        "ANTHROPIC_AUTH_TOKEN".into(),
        Value::String("unused".into()),
    );
    env.insert(
        "ANTHROPIC_MODEL".into(),
        Value::String(options.model.clone()),
    );
    env.insert(
        "ANTHROPIC_SMALL_FAST_MODEL".into(),
        Value::String(options.small_fast_model.clone()),
    );
    env.insert(
        "CLAUDE_CODE_AUTO_COMPACT_WINDOW".into(),
        Value::Number(options.auto_compact_window.into()),
    );
    env.insert(
        "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".into(),
        Value::Number(1.into()),
    );
    env.insert(
        "CLAUDE_CODE_DISABLE_NONSTREAMING_FALLBACK".into(),
        Value::Number(1.into()),
    );
    env
}

fn backup_existing(path: &Path) -> Result<Option<PathBuf>> {
    if !path.exists() {
        return Ok(None);
    }
    let timestamp = Utc::now().format("%Y%m%dT%H%M%SZ");
    let backup = path.with_file_name(format!(
        "{}.backup-{timestamp}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("settings.json")
    ));
    fs::copy(path, &backup)?;
    Ok(Some(backup))
}

fn read_settings(path: &Path) -> Result<Value> {
    match fs::read_to_string(path) {
        Ok(raw) if raw.trim().is_empty() => Ok(json!({})),
        Ok(raw) => Ok(serde_json::from_str(&raw)?),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(json!({})),
        Err(err) => Err(err.into()),
    }
}

fn merge_env(settings: &mut Value, managed: Map<String, Value>) {
    if !settings.is_object() {
        *settings = json!({});
    }
    let root = settings
        .as_object_mut()
        .expect("object after normalization");
    let env = root.entry("env").or_insert_with(|| json!({}));
    if !env.is_object() {
        *env = json!({});
    }
    let env = env.as_object_mut().expect("env object after normalization");
    for key in MANAGED_ENV_KEYS {
        env.remove(*key);
    }
    for (key, value) in managed {
        env.insert(key, value);
    }
}

fn write_pretty(path: &Path, value: &Value) -> Result<()> {
    fs::write(path, format!("{}\n", serde_json::to_string_pretty(value)?))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_preserves_unmanaged_env_and_creates_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(
            &path,
            r#"{"env":{"KEEP":"yes","ANTHROPIC_MODEL":"old"},"theme":"dark"}"#,
        )
        .unwrap();
        let result = install_settings(&path, &ClaudeSettingsOptions::default()).unwrap();
        assert!(result.backup_path.unwrap().exists());
        let value: Value = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(value["theme"], "dark");
        assert_eq!(value["env"]["KEEP"], "yes");
        assert_eq!(value["env"]["ANTHROPIC_MODEL"], "gpt-5.4[1m]");
    }

    #[test]
    fn restore_latest_backup_returns_none_without_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        assert!(restore_latest_backup(&path).unwrap().is_none());
    }
}
