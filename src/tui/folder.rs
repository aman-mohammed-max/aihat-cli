//! "Let AI Hat read this folder?": asked the first time aihat opens in a
//! folder, and again with `/folder`. Allowed, the assistant can read and
//! search the folder's files and ask to run commands there.

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
use std::path::PathBuf;

pub struct FolderDialog {
    dir: PathBuf,
    selection: usize,
    /// Opened by aihat on its own (not with /folder).
    first_time: bool,
}

// Only arrows and enter choose: letters typed straight away must not
// answer a question about access by accident.
const OPTIONS: [(&str, &str); 3] = [("Yes, allow this folder", ""), ("Only for now", ""), ("No, keep it private", "esc")];

impl FolderDialog {
    pub fn new(dir: PathBuf, first_time: bool) -> Self {
        Self { dir, selection: 0, first_time }
    }

    pub fn draw(&self, app: &App, buf: &mut Buffer, screen: Rect) {
        let t = theme();
        let place = text::home_relative(&self.dir);
        let explain = "The assistant can then list, read and search the files in this folder and the folders inside it, never outside it. Before it runs any command here, it asks you first.";
        let rows = wrap_segments(&[(explain.to_string(), t.fg(Role::Text))], 70);
        let commands = folder::allowed_commands(&app.db, &self.dir);
        let state = match (app.folder.is_some(), folder::access(&app.db, &self.dir)) {
            (true, Some(Access::Allowed)) => "Allowed",
            (true, _) => "Allowed for now",
            (false, Some(Access::Denied)) => "Kept private",
            (false, _) => "Not decided",
        };
        let extra = u16::from(!self.first_time) + u16::from(!commands.is_empty());
        let height = 9 + rows.len() as u16 + extra + 2;
        let area = draw::dialog_rect(screen, 76, height, 4);
        draw::boxed(buf, area, t.fg(Role::Accent), "Let AI Hat read this folder?", "");
        let inner = draw::inner(area);
        let right = inner.right();
        let mut y = inner.y;
        put(buf, inner.x, y, &place, t.bold(Role::Link), right);
        y += 2;
        for row in rows {
            put_line(buf, inner.x, y, &Line::from(row), right);
            y += 1;
        }
        y += 1;
        if !self.first_time {
            put_line(buf, inner.x, y, &Line::from(vec![Span::styled("Now  ", t.fg(Role::Dim)), Span::styled(state, t.fg(Role::Strong))]), right);
            y += 1;
        }
        if !commands.is_empty() {
            let list = commands.iter().map(|c| format!("`{c}`")).collect::<Vec<_>>().join(", ");
            let line = format!("Always allowed: {list} · c clears them");
            put(buf, inner.x, y, &text::truncate(&line, inner.width as usize), t.fg(Role::Dim), right);
            y += 1;
        }
        if !self.first_time || !commands.is_empty() {
            y += 1;
        }
        for (index, (label, key)) in OPTIONS.iter().enumerate() {
            let selected = index == self.selection;
            let line = if selected {
                Line::from(vec![
                    Span::styled(format!(" › {}", text::pad(label, 26)), t.selected(Role::Accent)),
                    Span::raw("   "),
                    Span::styled(*key, t.fg(Role::Text)),
                ])
            } else {
                Line::from(vec![Span::styled(format!("   {}", text::pad(label, 30)), t.fg(Role::Text)), Span::styled(*key, t.fg(Role::Dim))])
            };
            put_line(buf, inner.x, y, &line, right);
            y += 1;
        }
        let hint = if self.first_time { "↑↓ move · enter choose · /folder changes this later" } else { "↑↓ move · enter choose · esc close" };
        put(buf, inner.x, inner.bottom() - 1, hint, t.fg(Role::Dim), right);
    }

    /// Returns whether the dialog stays open.
    pub fn key(&mut self, app: &mut App, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Up => self.selection = self.selection.saturating_sub(1),
            KeyCode::Down | KeyCode::Tab => self.selection = (self.selection + 1).min(OPTIONS.len() - 1),
            KeyCode::Enter => {
                self.choose(app, self.selection);
                return false;
            }
            KeyCode::Esc => {
                // Closing /folder without choosing changes nothing.
                if self.first_time {
                    self.choose(app, 2);
                }
                return false;
            }
            KeyCode::Char('c') if !self.first_time => match folder::forget_commands(&app.db, &self.dir) {
                Ok(()) => app.ok("Every command asks first again"),
                Err(error) => app.fail(error.to_string()),
            },
            _ => {}
        }
        true
    }

    fn choose(&self, app: &mut App, option: usize) {
        let place = text::home_relative(&self.dir);
        let saved = match option {
            0 => folder::set_access(&app.db, &self.dir, Some(Access::Allowed)),
            1 => folder::set_access(&app.db, &self.dir, None),
            _ => folder::set_access(&app.db, &self.dir, Some(Access::Denied)),
        };
        if let Err(error) = saved {
            app.fail(error.to_string());
            return;
        }
        if option < 2 {
            app.folder = Some(self.dir.clone());
            app.ok(if option == 0 { format!("The assistant can read {place} · commands ask first") } else { format!("The assistant can read {place} until you quit") });
        } else {
            app.folder = None;
            app.notify(Role::Dim, format!("{place} stays private · /folder changes this"));
        }
    }
}
