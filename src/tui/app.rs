//! State and behaviour of the full-screen chat.

use super::composer::Composer;
use super::dialogs::{self, Dialog};
use super::files::FileIndex;
use super::providers::ProvidersPage;
use super::transcript::{self, Layout};
use crate::config::{AppConfig, Paths};
use crate::db::Database;
use crate::engine::{self, Approval, Decision, ToolLabel, TurnOutput, TurnSpec};
use crate::models::{Chat, Message, Provider};
use crate::secrets::Secrets;
use crate::ui::text;
use crate::ui::theme::{Role, theme};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::text::{Line, Span};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

pub enum WorkerMsg {
    Turn(u64, TurnEvent),
    Task(Task),
}

pub enum TurnEvent {
    Reasoning(String),
    Text(String),
    ToolStarted(ToolLabel),
    ToolFinished(Value),
    Warning(String),
    Approval(Approval, Sender<Decision>),
    Done(Result<TurnOutput, String>),
}

/// Results of background work other than chat turns.
pub enum Task {
    ProviderTested {
        provider_id: String,
        result: Result<(u64, usize), String>,
    },
    FormTested {
        token: u64,
        result: Result<(u64, Vec<String>), String>,
    },
    ModelsRefreshed {
        label: String,
        result: Result<crate::providers::RefreshSummary, String>,
    },
    Compacted {
        result: Result<Option<String>, String>,
    },
    Image {
        chat_id: String,
        prompt: String,
        result: Result<(Vec<std::path::PathBuf>, String), String>,
        ms: u64,
    },
    McpRefreshed {
        name: String,
        result: Result<usize, String>,
    },
    Shell {
        command: String,
        result: Result<String, String>,
    },
}

/// Something shown in the chat that is not a saved message.
#[derive(Clone)]
pub struct LocalEntry {
    /// Shown after this many visible messages.
    pub after: usize,
    pub lines: Rc<Vec<Line<'static>>>,
}

pub struct PendingApproval {
    pub request: Approval,
    pub reply: Sender<Decision>,
    pub selection: usize,
}

pub struct ActiveTurn {
    pub id: u64,
    pub chat_id: String,
    pub started: Instant,
    pub content: String,
    pub reasoning: String,
    pub reasoning_ms: Option<u64>,
    pub tools: Vec<(ToolLabel, Instant, Option<Value>)>,
    pub cancel: Arc<AtomicBool>,
    pub approval: Option<PendingApproval>,
    pub editing_args: bool,
}

pub struct Notice {
    pub text: String,
    pub role: Role,
    pub until: Instant,
}

pub enum Page {
    Chat,
    Providers(ProvidersPage),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Popup {
    Commands,
    Attach,
}

pub struct App {
    pub db: Database,
    pub paths: Paths,
    pub config: AppConfig,
    pub secrets: Secrets,
    pub ctx: engine::Context,
    pub chat: Option<Chat>,
    pub messages: Vec<Message>,
    pub composer: Composer,
    pub page: Page,
    pub dialog: Option<Dialog>,
    pub popup_selection: usize,
    pub popup_dismissed: Option<String>,
    pub turn: Option<ActiveTurn>,
    next_turn_id: u64,
    pub notice: Option<Notice>,
    pub local: Vec<LocalEntry>,
    pub scroll: usize,
    pub last_total: usize,
    pub show_reasoning: bool,
    pub clear_before: Option<i64>,
    history: Vec<String>,
    history_index: Option<usize>,
    history_draft: String,
    pub editing_last: Option<i64>,
    pub staged: Vec<crate::files::Attachment>,
    pub recent_selection: usize,
    pub next_model: Option<(String, String)>,
    pub context_tokens: Option<u64>,
    pub tx: Sender<WorkerMsg>,
    pub rx: Receiver<WorkerMsg>,
    last_ctrl_c: Option<Instant>,
    pub quit: bool,
    pub cache: HashMap<String, (Layout, Rc<Vec<Line<'static>>>)>,
    pub files: FileIndex,
    pub width: u16,
    pub busy_task: Option<String>,
    pub form_token: u64,
}

impl App {
    pub fn new(
        db: Database,
        paths: Paths,
        config: AppConfig,
        secrets: Secrets,
        initial_chat: Option<String>,
    ) -> anyhow::Result<Self> {
        let (tx, rx) = mpsc::channel();
        let ctx = engine::Context {
            db_path: paths.db_file.clone(),
            secrets: secrets.clone(),
            config: config.clone(),
        };
        let history = load_history(&paths);
        let mut app = Self {
            show_reasoning: config.show_reasoning,
            db,
            paths,
            config,
            secrets,
            ctx,
            chat: None,
            messages: Vec::new(),
            composer: Composer::default(),
            page: Page::Chat,
            dialog: None,
            popup_selection: 0,
            popup_dismissed: None,
            turn: None,
            next_turn_id: 1,
            notice: None,
            local: Vec::new(),
            scroll: 0,
            last_total: 0,
            clear_before: None,
            history,
            history_index: None,
            history_draft: String::new(),
            editing_last: None,
            staged: Vec::new(),
            recent_selection: 0,
            next_model: None,
            context_tokens: None,
            tx,
            rx,
            last_ctrl_c: None,
            quit: false,
            cache: HashMap::new(),
            files: FileIndex::default(),
            width: 80,
            busy_task: None,
            form_token: 0,
        };
        if let Some(id) = initial_chat {
            app.open_chat(&id)?;
        }
        Ok(app)
    }

    // ---------------------------------------------------------------- notices

    pub fn notify(&mut self, role: Role, text: impl Into<String>) {
        self.notice = Some(Notice {
            text: text.into(),
            role,
            until: Instant::now() + Duration::from_secs(5),
        });
    }

    pub fn ok(&mut self, text: impl Into<String>) {
        self.notify(Role::Accent, format!("✓ {}", text.into()));
    }

    pub fn fail(&mut self, text: impl Into<String>) {
        self.notify(Role::Error, format!("✗ {}", text.into()));
    }

    /// An error that belongs in the conversation, below the last message.
    pub fn chat_error(&mut self, text: &str) {
        let t = theme();
        let mut lines = transcript::prefixed(
            " ✗ ",
            t.fg(Role::Error),
            &[(text.to_string(), t.fg(Role::Error))],
            transcript::MAX_TEXT_WIDTH.min(self.width as usize),
        );
        lines.truncate(6);
        self.push_local(lines);
    }

    pub fn push_local(&mut self, lines: Vec<Line<'static>>) {
        let after = self.visible_messages().len();
        self.local.push(LocalEntry {
            after,
            lines: Rc::new(lines),
        });
        self.scroll = 0;
    }

    // ---------------------------------------------------------------- chats

