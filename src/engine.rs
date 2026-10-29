//! One chat turn, shared by the full-screen chat and `aihat "prompt"`.
//!
//! The caller saves the user's message, builds the request with
//! [`build_messages`], then calls [`run_turn`]. The turn streams the reply,
//! runs any tools the model asks for (asking the caller to approve tools that
//! write data), and loops until the model answers. The caller saves the result.

use crate::api::{self, StreamEvent};
use crate::config::AppConfig;
use crate::db::Database;
use crate::mcp::{self, DiscoveredTool};
use crate::models::{Chat, Provider};
use crate::search;
use crate::secrets::Secrets;
use anyhow::{Context as _, Result};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

const MAX_ROUNDS: usize = 10;

pub const RESPONSE_FORMAT: &str = "Format the answer as readable Markdown. Use headings, lists, tables, and fenced code blocks when useful. Keep prose in normal paragraphs; do not wrap the whole answer in a code block.";

const WEB_INSTRUCTION: &str = "Web search is on for this message. Use the web_search tool to check current information before answering, and cite sources inline as [1], [2] using the numbers from the results.";

/// Everything a turn needs that outlives one request.
#[derive(Clone)]
pub struct Context {
    pub db_path: PathBuf,
    pub secrets: Secrets,
    pub config: AppConfig,
}

pub struct TurnSpec {
    pub chat: Chat,
    pub provider: Provider,
    pub model: String,
    pub params: Value,
    pub messages: Vec<Value>,
    /// Offer the web_search tool.
    pub web: bool,
    /// The folder aihat runs in, unless the person said no to it. The
    /// assistant can ask to read it and to run commands there.
    pub folder: Option<PathBuf>,
    /// Reading the folder is already allowed; otherwise the first read asks.
    pub folder_allowed: bool,
}

/// What is waiting for the user's answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalKind {
    /// An MCP tool that writes data.
    Tool,
    /// A command the assistant wants to run in the folder.
    Command,
    /// The assistant's first look at the folder's files.
    ReadFolder,
}

/// A write tool, a command, or reading the folder, waiting for the user's
/// answer.
#[derive(Clone, Debug)]
pub struct Approval {
    pub server: String,
    pub tool: String,
    pub writes: bool,
    pub arguments: Value,
    /// The folder a command runs in, or that would be read.
    pub folder: Option<String>,
    pub kind: ApprovalKind,
}

#[derive(Clone, Debug)]
pub enum Decision {
    Once,
    AlwaysForServer,
    Edited(Value),
    Deny,
}

/// Receives progress while a turn runs.
pub trait Sink {
    fn reasoning(&mut self, _delta: &str) {}
    fn text(&mut self, _delta: &str) {}
    fn tool_started(&mut self, _label: &ToolLabel) {}
    fn tool_finished(&mut self, _log: &Value) {}
    fn warning(&mut self, _text: &str) {}
    /// An MCP server needs a sign-in: the browser was opened at `url`.
    fn signing_in(&mut self, _server: &str, _url: &str) {}
    fn approve(&mut self, request: &Approval) -> Decision;
}

/// How a tool call is shown: `● web_search "query"`.
#[derive(Clone, Debug)]
pub struct ToolLabel {
    pub name: String,
    pub detail: String,
}

#[derive(Clone, Debug, Default)]
pub struct TurnOutput {
    pub content: String,
    pub reasoning: String,
    pub completion_tokens: Option<u64>,
    pub prompt_tokens: Option<u64>,
    pub elapsed_ms: u64,
    pub tool_logs: Vec<Value>,
    pub citations: Vec<Value>,
    pub cancelled: bool,
    pub searches: usize,
    pub pages_read: usize,
}

impl TurnOutput {
    /// The text saved in the database: reasoning in `<think>` tags, like the app.
    pub fn stored_content(&self) -> String {
        if self.reasoning.trim().is_empty() {
            self.content.clone()
        } else {
            format!("<think>{}</think>{}", self.reasoning, self.content)
        }
    }
}

