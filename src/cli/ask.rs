//! `aihat "prompt"`: ask once, print the reply, exit.
//!
//! The reply goes to stdout; progress, sources hints and stats go to stderr,
//! so `aihat "…" > answer.md` saves only the answer. When stdout is not a
//! terminal the reply is printed as plain Markdown with no colours.

use super::{EXIT_AUTH, EXIT_ERROR, EXIT_STOPPED, EXIT_UNREACHABLE, Env, Failure};
use crate::api::ErrorKind;
use crate::engine::{self, Approval, Decision, ToolLabel, TurnSpec};
use crate::files::Attachment;
use crate::models::{Chat, Provider};
use crate::ui::ansi::{self, paint};
use crate::ui::markdown::{self, MdOptions};
use crate::ui::text;
use crate::ui::theme::{Role, theme};
use anyhow::Result;
use ratatui::text::Span;
use serde_json::{Value, json};
use std::io::{IsTerminal, Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub struct Options {
    pub prompt: String,
    pub files: Vec<PathBuf>,
    pub tools: Vec<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub system: Option<String>,
    pub web: bool,
    pub json: bool,
    pub stream: bool,
    pub continue_chat: bool,
}

static CANCEL: AtomicBool = AtomicBool::new(false);
static PRESSES: AtomicUsize = AtomicUsize::new(0);

const PROSE_WIDTH: usize = 78;

pub fn run(env: &mut Env, options: Options) -> Result<i32> {
    let _ = ctrlc::set_handler(|| {
        CANCEL.store(true, Ordering::SeqCst);
        if PRESSES.fetch_add(1, Ordering::SeqCst) >= 1 {
            std::process::exit(EXIT_STOPPED);
        }
    });
    let stdout_tty = std::io::stdout().is_terminal();
    let stderr_tty = std::io::stderr().is_terminal();
    let t = theme();

    // Piped input is attached to the message.
    let mut piped = String::new();
    if !std::io::stdin().is_terminal() {
        std::io::stdin().read_to_string(&mut piped)?;
    }
    let mut prompt = options.prompt.trim().to_string();
    if prompt.is_empty() {
        if piped.trim().is_empty() && options.files.is_empty() {
            return Err(Failure::new(super::EXIT_USAGE, "Nothing to ask")
                .note("Pass a prompt, pipe some text, or run aihat in a terminal for the full chat.")
                .into());
        }
        prompt = "Read the attached input.".into();
    }

    let continued = if options.continue_chat {
        Some(
            env.db
                .chats(false)?
                .into_iter()
                .next()
                .ok_or_else(|| Failure::new(EXIT_ERROR, "There is no chat to continue yet"))?,
        )
    } else {
        None
    };
    let (provider, model) = resolve(env, &options, continued.as_ref())?;
    let prompt_id = match &options.system {
        Some(name) => Some(find_prompt(env, name)?),
        None => None,
    };

    let mut attachments: Vec<Attachment> = Vec::new();
    for path in &options.files {
        let file = crate::files::read(path).map_err(|error| Failure::new(EXIT_ERROR, format!("Could not attach {}: {error:#}", path.display())))?;
        attachments.push(file);
    }
    if !piped.trim().is_empty() {
        let lines = piped.lines().count();
        attachments.push(Attachment {
            name: "stdin".into(),
            kind: "file".into(),
            mime: "text/plain".into(),
            text: Some(piped.clone()),
            data_uri: None,
            meta: text::plural(lines, "line", "lines"),
        });
    }
    if stderr_tty && !options.json {
        for attachment in &attachments {
            let name = if attachment.name == "stdin" { "Attached stdin".to_string() } else { format!("Attached {}", attachment.name) };
            eprintln!("{}", paint(&format!("{name} · {}", attachment.meta), t.fg(Role::Dim)));
        }
    }

    let chat = match continued {
        Some(chat) => {
            if chat.provider_id.as_deref() != Some(provider.id.as_str()) || chat.model_id.as_deref() != Some(model.as_str()) {
                env.db.patch_chat_selection(&chat.id, &provider.id, &model)?;
            }
            chat
        }
        None => {
            let chat = env.db.create_chat(Some((&provider.id, &model)))?;
            let default_prompt = env.db.settings().get("defaultPromptId").and_then(Value::as_str).map(str::to_owned);
            if let Some(id) = prompt_id.clone().or(default_prompt) {
                env.db.set_chat_prompt(&chat.id, Some(&id))?;
            }
            chat
        }
    };
    if let Some(id) = &prompt_id {
        env.db.set_chat_prompt(&chat.id, Some(id))?;
    }
    if !options.tools.is_empty() {
        let servers = env.db.mcp_servers()?;
        let mut ids = Vec::new();
        for name in &options.tools {
            let server = servers
                .iter()
                .find(|s| s.name.eq_ignore_ascii_case(name) || s.id == *name)
                .ok_or_else(|| Failure::new(EXIT_ERROR, format!("No MCP server named {name:?}")).note("List them: aihat mcp"))?;
            ids.push(server.id.clone());
        }
        env.db.set_chat_tools(&chat.id, &ids)?;
    }
    let chat = env.db.chat(&chat.id)?.unwrap_or(chat);
    let is_new = env.db.messages(&chat.id)?.is_empty();

    let mut messages = engine::build_messages(&env.db, &chat, &model)?;
    messages.push(json!({"role": "user", "content": crate::files::message_content(&prompt, &attachments)}));
    let metadata = json!(attachments.iter().map(Attachment::metadata).collect::<Vec<_>>());
    env.db.save_message(&chat.id, "user", &prompt, None, None, &metadata, &json!([]), &json!([]))?;

    let ctx = engine::Context {
        db_path: env.paths.db_file.clone(),
        secrets: env.secrets.clone(),
        config: env.config.clone(),
    };
    let spec = TurnSpec {
        params: chat.params.clone(),
        chat: chat.clone(),
        provider: provider.clone(),
        model: model.clone(),
        messages,
        web: options.web,
    };
    let width = crossterm::terminal::size().map(|(w, _)| w as usize).unwrap_or(80);
    let mut sink = CliSink::new(stdout_tty, stderr_tty && !options.json, options.json || !options.stream, env.yes, width);
    let result = engine::run_turn(&ctx, spec, &CANCEL, &mut sink);
    sink.spinner.stop();
    let output = match result {
        Ok(output) => output,
        Err(error) => {
            sink.finish_text();
            return Err(explain(env, &provider, &model, &options, error));
        }
    };
    let cancelled = output.cancelled || CANCEL.load(Ordering::SeqCst);
    let parsed = crate::genui::parse_reply(&output.content, None);
    env.db.save_message_with_ui(
        &chat.id,
        "assistant",
        &output.stored_content(),
        output.completion_tokens,
        Some(output.elapsed_ms),
        &json!([]),
        &json!(output.citations),
        &json!(output.tool_logs),
        parsed.spec.as_ref(),
    )?;
    let number = env.db.chat_number(&chat.id).unwrap_or(0);
    // Models without tools learn memories after the reply, as in the app.
    if !cancelled
        && let Some(note) = crate::memory::learn_from_turn(&env.db, &env.secrets, &chat, &provider, &model, &prompt, &output.content)
        && std::io::stderr().is_terminal()
        && !options.json
    {
        sink.finish_text();
        eprintln!("{} {}", paint("◇", theme().fg(Role::Accent)), paint(&note, theme().fg(Role::Dim)));
    }

    if options.json {
        let value = json!({
            "reply": output.content,
            "reasoning": if output.reasoning.is_empty() { Value::Null } else { json!(output.reasoning) },
            "usage": {
                "prompt_tokens": output.prompt_tokens,
                "completion_tokens": output.completion_tokens,
                "model": model,
                "provider": provider.name,
            },
            "sources": output.citations,
            "tools": output.tool_logs,
            "chat": number,
            "chat_id": chat.id,
            "ms": output.elapsed_ms,
            "stopped": cancelled,
        });
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(if cancelled { EXIT_STOPPED } else { 0 });
    }

    if !options.stream {
        sink.render_all(&output.content);
    }
    sink.finish_text();
    if cancelled {
        eprintln!();
        eprintln!(
            "{} {}",
            paint("■ Stopped.", t.fg(Role::Warn)),
            paint(&format!("Partial reply saved to chat #{number} · aihat -c to continue"), t.fg(Role::Dim))
        );
        return Ok(EXIT_STOPPED);
    }
    // Piped output is the reply only; sources are in --json.
    if !output.citations.is_empty() && stdout_tty {
        let mut out = std::io::stdout().lock();
        writeln!(out)?;
        writeln!(out, "{}", paint("Sources", t.fg(Role::Dim)))?;
        for (index, citation) in output.citations.iter().enumerate() {
            let url = citation.get("url").and_then(Value::as_str).unwrap_or("");
            let title = citation.get("title").and_then(Value::as_str).unwrap_or("");
            let host = text::host(url);
            let mut row = format!("  {} {host}", paint(&format!("[{}]", index + 1), t.fg(Role::Link)));
            if !title.is_empty() {
                row.push_str(&format!(" {} {}", paint("·", t.fg(Role::Dim)), text::truncate(title, 60)));
            }
            writeln!(out, "{row}")?;
        }
    }
    if stderr_tty {
        let mut stats = vec![model.clone(), provider.name.clone()];
        if let Some(tokens) = output.completion_tokens {
            stats.push(format!("{tokens} tokens"));
            if output.elapsed_ms > 0 && output.content.len() > 200 {
                stats.push(format!("{:.0} tok/s", tokens as f64 / (output.elapsed_ms as f64 / 1000.0)));
            }
        }
        stats.push(text::duration(output.elapsed_ms));
        stats.push(format!("chat #{number}"));
        if is_new {
            stats.push("aihat -c to follow up".into());
        }
        eprintln!();
        eprintln!("{}", paint(&stats.join(" · "), t.fg(Role::Dim)));
    }
    Ok(0)
}

/// The provider and model for this prompt, checking that the model exists.
fn resolve(env: &Env, options: &Options, chat: Option<&Chat>) -> Result<(Provider, String)> {
    let provider = match options.provider.as_deref().or(chat.and_then(|c| c.provider_id.as_deref())) {
        Some(name) => super::commands::find_provider(&env.db, name)?,
        None => env.db.default_provider()?.ok_or_else(|| {
            Failure::new(EXIT_ERROR, "No provider yet").detail(vec![
                Span::styled("Add one: ", theme().fg(Role::Dim)),
                Span::styled("aihat providers add", theme().fg(Role::Link)),
            ])
        })?,
    };
    let models = env.db.models(Some(&provider.id))?;
    let chat_model = chat
        .filter(|c| c.provider_id.as_deref() == Some(provider.id.as_str()))
        .and_then(|c| c.model_id.clone());
    let wanted = options.model.clone().or(chat_model);
    let model = match wanted {
        Some(model) => {
            if !models.is_empty() && !models.iter().any(|m| m.id == model) {
                let t = theme();
                let suggestion = models
                    .iter()
                    .filter(|m| m.enabled && m.is_chat())
                    .max_by(|a, b| {
                        strsim::jaro_winkler(&a.id, &model)
                            .partial_cmp(&strsim::jaro_winkler(&b.id, &model))
                            .unwrap_or(std::cmp::Ordering::Equal)
                    })
                    .map(|m| m.id.clone());
                let mut detail = Vec::new();
                if let Some(suggestion) = suggestion {
                    detail.push(Span::styled("Did you mean ", t.fg(Role::Dim)));
                    detail.push(Span::styled(suggestion, t.fg(Role::Text)));
                    detail.push(Span::styled("? ", t.fg(Role::Dim)));
                }
                detail.push(Span::styled("List them: ", t.fg(Role::Dim)));
                detail.push(Span::styled("aihat models", t.fg(Role::Link)));
                return Err(Failure::new(EXIT_ERROR, format!("{} has no model \"{model}\".", provider.name)).detail(detail).into());
            }
            model
        }
        None => match crate::providers::resolve_provider_model(&env.db, Some(&provider.id), None) {
            Ok((_, model)) => model,
            Err(_) => {
                // The list may be stale or the server down: ask it, so a
                // dead server is reported as unreachable, not as "no models".
                if let Err(error) = crate::api::list_models(&provider, &env.secrets) {
                    return Err(explain(env, &provider, "", options, error));
                }
                let _ = crate::providers::refresh_one(&env.db, &env.secrets, Some(&provider.id));
                crate::providers::resolve_provider_model(&env.db, Some(&provider.id), None)
                    .map(|(_, model)| model)
                    .map_err(|_| {
                        Failure::new(EXIT_ERROR, format!("{} has no chat models", provider.name)).detail(vec![
                            Span::styled("Add one by name: ", theme().fg(Role::Dim)),
                            Span::styled(format!("aihat models add MODEL -P {}", cli_name(&provider.name)), theme().fg(Role::Link)),
                        ])
                    })?
            }
        },
    };
    Ok((provider, model))
}

fn find_prompt(env: &Env, name: &str) -> Result<String> {
    let prompts = env.db.prompts()?;
    let wanted = name.to_lowercase();
    prompts
        .iter()
        .find(|p| p.name.to_lowercase() == wanted || p.id == name)
        .or_else(|| prompts.iter().find(|p| p.name.to_lowercase().contains(&wanted)))
        .map(|p| p.id.clone())
        .ok_or_else(|| {
            Failure::new(EXIT_ERROR, format!("No system prompt named \"{name}\""))
                .detail(vec![
                    Span::styled("List them: ", theme().fg(Role::Dim)),
                    Span::styled("aihat prompts", theme().fg(Role::Link)),
                ])
                .into()
        })
}

/// How a provider's name is typed on the command line.
pub fn cli_name(name: &str) -> String {
    if name.contains(' ') {
        format!("\"{name}\"")
    } else {
        name.to_lowercase()
    }
}

/// Turn a provider failure into the design's two- or three-line error.
fn explain(env: &Env, provider: &Provider, model: &str, options: &Options, error: anyhow::Error) -> anyhow::Error {
    let t = theme();
    let Some(api) = crate::api::api_error(&error) else {
        return error;
    };
    match api.kind {
        ErrorKind::Unreachable => {
            let _ = env.db.set_provider_status(&provider.id, "offline", None);
            let mut failure = Failure {
                code: EXIT_UNREACHABLE,
                lines: vec![ratatui::text::Line::from(vec![
                    Span::styled(format!("✗ Can't reach {}", provider.name), t.fg(Role::Error)),
                    Span::styled(format!(" at {}", provider.base_url), t.fg(Role::Dim)),
                ])],
            };
            failure = failure.note(format!("{} {}", api.detail, server_hint(provider)));
            let other = env
                .db
                .providers()
                .unwrap_or_default()
                .into_iter()
                .find(|p| p.id != provider.id && p.status != "offline" && p.status != "rejected");
            let mut spans = Vec::new();
            if let Some(other) = other {
                let prompt: String = options.prompt.chars().take(30).collect();
                spans.push(Span::styled("Try another: ", t.fg(Role::Dim)));
                spans.push(Span::styled(format!("aihat \"{prompt}\" -P {}", cli_name(&other.name)), t.fg(Role::Link)));
                spans.push(Span::raw("   "));
            }
            spans.push(Span::styled("Check all: ", t.fg(Role::Dim)));
            spans.push(Span::styled("aihat providers test", t.fg(Role::Link)));
            failure.detail(spans).into()
        }
        ErrorKind::Auth => {
            let _ = env.db.set_provider_status(&provider.id, "rejected", None);
            Failure::new(EXIT_AUTH, format!("{} rejected the key ({})", provider.name, api.status.unwrap_or(401)))
                .detail(vec![
                    Span::styled("Fix it: ", t.fg(Role::Dim)),
                    Span::styled(format!("aihat providers set-key {}", cli_name(&provider.name)), t.fg(Role::Link)),
                ])
                .into()
        }
        ErrorKind::NotFound => Failure::new(EXIT_ERROR, format!("{} answered 404 for {model}", provider.name))
            .note(api.detail.clone())
            .detail(vec![
                Span::styled("Check the URL ends in /v1: ", t.fg(Role::Dim)),
                Span::styled(format!("aihat providers test {}", cli_name(&provider.name)), t.fg(Role::Link)),
            ])
            .into(),
        ErrorKind::Http => error,
    }
}

/// "Is LM Studio running with its server on?"
fn server_hint(provider: &Provider) -> String {
    let url = provider.base_url.to_lowercase();
    let name = provider.name.to_lowercase();
    if url.contains(":1234") || name.contains("lm studio") {
        "Is LM Studio running with its server on?".into()
    } else if url.contains(":11434") || name.contains("ollama") {
        "Is Ollama running? Start it with: ollama serve".into()
    } else if ["localhost", "127.0.0.1", "192.168.", "10.", "0.0.0.0", ".local"].iter().any(|h| url.contains(h)) {
        "Is the server running and reachable from this machine?".into()
    } else {
        "Check your connection and the base URL.".into()
    }
}

/// Prints progress while the turn runs.
struct CliSink {
    status: bool,
    buffered: bool,
    yes: bool,
    width: usize,
    pending: String,
    printed_blocks: usize,
    raw_output: bool,
    spinner: Spinner,
    started: Instant,
    searches: usize,
    pages: usize,
    tools_done: Vec<String>,
    summary_printed: bool,
    reasoning_started: Option<Instant>,
    reasoning_reported: bool,
}

impl CliSink {
    fn new(pretty: bool, status: bool, buffered: bool, yes: bool, width: usize) -> Self {
        let spinner = Spinner::new(status);
        spinner.set("Thinking…");
        Self {
            status,
            buffered,
            yes,
            width,
            pending: String::new(),
            printed_blocks: 0,
            raw_output: !pretty,
            spinner,
            started: Instant::now(),
            searches: 0,
            pages: 0,
            tools_done: Vec::new(),
            summary_printed: false,
            reasoning_started: None,
            reasoning_reported: false,
        }
    }

    fn options(&self) -> MdOptions {
        MdOptions {
            width: self.width.saturating_sub(1).clamp(20, PROSE_WIDTH),
            indent: 0,
            block_indent: 2,
            wide: self.width.saturating_sub(1).max(20),
        }
    }

    /// `✓ Searched 2 queries · read 6 pages                    1.8s`
    fn print_summary(&mut self) {
        if self.summary_printed {
            return;
        }
        self.summary_printed = true;
        self.spinner.stop();
        if !self.status {
            return;
        }
        let t = theme();
        let elapsed = text::duration(self.started.elapsed().as_millis() as u64);
        let mut parts = Vec::new();
        if self.searches > 0 {
            let mut search = format!("Searched {}", text::plural(self.searches, "query", "queries"));
            if self.pages > 0 {
                search.push_str(&format!(" · read {}", text::plural(self.pages, "page", "pages")));
            }
            parts.push(search);
        }
        parts.extend(self.tools_done.iter().cloned());
        if let Some(started) = self.reasoning_started.filter(|_| !self.reasoning_reported) {
            self.reasoning_reported = true;
            let ms = started.elapsed().as_millis() as u64;
            parts.push(if ms < 1000 { "Thought".to_string() } else { format!("Thought for {}", text::seconds(ms)) });
        }
        if parts.is_empty() {
            return;
        }
        let left = parts.join(" · ");
        let room = self.width.min(84);
        let gap = room.saturating_sub(text::width(&left) + 2 + text::width(&elapsed)).max(2);
        eprintln!("{} {}{}{}", paint("✓", t.fg(Role::Accent)), paint(&left, t.fg(Role::Dim)), " ".repeat(gap), paint(&elapsed, t.fg(Role::Dim)));
        eprintln!();
    }

    /// Print finished Markdown blocks; keep an unfinished one for later.
    fn flush_blocks(&mut self, last: bool) {
        if self.raw_output {
            let mut out = std::io::stdout().lock();
            let _ = write!(out, "{}", self.pending);
            let _ = out.flush();
            self.pending.clear();
            return;
        }
        let split = if last { Some(self.pending.len()) } else { safe_split(&self.pending) };
        let Some(at) = split.filter(|at| *at > 0) else {
            return;
        };
        let chunk: String = self.pending.drain(..at).collect();
        if chunk.trim().is_empty() {
            return;
        }
        let rows = markdown::render(&chunk, self.options());
        if rows.is_empty() {
            return;
        }
        let mut out = std::io::stdout().lock();
        if self.printed_blocks > 0 {
            let _ = writeln!(out);
        }
        let _ = write!(out, "{}", ansi::lines(&rows));
        let _ = out.flush();
        self.printed_blocks += 1;
    }

    fn render_all(&mut self, content: &str) {
        self.pending = content.to_string();
        self.flush_blocks(true);
    }

    fn finish_text(&mut self) {
        self.spinner.stop();
        if self.buffered {
            return;
        }
        self.flush_blocks(true);
        if self.raw_output {
            println!();
        }
    }
}

/// The end of the last complete block: a blank line outside any code fence.
fn safe_split(text: &str) -> Option<usize> {
    let mut fence_open = false;
    let mut offset = 0;
    let mut split = None;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fence_open = !fence_open;
        }
        offset += line.len();
        if !fence_open && line.trim().is_empty() && line.ends_with('\n') {
            split = Some(offset);
        }
    }
    split
}

