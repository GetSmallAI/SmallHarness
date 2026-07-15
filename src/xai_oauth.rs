use anyhow::{anyhow, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::auth::{auth_file_path, AuthStore, OAuthCredential};
use crate::input::plain_read_line;

pub const PROVIDER: &str = "grok";
const CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
const ISSUER: &str = "https://auth.x.ai";
const DISCOVERY_URL: &str = "https://auth.x.ai/.well-known/openid-configuration";
const SCOPE: &str = "openid profile email offline_access grok-cli:access api:access";
const REDIRECT_HOST: &str = "127.0.0.1";
const PREFERRED_REDIRECT_PORT: u16 = 56121;
const REDIRECT_PATH: &str = "/callback";
const GROK_CLI_AUTH_SCOPE_KEY: &str = "https://auth.x.ai::b1a00492-073a-47ea-816f-4c329264a828";
const GROK_CLI_LEGACY_SCOPE_KEY: &str = "https://accounts.x.ai/sign-in";
pub const GROK_MODEL_LIST: &[&str] = &[
    "grok-4.5",
    "grok-4.3",
    "grok-4.20-0309-reasoning",
    "grok-4.20-0309-non-reasoning",
    "grok-4.20-multi-agent-0309",
    "grok-build-0.1",
    "grok-4",
    "grok-3",
    "grok-3-mini",
];

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn random_bytes<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    getrandom::getrandom(&mut out).expect("OS CSPRNG (getrandom) unavailable");
    out
}