    pub fn visible_messages(&self) -> Vec<&Message> {
        self.messages
            .iter()
            .filter(|message| matches!(message.role.as_str(), "user" | "assistant"))
            .filter(|message| self.clear_before.is_none_or(|cutoff| message.created_at >= cutoff))
            .collect()
    }

    pub fn is_welcome(&self) -> bool {
        self.turn.is_none() && self.visible_messages().is_empty() && self.local.is_empty()
    }

    pub fn open_chat(&mut self, id: &str) -> anyhow::Result<()> {
        if self.turn.is_some() {
            anyhow::bail!("wait for the reply to finish or press esc to stop it");
        }
        let chat = self
            .db
            .chat(id)?
            .ok_or_else(|| anyhow::anyhow!("no chat matching {id:?}"))?;
        self.messages = self.db.messages(&chat.id)?;
        self.chat = Some(chat);
        self.after_chat_change();
        Ok(())
    }

    pub fn new_chat(&mut self) {
        if self.turn.is_some() {
            self.notify(Role::Warn, "A reply is still streaming · esc to stop it first");
            return;
        }
        self.chat = None;
        self.messages.clear();
        self.after_chat_change();
    }

    fn after_chat_change(&mut self) {
        self.local.clear();
        self.clear_before = None;
        self.scroll = 0;
        self.editing_last = None;
        self.staged.clear();
        self.context_tokens = None;
        self.page = Page::Chat;
        self.dialog = None;
        self.recent_selection = 0;
    }

    pub fn reload_messages(&mut self) {
        if let Some(chat) = &self.chat {
            self.messages = self.db.messages(&chat.id).unwrap_or_default();
            if let Ok(Some(fresh)) = self.db.chat(&chat.id) {
                self.chat = Some(fresh);
            }
        }
    }

    /// The provider and model this chat (or the next new chat) will use.
    pub fn current_selection(&self) -> Option<(Provider, String)> {
        let (provider_id, model_id) = match (&self.chat, &self.next_model) {
            (Some(chat), _) => (chat.provider_id.clone(), chat.model_id.clone()),
            (None, Some((provider, model))) => (Some(provider.clone()), Some(model.clone())),
            (None, None) => (None, None),
        };
        crate::providers::resolve_provider_model(&self.db, provider_id.as_deref(), model_id.as_deref()).ok()
    }

    fn ensure_chat(&mut self) -> anyhow::Result<Chat> {
        if let Some(chat) = &self.chat {
            return Ok(chat.clone());
        }
        let (provider, model) = self
            .current_selection()
            .ok_or_else(|| anyhow::anyhow!("No model yet. Open /providers and add an endpoint."))?;
        let chat = self.db.create_chat(Some((&provider.id, &model)))?;
        let settings = self.db.settings();
        if let Some(prompt) = settings.get("defaultPromptId").and_then(Value::as_str) {
            let _ = self.db.set_chat_prompt(&chat.id, Some(prompt));
        }
        let chat = self.db.chat(&chat.id)?.unwrap_or(chat);
        self.chat = Some(chat.clone());
        self.next_model = None;
        Ok(chat)
    }

    // ---------------------------------------------------------------- sending

    pub fn submit(&mut self) {
        let raw = self.composer.text().to_string();
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            if self.is_welcome() {
                self.open_recent();
            }
            return;
        }
        if self.turn.as_ref().is_some_and(|turn| turn.editing_args) {
            self.submit_edited_arguments();
            return;
        }
        if self.turn.is_some() && !trimmed.starts_with('/') {
            self.notify(Role::Warn, "A reply is still streaming · esc to stop it");
            return;
        }
        self.remember(trimmed);
        self.composer.clear();
        self.popup_dismissed = None;
        if let Some(command) = trimmed.strip_prefix('/') {
            self.run_command(command.trim());
        } else if let Some(command) = trimmed.strip_prefix('!') {
            self.run_shell(command.trim().to_string());
        } else if let Some(note) = trimmed.strip_prefix('#') {
            self.save_note(note.trim());
        } else {
            self.send(trimmed.to_string(), false);
        }
    }

    pub fn send(&mut self, text: String, web: bool) {
        let chat = match self.ensure_chat() {
            Ok(chat) => chat,
            Err(error) => {
                self.composer.set(text);
                self.fail(error.to_string());
                return;
            }
        };
        let (provider, model) = match crate::providers::resolve_provider_model(
            &self.db,
            chat.provider_id.as_deref(),
            chat.model_id.as_deref(),
        ) {
            Ok(pair) => pair,
            Err(error) => {
                self.composer.set(text);
                self.fail(format!("{error} · open /providers"));
                return;
            }
        };
        if let Some(created_at) = self.editing_last.take() {
            if let Err(error) = self.db.delete_messages_from(&chat.id, created_at) {
                self.fail(format!("Could not replace the last message: {error}"));
                return;
            }
            self.cache.clear();
        }
        let (clean, mut attachments) = self.extract_attachments(&text);
        attachments.append(&mut self.staged);
        let mut messages = match engine::build_messages(&self.db, &chat, &model) {
            Ok(messages) => messages,
            Err(error) => {
                self.fail(format!("Could not load the chat: {error}"));
                return;
            }
        };
        messages.push(json!({"role": "user", "content": crate::files::message_content(&clean, &attachments)}));
        let metadata = json!(attachments.iter().map(|a| a.metadata()).collect::<Vec<_>>());
        if let Err(error) = self.db.save_message(&chat.id, "user", &clean, None, None, &metadata, &json!([]), &json!([])) {
            self.fail(format!("Could not save your message: {error}"));
            return;
        }
        self.reload_messages();
        self.context_tokens = Some(engine::estimate_tokens(&messages));
        self.scroll = 0;
        let chat = self.chat.clone().unwrap_or(chat);
        let spec = TurnSpec {
            params: chat.params.clone(),
            chat,
            provider,
            model,
            messages,
            web,
        };
        self.start_turn(spec);
    }

    fn start_turn(&mut self, spec: TurnSpec) {
        let id = self.next_turn_id;
        self.next_turn_id += 1;
        let cancel = Arc::new(AtomicBool::new(false));
        self.turn = Some(ActiveTurn {
            id,
            chat_id: spec.chat.id.clone(),
            started: Instant::now(),
            content: String::new(),
            reasoning: String::new(),
            reasoning_ms: None,
            tools: Vec::new(),
            cancel: cancel.clone(),
            approval: None,
            editing_args: false,
        });
        let ctx = self.ctx.clone();
        let tx = self.tx.clone();
        std::thread::Builder::new()
            .name("aihat-turn".into())
            .spawn(move || {
                let mut sink = ChannelSink {
                    id,
                    tx: tx.clone(),
                    cancel: cancel.clone(),
                };
                let result = engine::run_turn(&ctx, spec, &cancel, &mut sink).map_err(|error| describe_error(&error));
                let _ = tx.send(WorkerMsg::Turn(id, TurnEvent::Done(result)));
            })
            .ok();
    }

