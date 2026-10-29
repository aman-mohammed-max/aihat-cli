//! State and behaviour of the full-screen chat.

use super::composer::Composer;
use super::dialogs::{self, Dialog};
use super::files::FileIndex;
use super::keys::{self, Action};
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
    /// An MCP server needs a sign-in; the browser is open.
    SigningIn(String),
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
        result: Result<super::images::Drawn, String>,
        ms: u64,
    },
    McpRefreshed {
        name: String,
        result: Result<usize, String>,
    },
    /// The browser was opened for a server's sign-in.
    McpSigningIn {
        name: String,
    },
    /// Memory changed after a reply: "Saved to memory: …".
    MemoryNote(String),
    /// A skill read from its folder or GitHub: new, or `existing` again.
    SkillLoaded {
        existing: Option<String>,
        result: Result<crate::skills::Loaded, String>,
    },
    /// The server wants an access token pasted.
    McpNeedsToken {
        id: String,
        name: String,
    },
    Shell {
        command: String,
        result: Result<String, String>,
    },
    /// A marketplace search answered; `token` says which search.
    MarketResults {
        token: u64,
        result: Result<super::market::Found, String>,
    },
    MarketDetail {
        key: String,
        detail: super::market::Detail,
    },
    /// A marketplace skill put in ~/.agents/skills for every agent.
    SkillInstalledGlobally {
        result: Result<(String, std::path::PathBuf), String>,
    },
    /// How far the Phonon-2 download has got.
    VoiceProgress {
        done: u64,
        total: u64,
    },
    VoiceDownloaded {
        result: Result<(), String>,
    },
    /// Speech turned into text (or why not).
    Transcribed {
        result: Result<String, String>,
    },
    /// The model wrote a system prompt: its name and text.
    PromptWritten {
        result: Result<(String, String), String>,
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
    /// Waiting for a browser sign-in to this MCP server, since when.
    pub signing_in: Option<(String, Instant)>,
}

/// The folder aihat runs in: allowed (always, or until aihat quits), said no
/// to for now, or not asked yet (the assistant's first look asks).
pub struct FolderAccess {
    pub dir: std::path::PathBuf,
    pub allowed: bool,
    pub denied: bool,
}

pub struct Notice {
    pub text: String,
    pub role: Role,
    pub until: Instant,
}

