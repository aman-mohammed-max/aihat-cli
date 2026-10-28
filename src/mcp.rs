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

pub fn connect(server: &McpServer, secrets: &Secrets) -> Result<Vec<McpTool>> {
    api::mcp_rpc(
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
    let _ = api::mcp_notify(server, secrets, "notifications/initialized", json!({}));
    let mut found = Vec::new();
    let mut cursor: Option<String> = None;
    for page in 0..10u64 {
        let params = cursor
            .as_ref()
            .map(|c| json!({"cursor": c}))
            .unwrap_or_else(|| json!({}));
        let result = api::mcp_rpc(server, secrets, "tools/list", params, 2 + page)?;
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

pub fn discover(
    db: &Database,
    secrets: &Secrets,
    enabled_ids: &[String],
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
        if server.tools.is_empty() || !server.connected {
            match connect(&server, secrets) {
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
    let result = api::mcp_rpc(
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
    let words = name.replace('_', " ").replace('-', " ").to_lowercase();
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
            config_file: dir.join("config.toml"),
            keys_file: dir.join("keys.json"),
            downloads_dir: dir.join("Downloads"),
        };
        let db = Database::open(&paths.db_file).unwrap();
        let server_id = db
            .add_mcp_server("Notes", "https://example.test/mcp")
            .unwrap();
        let tools = vec![tool("create_note", "write", "ask")];
        db.update_mcp_tools(&server_id, &serde_json::to_value(&tools).unwrap())
            .unwrap();
        db.set_kv(&format!("mcp_always_allow_{server_id}"), "true")
            .unwrap();

        let (discovered, warnings) = discover(&db, &Secrets::new(&paths), &[server_id]);
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
        let (discovered, warnings) = discover(&db, &Secrets::new(&paths), &[server.id.clone()]);
        assert!(warnings.is_empty());
        assert_eq!(discovered[0].tool.policy, "ask");
        assert!(call_requires_approval(&discovered[0].tool));
        drop(db);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
