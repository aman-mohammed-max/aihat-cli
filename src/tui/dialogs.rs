//! Boxes drawn over the dimmed chat: pickers, forms, confirmations, keys.

use super::app::{App, Page, PendingApproval, Task};
use super::draw::{self, put, put_line};
use crate::engine::Decision;
use crate::models::Model;
use crate::ui::text::{self, width};
use crate::ui::theme::{Role, theme};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use serde_json::{Value, json};

pub enum Dialog {
    Models(ModelPicker),
    Prompts(PromptPicker),
    History(HistoryPicker),
    Keys,
    Tools(ToolsDialog),
    Mcp(ListDialog),
    Skills(ListDialog),
    Form(Form),
    Confirm(Confirm),
    Text(TextView),
}

// ------------------------------------------------------------------ shared

/// Fuzzy score of `query` against `haystack`; `Some(0)` for an empty query.
fn score(query: &str, haystack: &str) -> Option<u32> {
    if query.is_empty() {
        return Some(0);
    }
    let mut matcher = Matcher::new(Config::DEFAULT);
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
    let mut buf = Vec::new();
    pattern.score(Utf32Str::new(haystack, &mut buf), &mut matcher)
}

fn edit_query(query: &mut String, key: &KeyEvent) -> bool {
    match key.code {
        KeyCode::Backspace => {
            query.pop();
            true
        }
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            query.clear();
            true
        }
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            query.push(c);
            true
        }
        _ => false,
    }
}

/// `› query█                       337 models`
fn query_row(buf: &mut Buffer, inner: Rect, y: u16, query: &str, count: &str) {
    let t = theme();
    let x = put(buf, inner.x, y, "›", t.fg(Role::Accent), inner.right());
    let x = put(buf, x + 1, y, query, t.fg(Role::Strong), inner.right());
    if let Some(cell) = buf.cell_mut((x.min(inner.right() - 1), y)) {
        cell.set_style(t.cursor());
    }
    let count_x = inner.right().saturating_sub(width(count) as u16);
    put(buf, count_x, y, count, t.fg(Role::Dim), inner.right());
}

enum PickRow<K> {
    Header(Line<'static>),
    Item(Line<'static>, K, bool),
}

/// Scroll window that keeps `selected` (an index into `rows`) visible.
fn window(selected: usize, total: usize, visible: usize) -> usize {
    if total <= visible {
        0
    } else {
        (selected + 1).saturating_sub(visible).min(total - visible)
    }
}

fn draw_rows<K>(buf: &mut Buffer, inner: Rect, top: u16, rows: &[PickRow<K>], selected: Option<usize>, visible: usize) {
    let first = window(selected.unwrap_or(0), rows.len(), visible);
    for (offset, row) in rows.iter().skip(first).take(visible).enumerate() {
        let y = top + offset as u16;
        match row {
            PickRow::Header(line) => {
                put_line(buf, inner.x, y, line, inner.right());
            }
            PickRow::Item(line, _, _) => {
                let is_selected = selected == Some(first + offset);
                draw::row(buf, inner, y, line.clone(), is_selected);
            }
        }
    }
}

fn move_selection<K>(rows: &[PickRow<K>], current: Option<usize>, down: bool) -> Option<usize> {
    let items: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, row)| matches!(row, PickRow::Item(..)))
        .map(|(index, _)| index)
        .collect();
    if items.is_empty() {
        return None;
    }
    let position = current.and_then(|c| items.iter().position(|i| *i == c)).unwrap_or(0);
    let next = if down {
        (position + 1).min(items.len() - 1)
    } else {
        position.saturating_sub(1)
    };
    Some(items[next])
}

fn first_item<K>(rows: &[PickRow<K>]) -> Option<usize> {
    rows.iter().position(|row| matches!(row, PickRow::Item(..)))
}

/// Selected rows show their text in the accent colour, like the mockups.
fn emphasise(line: Line<'static>, selected: bool) -> Line<'static> {
    if !selected {
        return line;
    }
    let accent = theme().color(Role::Accent);
    Line::from(
        line.spans
            .into_iter()
            .map(|span| Span::styled(span.content, span.style.fg(accent)))
            .collect::<Vec<_>>(),
    )
}

// ------------------------------------------------------------------ models

pub struct ModelPicker {
    query: String,
    selected: Option<usize>,
    make_default: bool,
}

impl ModelPicker {
    pub fn new(app: &App) -> Self {
        let mut picker = Self {
            query: String::new(),
            selected: None,
            make_default: false,
        };
        let rows = picker.rows(app);
        let current = app.current_selection().map(|(p, m)| (p.id, m));
        picker.selected = rows
            .iter()
            .position(|row| matches!(row, PickRow::Item(_, key, _) if Some(key) == current.as_ref()))
            .or_else(|| first_item(&rows));
        picker
    }

    fn rows(&self, app: &App) -> Vec<PickRow<(String, String)>> {
        let t = theme();
        let current = app.current_selection().map(|(p, m)| (p.id, m));
        let settings = app.db.settings();
        let default_model = settings.get("defaultModelId").and_then(Value::as_str).map(str::to_owned);
        let mut providers = app.db.providers().unwrap_or_default();
        providers.sort_by_key(|p| !p.is_default);
        let mut rows = Vec::new();
        for provider in providers {
            let models: Vec<Model> = app
                .db
                .models(Some(&provider.id))
                .unwrap_or_default()
                .into_iter()
                .filter(|m| m.enabled && m.is_chat())
                .filter(|m| score(&self.query, &format!("{} {}", m.id, provider.name)).is_some())
                .collect();
            if models.is_empty() {
                continue;
            }
            let broken = matches!(provider.status.as_str(), "rejected" | "offline");
            let health = match provider.status.as_str() {
                "rejected" => "key rejected".to_string(),
                "offline" => "offline".to_string(),
                _ => provider.latency_ms.map(text::duration).unwrap_or_else(|| "not tested".into()),
            };
            rows.push(PickRow::Header(Line::styled(
                format!("{} · {health}", provider.name),
                t.fg(Role::Dim),
            )));
            for model in models {
                let key = (provider.id.clone(), model.id.clone());
                let is_current = current.as_ref() == Some(&key);
                let marker = if is_current { "● " } else { "  " };
                let caps = if broken {
                    if provider.status == "rejected" {
                        "fix the key: /providers".to_string()
                    } else {
                        "offline · /providers".to_string()
                    }
                } else {
                    model_caps_short(&model)
                };
                let is_default = provider.is_default && default_model.as_deref() == Some(model.id.as_str());
                let style = if broken { t.fg(Role::Dim) } else { t.fg(Role::Text) };
                let marker_style = if is_current { t.fg(Role::Accent) } else { style };
                let line = Line::from(vec![
                    Span::styled(marker, marker_style),
                    Span::styled(text::pad(&model.id, 23), style),
                    Span::styled(text::pad(&caps, 18), style),
                    Span::styled(if is_default { "default" } else { "" }, style),
                ]);
                rows.push(PickRow::Item(line, key, !broken));
            }
        }
        rows
    }

    fn draw(&self, app: &App, buf: &mut Buffer, screen: Rect) {
        let t = theme();
        let rows = self.rows(app);
        let rows: Vec<PickRow<(String, String)>> = rows
            .into_iter()
            .enumerate()
            .map(|(index, row)| match row {
                PickRow::Item(line, key, ok) => PickRow::Item(emphasise(line, Some(index) == self.selected), key, ok),
                header => header,
            })
            .collect();
        let count = rows.iter().filter(|r| matches!(r, PickRow::Item(..))).count();
        let visible = rows.len().clamp(1, 12);
        let area = draw::dialog_rect(screen, 64, visible as u16 + 6, 5);
        let title = if self.make_default { "Default model" } else { "Switch model" };
        draw::boxed(buf, area, t.fg(Role::Accent), title, "");
        let inner = draw::inner(area);
        query_row(buf, inner, inner.y, &self.query, &text::plural(count, "model", "models"));
        let list_rows = inner.height.saturating_sub(4) as usize;
        if rows.is_empty() {
            let message = if app.db.providers().map(|p| p.is_empty()).unwrap_or(true) {
                "No providers yet · /providers to add one"
            } else {
                "No model matches · /refresh reloads the lists"
            };
            put(buf, inner.x, inner.y + 2, message, t.fg(Role::Dim), inner.right());
        }
        draw_rows(buf, inner, inner.y + 2, &rows, self.selected, list_rows);
        let scope = if self.make_default { "tab default / this chat" } else { "tab this chat / default" };
        put(
            buf,
            inner.x,
            inner.bottom() - 1,
            &format!("↑↓ move · enter pick · {scope} · ctrl+e edit"),
            t.fg(Role::Dim),
            inner.right(),
        );
    }

    fn key(&mut self, app: &mut App, key: KeyEvent) -> bool {
        let rows = self.rows(app);
        match key.code {
            KeyCode::Esc => return false,
            KeyCode::Up => self.selected = move_selection(&rows, self.selected, false),
            KeyCode::Down => self.selected = move_selection(&rows, self.selected, true),
            KeyCode::Tab | KeyCode::BackTab => self.make_default = !self.make_default,
            KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(PickRow::Item(_, (provider, model), _)) = self.selected.and_then(|i| rows.get(i)) {
                    app.dialog = Some(Dialog::Form(Form::model_defaults(app, provider, model)));
                    return true;
                }
            }
            KeyCode::Enter => {
                if let Some(PickRow::Item(_, (provider, model), _)) = self.selected.and_then(|i| rows.get(i)) {
                    let (provider, model) = (provider.clone(), model.clone());
                    app.apply_model(&provider, &model, self.make_default);
                    return false;
                }
            }
            _ => {
                if edit_query(&mut self.query, &key) {
                    self.selected = first_item(&self.rows(app));
                }
            }
        }
        true
    }
}

