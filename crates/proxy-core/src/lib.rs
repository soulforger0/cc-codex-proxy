pub mod anthropic;
pub mod auth;
pub mod claude;
pub mod codex;
pub mod config;
pub mod custom_openai;
pub mod deepseek;
pub mod error;
pub mod logging;
pub mod model;
pub mod routing;
pub mod server;

pub use config::{AppConfig, AppPaths};
pub use server::{serve, ServerHandle};
