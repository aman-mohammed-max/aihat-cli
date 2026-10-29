//! MCP servers that run as a program on this computer and talk JSON-RPC over
//! its standard input and output:
//!
//! ```text
//! aihat mcp add postgres -- npx -y @modelcontextprotocol/server-postgres $DB_URL
//! ```
//!
//! The command line is kept in the server's `url` with transport `stdio`.
//! The program starts on first use and keeps running until aihat exits.

use crate::models::McpServer;
use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

struct Running {
    child: Child,
    stdin: ChildStdin,
    replies: Receiver<Value>,
    /// The last lines the program wrote to stderr, to explain a failure.
    errors: Arc<Mutex<VecDeque<String>>>,
    next_id: u64,
    initialized: Option<Value>,
}

static RUNNING: OnceLock<Mutex<HashMap<String, Running>>> = OnceLock::new();

fn running() -> &'static Mutex<HashMap<String, Running>> {
    RUNNING.get_or_init(|| Mutex::new(HashMap::new()))
}

fn start(server: &McpServer) -> Result<Running> {
    let line = server.url.trim();
    if line.is_empty() {
        bail!("{} has no command to run", server.name);
    }
    let mut command = if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.args(["/C", line]);
        c
    } else {
        // Through the shell, so quoting and $VARIABLES work as typed.
        let mut c = Command::new("sh");
        c.args(["-c", line]);
        c
    };
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("couldn't start {}", server.name))?;
    let stdin = child.stdin.take().context("no input to the program")?;
    let stdout = child.stdout.take().context("no output from the program")?;
    let stderr = child.stderr.take();
    let (tx, replies) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if let Ok(message) = serde_json::from_str::<Value>(line.trim())
                && tx.send(message).is_err() {
                    break;
                }
        }
    });
    let errors = Arc::new(Mutex::new(VecDeque::new()));
    if let Some(stderr) = stderr {
        let errors = errors.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines() {
                let Ok(line) = line else { break };
                if let Ok(mut kept) = errors.lock() {
                    kept.push_back(line);
                    while kept.len() > 20 {
                        kept.pop_front();
                    }
                }
            }
        });
    }
    Ok(Running { child, stdin, replies, errors, next_id: 1, initialized: None })
}

impl Running {
    fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    fn last_error(&self) -> String {
        self.errors
            .lock()
            .ok()
            .and_then(|lines| lines.iter().rev().find(|l| !l.trim().is_empty()).cloned())
            .unwrap_or_default()
    }

    fn send(&mut self, message: &Value) -> Result<()> {
        let mut line = serde_json::to_string(message)?;
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).context("the program stopped reading")?;
        self.stdin.flush()?;
        Ok(())
    }

    fn request(&mut self, name: &str, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        let started = Instant::now();
        loop {
            let left = timeout.saturating_sub(started.elapsed());
            match self.replies.recv_timeout(left) {
                Ok(message) => {
                    // The program may ask things of us (ping, roots); answer
                    // ping and decline the rest.
                    if let (Some(asked), Some(their_id)) = (message.get("method").and_then(Value::as_str), message.get("id")) {
                        let reply = if asked == "ping" {
                            json!({"jsonrpc": "2.0", "id": their_id, "result": {}})
                        } else {
                            json!({"jsonrpc": "2.0", "id": their_id, "error": {"code": -32601, "message": "not supported"}})
                        };
                        let _ = self.send(&reply);
                        continue;
                    }
                    if message.get("id").and_then(Value::as_u64) != Some(id) {
                        continue;
                    }
                    if let Some(error) = message.get("error") {
                        bail!("{}", error.get("message").and_then(Value::as_str).unwrap_or("MCP error"));
                    }
                    return Ok(message.get("result").cloned().unwrap_or(Value::Null));
                }
                Err(RecvTimeoutError::Timeout) => bail!("{name} didn't answer within {}s", timeout.as_secs()),
                Err(RecvTimeoutError::Disconnected) => {
                    let why = self.last_error();
                    if why.is_empty() {
                        bail!("{name} stopped");
                    }
                    bail!("{name} stopped: {why}");
                }
            }
        }
    }

    fn ensure_initialized(&mut self, name: &str) -> Result<Value> {
        if let Some(result) = &self.initialized {
            return Ok(result.clone());
        }
        // The first start may download the program (npx, uvx), so allow time.
        let result = self.request(
            name,
            "initialize",
            json!({
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": {"name": "aihat", "version": env!("CARGO_PKG_VERSION")}
            }),
            Duration::from_secs(120),
        )?;
        self.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}}))?;
        self.initialized = Some(result.clone());
        Ok(result)
    }
}