impl engine::Sink for CliSink {
    fn reasoning(&mut self, _delta: &str) {
        if self.reasoning_started.is_none() {
            self.reasoning_started = Some(Instant::now());
            self.spinner.set("Thinking…");
        }
    }

    fn text(&mut self, delta: &str) {
        if !delta.trim().is_empty() {
            if self.summary_printed {
                // Words again after a tool: the spinner line goes.
                self.spinner.stop();
            } else {
                self.print_summary();
            }
        }
        if self.buffered {
            return;
        }
        self.pending.push_str(delta);
        self.flush_blocks(false);
    }

    fn tool_started(&mut self, label: &ToolLabel) {
        // Finish the words already on screen so the spinner gets its own line.
        if !self.buffered {
            self.flush_blocks(true);
        }
        let what = if label.name == "web_search" {
            format!("Searching {}…", label.detail)
        } else {
            format!("Running {}…", label.name)
        };
        self.spinner.set(&what);
    }

    fn tool_finished(&mut self, log: &Value) {
        let name = log.get("name").and_then(Value::as_str).unwrap_or("");
        if name == "web_search" {
            if log.get("status").and_then(Value::as_str) == Some("done") {
                self.searches += 1;
                self.pages += log.get("pagesRead").and_then(Value::as_u64).unwrap_or(0) as usize;
            }
        } else if !name.is_empty() && name != crate::memory::TOOL_NAME {
            let label = log.get("label").and_then(Value::as_str).unwrap_or(name);
            let status = log.get("status").and_then(Value::as_str).unwrap_or("done");
            self.tools_done.push(match status {
                "rejected" => format!("{label} denied"),
                "error" => format!("{label} failed"),
                _ => format!("Ran {label}"),
            });
        }
        self.spinner.set("Thinking…");
    }

