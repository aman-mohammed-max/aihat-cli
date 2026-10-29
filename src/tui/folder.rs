//! `/folder`: whether the assistant may read the folder aihat runs in.
//! Nothing is asked when aihat opens; the assistant's first look asks (in
//! the chat), and `/folder allow`, `/folder session` or `/folder off` answer
//! ahead of time.

use super::app::App;
use super::draw::{self, put, put_line};
use crate::folder::{self, Access};
use crate::ui::markdown::wrap_segments;
use crate::ui::text;
use crate::ui::theme::{Role, theme};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};

#[derive(Default)]
pub struct FolderDialog {
    selection: usize,
}

#[derive(Clone, Copy)]
enum Answer {
    Always,
    Session,
    Off,
}

const OPTIONS: [(&str, &str, Answer); 3] = [
    ("Always allow this folder", "/folder allow", Answer::Always),
    ("Allow until I quit", "/folder session", Answer::Session),
    ("Don't allow", "/folder off", Answer::Off),
];

/// `/folder` and its words: allow, session, off, clear.
pub fn command(app: &mut App, args: &str) {
    if app.folder.is_none() {
        app.fail("aihat isn't running in a folder it can offer");
        return;
    }
    match args.trim().to_lowercase().as_str() {
        "" => app.dialog = Some(super::dialogs::Dialog::Folder(FolderDialog::default())),
        "allow" | "on" | "always" | "yes" => answer(app, Answer::Always),
        "session" | "now" | "once" | "for now" => answer(app, Answer::Session),
        "off" | "deny" | "no" | "private" => answer(app, Answer::Off),
        "clear" => clear_commands(app),
        other => app.fail(format!("/folder {other}? Use /folder allow, /folder session, /folder off or /folder clear")),
    }
}

fn answer(app: &mut App, choice: Answer) {
    let Some(dir) = app.folder.as_ref().map(|f| f.dir.clone()) else {
        return;
    };
    let saved = match choice {
        Answer::Always => folder::set_access(&app.db, &dir, Some(Access::Allowed)),
        Answer::Session | Answer::Off => folder::set_access(&app.db, &dir, None),
    };
    if let Err(error) = saved {
        app.fail(error.to_string());
        return;
    }
    if let Some(folder) = app.folder.as_mut() {
        folder.allowed = !matches!(choice, Answer::Off);
        folder.denied = matches!(choice, Answer::Off);
    }
    let place = text::home_relative(&dir);
    match choice {
        Answer::Always => app.ok(format!("The assistant can read {place} · commands still ask first")),
        Answer::Session => app.ok(format!("The assistant can read {place} until you quit")),
        Answer::Off => app.notify(Role::Dim, format!("The assistant won't read {place} · /folder allow changes this")),
    }
}

fn clear_commands(app: &mut App) {
    let Some(dir) = app.folder.as_ref().map(|f| f.dir.clone()) else {
        return;
    };
    match folder::forget_commands(&app.db, &dir) {
        Ok(()) => app.ok("Every command asks first again"),
        Err(error) => app.fail(error.to_string()),
    }
}

impl FolderDialog {
    pub fn draw(&self, app: &App, buf: &mut Buffer, screen: Rect) {
        let t = theme();
        let Some(access) = &app.folder else {
            return;
        };
        let place = text::home_relative(&access.dir);
        let explain = "Allowed, the assistant can list, read and search the files in this folder and the folders inside it, never outside it. Before it runs any command here, it asks you first. Not allowed yet, its first look asks you in the chat.";
        let rows = wrap_segments(&[(explain.to_string(), t.fg(Role::Text))], 70);
        let commands = folder::allowed_commands(&app.db, &access.dir);
        let state = match (access.allowed, access.denied, folder::access(&app.db, &access.dir)) {
            (true, _, Some(Access::Allowed)) => "Always allowed",
            (true, _, _) => "Allowed until you quit",
            (_, true, _) => "Not allowed",
            _ => "Asks the first time the assistant looks",
        };
        let height = 10 + rows.len() as u16 + u16::from(!commands.is_empty()) + 2;
        let area = draw::dialog_rect(screen, 76, height, 4);
        draw::boxed(buf, area, t.fg(Role::Accent), "This folder", "esc close");
        let inner = draw::inner(area);
        let right = inner.right();
        let mut y = inner.y;
        put(buf, inner.x, y, &text::truncate_left(&place, inner.width as usize), t.bold(Role::Link), right);
        y += 1;
        put_line(buf, inner.x, y, &Line::from(vec![Span::styled("Now  ", t.fg(Role::Dim)), Span::styled(state, t.fg(Role::Strong))]), right);
        y += 2;
        for row in rows {
            put_line(buf, inner.x, y, &Line::from(row), right);
            y += 1;
        }
        if !commands.is_empty() {
            let list = commands.join(", ");
            put(buf, inner.x, y, &text::truncate(&format!("Always run: {list} · c clears"), inner.width as usize), t.fg(Role::Dim), right);
            y += 1;
        }
        y += 1;
        for (index, (label, typed, _)) in OPTIONS.iter().enumerate() {
            let selected = index == self.selection;
            let line = if selected {
                Line::from(vec![Span::styled(format!(" › {}", text::pad(label, 26)), t.selected(Role::Accent)), Span::raw("   "), Span::styled(*typed, t.fg(Role::Text))])
            } else {
                Line::from(vec![Span::styled(format!("   {}", text::pad(label, 30)), t.fg(Role::Text)), Span::styled(*typed, t.fg(Role::Dim))])
            };
            put_line(buf, inner.x, y, &line, right);
            y += 1;
        }
        put(buf, inner.x, inner.bottom() - 1, "↑↓ move · enter choose · esc close", t.fg(Role::Dim), right);
    }

    /// Returns whether the dialog stays open.
    pub fn key(&mut self, app: &mut App, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Up => self.selection = self.selection.saturating_sub(1),
            KeyCode::Down | KeyCode::Tab => self.selection = (self.selection + 1).min(OPTIONS.len() - 1),
            KeyCode::Enter => {
                answer(app, OPTIONS[self.selection].2);
                return false;
            }
            KeyCode::Char('c') => clear_commands(app),
            KeyCode::Esc | KeyCode::Char('q') => return false,
            _ => {}
        }
        true
    }
}
