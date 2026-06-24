use crate::{config::AppPaths, error::Result};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

pub fn init(paths: &AppPaths, stderr: bool, verbose: bool) -> Result<Vec<WorkerGuard>> {
    paths.ensure()?;
    let filter = if verbose {
        EnvFilter::new("cc_codex_proxy=debug,proxy_core=debug,tower_http=info")
    } else {
        EnvFilter::new("cc_codex_proxy=info,proxy_core=info,tower_http=warn")
    };

    let file_appender = tracing_appender::rolling::never(&paths.logs_dir, "proxy.log");
    let (file_writer, file_guard) = tracing_appender::non_blocking(file_appender);
    let file_layer = fmt::layer().json().with_writer(file_writer).with_ansi(false);

    let registry = tracing_subscriber::registry().with(filter).with(file_layer);
    if stderr {
        registry.with(fmt::layer().with_writer(std::io::stderr)).try_init().ok();
    } else {
        registry.try_init().ok();
    }
    Ok(vec![file_guard])
}

pub fn redact_header(name: &str, value: &str) -> String {
    let lower = name.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "authorization" | "cookie" | "set-cookie" | "chatgpt-account-id" | "x-cc-codex-admin-token"
    ) {
        format!("[redacted len={}]", value.len())
    } else {
        value.to_string()
    }
}