/// Split saved content into (reasoning, answer).
/// The words of the last message the user sent (without attachments).
pub fn last_user_text(messages: &[Value]) -> String {
    let Some(last) = messages.iter().rev().find(|m| m.get("role").and_then(Value::as_str) == Some("user")) else {
        return String::new();
    };
    match last.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Who the model is and who made the app, so "who made you?" gets both answers.
fn identity(model: &str, provider: Option<&str>) -> String {
    let reached = match provider {
        Some(provider) => format!("through {provider}"),
        None => "through the user's own provider".to_string(),
    };
    let model_line = if model.is_empty() || model.eq_ignore_ascii_case("auto") {
        format!("You are an AI model reached {reached}, with the model setting \"{}\", so the exact model can change from message to message.", if model.is_empty() { "default" } else { model })
    } else {
        format!("You are the model \"{model}\", reached {reached}.")
    };
    format!(
        "{model_line} The user is talking to you in AI Hat CLI, the terminal version of AI Hat, an AI chat app made by Aman Mohammed (amanm.space, github.com/aman-mohammed-max). \
If the user asks who made you, who you are, or what app this is, answer both parts: which model you are and the company or team that made that model (only what you actually know), and that the app they are using, AI Hat, was made by Aman Mohammed."
    )
}

/// What the model is told when it writes a system prompt (as in the app).
const PROMPT_WRITER: &str = "You write system prompts for AI assistants. Given a short description of the assistant someone wants, write a complete, well-structured system prompt in the second person (\"You are…\"): role, tone, what to do, what to avoid, and how to format answers. 80 to 250 words. Do not mention that you are writing a prompt.\n\nReply in exactly this shape:\nNAME: <a 2 to 4 word title>\n\n<the system prompt>";

/// Ask the chat model to draft a system prompt from a description, like
/// "a patient Rust tutor". Returns its name and text.
pub fn write_system_prompt(secrets: &Secrets, provider: &Provider, model: &str, description: &str) -> Result<(String, String)> {
    let request = vec![
        json!({"role": "system", "content": PROMPT_WRITER}),
        json!({"role": "user", "content": description}),
    ];
    let reply = api::complete(provider, secrets, model, &request, &json!({"temperature": 0.7, "maxTokens": 700}))?;
    parse_written_prompt(&reply.content, description)
}

/// "NAME: Rust Tutor\n\nYou are…" → ("Rust Tutor", "You are…").
fn parse_written_prompt(raw: &str, description: &str) -> Result<(String, String)> {
    let (_, answer) = split_reasoning(raw);
    let text = answer.trim();
    let named = text.get(..5).is_some_and(|head| head.eq_ignore_ascii_case("NAME:"));
    let (name, body) = if named {
        let (first, rest) = text.split_once('\n').unwrap_or((text, ""));
        (first[5..].to_string(), rest.trim().to_string())
    } else {
        (description.to_string(), text.to_string())
    };
    let name: String = name.chars().filter(|c| !"*\"`".contains(*c)).collect::<String>().trim().chars().take(40).collect();
    if body.is_empty() {
        anyhow::bail!("The model did not write a prompt. Try a more specific description.");
    }
    Ok((if name.is_empty() { "New prompt".into() } else { name }, body))
}

pub fn split_reasoning(content: &str) -> (String, String) {
    let mut reasoning = String::new();
    let mut answer = String::new();
    let mut rest = content;
    loop {
        let Some(start) = rest.find("<think>") else {
            answer.push_str(rest);
            break;
        };
        answer.push_str(&rest[..start]);
        let thought = &rest[start + "<think>".len()..];
        let Some(end) = thought.find("</think>") else {
            reasoning.push_str(thought);
            break;
        };
        reasoning.push_str(&thought[..end]);
        rest = &thought[end + "</think>".len()..];
    }
    (reasoning.trim().to_string(), answer.trim_start().to_string())
}

/// The system prompt for a chat: the chosen prompt, chat notes, earlier-context
/// summary, skills, memory and the Markdown instruction.
pub fn system_prompt(db: &Database, chat: &Chat, model: &str) -> String {
    let settings = db.settings();
    let prompts = db.prompts().unwrap_or_default();
    let prompt_id = chat
        .prompt_id
        .clone()
        .or_else(|| settings.get("defaultPromptId").and_then(Value::as_str).map(str::to_owned));
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let mut sections: Vec<String> = Vec::new();
    let provider = chat.provider_id.as_deref().and_then(|id| db.provider(id).ok().flatten());
    sections.push(identity(model, provider.as_ref().map(|p| p.name.as_str())));
    if let Some(prompt) = prompt_id.and_then(|id| prompts.into_iter().find(|p| p.id == id)) {
        sections.push(prompt.body.replace("{model}", model).replace("{date}", &today));
    }
    if let Some(notes) = settings
        .get("chatNotes")
        .and_then(|all| all.get(&chat.id))
        .and_then(Value::as_array)
        .filter(|notes| !notes.is_empty())
    {
        let mut text = String::from("User notes for this conversation:");
        for note in notes.iter().filter_map(Value::as_str) {
            text.push_str("\n- ");
            text.push_str(note);
        }
        sections.push(text);
    }
    if let Some(summary) = compaction(&settings, &chat.id)
        .and_then(|entry| entry.get("summary"))
        .and_then(Value::as_str)
        .filter(|summary| !summary.trim().is_empty())
    {
        sections.push(format!("Earlier conversation summary:\n{summary}"));
    }
    let skills: Vec<_> = db
        .skills()
        .unwrap_or_default()
        .into_iter()
        .filter(|skill| skill.enabled)
        .collect();
    let always: Vec<_> = skills.iter().filter(|skill| skill.mode == "always").collect();
    if !always.is_empty() {
        let mut text = String::from("Always follow these enabled skills when they apply:");
        for skill in always {
            text.push_str(&format!("\n\n## {}\n{}", skill.name, skill.body));
        }
        sections.push(text);
    }
    let automatic: Vec<(String, String)> = skills
        .iter()
        .filter(|skill| skill.mode == "auto")
        .map(|skill| (skill.name.clone(), skill.description.clone()))
        .chain(crate::skills::active_globals(&db.skills().unwrap_or_default()).into_iter().map(|g| (g.name, g.description)))
        .collect();
    if !automatic.is_empty() {
        let mut text = String::from(
            "Available skills are task-specific instructions. When one matches the user's request, call use_skill with its exact name, then follow the returned instructions. Do not load unrelated skills.",
        );
        for (name, description) in automatic {
            let description: String = description.replace('\n', " ").chars().take(280).collect();
            text.push_str(&format!("\n- {name}: {description}"));
        }
        sections.push(text);
    }
    let memory = crate::memory::context(db, chat);
    if !memory.is_empty() {
        sections.push(memory);
    }
    sections.push(RESPONSE_FORMAT.to_string());
    sections.join("\n\n")
}

fn compaction<'a>(settings: &'a Value, chat_id: &str) -> Option<&'a Value> {
    settings.get("chatCompactions").and_then(|all| all.get(chat_id))
}

