use crate::api;
use crate::db::Database;
use crate::models::{McpServer, McpTool};
use crate::secrets::Secrets;
use anyhow::{Context, Result};
use serde_json::{Value, json};

#[derive(Clone, Debug)]
pub struct DiscoveredTool {
    pub server: McpServer,
    pub tool: McpTool,
    pub function_name: String,
    pub definition: Value,
}

/// Why a server can't be used until someone signs in.
#[derive(Debug)]
pub struct NeedsSignIn {
    pub server: String,
    /// A browser sign-in (rather than a pasted token) would fix it.
    pub browser: bool,
}

impl std::fmt::Display for NeedsSignIn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.browser {
            write!(f, "{} needs you to sign in (aihat mcp login {}, or s in /mcp)", self.server, self.server)
        } else {
            write!(f, "{} needs an access token (aihat mcp set-key {}, or k in /mcp)", self.server, self.server)
        }
    }
}

impl std::error::Error for NeedsSignIn {}

pub fn needs_sign_in(error: &anyhow::Error) -> Option<&NeedsSignIn> {
    error.chain().find_map(|cause| cause.downcast_ref::<NeedsSignIn>())
}

fn rejected(error: &anyhow::Error) -> bool {
    api::api_error(error).is_some_and(|e| e.kind == api::ErrorKind::Auth)
}

/// One request to a server, whatever it runs on. Signed-in servers get a
/// fresh token when theirs has run out, and one more try when the server
/// turns the token down.
fn rpc(server: &McpServer, secrets: &Secrets, method: &str, params: Value, id: u64) -> Result<Value> {
    if server.transport == "stdio" {
        return crate::mcp_stdio::request(server, method, params);
    }
    if server.auth != "oauth" {
        let token = secrets.mcp_token(&server.id);
        return api::mcp_rpc(server, token.as_deref(), method, params, id).map_err(|error| {
            if rejected(&error) {
                anyhow::Error::new(NeedsSignIn { server: server.name.clone(), browser: false })
            } else {
                error
            }
        });
    }
    let signed_out = || anyhow::Error::new(NeedsSignIn { server: server.name.clone(), browser: true });
    let token = crate::oauth::access_token(secrets, &server.id, false).ok_or_else(signed_out)?;
    match api::mcp_rpc(server, Some(&token), method, params.clone(), id) {
        Err(error) if rejected(&error) => {
            let fresh = crate::oauth::access_token(secrets, &server.id, true).ok_or_else(signed_out)?;
            api::mcp_rpc(server, Some(&fresh), method, params, id).map_err(|error| if rejected(&error) { signed_out() } else { error })
        }
        other => other,
    }
}

fn notify(server: &McpServer, secrets: &Secrets, method: &str, params: Value) -> Result<()> {
    if server.transport == "stdio" {
        return crate::mcp_stdio::notify(server, method, params);
    }
    let token = if server.auth == "oauth" {
        crate::oauth::access_token(secrets, &server.id, false)
    } else {
        secrets.mcp_token(&server.id)
    };
    api::mcp_notify(server, token.as_deref(), method, params)
}

pub fn connect(server: &McpServer, secrets: &Secrets) -> Result<Vec<McpTool>> {
    rpc(
        server,
        secrets,
        "initialize",
        json!({
            "protocolVersion": "2025-03-26",
            "capabilities": {},
            "clientInfo": {"name": "aihat", "version": env!("CARGO_PKG_VERSION")}
        }),
        1,
    )?;
    let _ = notify(server, secrets, "notifications/initialized", json!({}));
    let mut found = Vec::new();
    let mut cursor: Option<String> = None;
    for page in 0..10u64 {
        let params = cursor
            .as_ref()
            .map(|c| json!({"cursor": c}))
            .unwrap_or_else(|| json!({}));
        let result = rpc(server, secrets, "tools/list", params, 2 + page)?;
        if let Some(tools) = result.get("tools").and_then(Value::as_array) {
            for raw in tools {
                let Some(name) = raw.get("name").and_then(Value::as_str) else {
                    continue;
                };
                let kind = classify_tool(name, raw.get("annotations"));
                let policy = if kind == "read" { "auto" } else { "ask" };
                found.push(McpTool {
                    name: name.to_string(),
                    description: raw
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    kind: kind.to_string(),
                    policy: policy.to_string(),
                    schema: raw
                        .get("inputSchema")
                        .cloned()
                        .unwrap_or_else(|| json!({"type":"object","properties":{}})),
                });
            }
        }
        cursor = result
            .get("nextCursor")
            .and_then(Value::as_str)
            .map(str::to_string);
        if cursor.is_none() {
            break;
        }
    }
    preserve_policies(&mut found, &server.tools);
    Ok(found)
}