/// "tools · 128k", "vision · 32k".
pub fn model_caps_short(model: &Model) -> String {
    let kind = model.caps.get("kind").and_then(Value::as_str).unwrap_or("chat");
    let main = if model.caps.get("vision").and_then(Value::as_bool).unwrap_or(false) {
        "vision"
    } else if kind != "chat" {
        kind
    } else if model.supports_tools() {
        "tools"
    } else {
        "chat"
    };
    match model.context_k() {
        Some(k) => format!("{main} · {k}k"),
        None => main.to_string(),
    }
}

/// "chat · tools · 128k" for the provider details pane.
pub fn model_caps_long(model: &Model) -> String {
    let kind = model.caps.get("kind").and_then(Value::as_str).unwrap_or("chat");
    let mut parts = vec![match kind {
        "embedding" => "embeddings".to_string(),
        "transcription" | "audio" => "transcription".to_string(),
        other => other.to_string(),
    }];
    if kind == "chat" {
        if model.supports_tools() {
            parts.push("tools".into());
        }
        if model.caps.get("vision").and_then(Value::as_bool).unwrap_or(false) {
            parts.push("vision".into());
        }
    }
    if let Some(k) = model.context_k() {
        parts.push(format!("{k}k"));
    }
    parts.join(" · ")
}

// ------------------------------------------------------------------ prompts

pub struct PromptPicker {
    query: String,
    selected: Option<usize>,
    make_default: bool,
}

impl PromptPicker {
    pub fn new(app: &App) -> Self {
        let mut picker = Self {
            query: String::new(),
            selected: None,
            make_default: false,
        };
        let current = app.chat.as_ref().and_then(|c| c.prompt_id.clone());
        let rows = picker.rows(app);
        picker.selected = rows
            .iter()
            .position(|row| matches!(row, PickRow::Item(_, key, _) if *key == current))
            .or_else(|| first_item(&rows));
        picker
    }

    fn rows(&self, app: &App) -> Vec<PickRow<Option<String>>> {
        let t = theme();
        let settings = app.db.settings();
        let current = app
            .chat
            .as_ref()
            .and_then(|c| c.prompt_id.clone())
            .or_else(|| settings.get("defaultPromptId").and_then(Value::as_str).map(str::to_owned));
        let mut rows = Vec::new();
        if score(&self.query, "none").is_some() {
            let marker = if current.is_none() { "● " } else { "  " };
            rows.push(PickRow::Item(
                Line::from(vec![
                    Span::styled(marker, t.fg(Role::Accent)),
                    Span::styled("None", t.fg(Role::Text)),
                    Span::styled("  just the model", t.fg(Role::Dim)),
                ]),
                None,
                true,
            ));
        }
        let mut prompts = app.db.prompts().unwrap_or_default();
        prompts.sort_by_key(|p| !p.favorite);
        for prompt in prompts {
            let haystack = format!("{} {}", prompt.name, prompt.tags.join(" "));
            if score(&self.query, &haystack).is_none() {
                continue;
            }
            let is_current = current.as_deref() == Some(prompt.id.as_str());
            rows.push(PickRow::Item(
                Line::from(vec![
                    Span::styled(if is_current { "● " } else { "  " }, t.fg(Role::Accent)),
                    Span::styled(text::pad(&prompt.name, 30), t.fg(Role::Text)),
                    Span::styled(text::pad(&prompt.tags.join(", "), 18), t.fg(Role::Dim)),
                    Span::styled(format!("~{}", prompt.body.len() / 4), t.fg(Role::Dim)),
                ]),
                Some(prompt.id),
                true,
            ));
        }
        rows
    }

    fn draw(&self, app: &App, buf: &mut Buffer, screen: Rect) {
        let t = theme();
        let rows: Vec<_> = self
            .rows(app)
            .into_iter()
            .enumerate()
            .map(|(index, row)| match row {
                PickRow::Item(line, key, ok) => PickRow::Item(emphasise(line, Some(index) == self.selected), key, ok),
                header => header,
            })
            .collect();
        let count = rows.len().saturating_sub(1);
        let visible = rows.len().clamp(1, 12);
        let area = draw::dialog_rect(screen, 64, visible as u16 + 6, 5);
        let title = if self.make_default { "Default system prompt" } else { "System prompt" };
        draw::boxed(buf, area, t.fg(Role::Accent), title, "");
        let inner = draw::inner(area);
        query_row(buf, inner, inner.y, &self.query, &text::plural(count, "prompt", "prompts"));
        draw_rows(buf, inner, inner.y + 2, &rows, self.selected, inner.height.saturating_sub(4) as usize);
        let scope = if self.make_default { "tab default / this chat" } else { "tab this chat / default" };
        put(buf, inner.x, inner.bottom() - 1, &format!("↑↓ move · enter pick · {scope}"), t.fg(Role::Dim), inner.right());
    }

    fn key(&mut self, app: &mut App, key: KeyEvent) -> bool {
        let rows = self.rows(app);
        match key.code {
            KeyCode::Esc => return false,
            KeyCode::Up => self.selected = move_selection(&rows, self.selected, false),
            KeyCode::Down => self.selected = move_selection(&rows, self.selected, true),
            KeyCode::Tab | KeyCode::BackTab => self.make_default = !self.make_default,
            KeyCode::Enter => {
                if let Some(PickRow::Item(_, id, _)) = self.selected.and_then(|i| rows.get(i)) {
                    app.apply_prompt(id.clone(), self.make_default);
                    return false;
                }
            }
            _ => {
                if edit_query(&mut self.query, &key) {
                    self.selected = first_item(&self.rows(app));
                }
            }
        }
        true
    }
}

// ------------------------------------------------------------------ history

pub struct HistoryPicker {
    query: String,
    selected: Option<usize>,
}

impl HistoryPicker {
    pub fn new(app: &App) -> Self {
        let picker = Self {
            query: String::new(),
            selected: Some(0),
        };
        let rows = picker.rows(app);
        Self {
            selected: first_item(&rows),
            ..picker
        }
    }