/// The request messages for a chat: system prompt, then the saved history
/// after any compaction point.
pub fn build_messages(db: &Database, chat: &Chat, model: &str) -> Result<Vec<Value>> {
    let settings = db.settings();
    let mut messages = vec![json!({"role": "system", "content": system_prompt(db, chat, model)})];
    let cutoff = compaction(&settings, &chat.id)
        .and_then(|entry| entry.get("through"))
        .and_then(Value::as_i64);
    for message in db.messages(&chat.id)? {
        if cutoff.is_some_and(|cutoff| message.created_at <= cutoff) {
            continue;
        }
        match message.role.as_str() {
            "assistant" => {
                let (_, answer) = split_reasoning(&message.content);
                crate::db::append_assistant_history(&mut messages, &message, json!(answer));
            }
            "user" => messages.push(json!({
                "role": "user",
                "content": crate::files::saved_message_content(&message.content, &message.attachments)
            })),
            "system" => messages.push(json!({"role": "system", "content": message.content})),
            _ => {}
        }
    }
    Ok(messages)
}

/// A rough token count for the context meter when the provider has not told us.
pub fn estimate_tokens(messages: &[Value]) -> u64 {
    let chars: usize = messages
        .iter()
        .map(|message| match message.get("content") {
            Some(Value::String(text)) => text.len(),
            Some(other) => other.to_string().len(),
            None => 0,
        })
        .sum();
    (chars / 4) as u64
}

