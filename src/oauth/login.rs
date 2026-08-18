//! Authorization-code login: start a loopback callback server, send the user
//! to the issuer, exchange the returned code for tokens.

use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use url::Url;

use super::{account_id, generate_pkce, jwt_expiry, random_state, TokenResponse, Tokens};
use crate::config::OauthConfig;

/// How long to wait for the user to finish in the browser.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);

pub struct Started {
    pub authorize_url: String,
    listener: TcpListener,
    redirect_uri: String,
    verifier: String,
    state: String,
}

/// Bind the callback port and build the authorize URL. Binding first means a
/// port clash is reported before the user is sent to a browser.
pub async fn start(config: &OauthConfig) -> Result<Started, String> {
    let listener = TcpListener::bind(("127.0.0.1", config.callback_port)).await.map_err(|err| {
        format!(
            "cannot listen on 127.0.0.1:{} for the login callback: {err} \
             (another login in progress, or the provider's CLI is running?)",
            config.callback_port
        )
    })?;

    let pkce = generate_pkce();
    let state = random_state();
    // The issuer matches this against its registered redirect URIs, so the
    // host spelling matters: `localhost`, not `127.0.0.1`.
    let redirect_uri =
        format!("http://localhost:{}{}", config.callback_port, config.callback_path);

    let mut url = Url::parse(&format!("{}/oauth/authorize", config.issuer.trim_end_matches('/')))
        .map_err(|err| format!("invalid issuer URL: {err}"))?;
    {
        let mut query = url.query_pairs_mut();
        query
            .append_pair("response_type", "code")
            .append_pair("client_id", &config.client_id)
            .append_pair("redirect_uri", &redirect_uri)
            .append_pair("scope", &config.scope)
            .append_pair("code_challenge", &pkce.challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", &state);
        for (key, value) in &config.authorize_extra {
            query.append_pair(key, value);
        }
    }

    Ok(Started {
        authorize_url: url.to_string(),
        listener,
        redirect_uri,
        verifier: pkce.verifier,
        state,
    })
}

/// Best-effort browser launch; the caller always prints the URL too.
pub fn open_browser(url: &str) {
    let _ = std::process::Command::new("xdg-open")
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

impl Started {
    /// Wait for the redirect, verify state, and exchange the code.
    pub async fn complete(
        self,
        client: &reqwest::Client,
        config: &OauthConfig,
    ) -> Result<Tokens, String> {
        let code = tokio::time::timeout(LOGIN_TIMEOUT, self.wait_for_code())
            .await
            .map_err(|_| "timed out waiting for the browser redirect".to_string())??;
        exchange(client, config, &code, &self.redirect_uri, &self.verifier).await
    }

    async fn wait_for_code(&self) -> Result<String, String> {
        loop {
            let (stream, _) = self
                .listener
                .accept()
                .await
                .map_err(|err| format!("callback connection failed: {err}"))?;
            match self.handle(stream).await {
                // Browsers also fetch /favicon.ico and similar; keep waiting
                // until a request actually carries the callback parameters.
                Ok(None) => continue,
                Ok(Some(code)) => return Ok(code),
                Err(err) => return Err(err),
            }
        }
    }

    async fn handle(&self, stream: tokio::net::TcpStream) -> Result<Option<String>, String> {
        let mut stream = BufReader::new(stream);
        let mut request_line = String::new();
        stream
            .read_line(&mut request_line)
            .await
            .map_err(|err| format!("reading callback request: {err}"))?;

        let target = request_line.split_whitespace().nth(1).unwrap_or("/");
        let url = Url::parse(&format!("http://localhost{target}"))
            .map_err(|err| format!("unparseable callback request: {err}"))?;
        let params: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();

        if let Some(error) = params.get("error") {
            let detail = params.get("error_description").map(String::as_str).unwrap_or(error);
            respond(&mut stream, "Login failed. You can close this tab.").await;
            return Err(format!("authorization denied: {detail}"));
        }
        let Some(code) = params.get("code") else {
            respond(&mut stream, "Waiting for the authorization redirect...").await;
            return Ok(None);
        };
        // The state check is what stops a different page from feeding us a
        // code; without it the callback is an open redirect target.
        if params.get("state").map(String::as_str) != Some(self.state.as_str()) {
            respond(&mut stream, "Login failed. You can close this tab.").await;
            return Err("callback state did not match; login aborted".into());
        }
        respond(&mut stream, "Signed in. You can close this tab and return to the terminal.").await;
        Ok(Some(code.clone()))
    }
}

async fn respond(stream: &mut BufReader<tokio::net::TcpStream>, message: &str) {
    let body = format!("<!doctype html><meta charset=utf-8><p>{message}</p>");
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.get_mut().write_all(response.as_bytes()).await;
    let _ = stream.get_mut().flush().await;
}

/// RFC 6749 authorization-code exchange with PKCE, form-encoded.
async fn exchange(
    client: &reqwest::Client,
    config: &OauthConfig,
    code: &str,
    redirect_uri: &str,
    verifier: &str,
) -> Result<Tokens, String> {
    let endpoint = format!("{}/oauth/token", config.issuer.trim_end_matches('/'));
    let response = client
        .post(&endpoint)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("client_id", config.client_id.as_str()),
            ("code_verifier", verifier),
        ])
        .send()
        .await
        .map_err(|err| format!("token exchange failed: {err}"))?;

    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!("token exchange rejected ({status}): {}", body.trim()));
    }
    let parsed: TokenResponse = serde_json::from_str(&body)
        .map_err(|err| format!("token response unreadable: {err}"))?;

    let access_token = parsed.access_token.ok_or("token response had no access_token")?;
    let refresh_token = parsed.refresh_token.ok_or(
        "token response had no refresh_token; the login would expire within the hour",
    )?;
    Ok(Tokens {
        account_id: parsed
            .id_token
            .as_deref()
            .and_then(|jwt| account_id(jwt, config.account_id_claim.as_deref())),
        expires_at: jwt_expiry(&access_token),
        id_token: parsed.id_token,
        access_token,
        refresh_token,
    })
}