    fn rows(&self, app: &App) -> Vec<PickRow<String>> {
        let t = theme();
        app.db
            .chats(false)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|chat| {
                let found = score(&self.query, &chat.title)?;
                Some((found, chat))
            })
            .map(|(_, chat)| {
                let pin = if chat.pinned { "▪ " } else { "" };
                let line = Line::from(vec![
                    Span::styled(text::pad(&format!("{pin}{}", chat.title), 40), t.fg(Role::Text)),
                    Span::raw(" "),
                    Span::styled(text::pad(chat.model_id.as_deref().unwrap_or(""), 10), t.fg(Role::Text)),
                    Span::raw(" "),
                    Span::styled(text::relative_time(chat.updated_at), t.fg(Role::Dim)),
                ]);
                PickRow::Item(line, chat.id, true)
            })
            .collect()
    }

    fn draw(&self, app: &App, buf: &mut Buffer, screen: Rect) {
        let t = theme();
        let rows: Vec<_> = self
            .rows(app)
            .into_iter()
            .enumerate()
            .map(|(index, row)| match row {
                PickRow::Item(line, key, ok) => PickRow::Item(emphasise(line, Some(index) == self.selected), key, ok),
                header => header,
            })
            .collect();
        let total = app.db.chats(false).map(|c| c.len()).unwrap_or(0);
        let visible = rows.len().clamp(1, 14);
        let area = draw::dialog_rect(screen, 72, visible as u16 + 6, 8);
        draw::boxed(buf, area, t.fg(Role::Accent), "Chats", "");
        let inner = draw::inner(area);
        query_row(buf, inner, inner.y, &self.query, &text::plural(total, "chat", "chats"));
        if rows.is_empty() {
            let message = if total == 0 { "No chats yet" } else { "No chat matches" };
            put(buf, inner.x, inner.y + 2, message, t.fg(Role::Dim), inner.right());
        }
        draw_rows(buf, inner, inner.y + 2, &rows, self.selected, inner.height.saturating_sub(4) as usize);
        put(
            buf,
            inner.x,
            inner.bottom() - 1,
            "enter open · ctrl+d delete · ctrl+e export · ctrl+n new",
            t.fg(Role::Dim),
            inner.right(),
        );
    }

    fn key(&mut self, app: &mut App, key: KeyEvent) -> bool {
        let rows = self.rows(app);
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let chosen = self.selected.and_then(|i| rows.get(i)).and_then(|row| match row {
            PickRow::Item(_, id, _) => Some(id.clone()),
            PickRow::Header(_) => None,
        });
        match key.code {
            KeyCode::Esc => return false,
            KeyCode::Up => self.selected = move_selection(&rows, self.selected, false),
            KeyCode::Down => self.selected = move_selection(&rows, self.selected, true),
            KeyCode::Char('n') if ctrl => {
                app.new_chat();
                return false;
            }
            KeyCode::Char('d') if ctrl => {
                if let Some(id) = chosen {
                    let title = app.db.chat(&id).ok().flatten().map(|c| c.title).unwrap_or_default();
                    app.dialog = Some(Dialog::Confirm(Confirm {
                        title: format!("Delete “{}”?", text::truncate(&title, 40)),
                        body: "The chat and its messages are removed for good.".into(),
                        yes: "delete",
                        action: ConfirmAction::DeleteChat(id),
                    }));
                    return true;
                }
            }
            KeyCode::Char('e') if ctrl => {
                if let Some(chat) = chosen.and_then(|id| app.db.chat(&id).ok().flatten()) {
                    let name = format!("{}.md", crate::export::slug(&chat.title));
                    match crate::export::chat_markdown(&app.db, &chat).and_then(|md| std::fs::write(&name, md).map_err(Into::into)) {
                        Ok(()) => app.ok(format!("Wrote {name}")),
                        Err(error) => app.fail(format!("Could not export: {error}")),
                    }
                }
            }
            KeyCode::Enter => {
                if let Some(id) = chosen {
                    if let Err(error) = app.open_chat(&id) {
                        app.fail(error.to_string());
                    }
                    return false;
                }
            }
            _ => {
                if edit_query(&mut self.query, &key) {
                    self.selected = first_item(&self.rows(app));
                }
            }
        }
        true
    }
}

// ------------------------------------------------------------------ keys

fn draw_keys(buf: &mut Buffer, screen: Rect) {
    let t = theme();
    let area = draw::dialog_rect(screen, 84, 17, 5);
    draw::boxed(buf, area, t.fg(Role::Accent), "Keys", "esc close");
    let inner = draw::inner(area);
    let half = inner.x + 42;
    let head = |buf: &mut Buffer, y: u16, left: &str, right: &str| {
        put(buf, inner.x, y, left, t.bold(Role::Strong), inner.right());
        put(buf, half, y, right, t.bold(Role::Strong), inner.right());
    };
    let pair = |buf: &mut Buffer, x: u16, y: u16, key: &str, key_width: usize, what: &str, right: u16| {
        put(buf, x, y, &text::pad(key, key_width), t.fg(Role::Link), right);
        put(buf, x + key_width as u16, y, what, t.fg(Role::Text), right);
    };
    let mut y = inner.y;
    head(buf, y, "Chat", "Anywhere");
    let chat = [
        ("enter", "send"),
        ("shift+enter", "new line"),
        ("esc", "stop the reply"),
        ("↑", "edit your last message"),
        ("ctrl+o", "show or hide reasoning"),
        ("ctrl+y", "copy the last reply"),
    ];
    let anywhere = [
        ("ctrl+p", "switch model"),
        ("ctrl+s", "switch system prompt"),
        ("ctrl+r", "past chats"),
        ("ctrl+n", "new chat"),
        ("ctrl+l", "clear the screen"),
        ("ctrl+c", "twice to quit"),
    ];
    for ((left_key, left_what), (right_key, right_what)) in chat.iter().zip(anywhere.iter()) {
        y += 1;
        pair(buf, inner.x, y, left_key, 13, left_what, half - 1);
        pair(buf, half, y, right_key, 9, right_what, inner.right());
    }
    y += 2;
    head(buf, y, "Typing", "Lists and dialogs");
    y += 1;
    pair(buf, inner.x, y, "/", 13, "commands", half - 1);
    pair(buf, half, y, "↑ ↓", 9, "move", inner.right());
    y += 1;
    pair(buf, inner.x, y, "@", 13, "attach a file or URL", half - 1);
    pair(buf, half, y, "tab", 9, "complete or switch pane", inner.right());
    y += 1;
    pair(buf, inner.x, y, "!", 13, "run a shell command, add its output to the message", inner.right());
    y += 1;
    pair(buf, inner.x, y, "#", 13, "save a note to the system prompt", inner.right());
    y += 2;
    let config = crate::ui::text::home_relative(
        &dirs::config_dir().unwrap_or_default().join("aihat").join("config.toml"),
    );
    put(
        buf,
        inner.x,
        y,
        &format!("Settings in {config} · works in 80×24 and wider"),
        t.fg(Role::Dim),
        inner.right(),
    );
}

// ------------------------------------------------------------------ tools

#[derive(Clone)]
enum ToolRow {
    Web,
    Memory,
    Server(String),
    Skill(String),
}

pub struct ToolsDialog {
    selected: Option<usize>,
}

impl ToolsDialog {
    pub fn new(app: &App) -> Self {
        let rows = Self::rows(app);
        Self {
            selected: first_item(&rows),
        }
    }

    fn rows(app: &App) -> Vec<PickRow<ToolRow>> {
        let t = theme();
        let toggle = |on: bool| {
            if on {
                Span::styled("■ ", t.fg(Role::Accent))
            } else {
                Span::styled("□ ", t.fg(Role::Dim))
            }
        };
        let item = |on: bool, name: &str, detail: String| {
            Line::from(vec![
                toggle(on),
                Span::styled(text::pad(name, 20), if on { t.fg(Role::Text) } else { t.fg(Role::Dim) }),
                Span::styled(detail, t.fg(Role::Dim)),
            ])
        };
        let chat = app.chat.as_ref();
        let settings = app.db.settings();
        let search_auto = settings.get("searchMode").and_then(Value::as_str) == Some("auto");
        let web_on = chat.is_some_and(|c| c.search_mode) || search_auto;
        let memory_global = settings.get("memoryEnabled").and_then(Value::as_bool).unwrap_or(true);
        let memory_on = chat.map(|c| c.memory_enabled && !c.temporary).unwrap_or(memory_global) && memory_global;
        let mut rows = vec![
            PickRow::Item(
                item(
                    web_on,
                    "Web search",
                    if search_auto { "on for every chat (app setting)".into() } else { "the model searches when it needs to".into() },
                ),
                ToolRow::Web,
                true,
            ),
            PickRow::Item(item(memory_on, "Memory", "remembers useful facts across chats".into()), ToolRow::Memory, true),
        ];
        let servers = app.db.mcp_servers().unwrap_or_default();
        if !servers.is_empty() {
            rows.push(PickRow::Header(Line::styled("MCP servers", t.fg(Role::Dim))));
            for server in servers {
                let on = chat.map(|c| c.tool_ids.contains(&server.id)).unwrap_or(server.enabled_by_default);
                let ask = if crate::mcp::server_always_allowed(&app.db, &server.id) { "runs without asking" } else { "asks before writing" };
                let detail = format!("{} · {ask}", text::plural(server.tools.len(), "tool", "tools"));
                rows.push(PickRow::Item(item(on, &server.name, detail), ToolRow::Server(server.id), true));
            }
        }
        let skills = app.db.skills().unwrap_or_default();
        if !skills.is_empty() {
            rows.push(PickRow::Header(Line::styled("Skills (all chats)", t.fg(Role::Dim))));
            for skill in skills {
                let mode = skill_mode_label(&skill.mode);
                rows.push(PickRow::Item(item(skill.enabled, &skill.name, mode.into()), ToolRow::Skill(skill.id), true));
            }
        }
        rows
    }

    fn draw(&self, app: &App, buf: &mut Buffer, screen: Rect) {
        let t = theme();
        let rows = Self::rows(app);
        let visible = rows.len().clamp(1, 14);
        let area = draw::dialog_rect(screen, 72, visible as u16 + 4, 5);
        draw::boxed(buf, area, t.fg(Role::Accent), "Tools for this chat", "esc close");
        let inner = draw::inner(area);
        draw_rows(buf, inner, inner.y, &rows, self.selected, inner.height.saturating_sub(2) as usize);
        put(
            buf,
            inner.x,
            inner.bottom() - 1,
            "↑↓ move · space toggle · /mcp adds servers · /skills adds skills",
            t.fg(Role::Dim),
            inner.right(),
        );
    }

