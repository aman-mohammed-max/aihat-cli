use crate::models::Provider;
use crate::secrets::Secrets;
use anyhow::{Context, Result, anyhow, bail};
use reqwest::blocking::{Client, Response};
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

static MCP_SESSIONS: OnceLock<Mutex<BTreeMap<String, McpSession>>> = OnceLock::new();

#[derive(Clone, Debug)]
struct McpSession {
    id: Option<String>,
    protocol_version: String,
}

pub fn client(timeout_ms: u64) -> Result<Client> {
    Ok(Client::builder()
        .timeout(std::time::Duration::from_millis(timeout_ms.max(1000)))
        .user_agent(concat!("AI-Hat-CLI/", env!("CARGO_PKG_VERSION")))
        .build()?)
}

pub fn list_models(provider: &Provider, secrets: &Secrets) -> Result<(Vec<Value>, u64)> {
    let client = client(15_000)?;
    let started = Instant::now();
    let models_path = if provider.name.eq_ignore_ascii_case("FreeLLMAPI") {
        "models?available=true"
    } else {
        "models"
    };
    let response = client
        .get(endpoint(provider, models_path))
        .headers(headers(provider, secrets)?)
        .send()
        .map_err(|error| request_error(provider, error))?;
    let value = checked_json(response)?;
    let rows = value
        .as_array()
        .or_else(|| value.get("data").and_then(Value::as_array))
        .or_else(|| value.get("models").and_then(Value::as_array))
        .cloned()
        .unwrap_or_default();
    let rows = rows
        .into_iter()
        .filter_map(|item| match item {
            Value::String(id) => Some(json!({"id": id})),
            Value::Object(mut map) => {
                if map.get("id").is_none() {
                    if let Some(name) = map.get("name").cloned() {
                        map.insert("id".into(), name);
                    }
                }
                if map.get("id").and_then(Value::as_str).is_some() {
                    Some(Value::Object(map))
                } else {
                    None
                }
            }
            _ => None,
        })
        .collect();
    Ok((rows, started.elapsed().as_millis() as u64))
}

/// What the provider sent back, split into the answer and the model's reasoning.
#[derive(Clone, Debug, Default)]
pub struct StreamedReply {
    pub content: String,
    pub reasoning: String,
    pub tool_calls: Vec<Value>,
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub elapsed_ms: u64,
    pub cancelled: bool,
}