    /// Esc while a reply streams: keep what arrived and save it.
    pub fn stop(&mut self) {
        let Some(turn) = self.turn.take() else {
            return;
        };
        turn.cancel.store(true, Ordering::Relaxed);
        let output = TurnOutput {
            content: turn.content.clone(),
            reasoning: turn.reasoning.clone(),
            elapsed_ms: turn.started.elapsed().as_millis() as u64,
            tool_logs: turn.tools.iter().filter_map(|(_, _, log)| log.clone()).collect(),
            cancelled: true,
            ..TurnOutput::default()
        };
        if !output.content.trim().is_empty() || !output.tool_logs.is_empty() {
            self.save_reply(&turn.chat_id, &output);
            self.notify(Role::Warn, "■ Stopped · the partial reply is saved");
        } else {
            self.notify(Role::Warn, "■ Stopped");
        }
    }

    fn save_reply(&mut self, chat_id: &str, output: &TurnOutput) {
        let parsed = crate::genui::parse_reply(&output.content, None);
        let result = self.db.save_message_with_ui(
            chat_id,
            "assistant",
            &output.stored_content(),
            output.completion_tokens,
            Some(output.elapsed_ms),
            &json!([]),
            &json!(output.citations),
            &json!(output.tool_logs),
            parsed.spec.as_ref(),
        );
        if let Err(error) = result {
            self.fail(format!("The reply finished but could not be saved: {error}"));
        }
        self.reload_messages();
    }

    pub fn on_worker(&mut self, message: WorkerMsg) {
        match message {
            WorkerMsg::Turn(id, event) => self.on_turn_event(id, event),
            WorkerMsg::Task(task) => self.on_task(task),
        }
    }

    fn on_turn_event(&mut self, id: u64, event: TurnEvent) {
        let Some(turn) = self.turn.as_mut().filter(|turn| turn.id == id) else {
            // A stopped turn: refuse any approval it is still waiting on.
            if let TurnEvent::Approval(_, reply) = event {
                let _ = reply.send(Decision::Deny);
            }
            return;
        };
        match event {
            TurnEvent::Reasoning(delta) => turn.reasoning.push_str(&delta),
            TurnEvent::Text(delta) => {
                if turn.reasoning_ms.is_none() && !turn.reasoning.is_empty() {
                    turn.reasoning_ms = Some(turn.started.elapsed().as_millis() as u64);
                }
                turn.content.push_str(&delta);
            }
            TurnEvent::ToolStarted(label) => turn.tools.push((label, Instant::now(), None)),
            TurnEvent::ToolFinished(log) => {
                if let Some(slot) = turn.tools.iter_mut().rev().find(|(_, _, done)| done.is_none()) {
                    slot.2 = Some(log);
                } else {
                    let label = ToolLabel {
                        name: log["name"].as_str().unwrap_or("tool").into(),
                        detail: String::new(),
                    };
                    turn.tools.push((label, Instant::now(), Some(log)));
                }
            }
            TurnEvent::Warning(text) => {
                let line = Line::from(vec![
                    Span::styled(" ! ", theme().fg(Role::Warn)),
                    Span::styled(text, theme().fg(Role::Dim)),
                ]);
                self.push_local(vec![line]);
            }
            TurnEvent::Approval(request, reply) => {
                turn.approval = Some(PendingApproval {
                    request,
                    reply,
                    selection: 0,
                });
                self.scroll = 0;
            }
            TurnEvent::Done(result) => {
                let turn = self.turn.take().expect("checked above");
                match result {
                    Ok(output) => {
                        self.context_tokens = match (output.prompt_tokens, output.completion_tokens) {
                            (Some(prompt), completion) => Some(prompt + completion.unwrap_or(0)),
                            _ => self.context_tokens,
                        };
                        if output.content.trim().is_empty() && output.tool_logs.is_empty() && output.reasoning.is_empty() {
                            self.chat_error("The model returned an empty reply.");
                        } else {
                            self.save_reply(&turn.chat_id, &output);
                        }
                    }
                    Err(error) => {
                        if !turn.content.trim().is_empty() {
                            let partial = TurnOutput {
                                content: turn.content.clone(),
                                reasoning: turn.reasoning.clone(),
                                elapsed_ms: turn.started.elapsed().as_millis() as u64,
                                ..TurnOutput::default()
                            };
                            self.save_reply(&turn.chat_id, &partial);
                        }
                        self.chat_error(&error);
                    }
                }
            }
        }
    }

    pub fn answer_approval(&mut self, decision: Decision) {
        let Some(turn) = self.turn.as_mut() else {
            return;
        };
        if let Some(pending) = turn.approval.take() {
            let _ = pending.reply.send(decision);
        }
        turn.editing_args = false;
    }

    /// "Edit the arguments": put the JSON in the message box.
    pub fn edit_approval_arguments(&mut self) {
        let Some(turn) = self.turn.as_mut() else {
            return;
        };
        let Some(pending) = turn.approval.as_ref() else {
            return;
        };
        let pretty = serde_json::to_string_pretty(&pending.request.arguments).unwrap_or_default();
        turn.editing_args = true;
        self.composer.set(pretty);
    }

    fn submit_edited_arguments(&mut self) {
        match serde_json::from_str::<Value>(self.composer.text()) {
            Ok(value) if value.is_object() => {
                self.composer.clear();
                self.answer_approval(Decision::Edited(value));
            }
            _ => self.fail("The arguments must be a JSON object"),
        }
    }

    fn extract_attachments(&mut self, text: &str) -> (String, Vec<crate::files::Attachment>) {
        let mut words = Vec::new();
        let mut attachments = Vec::new();
        for (index, part) in text.split(' ').enumerate() {
            let path = part.strip_prefix('@').filter(|path| path.len() > 1 && !path.contains('\n'));
            if let Some(path) = path {
                let file = if path.starts_with("http://") || path.starts_with("https://") {
                    crate::files::read_url(path)
                } else {
                    crate::files::read(&super::files::expand(path))
                };
                match file {
                    Ok(file) => {
                        attachments.push(file);
                        words.push(path.to_string());
                        continue;
                    }
                    Err(error) => {
                        self.notify(Role::Warn, format!("! Could not attach {path}: {error}"));
                    }
                }
            }
            let _ = index;
            words.push(part.to_string());
        }
        (words.join(" "), attachments)
    }

    fn remember(&mut self, text: &str) {
        self.history_index = None;
        if self.history.last().map(String::as_str) != Some(text) {
            self.history.push(text.to_string());
            if self.history.len() > 500 {
                self.history.remove(0);
            }
            save_history(&self.paths, &self.history);
        }
    }

