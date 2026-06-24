use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use proxy_core::{
    auth::{browser_login, default_oauth_options, AuthManager, FileTokenStore, OAuthRefreshClient},
    claude::{
        default_settings_path, install_settings, install_shim, live_claude_sessions,
        live_claude_sessions_message, managed_env_strings, preview_settings, restore_latest_backup,
        restore_shim, ClaudeSettingsOptions, ClaudeShimInstallOptions, MANAGED_ENV_KEYS,
    },
    config::{AppConfig, DEFAULT_PORT},
    logging,
    model::ModelRegistry,
    serve,
};
use std::{path::PathBuf, process::Command as StdCommand, sync::Arc, time::Duration};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

#[derive(Debug, Parser)]
#[command(name = "cc-codex-proxy")]
#[command(about = "Local Claude Code to ChatGPT Codex proxy")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    Serve(ServeArgs),
    Auth(AuthCommand),
    Doctor(DoctorArgs),
    Claude(ClaudeCommand),
    Admin(AdminCommand),
    Bench(BenchArgs),
}

#[derive(Debug, Args)]
struct ServeArgs {
    #[arg(long, env = "PORT")]
    port: Option<u16>,
}

#[derive(Debug, Args)]
struct DoctorArgs {
    #[arg(long, default_value = "gpt-5.4")]
    model: String,
}

#[derive(Debug, Subcommand)]
enum AuthSubcommand {
    Login,
    Status,
    Logout,
}

#[derive(Debug, Args)]
struct AuthCommand {
    #[command(subcommand)]
    command: AuthSubcommand,
}

#[derive(Debug, Subcommand)]
enum ClaudeSubcommand {
    InstallSettings(InstallSettingsArgs),
    PreviewSettings(InstallSettingsArgs),
    RestoreSettings,
    InstallShim(InstallShimArgs),
    RestoreShim,
    CheckLiveSessions,
    Launch(LaunchArgs),
}

#[derive(Debug, Args)]
struct ClaudeCommand {
    #[command(subcommand)]
    command: ClaudeSubcommand,
}

#[derive(Debug, Args)]
struct InstallSettingsArgs {
    #[arg(long, default_value = "gpt-5.4[1m]")]
    model: String,
    #[arg(long = "small-model", default_value = "gpt-5.4-mini[1m]")]
    small_model: String,
    #[arg(long, default_value_t = DEFAULT_PORT)]
    port: u16,
    #[arg(long, default_value_t = 272_000)]
    auto_compact_window: u32,
}

#[derive(Debug, Args)]
struct InstallShimArgs {
    #[command(flatten)]
    settings: InstallSettingsArgs,
    #[arg(long)]
    app_pid: u32,
    #[arg(long)]
    claude_path: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct LaunchArgs {
    #[arg(long)]
    app_pid: u32,
    #[arg(long)]
    real_claude: PathBuf,
    #[command(flatten)]
    settings: InstallSettingsArgs,
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
}

#[derive(Debug, Subcommand)]
enum AdminSubcommand {
    Status,
}

#[derive(Debug, Args)]
struct AdminCommand {
    #[command(subcommand)]
    command: AdminSubcommand,
}

#[derive(Debug, Args)]
struct BenchArgs {
    #[arg(long, default_value_t = 100)]
    agents: usize,
    #[arg(long, default_value_t = DEFAULT_PORT)]
    port: u16,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli
        .command
        .unwrap_or(Command::Serve(ServeArgs { port: None }))
    {
        Command::Serve(args) => cmd_serve(args).await,
        Command::Auth(args) => cmd_auth(args).await,
        Command::Doctor(args) => cmd_doctor(args).await,
        Command::Claude(args) => cmd_claude(args).await,
        Command::Admin(args) => cmd_admin(args).await,
        Command::Bench(args) => cmd_bench(args).await,
    }
}

async fn cmd_serve(args: ServeArgs) -> Result<()> {
    exit_if_live_claude_sessions()?;
    let (mut config, paths) = AppConfig::load_default()?;
    if let Some(port) = args.port {
        config.port = port;
    }
    let _guards = logging::init(&paths, config.log.stderr, config.log.verbose)?;
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        port = config.port,
        transport = ?config.codex.transport,
        base_url = %config.codex.base_url,
        log_path = %paths.logs_dir.join("proxy.log").display(),
        "starting cc-codex-proxy server"
    );
    let auth = auth_manager(&config, &paths);
    let handle = serve(config.clone(), paths.clone(), auth).await?;
    println!("Proxy listening on http://{}", handle.addr);
    println!("Health: http://{}/healthz", handle.addr);
    println!("Logs: {}", paths.logs_dir.join("proxy.log").display());
    println!("Claude Code:");
    println!("  export ANTHROPIC_BASE_URL=\"http://{}\"", handle.addr);
    println!("  export ANTHROPIC_AUTH_TOKEN=\"unused\"");
    println!("  export ANTHROPIC_MODEL=\"gpt-5.4[1m]\"");
    println!("  export ANTHROPIC_DEFAULT_HAIKU_MODEL=\"gpt-5.4-mini[1m]\"");
    println!("  export ANTHROPIC_SMALL_FAST_MODEL=\"gpt-5.4-mini[1m]\"");
    println!("  export CLAUDE_CODE_ALWAYS_ENABLE_EFFORT=\"1\"");
    tokio::signal::ctrl_c().await?;
    handle.stop().await;
    Ok(())
}