/// Connect a server and load its tools, signing in first when it needs a
/// browser sign-in. `on_opened` hears the sign-in address once the browser
/// was asked to open it; `cancel` stops the wait for the browser.
pub fn connect_server(
    db: &Database,
    secrets: &Secrets,
    id: &str,
    cancel: &std::sync::atomic::AtomicBool,
    on_opened: &mut dyn FnMut(&str),
) -> Result<usize> {
    let server = db
        .mcp_servers()?
        .into_iter()
        .find(|s| s.id == id)
        .with_context(|| format!("no MCP server {id}"))?;
    let mut step = |step: crate::oauth::Step<'_>| match step {
        crate::oauth::Step::Opened(url) => on_opened(url),
    };
    if server.transport == "stdio" {
        return refresh(db, secrets, id);
    }
    if server.auth == "oauth" {
        if !crate::oauth::is_signed_in(secrets, id) {
            crate::oauth::sign_in(secrets, id, &server.url, cancel, &mut step)?;
        }
        return refresh(db, secrets, id);
    }
    match refresh(db, secrets, id) {
        // No token yet and the server turns us away: if it offers a browser
        // sign-in, use that.
        Err(error)
            if needs_sign_in(&error).is_some()
                && secrets.mcp_token(id).is_none()
                && crate::oauth::probe(&server.url) == crate::oauth::Need::SignIn =>
        {
            db.set_mcp_auth(id, "oauth")?;
            crate::oauth::sign_in(secrets, id, &server.url, cancel, &mut step)?;
            refresh(db, secrets, id)
        }
        other => other,
    }
}

/// Forget a server's sign-in and stop its program.
pub fn forget(secrets: &Secrets, id: &str) {
    let _ = secrets.delete(&format!("mcp.{id}"));
    crate::oauth::forget(secrets, id);
    crate::mcp_stdio::stop(Some(id));
}

pub fn refresh(db: &Database, secrets: &Secrets, id_or_name: &str) -> Result<usize> {
    let server = db
        .mcp_servers()?
        .into_iter()
        .find(|s| s.id == id_or_name || s.name.eq_ignore_ascii_case(id_or_name))
        .with_context(|| format!("no MCP server named {id_or_name:?}"))?;
    let mut tools = connect(&server, secrets)?;
    if server_always_allowed(db, &server.id) {
        for tool in &mut tools {
            tool.policy = "allow".into();
        }
    }
    let value = serde_json::to_value(&tools)?;
    db.update_mcp_tools(&server.id, &value)?;
    Ok(tools.len())
}

/// Connect, and when the server turns us away but offers a browser sign-in,
/// sign in (telling `on_sign_in` the server and address) and try again.
fn connect_signing_in(
    db: &Database,
    secrets: &Secrets,
    server: &mut McpServer,
    cancel: &std::sync::atomic::AtomicBool,
    on_sign_in: &mut dyn FnMut(&str, &str),
) -> Result<Vec<McpTool>> {
    match connect(server, secrets) {
        Err(error) if can_sign_in(server, secrets, &error) => {
            sign_in_now(db, secrets, server, cancel, on_sign_in)?;
            connect(server, secrets)
        }
        other => other,
    }
}