fn random_hex(bytes: usize) -> String {
    random_bytes::<32>()[..bytes]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn pkce_pair() -> (String, String) {
    let verifier = URL_SAFE_NO_PAD.encode(random_bytes::<32>());
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    (verifier, challenge)
}

fn percent_encode(input: &str) -> String {
    let mut out = String::new();
    for b in input.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn form_urlencoded(params: &[(&str, &str)]) -> String {
    params
        .iter()
        .map(|(k, v)| format!("{}={}", percent_encode(k), percent_encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

fn percent_decode(input: &str) -> String {
    let mut out = Vec::new();
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(hex) = std::str::from_utf8(&bytes[i + 1..i + 3]) {
                if let Ok(value) = u8::from_str_radix(hex, 16) {
                    out.push(value);
                    i += 3;
                    continue;
                }
            }
        }
        out.push(if bytes[i] == b'+' { b' ' } else { bytes[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn query_param(path: &str, name: &str) -> Option<String> {
    let raw_query = path.split_once('?').map(|(_, q)| q).unwrap_or(path);
    let query = raw_query
        .split_once('#')
        .map(|(q, _)| q)
        .unwrap_or(raw_query);
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        if k == name {
            return Some(percent_decode(v));
        }
    }
    None
}

fn parse_authorization_input(input: &str) -> (Option<String>, Option<String>) {
    let value = input.trim();
    if value.contains("code=") || value.contains("state=") {
        (query_param(value, "code"), query_param(value, "state"))
    } else if let Some((code, state)) = value.split_once('#') {
        (Some(code.to_string()), Some(state.to_string()))
    } else if value.is_empty() {
        (None, None)
    } else {
        (Some(value.to_string()), None)
    }
}

#[derive(Debug, Clone)]
struct Discovery {
    authorization_endpoint: String,
    token_endpoint: String,
    device_authorization_endpoint: String,
}

fn validate_xai_endpoint(url: &str) -> Result<String> {
    let parsed = reqwest::Url::parse(url).context("parsing xAI OAuth endpoint")?;
    if parsed.scheme() != "https" {
        return Err(anyhow!(
            "xAI OAuth discovery returned non-https endpoint: {url}"
        ));
    }
    let host = parsed.host_str().unwrap_or_default().to_ascii_lowercase();
    if host != "x.ai" && !host.ends_with(".x.ai") {
        return Err(anyhow!(
            "xAI OAuth discovery returned unexpected host: {url}"
        ));
    }
    Ok(url.to_string())
}

async fn discover(client: &reqwest::Client) -> Result<Discovery> {
    let resp = client
        .get(DISCOVERY_URL)
        .header("Accept", "application/json")
        .send()
        .await
        .context("fetching xAI OIDC discovery")?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!(
            "xAI OAuth discovery failed ({status}): {}",
            body.trim()
        ));
    }
    let data: Value = resp.json().await?;
    let auth = data
        .get("authorization_endpoint")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("xAI discovery missing authorization_endpoint"))?;
    let token = data
        .get("token_endpoint")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("xAI discovery missing token_endpoint"))?;
    let device = data
        .get("device_authorization_endpoint")
        .and_then(|v| v.as_str())
        .unwrap_or("https://auth.x.ai/oauth2/device/code");
    Ok(Discovery {
        authorization_endpoint: validate_xai_endpoint(auth)?,
        token_endpoint: validate_xai_endpoint(token)?,
        device_authorization_endpoint: validate_xai_endpoint(device)?,
    })
}

fn authorization_url(
    discovery: &Discovery,
    redirect_uri: &str,
    challenge: &str,
    state: &str,
    nonce: &str,
) -> String {
    let params = [
        ("response_type", "code"),
        ("client_id", CLIENT_ID),
        ("redirect_uri", redirect_uri),
        ("scope", SCOPE),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256"),
        ("state", state),
        ("nonce", nonce),
    ];
    format!(
        "{}?{}",
        discovery.authorization_endpoint,
        form_urlencoded(&params)
    )
}

fn write_callback_response(mut stream: TcpStream, ok: bool, message: &str, origin: Option<&str>) {
    let title = if ok {
        "Grok login complete"
    } else {
        "Grok login failed"
    };
    let body = format!(
        "<!doctype html><meta charset=\"utf-8\"><title>{}</title><body style=\"font-family: system-ui; margin: 3rem\"><h1>{}</h1><p>{}</p></body>",
        title, title, message
    );
    let status = if ok { "200 OK" } else { "400 Bad Request" };
    let mut headers = format!(
        "HTTP/1.1 {status}\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n",
        body.len()
    );
    if let Some(origin) = origin {
        headers.push_str(&format!(
            "access-control-allow-origin: {origin}\r\naccess-control-allow-methods: GET, OPTIONS\r\naccess-control-allow-headers: Content-Type\r\naccess-control-allow-private-network: true\r\nvary: Origin\r\n"
        ));
    }
    headers.push_str("\r\n");
    headers.push_str(&body);
    let _ = stream.write_all(headers.as_bytes());
}

fn cors_origin(headers: &str) -> Option<String> {
    for line in headers.lines() {
        let line = line.trim();
        if let Some(rest) = line
            .strip_prefix("Origin:")
            .or_else(|| line.strip_prefix("origin:"))
        {
            let origin = rest.trim();
            if origin == "https://accounts.x.ai" || origin == "https://auth.x.ai" {
                return Some(origin.to_string());
            }
        }
    }
    None
}

fn open_browser(url: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    let mut cmd = std::process::Command::new("open");
    #[cfg(target_os = "linux")]
    let mut cmd = std::process::Command::new("xdg-open");
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", ""]);
        c
    };
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = url;
        return Err(anyhow!("no browser open helper on this platform"));
    }
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    {
        cmd.arg(url);
        cmd.spawn().context("opening browser")?;
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
}

async fn read_token_response(
    resp: reqwest::Response,
    operation: &str,
    fallback_refresh: Option<&str>,
) -> Result<OAuthCredential> {
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!(
            "xAI Grok token {operation} failed ({status}): {}",
            body.trim()
        ));
    }
    let token: TokenResponse = resp.json().await?;
    let refresh = token
        .refresh_token
        .filter(|s| !s.is_empty())
        .or_else(|| fallback_refresh.map(str::to_string))
        .ok_or_else(|| anyhow!("xAI token response missing refresh_token"))?;
    Ok(OAuthCredential {
        credential_type: "oauth".into(),
        access: token.access_token,
        refresh,
        expires: now_secs() + token.expires_in.unwrap_or(3600),
        account_id: None,
    })
}