    fn key(&mut self, app: &mut App, key: KeyEvent) -> bool {
        let rows = Self::rows(app);
        match key.code {
            KeyCode::Esc | KeyCode::Enter => return false,
            KeyCode::Up => self.selected = move_selection(&rows, self.selected, false),
            KeyCode::Down => self.selected = move_selection(&rows, self.selected, true),
            KeyCode::Char(' ') => {
                let Some(PickRow::Item(_, row, _)) = self.selected.and_then(|i| rows.get(i)) else {
                    return true;
                };
                let row = row.clone();
                toggle_tool(app, row);
            }
            _ => {}
        }
        true
    }
}

fn toggle_tool(app: &mut App, row: ToolRow) {
    if let ToolRow::Skill(id) = &row {
        let enabled = app.db.skills().unwrap_or_default().iter().find(|s| &s.id == id).map(|s| s.enabled);
        if let Some(enabled) = enabled {
            if let Err(error) = app.db.set_skill_enabled(id, !enabled) {
                app.fail(error.to_string());
            }
        }
        return;
    }
    let chat = match app.chat.clone() {
        Some(chat) => chat,
        None => match app.current_selection() {
            Some((provider, model)) => match app.db.create_chat(Some((&provider.id, &model))) {
                Ok(chat) => {
                    app.chat = Some(chat.clone());
                    chat
                }
                Err(error) => {
                    app.fail(error.to_string());
                    return;
                }
            },
            None => {
                app.fail("Add a provider first: /providers");
                return;
            }
        },
    };
    let result = match row {
        ToolRow::Web => app.db.set_chat_search(&chat.id, !chat.search_mode),
        ToolRow::Memory => app.db.set_chat_memory(&chat.id, !chat.memory_enabled),
        ToolRow::Server(id) => {
            let mut ids = chat.tool_ids.clone();
            if let Some(position) = ids.iter().position(|x| x == &id) {
                ids.remove(position);
            } else {
                ids.push(id);
            }
            app.db.set_chat_tools(&chat.id, &ids)
        }
        ToolRow::Skill(_) => Ok(()),
    };
    if let Err(error) = result {
        app.fail(error.to_string());
    }
    app.reload_messages();
}

pub fn skill_mode_label(mode: &str) -> &'static str {
    match mode {
        "always" => "always on",
        "manual" | "command" => "when I ask",
        _ => "automatic",
    }
}

// ------------------------------------------------------------------ MCP & skills

#[derive(Default)]
pub struct ListDialog {
    selected: usize,
}

fn draw_mcp(dialog: &ListDialog, app: &App, buf: &mut Buffer, screen: Rect) {
    let t = theme();
    let servers = app.db.mcp_servers().unwrap_or_default();
    let visible = servers.len().clamp(1, 12);
    let area = draw::dialog_rect(screen, 84, visible as u16 + 6, 5);
    draw::boxed(buf, area, t.fg(Role::Accent), "MCP servers", "esc close");
    let inner = draw::inner(area);
    put(
        buf,
        inner.x,
        inner.y,
        &format!("  {}{}{}STATUS", text::pad("NAME", 14), text::pad("TRANSPORT", 11), text::pad("TOOLS", 7)),
        t.fg(Role::Dim),
        inner.right(),
    );
    if servers.is_empty() {
        put(buf, inner.x, inner.y + 2, "No MCP servers yet · press a to add one", t.fg(Role::Dim), inner.right());
    }
    let first = window(dialog.selected, servers.len(), visible);
    for (offset, server) in servers.iter().skip(first).take(visible).enumerate() {
        let selected = first + offset == dialog.selected;
        let (status, role) = if server.connected || !server.tools.is_empty() {
            let allowed = crate::mcp::server_always_allowed(&app.db, &server.id);
            (
                if allowed { "✓ ready · runs without asking" } else { "✓ ready · asks before writing" }.to_string(),
                Role::Accent,
            )
        } else {
            ("! not connected · r to retry".to_string(), Role::Warn)
        };
        let line = Line::from(vec![
            Span::raw("  "),
            Span::styled(text::pad(&server.name, 14), t.fg(Role::Strong)),
            Span::styled(text::pad(&server.transport, 11), t.fg(Role::Text)),
            Span::styled(text::pad(&server.tools.len().to_string(), 7), t.fg(Role::Text)),
            Span::styled(status, t.fg(role)),
        ]);
        draw::row(buf, inner, inner.y + 1 + offset as u16, line, selected);
    }
    put(
        buf,
        inner.x,
        inner.bottom() - 1,
        "a add · r refresh · k token · p ask/allow · x remove",
        t.fg(Role::Dim),
        inner.right(),
    );
}

fn mcp_key(dialog: &mut ListDialog, app: &mut App, key: KeyEvent) -> bool {
    let servers = app.db.mcp_servers().unwrap_or_default();
    let selected = servers.get(dialog.selected).cloned();
    match key.code {
        KeyCode::Esc => return false,
        KeyCode::Up => dialog.selected = dialog.selected.saturating_sub(1),
        KeyCode::Down => dialog.selected = (dialog.selected + 1).min(servers.len().saturating_sub(1)),
        KeyCode::Char('a') => {
            app.dialog = Some(Dialog::Form(Form::add_mcp()));
            return true;
        }
        KeyCode::Char('r') | KeyCode::Enter => {
            if let Some(server) = selected {
                refresh_mcp(app, server.id, server.name);
            }
        }
        KeyCode::Char('k') => {
            if let Some(server) = selected {
                app.dialog = Some(Dialog::Form(Form::mcp_token(&server.id, &server.name)));
                return true;
            }
        }
        KeyCode::Char('p') => {
            if let Some(mut server) = selected {
                let allowed = crate::mcp::server_always_allowed(&app.db, &server.id);
                match crate::mcp::set_server_always_allowed(&app.db, &mut server, !allowed) {
                    Ok(()) => app.ok(if allowed {
                        format!("{} asks before writing", server.name)
                    } else {
                        format!("{} runs without asking", server.name)
                    }),
                    Err(error) => app.fail(error.to_string()),
                }
            }
        }
        KeyCode::Char('x') | KeyCode::Delete => {
            if let Some(server) = selected {
                app.dialog = Some(Dialog::Confirm(Confirm {
                    title: format!("Remove {}?", server.name),
                    body: "Its tools stop working in every chat.".into(),
                    yes: "remove",
                    action: ConfirmAction::RemoveMcp(server.id),
                }));
                return true;
            }
        }
        _ => {}
    }
    true
}

pub fn refresh_mcp(app: &mut App, id: String, name: String) {
    app.busy_task = Some(format!("connecting to {name}"));
    let ctx = app.ctx.clone();
    app.spawn_task(move || {
        let result = crate::db::Database::open(&ctx.db_path)
            .and_then(|db| crate::mcp::refresh(&db, &ctx.secrets, &id))
            .map_err(|error| super::app::describe_error(&error));
        Task::McpRefreshed { name, result }
    });
}

fn draw_skills(dialog: &ListDialog, app: &App, buf: &mut Buffer, screen: Rect) {
    let t = theme();
    let skills = app.db.skills().unwrap_or_default();
    let visible = skills.len().clamp(1, 14);
    let area = draw::dialog_rect(screen, 84, visible as u16 + 4, 5);
    draw::boxed(buf, area, t.fg(Role::Accent), "Skills", "esc close");
    let inner = draw::inner(area);
    if skills.is_empty() {
        put(buf, inner.x, inner.y, "No skills yet · press a to add a folder with a SKILL.md", t.fg(Role::Dim), inner.right());
    }
    let first = window(dialog.selected, skills.len(), visible);
    for (offset, skill) in skills.iter().skip(first).take(visible).enumerate() {
        let selected = first + offset == dialog.selected;
        let (mark, name_role, desc_role) = if skill.enabled {
            ("✓ ", Role::Strong, Role::Dim)
        } else {
            ("○ ", Role::Dim, Role::Dim)
        };
        let description = skill.description.replace('\n', " ");
        let mode = skill_mode_label(&skill.mode);
        let room = (inner.width as usize).saturating_sub(2 + 18 + 12);
        let line = Line::from(vec![
            Span::styled(mark, if skill.enabled { t.fg(Role::Accent) } else { t.fg(Role::Dim) }),
            Span::styled(text::pad(&skill.name, 18), t.fg(name_role)),
            Span::styled(text::pad(&description, room), t.fg(desc_role)),
            Span::styled(text::pad_left(mode, 12), t.fg(Role::Dim)),
        ]);
        draw::row(buf, inner, inner.y + offset as u16, line, selected);
    }
    put(
        buf,
        inner.x,
        inner.bottom() - 1,
        "space on/off · m when to use · a add · x remove · enter read",
        t.fg(Role::Dim),
        inner.right(),
    );
}