/// Send a request, starting the program (and introducing ourselves) first
/// when it isn't running.
pub fn request(server: &McpServer, method: &str, params: Value) -> Result<Value> {
    let mut all = running().lock().map_err(|_| anyhow!("MCP programs are busy"))?;
    let restart = all.get_mut(&server.id).is_none_or(|run| !run.alive());
    if restart {
        all.insert(server.id.clone(), start(server)?);
    }
    let run = all.get_mut(&server.id).expect("just started");
    let initialized = run.ensure_initialized(&server.name);
    if let Err(error) = initialized {
        all.remove(&server.id);
        return Err(error);
    }
    if method == "initialize" {
        return initialized;
    }
    let result = run.request(&server.name, method, params, Duration::from_secs(120));
    if result.is_err() && !run.alive() {
        all.remove(&server.id);
    }
    result
}

/// Notifications need no answer; `initialized` was already sent at start.
pub fn notify(server: &McpServer, method: &str, params: Value) -> Result<()> {
    if method == "notifications/initialized" {
        return Ok(());
    }
    let mut all = running().lock().map_err(|_| anyhow!("MCP programs are busy"))?;
    if let Some(run) = all.get_mut(&server.id) {
        run.send(&json!({"jsonrpc": "2.0", "method": method, "params": params}))?;
    }
    Ok(())
}

/// Whether the program for this server is running now.
pub fn is_running(server_id: &str) -> bool {
    running().lock().ok().and_then(|mut all| all.get_mut(server_id).map(Running::alive)).unwrap_or(false)
}

/// Stop one program (after it was removed or changed) or all of them.
pub fn stop(server_id: Option<&str>) {
    if let Ok(mut all) = running().lock() {
        let ids: Vec<String> = match server_id {
            Some(id) => vec![id.to_string()],
            None => all.keys().cloned().collect(),
        };
        for id in ids {
            if let Some(mut run) = all.remove(&id) {
                let _ = run.child.kill();
                let _ = run.child.wait();
            }
        }
    }
}

/// `npx -y @scope/server $DB_URL` from the words after `--`, quoted so the
/// shell reads them back the same way.
pub fn command_line(words: &[String]) -> String {
    words
        .iter()
        .map(|word| {
            let plain = !word.is_empty() && word.chars().all(|c| c.is_ascii_alphanumeric() || "-_./:@=+,%".contains(c));
            if plain { word.clone() } else { format!("'{}'", word.replace('\'', "'\\''")) }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_lines_keep_their_words() {
        let words: Vec<String> = ["npx", "-y", "@scope/server", "postgres://u:p@h/db", "two words", "it's"].iter().map(|s| s.to_string()).collect();
        assert_eq!(command_line(&words), "npx -y @scope/server postgres://u:p@h/db 'two words' 'it'\\''s'");
    }

    #[cfg(unix)]
    #[test]
    fn talks_to_a_program_over_stdio() {
        // A tiny MCP server in shell: answers initialize and tools/list.
        let script = r#"while IFS= read -r line; do
  case "$line" in
    *'"initialize"'*) echo '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-03-26","capabilities":{}}}' ;;
    *'"tools/list"'*) echo '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"query","inputSchema":{"type":"object"}}]}}' ;;
  esac
done"#;
        let server = McpServer {
            id: format!("test-{}", uuid::Uuid::new_v4()),
            name: "shell".into(),
            url: format!("sh -c {}", command_line(&[script.to_string()])),
            transport: "stdio".into(),
            auth: "none".into(),
            tools: Vec::new(),
            enabled_by_default: true,
            is_connector: false,
            connected: false,
            auth_header: None,
            auth_template: None,
        };
        let tools = request(&server, "tools/list", json!({})).unwrap();
        assert_eq!(tools["tools"][0]["name"], "query");
        assert!(is_running(&server.id));
        stop(Some(&server.id));
        assert!(!is_running(&server.id));
    }
}