async fn exchange_authorization_code(
    client: &reqwest::Client,
    token_endpoint: &str,
    code: &str,
    verifier: &str,
    redirect_uri: &str,
) -> Result<OAuthCredential> {
    let body = form_urlencoded(&[
        ("grant_type", "authorization_code"),
        ("client_id", CLIENT_ID),
        ("code", code),
        ("code_verifier", verifier),
        ("redirect_uri", redirect_uri),
    ]);
    let resp = client
        .post(token_endpoint)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .body(body)
        .send()
        .await?;
    read_token_response(resp, "exchange", None).await
}

pub async fn refresh_oauth(
    client: &reqwest::Client,
    refresh: &str,
    token_endpoint: Option<&str>,
) -> Result<OAuthCredential> {
    let endpoint = match token_endpoint {
        Some(url) => validate_xai_endpoint(url)?,
        None => discover(client).await?.token_endpoint,
    };
    let body = form_urlencoded(&[
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh),
        ("client_id", CLIENT_ID),
    ]);
    let resp = client
        .post(endpoint)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .body(body)
        .send()
        .await?;
    read_token_response(resp, "refresh", Some(refresh)).await
}

fn save_oauth(credential: OAuthCredential) -> Result<PathBuf> {
    let mut store = AuthStore::load();
    store.set_oauth(PROVIDER, credential);
    store.save()?;
    auth_file_path().context("no auth file path")
}

fn bind_callback_port() -> Result<(TcpListener, u16)> {
    match TcpListener::bind((REDIRECT_HOST, PREFERRED_REDIRECT_PORT)) {
        Ok(listener) => Ok((listener, PREFERRED_REDIRECT_PORT)),
        Err(_) => {
            let listener = TcpListener::bind((REDIRECT_HOST, 0))
                .context("binding ephemeral OAuth callback port")?;
            let port = listener.local_addr()?.port();
            Ok((listener, port))
        }
    }
}

fn wait_for_browser_callback_with_listener(listener: TcpListener, state: String) -> Result<String> {
    loop {
        let (mut stream, _) = listener.accept().context("waiting for OAuth callback")?;
        let mut buf = [0u8; 8192];
        let n = stream.read(&mut buf).unwrap_or(0);
        let request = String::from_utf8_lossy(&buf[..n]);
        let first = request.lines().next().unwrap_or_default();
        let method = first.split_whitespace().next().unwrap_or_default();
        let path = first.split_whitespace().nth(1).unwrap_or_default();
        let origin = cors_origin(&request);

        if method.eq_ignore_ascii_case("OPTIONS") {
            let mut headers = "HTTP/1.1 204 No Content\r\nconnection: close\r\n".to_string();
            if let Some(ref o) = origin {
                headers.push_str(&format!(
                    "access-control-allow-origin: {o}\r\naccess-control-allow-methods: GET, OPTIONS\r\naccess-control-allow-headers: Content-Type\r\naccess-control-allow-private-network: true\r\nvary: Origin\r\n"
                ));
            }
            headers.push_str("\r\n");
            let _ = stream.write_all(headers.as_bytes());
            continue;
        }

        if !path.starts_with(REDIRECT_PATH) {
            write_callback_response(
                stream,
                false,
                "Callback route not found.",
                origin.as_deref(),
            );
            return Err(anyhow!("unexpected OAuth callback path"));
        }
        if let Some(err) = query_param(path, "error") {
            let desc = query_param(path, "error_description").unwrap_or_default();
            write_callback_response(
                stream,
                false,
                &format!("Authorization failed: {err} {desc}"),
                origin.as_deref(),
            );
            return Err(anyhow!("xAI authorization failed: {err} {desc}"));
        }
        let got_state = query_param(path, "state");
        if got_state.as_deref() != Some(state.as_str()) {
            write_callback_response(stream, false, "State mismatch.", origin.as_deref());
            return Err(anyhow!("OAuth state mismatch"));
        }
        let code =
            query_param(path, "code").ok_or_else(|| anyhow!("OAuth callback missing code"))?;
        write_callback_response(
            stream,
            true,
            "Grok authentication completed. You can close this window.",
            origin.as_deref(),
        );
        return Ok(code);
    }
}