fn skills_key(dialog: &mut ListDialog, app: &mut App, key: KeyEvent) -> bool {
    let skills = app.db.skills().unwrap_or_default();
    let selected = skills.get(dialog.selected).cloned();
    match key.code {
        KeyCode::Esc => return false,
        KeyCode::Up => dialog.selected = dialog.selected.saturating_sub(1),
        KeyCode::Down => dialog.selected = (dialog.selected + 1).min(skills.len().saturating_sub(1)),
        KeyCode::Char('a') => {
            app.dialog = Some(Dialog::Form(Form::add_skill()));
            return true;
        }
        KeyCode::Char(' ') => {
            if let Some(skill) = selected {
                if let Err(error) = app.db.set_skill_enabled(&skill.id, !skill.enabled) {
                    app.fail(error.to_string());
                }
            }
        }
        KeyCode::Char('m') => {
            if let Some(skill) = selected {
                let next = match skill.mode.as_str() {
                    "auto" => "always",
                    "always" => "manual",
                    _ => "auto",
                };
                match app.db.set_skill_mode(&skill.id, next) {
                    Ok(()) => app.ok(format!("{}: {}", skill.name, skill_mode_label(next))),
                    Err(error) => app.fail(error.to_string()),
                }
            }
        }
        KeyCode::Char('x') | KeyCode::Delete => {
            if let Some(skill) = selected {
                app.dialog = Some(Dialog::Confirm(Confirm {
                    title: format!("Remove {}?", skill.name),
                    body: "The skill and its files are deleted.".into(),
                    yes: "remove",
                    action: ConfirmAction::RemoveSkill(skill.id),
                }));
                return true;
            }
        }
        KeyCode::Enter => {
            if let Some(skill) = selected {
                let mut lines = vec![
                    Line::styled(skill.description.clone(), theme().fg(Role::Dim)),
                    Line::default(),
                ];
                lines.extend(crate::ui::markdown::render(
                    &skill.body,
                    crate::ui::markdown::MdOptions {
                        width: 76,
                        indent: 0,
                        block_indent: 2,
                    },
                ));
                app.dialog = Some(Dialog::Text(TextView {
                    title: skill.name,
                    lines,
                    scroll: 0,
                }));
                return true;
            }
        }
        _ => {}
    }
    true
}

// ------------------------------------------------------------------ forms

#[derive(Clone, PartialEq)]
pub enum FormKind {
    AddProvider,
    EditProvider(String),
    ProviderKey(String),
    ProviderDefaults(String),
    ModelDefaults(String, String),
    AddMcp,
    McpToken(String),
    AddSkill,
}

#[derive(Clone, PartialEq)]
enum FieldKind {
    Text,
    Secret,
    Choice(Vec<&'static str>, usize),
}

#[derive(Clone)]
struct Field {
    label: &'static str,
    value: String,
    kind: FieldKind,
    /// Dim text after the value (or instead of it when empty).
    hint: String,
    empty: String,
}

impl Field {
    fn text(label: &'static str, value: impl Into<String>, empty: &str) -> Self {
        Self {
            label,
            value: value.into(),
            kind: FieldKind::Text,
            hint: String::new(),
            empty: empty.into(),
        }
    }

    fn secret(label: &'static str, hint: &str, empty: &str) -> Self {
        Self {
            label,
            value: String::new(),
            kind: FieldKind::Secret,
            hint: hint.into(),
            empty: empty.into(),
        }
    }

    fn choice(label: &'static str, options: Vec<&'static str>, index: usize) -> Self {
        Self {
            label,
            value: String::new(),
            kind: FieldKind::Choice(options, index),
            hint: String::new(),
            empty: String::new(),
        }
    }

    fn chosen(&self) -> &str {
        match &self.kind {
            FieldKind::Choice(options, index) => options.get(*index).copied().unwrap_or(""),
            _ => &self.value,
        }
    }
}

pub struct Form {
    title: String,
    kind: FormKind,
    fields: Vec<Field>,
    focus: usize,
    status: Option<(Role, String)>,
    testable: bool,
    pub token: u64,
}

const PRESETS: &[&str] = &["Custom", "OpenRouter", "Groq", "Ollama", "LM Studio", "vLLM", "FreeLLMAPI"];

impl Form {
    pub fn add_provider() -> Self {
        Self {
            title: "Add an OpenAI-compatible endpoint".into(),
            kind: FormKind::AddProvider,
            fields: vec![
                Field::choice("Preset", PRESETS.to_vec(), 0),
                Field::text("Name", "", "a name you will recognise"),
                Field::text("Base URL", "", "ends in /v1"),
                Field::secret("API key", "saved in your keychain", "leave empty if none"),
                Field::text("Headers", "", "none · Name: value, Name: value"),
            ],
            focus: 0,
            status: None,
            testable: true,
            token: 0,
        }
    }

    pub fn edit_provider(app: &App, id: &str) -> Option<Self> {
        let provider = app.db.provider(id).ok().flatten()?;
        let mut form = Self::add_provider();
        form.title = format!("Edit {}", provider.name);
        form.kind = FormKind::EditProvider(provider.id.clone());
        form.fields.remove(0);
        form.fields[0].value = provider.name;
        form.fields[1].value = provider.base_url;
        form.fields[2].empty = "leave empty to keep the saved key".into();
        form.fields[3].value = headers_text(&provider.headers);
        Some(form)
    }

    pub fn provider_key(id: &str, name: &str) -> Self {
        Self {
            title: format!("API key for {name}"),
            kind: FormKind::ProviderKey(id.into()),
            fields: vec![Field::secret("API key", "saved in your keychain", "paste the key")],
            focus: 0,
            status: None,
            testable: false,
            token: 0,
        }
    }

    pub fn provider_defaults(app: &App, id: &str) -> Option<Self> {
        let provider = app.db.provider(id).ok().flatten()?;
        let settings = app.db.settings();
        let params = settings
            .pointer(&format!("/providerParams/{id}"))
            .cloned()
            .or_else(|| settings.get("defaultParams").cloned())
            .unwrap_or_else(|| json!({}));
        Some(Self {
            title: format!("Defaults for {}", provider.name),
            kind: FormKind::ProviderDefaults(id.into()),
            fields: vec![
                Field::text("Temperature", number_text(params.get("temperature"), "0.7"), "0 to 2"),
                Field::text("Max tokens", number_text(params.get("maxTokens"), "4096"), "a whole number"),
                Field::text("Timeout", (provider.timeout_ms / 1000).to_string(), "seconds"),
                Field::text("Headers", headers_text(&provider.headers), "none · Name: value, Name: value"),
            ],
            focus: 0,
            status: None,
            testable: false,
            token: 0,
        })
    }

    pub fn model_defaults(app: &App, provider_id: &str, model_id: &str) -> Self {
        let defaults = app
            .db
            .model(provider_id, model_id)
            .ok()
            .flatten()
            .map(|m| m.defaults)
            .unwrap_or_else(|| json!({}));
        let effort = defaults.get("reasoning").and_then(Value::as_str).unwrap_or("off");
        let efforts = vec!["off", "low", "medium", "high"];
        let index = efforts.iter().position(|e| *e == effort).unwrap_or(0);
        Self {
            title: format!("Defaults for {model_id}"),
            kind: FormKind::ModelDefaults(provider_id.into(), model_id.into()),
            fields: vec![
                Field::text("Temperature", number_text(defaults.get("temperature"), ""), "provider default"),
                Field::text("Max tokens", number_text(defaults.get("maxTokens"), ""), "provider default"),
                Field::text("Top p", number_text(defaults.get("topP"), ""), "provider default"),
                Field::choice("Reasoning", efforts, index),
            ],
            focus: 0,
            status: None,
            testable: false,
            token: 0,
        }
    }

    pub fn add_mcp() -> Self {
        Self {
            title: "Add a remote MCP server".into(),
            kind: FormKind::AddMcp,
            fields: vec![
                Field::text("Name", "", "github"),
                Field::text("URL", "", "https://…/mcp"),
                Field::secret("Token", "saved in your keychain", "leave empty if none"),
            ],
            focus: 0,
            status: None,
            testable: false,
            token: 0,
        }
    }

    pub fn mcp_token(id: &str, name: &str) -> Self {
        Self {
            title: format!("Access token for {name}"),
            kind: FormKind::McpToken(id.into()),
            fields: vec![Field::secret("Token", "saved in your keychain", "paste the token")],
            focus: 0,
            status: None,
            testable: false,
            token: 0,
        }
    }