pub fn run_turn(
    ctx: &Context,
    mut spec: TurnSpec,
    cancel: &AtomicBool,
    sink: &mut dyn Sink,
) -> Result<TurnOutput> {
    let db = Database::open(&ctx.db_path).context("opening the AI Hat database")?;
    let started = Instant::now();
    let settings = db.settings();
    let supports_tools = db
        .model(&spec.provider.id, &spec.model)
        .ok()
        .flatten()
        .map(|model| model.supports_tools())
        .unwrap_or(true);

    let mut mcp_tools: Vec<DiscoveredTool> = Vec::new();
    let mut definitions: Vec<Value> = Vec::new();
    if supports_tools {
        if !spec.chat.tool_ids.is_empty() {
            let (tools, warnings) =
                mcp::discover(&db, &ctx.secrets, &spec.chat.tool_ids, cancel, &mut |server, url| sink.signing_in(server, url));
            for warning in warnings {
                sink.warning(&warning);
            }
            definitions.extend(tools.iter().map(|tool| tool.definition.clone()));
            mcp_tools = tools;
        }
        let memory_on = !spec.chat.temporary
            && spec.chat.memory_enabled
            && settings.get("memoryEnabled").and_then(Value::as_bool).unwrap_or(true);
        if memory_on {
            definitions.push(crate::memory::tool_definition());
        }
        let skills = db.skills().unwrap_or_default();
        let globals = crate::skills::active_globals(&skills);
        let auto_skills: Vec<String> = skills
            .iter()
            .filter(|skill| skill.enabled && skill.mode == "auto")
            .map(|skill| skill.name.clone())
            .chain(globals.iter().map(|g| g.name.clone()))
            .collect();
        if !auto_skills.is_empty() {
            definitions.push(skill_tool(&auto_skills));
        }
        if !globals.is_empty()
            || skills
                .iter()
                .any(|skill| skill.enabled && matches!(skill.mode.as_str(), "auto" | "always"))
        {
            definitions.push(skill_file_tool());
        }
        let search_auto = settings.get("searchMode").and_then(Value::as_str) == Some("auto");
        if spec.web || spec.chat.search_mode || search_auto {
            definitions.push(search::tool_definition());
        }
        if let Some(dir) = &spec.folder {
            definitions.extend(crate::folder::tool_definitions());
            append_system(&mut spec.messages, &crate::folder::instruction(dir, spec.folder_allowed));
        }
    }
    if spec.web {
        if supports_tools {
            append_system(&mut spec.messages, WEB_INSTRUCTION);
        } else {
            // The model cannot call tools: search with the question itself.
            presearch(ctx, &db, &mut spec.messages, sink)?;
        }
    }

    let mut output = TurnOutput::default();
    let mut folder_allowed = spec.folder_allowed;
    for _round in 0..MAX_ROUNDS {
        // Text after a tool call starts a new paragraph, before its first word.
        let mut need_break = !output.content.is_empty() && !output.content.ends_with('\n');
        let mut on_event = |event: StreamEvent<'_>| match event {
            StreamEvent::Reasoning(delta) => sink.reasoning(delta),
            StreamEvent::Text(delta) => {
                if need_break && !delta.is_empty() {
                    sink.text("\n\n");
                    need_break = false;
                }
                sink.text(delta)
            }
        };
        let reply = api::chat(
            &spec.provider,
            &ctx.secrets,
            &spec.model,
            &spec.messages,
            &spec.params,
            &definitions,
            cancel,
            &mut on_event,
        )?;
        if !reply.content.is_empty() {
            if !output.content.is_empty() && !output.content.ends_with('\n') {
                output.content.push_str("\n\n");
            }
            output.content.push_str(&reply.content);
        }
        if !reply.reasoning.is_empty() {
            if !output.reasoning.is_empty() {
                output.reasoning.push_str("\n\n");
            }
            output.reasoning.push_str(&reply.reasoning);
        }
        output.completion_tokens = sum(output.completion_tokens, reply.completion_tokens);
        output.prompt_tokens = reply.prompt_tokens.or(output.prompt_tokens);
        if reply.cancelled || cancel.load(Ordering::Relaxed) {
            output.cancelled = true;
            break;
        }
        if reply.tool_calls.is_empty() {
            break;
        }
        spec.messages.push(json!({
            "role": "assistant",
            "content": if reply.content.is_empty() { Value::Null } else { json!(reply.content) },
            "tool_calls": reply.tool_calls,
        }));
        for call in &reply.tool_calls {
            if cancel.load(Ordering::Relaxed) {
                output.cancelled = true;
                break;
            }
            let folder = spec.folder.as_deref().map(|dir| (dir, &mut folder_allowed));
            let log = run_tool(ctx, &db, &spec.chat, folder, &mcp_tools, call, &mut output, cancel, sink);
            spec.messages.push(json!({
                "role": "tool",
                "tool_call_id": log["tool_call_id"],
                "content": log["result"],
            }));
            sink.tool_finished(&log);
            output.tool_logs.push(log);
        }
        if output.cancelled {
            break;
        }
    }
    output.elapsed_ms = started.elapsed().as_millis() as u64;
    Ok(output)
}