/// The error means "sign in", and a browser sign-in is how this server
/// does it (asked once, when nothing else was set up).
fn can_sign_in(server: &McpServer, secrets: &Secrets, error: &anyhow::Error) -> bool {
    match needs_sign_in(error) {
        Some(need) if need.browser => true,
        Some(_) => secrets.mcp_token(&server.id).is_none() && crate::oauth::probe(&server.url) == crate::oauth::Need::SignIn,
        None => false,
    }
}

fn sign_in_now(
    db: &Database,
    secrets: &Secrets,
    server: &mut McpServer,
    cancel: &std::sync::atomic::AtomicBool,
    on_sign_in: &mut dyn FnMut(&str, &str),
) -> Result<()> {
    if server.auth != "oauth" {
        db.set_mcp_auth(&server.id, "oauth")?;
        server.auth = "oauth".into();
    }
    let name = server.name.clone();
    crate::oauth::sign_in(secrets, &server.id, &server.url, cancel, &mut |step| match step {
        crate::oauth::Step::Opened(url) => on_sign_in(&name, url),
    })
}

/// Run a tool; if the sign-in ran out on the way, sign in again and run it once more.
pub fn call_signing_in(
    db: &Database,
    secrets: &Secrets,
    server: &McpServer,
    name: &str,
    args: Value,
    cancel: &std::sync::atomic::AtomicBool,
    on_sign_in: &mut dyn FnMut(&str, &str),
) -> Result<String> {
    match call(server, secrets, name, args.clone()) {
        Err(error) if can_sign_in(server, secrets, &error) => {
            let mut server = server.clone();
            sign_in_now(db, secrets, &mut server, cancel, on_sign_in)?;
            call(&server, secrets, name, args)
        }
        other => other,
    }
}

/// The tools of the servers a chat uses. Servers that need a browser
/// sign-in get one on the spot (`on_sign_in` hears about it; `cancel`
/// stops the wait).
pub fn discover(
    db: &Database,
    secrets: &Secrets,
    enabled_ids: &[String],
    cancel: &std::sync::atomic::AtomicBool,
    on_sign_in: &mut dyn FnMut(&str, &str),
) -> (Vec<DiscoveredTool>, Vec<String>) {
    let servers = match db.mcp_servers() {
        Ok(servers) => servers,
        Err(error) => {
            return (
                Vec::new(),
                vec![format!("Could not load MCP servers: {error}")],
            );
        }
    };
    let mut out = Vec::new();
    let mut warnings = Vec::new();
    for mut server in servers
        .into_iter()
        .filter(|s| enabled_ids.iter().any(|id| id == &s.id))
    {
        let signed_out = server.auth == "oauth" && !crate::oauth::is_signed_in(secrets, &server.id);
        if server.tools.is_empty() || !server.connected || signed_out {
            match connect_signing_in(db, secrets, &mut server, cancel, on_sign_in) {
                Ok(tools) => {
                    server.tools = tools;
                    if let Err(error) = db.update_mcp_tools(
                        &server.id,
                        &serde_json::to_value(&server.tools).unwrap_or(Value::Null),
                    ) {
                        warnings.push(format!(
                            "{} connected, but its tools could not be cached: {error}",
                            server.name
                        ));
                    }
                }
                Err(error) => {
                    warnings.push(format!(
                        "MCP server {} is unavailable: {error}",
                        server.name
                    ));
                    let _ = db.mark_mcp_disconnected(&server.id);
                    server.tools.clear();
                }
            }
        }
        if server_always_allowed(db, &server.id) {
            for tool in &mut server.tools {
                tool.policy = "allow".into();
            }
            if let Err(error) = db.update_mcp_tools(
                &server.id,
                &serde_json::to_value(&server.tools).unwrap_or(Value::Null),
            ) {
                warnings.push(format!(
                    "{} is always allowed, but its policy could not be cached: {error}",
                    server.name
                ));
            }
        }
        for tool in server.tools.iter().filter(|t| t.policy != "off") {
            let function_name = openai_name(&server.name, &tool.name);
            let definition = json!({
                "type": "function",
                "function": {
                    "name": function_name,
                    "description": format!("{}{}", if server.name.is_empty() { String::new() } else { format!("{}: ", server.name) }, tool.description),
                    "parameters": if tool.schema.is_object() { tool.schema.clone() } else { json!({"type":"object","properties":{}}) }
                }
            });
            out.push(DiscoveredTool {
                server: server.clone(),
                tool: tool.clone(),
                function_name,
                definition,
            });
        }
    }
    (out, warnings)
}

