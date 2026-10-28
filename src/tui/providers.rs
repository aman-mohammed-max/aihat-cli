//! `/providers`: the endpoint list on the left, details and models on the right.

use super::app::{App, Page, Task};
use super::dialogs::{self, Confirm, ConfirmAction, Dialog, Form};
use super::draw::{self, put, put_line};
use crate::db::Database;
use crate::models::{Model, Provider};
use crate::ui::text::{self, width};
use crate::ui::theme::{Role, theme};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

pub struct ProvidersPage {
    pub selection: usize,
    pub models_focused: bool,
    pub model_selection: usize,
    pub testing: HashSet<String>,
    pub refresh_keys: bool,
    keys: HashMap<String, Option<&'static str>>,
}

impl ProvidersPage {
    pub fn new(app: &App) -> Self {
        let selection = app
            .db
            .providers()
            .unwrap_or_default()
            .iter()
            .position(|p| p.is_default)
            .unwrap_or(0);
        Self {
            selection,
            models_focused: false,
            model_selection: 0,
            testing: HashSet::new(),
            refresh_keys: true,
            keys: HashMap::new(),
        }
    }

    pub fn select_provider(&mut self, db: &Database, id: &str) {
        if let Some(index) = db.providers().unwrap_or_default().iter().position(|p| p.id == id) {
            self.selection = index;
            self.models_focused = false;
            self.model_selection = 0;
        }
    }
}

fn selected(app: &App) -> Option<Provider> {
    let Page::Providers(page) = &app.page else {
        return None;
    };
    app.db.providers().unwrap_or_default().into_iter().nth(page.selection)
}

/// All models of a provider, enabled ones first, chat models before others.
fn provider_models(db: &Database, provider_id: &str) -> Vec<Model> {
    let mut models = db.models(Some(provider_id)).unwrap_or_default();
    models.sort_by_key(|m| (!m.enabled, !m.is_chat(), m.id.to_lowercase()));
    models
}

pub fn test_provider(app: &mut App, id: &str) {
    if let Page::Providers(page) = &mut app.page {
        page.testing.insert(id.to_string());
    }
    let ctx = app.ctx.clone();
    let id = id.to_string();
    app.spawn_task(move || Task::ProviderTested {
        result: super::tasks::test_saved(&ctx, &id),
        provider_id: id,
    });
}

