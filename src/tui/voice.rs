//! `/voice`: how speech becomes text. Phonon-2 runs on this computer once
//! downloaded from Hugging Face (with a progress bar here); or recordings go
//! to an OpenAI-compatible endpoint. ctrl+t talks either way.

use super::app::{App, Task, WorkerMsg};
use super::dialogs::{self, Dialog};
use super::draw::{self, put, put_line};
use super::keys::{self, Action};
use crate::ui::text;
use crate::ui::theme::{Role, theme};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Default)]
pub struct VoiceDialog {
    selection: usize,
}

/// A download in progress: bytes so far, total, and how to stop it.
pub struct Download {
    pub done: u64,
    pub total: u64,
    pub cancel: Arc<AtomicBool>,
}

const ENGINES: [(&str, &str); 3] = [("local", "On this computer (Phonon-2)"), ("endpoint", "Endpoint"), ("auto", "Automatic")];

/// Start downloading Phonon-2 in the background.
pub fn start_download(app: &mut App) {
    if app.voice_download.is_some() {
        return;
    }
    let cancel = Arc::new(AtomicBool::new(false));
    app.voice_download = Some(Download { done: 0, total: crate::voice::MODEL_BYTES, cancel: cancel.clone() });
    let tx = app.tx.clone();
    std::thread::spawn(move || {
        let mut last = 0u64;
        let result = crate::voice::download(
            &mut |done, total| {
                // A redraw for every megabyte is plenty.
                if done - last >= 1_000_000 || done == total {
                    last = done;
                    let _ = tx.send(WorkerMsg::Task(Task::VoiceProgress { done, total }));
                }
            },
            &cancel,
        );
        let _ = tx.send(WorkerMsg::Task(Task::VoiceDownloaded { result: result.map(|_| ()).map_err(|e| format!("{e:#}")) }));
    });
}

impl VoiceDialog {
    pub fn draw(&self, app: &App, buf: &mut Buffer, screen: Rect) {
        let t = theme();
        let voice = &app.config.voice;
        let installed = crate::voice::model_installed();
        let area = draw::dialog_rect(screen, 76, 16, 4);
        draw::boxed(buf, area, t.fg(Role::Accent), "Speech to text", "esc close");
        let inner = draw::inner(area);
        let right = inner.right();
        let mut y = inner.y;
        let using = crate::voice::engine(voice);
        let now = if using == "local" { "Phonon-2, on this computer" } else if voice.url.is_empty() { "your default provider's speech endpoint" } else { voice.url.as_str() };
        put_line(buf, inner.x, y, &Line::from(vec![Span::styled("Talk with ", t.fg(Role::Dim)), Span::styled(keys::label(Action::Voice), t.fg(Role::Link)), Span::styled(" · now: ", t.fg(Role::Dim)), Span::styled(now.to_string(), t.fg(Role::Strong))]), right);
        y += 2;

        let row = |buf: &mut Buffer, y: u16, index: usize, label: &str, value: Line<'static>| {
            let selected = index == self.selection;
            let marker = if selected { "› " } else { "  " };
            let label_style = if selected { t.fg(Role::Accent) } else { t.fg(Role::Text) };
            let mut spans = vec![Span::styled(marker, t.fg(Role::Accent)), Span::styled(text::pad(label, 18), label_style)];
            spans.extend(value.spans);
            put_line(buf, inner.x, y, &Line::from(spans), right);
        };

        // 0: which way.
        let mut choices = Vec::new();
        for (key, label) in ENGINES {
            let chosen = voice.engine == key || (key == "auto" && !ENGINES.iter().any(|(k, _)| *k == voice.engine));
            choices.push(if chosen { Span::styled(format!(" {label} "), t.selected(Role::Accent)) } else { Span::styled(format!(" {label} "), t.fg(Role::Dim)) });
        }
        row(buf, y, 0, "Use", Line::from(choices));
        y += 1;
        put(buf, inner.x + 20, y, "←→ change · automatic uses Phonon-2 once it is downloaded", t.fg(Role::Dim), right);
        y += 2;