fn sum(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a + b),
        (a, b) => a.or(b),
    }
}

fn append_system(messages: &mut Vec<Value>, text: &str) {
    if let Some(system) = messages
        .iter_mut()
        .find(|message| message.get("role").and_then(Value::as_str) == Some("system"))
    {
        let old = system["content"].as_str().unwrap_or("").to_string();
        system["content"] = json!(format!("{old}\n\n{text}"));
    } else {
        messages.insert(0, json!({"role": "system", "content": text}));
    }
}

/// Search with the user's last message and put the results in the prompt.
fn presearch(ctx: &Context, db: &Database, messages: &mut Vec<Value>, sink: &mut dyn Sink) -> Result<()> {
    let query = messages
        .iter()
        .rev()
        .find(|message| message["role"] == "user")
        .and_then(|message| message["content"].as_str())
        .unwrap_or("")
        .chars()
        .take(300)
        .collect::<String>();
    if query.trim().is_empty() {
        return Ok(());
    }
    sink.tool_started(&ToolLabel {
        name: "web_search".into(),
        detail: format!("\"{}\"", query.trim()),
    });
    let started = Instant::now();
    let result = search::search_with_metrics(db, &ctx.secrets, &ctx.config, &query);
    let mut log = json!({"name": "web_search", "args": {"query": query}, "elapsedMs": started.elapsed().as_millis() as u64});
    match result {
        Ok(found) => {
            log["status"] = json!("done");
            log["resultCount"] = json!(found.hits.len());
            log["pagesRead"] = json!(found.pages_read);
            append_system(
                messages,
                &format!(
                    "Use these current web search results to answer. Cite sources as [1], [2].\n\n{}",
                    search::format_hits(&found.hits)
                ),
            );
        }
        Err(error) => {
            log["status"] = json!("error");
            log["result"] = json!(format!("Search failed: {error:#}"));
        }
    }
    sink.tool_finished(&log);
    Ok(())
}

fn skill_tool(names: &[String]) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "use_skill",
            "description": "Load the full instructions of an installed skill when it matches the user's request.",
            "parameters": {
                "type": "object",
                "properties": {"name": {"type": "string", "enum": names}},
                "required": ["name"]
            }
        }
    })
}

fn skill_file_tool() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "read_skill_file",
            "description": "Read a reference file that belongs to an enabled skill, by its relative path.",
            "parameters": {
                "type": "object",
                "properties": {
                    "skill": {"type": "string"},
                    "path": {"type": "string"}
                },
                "required": ["skill", "path"]
            }
        }
    })
}

/// A short human label for a tool call.
pub fn tool_label(name: &str, args: &Value, mcp_tools: &[DiscoveredTool]) -> ToolLabel {
    if let Some(found) = mcp::find_tool(mcp_tools, name) {
        return ToolLabel {
            name: format!("{} · {}", found.server.name, found.tool.name),
            detail: argument_summary(args),
        };
    }
    ToolLabel {
        name: name.to_string(),
        detail: argument_summary(args),
    }
}

/// The most telling argument of a call: a query, a path, or compact JSON.
pub fn argument_summary(args: &Value) -> String {
    for key in ["command", "query", "q", "name", "path", "url", "title", "action"] {
        if let Some(value) = args.get(key).and_then(Value::as_str) {
            return format!("\"{}\"", crate::ui::text::truncate(value, 80));
        }
    }
    match args {
        Value::Object(map) if map.is_empty() => String::new(),
        Value::Null => String::new(),
        other => crate::ui::text::truncate(&other.to_string(), 80),
    }
}