pub fn draw(app: &mut App, buf: &mut Buffer, area: Rect) {
    // Asking the keychain can be slow, so key locations are cached.
    if let Page::Providers(page) = &mut app.page {
        if page.refresh_keys {
            page.keys = app
                .db
                .providers()
                .unwrap_or_default()
                .into_iter()
                .map(|p| {
                    let location = app.secrets.provider_key_location(&p.id);
                    (p.id, location)
                })
                .collect();
            page.refresh_keys = false;
        }
    }
    let Page::Providers(page) = &app.page else {
        return;
    };
    let t = theme();
    let providers = app.db.providers().unwrap_or_default();
    let top = area.y + 1;
    let left_width = 44.min(area.width * 2 / 5).max(30);
    let left_height = (providers.len().max(1) as u16 + 5).min(area.height.saturating_sub(1));
    let left = Rect::new(area.x, top, left_width, left_height);
    let left_border = if page.models_focused { t.fg(Role::Dim) } else { t.fg(Role::Accent) };
    draw::boxed(buf, left, left_border, "Providers", "");
    let inner = draw::inner(left);
    if providers.is_empty() {
        put(buf, inner.x, inner.y, "No providers yet", t.fg(Role::Dim), inner.right());
    }
    let list_rows = inner.height.saturating_sub(3) as usize;
    let first = (page.selection + 1).saturating_sub(list_rows);
    for (offset, provider) in providers.iter().skip(first).take(list_rows).enumerate() {
        let index = first + offset;
        let is_selected = index == page.selection && !page.models_focused;
        let (status, role) = if page.testing.contains(&provider.id) {
            ("◇ testing…".to_string(), Role::Dim)
        } else {
            status_label(provider)
        };
        let marker = if provider.is_default { "● " } else { "  " };
        let name_width = (inner.width as usize).saturating_sub(2 + 18).max(8);
        let line = if is_selected {
            Line::from(vec![
                Span::styled(marker, t.fg(Role::Accent)),
                Span::styled(text::pad(&provider.name, name_width), t.fg(Role::Accent)),
                Span::styled(status, t.fg(Role::Accent)),
            ])
        } else {
            Line::from(vec![
                Span::styled(marker, t.fg(Role::Accent)),
                Span::styled(text::pad(&provider.name, name_width), t.fg(Role::Text)),
                Span::styled(status, t.fg(role)),
            ])
        };
        draw::row(buf, inner, inner.y + offset as u16, line, index == page.selection);
    }
    let hints_y = inner.bottom().saturating_sub(2);
    put(buf, inner.x + 2, hints_y, "a add  ·  t test  ·  d default", t.fg(Role::Dim), inner.right());
    put(buf, inner.x + 2, hints_y + 1, "e edit ·  k key   ·  x remove", t.fg(Role::Dim), inner.right());

    let right_x = left.right() + 1;
    if right_x + 20 > area.right() {
        return;
    }
    let right_width = area.right() - right_x;
    let Some(provider) = providers.get(page.selection) else {
        let detail = Rect::new(right_x, top, right_width, 5);
        draw::boxed(buf, detail, t.fg(Role::Dim), "Details", "");
        let inner = draw::inner(detail);
        put(buf, inner.x, inner.y, "Press a to add an OpenAI-compatible endpoint:", t.fg(Role::Text), inner.right());
        put(buf, inner.x, inner.y + 1, "vLLM, Ollama, LM Studio, OpenRouter, Groq or your own.", t.fg(Role::Dim), inner.right());
        return;
    };
    let models = provider_models(&app.db, &provider.id);
    let max_height = area.bottom().saturating_sub(top);
    let model_rows = models.len().max(1).min(max_height.saturating_sub(11) as usize).max(1);
    let height = (model_rows as u16 + 11).min(max_height);
    let detail = Rect::new(right_x, top, right_width, height);
    let border = if page.models_focused { t.fg(Role::Accent) } else { t.fg(Role::Dim) };
    draw::boxed(buf, detail, border, "Details", "r refresh models");
    let inner = draw::inner(detail);
    let mut y = inner.y;
    let x = put(buf, inner.x, y, &provider.name, t.bold(Role::Strong), inner.right());
    if provider.is_default {
        put(buf, x + 2, y, " default ", t.badge(), inner.right());
    }
    y += 1;
    let key = match page.keys.get(&provider.id).copied().flatten() {
        Some("keychain") => "key in keychain",
        Some("keys file") => "key in keys.json",
        Some(_) => "key from the environment",
        None => "no key",
    };
    put(
        buf,
        inner.x,
        y,
        &text::truncate(&format!("{} · {key}", provider.base_url), inner.width as usize),
        t.fg(Role::Dim),
        inner.right(),
    );
    y += 2;
    put(buf, inner.x, y, "Models in the picker", t.fg(Role::Strong), inner.right());
    let hint = "space toggles";
    put(buf, inner.right().saturating_sub(width(hint) as u16 + 4), y, hint, t.fg(Role::Dim), inner.right());
    y += 1;
    if models.is_empty() {
        put(buf, inner.x, y, "No models yet · r loads them from the endpoint", t.fg(Role::Dim), inner.right());
    }
    let first = (page.model_selection + 1).saturating_sub(model_rows);
    for (offset, model) in models.iter().skip(first).take(model_rows).enumerate() {
        let index = first + offset;
        let caps = dialogs::model_caps_long(model);
        let line = if model.enabled {
            Line::from(vec![
                Span::styled("■ ", t.fg(Role::Accent)),
                Span::styled(text::pad(&model.id, 19), t.fg(Role::Text)),
                Span::styled(caps, t.fg(Role::Dim)),
            ])
        } else {
            Line::from(vec![Span::styled(
                format!("□ {}{caps}", text::pad(&model.id, 19)),
                t.fg(Role::Dim),
            )])
        };
        let is_selected = page.models_focused && index == page.model_selection;
        draw::row(buf, inner, y + offset as u16, line, is_selected);
    }
    if models.len() > model_rows {
        let more = format!("{}/{}", page.model_selection + 1, models.len());
        put(buf, inner.right().saturating_sub(width(&more) as u16), y + model_rows as u16 - 1, &more, t.fg(Role::Dim), inner.right());
    }
    y += model_rows as u16 + 1;
    if y + 2 >= detail.bottom() {
        return;
    }
    put(buf, inner.x, y, "Defaults for this provider", t.fg(Role::Strong), inner.right());
    y += 1;
    let settings = app.db.settings();
    let params = settings
        .pointer(&format!("/providerParams/{}", provider.id))
        .or_else(|| settings.get("defaultParams"))
        .cloned()
        .unwrap_or(Value::Null);
    let temperature = params.get("temperature").map(|v| v.to_string()).unwrap_or_else(|| "0.7".into());
    let max_tokens = params.get("maxTokens").map(|v| v.to_string()).unwrap_or_else(|| "4096".into());
    let defaults = Line::from(vec![
        Span::styled("temperature ", t.fg(Role::Dim)),
        Span::styled(temperature, t.fg(Role::Text)),
        Span::styled("   max tokens ", t.fg(Role::Dim)),
        Span::styled(max_tokens, t.fg(Role::Text)),
        Span::styled("   timeout ", t.fg(Role::Dim)),
        Span::styled(format!("{}s", provider.timeout_ms / 1000), t.fg(Role::Text)),
    ]);
    put_line(buf, inner.x, y, &defaults, inner.right());
    y += 1;
    let headers = dialogs::headers_text(&provider.headers);
    let headers = if headers.is_empty() { "none".to_string() } else { headers };
    let x = put(buf, inner.x, y, "headers ", t.fg(Role::Dim), inner.right());
    let edit = "enter to edit";
    let edit_x = inner.right().saturating_sub(width(edit) as u16 + 4);
    put(buf, x, y, &text::truncate(&headers, edit_x.saturating_sub(x + 2) as usize), t.fg(Role::Text), edit_x);
    put(buf, edit_x, y, edit, t.fg(Role::Dim), inner.right());
}