    pub fn add_skill() -> Self {
        Self {
            title: "Add a skill".into(),
            kind: FormKind::AddSkill,
            fields: vec![Field::text("Folder", "", "a folder with SKILL.md, or a SKILL.md file")],
            focus: 0,
            status: None,
            testable: false,
            token: 0,
        }
    }

    pub fn paste(&mut self, text: &str) {
        if let Some(field) = self.fields.get_mut(self.focus) {
            if !matches!(field.kind, FieldKind::Choice(..)) {
                field.value.push_str(text.trim_end_matches(['\n', '\r']));
            }
        }
    }

    pub fn set_test_result(&mut self, result: Result<(u64, Vec<String>), String>) {
        self.status = Some(match result {
            Ok((latency, models)) => (
                Role::Accent,
                format!(
                    "✓ Connected · {} · {} found",
                    text::duration(latency),
                    text::plural(models.len(), "model", "models")
                ),
            ),
            Err(error) => (Role::Error, format!("✗ {error}")),
        });
    }

    fn field(&self, label: &str) -> String {
        self.fields
            .iter()
            .find(|f| f.label == label)
            .map(|f| f.chosen().trim().to_string())
            .unwrap_or_default()
    }

    fn rows(&self) -> u16 {
        let choice_gap = u16::from(matches!(self.fields.first().map(|f| &f.kind), Some(FieldKind::Choice(..))) && self.fields.len() > 1);
        self.fields.len() as u16 + choice_gap + 2 + if self.status.is_some() { 2 } else { 0 }
    }

    fn draw(&self, buf: &mut Buffer, screen: Rect) {
        let t = theme();
        let area = draw::dialog_rect(screen, 76, self.rows() + 2, 6);
        draw::boxed(buf, area, t.fg(Role::Accent), &self.title, "");
        let inner = draw::inner(area);
        let label_width = self.fields.iter().map(|f| width(f.label)).max().unwrap_or(8).max(8) as u16 + 2;
        let mut y = inner.y;
        for (index, field) in self.fields.iter().enumerate() {
            let focused = index == self.focus;
            let label_style = if focused { t.fg(Role::Accent) } else { t.fg(Role::Dim) };
            put(buf, inner.x, y, field.label, label_style, inner.right());
            let x = inner.x + label_width;
            match &field.kind {
                FieldKind::Choice(options, chosen) => {
                    draw_choices(buf, x, y, inner.right(), options, *chosen);
                }
                FieldKind::Secret => {
                    let end = if field.value.is_empty() {
                        if !focused {
                            put(buf, x, y, &field.empty, t.fg(Role::Dim), inner.right())
                        } else {
                            x
                        }
                    } else {
                        let dots = "•".repeat(field.value.chars().count().min(24));
                        let end = put(buf, x, y, &dots, t.fg(Role::Text), inner.right());
                        put(buf, x + 25, y, &field.hint, t.fg(Role::Dim), inner.right());
                        end
                    };
                    if focused {
                        cursor_at(buf, end, y);
                        if field.value.is_empty() {
                            put(buf, end + 2, y, &field.empty, t.fg(Role::Dim), inner.right());
                        }
                    }
                }
                FieldKind::Text => {
                    let shown = text::truncate(&field.value, (inner.right() - x).saturating_sub(2) as usize);
                    let end = put(buf, x, y, &shown, t.fg(Role::Strong), inner.right());
                    if field.value.is_empty() {
                        let hint_x = if focused { end + 2 } else { end };
                        put(buf, hint_x, y, &field.empty, t.fg(Role::Dim), inner.right());
                    }
                    if focused {
                        cursor_at(buf, end, y);
                    }
                }
            }
            y += 1;
            if index == 0 && matches!(field.kind, FieldKind::Choice(..)) && self.fields.len() > 1 {
                y += 1;
            }
        }
        if let Some((role, status)) = &self.status {
            y += 1;
            put(buf, inner.x, y, &text::truncate(status, inner.width as usize), t.fg(*role), inner.right());
            y += 1;
        }
        y += 1;
        let x = put(buf, inner.x + 1, y, " Save ", t.badge(), inner.right());
        let rest = if self.testable { "   Test (ctrl+t)   Cancel (esc)" } else { "   Cancel (esc)" };
        let rest = if self.testable && self.status.is_some() { "   Test again (ctrl+t)   Cancel (esc)" } else { rest };
        put(buf, x, y, rest, t.fg(Role::Dim), inner.right());
    }

    fn key(&mut self, app: &mut App, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let count = self.fields.len();
        match key.code {
            KeyCode::Esc => return false,
            KeyCode::Tab | KeyCode::Down => self.focus = (self.focus + 1) % count,
            KeyCode::BackTab | KeyCode::Up => self.focus = (self.focus + count - 1) % count,
            KeyCode::Char('t') if ctrl && self.testable => self.test(app),
            KeyCode::Char('u') if ctrl => self.fields[self.focus].value.clear(),
            KeyCode::Enter => return !self.save(app),
            KeyCode::Left | KeyCode::Right => {
                let forward = key.code == KeyCode::Right;
                let field = &mut self.fields[self.focus];
                if let FieldKind::Choice(options, index) = &mut field.kind {
                    *index = if forward { (*index + 1) % options.len() } else { (*index + options.len() - 1) % options.len() };
                    if self.kind == FormKind::AddProvider && self.focus == 0 {
                        self.apply_preset();
                    }
                }
            }
            KeyCode::Backspace => {
                self.fields[self.focus].value.pop();
            }
            KeyCode::Char(c) if !ctrl => {
                let field = &mut self.fields[self.focus];
                if matches!(field.kind, FieldKind::Choice(..)) {
                    if c == ' ' {
                        if let FieldKind::Choice(options, index) = &mut field.kind {
                            *index = (*index + 1) % options.len();
                        }
                        if self.kind == FormKind::AddProvider && self.focus == 0 {
                            self.apply_preset();
                        }
                    }
                } else {
                    field.value.push(c);
                }
            }
            _ => {}
        }
        true
    }

    fn apply_preset(&mut self) {
        let preset = self.fields[0].chosen().to_string();
        let url = crate::providers::preset_url(&preset).unwrap_or("");
        let name_is_preset = self.fields[1].value.is_empty() || PRESETS.contains(&self.fields[1].value.as_str());
        if name_is_preset {
            self.fields[1].value = if preset == "Custom" { String::new() } else { preset };
        }
        if self.fields[2].value.is_empty() || crate::providers::PRESETS.iter().any(|(_, u)| *u == self.fields[2].value) {
            self.fields[2].value = url.to_string();
        }
    }

    fn test(&mut self, app: &mut App) {
        let url = self.field("Base URL");
        if url.is_empty() {
            self.status = Some((Role::Warn, "! Type the base URL first".into()));
            return;
        }
        let mut key = self.field("API key");
        if key.is_empty() {
            if let FormKind::EditProvider(id) = &self.kind {
                key = app.secrets.provider_key(id).unwrap_or_default();
            }
        }
        let headers = match parse_headers(&self.field("Headers")) {
            Ok(headers) => headers,
            Err(error) => {
                self.status = Some((Role::Error, format!("✗ {error}")));
                return;
            }
        };
        app.form_token += 1;
        self.token = app.form_token;
        let token = self.token;
        self.status = Some((Role::Dim, "◇ Testing connection…".into()));
        app.spawn_task(move || Task::FormTested {
            token,
            result: super::tasks::test_endpoint(&url, &key, &headers),
        });
    }

    /// Returns true when the form is done and should close.
    fn save(&mut self, app: &mut App) -> bool {
        match self.save_inner(app) {
            Ok(done) => done,
            Err(error) => {
                self.status = Some((Role::Error, format!("✗ {error}")));
                false
            }
        }
    }