#[allow(clippy::too_many_arguments)]
fn run_tool(
    ctx: &Context,
    db: &Database,
    chat: &Chat,
    folder: Option<(&std::path::Path, &mut bool)>,
    mcp_tools: &[DiscoveredTool],
    call: &Value,
    output: &mut TurnOutput,
    cancel: &AtomicBool,
    sink: &mut dyn Sink,
) -> Value {
    let name = call.pointer("/function/name").and_then(Value::as_str).unwrap_or("").to_string();
    let raw_args = call.pointer("/function/arguments").and_then(Value::as_str).unwrap_or("{}");
    let mut args: Value = serde_json::from_str(raw_args).unwrap_or_else(|_| json!({}));
    let call_id = call.get("id").and_then(Value::as_str).unwrap_or("").to_string();
    let label = tool_label(&name, &args, mcp_tools);
    sink.tool_started(&label);
    let started = Instant::now();
    let mut log = json!({
        "tool_call_id": call_id,
        "name": name,
        "label": label.name,
        "detail": label.detail,
        "args": args,
        "status": "done",
    });
    let result: Result<String, String> = match name.as_str() {
        crate::memory::TOOL_NAME => crate::memory::apply(db, chat, &args.to_string()).map_err(|e| e.to_string()),
        "use_skill" => {
            let wanted = args.get("name").and_then(Value::as_str).unwrap_or("").trim().to_string();
            let library = db.skills().unwrap_or_default();
            library
                .iter()
                .find(|skill| skill.enabled && skill.mode == "auto" && skill.name.eq_ignore_ascii_case(&wanted))
                .map(|skill| skill.body.chars().take(60_000).collect())
                .or_else(|| {
                    // A skill another agent installed: its files are read from its folder.
                    crate::skills::active_globals(&library)
                        .into_iter()
                        .find(|g| g.name.eq_ignore_ascii_case(&wanted))
                        .map(|g| format!("{}\n\n(This skill's files are in {}; read them with read_skill_file.)", g.body.chars().take(60_000).collect::<String>(), g.place))
                })
                .ok_or_else(|| format!("No enabled skill named {wanted:?}."))
        }
        "read_skill_file" => read_skill_file(db, &args),
        "list_files" | "read_file" | "search_files" => match folder {
            Some((root, allowed)) => {
                // The first look at the folder asks the user.
                if !*allowed {
                    let request = Approval {
                        server: "AI Hat".into(),
                        tool: name.clone(),
                        writes: false,
                        arguments: args.clone(),
                        folder: Some(crate::ui::text::home_relative(root)),
                        kind: ApprovalKind::ReadFolder,
                    };
                    match sink.approve(&request) {
                        Decision::Once | Decision::Edited(_) => *allowed = true,
                        Decision::AlwaysForServer => {
                            *allowed = true;
                            if let Err(error) = crate::folder::set_access(db, root, Some(crate::folder::Access::Allowed)) {
                                sink.warning(&format!("Could not save the permission: {error}"));
                            }
                        }
                        Decision::Deny => {}
                    }
                }
                if *allowed {
                    crate::folder::read_tool(root, &name, &args)
                } else {
                    log["status"] = json!("rejected");
                    Ok("The user didn't allow reading this folder. Don't try again unless they ask; they can allow it with /folder allow.".into())
                }
            }
            None => Err("The user hasn't allowed reading this folder.".into()),
        },
        "run_command" => match folder {
            Some((root, _)) => {
                let mut command = args.get("command").and_then(Value::as_str).unwrap_or("").trim().to_string();
                let mut allowed = crate::folder::allowed_commands(db, root).contains(&command);
                if !allowed && !command.is_empty() {
                    let request = Approval {
                        server: "AI Hat".into(),
                        tool: "run_command".into(),
                        writes: true,
                        arguments: args.clone(),
                        folder: Some(crate::ui::text::home_relative(root)),
                        kind: ApprovalKind::Command,
                    };
                    match sink.approve(&request) {
                        Decision::Once => allowed = true,
                        Decision::AlwaysForServer => {
                            allowed = true;
                            if let Err(error) = crate::folder::allow_command(db, root, &command) {
                                sink.warning(&format!("Could not save the approval: {error}"));
                            }
                        }
                        Decision::Edited(edited) => {
                            command = edited.get("command").and_then(Value::as_str).unwrap_or("").trim().to_string();
                            args = edited;
                            log["args"] = args.clone();
                            log["detail"] = json!(argument_summary(&args));
                            allowed = !command.is_empty();
                        }
                        Decision::Deny => {}
                    }
                }
                if allowed {
                    crate::folder::run_command(root, &command, cancel)
                } else {
                    log["status"] = json!("rejected");
                    Ok("The user declined to run this command.".into())
                }
            }
            None => Err("The user hasn't allowed running commands in this folder.".into()),
        },
        "web_search" => {
            let query = args.get("query").and_then(Value::as_str).unwrap_or("").to_string();
            match search::search_with_metrics(db, &ctx.secrets, &ctx.config, &query) {
                Ok(found) => {
                    // Number this search's sources after the earlier ones so
                    // [n] stays unique across the whole reply.
                    let offset = output.citations.len();
                    let text = found
                        .hits
                        .iter()
                        .enumerate()
                        .map(|(i, hit)| format!("[{}] {}\n{}\n{}", offset + i + 1, hit.title, hit.url, hit.snippet))
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    for hit in &found.hits {
                        output
                            .citations
                            .push(json!({"title": hit.title, "url": hit.url, "snippet": hit.snippet}));
                    }
                    output.searches += 1;
                    output.pages_read += found.pages_read;
                    log["resultCount"] = json!(found.hits.len());
                    log["pagesRead"] = json!(found.pages_read);
                    Ok(if text.is_empty() { "No results.".into() } else { text })
                }
                Err(error) => Err(format!("Search failed: {error:#}")),
            }
        }
        _ => match mcp::find_tool(mcp_tools, &name) {
            Some(found) => {
                log["server"] = json!(found.server.name);
                log["tool"] = json!(found.tool.name);
                let mut allowed = true;
                if mcp::call_requires_approval(&found.tool) {
                    let request = Approval {
                        server: found.server.name.clone(),
                        tool: found.tool.name.clone(),
                        writes: found.tool.kind != "read",
                        arguments: args.clone(),
                        folder: None,
                        kind: ApprovalKind::Tool,
                    };
                    match sink.approve(&request) {
                        Decision::Once => {}
                        Decision::AlwaysForServer => {
                            let mut server = found.server.clone();
                            if let Err(error) = mcp::set_server_always_allowed(db, &mut server, true) {
                                sink.warning(&format!("Could not save the approval: {error}"));
                            }
                        }
                        Decision::Edited(edited) => {
                            args = edited;
                            log["args"] = args.clone();
                        }
                        Decision::Deny => allowed = false,
                    }
                }
                if allowed {
                    mcp::call_signing_in(db, &ctx.secrets, &found.server, &found.tool.name, args.clone(), cancel, &mut |server, url| {
                        sink.signing_in(server, url)
                    })
                    .map_err(|e| format!("{e:#}"))
                } else {
                    log["status"] = json!("rejected");
                    Ok("The user declined this tool call.".into())
                }
            }
            None => Err(format!("Unknown tool: {name}")),
        },
    };
    match result {
        Ok(text) => log["result"] = json!(text),
        Err(error) => {
            log["status"] = json!("error");
            log["result"] = json!(error);
        }
    }
    log["elapsedMs"] = json!(started.elapsed().as_millis() as u64);
    log
}