fn parse_expiry_ms(value: &Value) -> Option<u64> {
    if let Some(n) = value.as_u64() {
        return Some(if n > 10_000_000_000 { n / 1000 } else { n });
    }
    if let Some(s) = value.as_str() {
        if let Ok(n) = s.parse::<u64>() {
            return Some(if n > 10_000_000_000 { n / 1000 } else { n });
        }
        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
            return Some(dt.timestamp().max(0) as u64);
        }
    }
    None
}

pub fn load_grok_cli_credentials() -> Option<OAuthCredential> {
    let home = std::env::var_os("HOME")?;
    let path = PathBuf::from(home).join(".grok").join("auth.json");
    let text = std::fs::read_to_string(path).ok()?;
    let data: Value = serde_json::from_str(&text).ok()?;

    let try_entry = |entry: &Value| -> Option<OAuthCredential> {
        let access = entry
            .get("key")
            .or_else(|| entry.get("access_token"))
            .or_else(|| entry.get("token"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())?
            .to_string();
        let refresh = entry
            .get("refresh_token")
            .or_else(|| entry.get("refresh"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let expires = entry
            .get("expires_at")
            .or_else(|| entry.get("expires"))
            .and_then(parse_expiry_ms)
            .unwrap_or_else(|| now_secs() + 3600);
        Some(OAuthCredential {
            credential_type: "oauth".into(),
            access,
            refresh,
            expires,
            account_id: entry
                .get("user_id")
                .or_else(|| entry.get("principal_id"))
                .and_then(|v| v.as_str())
                .map(str::to_string),
        })
    };

    if let Some(entry) = data.get(GROK_CLI_AUTH_SCOPE_KEY) {
        if let Some(cred) = try_entry(entry) {
            return Some(cred);
        }
    }
    if let Some(entry) = data.get(GROK_CLI_LEGACY_SCOPE_KEY) {
        if let Some(cred) = try_entry(entry) {
            return Some(cred);
        }
    }
    try_entry(&data)
}

async fn maybe_import_grok_cli(client: &reqwest::Client) -> Result<Option<OAuthCredential>> {
    let Some(existing) = load_grok_cli_credentials() else {
        return Ok(None);
    };
    println!("  Found existing Grok CLI credentials in ~/.grok/auth.json.");
    let pick = plain_read_line("  Use them instead of a new OAuth login? [Y/n]: ".into()).await?;
    let trimmed = pick.trim().to_lowercase();
    if trimmed.is_empty() || trimmed == "y" || trimmed == "yes" {
        if existing.expires <= now_secs() + 60 {
            if existing.refresh.is_empty() {
                println!("  Stored Grok CLI token is expired and has no refresh token.");
                return Ok(None);
            }
            match refresh_oauth(
                client,
                &existing.refresh,
                Some(&format!("{ISSUER}/oauth2/token")),
            )
            .await
            {
                Ok(refreshed) => return Ok(Some(refreshed)),
                Err(e) => {
                    println!(
                        "  Could not refresh Grok CLI credentials ({e}); starting fresh login."
                    );
                    return Ok(None);
                }
            }
        }
        return Ok(Some(existing));
    }
    Ok(None)
}

pub async fn login_browser(client: &reqwest::Client) -> Result<OAuthCredential> {
    if let Some(imported) = maybe_import_grok_cli(client).await? {
        return Ok(imported);
    }

    let discovery = discover(client).await?;
    let (verifier, challenge) = pkce_pair();
    let state = random_hex(16);
    let nonce = random_hex(16);
    let (listener, port) = bind_callback_port()?;
    let redirect_uri = format!("http://{REDIRECT_HOST}:{port}{REDIRECT_PATH}");
    let url = authorization_url(&discovery, &redirect_uri, &challenge, &state, &nonce);

    println!("  Open this URL to sign in with Grok / SuperGrok:\n\n  {url}\n");
    if let Err(e) = open_browser(&url) {
        println!("  Browser did not open automatically: {e}");
    }
    println!("  Waiting for callback on {redirect_uri} ...");
    println!(
        "  (If the browser cannot reach localhost, cancel and paste the redirect URL or code.)"
    );

    let state_for_thread = state.clone();
    let callback = tokio::task::spawn_blocking(move || {
        wait_for_browser_callback_with_listener(listener, state_for_thread)
    })
    .await
    .context("joining OAuth callback task")?;
    let code = match callback {
        Ok(code) => code,
        Err(e) => {
            println!("  Callback failed: {e}");
            let input = plain_read_line(
                "  Paste the authorization code or full redirect URL (blank to cancel): ".into(),
            )
            .await?;
            let (code, got_state) = parse_authorization_input(&input);
            if let Some(got_state) = got_state {
                if got_state != state {
                    return Err(anyhow!("OAuth state mismatch"));
                }
            }
            code.ok_or_else(|| anyhow!("missing authorization code"))?
        }
    };

    exchange_authorization_code(
        client,
        &discovery.token_endpoint,
        &code,
        &verifier,
        &redirect_uri,
    )
    .await
}

#[derive(Debug, Deserialize)]
struct DeviceStartResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    #[serde(default)]
    verification_uri_complete: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    interval: Option<u64>,
}

pub async fn login_device_code(client: &reqwest::Client) -> Result<OAuthCredential> {
    if let Some(imported) = maybe_import_grok_cli(client).await? {
        return Ok(imported);
    }

    let discovery = discover(client).await?;
    let body = form_urlencoded(&[("client_id", CLIENT_ID), ("scope", SCOPE)]);
    let resp = client
        .post(&discovery.device_authorization_endpoint)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .body(body)
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!(
            "xAI device-code request failed ({status}): {}",
            body.trim()
        ));
    }
    let device: DeviceStartResponse = resp.json().await?;
    let interval = device.interval.unwrap_or(5).max(1);
    let expires_in = device.expires_in.unwrap_or(1800);
    let open_url = device
        .verification_uri_complete
        .as_deref()
        .unwrap_or(device.verification_uri.as_str());

    println!("  Open: {open_url}");
    println!("  Code: {}", device.user_code);
    if let Err(e) = open_browser(open_url) {
        println!("  Browser did not open automatically: {e}");
        println!(
            "  Open {} and enter code {}",
            device.verification_uri, device.user_code
        );
    }
    println!("  Waiting for approval ...");

    let deadline = now_secs() + expires_in;
    let mut sleep_secs = interval;
    loop {
        if now_secs() > deadline {
            return Err(anyhow!("device-code login timed out"));
        }
        tokio::time::sleep(Duration::from_secs(sleep_secs)).await;
        let body = form_urlencoded(&[
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("device_code", &device.device_code),
            ("client_id", CLIENT_ID),
        ]);
        let resp = client
            .post(&discovery.token_endpoint)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header("Accept", "application/json")
            .body(body)
            .send()
            .await?;
        if resp.status().is_success() {
            return read_token_response(resp, "device", None).await;
        }
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        let err = serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
            .unwrap_or_default();
        match err.as_str() {
            "authorization_pending" | "" if status.as_u16() == 400 || status.as_u16() == 403 => {
                continue;
            }
            "slow_down" => {
                sleep_secs = sleep_secs.saturating_add(5);
                continue;
            }
            "expired_token" | "access_denied" => {
                return Err(anyhow!(
                    "xAI device-code login failed ({err}): {}",
                    body.trim()
                ));
            }
            _ => {
                if body.contains("authorization_pending") {
                    continue;
                }
                return Err(anyhow!(
                    "xAI device-code polling failed ({status}): {}",
                    body.trim()
                ));
            }
        }
    }
}