    fn save_inner(&mut self, app: &mut App) -> Result<bool, String> {
        let kind = self.kind.clone();
        match kind {
            FormKind::AddProvider | FormKind::EditProvider(_) => {
                let name = self.field("Name");
                let url = self.field("Base URL");
                if name.is_empty() {
                    self.focus = self.fields.iter().position(|f| f.label == "Name").unwrap_or(0);
                    return Err("Give the endpoint a name".into());
                }
                if !(url.starts_with("http://") || url.starts_with("https://")) {
                    self.focus = self.fields.iter().position(|f| f.label == "Base URL").unwrap_or(0);
                    return Err("The base URL starts with http:// or https://".into());
                }
                let headers = parse_headers(&self.field("Headers"))?;
                let key = self.field("API key");
                let id = match &kind {
                    FormKind::EditProvider(id) => {
                        app.db.update_provider(id, &name, &url, &headers).map_err(|e| e.to_string())?;
                        id.clone()
                    }
                    _ => app.db.insert_provider(&name, &url, &headers, 60_000).map_err(|e| e.to_string())?,
                };
                if !key.is_empty() {
                    app.secrets.set(&format!("apikey.{id}"), &key).map_err(|e| e.to_string())?;
                }
                app.refresh_models(Some(id.clone()));
                if let Page::Providers(page) = &mut app.page {
                    page.select_provider(&app.db, &id);
                    page.refresh_keys = true;
                }
                app.ok(if kind == FormKind::AddProvider {
                    format!("Added {name} · loading its models")
                } else {
                    format!("Saved {name}")
                });
                Ok(true)
            }
            FormKind::ProviderKey(id) => {
                let key = self.field("API key");
                if key.is_empty() {
                    return Err("Paste the key first".into());
                }
                app.secrets.set(&format!("apikey.{id}"), &key).map_err(|e| e.to_string())?;
                if let Page::Providers(page) = &mut app.page {
                    page.refresh_keys = true;
                }
                super::providers::test_provider(app, &id);
                app.ok("Key saved · testing it");
                Ok(true)
            }
            FormKind::ProviderDefaults(id) => {
                let temperature: f64 = self
                    .field("Temperature")
                    .parse()
                    .ok()
                    .filter(|v: &f64| (0.0..=2.0).contains(v))
                    .ok_or("Temperature is a number from 0 to 2")?;
                let max_tokens: u64 = self
                    .field("Max tokens")
                    .parse()
                    .ok()
                    .filter(|v: &u64| *v > 0)
                    .ok_or("Max tokens is a whole number above 0")?;
                let timeout: u64 = self
                    .field("Timeout")
                    .trim_end_matches('s')
                    .parse()
                    .ok()
                    .filter(|v: &u64| (1..=600).contains(v))
                    .ok_or("Timeout is 1 to 600 seconds")?;
                let headers = parse_headers(&self.field("Headers"))?;
                app.db.update_provider_defaults(&id, &headers, timeout * 1000).map_err(|e| e.to_string())?;
                let mut settings = app.db.settings();
                if !settings.get("providerParams").is_some_and(Value::is_object) {
                    settings["providerParams"] = json!({});
                }
                settings["providerParams"][&id] = json!({"temperature": temperature, "maxTokens": max_tokens});
                app.db.set_kv("settings", &settings.to_string()).map_err(|e| e.to_string())?;
                app.ok("Defaults saved · new chats use them");
                Ok(true)
            }
            FormKind::ModelDefaults(provider_id, model_id) => {
                let mut defaults = app
                    .db
                    .model(&provider_id, &model_id)
                    .ok()
                    .flatten()
                    .map(|m| m.defaults)
                    .filter(Value::is_object)
                    .unwrap_or_else(|| json!({}));
                for (label, key) in [("Temperature", "temperature"), ("Max tokens", "maxTokens"), ("Top p", "topP")] {
                    let raw = self.field(label);
                    if raw.is_empty() {
                        if let Some(map) = defaults.as_object_mut() {
                            map.remove(key);
                        }
                        continue;
                    }
                    let number: f64 = raw.parse().map_err(|_| format!("{label} must be a number"))?;
                    defaults[key] = if key == "maxTokens" { json!(number as u64) } else { json!(number) };
                }
                defaults["reasoning"] = json!(self.field("Reasoning"));
                app.db
                    .set_model_defaults(&provider_id, &model_id, &defaults)
                    .map_err(|e| e.to_string())?;
                if let Some(chat) = app.chat.clone() {
                    if chat.provider_id.as_deref() == Some(&provider_id) && chat.model_id.as_deref() == Some(&model_id) {
                        let params = app.db.default_params(&provider_id, &model_id);
                        let _ = app.db.set_chat_params(&chat.id, &params);
                        app.reload_messages();
                    }
                }
                app.ok(format!("Saved defaults for {model_id}"));
                Ok(true)
            }
            FormKind::AddMcp => {
                let name = self.field("Name");
                let url = self.field("URL");
                if name.is_empty() || !url.starts_with("http") {
                    return Err("Give it a name and an https:// URL".into());
                }
                let id = app.db.add_mcp_server(&name, &url).map_err(|e| e.to_string())?;
                let token = self.field("Token");
                if !token.is_empty() {
                    app.secrets.set_mcp_token(&id, &token).map_err(|e| e.to_string())?;
                }
                refresh_mcp(app, id, name);
                app.dialog = Some(Dialog::Mcp(ListDialog::default()));
                Ok(false)
            }
            FormKind::McpToken(id) => {
                let token = self.field("Token");
                if token.is_empty() {
                    return Err("Paste the token first".into());
                }
                app.secrets.set_mcp_token(&id, &token).map_err(|e| e.to_string())?;
                let name = app.db.mcp_servers().unwrap_or_default().into_iter().find(|s| s.id == id).map(|s| s.name).unwrap_or_default();
                refresh_mcp(app, id, name);
                app.dialog = Some(Dialog::Mcp(ListDialog::default()));
                Ok(false)
            }
            FormKind::AddSkill => {
                let path = super::files::expand(&self.field("Folder"));
                let (name, description, body, source, files) =
                    crate::skills::read_skill_source(&path).map_err(|e| format!("{e:#}"))?;
                app.db
                    .save_skill_with_files(&name, &description, &body, &source, &files)
                    .map_err(|e| e.to_string())?;
                app.ok(format!("Added {name}"));
                app.dialog = Some(Dialog::Skills(ListDialog::default()));
                Ok(false)
            }
        }
    }
}

fn cursor_at(buf: &mut Buffer, x: u16, y: u16) {
    if let Some(cell) = buf.cell_mut((x, y)) {
        cell.set_style(theme().cursor());
    }
}

/// ` Custom `  OpenRouter  Groq … with the chosen one highlighted, scrolled
/// so the chosen option is always visible.
fn draw_choices(buf: &mut Buffer, x: u16, y: u16, right: u16, options: &[&str], chosen: usize) {
    let t = theme();
    let room = right.saturating_sub(x) as usize;
    let mut first = 0;
    loop {
        let used: usize = options[first..=chosen].iter().map(|o| width(o) + 2).sum();
        if used <= room || first == chosen {
            break;
        }
        first += 1;
    }
    let mut cursor = x;
    for (index, option) in options.iter().enumerate().skip(first) {
        let label = format!(" {option} ");
        if cursor + width(&label) as u16 > right {
            break;
        }
        let style = if index == chosen { t.selected(Role::Accent) } else { t.fg(Role::Text) };
        cursor = put(buf, cursor, y, &label, style, right);
        cursor = put(buf, cursor, y, " ", Style::default(), right);
    }
}

fn number_text(value: Option<&Value>, fallback: &str) -> String {
    match value {
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::String(s)) => s.clone(),
        _ => fallback.to_string(),
    }
}