fn read_skill_file(db: &Database, args: &Value) -> Result<String, String> {
    let wanted = args.get("skill").and_then(Value::as_str).unwrap_or("").trim();
    let path = args.get("path").and_then(Value::as_str).unwrap_or("").replace('\\', "/");
    if path.is_empty() || path.starts_with('/') || path.split('/').any(|part| part == "..") {
        return Err("Invalid skill file path.".into());
    }
    let skill = db
        .skills()
        .ok()
        .and_then(|skills| {
            skills.into_iter().find(|skill| {
                skill.enabled
                    && matches!(skill.mode.as_str(), "auto" | "always")
                    && skill.name.eq_ignore_ascii_case(wanted)
            })
        })
        .ok_or_else(|| format!("No enabled skill named {wanted:?}."));
    let skill = match skill {
        Ok(skill) => skill,
        Err(missing) => {
            let library = db.skills().unwrap_or_default();
            let global = crate::skills::active_globals(&library)
                .into_iter()
                .find(|g| g.name.eq_ignore_ascii_case(wanted))
                .ok_or(missing)?;
            return crate::skills::global_file(&global, &path).map_err(|e| e.to_string());
        }
    };
    db.conn
        .query_row(
            "SELECT content FROM skill_files WHERE skill_id=?1 AND path=?2",
            rusqlite::params![skill.id, path],
            |row| row.get::<_, String>(0),
        )
        .map(|content| content.chars().take(100_000).collect())
        .map_err(|_| format!("No file {path:?} in skill {wanted:?}."))
}