pub async fn login_and_save_browser(client: &reqwest::Client) -> Result<PathBuf> {
    let credential = login_browser(client).await?;
    save_oauth(credential)
}

pub async fn login_and_save_device_code(client: &reqwest::Client) -> Result<PathBuf> {
    let credential = login_device_code(client).await?;
    save_oauth(credential)
}

pub async fn access_token(client: &reqwest::Client) -> Result<String> {
    let store = AuthStore::load();
    let credential = store
        .get_oauth(PROVIDER)
        .cloned()
        .ok_or_else(|| anyhow!("not logged in for grok; run `/login grok`"))?;
    let credential = if credential.expires <= now_secs() + 60 {
        if credential.refresh.is_empty() {
            return Err(anyhow!(
                "Grok OAuth token expired with no refresh token; run `/login grok`"
            ));
        }
        let refreshed = refresh_oauth(
            client,
            &credential.refresh,
            Some(&format!("{ISSUER}/oauth2/token")),
        )
        .await?;
        let mut store = AuthStore::load();
        store.set_oauth(PROVIDER, refreshed.clone());
        store.save()?;
        refreshed
    } else {
        credential
    };
    Ok(credential.access)
}

pub fn grok_model_list() -> Vec<String> {
    GROK_MODEL_LIST.iter().map(|s| (*s).to_string()).collect()
}

