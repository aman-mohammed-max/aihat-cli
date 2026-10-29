//! Browser sign-in (OAuth 2.1 with PKCE) for MCP servers, the way the MCP
//! specification describes it and the AI Hat app does it:
//!
//! 1. Ask the server without credentials. A 401 answer names where its
//!    sign-in is described (RFC 9728).
//! 2. Read the sign-in service's addresses (RFC 8414 / OpenID discovery).
//! 3. Register as a client on the fly (RFC 7591), so there is nothing to set
//!    up by hand.
//! 4. Open the browser at the authorization address with a PKCE challenge and
//!    wait for it to come back to a one-off address on 127.0.0.1.
//! 5. Trade the code for tokens, keep them with the other secrets, and
//!    refresh them when they run out.
//!
//! The tokens and this computer's registration are kept with the secrets
//! (not in the shared database), so the app's own sign-in is left alone.

use crate::api::SendRetry;
use crate::secrets::Secrets;
use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// The port the browser comes back to. A fixed one lets a later sign-in
/// reuse the registration; another is picked when it is taken.
const CALLBACK_PORT: u16 = 33418;
const CALLBACK_PATH: &str = "/oauth/mcp";
const WAIT: Duration = Duration::from_secs(300);

#[derive(Clone, Debug)]
pub struct Discovery {
    /// The canonical address of the MCP server, sent as `resource`.
    pub resource: String,
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub registration_endpoint: Option<String>,
    pub scope: Option<String>,
    pub auth_methods: Vec<String>,
}

/// What a server needs before it can be used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Need {
    Nothing,
    /// A browser sign-in.
    SignIn,
    /// An access token to paste.
    Token,
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Meta {
    client_id: String,
    /// none, post or basic: how the client proves itself to the token address.
    method: String,
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    resource: String,
    redirect: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scope: Option<String>,
    /// When the access token stops working (ms since 1970), if the service said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expires_at: Option<i64>,
}

fn key(id: &str, part: &str) -> String {
    format!("mcp.{id}.oauth.{part}")
}

fn http(timeout_ms: u64) -> Result<reqwest::blocking::Client> {
    // Sign-in services redirect between their own pages; never follow a
    // redirect away with a token, so redirects are off.
    crate::api::tls_ready();
    Ok(reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(timeout_ms))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("AI-Hat-CLI/", env!("CARGO_PKG_VERSION")))
        .build()?)
}

fn b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn random_bytes(count: usize) -> Vec<u8> {
    // uuid v4 is drawn from the operating system's random source.
    let mut out = Vec::with_capacity(count);
    while out.len() < count {
        out.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    }
    out.truncate(count);
    out
}

// ------------------------------------------------------------------ discovery

/// `Bearer resource_metadata="https://…", scope="read write"` → the parts used.
pub fn parse_challenge(header: &str) -> (Option<String>, Option<String>) {
    let get = |name: &str| {
        let lower = header.to_ascii_lowercase();
        let at = lower.find(&format!("{name}=\""))? + name.len() + 2;
        let end = header[at..].find('"')? + at;
        Some(header[at..end].to_string())
    };
    (get("resource_metadata"), get("scope"))
}

fn is_local_host(host: &str) -> bool {
    host == "localhost"
        || host.starts_with("127.")
        || host.starts_with("10.")
        || host.starts_with("192.168.")
        || host == "[::1]"
        || host == "::1"
        || host.ends_with(".local")
        || host.strip_prefix("172.").and_then(|rest| rest.split('.').next()?.parse::<u8>().ok()).is_some_and(|n| (16..=31).contains(&n))
}

/// Sign-in addresses must use https; a server on your own network over plain
/// http may use http for its own sign-in too.
fn require_safe(address: &str, mcp_url: &str, what: &str) -> Result<()> {
    let url = reqwest::Url::parse(address).with_context(|| format!("the {what} is not a valid address"))?;
    let mcp = reqwest::Url::parse(mcp_url)?;
    if url.scheme() == "https" || (url.scheme() == "http" && mcp.scheme() == "http" && is_local_host(url.host_str().unwrap_or(""))) {
        return Ok(());
    }
    bail!("The {what} isn't on https, so it won't be used.")
}

