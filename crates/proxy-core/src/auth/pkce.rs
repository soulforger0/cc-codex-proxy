use crate::{
    config::{OAUTH_CALLBACK_PORT, OAUTH_REDIRECT_URI},
    error::{ProxyError, Result},
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::{distributions::Alphanumeric, Rng};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use url::Url;

#[derive(Debug, Clone)]
pub struct OAuthOptions {
    pub issuer: String,
    pub client_id: String,
    pub redirect_uri: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TokenResponse {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_in: Option<i64>,
    #[serde(default)]
    pub id_token: Option<String>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl TokenResponse {
    pub fn validate_initial(&self) -> Result<()> {
        if self.access_token.is_empty() {
            return Err(ProxyError::InvalidRequest(
                "OAuth response did not include access_token".into(),
            ));
        }
        if self.refresh_token.as_deref().unwrap_or("").is_empty() {
            return Err(ProxyError::InvalidRequest(
                "OAuth response did not include refresh_token".into(),
            ));
        }
        Ok(())
    }
}

pub async fn browser_login(opts: OAuthOptions) -> Result<TokenResponse> {
    let pkce = Pkce::new();
    let listener = TcpListener::bind(("127.0.0.1", OAUTH_CALLBACK_PORT)).await?;
    let auth_url = build_authorization_url(&opts, &pkce)?;
    open::that(auth_url.as_str())
        .map_err(|err| ProxyError::Transport(format!("failed to open browser: {err}")))?;
    println!("Opened browser for ChatGPT OAuth. If it did not open, visit:\n{auth_url}\n");
    let code = wait_for_callback(listener, &pkce.state).await?;
    exchange_code(&opts, &code, &pkce.verifier).await
}

pub async fn exchange_code(
    opts: &OAuthOptions,
    code: &str,
    verifier: &str,
) -> Result<TokenResponse> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()?;
    let response = client
        .post(format!("{}/oauth/token", opts.issuer.trim_end_matches('/')))
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", opts.client_id.as_str()),
            ("code", code),
            ("redirect_uri", opts.redirect_uri.as_str()),
            ("code_verifier", verifier),
        ])
        .send()
        .await?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(ProxyError::Upstream {
            status,
            body,
            retry_after: None,
        });
    }
    let tokens = response.json::<TokenResponse>().await?;
    tokens.validate_initial()?;
    Ok(tokens)
}

fn build_authorization_url(opts: &OAuthOptions, pkce: &Pkce) -> Result<Url> {
    let mut url = Url::parse(&format!(
        "{}/oauth/authorize",
        opts.issuer.trim_end_matches('/')
    ))
    .map_err(|err| ProxyError::Config(format!("bad OAuth issuer URL: {err}")))?;
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &opts.client_id)
        .append_pair("redirect_uri", &opts.redirect_uri)
        .append_pair("scope", "openid profile email offline_access")
        .append_pair("code_challenge", &pkce.challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", &pkce.state);
    Ok(url)
}

async fn wait_for_callback(listener: TcpListener, expected_state: &str) -> Result<String> {
    let (mut socket, _) = listener.accept().await?;
    let mut buf = vec![0_u8; 8192];
    let n = socket.read(&mut buf).await?;
    let req = String::from_utf8_lossy(&buf[..n]);
    let first_line = req.lines().next().unwrap_or_default();
    let path = first_line.split_whitespace().nth(1).ok_or_else(|| {
        ProxyError::InvalidRequest("OAuth callback did not include request path".into())
    })?;
    let url = Url::parse(&format!("http://localhost{path}"))
        .map_err(|err| ProxyError::InvalidRequest(format!("bad OAuth callback URL: {err}")))?;
    let state = url
        .query_pairs()
        .find(|(k, _)| k == "state")
        .map(|(_, v)| v.to_string());
    if state.as_deref() != Some(expected_state) {
        let _ = write_callback_response(&mut socket, 400, "OAuth state mismatch").await;
        return Err(ProxyError::InvalidRequest("OAuth state mismatch".into()));
    }
    let code = url
        .query_pairs()
        .find(|(k, _)| k == "code")
        .map(|(_, v)| v.to_string());
    match code {
        Some(code) if !code.is_empty() => {
            write_callback_response(
                &mut socket,
                200,
                "Authentication complete. You can close this tab.",
            )
            .await?;
            Ok(code)
        }
        _ => {
            write_callback_response(&mut socket, 400, "OAuth callback did not include a code")
                .await?;
            Err(ProxyError::InvalidRequest(
                "OAuth callback did not include a code".into(),
            ))
        }
    }
}

async fn write_callback_response(
    socket: &mut tokio::net::TcpStream,
    status: u16,
    body: &str,
) -> Result<()> {
    let status_text = if status == 200 { "OK" } else { "Bad Request" };
    let html = format!("<html><body>{body}</body></html>");
    let response = format!(
        "HTTP/1.1 {status} {status_text}\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        html.len(),
        html
    );
    socket.write_all(response.as_bytes()).await?;
    Ok(())
}

#[derive(Debug, Clone)]
struct Pkce {
    verifier: String,
    challenge: String,
    state: String,
}

impl Pkce {
    fn new() -> Self {
        let verifier = random_string(96);
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        Self {
            verifier,
            challenge,
            state: random_string(32),
        }
    }
}

fn random_string(len: usize) -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(len)
        .map(char::from)
        .collect()
}

pub fn default_oauth_options(issuer: String, client_id: String) -> OAuthOptions {
    OAuthOptions {
        issuer,
        client_id,
        redirect_uri: OAUTH_REDIRECT_URI.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_requires_refresh_token_for_initial_login() {
        let tokens = TokenResponse {
            access_token: "access".into(),
            refresh_token: None,
            expires_in: Some(3600),
            id_token: None,
            extra: Default::default(),
        };
        assert!(tokens.validate_initial().is_err());
    }
}