    pub fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let index = match self.history_index {
            None => {
                self.history_draft = self.composer.text().to_string();
                self.history.len() - 1
            }
            Some(0) => 0,
            Some(index) => index - 1,
        };
        self.history_index = Some(index);
        self.composer.set(self.history[index].clone());
    }

    pub fn history_next(&mut self) {
        let Some(index) = self.history_index else {
            return;
        };
        if index + 1 >= self.history.len() {
            self.history_index = None;
            let draft = std::mem::take(&mut self.history_draft);
            self.composer.set(draft);
        } else {
            self.history_index = Some(index + 1);
            self.composer.set(self.history[index + 1].clone());
        }
    }

    /// ↑ in an empty box: edit the last message you sent.
    pub fn edit_last(&mut self) -> bool {
        if self.turn.is_some() {
            return false;
        }
        let Some(last) = self.visible_messages().into_iter().rev().find(|m| m.role == "user").cloned() else {
            return false;
        };
        self.composer.set(last.content.clone());
        self.editing_last = Some(last.created_at);
        self.notify(Role::Dim, "Editing your last message · enter resends it · esc cancels");
        true
    }

    // ---------------------------------------------------------------- commands

    pub fn run_command(&mut self, input: &str) {
        let (name, args) = input.split_once(' ').unwrap_or((input, ""));
        let args = args.trim();
        match name {
            "model" | "models" => {
                if args.is_empty() {
                    self.open_models();
                } else {
                    self.pick_model_by_name(args);
                }
            }
            "prompt" | "system" => {
                if args.is_empty() {
                    self.open_prompts();
                } else {
                    self.pick_prompt_by_name(args);
                }
            }
            "providers" | "provider" => self.open_providers(),
            "tools" => self.dialog = Some(Dialog::Tools(dialogs::ToolsDialog::new(self))),
            "mcp" => self.dialog = Some(Dialog::Mcp(dialogs::ListDialog::default())),
            "skills" => self.dialog = Some(Dialog::Skills(dialogs::ListDialog::default())),
            "new" | "clear-chat" => self.new_chat(),
            "resume" | "history" | "chats" => self.open_history(),
            "compact" => self.compact(),
            "export" => self.export(args),
            "image" | "draw" => self.image(args),
            "search" | "web" => {
                if args.is_empty() {
                    self.notify(Role::Warn, "Type what to search: /search fabric api changes");
                } else {
                    self.send(args.to_string(), true);
                }
            }
            "refresh" => self.refresh_models(if args.is_empty() { None } else { Some(args.to_string()) }),
            "rename" => self.rename(args),
            "copy" => self.copy_last_reply(),
            "clear" => self.clear_screen(),
            "memory" => self.toggle_memory(),
            "keys" | "help" => self.dialog = Some(Dialog::Keys),
            "quit" | "exit" | "q" => self.quit = true,
            "" => {}
            other => self.fail(format!("Unknown command /{other} · type / to see them all")),
        }
    }

    pub fn open_models(&mut self) {
        self.dialog = Some(Dialog::Models(dialogs::ModelPicker::new(self)));
    }

    pub fn open_prompts(&mut self) {
        self.dialog = Some(Dialog::Prompts(dialogs::PromptPicker::new(self)));
    }

    pub fn open_history(&mut self) {
        self.dialog = Some(Dialog::History(dialogs::HistoryPicker::new(self)));
    }

    pub fn open_providers(&mut self) {
        self.dialog = None;
        self.page = Page::Providers(ProvidersPage::new(self));
    }

    fn pick_model_by_name(&mut self, query: &str) {
        let models = self.db.models(None).unwrap_or_default();
        let wanted = query.to_lowercase();
        let found = models
            .iter()
            .filter(|model| model.enabled && model.is_chat())
            .find(|model| model.id.to_lowercase() == wanted)
            .or_else(|| {
                models
                    .iter()
                    .filter(|model| model.enabled && model.is_chat())
                    .find(|model| model.id.to_lowercase().contains(&wanted))
            })
            .cloned();
        match found {
            Some(model) => self.apply_model(&model.provider_id, &model.id, false),
            None => self.fail(format!("No model matches {query:?} · ctrl+p lists them")),
        }
    }

    /// Use this model for the current chat, or make it the default.
    pub fn apply_model(&mut self, provider_id: &str, model_id: &str, make_default: bool) {
        let provider_name = self
            .db
            .provider(provider_id)
            .ok()
            .flatten()
            .map(|p| p.name)
            .unwrap_or_default();
        if make_default {
            match self.db.set_default_model(provider_id, model_id) {
                Ok(()) => self.ok(format!("Default model: {model_id} on {provider_name}")),
                Err(error) => self.fail(error.to_string()),
            }
        }
        match &self.chat {
            Some(chat) => {
                let chat_id = chat.id.clone();
                let mut params = self.db.default_params(provider_id, model_id);
                if let (Some(target), Some(old)) = (params.as_object_mut(), chat.params.as_object()) {
                    // Keep choices the user made for this chat, like reasoning effort.
                    for key in ["reasoning", "stream"] {
                        if let Some(value) = old.get(key) {
                            target.insert(key.into(), value.clone());
                        }
                    }
                }
                let result = self
                    .db
                    .patch_chat_selection(&chat_id, provider_id, model_id)
                    .and_then(|_| self.db.set_chat_params(&chat_id, &params));
                match result {
                    Ok(()) => {
                        self.reload_messages();
                        if !make_default {
                            self.ok(format!("This chat uses {model_id} · {provider_name}"));
                        }
                    }
                    Err(error) => self.fail(error.to_string()),
                }
            }
            None => {
                self.next_model = Some((provider_id.to_string(), model_id.to_string()));
                if !make_default {
                    self.ok(format!("New chat uses {model_id} · {provider_name}"));
                }
            }
        }
    }

    fn pick_prompt_by_name(&mut self, query: &str) {
        if matches!(query, "none" | "off") {
            self.apply_prompt(None, false);
            return;
        }
        let wanted = query.to_lowercase();
        let found = self
            .db
            .prompts()
            .unwrap_or_default()
            .into_iter()
            .find(|prompt| prompt.name.to_lowercase().contains(&wanted));
        match found {
            Some(prompt) => self.apply_prompt(Some(prompt.id), false),
            None => self.fail(format!("No system prompt matches {query:?} · ctrl+s lists them")),
        }
    }

