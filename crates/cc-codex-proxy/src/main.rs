use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use proxy_core::{
    auth::{
        browser_login, default_oauth_options, AuthManager, KeychainTokenStore, OAuthRefreshClient,
    },
    claude::{
        default_settings_path, install_settings, restore_latest_backup, ClaudeSettingsOptions,
    },
    config::{AppConfig, DEFAULT_PORT},
    logging,
    model::ModelRegistry,
    serve,
};
use std::sync::Arc;

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
    RestoreSettings,
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
    let (mut config, paths) = AppConfig::load_default()?;
    if let Some(port) = args.port {
        config.port = port;
    }
    let _guards = logging::init(&paths, config.log.stderr, config.log.verbose)?;
    let auth = auth_manager(&config);
    let handle = serve(config.clone(), paths.clone(), auth).await?;
    println!("Proxy listening on http://{}", handle.addr);
    println!("Health: http://{}/healthz", handle.addr);
    println!("Logs: {}", paths.logs_dir.join("proxy.log").display());
    println!("Claude Code:");
    println!("  export ANTHROPIC_BASE_URL=\"http://{}\"", handle.addr);
    println!("  export ANTHROPIC_AUTH_TOKEN=\"unused\"");
    println!("  export ANTHROPIC_MODEL=\"gpt-5.4[1m]\"");
    println!("  export ANTHROPIC_SMALL_FAST_MODEL=\"gpt-5.4-mini[1m]\"");
    tokio::signal::ctrl_c().await?;
    handle.stop().await;
    Ok(())
}

async fn cmd_auth(args: AuthCommand) -> Result<()> {
    let (config, _) = AppConfig::load_default()?;
    let manager = auth_manager(&config);
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
    let manager = auth_manager(&config);
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
            let result = install_settings(
                &settings,
                &ClaudeSettingsOptions {
                    port: args.port,
                    model: args.model,
                    small_fast_model: args.small_model,
                    auto_compact_window: args.auto_compact_window,
                },
            )?;
            println!("Updated {}", result.settings_path.display());
            if let Some(backup) = result.backup_path {
                println!("Backup: {}", backup.display());
            }
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
    }
    Ok(())
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

fn auth_manager(config: &AppConfig) -> AuthManager {
    AuthManager::new(
        Arc::new(KeychainTokenStore::default()),
        Arc::new(OAuthRefreshClient::new(
            config.codex.oauth_issuer.clone(),
            config.codex.oauth_client_id.clone(),
        )),
    )
}