        // 1: the model.
        put(buf, inner.x, y, "Phonon-2 · English · 164 MB download · runs offline on the CPU", t.fg(Role::Dim), right);
        y += 1;
        put(buf, inner.x, y, "by Fermion Research, from NVIDIA's Parakeet TDT 0.6B v3 · CC-BY-4.0", t.fg(Role::Dim), right);
        y += 1;
        let model_line = match &app.voice_download {
            Some(download) => {
                let pct = download.done * 100 / download.total.max(1);
                let filled = (pct as usize * 24 / 100).min(24);
                Line::from(vec![
                    Span::styled("█".repeat(filled), t.fg(Role::Accent)),
                    Span::styled("░".repeat(24 - filled), t.fg(Role::Dim)),
                    Span::styled(format!(" {pct}% · {} of {} MB · x stops", download.done / 1_000_000, download.total / 1_000_000), t.fg(Role::Text)),
                ])
            }
            None if installed => Line::from(vec![Span::styled("✓ downloaded", t.fg(Role::Accent)), Span::styled(" · enter deletes it", t.fg(Role::Dim))]),
            None => Line::from(vec![Span::styled("not downloaded", t.fg(Role::Warn)), Span::styled(" · enter downloads it from Hugging Face", t.fg(Role::Dim))]),
        };
        row(buf, y, 1, "Model", model_line);
        y += 2;

        // 2: the endpoint.
        put(buf, inner.x, y, "Endpoint: an OpenAI-compatible /audio/transcriptions", t.fg(Role::Dim), right);
        y += 1;
        let address = if voice.url.is_empty() { "your default provider".to_string() } else { voice.url.clone() };
        let model = if voice.model.is_empty() { "whisper-1".to_string() } else { voice.model.clone() };
        row(buf, y, 2, "Address · model", Line::from(vec![Span::styled(format!("{address} · {model}"), t.fg(Role::Link)), Span::styled("  enter edits", t.fg(Role::Dim))]));
        put(buf, inner.x, inner.bottom() - 1, "↑↓ move · ←→ change · enter choose · esc close", t.fg(Role::Dim), right);
    }

    /// Returns whether the dialog stays open.
    pub fn key(&mut self, app: &mut App, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return false,
            KeyCode::Up => self.selection = self.selection.saturating_sub(1),
            KeyCode::Down | KeyCode::Tab => self.selection = (self.selection + 1).min(2),
            KeyCode::Left | KeyCode::Right if self.selection == 0 => {
                let at = ENGINES.iter().position(|(k, _)| *k == app.config.voice.engine).unwrap_or(2);
                let next = if key.code == KeyCode::Right { (at + 1) % 3 } else { (at + 2) % 3 };
                set_engine(app, ENGINES[next].0);
            }
            KeyCode::Char('x') if app.voice_download.is_some() => {
                if let Some(download) = &app.voice_download {
                    download.cancel.store(true, Ordering::Relaxed);
                }
            }
            KeyCode::Enter => match self.selection {
                0 => {
                    let at = ENGINES.iter().position(|(k, _)| *k == app.config.voice.engine).unwrap_or(2);
                    set_engine(app, ENGINES[(at + 1) % 3].0);
                }
                1 if app.voice_download.is_some() => {}
                1 if crate::voice::model_installed() => match crate::voice::remove_model() {
                    Ok(()) => app.ok("Deleted Phonon-2"),
                    Err(error) => app.fail(error.to_string()),
                },
                1 => start_download(app),
                _ => {
                    app.dialog = Some(Dialog::Form(dialogs::Form::voice_endpoint(&app.config.voice)));
                    return false;
                }
            },
            _ => {}
        }
        true
    }
}

fn set_engine(app: &mut App, engine: &str) {
    app.config.voice.engine = engine.to_string();
    if let Err(error) = app.config.save(&app.paths) {
        app.fail(error.to_string());
    }
}