pub fn find_tool<'a>(tools: &'a [DiscoveredTool], name: &str) -> Option<&'a DiscoveredTool> {
    tools.iter().find(|tool| tool.function_name == name)
}

pub fn call(server: &McpServer, secrets: &Secrets, name: &str, args: Value) -> Result<String> {
    let result = rpc(
        server,
        secrets,
        "tools/call",
        json!({"name": name, "arguments": args}),
        20,
    )?;
    if result
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        anyhow::bail!(
            "{}",
            result
                .get("content")
                .and_then(Value::as_array)
                .map(|parts| text_parts(parts))
                .unwrap_or_else(|| "MCP tool failed".into())
        );
    }
    Ok(result
        .get("content")
        .and_then(Value::as_array)
        .map(|parts| text_parts(parts))
        .unwrap_or_default())
}

pub fn call_requires_approval(tool: &McpTool) -> bool {
    match tool.policy.as_str() {
        "allow" | "auto" => false,
        "ask" => true,
        _ => tool.kind != "read",
    }
}

pub fn set_server_always_allowed(db: &Database, server: &mut McpServer, allow: bool) -> Result<()> {
    db.set_kv(
        &format!("mcp_always_allow_{}", server.id),
        if allow { "true" } else { "false" },
    )?;
    for tool in &mut server.tools {
        tool.policy = if allow {
            "allow"
        } else if tool.kind == "read" {
            "auto"
        } else {
            "ask"
        }
        .into();
    }
    db.update_mcp_tools(&server.id, &serde_json::to_value(&server.tools)?)
}

pub fn server_always_allowed(db: &Database, server_id: &str) -> bool {
    db.get_kv(&format!("mcp_always_allow_{server_id}"))
        .ok()
        .flatten()
        .is_some_and(|value| value == "true")
}

fn preserve_policies(discovered: &mut [McpTool], stored: &[McpTool]) {
    for tool in discovered {
        let Some(previous) = stored.iter().find(|candidate| candidate.name == tool.name) else {
            continue;
        };
        if matches!(previous.policy.as_str(), "allow" | "ask" | "auto" | "off") {
            tool.policy.clone_from(&previous.policy);
        }
    }
}

pub fn openai_name(server: &str, tool: &str) -> String {
    fn clean(s: &str) -> String {
        s.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' {
                    c.to_ascii_lowercase()
                } else {
                    '_'
                }
            })
            .collect()
    }
    let server = clean(server).chars().take(20).collect::<String>();
    let tool = clean(tool).chars().take(36).collect::<String>();
    format!("mcp_{server}_{tool}")
}