    fn signing_in(&mut self, server: &str, url: &str) {
        if !self.buffered {
            self.flush_blocks(true);
        }
        self.spinner.stop();
        if self.status {
            let t = theme();
            eprintln!("{} {server} needs you to sign in. Opening your browser…", paint("◇", t.fg(Role::Accent)));
            eprintln!(
                "{} {} {}",
                paint("│", t.fg(Role::Dim)),
                paint("If it doesn't open:", t.fg(Role::Dim)),
                paint(&text::truncate(url, 80), t.fg(Role::Link))
            );
        }
        self.spinner.set("Waiting for you to sign in…");
    }

    fn warning(&mut self, text: &str) {
        self.spinner.stop();
        if self.status {
            eprintln!("{} {}", paint("!", theme().fg(Role::Warn)), paint(text, theme().fg(Role::Dim)));
        }
    }

    fn approve(&mut self, request: &Approval) -> Decision {
        if self.yes {
            return Decision::Once;
        }
        self.spinner.stop();
        let t = theme();
        let interactive = std::io::stdin().is_terminal();
        eprintln!(
            "{} {} wants to run {}",
            paint("?", t.fg(Role::Warn)),
            paint(&request.server, t.fg(Role::Strong)),
            paint(&request.tool, t.fg(Role::Link))
        );
        if let Some(map) = request.arguments.as_object() {
            let key_width = map.keys().map(|k| text::width(k)).max().unwrap_or(0).min(14);
            for (key, value) in map {
                let value = value.as_str().map(str::to_owned).unwrap_or_else(|| value.to_string());
                eprintln!("  {} {}", paint(&text::pad(key, key_width), t.fg(Role::Dim)), text::truncate(&value.replace('\n', " "), 100));
            }
        }
        if !interactive {
            eprintln!("{}", paint("  Denied: no terminal to ask. Pass --yes to allow tools that write.", t.fg(Role::Dim)));
            return Decision::Deny;
        }
        eprint!("  {} ", paint("Allow? (y)es once · (a)lways for this server · (N)o", t.fg(Role::Dim)));
        let _ = std::io::stderr().flush();
        let mut answer = String::new();
        let _ = std::io::stdin().read_line(&mut answer);
        let decision = match answer.trim().to_lowercase().as_str() {
            "y" | "yes" => Decision::Once,
            "a" | "always" => Decision::AlwaysForServer,
            _ => Decision::Deny,
        };
        self.spinner.set("Thinking…");
        decision
    }
}