async fn cmd_auth(args: AuthCommand) -> Result<()> {
    let (config, paths) = AppConfig::load_default()?;
    let manager = auth_manager(&config, &paths);
    match args.command {
        AuthSubcommand::Login => {
            let opts =
                default_oauth_options(config.codex.oauth_issuer, config.codex.oauth_client_id);
            let tokens = browser_login(opts).await?;
            let stored = manager.persist_initial(tokens).await?;
            println!("Authenticated.");
            if let Some(account_id) = stored.account_id {
                println!("Account: {account_id}");
            }
            println!("Expires: {}", stored.expires_at_ms);
            println!("Storage: {}", manager.storage_label());
        }
        AuthSubcommand::Status => match manager.status().await? {
            Some(auth) => {
                println!("Authenticated: yes");
                println!("Storage: {}", manager.storage_label());
                if let Some(account_id) = auth.account_id {
                    println!("Account: {account_id}");
                }
                println!("ExpiresAtMs: {}", auth.expires_at_ms);
            }
            None => {
                println!("Authenticated: no");
                std::process::exit(1);
            }
        },
        AuthSubcommand::Logout => {
            manager.logout().await?;
            println!("Logged out.");
        }
    }
    Ok(())
}

async fn cmd_doctor(args: DoctorArgs) -> Result<()> {
    let (config, paths) = AppConfig::load_default()?;
    let registry = ModelRegistry::load_or_create(&paths.model_profiles_file)?;
    let resolved = registry.resolve(&args.model)?;
    println!("Config: {}", paths.config_file.display());
    println!("Model profiles: {}", paths.model_profiles_file.display());
    println!("Model: {} -> {}", args.model, resolved.upstream_model);
    println!("Transport: {:?}", config.codex.transport);
    let manager = auth_manager(&config, &paths);
    match manager.get_auth().await {
        Ok(auth) => {
            println!("Auth: ok");
            if let Some(account_id) = auth.account_id {
                println!("Account: {account_id}");
            }
            println!("Storage: {}", manager.storage_label());
        }
        Err(err) => {
            println!("Auth: failed ({err})");
            std::process::exit(1);
        }
    }
    Ok(())
}

async fn cmd_claude(args: ClaudeCommand) -> Result<()> {
    let settings = default_settings_path()?;
    match args.command {
        ClaudeSubcommand::InstallSettings(args) => {
            let result = install_settings(&settings, &claude_settings_options(args))?;
            println!("Updated {}", result.settings_path.display());
            if let Some(backup) = result.backup_path {
                println!("Backup: {}", backup.display());
            }
        }
        ClaudeSubcommand::PreviewSettings(args) => {
            let preview = preview_settings(&settings, &claude_settings_options(args))?;
            println!("{}", serde_json::to_string_pretty(&preview)?);
        }
        ClaudeSubcommand::RestoreSettings => match restore_latest_backup(&settings)? {
            Some(backup) => {
                println!("Restored {} from {}", settings.display(), backup.display());
            }
            None => {
                println!("No backup found for {}", settings.display());
                std::process::exit(1);
            }
        },
        ClaudeSubcommand::InstallShim(args) => {
            let (_, paths) = AppConfig::load_default()?;
            let helper_path = std::env::current_exe().context("failed to locate proxy helper")?;
            let result = install_shim(
                &paths.claude_shim_file,
                &ClaudeShimInstallOptions {
                    app_pid: args.app_pid,
                    helper_path,
                    claude_path: args.claude_path,
                    settings: claude_settings_options(args.settings),
                },
            )?;
            println!("Claude shims installed: {}", result.states.len());
            for state in &result.states {
                println!(
                    "Shim: {} -> {}",
                    state.shim_path.display(),
                    state.real_claude_path.display()
                );
            }
            println!("State: {}", paths.claude_shim_file.display());
        }
        ClaudeSubcommand::RestoreShim => {
            let (_, paths) = AppConfig::load_default()?;
            match restore_shim(&paths.claude_shim_file) {
                Ok(result) => {
                    println!("Claude shims restored: {}", result.restored.len());
                    for path in &result.restored {
                        println!("Restored: {}", path.display());
                    }
                    for skipped in &result.skipped {
                        println!(
                            "Skipped: {} ({})",
                            skipped.shim_path.display(),
                            skipped.reason
                        );
                    }
                }
                Err(err) => {
                    println!("Claude shim restore skipped: {err}");
                }
            }
        }
        ClaudeSubcommand::CheckLiveSessions => {
            let sessions = live_claude_sessions()?;
            if sessions.is_empty() {
                println!("No live Claude Code sessions found.");
            } else {
                eprintln!("{}", live_claude_sessions_message(&sessions));
                std::process::exit(2);
            }
        }
        ClaudeSubcommand::Launch(args) => {
            launch_claude(args).await?;
        }
    }
    Ok(())
}