    pub fn apply_prompt(&mut self, prompt_id: Option<String>, make_default: bool) {
        let name = prompt_id
            .as_ref()
            .and_then(|id| self.db.prompts().ok()?.into_iter().find(|p| &p.id == id))
            .map(|p| p.name)
            .unwrap_or_else(|| "none".into());
        if make_default {
            let mut settings = self.db.settings();
            match &prompt_id {
                Some(id) => settings["defaultPromptId"] = json!(id),
                None => {
                    if let Some(map) = settings.as_object_mut() {
                        map.remove("defaultPromptId");
                    }
                }
            }
            if let Err(error) = self.db.set_kv("settings", &settings.to_string()) {
                self.fail(error.to_string());
                return;
            }
        }
        if let Some(chat) = self.chat.clone() {
            if let Err(error) = self.db.set_chat_prompt(&chat.id, prompt_id.as_deref()) {
                self.fail(error.to_string());
                return;
            }
            self.reload_messages();
        }
        if make_default {
            self.ok(format!("New chats use the system prompt {name}"));
        } else {
            self.ok(format!("System prompt: {name}"));
        }
    }

    pub fn current_prompt_name(&self) -> Option<String> {
        let settings = self.db.settings();
        let id = self
            .chat
            .as_ref()
            .and_then(|chat| chat.prompt_id.clone())
            .or_else(|| settings.get("defaultPromptId").and_then(Value::as_str).map(str::to_owned))?;
        self.db.prompts().ok()?.into_iter().find(|p| p.id == id).map(|p| p.name)
    }

    fn open_recent(&mut self) {
        let chats = self.db.chats(false).unwrap_or_default();
        if let Some(chat) = chats.get(self.recent_selection) {
            let id = chat.id.clone();
            if let Err(error) = self.open_chat(&id) {
                self.fail(error.to_string());
            }
        }
    }

    fn save_note(&mut self, note: &str) {
        if note.is_empty() {
            self.notify(Role::Warn, "Type the note after #, like: # answer in British English");
            return;
        }
        let chat = match self.ensure_chat() {
            Ok(chat) => chat,
            Err(error) => {
                self.fail(error.to_string());
                return;
            }
        };
        let mut settings = self.db.settings();
        if !settings.get("chatNotes").is_some_and(Value::is_object) {
            settings["chatNotes"] = json!({});
        }
        let notes = settings["chatNotes"][&chat.id].as_array().cloned().unwrap_or_default();
        let mut notes = notes;
        notes.push(json!(note));
        settings["chatNotes"][&chat.id] = json!(notes);
        match self.db.set_kv("settings", &settings.to_string()) {
            Ok(()) => {
                let t = theme();
                self.push_local(vec![Line::from(vec![
                    Span::styled(" # ", t.fg(Role::Link)),
                    Span::styled("Saved to this chat's system prompt: ", t.fg(Role::Dim)),
                    Span::styled(note.to_string(), t.fg(Role::Text)),
                ])]);
            }
            Err(error) => self.fail(error.to_string()),
        }
    }