fn get_json(url: &str) -> Option<Value> {
    let response = http(8_000).ok()?.get(url).header("Accept", "application/json").send_retry().ok()?;
    if !response.status().is_success() {
        return None;
    }
    response.json().ok()
}

const INIT: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"aihat","version":"1"}}}"#;

fn knock(mcp_url: &str) -> Result<reqwest::blocking::Response> {
    http(10_000)?
        .post(mcp_url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .body(INIT)
        .send_retry()
        .map_err(|_| anyhow!("Couldn't reach the server. Check the address and your connection."))
}

fn metadata_urls(issuer: &str) -> Vec<String> {
    let Ok(url) = reqwest::Url::parse(issuer) else {
        return Vec::new();
    };
    let origin = url.origin().ascii_serialization();
    let path = url.path().trim_end_matches('/');
    if path.is_empty() {
        vec![
            format!("{origin}/.well-known/oauth-authorization-server"),
            format!("{origin}/.well-known/openid-configuration"),
        ]
    } else {
        vec![
            format!("{origin}/.well-known/oauth-authorization-server{path}"),
            format!("{origin}/.well-known/openid-configuration{path}"),
            format!("{}/.well-known/openid-configuration", issuer.trim_end_matches('/')),
        ]
    }
}

/// How a server wants you to sign in.
pub fn discover(mcp_url: &str) -> Result<Discovery> {
    let response = knock(mcp_url)?;
    if response.status().as_u16() != 401 {
        if response.status().is_success() {
            bail!("This server doesn't ask for a sign-in.");
        }
        bail!("The server answered {}.", response.status().as_u16());
    }
    let challenge = response
        .headers()
        .get("www-authenticate")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string();
    let (resource_metadata, challenge_scope) = parse_challenge(&challenge);
    let url = reqwest::Url::parse(mcp_url)?;
    let origin = url.origin().ascii_serialization();
    let path = url.path().trim_end_matches('/').to_string();

    let mut candidates = Vec::new();
    candidates.extend(resource_metadata);
    if !path.is_empty() {
        candidates.push(format!("{origin}/.well-known/oauth-protected-resource{path}"));
    }
    candidates.push(format!("{origin}/.well-known/oauth-protected-resource"));
    let mut protected = None;
    for candidate in candidates {
        require_safe(&candidate, mcp_url, "sign-in description")?;
        if let Some(found) = get_json(&candidate) {
            protected = Some(found);
            break;
        }
    }

    let issuer = protected
        .as_ref()
        .and_then(|p| p.pointer("/authorization_servers/0"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| origin.clone());
    require_safe(&issuer, mcp_url, "sign-in service")?;
    let mut meta = metadata_urls(&issuer)
        .into_iter()
        .filter_map(|candidate| get_json(&candidate))
        .find(|m| m.get("authorization_endpoint").is_some() && m.get("token_endpoint").is_some());
    // Servers from before the discovery documents used fixed paths.
    if meta.is_none() && protected.is_none() {
        meta = Some(json!({
            "authorization_endpoint": format!("{origin}/authorize"),
            "token_endpoint": format!("{origin}/token"),
            "registration_endpoint": format!("{origin}/register"),
        }));
    }
    let meta = meta.ok_or_else(|| anyhow!("This server's sign-in service could not be found."))?;
    if let Some(methods) = meta.get("code_challenge_methods_supported").and_then(Value::as_array)
        && !methods.iter().any(|m| m == "S256")
    {
        bail!("This service doesn't support the secure sign-in (PKCE) aihat uses.");
    }
    let field = |name: &str| meta.get(name).and_then(Value::as_str).map(str::to_owned);
    let authorization_endpoint = field("authorization_endpoint").unwrap_or_default();
    let token_endpoint = field("token_endpoint").unwrap_or_default();
    let registration_endpoint = field("registration_endpoint");
    require_safe(&authorization_endpoint, mcp_url, "sign-in page")?;
    require_safe(&token_endpoint, mcp_url, "token address")?;
    if let Some(registration) = &registration_endpoint {
        require_safe(registration, mcp_url, "registration address")?;
    }

    let listed: Option<String> = protected
        .as_ref()
        .and_then(|p| p.get("scopes_supported"))
        .and_then(Value::as_array)
        .map(|scopes| scopes.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" "))
        .filter(|scopes| !scopes.is_empty());
    let mut scope = challenge_scope.or(listed);
    // Refresh tokens are only handed out on request by services that list this scope.
    let offline = meta
        .get("scopes_supported")
        .and_then(Value::as_array)
        .is_some_and(|scopes| scopes.iter().any(|s| s == "offline_access"));
    if let Some(scope) = scope.as_mut()
        && offline
        && !scope.split(' ').any(|s| s == "offline_access")
    {
        scope.push_str(" offline_access");
    }
    Ok(Discovery {
        resource: protected
            .as_ref()
            .and_then(|p| p.get("resource"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| mcp_url.to_string()),
        issuer,
        authorization_endpoint,
        token_endpoint,
        registration_endpoint,
        scope,
        auth_methods: meta
            .get("token_endpoint_auth_methods_supported")
            .and_then(Value::as_array)
            .map(|m| m.iter().filter_map(Value::as_str).map(str::to_owned).collect())
            .unwrap_or_default(),
    })
}

/// What a server needs: nothing, a browser sign-in, or a token you paste.
pub fn probe(mcp_url: &str) -> Need {
    let Ok(response) = knock(mcp_url) else {
        return Need::Unknown;
    };
    if response.status().is_success() {
        return Need::Nothing;
    }
    if response.status().as_u16() != 401 {
        return Need::Unknown;
    }
    match discover(mcp_url) {
        Ok(found) if found.registration_endpoint.is_some() => Need::SignIn,
        _ => Need::Token,
    }
}

// ------------------------------------------------------------------ stored state

fn read_meta(secrets: &Secrets, id: &str) -> Option<Meta> {
    serde_json::from_str(&secrets.get(&key(id, "meta"))?).ok()
}

fn write_meta(secrets: &Secrets, id: &str, meta: &Meta) -> Result<()> {
    secrets.set(&key(id, "meta"), &serde_json::to_string(meta)?)?;
    Ok(())
}

pub fn is_signed_in(secrets: &Secrets, id: &str) -> bool {
    read_meta(secrets, id).is_some() && secrets.get(&key(id, "access")).is_some()
}

/// Sign out: the tokens go; the registration stays so signing in again
/// doesn't register again.
pub fn sign_out(secrets: &Secrets, id: &str) {
    for part in ["access", "refresh"] {
        let _ = secrets.delete(&key(id, part));
    }
}

/// Forget everything about a server's sign-in (when it is removed).
pub fn forget(secrets: &Secrets, id: &str) {
    for part in ["access", "refresh", "secret", "meta"] {
        let _ = secrets.delete(&key(id, part));
    }
}

// ------------------------------------------------------------------ tokens

#[derive(Deserialize, Default)]
struct TokenReply {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
    scope: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

/// The refresh was refused for good (as opposed to a network problem).
#[derive(Debug)]
struct Expired;

impl std::fmt::Display for Expired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the sign-in has expired")
    }
}

impl std::error::Error for Expired {}

fn token_request(meta: &Meta, secret: Option<&str>, form: &[(&str, &str)]) -> Result<TokenReply> {
    let mut fields: Vec<(&str, &str)> = form.to_vec();
    fields.push(("client_id", &meta.client_id));
    if let (Some(secret), "post") = (secret, meta.method.as_str()) {
        fields.push(("client_secret", secret));
    }
    let body = fields
        .iter()
        .map(|(k, v)| format!("{}={}", urlencoding::encode(k), urlencoding::encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    let mut request = http(20_000)?
        .post(&meta.token_endpoint)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .body(body);
    if let (Some(secret), "basic") = (secret, meta.method.as_str()) {
        let pair = format!("{}:{}", urlencoding::encode(&meta.client_id), urlencoding::encode(secret));
        request = request.header("Authorization", format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(pair)));
    }
    let response = request.send_retry().context("couldn't reach the sign-in service")?;
    let status = response.status();
    let reply: TokenReply = response.json().unwrap_or_default();
    if !status.is_success() || reply.access_token.is_none() {
        let message = reply
            .error_description
            .clone()
            .or(reply.error.clone())
            .unwrap_or_else(|| format!("The sign-in service answered {}.", status.as_u16()));
        if reply.error.as_deref() == Some("invalid_grant") {
            return Err(anyhow::Error::new(Expired).context(message));
        }
        bail!(message);
    }
    Ok(reply)
}

fn keep(secrets: &Secrets, id: &str, meta: &Meta, reply: &TokenReply) -> Result<()> {
    secrets.set(&key(id, "access"), reply.access_token.as_deref().unwrap_or_default())?;
    if let Some(refresh) = &reply.refresh_token {
        secrets.set(&key(id, "refresh"), refresh)?;
    }
    let mut meta = meta.clone();
    if reply.scope.is_some() {
        meta.scope = reply.scope.clone();
    }
    meta.expires_at = reply.expires_in.map(|s| chrono::Utc::now().timestamp_millis() + s * 1000);
    write_meta(secrets, id, &meta)
}

/// A working access token, refreshed when it has run out (or when `force`),
/// or `None` when you need to sign in again.
pub fn access_token(secrets: &Secrets, id: &str, force: bool) -> Option<String> {
    let meta = read_meta(secrets, id)?;
    let access = secrets.get(&key(id, "access"))?;
    let now = chrono::Utc::now().timestamp_millis();
    let stale = force || meta.expires_at.is_some_and(|at| now > at - 60_000);
    if !stale {
        return Some(access);
    }
    let refresh = secrets.get(&key(id, "refresh"))?;
    let secret = secrets.get(&key(id, "secret"));
    match token_request(
        &meta,
        secret.as_deref(),
        &[("grant_type", "refresh_token"), ("refresh_token", &refresh), ("resource", &meta.resource)],
    ) {
        Ok(reply) => {
            let _ = keep(secrets, id, &meta, &reply);
            reply.access_token
        }
        Err(error) => {
            // A refused refresh ends the sign-in; a network hiccup keeps it.
            if error.downcast_ref::<Expired>().is_some() {
                sign_out(secrets, id);
            }
            None
        }
    }
}

// ------------------------------------------------------------------ sign in

/// What the sign-in is doing, for the caller to show.
pub enum Step<'a> {
    /// The browser was asked to open this address.
    Opened(&'a str),
}

fn register(found: &Discovery, redirect: &str) -> Result<(String, Option<String>, String)> {
    let Some(endpoint) = &found.registration_endpoint else {
        bail!("This service doesn't let apps sign themselves up, so browser sign-in isn't possible here. Use an access token instead.");
    };
    let methods = &found.auth_methods;
    let method = if methods.is_empty() || methods.iter().any(|m| m == "none") {
        "none"
    } else if methods.iter().any(|m| m == "client_secret_post") {
        "client_secret_post"
    } else {
        "client_secret_basic"
    };
    let response = http(15_000)?
        .post(endpoint)
        .header("Accept", "application/json")
        .json(&json!({
            "client_name": "AI Hat (terminal)",
            "redirect_uris": [redirect],
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "token_endpoint_auth_method": method,
        }))
        .send_retry()
        .context("couldn't reach the sign-in service")?;
    let status = response.status();
    let data: Value = response.json().unwrap_or(Value::Null);
    let Some(client_id) = data.get("client_id").map(|v| v.as_str().map(str::to_owned).unwrap_or_else(|| v.to_string())) else {
        let reason = data
            .get("error_description")
            .or_else(|| data.get("error"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| format!("The service refused the registration ({}).", status.as_u16()));
        bail!(reason);
    };
    let given = data.get("token_endpoint_auth_method").and_then(Value::as_str).unwrap_or(method);
    let short = match given {
        "client_secret_post" => "post",
        "client_secret_basic" => "basic",
        _ => "none",
    };
    Ok((client_id, data.get("client_secret").and_then(Value::as_str).map(str::to_owned), short.to_string()))
}

/// Whether this computer can show a web page: a desktop session, or a
/// browser named in $BROWSER. Over SSH or on a server there is none.
pub fn has_browser() -> bool {
    if std::env::var_os("BROWSER").is_some_and(|b| !b.is_empty()) {
        return true;
    }
    if cfg!(any(target_os = "macos", target_os = "windows")) {
        return true;
    }
    let display = ["DISPLAY", "WAYLAND_DISPLAY"].iter().any(|name| std::env::var_os(name).is_some_and(|v| !v.is_empty()));
    display || std::path::Path::new("/opt/google/cros-containers").exists()
}

fn open_browser(url: &str) -> Result<()> {
    if !has_browser() {
        bail!(NO_BROWSER);
    }
    webbrowser::open(url).map_err(|_| anyhow!(NO_BROWSER))
}

pub const NO_BROWSER: &str = "No web browser was found on this computer, so the sign-in page can't open. Sign in from a computer with a browser, or use an access token instead.";

/// Run the whole browser sign-in for a server and keep the tokens. `cancel`
/// stops the wait for the browser.
pub fn sign_in(secrets: &Secrets, id: &str, mcp_url: &str, cancel: &AtomicBool, on_step: &mut dyn FnMut(Step<'_>)) -> Result<()> {
    if !has_browser() {
        bail!(NO_BROWSER);
    }
    let found = discover(mcp_url)?;
    let listener = TcpListener::bind(("127.0.0.1", CALLBACK_PORT))
        .or_else(|_| TcpListener::bind(("127.0.0.1", 0)))
        .context("couldn't open a local address for the browser to come back to")?;
    let port = listener.local_addr()?.port();
    let redirect = format!("http://127.0.0.1:{port}{CALLBACK_PATH}");

    // Reuse the registration from an earlier sign-in to the same service.
    let previous = read_meta(secrets, id);
    let (client_id, client_secret, method) = match previous {
        Some(prev) if prev.issuer == found.issuer && prev.redirect == redirect => {
            (prev.client_id, secrets.get(&key(id, "secret")), prev.method)
        }
        _ => {
            let (client_id, secret, method) = register(&found, &redirect)?;
            match &secret {
                Some(secret) => {
                    secrets.set(&key(id, "secret"), secret)?;
                }
                None => {
                    let _ = secrets.delete(&key(id, "secret"));
                }
            }
            (client_id, secret, method)
        }
    };

    let verifier = b64url(&random_bytes(32));
    let challenge = b64url(&Sha256::digest(verifier.as_bytes()));
    let state = b64url(&random_bytes(16));
    let mut params = vec![
        ("response_type", "code".to_string()),
        ("client_id", client_id.clone()),
        ("redirect_uri", redirect.clone()),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256".to_string()),
        ("state", state.clone()),
        ("resource", found.resource.clone()),
    ];
    if let Some(scope) = &found.scope {
        params.push(("scope", scope.clone()));
    }
    let query = params
        .iter()
        .map(|(k, v)| format!("{k}={}", urlencoding::encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    let joiner = if found.authorization_endpoint.contains('?') { '&' } else { '?' };
    let auth_url = format!("{}{joiner}{query}", found.authorization_endpoint);

    open_browser(&auth_url)?;
    on_step(Step::Opened(&auth_url));

    let returned = wait_for_browser(&listener, cancel)?;
    let get = |name: &str| returned.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());
    if let Some(error) = get("error") {
        bail!(get("error_description").unwrap_or_else(|| format!("The service said: {error}.")));
    }
    if get("state").as_deref() != Some(state.as_str()) {
        bail!("The sign-in answer didn't match the request, so it was ignored.");
    }
    let code = get("code").ok_or_else(|| anyhow!("The sign-in finished without a code."))?;

    let meta = Meta {
        client_id,
        method,
        issuer: found.issuer.clone(),
        authorization_endpoint: found.authorization_endpoint.clone(),
        token_endpoint: found.token_endpoint.clone(),
        resource: found.resource.clone(),
        redirect: redirect.clone(),
        scope: found.scope.clone(),
        expires_at: None,
    };
    let reply = token_request(
        &meta,
        client_secret.as_deref(),
        &[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("redirect_uri", &redirect),
            ("code_verifier", &verifier),
            ("resource", &found.resource),
        ],
    )?;
    keep(secrets, id, &meta, &reply)
}

/// Wait for the browser to come back to the local address; returns the
/// query parameters it brought.
fn wait_for_browser(listener: &TcpListener, cancel: &AtomicBool) -> Result<Vec<(String, String)>> {
    listener.set_nonblocking(true)?;
    let started = Instant::now();
    loop {
        if cancel.load(Ordering::Relaxed) {
            bail!("Sign-in was cancelled.");
        }
        if started.elapsed() > WAIT {
            bail!("The browser didn't come back within 5 minutes, so the sign-in was stopped.");
        }
        match listener.accept() {
            Ok((stream, _)) => {
                if let Some(params) = answer_browser(stream)? {
                    return Ok(params);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(100)),
            Err(error) => return Err(error.into()),
        }
    }
}

/// Read one request from the browser. Returns its parameters when it is the
/// sign-in coming back, `None` for anything else (like a favicon).
fn answer_browser(mut stream: TcpStream) -> Result<Option<Vec<(String, String)>>> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line)?;
    let target = line.split_whitespace().nth(1).unwrap_or("");
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if path != CALLBACK_PATH {
        let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        return Ok(None);
    }
    let params: Vec<(String, String)> = query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            let decode = |s: &str| urlencoding::decode(&s.replace('+', " ")).map(|c| c.into_owned()).unwrap_or_default();
            (decode(k), decode(v))
        })
        .collect();
    let failed = params.iter().any(|(k, _)| k == "error");
    let (title, body) = if failed {
        ("Sign-in didn't finish", "Go back to the terminal to see why.")
    } else {
        ("You're signed in", "You can close this tab and go back to the terminal.")
    };
    let page = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>AI Hat · {title}</title></head>\
         <body style=\"margin:0;height:100vh;display:flex;align-items:center;justify-content:center;\
         background:#0B0C0E;color:#D4D7DC;font:16px ui-monospace,Menlo,monospace\">\
         <div><div style=\"color:#C6F46B;font-weight:700\">◆ AI Hat</div>\
         <h1 style=\"font-size:20px;color:#F4F5F7;margin:12px 0 6px\">{title}</h1>\
         <div style=\"color:#7A808A\">{body}</div></div></body></html>"
    );
    let _ = write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}",
        page.len()
    );
    Ok(Some(params))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_challenge_header() {
        let (metadata, scope) = parse_challenge(r#"Bearer realm="OAuth", resource_metadata="https://x.test/.well-known/oauth-protected-resource/mcp", scope="read write""#);
        assert_eq!(metadata.as_deref(), Some("https://x.test/.well-known/oauth-protected-resource/mcp"));
        assert_eq!(scope.as_deref(), Some("read write"));
        assert_eq!(parse_challenge("Bearer").0, None);
    }

    #[test]
    fn only_https_or_local_http_is_trusted() {
        assert!(require_safe("https://auth.test/authorize", "https://mcp.test/mcp", "x").is_ok());
        assert!(require_safe("http://auth.test/authorize", "https://mcp.test/mcp", "x").is_err());
        assert!(require_safe("http://127.0.0.1:9000/authorize", "http://127.0.0.1:9000/mcp", "x").is_ok());
        assert!(require_safe("http://evil.test/authorize", "http://127.0.0.1:9000/mcp", "x").is_err());
    }

    #[test]
    fn pkce_challenge_is_the_rfc_example() {
        // RFC 7636 appendix B.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(b64url(&Sha256::digest(verifier.as_bytes())), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
        assert_eq!(b64url(&random_bytes(32)).len(), 43);
    }
}