/// A one-line spinner on stderr: `⠋ Searching "fabric api"… 2s`.
pub(crate) struct Spinner {
    state: Arc<Mutex<Option<String>>>,
    enabled: bool,
    alive: Arc<AtomicBool>,
}

impl Spinner {
    pub(crate) fn new(enabled: bool) -> Self {
        let state: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let alive = Arc::new(AtomicBool::new(true));
        if enabled {
            let state = state.clone();
            let alive = alive.clone();
            std::thread::spawn(move || {
                const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
                let started = Instant::now();
                let mut frame = 0;
                while alive.load(Ordering::Relaxed) {
                    if let Ok(guard) = state.lock()
                        && let Some(text) = guard.as_ref() {
                            let t = theme();
                            let line = format!(
                                "\r\x1b[2K{} {} {}",
                                paint(FRAMES[frame % FRAMES.len()], t.fg(Role::Accent)),
                                text,
                                paint(&text::seconds(started.elapsed().as_millis() as u64), t.fg(Role::Dim))
                            );
                            let mut err = std::io::stderr().lock();
                            let _ = write!(err, "{line}");
                            let _ = err.flush();
                        }
                    frame += 1;
                    std::thread::sleep(Duration::from_millis(90));
                }
            });
        }
        Self { state, enabled, alive }
    }

    pub(crate) fn set(&self, text: &str) {
        if let Ok(mut guard) = self.state.lock() {
            *guard = Some(text.to_string());
        }
    }

    pub(crate) fn stop(&self) {
        if let Ok(mut guard) = self.state.lock()
            && guard.take().is_some() && self.enabled {
                let mut err = std::io::stderr().lock();
                let _ = write!(err, "\r\x1b[2K");
                let _ = err.flush();
            }
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        self.stop();
        self.alive.store(false, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::safe_split;

    #[test]
    fn blocks_split_on_blank_lines_outside_code() {
        assert_eq!(safe_split("one\n\ntwo"), Some(5));
        assert_eq!(safe_split("```\na\n\nb"), None);
        assert_eq!(safe_split("```\na\n```\n\nnext"), Some(11));
    }
}