pub enum Page {
    Chat,
    Providers(ProvidersPage),
    Market(Box<super::market::MarketPage>),
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
    /// The folder aihat runs in, and what the person said about it.
    pub folder: Option<FolderAccess>,
    /// Listening to the microphone (ctrl+t).
    pub recording: Option<crate::voice::Recording>,
    /// Phonon-2 being downloaded (from /voice).
    pub voice_download: Option<super::voice::Download>,
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
    /// The idea being drawn by /image, and when it started.
    pub drawing: Option<(String, Instant)>,
    /// A finished picture waiting for the open dialog to close.
    pub pending_image: Option<Box<super::images::ImageViewer>>,
    /// /settings: the event loop hands the terminal to an editor.
    pub edit_settings: bool,
    /// A system prompt to open in the editor.
    pub edit_prompt: Option<String>,
    /// Where "Jump to bottom" was drawn, for clicks.
    pub jump_button: Option<ratatui::layout::Rect>,
    /// Stops a browser sign-in that is waiting.
    pub sign_in_cancel: Option<Arc<AtomicBool>>,
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
            folder: None,
            recording: None,
            voice_download: None,
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
            drawing: None,
            pending_image: None,
            edit_settings: false,
            edit_prompt: None,
            jump_button: None,
            sign_in_cancel: None,
            form_token: 0,
        };
        if let Some(id) = initial_chat {
            app.open_chat(&id)?;
        }
        // The folder aihat runs in. Nothing is asked now: the assistant's
        // first look at it asks, unless it was allowed before.
        if let Ok(dir) = std::env::current_dir()
            && dir.parent().is_some()
        {
            let allowed = crate::folder::access(&app.db, &dir) == Some(crate::folder::Access::Allowed);
            app.folder = Some(FolderAccess { dir, allowed, denied: false });
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
            transcript::prose_width(self.width as usize).min(self.width as usize),
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
            self.notify(Role::Warn, format!("A reply is still streaming · {} to stop it first", keys::label(Action::Stop)));
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
            self.notify(Role::Warn, format!("A reply is still streaming · {} to stop it", keys::label(Action::Stop)));
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
            folder: self.folder.as_ref().filter(|f| !f.denied).map(|f| f.dir.clone()),
            folder_allowed: self.folder.as_ref().is_some_and(|f| f.allowed),
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
            signing_in: None,
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
                let (chat, provider, model) = (spec.chat.clone(), spec.provider.clone(), spec.model.clone());
                let asked = engine::last_user_text(&spec.messages);
                let result = engine::run_turn(&ctx, spec, &cancel, &mut sink).map_err(|error| describe_error(&error));
                let answer = result.as_ref().ok().filter(|o| !o.cancelled).map(|o| o.content.clone());
                let _ = tx.send(WorkerMsg::Turn(id, TurnEvent::Done(result)));
                // Models without tools learn memories after the reply, as in the app.
                if let (Some(answer), Ok(db)) = (answer, crate::db::Database::open(&ctx.db_path))
                    && let Some(note) = crate::memory::learn_from_turn(&db, &ctx.secrets, &chat, &provider, &model, &asked, &answer)
                {
                    let _ = tx.send(WorkerMsg::Task(Task::MemoryNote(note)));
                }
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
        // Anything else arriving means the sign-in is over.
        if !matches!(event, TurnEvent::SigningIn(_)) {
            turn.signing_in = None;
        }
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
                // "Saved “Rust at work”." from the memory tool, shown like the app's note.
                let memory_note = (log["name"].as_str() == Some(crate::memory::TOOL_NAME))
                    .then(|| log["result"].as_str().unwrap_or("").to_string())
                    .filter(|r| ["Saved", "Updated", "Deleted"].iter().any(|w| r.starts_with(w)));
                if let Some(slot) = turn.tools.iter_mut().rev().find(|(_, _, done)| done.is_none()) {
                    slot.2 = Some(log);
                } else {
                    let label = ToolLabel {
                        name: log["name"].as_str().unwrap_or("tool").into(),
                        detail: String::new(),
                    };
                    turn.tools.push((label, Instant::now(), Some(log)));
                }
                if let Some(note) = memory_note {
                    let note = note.replace("Saved", "Saved to memory:").replace("Updated", "Memory updated:").replace("Deleted", "Forgot");
                    self.notify(Role::Accent, format!("◇ {}", note.trim_end_matches('.')));
                }
            }
            TurnEvent::SigningIn(server) => {
                turn.signing_in = Some((server.clone(), Instant::now()));
                self.notify(Role::Accent, format!("Sign in to {server} in your browser · esc cancels"));
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
            // The answer about reading the folder holds until aihat quits
            // ("always" is also saved by the turn).
            if pending.request.kind == engine::ApprovalKind::ReadFolder
                && let Some(folder) = self.folder.as_mut()
            {
                match decision {
                    Decision::Deny => folder.denied = true,
                    _ => folder.allowed = true,
                }
            }
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
        // A command is edited as the command line itself.
        if pending.request.kind == engine::ApprovalKind::ReadFolder {
            return;
        }
        let text = match (pending.request.kind, pending.request.arguments.get("command").and_then(Value::as_str)) {
            (engine::ApprovalKind::Command, Some(command)) => command.to_string(),
            _ => serde_json::to_string_pretty(&pending.request.arguments).unwrap_or_default(),
        };
        turn.editing_args = true;
        self.composer.set(text);
    }

    fn submit_edited_arguments(&mut self) {
        let command = self.turn.as_ref().and_then(|turn| turn.approval.as_ref()).filter(|pending| pending.request.kind == engine::ApprovalKind::Command).map(|pending| pending.request.arguments.clone());
        if let Some(mut arguments) = command {
            let line = self.composer.text().trim().to_string();
            if line.is_empty() {
                self.fail("Type the command to run, or esc to go back");
                return;
            }
            arguments["command"] = json!(line);
            self.composer.clear();
            self.answer_approval(Decision::Edited(arguments));
            return;
        }
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
            "newprompt" => {
                if args.is_empty() {
                    self.dialog = Some(Dialog::Form(dialogs::Form::write_prompt()));
                } else {
                    self.write_prompt(args);
                }
            }
            "providers" | "provider" => self.open_providers(),
            "tools" => self.dialog = Some(Dialog::Tools(dialogs::ToolsDialog::new(self))),
            "mcp" => self.dialog = Some(Dialog::Mcp(dialogs::ListDialog::default())),
            "skills" => self.dialog = Some(Dialog::Skills(dialogs::ListDialog::default())),
            "market" | "marketplace" | "store" => self.open_market(args),
            "effort" | "think" | "reasoning" => {
                let level = match args.to_lowercase().as_str() {
                    "none" | "0" => "off".to_string(),
                    other => other.to_string(),
                };
                if dialogs::EFFORTS.iter().any(|(known, _)| *known == level) {
                    self.set_effort(&level);
                } else if args.is_empty() {
                    self.dialog = Some(Dialog::Effort(dialogs::EffortPicker::new(self)));
                } else {
                    self.fail(format!("Effort is off, low, medium or high, not {args:?}"));
                }
            }
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
            "memory" => self.dialog = Some(Dialog::Memory(dialogs::MemoryDialog::default())),
            "keys" | "help" => self.dialog = Some(Dialog::Keys),
            "about" | "version" => self.dialog = Some(Dialog::About(Default::default())),
            "voice" | "speech" => self.dialog = Some(Dialog::Voice(Default::default())),
            "talk" | "speak" => self.toggle_voice(),
            "folder" | "workspace" | "access" => super::folder::command(self, args),
            "settings" | "config" => self.edit_settings = true,
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

    /// The reasoning effort this chat sends (or new chats will): off, low,
    /// medium or high.
    pub fn current_effort(&self) -> String {
        let params = match (&self.chat, self.current_selection()) {
            (Some(chat), _) => chat.params.clone(),
            (None, Some((provider, model))) => self.db.default_params(&provider.id, &model),
            (None, None) => self.db.settings().get("defaultParams").cloned().unwrap_or_default(),
        };
        params.get("reasoning").and_then(Value::as_str).filter(|level| !level.is_empty()).unwrap_or("off").to_string()
    }

    /// `/effort high`: for this chat, or for new chats when there is none.
    pub fn set_effort(&mut self, level: &str) {
        let saved = match self.chat.clone() {
            Some(chat) => {
                let mut params = if chat.params.is_object() { chat.params.clone() } else { json!({}) };
                params["reasoning"] = json!(level);
                self.db.set_chat_params(&chat.id, &params).map(|_| {
                    if let Some(open) = self.chat.as_mut() {
                        open.params = params;
                    }
                })
            }
            None => {
                let mut settings = self.db.settings();
                if !settings.get("defaultParams").is_some_and(Value::is_object) {
                    settings["defaultParams"] = json!({"temperature": 0.7, "maxTokens": 4096, "topP": 0.95, "stream": true});
                }
                settings["defaultParams"]["reasoning"] = json!(level);
                self.db.set_kv("settings", &settings.to_string())
            }
        };
        match saved {
            Ok(()) if level == "off" => self.ok("Reasoning effort: off · the model decides"),
            Ok(()) => self.ok(format!("Reasoning effort: {level}")),
            Err(error) => self.fail(error.to_string()),
        }
    }

    /// `/market`, or `/market mcp` to start on MCP servers.
    pub fn open_market(&mut self, args: &str) {
        use super::market::{MarketPage, Tab};
        let tab = if args.to_lowercase().starts_with("mcp") { Tab::Mcp } else { Tab::Skills };
        self.dialog = None;
        self.page = Page::Market(Box::new(MarketPage::new(tab)));
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
            None => self.fail(format!("No model matches {query:?} · {} lists them", keys::label(Action::SwitchModel))),
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
        let found = self.db.prompts().unwrap_or_default().into_iter().find(|prompt| prompt.name.to_lowercase() == wanted);
        match found {
            Some(prompt) => self.apply_prompt(Some(prompt.id), false),
            // Not a saved prompt's name: a description for the model to write one.
            None => self.write_prompt(query),
        }
    }

    /// `/prompt a patient Rust tutor`: the model writes a system prompt; it
    /// is saved and used in this chat, as in the app.
    pub fn write_prompt(&mut self, description: &str) {
        let Some((provider, model)) = self.current_selection() else {
            self.fail("Add an endpoint and pick a chat model first · /providers");
            return;
        };
        self.busy_task = Some("writing your system prompt".into());
        let ctx = self.ctx.clone();
        let description = description.trim().to_string();
        self.spawn_task(move || Task::PromptWritten {
            result: engine::write_system_prompt(&ctx.secrets, &provider, &model, &description).map_err(|e| describe_error(&e)),
        });
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
        self.drawing = Some((prompt.clone(), Instant::now()));
        self.scroll = 0;
        let selection = self.current_selection();
        let ctx = self.ctx.clone();
        let tx = self.tx.clone();
        let chat_id = chat.id.clone();
        std::thread::spawn(move || {
            let started = Instant::now();
            let result = super::tasks::draw_image(&ctx, selection, &prompt, raw);
            let _ = tx.send(WorkerMsg::Task(Task::Image {
                chat_id,
                prompt,
                result,
                ms: started.elapsed().as_millis() as u64,
            }));
        });
    }

    /// The picture dialog is done: record the reply, with the saved files.
    pub fn finish_image(&mut self, chat_id: &str, prompt: &str, used: &str, saved: &[std::path::PathBuf], ms: u64) {
        let attachments: Vec<Value> = saved
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
        let mut content = if used.trim() == prompt.trim() { String::new() } else { format!("Prompt used: {used}") };
        if saved.is_empty() {
            if !content.is_empty() {
                content.push_str("\n\n");
            }
            content.push_str("Picture not saved.");
        }
        let _ = self.db.save_message(chat_id, "assistant", &content, None, Some(ms), &json!(attachments), &json!([]), &json!([]));
        self.reload_messages();
        self.scroll = 0;
    }

    /// After /settings: use what can change while running; colours, ascii
    /// boxes and keys are read at start.
    pub fn reload_settings(&mut self) {
        match AppConfig::load(&self.paths) {
            Ok(config) => {
                let restart = config.accent != self.config.accent
                    || config.colors != self.config.colors
                    || config.ascii != self.config.ascii
                    || serde_json::to_value(&config.keys).ok() != serde_json::to_value(&self.config.keys).ok();
                self.show_reasoning = config.show_reasoning;
                crate::skills::set_global_policy(config.skills.use_global, &config.skills.off);
                self.ctx.config = config.clone();
                self.config = config;
                self.cache.clear();
                if restart {
                    self.notify(Role::Accent, "Saved settings.json · restart aihat for the new colours and keys");
                } else {
                    self.ok("Saved settings.json");
                }
            }
            Err(error) => self.fail(format!("{error} · /settings to fix it")),
        }
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
        self.notify(Role::Dim, format!("Screen cleared · the chat is still saved · {} to reopen it", keys::label(Action::PastChats)));
    }

    pub fn toggle_memory(&mut self) {
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
        // Marketplace lookups run quietly beside other work.
        if !matches!(task, Task::MarketResults { .. } | Task::MarketDetail { .. } | Task::VoiceProgress { .. } | Task::VoiceDownloaded { .. }) {
            self.busy_task = None;
        }
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
                if let Some(Dialog::Form(form)) = self.dialog.as_mut()
                    && form.token == token {
                        form.set_test_result(result);
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
                self.drawing = None;
                match result {
                    Ok(drawn) => {
                        let viewer = Box::new(super::images::ImageViewer::new(chat_id, prompt, drawn, ms));
                        if self.dialog.is_none() {
                            self.dialog = Some(Dialog::Image(viewer));
                        } else {
                            self.pending_image = Some(viewer);
                            self.notify(Role::Accent, "Your picture is ready · it opens when you close this");
                        }
                    }
                    Err(error) => self.chat_error(&format!("Image failed: {error}")),
                }
            }
            Task::McpRefreshed { name, result } => {
                self.sign_in_cancel = None;
                match result {
                    Ok(count) => self.ok(format!("{name}: {} ready", text::plural(count, "tool", "tools"))),
                    Err(error) => self.fail(format!("{name}: {error}")),
                }
            }
            Task::MemoryNote(note) => self.notify(Role::Accent, format!("◇ {note}")),
            Task::SkillLoaded { existing, result } => match result {
                Ok((name, description, body, source, files)) => {
                    let saved = match &existing {
                        Some(id) => self.db.update_skill_source_with_files(id, &name, &description, &body, &source, &files).map(|_| id.clone()),
                        None if self.db.skills().unwrap_or_default().iter().any(|s| s.name.eq_ignore_ascii_case(&name)) => {
                            Err(anyhow::anyhow!("there is already a skill named {name}; r reads it again"))
                        }
                        None => self.db.save_skill_with_files(&name, &description, &body, &source, &files),
                    };
                    match saved {
                        Ok(id) => {
                            if existing.is_none() && crate::skills::is_github(&source) {
                                let _ = self.db.set_skill_auto_update(&id, true);
                            }
                            let files = if files.is_empty() { String::new() } else { format!(" · {}", text::plural(files.len(), "file", "files")) };
                            self.ok(format!("{} {name}{files}", if existing.is_some() { "Updated" } else { "Added" }));
                        }
                        Err(error) => self.fail(format!("Could not save {name}: {error}")),
                    }
                }
                Err(error) => self.fail(format!("Could not read the skill: {error}")),
            },
            Task::McpSigningIn { name } => {
                self.busy_task = Some(format!("waiting for you to sign in to {name}"));
                self.notify(Role::Accent, format!("Sign in to {name} in your browser · esc cancels"));
            }
            Task::McpNeedsToken { id, name } => {
                self.sign_in_cancel = None;
                self.dialog = Some(Dialog::Form(dialogs::Form::mcp_token(&id, &name)));
                self.notify(Role::Warn, format!("{name} needs an access token"));
            }
            Task::VoiceProgress { done, total } => {
                if let Some(download) = self.voice_download.as_mut() {
                    download.done = done;
                    download.total = total;
                }
            }
            Task::VoiceDownloaded { result } => {
                self.voice_download = None;
                match result {
                    Ok(()) => self.ok(format!("Phonon-2 is ready · {} talks, on this computer", keys::label(Action::Voice))),
                    Err(error) => self.fail(error),
                }
            }
            Task::Transcribed { result } => match result {
                Ok(text) => {
                    let before = self.composer.text().to_string();
                    if !before.is_empty() && !before.ends_with(char::is_whitespace) {
                        self.composer.insert(" ");
                    }
                    self.composer.insert(&text);
                    self.ok(format!("{} sends it", keys::label(Action::Send)));
                }
                Err(error) => self.fail(error),
            },
            Task::PromptWritten { result } => match result {
                Ok((name, body)) => match self.db.add_prompt_tagged(&name, &body, &["generated"]) {
                    Ok(id) => {
                        if self.ensure_chat().is_ok() {
                            self.apply_prompt(Some(id.clone()), false);
                        }
                        self.ok(format!("Saved “{name}” and set it for this chat"));
                        if self.dialog.is_none() {
                            self.dialog = Some(Dialog::Text(dialogs::TextView::written_prompt(&id, &name, &body)));
                        }
                    }
                    Err(error) => self.fail(format!("Could not save the prompt: {error}")),
                },
                Err(error) => self.fail(format!("Could not write the prompt: {error}")),
            },
            Task::MarketResults { token, result } => super::market::on_results(self, token, result),
            Task::MarketDetail { key, detail } => super::market::on_detail(self, key, detail),
            Task::SkillInstalledGlobally { result } => match result {
                Ok((name, dir)) => self.ok(format!("Added {name} for every agent · {}", text::home_relative(&dir))),
                Err(error) => self.fail(format!("Could not add it: {error}")),
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

    /// Ctrl+V: a picture on the clipboard goes with the next message, and so
    /// do files copied in a file manager; text is typed in.
    pub fn paste_clipboard(&mut self) {
        match super::tasks::read_clipboard() {
            Ok(super::tasks::Clip::Picture(picture)) => {
                self.ok(format!("Picture attached · {} · esc removes it", picture.meta));
                self.staged.push(picture);
            }
            Ok(super::tasks::Clip::Files(paths)) => {
                let mut added = Vec::new();
                for path in paths {
                    match crate::files::read(&path) {
                        Ok(file) => {
                            added.push(file.name.clone());
                            self.staged.push(file);
                        }
                        Err(error) => self.fail(format!("{error:#}")),
                    }
                }
                if !added.is_empty() {
                    self.ok(format!("Attached {} · esc removes", added.join(", ")));
                }
            }
            Ok(super::tasks::Clip::Text(text)) => self.on_paste(&text),
            Ok(super::tasks::Clip::Empty) => self.notify(Role::Dim, "The clipboard is empty"),
            Err(error) => self.fail(format!("Could not read the clipboard: {error}")),
        }
    }

    pub fn on_paste(&mut self, text: &str) {
        if let Some(Dialog::Form(form)) = self.dialog.as_mut() {
            form.paste(text);
            return;
        }
        // A picture dragged into the terminal arrives as its path.
        if self.dialog.is_none()
            && matches!(self.page, Page::Chat)
            && let Some(path) = super::tasks::picture_path(text)
        {
            match crate::files::read(&path) {
                Ok(picture) => {
                    self.ok(format!("Picture attached · {} · esc removes it", picture.name));
                    self.staged.push(picture);
                }
                Err(error) => self.fail(format!("{error:#}")),
            }
            return;
        }
        if self.dialog.is_none() && matches!(self.page, Page::Chat) {
            self.composer.insert(text);
            self.history_index = None;
        }
        if self.dialog.is_none() && matches!(self.page, Page::Market(_)) {
            super::market::on_paste(self, text);
        }
    }

    /// The wheel scrolls the chat (or moves through an open list); a click
    /// on "Jump to bottom" goes back to the newest message. Returns whether
    /// anything changed.
    pub fn on_mouse(&mut self, mouse: crossterm::event::MouseEvent) -> bool {
        use crossterm::event::{MouseButton, MouseEventKind};
        let arrow = |code| KeyEvent::new(code, KeyModifiers::NONE);
        match mouse.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let up = mouse.kind == MouseEventKind::ScrollUp;
                if self.dialog.is_some() || matches!(self.page, Page::Providers(_) | Page::Market(_)) {
                    self.on_key(arrow(if up { KeyCode::Up } else { KeyCode::Down }));
                } else if up {
                    self.scroll = self.scroll.saturating_add(3);
                } else {
                    self.scroll = self.scroll.saturating_sub(3);
                }
                true
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let hit = self.jump_button.is_some_and(|button| {
                    mouse.row == button.y && mouse.column >= button.x && mouse.column < button.x + button.width
                });
                if hit && self.dialog.is_none() {
                    self.scroll = 0;
                }
                hit
            }
            _ => false,
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        // ctrl+c always works, whatever quit is bound to.
        if (ctrl && key.code == KeyCode::Char('c')) || keys::is(Action::Quit, &key) {
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
        if matches!(self.page, Page::Market(_)) {
            super::market::on_key(self, key);
            return;
        }
        if let Some(popup) = self.popup()
            && self.popup_key(popup, key) {
                return;
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
            self.notify(Role::Dim, format!("Press {} again to quit", keys::label(Action::Quit)));
        }
    }

    fn approval_key(&mut self, key: KeyEvent) {
        let Some(pending) = self.turn.as_mut().and_then(|turn| turn.approval.as_mut()) else {
            return;
        };
        // Reading the folder has three answers; tools and commands have four.
        let reading = pending.request.kind == engine::ApprovalKind::ReadFolder;
        match key.code {
            KeyCode::Up => pending.selection = pending.selection.saturating_sub(1),
            KeyCode::Down => pending.selection = (pending.selection + 1).min(if reading { 2 } else { 3 }),
            KeyCode::Char('a') => self.answer_approval(Decision::AlwaysForServer),
            KeyCode::Enter if reading => match pending.selection {
                0 => self.answer_approval(Decision::Once),
                1 => self.answer_approval(Decision::AlwaysForServer),
                _ => self.answer_approval(Decision::Deny),
            },
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

    /// ctrl+t: start listening, or stop and turn the speech into text, which
    /// lands in the message box to check before sending.
    pub fn toggle_voice(&mut self) {
        if let Some(recording) = self.recording.take() {
            self.busy_task = Some("turning speech into text".into());
            let ctx = self.ctx.clone();
            let voice = self.config.voice.clone();
            self.spawn_task(move || {
                let result = recording.stop().and_then(|file| {
                    let text = crate::db::Database::open(&ctx.db_path).and_then(|db| crate::voice::transcribe(&voice, &db, &ctx.secrets, &file));
                    let _ = std::fs::remove_file(&file);
                    text
                });
                Task::Transcribed { result: result.map_err(|e| format!("{e:#}")) }
            });
            return;
        }
        if crate::voice::engine(&self.config.voice) == "local" && !crate::voice::model_installed() {
            self.dialog = Some(Dialog::Voice(Default::default()));
            self.notify(Role::Warn, "Download Phonon-2 first (enter on Model), or choose the endpoint");
            return;
        }
        match crate::voice::start(&self.config.voice) {
            Ok(recording) => {
                if crate::voice::engine(&self.config.voice) == "local" {
                    crate::voice::preload();
                }
                self.recording = Some(recording);
                self.notify(Role::Accent, format!("Listening… {} or enter stops · esc cancels", keys::label(Action::Voice)));
            }
            Err(error) => self.fail(format!("{error:#}")),
        }
    }

    fn chat_key(&mut self, key: KeyEvent) {
        // While listening: enter or the voice key stops, esc cancels.
        if self.recording.is_some() {
            match key.code {
                KeyCode::Esc => {
                    if let Some(recording) = self.recording.take() {
                        recording.cancel();
                    }
                    self.notify(Role::Dim, "Stopped listening");
                }
                KeyCode::Enter => self.toggle_voice(),
                _ if keys::is(Action::Voice, &key) => self.toggle_voice(),
                _ => {}
            }
            return;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let cols = self.composer_cols();
        let before = self.composer.text().to_string();
        if let Some(action) = keys::action_for(&key, self.composer.is_empty()) {
            match action {
                Action::Send => {
                    if self.composer.text().ends_with('\\') {
                        self.composer.backspace();
                        self.composer.insert("\n");
                    } else {
                        self.submit();
                    }
                }
                Action::NewLine => self.composer.insert("\n"),
                Action::Stop => self.escape(),
                Action::SwitchModel => self.open_models(),
                Action::SwitchPrompt => self.open_prompts(),
                Action::PastChats => self.open_history(),
                Action::NewChat => self.new_chat(),
                Action::ClearScreen => self.clear_screen(),
                Action::ToggleReasoning => self.toggle_reasoning(),
                Action::CopyReply => self.copy_last_reply(),
                Action::Voice => self.toggle_voice(),
                Action::ShowKeys => self.dialog = Some(Dialog::Keys),
                Action::Quit => self.ctrl_c(),
            }
            self.after_edit(&before);
            return;
        }
        match key.code {
            KeyCode::Char('v') if ctrl => self.paste_clipboard(),
            KeyCode::Char('u') if ctrl => self.composer.delete_to_start(),
            KeyCode::Char('k') if ctrl => self.composer.delete_to_end(),
            KeyCode::Char('w') if ctrl => self.composer.delete_word(),
            KeyCode::Char('a') if ctrl => self.composer.home(),
            KeyCode::Char('e') if ctrl => self.composer.end(),
            KeyCode::Char(c) if !ctrl => {
                self.composer.insert(c.encode_utf8(&mut [0; 4]));
                self.history_index = None;
            }
            KeyCode::Enter if shift || alt => self.composer.insert("\n"),
            KeyCode::Backspace if alt || ctrl => self.composer.delete_word(),
            KeyCode::Backspace => self.composer.backspace(),
            KeyCode::Delete => self.composer.delete(),
            KeyCode::Left if alt || ctrl => self.composer.word_left(),
            KeyCode::Right if alt || ctrl => self.composer.word_right(),
            KeyCode::Left => self.composer.left(),
            KeyCode::Right => self.composer.right(),
            KeyCode::Home if self.composer.is_empty() => self.scroll = usize::MAX,
            KeyCode::End if self.composer.is_empty() || ctrl => self.scroll = 0,
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
            KeyCode::Esc => self.escape(),
            _ => {}
        }
        self.after_edit(&before);
    }

    fn after_edit(&mut self, before: &str) {
        if self.composer.text() != before {
            self.popup_selection = 0;
            self.popup_dismissed = None;
            if self.composer.text().is_empty() {
                self.editing_last = None;
            }
        }
    }

    /// esc (or the key bound to stop): stop the reply, or back out of
    /// whatever is in progress.
    fn escape(&mut self) {
        if let Some(cancel) = self.sign_in_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
            self.notify(Role::Dim, "Sign-in cancelled");
            return;
        }
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
            self.notify(Role::Dim, "Removed what was attached");
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
            pictures: self.config.images.preview != "off",
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
        if let Some((prompt, started)) = &self.drawing {
            blocks.push(Rc::new(super::images::drawing_lines(prompt, "drawing", started.elapsed().as_millis() as u64, layout)));
        }
        if let Some(turn) = &self.turn {
            blocks.extend(super::view::turn_blocks(turn, layout));
        }
        blocks
    }
}

fn same_layout(a: &Layout, b: &Layout) -> bool {
    a.width == b.width && a.narrow == b.narrow && a.show_reasoning == b.show_reasoning && a.show_stats == b.show_stats && a.pictures == b.pictures
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

    fn signing_in(&mut self, server: &str, _url: &str) {
        let _ = self.tx.send(WorkerMsg::Turn(self.id, TurnEvent::SigningIn(server.to_string())));
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