    fn run_shell(&mut self, command: String) {
        if command.is_empty() {
            self.notify(Role::Warn, "Type a command after !, like: !git status");
            return;
        }
        self.busy_task = Some(format!("running {command}"));
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = shell_output(&command);
            let _ = tx.send(WorkerMsg::Task(Task::Shell { command, result }));
        });
    }

    fn compact(&mut self) {
        let Some(chat) = self.chat.clone() else {
            self.notify(Role::Warn, "Nothing to compact yet");
            return;
        };
        let Some((provider, model)) = self.current_selection() else {
            self.fail("No model to write the summary");
            return;
        };
        self.busy_task = Some("summarising earlier messages".into());
        let ctx = self.ctx.clone();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = Database::open(&ctx.db_path)
                .and_then(|db| engine::compact(&ctx, &db, &chat, &provider, &model))
                .map_err(|error| describe_error(&error));
            let _ = tx.send(WorkerMsg::Task(Task::Compacted { result }));
        });
    }

    fn export(&mut self, args: &str) {
        let Some(chat) = self.chat.clone() else {
            self.notify(Role::Warn, "Nothing to export yet");
            return;
        };
        let json_format = args.eq_ignore_ascii_case("json");
        let result = if json_format {
            crate::export::chat_json(&self.db, &chat).map(|v| serde_json::to_string_pretty(&v).unwrap_or_default())
        } else {
            crate::export::chat_markdown(&self.db, &chat)
        };
        let name = format!(
            "{}.{}",
            crate::export::slug(&chat.title),
            if json_format { "json" } else { "md" }
        );
        match result.and_then(|content| std::fs::write(&name, content).map_err(Into::into)) {
            Ok(()) => {
                let count = self.visible_messages().len();
                self.ok(format!("Wrote {} to {name}", text::plural(count, "message", "messages")));
            }
            Err(error) => self.fail(format!("Could not export: {error}")),
        }
    }

    fn image(&mut self, args: &str) {
        let raw = args.split_whitespace().any(|word| word == "--raw");
        let prompt: String = args
            .split_whitespace()
            .filter(|word| *word != "--raw")
            .collect::<Vec<_>>()
            .join(" ");
        if prompt.is_empty() {
            self.notify(Role::Warn, "Describe the picture: /image a lighthouse in a storm");
            return;
        }
        let chat = match self.ensure_chat() {
            Ok(chat) => chat,
            Err(error) => {
                self.fail(error.to_string());
                return;
            }
        };
        if let Err(error) = self.db.save_message(&chat.id, "user", &format!("/image {prompt}"), None, None, &json!([]), &json!([]), &json!([])) {
            self.fail(error.to_string());
            return;
        }
        self.reload_messages();
        self.busy_task = Some("drawing".into());
        let selection = self.current_selection();
        let ctx = self.ctx.clone();
        let downloads = self.paths.downloads_dir.clone();
        let tx = self.tx.clone();
        let chat_id = chat.id.clone();
        std::thread::spawn(move || {
            let started = Instant::now();
            let result = super::tasks::draw_image(&ctx, selection, &prompt, raw, &downloads);
            let _ = tx.send(WorkerMsg::Task(Task::Image {
                chat_id,
                prompt,
                result,
                ms: started.elapsed().as_millis() as u64,
            }));
        });
    }

    pub fn refresh_models(&mut self, provider: Option<String>) {
        let label = provider.clone().unwrap_or_else(|| "all providers".into());
        self.busy_task = Some(format!("refreshing models from {label}"));
        let ctx = self.ctx.clone();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = Database::open(&ctx.db_path)
                .and_then(|db| crate::providers::refresh_one(&db, &ctx.secrets, provider.as_deref()))
                .map_err(|error| describe_error(&error));
            let _ = tx.send(WorkerMsg::Task(Task::ModelsRefreshed { label, result }));
        });
    }

    fn rename(&mut self, title: &str) {
        let Some(chat) = self.chat.clone() else {
            self.notify(Role::Warn, "Start the chat first, then rename it");
            return;
        };
        if title.is_empty() {
            self.notify(Role::Warn, "Type the new name: /rename QEMU notes");
            return;
        }
        match self.db.rename_chat(&chat.id, title) {
            Ok(()) => {
                self.reload_messages();
                self.ok(format!("Renamed to {title}"));
            }
            Err(error) => self.fail(error.to_string()),
        }
    }

    pub fn copy_last_reply(&mut self) {
        let last = self
            .messages
            .iter()
            .rev()
            .find(|message| message.role == "assistant")
            .map(|message| engine::split_reasoning(&message.content).1);
        match last {
            Some(text) => match super::tasks::copy_to_clipboard(&text) {
                Ok(()) => self.ok("Copied the last reply"),
                Err(error) => self.fail(format!("Could not copy: {error}")),
            },
            None => self.notify(Role::Warn, "There is no reply to copy yet"),
        }
    }

    pub fn clear_screen(&mut self) {
        self.clear_before = Some(chrono::Utc::now().timestamp_millis());
        self.local.clear();
        self.scroll = 0;
        self.notify(Role::Dim, "Screen cleared · the chat is still saved · ctrl+r to reopen it");
    }

    fn toggle_memory(&mut self) {
        let Some(chat) = self.chat.clone() else {
            self.notify(Role::Warn, "Start the chat first, then change its memory");
            return;
        };
        let next = !chat.memory_enabled;
        match self.db.set_chat_memory(&chat.id, next) {
            Ok(()) => {
                self.reload_messages();
                self.ok(if next { "Memory is on for this chat" } else { "Memory is off for this chat" });
            }
            Err(error) => self.fail(error.to_string()),
        }
    }

    pub fn toggle_reasoning(&mut self) {
        self.show_reasoning = !self.show_reasoning;
        self.cache.clear();
        self.config.show_reasoning = self.show_reasoning;
        let _ = self.config.save(&self.paths);
    }

    // ---------------------------------------------------------------- tasks

    fn on_task(&mut self, task: Task) {
        self.busy_task = None;
        match task {
            Task::ProviderTested { provider_id, result } => {
                if let Page::Providers(page) = &mut self.page {
                    page.testing.remove(&provider_id);
                }
                let name = self.db.provider(&provider_id).ok().flatten().map(|p| p.name).unwrap_or_default();
                match result {
                    Ok((latency, models)) => self.ok(format!(
                        "{name} · {} · {}",
                        text::duration(latency),
                        text::plural(models, "model", "models")
                    )),
                    Err(error) => self.fail(format!("{name}: {error}")),
                }
                if let Page::Providers(page) = &mut self.page {
                    page.refresh_keys = true;
                }
            }
            Task::FormTested { token, result } => {
                if let Some(Dialog::Form(form)) = self.dialog.as_mut() {
                    if form.token == token {
                        form.set_test_result(result);
                    }
                }
            }
            Task::ModelsRefreshed { label, result } => match result {
                Ok(summary) => {
                    let role = if summary.failures.is_empty() { Role::Accent } else { Role::Warn };
                    self.notify(role, format!("{} {label}: {}", summary.marker(), summary.describe()));
                }
                Err(error) => self.fail(format!("{label}: {error}")),
            },
            Task::Compacted { result } => match result {
                Ok(Some(_)) => {
                    self.cache.clear();
                    self.push_local(vec![Line::from(vec![
                        Span::styled(" ◇ ", theme().fg(Role::Code)),
                        Span::styled(
                            "Earlier messages are summarised to free context · the full chat stays in history",
                            theme().fg(Role::Dim),
                        ),
                    ])]);
                    self.context_tokens = None;
                }
                Ok(None) => self.notify(Role::Dim, "The chat is short; nothing to compact yet"),
                Err(error) => self.fail(format!("Could not compact: {error}")),
            },
            Task::Image { chat_id, prompt, result, ms } => {
                let (content, attachments) = match result {
                    Ok((files, used)) => {
                        let attachments: Vec<Value> = files
                            .iter()
                            .map(|path| {
                                json!({
                                    "id": uuid::Uuid::new_v4().to_string(),
                                    "kind": "image",
                                    "name": path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                                    "uri": path.display().to_string(),
                                })
                            })
                            .collect();
                        let content = if used.trim() == prompt.trim() {
                            String::new()
                        } else {
                            format!("Prompt used: {used}")
                        };
                        (content, json!(attachments))
                    }
                    Err(error) => {
                        self.chat_error(&format!("Image failed: {error}"));
                        return;
                    }
                };
                let _ = self.db.save_message(&chat_id, "assistant", &content, None, Some(ms), &attachments, &json!([]), &json!([]));
                self.reload_messages();
            }
            Task::McpRefreshed { name, result } => match result {
                Ok(count) => self.ok(format!("{name}: {} ready", text::plural(count, "tool", "tools"))),
                Err(error) => self.fail(format!("{name}: {error}")),
            },
            Task::Shell { command, result } => {
                let t = theme();
                match result {
                    Ok(output) => {
                        let lines = output.lines().count();
                        self.staged.push(crate::files::Attachment {
                            name: format!("$ {command}"),
                            kind: "file".into(),
                            mime: "text/plain".into(),
                            text: Some(format!("$ {command}\n{output}")),
                            data_uri: None,
                            meta: text::plural(lines, "line", "lines"),
                        });
                        self.push_local(vec![
                            Line::from(vec![
                                Span::styled(" ! ", t.fg(Role::Link)),
                                Span::styled(command, t.fg(Role::Strong)),
                            ]),
                            Line::styled(
                                format!(
                                    "   └ {} · added to your next message",
                                    text::plural(lines, "line", "lines")
                                ),
                                t.fg(Role::Dim),
                            ),
                        ]);
                    }
                    Err(error) => {
                        self.push_local(vec![
                            Line::from(vec![
                                Span::styled(" ! ", t.fg(Role::Link)),
                                Span::styled(command, t.fg(Role::Strong)),
                            ]),
                            Line::styled(format!("   └ ✗ {error}"), t.fg(Role::Error)),
                        ]);
                    }
                }
            }
        }
    }

    pub fn spawn_task(&self, work: impl FnOnce() -> Task + Send + 'static) {
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(WorkerMsg::Task(work()));
        });
    }

    // ---------------------------------------------------------------- keys

    pub fn on_paste(&mut self, text: &str) {
        if let Some(Dialog::Form(form)) = self.dialog.as_mut() {
            form.paste(text);
            return;
        }
        if self.dialog.is_none() && matches!(self.page, Page::Chat) {
            self.composer.insert(text);
            self.history_index = None;
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('c') {
            self.ctrl_c();
            return;
        }
        if self.dialog.is_some() {
            dialogs::on_key(self, key);
            return;
        }
        if self.turn.as_ref().is_some_and(|turn| turn.approval.is_some() && !turn.editing_args) {
            self.approval_key(key);
            return;
        }
        if matches!(self.page, Page::Providers(_)) {
            super::providers::on_key(self, key);
            return;
        }
        if let Some(popup) = self.popup() {
            if self.popup_key(popup, key) {
                return;
            }
        }
        self.chat_key(key);
    }

    fn ctrl_c(&mut self) {
        if self.dialog.is_some() {
            self.dialog = None;
            return;
        }
        if !self.composer.is_empty() {
            self.composer.clear();
            self.editing_last = None;
            return;
        }
        if self.turn.is_some() {
            self.stop();
            return;
        }
        let now = Instant::now();
        if self.last_ctrl_c.is_some_and(|last| now.duration_since(last) < Duration::from_millis(1500)) {
            self.quit = true;
        } else {
            self.last_ctrl_c = Some(now);
            self.notify(Role::Dim, "Press ctrl+c again to quit");
        }
    }

    fn approval_key(&mut self, key: KeyEvent) {
        let Some(pending) = self.turn.as_mut().and_then(|turn| turn.approval.as_mut()) else {
            return;
        };
        match key.code {
            KeyCode::Up => pending.selection = pending.selection.saturating_sub(1),
            KeyCode::Down => pending.selection = (pending.selection + 1).min(3),
            KeyCode::Char('a') => self.answer_approval(Decision::AlwaysForServer),
            KeyCode::Char('e') => self.edit_approval_arguments(),
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('d') => self.answer_approval(Decision::Deny),
            KeyCode::Char('y') => self.answer_approval(Decision::Once),
            KeyCode::Enter => match pending.selection {
                0 => self.answer_approval(Decision::Once),
                1 => self.answer_approval(Decision::AlwaysForServer),
                2 => self.edit_approval_arguments(),
                _ => self.answer_approval(Decision::Deny),
            },
            _ => {}
        }
    }

    /// Which suggestion list belongs above the message box right now.
    pub fn popup(&self) -> Option<Popup> {
        if self.popup_dismissed.as_deref() == Some(self.composer.text()) || self.turn.as_ref().is_some_and(|t| t.editing_args) {
            return None;
        }
        let text = self.composer.text();
        if text.starts_with('/') && !text.contains(char::is_whitespace) {
            return Some(Popup::Commands);
        }
        let (_, token) = self.composer.token_at_cursor()?;
        (token.starts_with('@') && !text.starts_with('/')).then_some(Popup::Attach)
    }

    fn popup_key(&mut self, popup: Popup, key: KeyEvent) -> bool {
        let count = match popup {
            Popup::Commands => super::view::matching_commands(self.composer.text()).len(),
            Popup::Attach => self.attach_matches().len(),
        };
        match key.code {
            KeyCode::Up => {
                self.popup_selection = self.popup_selection.checked_sub(1).unwrap_or(count.saturating_sub(1));
                true
            }
            KeyCode::Down => {
                self.popup_selection = if self.popup_selection + 1 >= count { 0 } else { self.popup_selection + 1 };
                true
            }
            KeyCode::Esc => {
                self.popup_dismissed = Some(self.composer.text().to_string());
                true
            }
            KeyCode::Tab | KeyCode::Enter if count > 0 => {
                match popup {
                    Popup::Commands => {
                        let commands = super::view::matching_commands(self.composer.text());
                        let command = commands[self.popup_selection.min(count - 1)];
                        if key.code == KeyCode::Tab || command.takes_args {
                            self.composer.set(format!("/{} ", command.name));
                        } else {
                            self.composer.set(format!("/{}", command.name));
                            self.submit();
                        }
                    }
                    Popup::Attach => {
                        let matches = self.attach_matches();
                        let chosen = matches[self.popup_selection.min(count - 1)].path.clone();
                        if let Some((range, _)) = self.composer.token_at_cursor() {
                            self.composer.replace_range(range, &format!("@{chosen} "));
                        }
                    }
                }
                self.popup_selection = 0;
                true
            }
            _ => false,
        }
    }

    pub fn attach_matches(&mut self) -> Vec<super::files::FileMatch> {
        let query = self
            .composer
            .token_at_cursor()
            .map(|(_, token)| token.trim_start_matches('@').to_string())
            .unwrap_or_default();
        self.files.matches(&query, 4)
    }

    fn chat_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let cols = self.composer_cols();
        let before = self.composer.text().to_string();
        match key.code {
            KeyCode::Char('p') if ctrl => self.open_models(),
            KeyCode::Char('s') if ctrl => self.open_prompts(),
            KeyCode::Char('r') if ctrl => self.open_history(),
            KeyCode::Char('n') if ctrl => self.new_chat(),
            KeyCode::Char('l') if ctrl => self.clear_screen(),
            KeyCode::Char('o') if ctrl => self.toggle_reasoning(),
            KeyCode::Char('y') if ctrl => self.copy_last_reply(),
            KeyCode::Char('u') if ctrl => self.composer.delete_to_start(),
            KeyCode::Char('k') if ctrl => self.composer.delete_to_end(),
            KeyCode::Char('w') if ctrl => self.composer.delete_word(),
            KeyCode::Char('a') if ctrl => self.composer.home(),
            KeyCode::Char('e') if ctrl => self.composer.end(),
            KeyCode::Char('j') if ctrl => self.composer.insert("\n"),
            KeyCode::Char('?') if self.composer.is_empty() => self.dialog = Some(Dialog::Keys),
            KeyCode::Char(c) if !ctrl => {
                self.composer.insert(c.encode_utf8(&mut [0; 4]));
                self.history_index = None;
            }
            KeyCode::Enter if shift || alt => self.composer.insert("\n"),
            KeyCode::Enter => {
                if self.composer.text().ends_with('\\') {
                    self.composer.backspace();
                    self.composer.insert("\n");
                } else {
                    self.submit();
                }
            }
            KeyCode::Backspace if alt || ctrl => self.composer.delete_word(),
            KeyCode::Backspace => self.composer.backspace(),
            KeyCode::Delete => self.composer.delete(),
            KeyCode::Left if alt || ctrl => self.composer.word_left(),
            KeyCode::Right if alt || ctrl => self.composer.word_right(),
            KeyCode::Left => self.composer.left(),
            KeyCode::Right => self.composer.right(),
            KeyCode::Home if self.composer.is_empty() => self.scroll = usize::MAX,
            KeyCode::End if self.composer.is_empty() => self.scroll = 0,
            KeyCode::Home => self.composer.home(),
            KeyCode::End => self.composer.end(),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_add(self.page_rows()),
            KeyCode::PageDown => self.scroll = self.scroll.saturating_sub(self.page_rows()),
            KeyCode::Up => {
                if self.is_welcome() && self.composer.is_empty() {
                    self.recent_selection = self.recent_selection.saturating_sub(1);
                } else if self.composer.is_empty() && self.history_index.is_none() && self.editing_last.is_none() {
                    if !self.edit_last() {
                        self.history_prev();
                    }
                } else if !self.composer.up(cols) {
                    self.editing_last = None;
                    self.history_prev();
                }
            }
            KeyCode::Down => {
                if self.is_welcome() && self.composer.is_empty() {
                    let count = self.db.chats(false).map(|c| c.len()).unwrap_or(0).min(3);
                    self.recent_selection = (self.recent_selection + 1).min(count.saturating_sub(1));
                } else if !self.composer.down(cols) {
                    self.history_next();
                }
            }
            KeyCode::Esc => {
                if self.turn.as_ref().is_some_and(|turn| turn.editing_args) {
                    self.composer.clear();
                    if let Some(turn) = self.turn.as_mut() {
                        turn.editing_args = false;
                    }
                } else if self.turn.is_some() {
                    self.stop();
                } else if self.editing_last.is_some() {
                    self.editing_last = None;
                    self.composer.clear();
                } else if !self.staged.is_empty() {
                    self.staged.clear();
                    self.notify(Role::Dim, "Removed the attached command output");
                }
            }
            KeyCode::Tab => {}
            _ => {}
        }
        if self.composer.text() != before {
            self.popup_selection = 0;
            self.popup_dismissed = None;
            if self.composer.text().is_empty() {
                self.editing_last = None;
            }
        }
    }

    pub fn composer_cols(&self) -> usize {
        (self.width as usize).saturating_sub(6).max(4)
    }

    fn page_rows(&self) -> usize {
        10
    }

    // ---------------------------------------------------------------- transcript

    pub fn layout(&self) -> Layout {
        Layout {
            width: self.width as usize,
            narrow: self.width < 100,
            show_reasoning: self.show_reasoning,
            show_stats: self.config.show_stats,
        }
    }

    /// The chat as blocks of rows; a blank row separates blocks.
    pub fn transcript_blocks(&mut self) -> Vec<Rc<Vec<Line<'static>>>> {
        let layout = self.layout();
        let mut blocks: Vec<Rc<Vec<Line<'static>>>> = Vec::new();
        let visible: Vec<Message> = self.visible_messages().into_iter().cloned().collect();
        let mut local = self.local.iter().peekable();
        for (index, message) in visible.iter().enumerate() {
            while let Some(entry) = local.next_if(|entry| entry.after <= index) {
                blocks.push(entry.lines.clone());
            }
            let cached = self
                .cache
                .get(&message.id)
                .filter(|(cached_layout, _)| same_layout(cached_layout, &layout))
                .map(|(_, lines)| lines.clone());
            let lines = match cached {
                Some(lines) => lines,
                None => {
                    let lines = Rc::new(match message.role.as_str() {
                        "user" => transcript::user_lines(message, layout),
                        _ => transcript::assistant_lines(message, layout),
                    });
                    self.cache.insert(message.id.clone(), (layout, lines.clone()));
                    lines
                }
            };
            blocks.push(lines);
        }
        for entry in local {
            blocks.push(entry.lines.clone());
        }
        if let Some(turn) = &self.turn {
            blocks.extend(super::view::turn_blocks(turn, layout));
        }
        blocks
    }
}