/// Summarise everything before the last two exchanges so long chats fit the
/// model's context. The full transcript stays in the database.
pub fn compact(ctx: &Context, db: &Database, chat: &Chat, provider: &Provider, model: &str) -> Result<Option<String>> {
    let messages = db.messages(&chat.id)?;
    let keep_from = messages
        .iter()
        .enumerate()
        .filter(|(_, message)| message.role == "user")
        .map(|(index, _)| index)
        .rev()
        .nth(1)
        .unwrap_or(0);
    if keep_from == 0 {
        return Ok(None);
    }
    let through = messages[keep_from - 1].created_at;
    let mut transcript = String::new();
    for message in &messages[..keep_from] {
        let (_, answer) = split_reasoning(&message.content);
        transcript.push_str(&format!("{}: {}\n\n", message.role, answer.trim()));
    }
    let request = vec![
        json!({"role": "system", "content": "Summarise this conversation so it can continue without the full transcript. Keep decisions, facts, names, code identifiers, open questions and the user's preferences. Write compact Markdown bullet points."}),
        json!({"role": "user", "content": transcript.chars().take(200_000).collect::<String>()}),
    ];
    let reply = api::complete(provider, &ctx.secrets, model, &request, &json!({"temperature": 0.2}))?;
    let summary = reply.content.trim().to_string();
    if summary.is_empty() {
        anyhow::bail!("the model returned an empty summary");
    }
    let mut settings = db.settings();
    if !settings.get("chatCompactions").is_some_and(Value::is_object) {
        settings["chatCompactions"] = json!({});
    }
    settings["chatCompactions"][&chat.id] = json!({"through": through, "summary": summary});
    db.set_kv("settings", &settings.to_string())?;
    Ok(Some(summary))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reasoning_is_split_from_the_answer() {
        let (reasoning, answer) = split_reasoning("<think>plan it</think>Here you go.");
        assert_eq!(reasoning, "plan it");
        assert_eq!(answer, "Here you go.");
        let (reasoning, answer) = split_reasoning("<think>still thinking");
        assert_eq!(reasoning, "still thinking");
        assert_eq!(answer, "");
    }

    #[test]
    fn written_prompts_are_named() {
        let (name, body) = parse_written_prompt("<think>hm</think>NAME: **Rust Tutor**\n\nYou are a patient tutor.", "x").unwrap();
        assert_eq!(name, "Rust Tutor");
        assert_eq!(body, "You are a patient tutor.");
        let (name, body) = parse_written_prompt("You are kind.", "a kind helper").unwrap();
        assert_eq!((name.as_str(), body.as_str()), ("a kind helper", "You are kind."));
        assert!(parse_written_prompt("NAME: Empty\n", "x").is_err());
    }

    #[test]
    fn stored_content_round_trips() {
        let output = TurnOutput {
            content: "Answer".into(),
            reasoning: "Because".into(),
            ..TurnOutput::default()
        };
        assert_eq!(split_reasoning(&output.stored_content()), ("Because".into(), "Answer".into()));
    }

    #[test]
    fn argument_summary_prefers_the_query() {
        assert_eq!(argument_summary(&json!({"query": "fabric api", "n": 3})), "\"fabric api\"");
        assert_eq!(argument_summary(&json!({})), "");
    }
}
