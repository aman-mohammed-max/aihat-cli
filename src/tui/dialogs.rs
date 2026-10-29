//! Boxes drawn over the dimmed chat: pickers, forms, confirmations, keys.

use super::app::{App, Page, PendingApproval, Task};
use super::draw::{self, put, put_line};
use crate::models::Model;
use crate::ui::text::{self, width};
use crate::ui::theme::{Role, theme};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
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
    Image(Box<super::images::ImageViewer>),
    Memory(MemoryDialog),
    About(super::about::AboutDialog),
    Effort(EffortPicker),
    Folder(super::folder::FolderDialog),
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

/// The prompt list's last row: write a new prompt with AI.
const WRITE_NEW: &str = "new:ai";

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
        if score(&self.query, "new write ai").is_some() || self.query.is_empty() {
            rows.push(PickRow::Item(
                Line::from(vec![
                    Span::styled("+ ", t.fg(Role::Accent)),
                    Span::styled("Write a new one with AI", t.fg(Role::Link)),
                    Span::styled("  describe it, the model writes it", t.fg(Role::Dim)),
                ]),
                Some(WRITE_NEW.to_string()),
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
        put(buf, inner.x, inner.bottom() - 1, &format!("↑↓ move · enter pick · {scope} · ctrl+e edit"), t.fg(Role::Dim), inner.right());
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
                    if id.as_deref() == Some(WRITE_NEW) {
                        app.dialog = Some(Dialog::Form(Form::write_prompt()));
                        return false;
                    }
                    app.apply_prompt(id.clone(), self.make_default);
                    return false;
                }
            }
            KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(PickRow::Item(_, Some(id), _)) = self.selected.and_then(|i| rows.get(i))
                    && id != WRITE_NEW
                {
                    app.edit_prompt = Some(id.clone());
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

// ------------------------------------------------------------------ effort

/// How much the model thinks before answering, as `/effort` in the app:
/// sent as `reasoning_effort`; models without reasoning ignore it.
pub const EFFORTS: [(&str, &str); 4] = [
    ("off", "let the model decide"),
    ("low", "quick answers"),
    ("medium", "thinks it through"),
    ("high", "thinks longest, for hard problems"),
];

pub struct EffortPicker {
    selected: Option<usize>,
}

impl EffortPicker {
    pub fn new(app: &App) -> Self {
        let current = app.current_effort();
        Self { selected: Some(EFFORTS.iter().position(|(level, _)| *level == current).unwrap_or(0)) }
    }

    fn rows(&self, app: &App) -> Vec<PickRow<&'static str>> {
        let t = theme();
        let current = app.current_effort();
        EFFORTS
            .iter()
            .map(|(level, about)| {
                let mut label = level.to_string();
                label[..1].make_ascii_uppercase();
                PickRow::Item(
                    Line::from(vec![
                        Span::styled(if *level == current { "● " } else { "  " }, t.fg(Role::Accent)),
                        Span::styled(text::pad(&label, 10), t.fg(Role::Text)),
                        Span::styled(*about, t.fg(Role::Dim)),
                    ]),
                    *level,
                    true,
                )
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
        let area = draw::dialog_rect(screen, 64, rows.len() as u16 + 6, 5);
        let scope = if app.chat.is_some() { "this chat" } else { "new chats" };
        draw::boxed(buf, area, t.fg(Role::Accent), "Reasoning effort", scope);
        let inner = draw::inner(area);
        put(buf, inner.x, inner.y, "How much the model thinks before it answers.", t.fg(Role::Dim), inner.right());
        draw_rows(buf, inner, inner.y + 2, &rows, self.selected, rows.len());
        put(buf, inner.x, inner.bottom() - 1, "↑↓ move · enter pick · esc close", t.fg(Role::Dim), inner.right());
    }

    fn key(&mut self, app: &mut App, key: KeyEvent) -> bool {
        let rows = self.rows(app);
        match key.code {
            KeyCode::Esc => return false,
            KeyCode::Up => self.selected = move_selection(&rows, self.selected, false),
            KeyCode::Down | KeyCode::Tab => self.selected = move_selection(&rows, self.selected, true),
            KeyCode::Enter => {
                if let Some(PickRow::Item(_, level, _)) = self.selected.and_then(|i| rows.get(i)) {
                    app.set_effort(level);
                    return false;
                }
            }
            // The first letter picks one: o, l, m, h.
            KeyCode::Char(c) => {
                if let Some((level, _)) = EFFORTS.iter().find(|(level, _)| level.starts_with(c.to_ascii_lowercase())) {
                    app.set_effort(level);
                    return false;
                }
            }
            _ => {}
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
                // The design draws the chosen chat two columns further left.
                PickRow::Item(line, key, ok) if Some(index) == self.selected => PickRow::Item(emphasise(line, true), key, ok),
                PickRow::Item(line, key, ok) => {
                    let mut spans = vec![Span::raw("  ")];
                    spans.extend(line.spans);
                    PickRow::Item(Line::from(spans), key, ok)
                }
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
    use super::keys::{Action, label};
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
        let key_width = key_width.max(width(key) + 1);
        put(buf, x, y, &text::pad(key, key_width), t.fg(Role::Link), right);
        put(buf, x + key_width as u16, y, what, t.fg(Role::Text), right);
    };
    let mut y = inner.y;
    head(buf, y, "Chat", "Anywhere");
    let chat = [
        (label(Action::Send), "send"),
        (label(Action::NewLine), "new line"),
        (label(Action::Stop), "stop the reply"),
        ("↑", "edit your last message"),
        (label(Action::ToggleReasoning), "show or hide reasoning"),
        (label(Action::CopyReply), "copy the last reply"),
    ];
    let anywhere = [
        (label(Action::SwitchModel), "switch model"),
        (label(Action::SwitchPrompt), "switch system prompt"),
        (label(Action::PastChats), "past chats"),
        (label(Action::NewChat), "new chat"),
        (label(Action::ClearScreen), "clear the screen"),
        (label(Action::Quit), "twice to quit"),
    ];
    for ((left_key, left_what), (right_key, right_what)) in chat.iter().zip(anywhere.iter()) {
        y += 1;
        pair(buf, inner.x, y, left_key, 13, left_what, half - 1);
        pair(buf, half + 1, y, right_key, 9, right_what, inner.right());
    }
    y += 2;
    head(buf, y, "Typing", "Lists and dialogs");
    y += 1;
    pair(buf, inner.x, y, "/", 13, "commands", half - 1);
    pair(buf, half + 1, y, "↑ ↓", 9, "move", inner.right());
    y += 1;
    pair(buf, inner.x, y, "@", 13, "attach a file or URL", half - 1);
    pair(buf, half + 1, y, "tab", 9, "complete or switch pane", inner.right());
    y += 1;
    pair(buf, inner.x, y, "!", 13, "run a shell command, add its output to the message", inner.right());
    y += 1;
    pair(buf, inner.x, y, "#", 13, "save a note to the system prompt", inner.right());
    y += 2;
    let settings = crate::ui::text::home_relative(
        &dirs::config_dir().unwrap_or_default().join("aihat").join("settings.json"),
    );
    put(
        buf,
        inner.x,
        y,
        &format!(
            "Change keys in {} · works in 80×24 and wider",
            text::truncate_left(&settings, (inner.width as usize).saturating_sub(43))
        ),
        t.fg(Role::Dim),
        inner.right(),
    );
}

// ------------------------------------------------------------------ memory

/// /memory: what models remember across chats, and whether this chat uses it.
#[derive(Default)]
pub struct MemoryDialog {
    selected: usize,
    adding: Option<super::composer::Composer>,
}

impl MemoryDialog {
    fn draw(&self, app: &App, buf: &mut Buffer, screen: Rect) {
        let t = theme();
        let items = crate::memory::list(&app.db);
        let everywhere = app.db.settings().get("memoryEnabled").and_then(Value::as_bool).unwrap_or(true);
        let here = app.chat.as_ref().is_none_or(|chat| chat.memory_enabled && !chat.temporary);
        let state = match (everywhere, here) {
            (false, _) => "off everywhere",
            (true, false) => "off for this chat",
            (true, true) => "on for this chat",
        };
        let visible = items.len().clamp(1, 12);
        let extra = if self.adding.is_some() { 3 } else { 0 };
        let area = draw::dialog_rect(screen, 84, visible as u16 + 5 + extra, 5);
        draw::boxed(buf, area, t.fg(Role::Accent), "Memory", &format!("{state} · {} saved", items.len()));
        let inner = draw::inner(area);
        if items.is_empty() {
            put(buf, inner.x, inner.y, "Nothing saved yet. Models keep what will matter in later chats; a adds one.", t.fg(Role::Dim), inner.right());
        }
        let first = window(self.selected, items.len(), visible);
        for (offset, (_, title, body)) in items.iter().enumerate().skip(first).take(visible) {
            let selected = offset == self.selected && self.adding.is_none();
            let title_style = if selected { t.fg(Role::Accent) } else { t.fg(Role::Strong) };
            let line = Line::from(vec![
                Span::styled(text::pad(&text::truncate(title, 22), 24), title_style),
                Span::styled(text::truncate(body, (inner.width as usize).saturating_sub(25)), if selected { title_style } else { t.fg(Role::Text) }),
            ]);
            draw::row(buf, inner, inner.y + (offset - first) as u16, line, selected);
        }
        let mut y = inner.y + visible as u16 + 1;
        if let Some(editor) = &self.adding {
            put(buf, inner.x, y, "Remember", t.fg(Role::Strong), inner.right());
            y += 1;
            put(buf, inner.x, y, "›", t.fg(Role::Accent), inner.right());
            let shown = text::truncate_left(editor.text(), (inner.width as usize).saturating_sub(4));
            let end = put(buf, inner.x + 2, y, &shown, t.fg(Role::Strong), inner.right());
            cursor_at(buf, end, y);
            y += 1;
            put(buf, inner.x, y, "enter saves · esc back · one short sentence", t.fg(Role::Dim), inner.right());
            return;
        }
        put(
            buf,
            inner.x,
            inner.bottom() - 1,
            "m on/off for this chat · g on/off everywhere · a add · d forget · esc close",
            t.fg(Role::Dim),
            inner.right(),
        );
    }

    fn key(&mut self, app: &mut App, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if let Some(editor) = self.adding.as_mut() {
            match key.code {
                KeyCode::Esc => self.adding = None,
                KeyCode::Enter => {
                    match crate::memory::add(&app.db, "", editor.text()) {
                        Ok(title) => app.ok(format!("Saved to memory: {title}")),
                        Err(error) => app.fail(error.to_string()),
                    }
                    self.adding = None;
                    self.selected = 0;
                }
                KeyCode::Char('u') if ctrl => editor.delete_to_start(),
                KeyCode::Char('w') if ctrl => editor.delete_word(),
                KeyCode::Char(c) if !ctrl => editor.insert(c.encode_utf8(&mut [0; 4])),
                KeyCode::Backspace => editor.backspace(),
                KeyCode::Left => editor.left(),
                KeyCode::Right => editor.right(),
                KeyCode::Home => editor.home(),
                KeyCode::End => editor.end(),
                _ => {}
            }
            return true;
        }
        let count = crate::memory::list(&app.db).len();
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return false,
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down => self.selected = (self.selected + 1).min(count.saturating_sub(1)),
            KeyCode::Char('a') => self.adding = Some(super::composer::Composer::default()),
            KeyCode::Char('m') => app.toggle_memory(),
            KeyCode::Char('g') => {
                let mut settings = app.db.settings();
                if !settings.is_object() {
                    settings = json!({});
                }
                let on = !settings.get("memoryEnabled").and_then(Value::as_bool).unwrap_or(true);
                settings["memoryEnabled"] = json!(on);
                match app.db.set_kv("settings", &settings.to_string()) {
                    Ok(()) => app.ok(if on { "Memory is on" } else { "Memory is off in every chat" }),
                    Err(error) => app.fail(error.to_string()),
                }
            }
            KeyCode::Char('d') | KeyCode::Char('x') | KeyCode::Delete if count > 0 => {
                match crate::memory::forget(&app.db, &(self.selected + 1).to_string()) {
                    Ok(title) => app.ok(format!("Forgot {title}")),
                    Err(error) => app.fail(error.to_string()),
                }
                self.selected = self.selected.min(count.saturating_sub(2));
            }
            _ => {}
        }
        true
    }
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
        if let Some(enabled) = enabled
            && let Err(error) = app.db.set_skill_enabled(id, !enabled) {
                app.fail(error.to_string());
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
        "onAsk" | "manual" | "command" => "when I ask",
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
        let ready = server.connected || !server.tools.is_empty();
        let (status, role) = if server.auth == "oauth" && !crate::oauth::is_signed_in(&app.secrets, &server.id) {
            ("! signed out · s to sign in".to_string(), Role::Warn)
        } else if ready {
            let allowed = crate::mcp::server_always_allowed(&app.db, &server.id);
            let how = if server.auth == "oauth" {
                "signed in"
            } else if crate::mcp_stdio::is_running(&server.id) {
                "running"
            } else {
                "ready"
            };
            (
                format!("✓ {how} · {}", if allowed { "runs without asking" } else { "asks before writing" }),
                Role::Accent,
            )
        } else if server.transport == "stdio" {
            ("! not started · r to start".to_string(), Role::Warn)
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
        "a add · r refresh · s sign in · o sign out · k token · p ask/allow · x remove",
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
        KeyCode::Char('s') => {
            if let Some(server) = selected {
                if server.transport == "stdio" {
                    app.notify(Role::Dim, format!("{} runs on this computer and needs no sign-in", server.name));
                } else {
                    let _ = app.db.set_mcp_auth(&server.id, "oauth");
                    crate::oauth::sign_out(&app.secrets, &server.id);
                    refresh_mcp(app, server.id, server.name);
                }
            }
        }
        KeyCode::Char('o') => {
            if let Some(server) = selected {
                crate::oauth::sign_out(&app.secrets, &server.id);
                let _ = app.db.mark_mcp_disconnected(&server.id);
                app.ok(format!("Signed out of {}", server.name));
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

/// Connect a server in the background, signing in through the browser when
/// it needs that; esc in the chat cancels the wait for the browser.
pub fn refresh_mcp(app: &mut App, id: String, name: String) {
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    app.busy_task = Some(format!("connecting to {name}"));
    let cancel = Arc::new(AtomicBool::new(false));
    app.sign_in_cancel = Some(cancel.clone());
    let ctx = app.ctx.clone();
    let tx = app.tx.clone();
    std::thread::spawn(move || {
        let mut opened = |_: &str| {
            let _ = tx.send(super::app::WorkerMsg::Task(Task::McpSigningIn { name: name.clone() }));
        };
        let result = crate::db::Database::open(&ctx.db_path)
            .and_then(|db| crate::mcp::connect_server(&db, &ctx.secrets, &id, &cancel, &mut opened));
        let task = match result {
            Ok(count) => Task::McpRefreshed { name, result: Ok(count) },
            Err(error) if crate::mcp::needs_sign_in(&error).is_some_and(|need| !need.browser) => Task::McpNeedsToken { id, name },
            Err(error) => Task::McpRefreshed { name, result: Err(super::app::describe_error(&error)) },
        };
        let _ = tx.send(super::app::WorkerMsg::Task(task));
    });
}

/// A row of /skills: one of AI Hat's own skills, or a global one that
/// another agent installed on this computer.
enum SkillRow {
    Library(crate::models::Skill),
    Global(crate::skills::Global),
}

fn skill_rows(app: &App) -> Vec<SkillRow> {
    let library = app.db.skills().unwrap_or_default();
    let globals: Vec<_> = crate::skills::globals()
        .into_iter()
        .filter(|g| !library.iter().any(|s| s.name.eq_ignore_ascii_case(&g.name)))
        .collect();
    library
        .into_iter()
        .map(SkillRow::Library)
        .chain(globals.into_iter().map(SkillRow::Global))
        .collect()
}

fn draw_skills(dialog: &ListDialog, app: &App, buf: &mut Buffer, screen: Rect) {
    let t = theme();
    let rows = skill_rows(app);
    let visible = rows.len().clamp(1, 14);
    let area = draw::dialog_rect(screen, 84, visible as u16 + 4, 5);
    draw::boxed(buf, area, t.fg(Role::Accent), "Skills", "esc close");
    let inner = draw::inner(area);
    if rows.is_empty() {
        put(buf, inner.x, inner.y, "No skills yet · a adds a folder or a GitHub link · /market finds more", t.fg(Role::Dim), inner.right());
    }
    let first = window(dialog.selected, rows.len(), visible);
    for (offset, row) in rows.iter().skip(first).take(visible).enumerate() {
        let selected = first + offset == dialog.selected;
        let (name, description, on, right) = match row {
            SkillRow::Library(skill) => (skill.name.clone(), skill.description.clone(), skill.enabled, skill_mode_label(&skill.mode).to_string()),
            SkillRow::Global(g) => (g.name.clone(), g.description.clone(), !crate::skills::global_is_off(&g.name), "global".to_string()),
        };
        let (mark, name_role) = if on { ("✓ ", Role::Strong) } else { ("○ ", Role::Dim) };
        let room = (inner.width as usize).saturating_sub(2 + 18 + 12);
        let line = Line::from(vec![
            Span::styled(mark, if on { t.fg(Role::Accent) } else { t.fg(Role::Dim) }),
            Span::styled(text::pad(&name, 18), t.fg(name_role)),
            Span::styled(text::pad(&description.replace('\n', " "), room), t.fg(Role::Dim)),
            Span::styled(text::pad_left(&right, 12), t.fg(Role::Dim)),
        ]);
        draw::row(buf, inner, inner.y + offset as u16, line, selected);
    }
    put(
        buf,
        inner.x,
        inner.bottom() - 1,
        "space on/off · m when to use · a add · r reload · x remove · enter read",
        t.fg(Role::Dim),
        inner.right(),
    );
}

/// Turn a global skill on or off: kept in settings.json, used from the next message.
fn toggle_global_skill(app: &mut App, name: &str) {
    let off = &mut app.config.skills.off;
    let was_off = off.iter().any(|n| n.eq_ignore_ascii_case(name));
    if was_off {
        off.retain(|n| !n.eq_ignore_ascii_case(name));
    } else {
        off.push(name.to_string());
    }
    crate::skills::set_global_policy(app.config.skills.use_global, &app.config.skills.off);
    app.ctx.config = app.config.clone();
    match app.config.save(&app.paths) {
        Ok(()) => app.ok(if was_off { format!("{name} is on") } else { format!("{name} is off") }),
        Err(error) => app.fail(error.to_string()),
    }
}

fn skills_key(dialog: &mut ListDialog, app: &mut App, key: KeyEvent) -> bool {
    let rows = skill_rows(app);
    let count = rows.len();
    let row = rows.into_iter().nth(dialog.selected);
    let (selected, global) = match row {
        Some(SkillRow::Library(skill)) => (Some(skill), None),
        Some(SkillRow::Global(g)) => (None, Some(g)),
        None => (None, None),
    };
    if let Some(g) = global {
        match key.code {
            KeyCode::Esc => return false,
            KeyCode::Up => dialog.selected = dialog.selected.saturating_sub(1),
            KeyCode::Down => dialog.selected = (dialog.selected + 1).min(count.saturating_sub(1)),
            KeyCode::Char('a') => {
                app.dialog = Some(Dialog::Form(Form::add_skill()));
                return true;
            }
            KeyCode::Char(' ') => toggle_global_skill(app, &g.name),
            KeyCode::Char('m') => app.notify(Role::Dim, format!("{} is a global skill: the assistant opens it when a task matches", g.name)),
            KeyCode::Char('r') => app.ok(format!("{} is read from its folder every time", g.name)),
            KeyCode::Char('x') | KeyCode::Delete => app.notify(
                Role::Dim,
                format!("{} belongs to the agents on this computer · delete its folder {} to remove it everywhere, or space to turn it off here", g.name, text::home_relative(&g.dir)),
            ),
            KeyCode::Enter => {
                let mut lines = vec![
                    Line::styled(g.description.clone(), theme().fg(Role::Dim)),
                    Line::styled(format!("global skill · {}", text::home_relative(&g.dir)), theme().fg(Role::Dim)),
                    Line::default(),
                ];
                lines.extend(crate::ui::markdown::render(
                    &g.body,
                    crate::ui::markdown::MdOptions { width: 76, indent: 0, block_indent: 2, wide: 76 },
                ));
                app.dialog = Some(Dialog::Text(TextView { title: g.name, lines, scroll: 0, prompt_id: None }));
                return true;
            }
            _ => {}
        }
        return true;
    }
    match key.code {
        KeyCode::Esc => return false,
        KeyCode::Up => dialog.selected = dialog.selected.saturating_sub(1),
        KeyCode::Down => dialog.selected = (dialog.selected + 1).min(count.saturating_sub(1)),
        KeyCode::Char('a') => {
            app.dialog = Some(Dialog::Form(Form::add_skill()));
            return true;
        }
        KeyCode::Char(' ') => {
            if let Some(skill) = selected
                && let Err(error) = app.db.set_skill_enabled(&skill.id, !skill.enabled) {
                    app.fail(error.to_string());
                }
        }
        KeyCode::Char('m') => {
            if let Some(skill) = selected {
                let next = match skill.mode.as_str() {
                    "auto" => "always",
                    "always" => "onAsk",
                    _ => "auto",
                };
                match app.db.set_skill_mode(&skill.id, next) {
                    Ok(()) => app.ok(format!("{}: {}", skill.name, skill_mode_label(next))),
                    Err(error) => app.fail(error.to_string()),
                }
            }
        }
        KeyCode::Char('r') => {
            if let Some(skill) = selected {
                app.busy_task = Some(format!("reading {} again", skill.name));
                app.spawn_task(move || Task::SkillLoaded {
                    existing: Some(skill.id.clone()),
                    result: crate::skills::reload(&skill.source).map_err(|e| format!("{}: {e:#}", skill.name)),
                });
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
                let files = app.db.skill_file_count(&skill.id).unwrap_or(0);
                let mut about = skill.description.clone();
                if files > 0 {
                    about.push_str(&format!(" · {}", text::plural(files, "reference file", "reference files")));
                }
                let mut lines = vec![Line::styled(about, theme().fg(Role::Dim)), Line::default()];
                lines.extend(crate::ui::markdown::render(
                    &skill.body,
                    crate::ui::markdown::MdOptions {
                        width: 76,
                        indent: 0,
                        block_indent: 2,
                        wide: 76,
                    },
                ));
                app.dialog = Some(Dialog::Text(TextView {
                    title: skill.name,
                    lines,
                    scroll: 0,
                    prompt_id: None,
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
    /// A server from /market that takes a token or settings first.
    MarketMcp(Box<crate::market::McpListing>),
    /// Describe an assistant; the model writes its system prompt.
    WritePrompt,
}

#[derive(Clone, PartialEq)]
enum FieldKind {
    Text,
    Secret,
    Choice(Vec<&'static str>, usize),
}

#[derive(Clone)]
struct Field {
    label: String,
    value: String,
    kind: FieldKind,
    /// Dim text after the value (or instead of it when empty).
    hint: String,
    empty: String,
}

impl Field {
    fn text(label: impl Into<String>, value: impl Into<String>, empty: &str) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
            kind: FieldKind::Text,
            hint: String::new(),
            empty: empty.into(),
        }
    }

    fn secret(label: impl Into<String>, hint: &str, empty: &str) -> Self {
        Self {
            label: label.into(),
            value: String::new(),
            kind: FieldKind::Secret,
            hint: hint.into(),
            empty: empty.into(),
        }
    }

    fn choice(label: &'static str, options: Vec<&'static str>, index: usize) -> Self {
        Self {
            label: label.into(),
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
                Field::secret("API key", crate::secrets::where_keys_go(false), "leave empty if none"),
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
            fields: vec![Field::secret("API key", crate::secrets::where_keys_go(false), "paste the key")],
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
            title: "Add an MCP server".into(),
            kind: FormKind::AddMcp,
            fields: vec![
                Field::text("Name", "", "github"),
                Field::text("Address", "", "https://…/mcp, or a command: npx -y @scope/server"),
                Field::secret("Token", crate::secrets::where_keys_go(false), "leave empty if none"),
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
            fields: vec![Field::secret("Token", crate::secrets::where_keys_go(false), "paste the token")],
            focus: 0,
            status: None,
            testable: false,
            token: 0,
        }
    }

    /// The token and settings a marketplace server asks for, filled from
    /// the environment where they are already set.
    pub fn market_mcp(server: crate::market::McpListing, plan: &crate::market::McpPlan) -> Self {
        let mut fields = Vec::new();
        if let Some((about, required)) = &plan.token {
            let empty = if *required { format!("needed · {about}") } else { format!("optional · {about}") };
            fields.push(Field::secret("Token", crate::secrets::where_keys_go(false), &empty));
        }
        for (var, about) in &plan.env {
            let empty = match (about.required, about.description.is_empty()) {
                (true, true) => "needed".to_string(),
                (true, false) => format!("needed · {}", about.description),
                (false, true) => "optional".to_string(),
                (false, false) => format!("optional · {}", about.description),
            };
            let mut field = if about.secret { Field::secret(var.clone(), crate::secrets::where_keys_go(false), &empty) } else { Field::text(var.clone(), "", &empty) };
            field.value = std::env::var(var).unwrap_or_default();
            fields.push(field);
        }
        Self {
            title: format!("Add {}", server.title),
            kind: FormKind::MarketMcp(Box::new(server)),
            fields,
            focus: 0,
            status: None,
            testable: false,
            token: 0,
        }
    }

    pub fn write_prompt() -> Self {
        Self {
            title: "Write a system prompt with AI".into(),
            kind: FormKind::WritePrompt,
            fields: vec![Field::text("Describe it", "", "a patient Rust tutor who explains with small examples")],
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
            fields: vec![Field::text("Source", "", "a folder with SKILL.md, or a GitHub link to one")],
            focus: 0,
            status: None,
            testable: false,
            token: 0,
        }
    }

    pub fn paste(&mut self, text: &str) {
        if let Some(field) = self.fields.get_mut(self.focus)
            && !matches!(field.kind, FieldKind::Choice(..)) {
                field.value.push_str(text.trim_end_matches(['\n', '\r']));
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
        let label_width = self.fields.iter().map(|f| width(&f.label)).max().unwrap_or(8).max(8) as u16 + 2;
        let mut y = inner.y;
        for (index, field) in self.fields.iter().enumerate() {
            let focused = index == self.focus;
            let label_style = if focused { t.fg(Role::Accent) } else { t.fg(Role::Dim) };
            put(buf, inner.x, y, &field.label, label_style, inner.right());
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
        let x = put(buf, inner.x, y, " Save ", t.badge(), inner.right());
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
        if key.is_empty()
            && let FormKind::EditProvider(id) = &self.kind {
                key = app.secrets.provider_key(id).unwrap_or_default();
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
                if let Some(chat) = app.chat.clone()
                    && chat.provider_id.as_deref() == Some(&provider_id) && chat.model_id.as_deref() == Some(&model_id) {
                        let params = app.db.default_params(&provider_id, &model_id);
                        let _ = app.db.set_chat_params(&chat.id, &params);
                        app.reload_messages();
                    }
                app.ok(format!("Saved defaults for {model_id}"));
                Ok(true)
            }
            FormKind::AddMcp => {
                let name = self.field("Name");
                let address = self.field("Address");
                if name.is_empty() || address.is_empty() {
                    return Err("Give it a name, and its https:// address or the command that runs it".into());
                }
                if app.db.mcp_servers().unwrap_or_default().iter().any(|s| s.name.eq_ignore_ascii_case(&name)) {
                    return Err(format!("There is already a server named {name}"));
                }
                let transport = if address.starts_with("http://") || address.starts_with("https://") { "http" } else { "stdio" };
                let token = self.field("Token");
                let auth = if token.is_empty() { "none" } else { "token" };
                let id = app.db.add_mcp_server(&name, &address, transport, auth).map_err(|e| e.to_string())?;
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
                let _ = app.db.set_mcp_auth(&id, "token");
                let name = app.db.mcp_servers().unwrap_or_default().into_iter().find(|s| s.id == id).map(|s| s.name).unwrap_or_default();
                refresh_mcp(app, id, name);
                app.dialog = Some(Dialog::Mcp(ListDialog::default()));
                Ok(false)
            }
            FormKind::MarketMcp(server) => {
                let plan = crate::market::plan_mcp(&server);
                let token = self.field("Token");
                if plan.token.as_ref().is_some_and(|(_, required)| *required) && token.is_empty() {
                    self.focus = self.fields.iter().position(|f| f.label == "Token").unwrap_or(0);
                    return Err("Paste the token first".into());
                }
                let mut env = Vec::new();
                for (var, about) in &plan.env {
                    let value = self.field(var);
                    if about.required && value.is_empty() {
                        self.focus = self.fields.iter().position(|f| &f.label == var).unwrap_or(0);
                        return Err(format!("{var} is needed"));
                    }
                    env.push((var.clone(), value));
                }
                let id = crate::market::add_mcp(&app.db, &app.secrets, &server, &plan, Some(&token), &env).map_err(|e| e.to_string())?;
                let name = app.db.mcp_servers().unwrap_or_default().into_iter().find(|s| s.id == id).map(|s| s.name).unwrap_or_else(|| server.short_name());
                refresh_mcp(app, id, name);
                Ok(true)
            }
            FormKind::WritePrompt => {
                let description = self.field("Describe it");
                if description.is_empty() {
                    return Err("Describe the assistant first, like: a patient Rust tutor".into());
                }
                app.write_prompt(&description);
                Ok(true)
            }
            FormKind::AddSkill => {
                let source = self.field("Source");
                if source.is_empty() {
                    return Err("Give a folder with SKILL.md, or a GitHub link".into());
                }
                app.busy_task = Some("reading the skill".into());
                app.spawn_task(move || Task::SkillLoaded {
                    existing: None,
                    result: crate::skills::load(&source).map_err(|e| format!("{e:#}")),
                });
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
pub(super) fn draw_choices(buf: &mut Buffer, x: u16, y: u16, right: u16, options: &[&str], chosen: usize) {
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
    // "␣Custom␣  OpenRouter  Groq": the chosen one sits in a highlighted
    // chip, the others are two spaces apart.
    let mut cursor = x;
    for (index, option) in options.iter().enumerate().skip(first) {
        let (label, style) = if index == chosen {
            (format!(" {option} "), t.selected(Role::Accent))
        } else {
            (option.to_string(), t.fg(Role::Text))
        };
        if cursor + width(&label) as u16 > right {
            break;
        }
        cursor = put(buf, cursor, y, &label, style, right) + 2;
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
                        crate::mcp::forget(&app.secrets, id);
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
    /// A saved system prompt being shown: e opens it in the editor.
    pub prompt_id: Option<String>,
}

impl TextView {
    /// A system prompt the model just wrote, to read before using it.
    pub fn written_prompt(id: &str, name: &str, body: &str) -> Self {
        let t = theme();
        let mut lines: Vec<Line<'static>> = vec![
            Line::styled("Saved to your system prompts and set for this chat.", t.fg(Role::Accent)),
            Line::default(),
        ];
        for paragraph in body.lines() {
            if paragraph.trim().is_empty() {
                lines.push(Line::default());
                continue;
            }
            for row in crate::ui::markdown::wrap_segments(&[(paragraph.to_string(), t.fg(Role::Text))], 78) {
                lines.push(Line::from(row));
            }
        }
        Self { title: name.to_string(), lines, scroll: 0, prompt_id: Some(id.to_string()) }
    }

    fn draw(&self, buf: &mut Buffer, screen: Rect) {
        let t = theme();
        // As tall as the text, up to the screen.
        let height = (self.lines.len() as u16 + 2).clamp(8, screen.height.saturating_sub(8).max(8));
        let area = draw::dialog_rect(screen, 84, height, 3);
        let keys = if self.prompt_id.is_some() { "↑↓ scroll · e edit · esc close" } else { "↑↓ scroll · esc close" };
        draw::boxed(buf, area, t.fg(Role::Accent), &self.title, keys);
        let inner = draw::inner(area);
        for (offset, line) in self.lines.iter().skip(self.scroll).take(inner.height as usize).enumerate() {
            put_line(buf, inner.x, inner.y + offset as u16, line, inner.right());
        }
    }

    fn key(&mut self, app: &mut App, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => return false,
            KeyCode::Char('e') if self.prompt_id.is_some() => {
                app.edit_prompt = self.prompt_id.clone();
                return false;
            }
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
    let mut body: Vec<Line<'static>> = Vec::new();
    if let Some(folder) = &request.folder {
        // A command the assistant wants to run: the command line itself, and why.
        body.push(Line::from(vec![Span::styled("The assistant wants to run this in ", t.fg(Role::Text)), Span::styled(folder.clone(), t.fg(Role::Link))]));
        body.push(Line::default());
        let command = request.arguments.get("command").and_then(Value::as_str).unwrap_or("");
        for (index, row) in crate::ui::markdown::wrap_segments(&[(command.to_string(), t.bold(Role::Strong))], inner.saturating_sub(2)).into_iter().take(6).enumerate() {
            let mut spans = vec![Span::styled(if index == 0 { "$ " } else { "  " }, t.fg(Role::Dim))];
            spans.extend(row);
            body.push(Line::from(spans));
        }
        if let Some(reason) = request.arguments.get("reason").and_then(Value::as_str).filter(|r| !r.trim().is_empty()) {
            body.push(Line::default());
            for row in crate::ui::markdown::wrap_segments(&[(reason.to_string(), t.fg(Role::Dim))], inner).into_iter().take(3) {
                body.push(Line::from(row));
            }
        }
        body.push(Line::default());
    } else {
        body.push(Line::from(vec![
            Span::styled(request.server.clone(), t.fg(Role::Strong)),
            Span::styled(" wants to run ", t.fg(Role::Text)),
            Span::styled(request.tool.clone(), t.fg(Role::Link)),
        ]));
        body.push(Line::default());
    }
    if let Some(map) = request.arguments.as_object().filter(|_| request.folder.is_none()) {
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
    let command = request.folder.is_some();
    let always = if command { "Always run this here".to_string() } else { format!("Always for {}", request.server) };
    let options = [
        (if command { "Run it once" } else { "Allow once" }, "enter"),
        (always.as_str(), "a"),
        (if command { "Edit the command" } else { "Edit the arguments" }, "e"),
        (if command { "Don't run it" } else { "Deny" }, "esc"),
    ];
    let mut option_rows = Vec::new();
    for (index, (label, key)) in options.iter().enumerate() {
        let selected = index == pending.selection;
        // Laid out as the design draws it: the highlight ends three columns
        // before the chosen key; the other keys sit one column further right.
        let line = if selected {
            Line::from(vec![
                Span::styled(format!(" › {}", text::pad(label, 18)), t.selected(Role::Accent)),
                Span::raw("   "),
                Span::styled(key.to_string(), t.fg(Role::Text)),
            ])
        } else {
            Line::from(vec![
                Span::styled(format!("   {}", text::pad(label, 22)), t.fg(Role::Text)),
                Span::styled(key.to_string(), t.fg(Role::Dim)),
            ])
        };
        option_rows.push((line, selected));
    }
    let kind = if command { "runs on this computer" } else if request.writes { "MCP · writes data" } else { "MCP · reads data" };
    let g = t.glyphs();
    let title = format!("{} {} ", g.horizontal, if command { "Run this command?" } else { "Allow this tool?" });
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
        Some(Dialog::Image(viewer)) => viewer.draw(app, buf, screen),
        Some(Dialog::Memory(dialog)) => dialog.draw(app, buf, screen),
        Some(Dialog::About(dialog)) => dialog.draw(buf, screen),
        Some(Dialog::Effort(picker)) => picker.draw(app, buf, screen),
        Some(Dialog::Folder(dialog)) => dialog.draw(app, buf, screen),
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
        Dialog::Text(view) => view.key(app, key),
        Dialog::Image(viewer) => viewer.key(app, key),
        Dialog::Memory(dialog) => dialog.key(app, key),
        Dialog::About(dialog) => dialog.key(app, key),
        Dialog::Effort(picker) => picker.key(app, key),
        Dialog::Folder(dialog) => dialog.key(app, key),
    };
    if keep && app.dialog.is_none() {
        app.dialog = Some(dialog);
    }
    if app.dialog.is_none()
        && let Some(viewer) = app.pending_image.take()
    {
        app.dialog = Some(Dialog::Image(viewer));
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