fn same_layout(a: &Layout, b: &Layout) -> bool {
    a.width == b.width && a.narrow == b.narrow && a.show_reasoning == b.show_reasoning && a.show_stats == b.show_stats
}

struct ChannelSink {
    id: u64,
    tx: Sender<WorkerMsg>,
    cancel: Arc<AtomicBool>,
}

impl engine::Sink for ChannelSink {
    fn reasoning(&mut self, delta: &str) {
        let _ = self.tx.send(WorkerMsg::Turn(self.id, TurnEvent::Reasoning(delta.to_string())));
    }

    fn text(&mut self, delta: &str) {
        let _ = self.tx.send(WorkerMsg::Turn(self.id, TurnEvent::Text(delta.to_string())));
    }

    fn tool_started(&mut self, label: &ToolLabel) {
        let _ = self.tx.send(WorkerMsg::Turn(self.id, TurnEvent::ToolStarted(label.clone())));
    }

    fn tool_finished(&mut self, log: &Value) {
        let _ = self.tx.send(WorkerMsg::Turn(self.id, TurnEvent::ToolFinished(log.clone())));
    }

    fn warning(&mut self, text: &str) {
        let _ = self.tx.send(WorkerMsg::Turn(self.id, TurnEvent::Warning(text.to_string())));
    }