pub fn headers_text(headers: &Value) -> String {
    headers
        .as_object()
        .map(|map| {
            map.iter()
                .map(|(k, v)| format!("{k}: {}", v.as_str().map(str::to_owned).unwrap_or_else(|| v.to_string())))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}

/// `Name: value, Name: value` (or a JSON object) → headers.
pub fn parse_headers(text: &str) -> Result<Value, String> {
    let text = text.trim();
    if text.is_empty() || text == "none" {
        return Ok(json!({}));
    }
    if text.starts_with('{') {
        return serde_json::from_str::<Value>(text)
            .ok()
            .filter(Value::is_object)
            .ok_or_else(|| "Headers must be a JSON object or Name: value pairs".to_string());
    }
    let mut map = serde_json::Map::new();
    for pair in text.split([',', ';', '\n']) {
        let pair = pair.trim();
        if pair.is_empty() {
            continue;
        }
        let (name, value) = pair
            .split_once(':')
            .ok_or_else(|| format!("“{pair}” needs a colon, like X-Title: AI Hat"))?;
        map.insert(name.trim().to_string(), json!(value.trim()));
    }
    Ok(Value::Object(map))
}

// ------------------------------------------------------------------ confirm & text

pub enum ConfirmAction {
    DeleteChat(String),
    RemoveProvider(String),
    RemoveMcp(String),
    RemoveSkill(String),
}

pub struct Confirm {
    pub title: String,
    pub body: String,
    pub yes: &'static str,
    pub action: ConfirmAction,
}

impl Confirm {
    fn draw(&self, buf: &mut Buffer, screen: Rect) {
        let t = theme();
        let area = draw::dialog_rect(screen, 64, 6, 8);
        draw::boxed(buf, area, t.fg(Role::Warn), &self.title, "");
        let inner = draw::inner(area);
        put(buf, inner.x, inner.y, &self.body, t.fg(Role::Text), inner.right());
        let x = put(buf, inner.x + 1, inner.y + 2, &format!(" y {} ", self.yes), t.badge(), inner.right());
        put(buf, x, inner.y + 2, "   n keep (esc)", t.fg(Role::Dim), inner.right());
    }

    fn key(&mut self, app: &mut App, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Char('y') | KeyCode::Enter => {
                let result = match &self.action {
                    ConfirmAction::DeleteChat(id) => {
                        let current = app.chat.as_ref().is_some_and(|c| &c.id == id);
                        let result = app.db.delete_chat(id);
                        if current && result.is_ok() {
                            app.new_chat();
                        }
                        app.dialog = Some(Dialog::History(HistoryPicker::new(app)));
                        result.map(|_| "Chat deleted")
                    }
                    ConfirmAction::RemoveProvider(id) => {
                        let _ = app.secrets.delete(&format!("apikey.{id}"));
                        let result = app.db.remove_provider(id);
                        if let Page::Providers(page) = &mut app.page {
                            page.selection = page.selection.saturating_sub(1);
                            page.refresh_keys = true;
                        }
                        result.map(|_| "Removed · chats that used it stay")
                    }
                    ConfirmAction::RemoveMcp(id) => {
                        let _ = app.secrets.delete(&format!("mcp.{id}"));
                        app.dialog = Some(Dialog::Mcp(ListDialog::default()));
                        app.db.remove_mcp_server(id).map(|_| "Removed")
                    }
                    ConfirmAction::RemoveSkill(id) => {
                        app.dialog = Some(Dialog::Skills(ListDialog::default()));
                        app.db.remove_skill(id).map(|_| "Removed")
                    }
                };
                match result {
                    Ok(message) => app.ok(message),
                    Err(error) => app.fail(error.to_string()),
                }
                // Close; a follow-up dialog opened above stays open.
                false
            }
            KeyCode::Char('n') | KeyCode::Esc => false,
            _ => true,
        }
    }
}

pub struct TextView {
    pub title: String,
    pub lines: Vec<Line<'static>>,
    pub scroll: usize,
}

impl TextView {
    fn draw(&self, buf: &mut Buffer, screen: Rect) {
        let t = theme();
        let height = (screen.height.saturating_sub(8)).max(8);
        let area = draw::dialog_rect(screen, 84, height, 3);
        draw::boxed(buf, area, t.fg(Role::Accent), &self.title, "↑↓ scroll · esc close");
        let inner = draw::inner(area);
        for (offset, line) in self.lines.iter().skip(self.scroll).take(inner.height as usize).enumerate() {
            put_line(buf, inner.x, inner.y + offset as u16, line, inner.right());
        }
    }

    fn key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => return false,
            KeyCode::Up => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::Down => self.scroll = (self.scroll + 1).min(self.lines.len().saturating_sub(1)),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(10),
            KeyCode::PageDown => self.scroll = (self.scroll + 10).min(self.lines.len().saturating_sub(1)),
            _ => {}
        }
        true
    }
}

// ------------------------------------------------------------------ approval

/// The orange "Allow this tool?" box shown in the chat while a write tool waits.
pub fn approval_lines(pending: &PendingApproval, total_width: usize) -> Vec<Line<'static>> {
    let t = theme();
    let request = &pending.request;
    let border = t.fg(Role::Warn);
    let inner = total_width.saturating_sub(4).max(20);
    let mut body: Vec<Line<'static>> = vec![Line::from(vec![
        Span::styled(request.server.clone(), t.fg(Role::Strong)),
        Span::styled(" wants to run ", t.fg(Role::Text)),
        Span::styled(request.tool.clone(), t.fg(Role::Link)),
    ])];
    body.push(Line::default());
    if let Some(map) = request.arguments.as_object() {
        let key_width = map.keys().map(|k| width(k)).max().unwrap_or(0).min(14);
        for (key, value) in map.iter().take(8) {
            let value = value
                .as_str()
                .map(|s| s.replace('\n', " "))
                .unwrap_or_else(|| value.to_string());
            body.push(Line::from(vec![
                Span::styled(text::pad(key, key_width), t.fg(Role::Dim)),
                Span::raw(" "),
                Span::styled(text::truncate(&value, inner.saturating_sub(key_width + 1)), t.fg(Role::Text)),
            ]));
        }
        if map.len() > 8 {
            body.push(Line::styled(format!("… {} more", map.len() - 8), t.fg(Role::Dim)));
        }
        body.push(Line::default());
    }
    let always = format!("Always for {}", request.server);
    let options = [
        ("Allow once", "enter"),
        (always.as_str(), "a"),
        ("Edit the arguments", "e"),
        ("Deny", "esc"),
    ];
    let mut option_rows = Vec::new();
    for (index, (label, key)) in options.iter().enumerate() {
        let selected = index == pending.selection;
        let marker = if selected { "› " } else { "  " };
        let style = if selected { t.selected(Role::Accent) } else { t.fg(Role::Text) };
        option_rows.push((
            Line::from(vec![
                Span::styled(format!(" {marker}{}", text::pad(label, 20)), style),
                Span::raw(" "),
                Span::styled(key.to_string(), t.fg(Role::Dim)),
            ]),
            selected,
        ));
    }
    let kind = if request.writes { "MCP · writes data" } else { "MCP · reads data" };
    let g = t.glyphs();
    let title = format!("{} Allow this tool? ", g.horizontal);
    let right = format!(" {kind} {}", g.horizontal);
    let fill = (total_width.saturating_sub(2)).saturating_sub(width(&title) + width(&right));
    let mut out = vec![Line::styled(
        format!("{}{title}{}{right}{}", g.top_left, g.horizontal.repeat(fill), g.top_right),
        border,
    )];
    let pad_row = |line: Line<'static>| -> Line<'static> {
        let line = text::clip_line(line, inner);
        let gap = inner.saturating_sub(text::line_width(&line));
        let mut spans = vec![Span::styled(g.vertical.to_string(), border), Span::raw(" ")];
        spans.extend(line.spans);
        spans.push(Span::raw(" ".repeat(gap + 1)));
        spans.push(Span::styled(g.vertical.to_string(), border));
        Line::from(spans)
    };
    for line in body {
        out.push(pad_row(line));
    }
    for (line, _) in option_rows {
        out.push(pad_row(line));
    }
    out.push(Line::styled(
        format!("{}{}{}", g.bottom_left, g.horizontal.repeat(total_width.saturating_sub(2)), g.bottom_right),
        border,
    ));
    out
}

// ------------------------------------------------------------------ dispatch

pub fn draw(app: &App, buf: &mut Buffer, screen: Rect) {
    match &app.dialog {
        Some(Dialog::Models(picker)) => picker.draw(app, buf, screen),
        Some(Dialog::Prompts(picker)) => picker.draw(app, buf, screen),
        Some(Dialog::History(picker)) => picker.draw(app, buf, screen),
        Some(Dialog::Keys) => draw_keys(buf, screen),
        Some(Dialog::Tools(dialog)) => dialog.draw(app, buf, screen),
        Some(Dialog::Mcp(dialog)) => draw_mcp(dialog, app, buf, screen),
        Some(Dialog::Skills(dialog)) => draw_skills(dialog, app, buf, screen),
        Some(Dialog::Form(form)) => form.draw(buf, screen),
        Some(Dialog::Confirm(confirm)) => confirm.draw(buf, screen),
        Some(Dialog::Text(view)) => view.draw(buf, screen),
        None => {}
    }
}

/// Route a key to the open dialog. The dialog is taken out while it runs so
/// it can change the app; it goes back unless it closed or opened another.
pub fn on_key(app: &mut App, key: KeyEvent) {
    let Some(mut dialog) = app.dialog.take() else {
        return;
    };
    let keep = match &mut dialog {
        Dialog::Models(picker) => picker.key(app, key),
        Dialog::Prompts(picker) => picker.key(app, key),
        Dialog::History(picker) => picker.key(app, key),
        Dialog::Keys => !matches!(key.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('?') | KeyCode::Char('q')),
        Dialog::Tools(tools) => tools.key(app, key),
        Dialog::Mcp(list) => mcp_key(list, app, key),
        Dialog::Skills(list) => skills_key(list, app, key),
        Dialog::Form(form) => form.key(app, key),
        Dialog::Confirm(confirm) => confirm.key(app, key),
        Dialog::Text(view) => view.key(key),
    };
    if keep && app.dialog.is_none() {
        app.dialog = Some(dialog);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headers_parse_from_pairs_and_json() {
        assert_eq!(parse_headers("").unwrap(), json!({}));
        assert_eq!(parse_headers("X-Title: AI Hat, HTTP-Referer: https://x").unwrap(), json!({"X-Title": "AI Hat", "HTTP-Referer": "https://x"}));
        assert_eq!(parse_headers("{\"a\":\"b\"}").unwrap(), json!({"a": "b"}));
        assert!(parse_headers("broken").is_err());
    }
}