pub enum StreamEvent<'a> {
    Reasoning(&'a str),
    Text(&'a str),
}

/// Stream one chat completion. `cancel` is checked between chunks; when it is
/// set the partial reply is returned with `cancelled` set.
pub fn chat(
    provider: &Provider,
    secrets: &Secrets,
    model: &str,
    messages: &[Value],
    params: &Value,
    tools: &[Value],
    cancel: &AtomicBool,
    on_event: &mut dyn FnMut(StreamEvent<'_>),
) -> Result<StreamedReply> {
    let client = client(provider.timeout_ms)?;
    let started = Instant::now();
    let mut body = json!({
        "model": model,
        "messages": messages,
        "stream": true,
        "stream_options": {"include_usage": true}
    });
    if let Some(value) = params.get("temperature").and_then(Value::as_f64) {
        body["temperature"] = json!(value);
    }
    if let Some(value) = params.get("maxTokens").and_then(Value::as_u64) {
        body["max_tokens"] = json!(value);
    }
    if let Some(value) = params.get("topP").and_then(Value::as_f64) {
        body["top_p"] = json!(value);
    }
    let reasoning = params.get("reasoning").and_then(Value::as_str).unwrap_or("off");
    if reasoning != "off" && !reasoning.is_empty() {
        body["reasoning_effort"] = json!(reasoning);
    }
    if !tools.is_empty() {
        body["tools"] = json!(tools);
    }
    let response = client
        .post(endpoint(provider, "chat/completions"))
        .headers(headers(provider, secrets)?)
        .json(&body)
        .send()
        .map_err(|error| request_error(provider, error))?;
    let response = ensure_success(response)?;
    let mut reply = StreamedReply::default();
    let is_event_stream = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.contains("event-stream"));
    let mut splitter = ThinkSplitter::default();

    if !is_event_stream {
        // Some servers ignore `stream: true` and answer with one JSON object.
        let value: Value = response.json().context("reading model response")?;
        let message = value.pointer("/choices/0/message").cloned().unwrap_or_else(|| json!({}));
        let thought = message
            .get("reasoning_content")
            .or_else(|| message.get("reasoning"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if !thought.is_empty() {
            on_event(StreamEvent::Reasoning(thought));
            reply.reasoning.push_str(thought);
        }
        splitter.feed(&content_text(message.get("content")), &mut reply, on_event);
        splitter.finish(&mut reply, on_event);
        reply.tool_calls = message
            .get("tool_calls")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        reply.prompt_tokens = value.pointer("/usage/prompt_tokens").and_then(Value::as_u64);
        reply.completion_tokens = value.pointer("/usage/completion_tokens").and_then(Value::as_u64);
        reply.elapsed_ms = started.elapsed().as_millis() as u64;
        return Ok(reply);
    }

    let mut calls: BTreeMap<usize, Value> = BTreeMap::new();
    let reader = BufReader::new(response);
    for line in reader.lines() {
        if cancel.load(Ordering::Relaxed) {
            reply.cancelled = true;
            break;
        }
        let line = line.context("reading the reply stream")?;
        let Some(payload) = line.strip_prefix("data:").map(str::trim) else {
            continue;
        };
        if payload == "[DONE]" {
            break;
        }
        let Ok(chunk) = serde_json::from_str::<Value>(payload) else {
            continue;
        };
        if let Some(message) = chunk.pointer("/error/message").and_then(Value::as_str) {
            bail!(ApiError::new(ErrorKind::Http, None, message));
        }
        if let Some(tokens) = chunk.pointer("/usage/prompt_tokens").and_then(Value::as_u64) {
            reply.prompt_tokens = Some(tokens);
        }
        if let Some(tokens) = chunk.pointer("/usage/completion_tokens").and_then(Value::as_u64) {
            reply.completion_tokens = Some(tokens);
        }
        let Some(delta) = chunk.pointer("/choices/0/delta") else {
            continue;
        };
        let thought = delta
            .get("reasoning_content")
            .or_else(|| delta.get("reasoning"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if !thought.is_empty() {
            on_event(StreamEvent::Reasoning(thought));
            reply.reasoning.push_str(thought);
        }
        splitter.feed(&content_text(delta.get("content")), &mut reply, on_event);
        if let Some(parts) = delta.get("tool_calls").and_then(Value::as_array) {
            for part in parts {
                let index = part.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                let call = calls.entry(index).or_insert_with(|| {
                    json!({"id": "", "type": "function", "function": {"name": "", "arguments": ""}})
                });
                if let Some(id) = part.get("id").and_then(Value::as_str) {
                    call["id"] = json!(id);
                }
                for field in ["name", "arguments"] {
                    if let Some(piece) = part.pointer(&format!("/function/{field}")).and_then(Value::as_str) {
                        let old = call["function"][field].as_str().unwrap_or("").to_string();
                        call["function"][field] = json!(format!("{old}{piece}"));
                    }
                }
            }
        }
    }
    splitter.finish(&mut reply, on_event);
    reply.tool_calls = calls
        .into_values()
        .enumerate()
        .map(|(index, mut call)| {
            if call["id"].as_str().unwrap_or("").is_empty() {
                call["id"] = json!(format!("call_{index}_{}", uuid::Uuid::new_v4().simple()));
            }
            call
        })
        .collect();
    reply.elapsed_ms = started.elapsed().as_millis() as u64;
    Ok(reply)
}

/// Splits `<think>…</think>` blocks that some models write into the answer
/// text, including tags that arrive split across chunks.
#[derive(Default)]
struct ThinkSplitter {
    pending: String,
    inside: bool,
}

impl ThinkSplitter {
    fn feed(&mut self, text: &str, reply: &mut StreamedReply, on_event: &mut dyn FnMut(StreamEvent<'_>)) {
        if text.is_empty() {
            return;
        }
        self.pending.push_str(text);
        loop {
            let tag = if self.inside { "</think>" } else { "<think>" };
            if let Some(at) = self.pending.find(tag) {
                let before: String = self.pending[..at].to_string();
                self.emit(&before, reply, on_event);
                self.pending.drain(..at + tag.len());
                self.inside = !self.inside;
                continue;
            }
            // Keep a possible partial tag at the end for the next chunk.
            let keep = (1..tag.len())
                .rev()
                .find(|n| self.pending.ends_with(&tag[..*n]))
                .unwrap_or(0);
            let cut = self.pending.len() - keep;
            let ready: String = self.pending[..cut].to_string();
            self.emit(&ready, reply, on_event);
            self.pending.drain(..cut);
            break;
        }
    }

    fn finish(&mut self, reply: &mut StreamedReply, on_event: &mut dyn FnMut(StreamEvent<'_>)) {
        let rest = std::mem::take(&mut self.pending);
        self.emit(&rest, reply, on_event);
    }

    fn emit(&self, text: &str, reply: &mut StreamedReply, on_event: &mut dyn FnMut(StreamEvent<'_>)) {
        if text.is_empty() {
            return;
        }
        if self.inside {
            reply.reasoning.push_str(text);
            on_event(StreamEvent::Reasoning(text));
        } else {
            reply.content.push_str(text);
            on_event(StreamEvent::Text(text));
        }
    }
}

/// A non-streaming completion that returns only the answer text. Used for
/// small helper requests such as titles and summaries.
pub fn complete(
    provider: &Provider,
    secrets: &Secrets,
    model: &str,
    messages: &[Value],
    params: &Value,
) -> Result<StreamedReply> {
    let never = AtomicBool::new(false);
    chat(provider, secrets, model, messages, params, &[], &never, &mut |_| {})
}

pub fn image_generation_sized(
    provider: &Provider,
    secrets: &Secrets,
    model: &str,
    prompt: &str,
    count: usize,
    size: Option<&str>,
) -> Result<Vec<Value>> {
    let mut body = json!({"prompt": prompt, "n": count.clamp(1, 4), "response_format": "b64_json"});
    if !model.is_empty() {
        body["model"] = json!(model);
    }
    if let Some(size) = size {
        body["size"] = json!(size);
    }
    let client = client(provider.timeout_ms)?;
    let response = client
        .post(endpoint(provider, "images/generations"))
        .headers(headers(provider, secrets)?)
        .json(&body)
        .send()?;
    let value = checked_json(response)?;
    Ok(value
        .get("data")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

pub fn image_a1111(
    url: &str,
    prompt: &str,
    negative: &str,
    model: &str,
    count: usize,
    width: u32,
    height: u32,
) -> Result<Vec<Value>> {
    let mut body = json!({
        "prompt": prompt,
        "negative_prompt": negative,
        "width": width.min(1024),
        "height": height.min(1024),
        "batch_size": count.clamp(1, 4),
        "steps": 25
    });
    if !model.is_empty() {
        body["override_settings"] = json!({"sd_model_checkpoint":model});
    }
    let response = client(240_000)?
        .post(format!("{}/sdapi/v1/txt2img", url.trim_end_matches('/')))
        .json(&body)
        .send()
        .context("connecting to Automatic1111")?;
    let value = checked_json(response)?;
    Ok(value
        .get("images")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|image| image.as_str().map(|data| json!({"b64_json":data})))
        .collect())
}

pub fn image_comfy(
    url: &str,
    workflow: &str,
    prompt: &str,
    negative: &str,
    model: &str,
    count: usize,
    width: u32,
    height: u32,
) -> Result<Vec<Value>> {
    let workflow = if workflow.trim().is_empty() {
        default_comfy_workflow()
    } else {
        serde_json::from_str(workflow).context("parsing the ComfyUI workflow")?
    };
    let vars = json!({
        "prompt": prompt,
        "negative": negative,
        "model": model,
        "width": width,
        "height": height,
        "n": count.clamp(1, 4),
        "seed": chrono::Utc::now().timestamp_millis().unsigned_abs() % (i32::MAX as u64)
    });
    let workflow = fill_workflow(workflow, &vars);
    let base = url.trim_end_matches('/');
    let client = client(30_000)?;
    let submit = client
        .post(format!("{base}/prompt"))
        .json(&json!({"prompt":workflow}))
        .send()
        .context("connecting to ComfyUI")?;
    let submitted: Value = checked_json(submit).context("submitting the ComfyUI workflow")?;
    let prompt_id = submitted
        .get("prompt_id")
        .and_then(Value::as_str)
        .context("ComfyUI did not return a prompt id")?;
    for _ in 0..240 {
        std::thread::sleep(std::time::Duration::from_secs(1));
        let history_response = client
            .get(format!("{base}/history/{}", urlencoding::encode(prompt_id)))
            .send();
        let Ok(response) = history_response else {
            continue;
        };
        if !response.status().is_success() {
            continue;
        }
        let history: Value = match response.json() {
            Ok(value) => value,
            Err(_) => continue,
        };
        let Some(entry) = history.get(prompt_id) else {
            continue;
        };
        if entry.pointer("/status/status_str").and_then(Value::as_str) == Some("error") {
            bail!("ComfyUI failed to run the workflow");
        }
        let files = entry
            .get("outputs")
            .and_then(Value::as_object)
            .into_iter()
            .flat_map(|outputs| outputs.values())
            .filter_map(|output| output.get("images").and_then(Value::as_array))
            .flat_map(|images| images.iter())
            .filter_map(|image| {
                Some((
                    image.get("filename")?.as_str()?.to_string(),
                    image
                        .get("subfolder")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    image
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or("output")
                        .to_string(),
                ))
            })
            .collect::<Vec<_>>();
        if files.is_empty() {
            continue;
        }
        let mut result = Vec::new();
        for (filename, subfolder, kind) in files.into_iter().take(count.clamp(1, 4)) {
            let address = format!(
                "{base}/view?filename={}&subfolder={}&type={}",
                urlencoding::encode(&filename),
                urlencoding::encode(&subfolder),
                urlencoding::encode(&kind)
            );
            let response = client
                .get(address)
                .send()
                .context("downloading a ComfyUI image")?;
            let bytes = checked_bytes(response)?;
            result.push(json!({"b64_json":base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes)}));
        }
        return Ok(result);
    }
    bail!("ComfyUI took too long (4 minutes)")
}

pub fn search_searxng_safe(url: &str, query: &str, count: usize, safe: bool) -> Result<Vec<Value>> {
    let address = format!(
        "{}/search?format=json&q={}&safesearch={}",
        url.trim_end_matches('/'),
        urlencoding::encode(query),
        if safe { 1 } else { 0 }
    );
    let response = client(15_000)?.get(address).send()?;
    let value = checked_json(response)?;
    Ok(value
        .get("results")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .take(count)
        .collect())
}

pub fn search_provider(
    provider: &Provider,
    secrets: &Secrets,
    query: &str,
    count: usize,
    model: Option<&str>,
) -> Result<Value> {
    let mut body = json!({
        "query": query,
        "q": query,
        "max_results": count,
        "num_results": count,
        "n": count,
        "count": count
    });
    if let Some(model) = model.filter(|value| !value.is_empty()) {
        body["model"] = json!(model);
    }
    let response = client(provider.timeout_ms)?
        .post(endpoint(provider, "search"))
        .headers(headers(provider, secrets)?)
        .json(&body)
        .send()
        .context("connecting to the web search provider")?;
    checked_json(response)
}

/// Use FreeLLMAPI's Gemini Google Search grounding through its OpenAI-compatible
/// tool bridge. This keeps web search on the user's configured provider instead
/// of assuming a separate local SearXNG process is installed.
pub fn search_google_grounded(
    provider: &Provider,
    secrets: &Secrets,
    query: &str,
    count: usize,
) -> Result<Value> {
    let prompt = format!(
        "Search the web for this query: {query}\n\nReturn only valid JSON in this exact shape: {{\"results\":[{{\"title\":\"page title\",\"url\":\"https://...\",\"snippet\":\"short relevant excerpt\"}}]}}. Return up to {count} distinct pages actually found by Google Search. Use their exact page URLs and titles. Do not invent a URL, and do not add prose or Markdown outside the JSON."
    );
    let body = json!({
        "model": "gemini-2.5-flash",
        "messages": [
            {"role":"system","content":"You are a web search result extractor. Use Google Search grounding for every query. Return source pages only."},
            {"role":"user","content":prompt}
        ],
        "tools": [{
            "type":"function",
            "function":{"name":"google_search","parameters":{}}
        }],
        "stream": false,
        "temperature": 0
    });
    let response = client(provider.timeout_ms.max(60_000))?
        .post(endpoint(provider, "chat/completions"))
        .headers(headers(provider, secrets)?)
        .json(&body)
        .send()
        .context("connecting to FreeLLMAPI Google Search grounding")?;
    checked_json(response)
}

pub fn search_brave(query: &str, count: usize, safe: bool, key: &str) -> Result<Value> {
    let address = format!(
        "https://api.search.brave.com/res/v1/web/search?q={}&count={}&safesearch={}",
        urlencoding::encode(query),
        count,
        if safe { "strict" } else { "off" }
    );
    let token = HeaderValue::from_str(key).context("invalid Brave Search API key")?;
    let response = client(20_000)?
        .get(address)
        .header("X-Subscription-Token", token)
        .header(ACCEPT, "application/json")
        .send()
        .context("connecting to Brave Search")?;
    checked_json(response)
}

pub fn search_tavily(query: &str, count: usize, key: &str) -> Result<Value> {
    let token =
        HeaderValue::from_str(&format!("Bearer {key}")).context("invalid Tavily API key")?;
    let mut request_headers = HeaderMap::new();
    request_headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    request_headers.insert(AUTHORIZATION, token);
    let response = client(20_000)?
        .post("https://api.tavily.com/search")
        .headers(request_headers)
        .json(&json!({"query":query,"max_results":count}))
        .send()
        .context("connecting to Tavily Search")?;
    checked_json(response)
}

pub fn mcp_rpc(
    server: &crate::models::McpServer,
    secrets: &Secrets,
    method: &str,
    params: Value,
    id: u64,
) -> Result<Value> {
    let client = client(30_000)?;
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(
        ACCEPT,
        HeaderValue::from_static("application/json, text/event-stream"),
    );
    if let Some(token) = secrets.mcp_token(&server.id) {
        let header = server.auth_header.as_deref().unwrap_or("Authorization");
        let key = HeaderName::from_bytes(header.as_bytes()).context("invalid MCP auth header")?;
        let template = server.auth_template.as_deref().unwrap_or("Bearer {token}");
        let value = template.replace("{token}", &token);
        headers.insert(
            key,
            HeaderValue::from_str(&value).context("invalid MCP auth value")?,
        );
    }
    let known_session = MCP_SESSIONS
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .ok()
        .and_then(|sessions| sessions.get(&server.id).cloned());
    if let Some(session) = &known_session {
        if let Some(id) = &session.id {
            headers.insert(
                HeaderName::from_static("mcp-session-id"),
                HeaderValue::from_str(id)?,
            );
        }
        if method != "initialize" {
            headers.insert(
                HeaderName::from_static("mcp-protocol-version"),
                HeaderValue::from_str(&session.protocol_version)?,
            );
        }
    }
    let response = client
        .post(&server.url)
        .headers(headers)
        .json(&json!({
            "jsonrpc": "2.0", "id": id, "method": method, "params": params
        }))
        .send()
        .context("calling MCP server")?;
    let response_session = response
        .headers()
        .get("mcp-session-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|x| x.to_str().ok())
        .unwrap_or("")
        .to_string();
    let text = checked_text(response)?;
    let value = if content_type.contains("text/event-stream") {
        text.lines()
            .filter_map(|line| line.strip_prefix("data:").map(str::trim))
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .find(|msg| msg.get("id").and_then(Value::as_u64) == Some(id))
            .ok_or_else(|| anyhow!("MCP server ended without a response"))?
    } else {
        serde_json::from_str::<Value>(&text).context("reading MCP response")?
    };
    if let Some(error) = value.get("error") {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("MCP error");
        return Err(anyhow!("{message}"));
    }
    let protocol_version = if method == "initialize" {
        value
            .pointer("/result/protocolVersion")
            .and_then(Value::as_str)
            .unwrap_or("2025-03-26")
            .to_string()
    } else {
        known_session
            .map(|session| session.protocol_version)
            .unwrap_or_else(|| "2025-03-26".into())
    };
    if method == "initialize" || response_session.is_some() {
        if let Ok(mut sessions) = MCP_SESSIONS
            .get_or_init(|| Mutex::new(BTreeMap::new()))
            .lock()
        {
            let previous_id = sessions
                .get(&server.id)
                .and_then(|session| session.id.clone());
            sessions.insert(
                server.id.clone(),
                McpSession {
                    id: response_session.or(previous_id),
                    protocol_version,
                },
            );
        }
    }
    Ok(value.get("result").cloned().unwrap_or(Value::Null))
}

pub fn mcp_notify(
    server: &crate::models::McpServer,
    secrets: &Secrets,
    method: &str,
    params: Value,
) -> Result<()> {
    let client = client(30_000)?;
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(
        ACCEPT,
        HeaderValue::from_static("application/json, text/event-stream"),
    );
    if let Some(token) = secrets.mcp_token(&server.id) {
        let header = server.auth_header.as_deref().unwrap_or("Authorization");
        let key = HeaderName::from_bytes(header.as_bytes()).context("invalid MCP auth header")?;
        let template = server.auth_template.as_deref().unwrap_or("Bearer {token}");
        headers.insert(
            key,
            HeaderValue::from_str(&template.replace("{token}", &token))
                .context("invalid MCP auth value")?,
        );
    }
    if let Some(session) = MCP_SESSIONS
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .ok()
        .and_then(|sessions| sessions.get(&server.id).cloned())
    {
        if let Some(id) = session.id {
            headers.insert(
                HeaderName::from_static("mcp-session-id"),
                HeaderValue::from_str(&id)?,
            );
        }
        headers.insert(
            HeaderName::from_static("mcp-protocol-version"),
            HeaderValue::from_str(&session.protocol_version)?,
        );
    }
    let response = client
        .post(&server.url)
        .headers(headers)
        .json(&json!({
            "jsonrpc": "2.0", "method": method, "params": params
        }))
        .send()?;
    ensure_success(response)?;
    Ok(())
}

fn headers(provider: &Provider, secrets: &Secrets) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(
        ACCEPT,
        HeaderValue::from_static("application/json, text/event-stream"),
    );
    if let Some(key) = secrets.provider_key(&provider.id) {
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {key}")).context("invalid API key")?,
        );
    }
    if let Some(custom) = provider.headers.as_object() {
        for (name, value) in custom {
            let name =
                HeaderName::from_bytes(name.as_bytes()).context("invalid provider header name")?;
            let value = value
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| value.to_string());
            headers.insert(
                name,
                HeaderValue::from_str(&value).context("invalid provider header value")?,
            );
        }
    }
    Ok(headers)
}

fn endpoint(provider: &Provider, path: &str) -> String {
    format!(
        "{}/{}",
        provider.base_url.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

fn ensure_success(response: Response) -> Result<Response> {
    if response.status().is_success() {
        return Ok(response);
    }
    let status = response.status();
    let body = response.text().unwrap_or_default();
    let detail = serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|value| {
            value
                .pointer("/error/message")
                .or_else(|| value.get("message"))
                .or_else(|| value.get("detail"))
                .or_else(|| value.get("error"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| body.trim().chars().take(300).collect());
    let kind = match status.as_u16() {
        401 | 403 => ErrorKind::Auth,
        404 => ErrorKind::NotFound,
        _ => ErrorKind::Http,
    };
    Err(ApiError::new(kind, Some(status.as_u16()), &detail).into())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// No connection: refused, DNS, timeout.
    Unreachable,
    /// 401/403: the key was rejected.
    Auth,
    NotFound,
    Http,
}

/// A provider failure with enough structure for exit codes and friendly messages.
#[derive(Clone, Debug)]
pub struct ApiError {
    pub kind: ErrorKind,
    pub status: Option<u16>,
    pub detail: String,
}

impl ApiError {
    pub fn new(kind: ErrorKind, status: Option<u16>, detail: &str) -> Self {
        Self {
            kind,
            status,
            detail: detail.trim().to_string(),
        }
    }

    /// A short label for lists: "401", "offline", "timed out".
    pub fn short(&self) -> String {
        match (self.kind, self.status) {
            (ErrorKind::Unreachable, _) if self.detail.contains("timed out") => "timed out".into(),
            (ErrorKind::Unreachable, _) => "offline".into(),
            (_, Some(status)) => status.to_string(),
            _ => "failed".into(),
        }
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (self.kind, self.status) {
            (ErrorKind::Auth, Some(status)) => {
                let reason = if status == 401 { "Unauthorized" } else { "Forbidden" };
                write!(f, "{status} {reason}: the key was rejected.")?;
                if !self.detail.is_empty() && !self.detail.to_lowercase().contains("unauthorized") {
                    write!(f, " {}", self.detail)?;
                }
                Ok(())
            }
            (_, Some(status)) if self.detail.is_empty() => write!(f, "HTTP {status}"),
            (_, Some(status)) => write!(f, "HTTP {status}: {}", self.detail),
            _ => f.write_str(&self.detail),
        }
    }
}

impl std::error::Error for ApiError {}

/// Turn a reqwest send error into an `ApiError` that says what went wrong.
pub fn request_error(provider: &Provider, error: reqwest::Error) -> anyhow::Error {
    let seconds = provider.timeout_ms.max(1000) / 1000;
    let detail = if error.is_timeout() {
        format!("No answer after {seconds}s (timed out).")
    } else if error.is_connect() {
        let text = format!("{error:#}").to_lowercase();
        if text.contains("refused") {
            "Connection refused.".to_string()
        } else if text.contains("dns") || text.contains("resolve") || text.contains("name") {
            "The host name could not be resolved.".to_string()
        } else {
            "Could not connect.".to_string()
        }
    } else {
        return anyhow::Error::new(error).context("sending the request");
    };
    ApiError::new(ErrorKind::Unreachable, None, &detail).into()
}

/// Find the provider failure inside an error chain, if there is one.
pub fn api_error(error: &anyhow::Error) -> Option<&ApiError> {
    error.chain().find_map(|cause| cause.downcast_ref::<ApiError>())
}

fn checked_json(response: Response) -> Result<Value> {
    ensure_success(response)?
        .json()
        .context("reading JSON response")
}

fn checked_text(response: Response) -> Result<String> {
    ensure_success(response)?.text().context("reading response")
}

fn checked_bytes(response: Response) -> Result<Vec<u8>> {
    Ok(ensure_success(response)?.bytes()?.to_vec())
}

fn default_comfy_workflow() -> Value {
    json!({
        "3":{"class_type":"KSampler","inputs":{"seed":"{{seed}}","steps":20,"cfg":7,"sampler_name":"euler","scheduler":"normal","denoise":1,"model":["4",0],"positive":["6",0],"negative":["7",0],"latent_image":["5",0]}},
        "4":{"class_type":"CheckpointLoaderSimple","inputs":{"ckpt_name":"{{model}}"}},
        "5":{"class_type":"EmptyLatentImage","inputs":{"width":"{{width}}","height":"{{height}}","batch_size":"{{n}}"}},
        "6":{"class_type":"CLIPTextEncode","inputs":{"text":"{{prompt}}","clip":["4",1]}},
        "7":{"class_type":"CLIPTextEncode","inputs":{"text":"{{negative}}","clip":["4",1]}},
        "8":{"class_type":"VAEDecode","inputs":{"samples":["3",0],"vae":["4",2]}},
        "9":{"class_type":"SaveImage","inputs":{"filename_prefix":"aihat","images":["8",0]}}
    })
}

fn fill_workflow(value: Value, vars: &Value) -> Value {
    match value {
        Value::String(text) => {
            if text.starts_with("{{") && text.ends_with("}}") {
                let key = &text[2..text.len().saturating_sub(2)];
                if let Some(value) = vars.get(key) {
                    return value.clone();
                }
            }
            let mut filled = text;
            if let Some(values) = vars.as_object() {
                for (key, value) in values {
                    let rendered = value
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| value.to_string());
                    filled = filled.replace(&format!("{{{{{key}}}}}"), &rendered);
                }
            }
            Value::String(filled)
        }
        Value::Array(values) => Value::Array(
            values
                .into_iter()
                .map(|value| fill_workflow(value, vars))
                .collect(),
        ),
        Value::Object(values) => Value::Object(
            values
                .into_iter()
                .map(|(key, value)| (key, fill_workflow(value, vars)))
                .collect(),
        ),
        other => other,
    }
}

fn content_text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}