fn text_parts(parts: &[Value]) -> String {
    parts
        .iter()
        .map(|part| {
            if part.get("type").and_then(Value::as_str) == Some("text") {
                part.get("text")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string()
            } else {
                format!(
                    "[{}]",
                    part.get("type")
                        .and_then(Value::as_str)
                        .unwrap_or("content")
                )
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn classify_tool(name: &str, annotations: Option<&Value>) -> &'static str {
    if annotations
        .and_then(|a| a.get("readOnlyHint"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return "read";
    }
    let words = name.replace(['_', '-'], " ").to_lowercase();
    if annotations
        .and_then(|a| a.get("destructiveHint"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || [
            "delete",
            "remove",
            "drop",
            "destroy",
            "purge",
            "wipe",
            "erase",
            "revoke",
            "terminate",
        ]
        .iter()
        .any(|word| words.contains(word))
    {
        return "delete";
    }
    let first = words.split_whitespace().next().unwrap_or("");
    if [
        "get", "list", "read", "search", "find", "fetch", "query", "describe", "show", "view",
        "lookup", "check", "count", "resolve", "retrieve", "browse", "inspect", "validate",
    ]
    .contains(&first)
    {
        return "read";
    }
    if [
        "create", "update", "write", "send", "post", "set", "insert", "edit", "add", "save",
        "upload", "publish", "commit", "merge", "apply", "deploy", "install", "run", "execute",
        "shell", "script", "move", "copy", "invite", "grant", "approve",
    ]
    .iter()
    .any(|word| words.contains(word))
    {
        return "write";
    }
    "write"
}

#[cfg(test)]
mod tests {
    use super::{call_requires_approval, discover, preserve_policies, set_server_always_allowed};
    use crate::config::Paths;
    use crate::db::Database;
    use crate::models::McpTool;
    use crate::secrets::Secrets;
    use serde_json::json;

    fn tool(name: &str, kind: &str, policy: &str) -> McpTool {
        McpTool {
            name: name.into(),
            description: String::new(),
            kind: kind.into(),
            policy: policy.into(),
            schema: json!({"type":"object"}),
        }
    }

    #[test]
    fn explicit_allow_removes_the_approval_prompt_for_writing_tools() {
        assert!(call_requires_approval(&tool("create_note", "write", "ask")));
        assert!(!call_requires_approval(&tool(
            "create_note",
            "write",
            "allow"
        )));
        assert!(!call_requires_approval(&tool("search", "read", "auto")));
        assert!(call_requires_approval(&tool("search", "read", "ask")));
    }

    #[test]
    fn refresh_preserves_existing_per_tool_policies() {
        let mut discovered = vec![tool("create_note", "write", "ask")];
        let stored = vec![tool("create_note", "write", "allow")];
        preserve_policies(&mut discovered, &stored);
        assert_eq!(discovered[0].policy, "allow");
    }

    #[test]
    fn always_for_server_policy_applies_to_its_discovered_tools() {
        let dir = std::env::temp_dir().join(format!("aihat-mcp-always-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let paths = Paths {
            data_dir: dir.clone(),
            db_file: dir.join("aihat.db"),
            config_file: dir.join("settings.json"),
            keys_file: dir.join("keys.json"),
            downloads_dir: dir.join("Downloads"),
        };
        let db = Database::open(&paths.db_file).unwrap();
        let server_id = db
            .add_mcp_server("Notes", "https://example.test/mcp", "http", "none")
            .unwrap();
        let tools = vec![tool("create_note", "write", "ask")];
        db.update_mcp_tools(&server_id, &serde_json::to_value(&tools).unwrap())
            .unwrap();
        db.set_kv(&format!("mcp_always_allow_{server_id}"), "true")
            .unwrap();

        let (discovered, warnings) = discover(&db, &Secrets::new(&paths), &[server_id], &std::sync::atomic::AtomicBool::new(false), &mut |_, _| {});
        assert!(warnings.is_empty());
        assert_eq!(discovered.len(), 1);
        assert_eq!(discovered[0].tool.policy, "allow");
        assert!(!call_requires_approval(&discovered[0].tool));
        let mut server = db
            .mcp_servers()
            .unwrap()
            .into_iter()
            .find(|server| server.id == discovered[0].server.id)
            .unwrap();
        set_server_always_allowed(&db, &mut server, false).unwrap();
        let (discovered, warnings) = discover(&db, &Secrets::new(&paths), &[server.id.clone()], &std::sync::atomic::AtomicBool::new(false), &mut |_, _| {});
        assert!(warnings.is_empty());
        assert_eq!(discovered[0].tool.policy, "ask");
        assert!(call_requires_approval(&discovered[0].tool));
        drop(db);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
