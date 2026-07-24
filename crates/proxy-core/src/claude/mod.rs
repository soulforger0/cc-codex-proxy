use crate::{
    config::{
        Provider, DEFAULT_PORT, DEFAULT_PUBLIC_PRIMARY_MODEL, DEFAULT_PUBLIC_SMALL_MODEL,
        DEFAULT_PUBLIC_SONNET_MODEL,
    },
    error::{ProxyError, Result},
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::{
    env, fs,
    io::{ErrorKind, Read},
    path::{Path, PathBuf},
    process::Command,
};

pub const MANAGED_ENV_KEYS: &[&str] = &[
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_MODEL",
    "ANTHROPIC_DEFAULT_OPUS_MODEL",
    "ANTHROPIC_DEFAULT_SONNET_MODEL",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL",
    "ANTHROPIC_SMALL_FAST_MODEL",
    "CLAUDE_CODE_SUBAGENT_MODEL",
    "CLAUDE_CODE_AUTO_COMPACT_WINDOW",
    "CLAUDE_CODE_EFFORT_LEVEL",
    "CLAUDE_CODE_ALWAYS_ENABLE_EFFORT",
    "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
    "CLAUDE_CODE_DISABLE_NONSTREAMING_FALLBACK",
];

pub const SHIM_MARKER: &str = "CC_CODEX_PROXY_MANAGED_CLAUDE_SHIM";
const SHIM_STATE_VERSION: u32 = 1;

#[derive(Debug, Clone)]
pub struct ClaudeSettingsOptions {
    pub provider: Provider,
    pub port: u16,
    pub model: String,
    pub small_fast_model: String,
    pub auto_compact_window: u32,
}

impl Default for ClaudeSettingsOptions {
    fn default() -> Self {
        Self {
            provider: Provider::Codex,
            port: DEFAULT_PORT,
            model: DEFAULT_PUBLIC_PRIMARY_MODEL.into(),
            small_fast_model: DEFAULT_PUBLIC_SMALL_MODEL.into(),
            auto_compact_window: 372_000,
        }
    }
}

#[derive(Debug, Clone)]
pub struct InstallResult {
    pub settings_path: PathBuf,
    pub backup_path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct ClaudeShimInstallOptions {
    pub app_pid: u32,
    pub helper_path: PathBuf,
    pub claude_path: Option<PathBuf>,
    pub settings: ClaudeSettingsOptions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeShimState {
    pub version: u32,
    #[serde(default)]
    pub provider: Provider,
    pub shim_path: PathBuf,
    pub real_claude_path: PathBuf,
    pub helper_path: PathBuf,
    pub app_pid: u32,
    pub port: u16,
    pub model: String,
    pub small_fast_model: String,
    pub auto_compact_window: u32,
    pub original: ClaudeShimOriginal,
    pub installed_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ClaudeShimOriginal {
    Symlink { target: PathBuf },
    RegularFile { backup_path: PathBuf },
}

#[derive(Debug, Clone)]
pub struct ClaudeShimInstallResult {
    pub states: Vec<ClaudeShimState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeShimStateFile {
    version: u32,
    shims: Vec<ClaudeShimState>,
}

#[derive(Debug, Clone)]
pub struct ClaudeShimRestoreReport {
    pub restored: Vec<PathBuf>,
    pub skipped: Vec<ClaudeShimRestoreSkip>,
}

#[derive(Debug, Clone)]
pub struct ClaudeShimRestoreSkip {
    pub shim_path: PathBuf,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveClaudeSession {
    pub pid: u32,
    pub command: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeSettingsPreview {
    pub settings_path: String,
    pub settings_exists: bool,
    pub current_settings: String,
    pub proposed_settings: String,
    pub latest_backup_path: Option<String>,
    pub restore_settings: Option<String>,
    pub managed_changes: Vec<ClaudeEnvChange>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeEnvChange {
    pub key: String,
    pub action: ClaudeEnvChangeAction,
    pub current: Option<String>,
    pub proposed: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ClaudeEnvChangeAction {
    Add,
    Change,
    Keep,
}

pub fn default_settings_path() -> Result<PathBuf> {
    let home = dirs::home_dir()
        .ok_or_else(|| ProxyError::Config("cannot locate home directory".into()))?;
    Ok(home.join(".claude").join("settings.json"))
}

pub fn install_settings(path: &Path, options: &ClaudeSettingsOptions) -> Result<InstallResult> {
    let current = read_settings(path)?;
    let mut settings = current.clone();
    merge_env(&mut settings, managed_env(options));
    if settings == current {
        return Ok(InstallResult {
            settings_path: path.to_path_buf(),
            backup_path: None,
        });
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let backup_path = backup_existing(path)?;
    write_pretty(path, &settings)?;
    Ok(InstallResult {
        settings_path: path.to_path_buf(),
        backup_path,
    })
}

pub fn preview_settings(
    path: &Path,
    options: &ClaudeSettingsOptions,
) -> Result<ClaudeSettingsPreview> {
    let current = read_settings(path)?;
    let managed = managed_env(options);
    let managed_changes = managed_changes(&current, &managed);
    let mut proposed = current.clone();
    merge_env(&mut proposed, managed);
    let latest_backup_path = latest_backup_path(path)?;
    let restore_settings = latest_backup_path
        .as_deref()
        .map(read_existing_pretty_or_raw)
        .transpose()?;

    Ok(ClaudeSettingsPreview {
        settings_path: path.display().to_string(),
        settings_exists: path.exists(),
        current_settings: pretty_json(&current)?,
        proposed_settings: pretty_json(&proposed)?,
        latest_backup_path: latest_backup_path.map(|path| path.display().to_string()),
        restore_settings,
        managed_changes,
    })
}

pub fn restore_latest_backup(path: &Path) -> Result<Option<PathBuf>> {
    let Some(latest) = latest_backup_path(path)? else {
        return Ok(None);
    };
    fs::copy(&latest, path)?;
    Ok(Some(latest))
}

pub fn live_claude_sessions() -> Result<Vec<LiveClaudeSession>> {
    let output = Command::new("/bin/ps")
        .args(["-axo", "pid=,args="])
        .output()?;
    if !output.status.success() {
        return Err(ProxyError::Config(
            "failed to inspect running processes for Claude Code sessions".into(),
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_live_claude_sessions(&stdout, std::process::id()))
}

pub fn live_claude_sessions_message(sessions: &[LiveClaudeSession]) -> String {
    let mut message = String::from(
        "Claude Code is already running. Close all Claude Code sessions before starting the proxy.",
    );
    if !sessions.is_empty() {
        message.push_str("\n\nRunning Claude Code processes:");
        for session in sessions.iter().take(8) {
            message.push_str(&format!(
                "\n- pid {}: {}",
                session.pid,
                truncate_command(&session.command)
            ));
        }
        if sessions.len() > 8 {
            message.push_str(&format!("\n- and {} more", sessions.len() - 8));
        }
    }
    message
}

pub fn install_shim(
    state_path: &Path,
    options: &ClaudeShimInstallOptions,
) -> Result<ClaudeShimInstallResult> {
    let shim_paths = match &options.claude_path {
        Some(path) => vec![path.clone()],
        None => discover_claude_paths()?,
    };
    let existing_states = read_shim_states(state_path).unwrap_or_default();
    let mut states = Vec::new();
    let mut installed = Vec::new();

    for shim_path in shim_paths {
        match install_one_shim(shim_path, options, &existing_states) {
            Ok(state) => {
                installed.push(state.clone());
                states.push(state);
            }
            Err(err) => {
                for state in installed.iter().rev() {
                    let _ = restore_original(state);
                }
                return Err(err);
            }
        }
    }

    if let Some(parent) = state_path.parent() {
        fs::create_dir_all(parent)?;
    }
    if let Err(err) = write_shim_states(state_path, &states) {
        for state in installed.iter().rev() {
            let _ = restore_original(state);
        }
        return Err(err.into());
    }
    Ok(ClaudeShimInstallResult { states })
}

pub fn restore_shim(state_path: &Path) -> Result<ClaudeShimRestoreReport> {
    let states = read_shim_states(state_path)?;
    let mut restored = Vec::new();
    let mut skipped = Vec::new();

    for state in states {
        if !path_contains_marker(&state.shim_path)? {
            skipped.push(ClaudeShimRestoreSkip {
                shim_path: state.shim_path,
                reason: "current claude command is no longer the managed shim".into(),
            });
            continue;
        }

        match restore_original(&state) {
            Ok(()) => restored.push(state.shim_path),
            Err(err) => skipped.push(ClaudeShimRestoreSkip {
                shim_path: state.shim_path,
                reason: err.to_string(),
            }),
        }
    }

    if skipped.is_empty() {
        let _ = fs::remove_file(state_path);
    }

    Ok(ClaudeShimRestoreReport { restored, skipped })
}

/// Temporarily restore a managed launcher so Claude Code's native updater can
/// update the symlink it owns. Always follow this with [`finish_shim_update`].
pub fn begin_shim_update(state_path: &Path, shim_path: &Path) -> Result<()> {
    let states = read_shim_states(state_path)?;
    let state = states
        .iter()
        .find(|state| state.shim_path == shim_path)
        .ok_or_else(|| {
            ProxyError::Config(format!(
                "no managed Claude shim found at {}",
                shim_path.display()
            ))
        })?;

    if !path_contains_marker(&state.shim_path)? {
        return Err(ProxyError::Config(format!(
            "{} is no longer the managed Claude shim",
            state.shim_path.display()
        )));
    }

    restore_original(state)
}

/// Reinstall the managed launcher after Claude Code's native updater has
/// finished, recording the executable now selected by the native launcher.
pub fn finish_shim_update(state_path: &Path, shim_path: &Path) -> Result<ClaudeShimState> {
    let mut states = read_shim_states(state_path)?;
    let index = states
        .iter()
        .position(|state| state.shim_path == shim_path)
        .ok_or_else(|| {
            ProxyError::Config(format!(
                "no managed Claude shim found at {}",
                shim_path.display()
            ))
        })?;
    let captured = capture_original_claude(shim_path)?;
    let previous = &states[index];
    let updated = ClaudeShimState {
        original: captured.original,
        real_claude_path: captured.real_claude_path,
        installed_at: Utc::now().to_rfc3339(),
        ..previous.clone()
    };

    states[index] = updated.clone();
    write_shim_states(state_path, &states)?;
    replace_with_shim(&updated, shim_script(&updated).as_bytes())?;
    Ok(updated)
}

fn write_shim_states(state_path: &Path, states: &[ClaudeShimState]) -> Result<()> {
    let state_file = ClaudeShimStateFile {
        version: SHIM_STATE_VERSION,
        shims: states.to_vec(),
    };
    fs::write(state_path, serde_json::to_string_pretty(&state_file)?)?;
    Ok(())
}

fn install_one_shim(
    shim_path: PathBuf,
    options: &ClaudeShimInstallOptions,
    existing_states: &[ClaudeShimState],
) -> Result<ClaudeShimState> {
    let current_is_managed = path_contains_marker(&shim_path)?;

    let (original, real_claude_path, replacing_existing_shim) = if current_is_managed {
        let state = existing_states
            .iter()
            .find(|state| state.shim_path == shim_path)
            .cloned()
            .ok_or_else(|| {
                ProxyError::Config(format!(
                    "{} is already a managed Claude shim, but stored state is missing or mismatched",
                    shim_path.display()
                ))
            })?;
        (state.original, state.real_claude_path, true)
    } else {
        let captured = capture_original_claude(&shim_path)?;
        (captured.original, captured.real_claude_path, false)
    };

    let state = ClaudeShimState {
        version: SHIM_STATE_VERSION,
        shim_path: shim_path.clone(),
        real_claude_path,
        helper_path: options.helper_path.clone(),
        app_pid: options.app_pid,
        provider: options.settings.provider,
        port: options.settings.port,
        model: options.settings.model.clone(),
        small_fast_model: options.settings.small_fast_model.clone(),
        auto_compact_window: options.settings.auto_compact_window,
        original,
        installed_at: Utc::now().to_rfc3339(),
    };

    let script = shim_script(&state);
    if replacing_existing_shim {
        write_executable(&shim_path, script.as_bytes())?;
    } else if let Err(err) = replace_with_shim(&state, script.as_bytes()) {
        let _ = restore_original(&state);
        return Err(err);
    }

    Ok(state)
}

pub fn managed_env_strings(options: &ClaudeSettingsOptions) -> Vec<(String, String)> {
    managed_env(options)
        .into_iter()
        .map(|(key, value)| (key, display_value(&value)))
        .collect()
}

pub fn read_shim_state(path: &Path) -> Result<ClaudeShimState> {
    read_shim_states(path)?
        .into_iter()
        .next()
        .ok_or_else(|| ProxyError::Config("Claude shim state file contains no shims".into()))
}

pub fn read_shim_states(path: &Path) -> Result<Vec<ClaudeShimState>> {
    let raw = fs::read_to_string(path)?;
    if let Ok(file) = serde_json::from_str::<ClaudeShimStateFile>(&raw) {
        return Ok(file.shims);
    }
    Ok(vec![serde_json::from_str::<ClaudeShimState>(&raw)?])
}

pub fn path_contains_marker(path: &Path) -> Result<bool> {
    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(false),
        Err(err) if err.kind() == ErrorKind::PermissionDenied => return Ok(false),
        Err(err) => return Err(err.into()),
    };
    let mut buf = Vec::new();
    file.by_ref().take(8192).read_to_end(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).contains(SHIM_MARKER))
}

fn latest_backup_path(path: &Path) -> Result<Option<PathBuf>> {
    let Some(parent) = path.parent() else {
        return Ok(None);
    };
    if !parent.exists() {
        return Ok(None);
    }
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
    Ok(Some(latest))
}

pub fn managed_env(options: &ClaudeSettingsOptions) -> Map<String, Value> {
    let mut env = Map::new();
    let public_primary_model = claude_primary_alias(&options.model);
    let public_small_model = claude_small_alias(&options.small_fast_model);
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
        Value::String(public_primary_model.clone()),
    );
    env.insert(
        "ANTHROPIC_SMALL_FAST_MODEL".into(),
        Value::String(public_small_model.clone()),
    );
    env.insert(
        "ANTHROPIC_DEFAULT_OPUS_MODEL".into(),
        Value::String(DEFAULT_PUBLIC_PRIMARY_MODEL.into()),
    );
    env.insert(
        "ANTHROPIC_DEFAULT_SONNET_MODEL".into(),
        Value::String(DEFAULT_PUBLIC_SONNET_MODEL.into()),
    );
    env.insert(
        "ANTHROPIC_DEFAULT_HAIKU_MODEL".into(),
        Value::String(DEFAULT_PUBLIC_SMALL_MODEL.into()),
    );
    env.insert(
        "CLAUDE_CODE_SUBAGENT_MODEL".into(),
        Value::String(public_small_model),
    );
    env.insert(
        "CLAUDE_CODE_AUTO_COMPACT_WINDOW".into(),
        Value::Number(options.auto_compact_window.into()),
    );
    env.insert(
        "CLAUDE_CODE_ALWAYS_ENABLE_EFFORT".into(),
        Value::Number(1.into()),
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

fn claude_primary_alias(model: &str) -> String {
    let model = model.trim();
    if model.starts_with("claude-") && !model.contains("haiku") {
        model.to_string()
    } else {
        DEFAULT_PUBLIC_PRIMARY_MODEL.into()
    }
}

fn claude_small_alias(model: &str) -> String {
    let model = model.trim();
    if model.starts_with("claude-") && model.contains("haiku") {
        model.to_string()
    } else {
        DEFAULT_PUBLIC_SMALL_MODEL.into()
    }
}

#[derive(Debug, Clone)]
struct CapturedOriginal {
    original: ClaudeShimOriginal,
    real_claude_path: PathBuf,
}

fn discover_claude_paths() -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    paths.extend(discover_claude_paths_from_shell()?);
    paths.extend(
        common_claude_candidates()
            .into_iter()
            .filter(|path| path.exists()),
    );
    dedupe_paths(&mut paths);
    if !paths.is_empty() {
        return Ok(paths);
    }
    Err(ProxyError::Config(
        "could not find a claude command in the user shell or common install paths".into(),
    ))
}

fn discover_claude_paths_from_shell() -> Result<Vec<PathBuf>> {
    let shell = env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    let output = Command::new(shell)
        .arg("-l")
        .arg("-c")
        .arg("type -a -p claude 2>/dev/null || command -v claude")
        .output();
    let Ok(output) = output else {
        return Ok(Vec::new());
    };
    if !output.status.success() {
        return Ok(Vec::new());
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with('/'))
        .map(PathBuf::from)
        .collect())
}

fn common_claude_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    let Some(home) = dirs::home_dir() else {
        return candidates;
    };
    candidates.push(home.join(".local/bin/claude"));
    candidates.push(home.join(".claude/local/claude"));

    let nvm_versions = home.join(".nvm/versions/node");
    if let Ok(entries) = fs::read_dir(nvm_versions) {
        let mut node_versions = entries
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect::<Vec<_>>();
        node_versions.sort();
        node_versions.reverse();
        for version in node_versions {
            candidates.push(version.join("bin/claude"));
        }
    }
    candidates
}

fn dedupe_paths(paths: &mut Vec<PathBuf>) {
    let mut deduped = Vec::new();
    for path in paths.drain(..) {
        if !deduped.iter().any(|existing| existing == &path) {
            deduped.push(path);
        }
    }
    *paths = deduped;
}

fn parse_live_claude_sessions(ps_output: &str, current_pid: u32) -> Vec<LiveClaudeSession> {
    ps_output
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim_start();
            let (pid, command) = trimmed.split_once(char::is_whitespace)?;
            let pid = pid.parse::<u32>().ok()?;
            if pid == current_pid {
                return None;
            }
            let command = command.trim();
            if is_live_claude_code_session(command) {
                Some(LiveClaudeSession {
                    pid,
                    command: command.to_string(),
                })
            } else {
                None
            }
        })
        .collect()
}

fn is_live_claude_code_session(command: &str) -> bool {
    let mut parts = command.split_whitespace();
    let Some(program) = parts.next() else {
        return false;
    };
    let Some(file_name) = Path::new(program)
        .file_name()
        .and_then(|name| name.to_str())
    else {
        return false;
    };
    let is_claude = matches!(
        file_name.to_ascii_lowercase().as_str(),
        "claude" | "claude.exe"
    );
    if !is_claude {
        return false;
    }

    let args = parts.collect::<Vec<_>>();
    let is_background_helper = args
        .iter()
        .any(|arg| matches!(*arg, "--bg-pty-host" | "--bg-spare"));
    let is_daemon = matches!(args.as_slice(), ["daemon", "run", ..]);

    !is_background_helper && !is_daemon
}

fn truncate_command(command: &str) -> String {
    const MAX_LEN: usize = 140;
    if command.chars().count() <= MAX_LEN {
        return command.to_string();
    }
    let mut truncated = command.chars().take(MAX_LEN - 3).collect::<String>();
    truncated.push_str("...");
    truncated
}

fn capture_original_claude(path: &Path) -> Result<CapturedOriginal> {
    let metadata = fs::symlink_metadata(path).map_err(|err| {
        if err.kind() == ErrorKind::NotFound {
            ProxyError::Config(format!("Claude command not found at {}", path.display()))
        } else {
            err.into()
        }
    })?;

    if metadata.file_type().is_symlink() {
        let target = fs::read_link(path)?;
        let real_claude_path = resolve_symlink_target(path, &target)?;
        return Ok(CapturedOriginal {
            original: ClaudeShimOriginal::Symlink { target },
            real_claude_path,
        });
    }

    if metadata.is_file() {
        let backup_path = regular_file_backup_path(path)?;
        return Ok(CapturedOriginal {
            original: ClaudeShimOriginal::RegularFile {
                backup_path: backup_path.clone(),
            },
            real_claude_path: backup_path,
        });
    }

    Err(ProxyError::Config(format!(
        "Claude command at {} is not a file or symlink",
        path.display()
    )))
}

fn resolve_symlink_target(path: &Path, target: &Path) -> Result<PathBuf> {
    let resolved = if target.is_absolute() {
        target.to_path_buf()
    } else {
        path.parent().unwrap_or_else(|| Path::new("/")).join(target)
    };
    Ok(resolved.canonicalize()?)
}

fn regular_file_backup_path(path: &Path) -> Result<PathBuf> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| ProxyError::Config("Claude command path has no filename".into()))?;
    let timestamp = Utc::now().format("%Y%m%dT%H%M%SZ");
    for suffix in 0..100 {
        let candidate = if suffix == 0 {
            path.with_file_name(format!("{file_name}.cc-codex-proxy-original-{timestamp}"))
        } else {
            path.with_file_name(format!(
                "{file_name}.cc-codex-proxy-original-{timestamp}-{suffix}"
            ))
        };
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(ProxyError::Config(format!(
        "could not allocate a backup path for {}",
        path.display()
    )))
}

fn replace_with_shim(state: &ClaudeShimState, script: &[u8]) -> Result<()> {
    match &state.original {
        ClaudeShimOriginal::Symlink { .. } => {
            fs::remove_file(&state.shim_path)?;
            write_executable(&state.shim_path, script)?;
        }
        ClaudeShimOriginal::RegularFile { backup_path } => {
            fs::rename(&state.shim_path, backup_path)?;
            write_executable(&state.shim_path, script)?;
        }
    }
    Ok(())
}

fn restore_original(state: &ClaudeShimState) -> Result<()> {
    if state.shim_path.exists() {
        fs::remove_file(&state.shim_path)?;
    }
    match &state.original {
        ClaudeShimOriginal::Symlink { target } => {
            #[cfg(unix)]
            {
                std::os::unix::fs::symlink(target, &state.shim_path)?;
            }
            #[cfg(not(unix))]
            {
                return Err(ProxyError::Config(
                    "Claude shim restore is only supported on Unix platforms".into(),
                ));
            }
        }
        ClaudeShimOriginal::RegularFile { backup_path } => {
            fs::rename(backup_path, &state.shim_path)?;
        }
    }
    Ok(())
}

fn write_executable(path: &Path, bytes: &[u8]) -> Result<()> {
    fs::write(path, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

fn shim_script(state: &ClaudeShimState) -> String {
    format!(
        "#!/usr/bin/env bash\n\
         # {marker}\n\
         exec {helper} claude launch \\\n           --provider {provider} \\\n           --app-pid {app_pid} \\\n           --shim-path {shim_path} \\\n           --real-claude {real_claude} \\\n           --model {model} \\\n           --small-model {small_model} \\\n           --port {port} \\\n           --auto-compact-window {auto_compact_window} \\\n           -- \"$@\"\n",
        marker = SHIM_MARKER,
        helper = shell_quote(&state.helper_path.display().to_string()),
        provider = state.provider.as_str(),
        app_pid = state.app_pid,
        shim_path = shell_quote(&state.shim_path.display().to_string()),
        real_claude = shell_quote(&state.real_claude_path.display().to_string()),
        model = shell_quote(&state.model),
        small_model = shell_quote(&state.small_fast_model),
        port = state.port,
        auto_compact_window = state.auto_compact_window,
    )
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn managed_changes(current: &Value, managed: &Map<String, Value>) -> Vec<ClaudeEnvChange> {
    let current_env = current.get("env").and_then(Value::as_object);

    MANAGED_ENV_KEYS
        .iter()
        .filter_map(|key| {
            let proposed = managed.get(*key)?;
            let current = current_env.and_then(|env| env.get(*key));
            let action = match current {
                None => ClaudeEnvChangeAction::Add,
                Some(value) if value == proposed => ClaudeEnvChangeAction::Keep,
                Some(_) => ClaudeEnvChangeAction::Change,
            };

            Some(ClaudeEnvChange {
                key: (*key).to_string(),
                action,
                current: current.map(display_value),
                proposed: display_value(proposed),
            })
        })
        .collect()
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

fn read_existing_pretty_or_raw(path: &Path) -> Result<String> {
    let raw = fs::read_to_string(path)?;
    if raw.trim().is_empty() {
        return Ok("{}\n".into());
    }
    match serde_json::from_str::<Value>(&raw) {
        Ok(value) => pretty_json(&value),
        Err(_) => Ok(raw),
    }
}

fn pretty_json(value: &Value) -> Result<String> {
    Ok(format!("{}\n", serde_json::to_string_pretty(value)?))
}

fn display_value(value: &Value) -> String {
    value
        .as_str()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| value.to_string())
}

fn merge_env(settings: &mut Value, env: Map<String, Value>) {
    if !settings.is_object() {
        *settings = json!({});
    }
    let object = settings.as_object_mut().expect("settings object");
    let entry = object
        .entry("env")
        .or_insert_with(|| Value::Object(Map::new()));
    if !entry.is_object() {
        *entry = Value::Object(Map::new());
    }
    let env_object = entry.as_object_mut().expect("env object");
    for (key, value) in env {
        env_object.insert(key, value);
    }
}

fn write_pretty(path: &Path, value: &Value) -> Result<()> {
    fs::write(path, pretty_json(value)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_env_is_provider_neutral() {
        let codex = ClaudeSettingsOptions {
            provider: Provider::Codex,
            ..ClaudeSettingsOptions::default()
        };
        let deepseek = ClaudeSettingsOptions {
            provider: Provider::DeepSeek,
            ..ClaudeSettingsOptions::default()
        };

        let codex_env = managed_env(&codex);
        let deepseek_env = managed_env(&deepseek);
        assert_eq!(
            codex_env.get("ANTHROPIC_BASE_URL"),
            deepseek_env.get("ANTHROPIC_BASE_URL")
        );
        assert_eq!(
            codex_env.get("ANTHROPIC_MODEL"),
            deepseek_env.get("ANTHROPIC_MODEL")
        );
        assert_eq!(
            codex_env.get("CLAUDE_CODE_SUBAGENT_MODEL"),
            deepseek_env.get("CLAUDE_CODE_SUBAGENT_MODEL")
        );
        assert_eq!(
            codex_env.get("ANTHROPIC_DEFAULT_OPUS_MODEL"),
            Some(&Value::String(DEFAULT_PUBLIC_PRIMARY_MODEL.into()))
        );
        assert_eq!(
            codex_env.get("ANTHROPIC_DEFAULT_SONNET_MODEL"),
            Some(&Value::String(DEFAULT_PUBLIC_SONNET_MODEL.into()))
        );
        assert_eq!(
            codex_env.get("ANTHROPIC_DEFAULT_HAIKU_MODEL"),
            Some(&Value::String(DEFAULT_PUBLIC_SMALL_MODEL.into()))
        );
    }

    #[test]
    fn managed_env_coerces_provider_models_to_claude_aliases() {
        let env = managed_env(&ClaudeSettingsOptions {
            provider: Provider::Codex,
            model: "gpt-5.5[1m]".into(),
            small_fast_model: "gpt-5.4-mini[1m]".into(),
            ..ClaudeSettingsOptions::default()
        });

        assert_eq!(
            env.get("ANTHROPIC_MODEL"),
            Some(&Value::String(DEFAULT_PUBLIC_PRIMARY_MODEL.into()))
        );
        assert_eq!(
            env.get("ANTHROPIC_SMALL_FAST_MODEL"),
            Some(&Value::String(DEFAULT_PUBLIC_SMALL_MODEL.into()))
        );
        assert_eq!(
            env.get("CLAUDE_CODE_SUBAGENT_MODEL"),
            Some(&Value::String(DEFAULT_PUBLIC_SMALL_MODEL.into()))
        );
    }

    #[test]
    fn install_settings_is_idempotent_when_managed_env_is_current() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let options = ClaudeSettingsOptions::default();

        let first = install_settings(&path, &options).unwrap();
        assert!(first.backup_path.is_none());
        let first_raw = fs::read_to_string(&path).unwrap();

        let second = install_settings(&path, &options).unwrap();
        assert!(second.backup_path.is_none());
        let second_raw = fs::read_to_string(&path).unwrap();
        assert_eq!(second_raw, first_raw);

        let backup_count = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with("settings.json.backup-"))
            })
            .count();
        assert_eq!(backup_count, 0);
    }

    #[cfg(unix)]
    #[test]
    fn native_update_reinstalls_shim_with_the_new_claude_target() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let first_version = dir.path().join("claude-1");
        let second_version = dir.path().join("claude-2");
        fs::write(&first_version, "first").unwrap();
        fs::write(&second_version, "second").unwrap();

        let shim_path = dir.path().join("claude");
        symlink(&first_version, &shim_path).unwrap();
        let state_path = dir.path().join("claude-shim.json");
        let options = ClaudeShimInstallOptions {
            app_pid: 42,
            helper_path: dir.path().join("cc-codex-proxy"),
            claude_path: Some(shim_path.clone()),
            settings: ClaudeSettingsOptions::default(),
        };
        install_shim(&state_path, &options).unwrap();
        assert!(fs::read_to_string(&shim_path)
            .unwrap()
            .contains("--shim-path"));

        begin_shim_update(&state_path, &shim_path).unwrap();
        assert_eq!(fs::read_link(&shim_path).unwrap(), first_version);

        fs::remove_file(&shim_path).unwrap();
        symlink(&second_version, &shim_path).unwrap();
        let updated = finish_shim_update(&state_path, &shim_path).unwrap();

        assert_eq!(
            updated.real_claude_path,
            second_version.canonicalize().unwrap()
        );
        assert!(fs::read_to_string(&shim_path)
            .unwrap()
            .contains(SHIM_MARKER));

        restore_shim(&state_path).unwrap();
        assert_eq!(fs::read_link(&shim_path).unwrap(), second_version);
    }

    #[test]
    fn live_session_parser_ignores_claude_background_helpers() {
        let output = r#"
          10 /Users/me/.local/bin/claude
          11 /Users/me/.local/share/claude/ClaudeCode.app/Contents/MacOS/claude --bg-pty-host /tmp/cc-daemon-501/session/pty.sock 162 66
          12 /Users/me/.nvm/versions/node/v22/lib/node_modules/@anthropic-ai/claude-code/bin/claude.exe --continue
          13 /Users/me/.local/bin/claude bg-spare --bg-spare /tmp/cc-daemon-501/bb6ac682/spare/74f016fa.claim.sock
          14 /Users/me/.local/bin/claude daemon run --json-path /Users/me/.claude/daemon.json --log-file /Users/me/.claude/daemon.log --orphaned
        "#;

        let sessions = parse_live_claude_sessions(output, 99);

        assert_eq!(
            sessions,
            vec![
                LiveClaudeSession {
                    pid: 10,
                    command: "/Users/me/.local/bin/claude".into(),
                },
                LiveClaudeSession {
                    pid: 12,
                    command: "/Users/me/.nvm/versions/node/v22/lib/node_modules/@anthropic-ai/claude-code/bin/claude.exe --continue".into(),
                },
            ]
        );
    }
}