fn claude_settings_options(args: InstallSettingsArgs) -> ClaudeSettingsOptions {
    ClaudeSettingsOptions {
        port: args.port,
        model: args.model,
        small_fast_model: args.small_model,
        auto_compact_window: args.auto_compact_window,
    }
}

fn exit_if_live_claude_sessions() -> Result<()> {
    let sessions = live_claude_sessions()?;
    if sessions.is_empty() {
        return Ok(());
    }
    eprintln!("{}", live_claude_sessions_message(&sessions));
    std::process::exit(2);
}

async fn launch_claude(args: LaunchArgs) -> Result<()> {
    let settings = claude_settings_options(args.settings);
    let app_is_alive = pid_is_alive(args.app_pid);
    let mut command = StdCommand::new(&args.real_claude);
    command.args(&args.args);
    for key in MANAGED_ENV_KEYS {
        command.env_remove(key);
    }

    if app_is_alive {
        if proxy_health_ok(settings.port).await {
            for (key, value) in managed_env_strings(&settings) {
                command.env(key, value);
            }
        } else {
            let message = format!(
                "CC Codex Proxy is open, but the proxy server is stopped on 127.0.0.1:{}. Start the proxy before launching Claude Code.",
                settings.port
            );
            eprintln!("{message}");
            notify_proxy_stopped(&message);
            std::process::exit(2);
        }
    }

    exec_command(command)
}

fn pid_is_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    StdCommand::new("/bin/kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

async fn proxy_health_ok(port: u16) -> bool {
    let url = format!("http://127.0.0.1:{port}/healthz");
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_millis(700))
        .build()
    {
        Ok(client) => client,
        Err(_) => return false,
    };
    match client.get(url).send().await {
        Ok(response) => response.status().is_success(),
        Err(_) => false,
    }
}

fn notify_proxy_stopped(message: &str) {
    let script = format!(
        "display notification {} with title {}",
        applescript_string(message),
        applescript_string("CC Codex Proxy")
    );
    let _ = StdCommand::new("osascript").arg("-e").arg(script).spawn();
}

fn applescript_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(unix)]
fn exec_command(mut command: StdCommand) -> Result<()> {
    let err = command.exec();
    Err(err).context("failed to launch Claude Code")
}

#[cfg(not(unix))]
fn exec_command(mut command: StdCommand) -> Result<()> {
    let status = command.status().context("failed to launch Claude Code")?;
    std::process::exit(status.code().unwrap_or(1));
}

async fn cmd_admin(args: AdminCommand) -> Result<()> {
    let (config, _) = AppConfig::load_default()?;
    match args.command {
        AdminSubcommand::Status => {
            let client = reqwest::Client::new();
            let resp = client
                .get(format!("http://127.0.0.1:{}/admin/status", config.port))
                .header("x-cc-codex-admin-token", config.admin_token)
                .send()
                .await
                .context("failed to reach local proxy admin endpoint")?;
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            if !status.is_success() {
                anyhow::bail!("admin status failed: {status} {body}");
            }
            println!("{body}");
        }
    }
    Ok(())
}

async fn cmd_bench(args: BenchArgs) -> Result<()> {
    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "model": "gpt-5.4",
        "max_tokens": 1,
        "messages": [{"role": "user", "content": "count"}]
    });
    let started = std::time::Instant::now();
    let mut tasks = Vec::with_capacity(args.agents);
    for _ in 0..args.agents {
        let client = client.clone();
        let body = body.clone();
        let url = format!("http://127.0.0.1:{}/v1/messages/count_tokens", args.port);
        tasks.push(tokio::spawn(async move {
            let resp = client.post(url).json(&body).send().await?;
            anyhow::ensure!(resp.status().is_success(), "status {}", resp.status());
            Ok::<(), anyhow::Error>(())
        }));
    }
    for task in tasks {
        task.await??;
    }
    println!(
        "Completed {} concurrent local count_tokens requests in {:?}",
        args.agents,
        started.elapsed()
    );
    Ok(())
}

fn auth_manager(config: &AppConfig, paths: &proxy_core::AppPaths) -> AuthManager {
    AuthManager::new(
        Arc::new(FileTokenStore::new(paths.auth_file.clone())),
        Arc::new(OAuthRefreshClient::new(
            config.codex.oauth_issuer.clone(),
            config.codex.oauth_client_id.clone(),
        )),
    )
}