/// "✓ 38 ms", "✗ key rejected", "! offline", "· not tested".
pub fn status_label(provider: &Provider) -> (String, Role) {
    match provider.status.as_str() {
        "online" | "ok" => (
            format!("✓ {}", provider.latency_ms.map(text::duration).unwrap_or_else(|| "ready".into())),
            Role::Accent,
        ),
        "rejected" => ("✗ key rejected".into(), Role::Error),
        "offline" => ("! offline".into(), Role::Warn),
        _ => ("· not tested".into(), Role::Dim),
    }
}

pub fn on_key(app: &mut App, key: KeyEvent) {
    let providers = app.db.providers().unwrap_or_default();
    let Page::Providers(page) = &mut app.page else {
        return;
    };
    let provider = providers.get(page.selection).cloned();
    let models = provider
        .as_ref()
        .map(|p| provider_models(&app.db, &p.id))
        .unwrap_or_default();
    match key.code {
        KeyCode::Esc => {
            app.page = Page::Chat;
            return;
        }
        KeyCode::Char('/') => {
            app.page = Page::Chat;
            app.composer.set("/");
            return;
        }
        KeyCode::Tab | KeyCode::BackTab | KeyCode::Left | KeyCode::Right => {
            if provider.is_some() {
                page.models_focused = !page.models_focused;
            }
        }
        KeyCode::Up if page.models_focused => page.model_selection = page.model_selection.saturating_sub(1),
        KeyCode::Down if page.models_focused => {
            page.model_selection = (page.model_selection + 1).min(models.len().saturating_sub(1))
        }
        KeyCode::PageUp if page.models_focused => page.model_selection = page.model_selection.saturating_sub(10),
        KeyCode::PageDown if page.models_focused => {
            page.model_selection = (page.model_selection + 10).min(models.len().saturating_sub(1))
        }
        KeyCode::Up => {
            page.selection = page.selection.saturating_sub(1);
            page.model_selection = 0;
        }
        KeyCode::Down => {
            page.selection = (page.selection + 1).min(providers.len().saturating_sub(1));
            page.model_selection = 0;
        }
        KeyCode::Char('a') => app.dialog = Some(Dialog::Form(Form::add_provider())),
        KeyCode::Char(' ') if page.models_focused => {
            if let (Some(provider), Some(model)) = (&provider, models.get(page.model_selection)) {
                if let Err(error) = app.db.set_model_enabled(&provider.id, &model.id, !model.enabled) {
                    app.fail(error.to_string());
                }
                // Keep the cursor on the model that was toggled.
                let reordered = provider_models(&app.db, &provider.id);
                if let (Page::Providers(page), Some(index)) =
                    (&mut app.page, reordered.iter().position(|m| m.id == model.id))
                {
                    page.model_selection = index;
                }
            }
        }
        KeyCode::Char('d') => {
            let Some(provider) = provider else { return };
            if page.models_focused {
                if let Some(model) = models.get(page.model_selection) {
                    match app.db.set_default_model(&provider.id, &model.id) {
                        Ok(()) => app.ok(format!("{} is the default model", model.id)),
                        Err(error) => app.fail(error.to_string()),
                    }
                }
            } else {
                match app.db.set_default_provider(&provider.id) {
                    Ok(()) => app.ok(format!("{} is now the default provider", provider.name)),
                    Err(error) => app.fail(error.to_string()),
                }
            }
        }
        KeyCode::Char('t') => {
            if let Some(provider) = provider {
                test_provider(app, &provider.id);
            }
        }
        KeyCode::Char('r') => {
            if let Some(provider) = provider {
                app.refresh_models(Some(provider.id));
            }
        }
        KeyCode::Char('e') => {
            if let Some(form) = provider.and_then(|p| Form::edit_provider(app, &p.id)) {
                app.dialog = Some(Dialog::Form(form));
            }
        }
        KeyCode::Char('k') => {
            if let Some(provider) = provider {
                app.dialog = Some(Dialog::Form(Form::provider_key(&provider.id, &provider.name)));
            }
        }
        KeyCode::Char('x') | KeyCode::Delete => {
            if let Some(provider) = provider {
                app.dialog = Some(Dialog::Confirm(Confirm {
                    title: format!("Remove {}?", provider.name),
                    body: "Chats that used it stay.".into(),
                    yes: "remove",
                    action: ConfirmAction::RemoveProvider(provider.id),
                }));
            }
        }
        KeyCode::Enter => {
            let Some(provider) = provider else {
                app.dialog = Some(Dialog::Form(Form::add_provider()));
                return;
            };
            if page.models_focused {
                if let Some(model) = models.get(page.model_selection) {
                    app.dialog = Some(Dialog::Form(Form::model_defaults(app, &provider.id, &model.id)));
                }
            } else if let Some(form) = Form::provider_defaults(app, &provider.id) {
                app.dialog = Some(Dialog::Form(form));
            }
        }
        _ => {}
    }
}