pub fn canonical_grok_model(model: &str) -> Option<&str> {
    let trimmed = model.trim();
    if trimmed.is_empty() {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();
    for id in GROK_MODEL_LIST {
        if id.eq_ignore_ascii_case(&lower) {
            return Some(*id);
        }
    }
    if let Some(bare) = lower.rsplit('/').next() {
        for id in GROK_MODEL_LIST {
            if id.eq_ignore_ascii_case(bare) {
                return Some(*id);
            }
        }
    }
    Some(trimmed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorization_url_has_xai_oauth_params() {
        let discovery = Discovery {
            authorization_endpoint: "https://auth.x.ai/oauth2/authorize".into(),
            token_endpoint: "https://auth.x.ai/oauth2/token".into(),
            device_authorization_endpoint: "https://auth.x.ai/oauth2/device/code".into(),
        };
        let url = authorization_url(
            &discovery,
            "http://127.0.0.1:56121/callback",
            "challenge",
            "state",
            "nonce",
        );
        assert!(url.starts_with("https://auth.x.ai/oauth2/authorize?"));
        assert!(url.contains("client_id=b1a00492-073a-47ea-816f-4c329264a828"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("scope="));
        assert!(url.contains("nonce=nonce"));
    }

    #[test]
    fn parses_redirect_url_input() {
        let (code, state) =
            parse_authorization_input("http://127.0.0.1:56121/callback?code=abc%20123&state=st");
        assert_eq!(code.as_deref(), Some("abc 123"));
        assert_eq!(state.as_deref(), Some("st"));
    }

    #[test]
    fn validates_xai_hosts_only() {
        assert!(validate_xai_endpoint("https://auth.x.ai/oauth2/token").is_ok());
        assert!(validate_xai_endpoint("https://evil.example/oauth2/token").is_err());
        assert!(validate_xai_endpoint("http://auth.x.ai/oauth2/token").is_err());
    }

    #[test]
    fn canonical_model_accepts_aliases() {
        assert_eq!(canonical_grok_model("grok-4.5"), Some("grok-4.5"));
        assert_eq!(canonical_grok_model("xai/grok-4.5"), Some("grok-4.5"));
        assert_eq!(canonical_grok_model("future-model"), Some("future-model"));
        assert_eq!(canonical_grok_model(""), None);
    }

    #[test]
    fn parse_expiry_accepts_rfc3339() {
        let v = Value::String("2026-07-15T18:03:10.175Z".into());
        let secs = parse_expiry_ms(&v).unwrap();
        assert!(secs > 1_700_000_000);
    }
}