    fn approve(&mut self, request: &Approval) -> Decision {
        let (reply, answer) = mpsc::channel();
        if self
            .tx
            .send(WorkerMsg::Turn(self.id, TurnEvent::Approval(request.clone(), reply)))
            .is_err()
        {
            return Decision::Deny;
        }
        loop {
            match answer.recv_timeout(Duration::from_millis(200)) {
                Ok(decision) => return decision,
                Err(mpsc::RecvTimeoutError::Timeout) if !self.cancel.load(Ordering::Relaxed) => continue,
                Err(_) => return Decision::Deny,
            }
        }
    }
}

/// A readable one-line error, with the provider's own wording when it has one.
pub fn describe_error(error: &anyhow::Error) -> String {
    if let Some(api) = crate::api::api_error(error) {
        return match api.kind {
            crate::api::ErrorKind::Unreachable => format!("Can't reach the provider. {} Check it in /providers.", api.detail),
            crate::api::ErrorKind::Auth => format!("{api} Fix it in /providers (k sets the key)."),
            _ => api.to_string(),
        };
    }
    format!("{error:#}")
}

fn shell_output(command: &str) -> Result<String, String> {
    let output = if cfg!(windows) {
        std::process::Command::new("cmd").args(["/C", command]).output()
    } else {
        std::process::Command::new("sh").args(["-c", command]).output()
    }
    .map_err(|error| error.to_string())?;
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.trim().is_empty() {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&stderr);
    }
    if !output.status.success() && text.trim().is_empty() {
        return Err(format!("exited with {}", output.status));
    }
    Ok(text.chars().take(200_000).collect())
}

fn history_file(paths: &Paths) -> std::path::PathBuf {
    paths.data_dir.join("cli-history.txt")
}

fn load_history(paths: &Paths) -> Vec<String> {
    std::fs::read_to_string(history_file(paths))
        .map(|text| {
            text.lines()
                .map(|line| line.replace("\\n", "\n"))
                .filter(|line| !line.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn save_history(paths: &Paths, history: &[String]) {
    let text: String = history
        .iter()
        .rev()
        .take(500)
        .rev()
        .map(|entry| format!("{}\n", entry.replace('\n', "\\n")))
        .collect();
    let _ = std::fs::write(history_file(paths), text);
}
