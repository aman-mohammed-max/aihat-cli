//! `/about`: the phone app's About screen in a dialog. Enter opens the
//! chosen link in the browser; without one, the link is copied instead.

use super::app::App;
use super::draw::{self, put, put_line};
use crate::about::{self, APP_LINKS, BIO, MAKER, MAKER_LINKS, ROLE};
use crate::ui::markdown::wrap_segments;
use crate::ui::text;
use crate::ui::theme::{Role, theme};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};

#[derive(Default)]
pub struct AboutDialog {
    pub selection: usize,
}

const LOGO: [(&str, u16); 4] = [("▗▄▄▄▄▖", 1), ("▟██████▙", 0), ("▜██████▛", 0), ("▀▀▀▀", 2)];

impl AboutDialog {
    pub fn draw(&self, buf: &mut Buffer, screen: Rect) {
        let t = theme();
        let bio = wrap_segments(&[(BIO.to_string(), t.fg(Role::Text))], 70);
        let full = 20 + bio.len() as u16;
        let area = draw::dialog_rect(screen, 76, full + 2, 3);
        draw::boxed(buf, area, t.fg(Role::Accent), "About", "esc close");
        let inner = draw::inner(area);
        let right = inner.right();
        // Short windows drop the logo, then the bio, so every link shows.
        let logo = inner.height >= full;
        let with_bio = inner.height >= full - 5;
        let mut y = inner.y;

        if logo {
            y += 1;
            let beside = [
                Line::styled("AI Hat", t.bold(Role::Strong)),
                Line::styled(about::version_line(), t.fg(Role::Dim)),
                Line::styled("Chat with any model, on your terms", t.fg(Role::Dim)),
            ];
            for (row, (mark, indent)) in LOGO.iter().enumerate() {
                put(buf, inner.x + 1 + indent, y, mark, t.fg(Role::Accent), right);
                if let Some(line) = beside.get(row) {
                    put_line(buf, inner.x + 12, y, line, right);
                }
                y += 1;
            }
        } else {
            put_line(
                buf,
                inner.x,
                y,
                &Line::from(vec![Span::styled("◆ AI Hat ", t.bold(Role::Accent)), Span::styled(about::version_line(), t.fg(Role::Dim))]),
                right,
            );
            y += 1;
        }
        y += 1;

        put(buf, inner.x, y, "MADE BY", t.fg(Role::Dim), right);
        y += 1;
        if with_bio {
            put(buf, inner.x, y, MAKER, t.bold(Role::Strong), right);
            y += 1;
            put(buf, inner.x, y, ROLE, t.fg(Role::Text), right);
            y += 1;
            for row in bio {
                put_line(buf, inner.x, y, &Line::from(row), right);
                y += 1;
            }
        } else {
            put_line(buf, inner.x, y, &Line::from(vec![Span::styled(MAKER, t.bold(Role::Strong)), Span::styled(format!(" · {ROLE}"), t.fg(Role::Dim))]), right);
            y += 1;
        }
        y += 1;

        // The last row is for the keys.
        let last = inner.bottom().saturating_sub(1);
        let link_row = |buf: &mut Buffer, y: u16, label: &str, detail: &str, detail_role: Role, index: usize| {
            if y >= last {
                return;
            }
            let selected = index == self.selection;
            let label_style = if selected { t.fg(Role::Accent) } else { t.fg(Role::Strong) };
            let line = Line::from(vec![
                Span::styled(text::pad(label, 23), label_style),
                Span::styled(detail.to_string(), if selected { label_style } else { t.fg(detail_role) }),
            ]);
            draw::row(buf, inner, y, line, selected);
            put(buf, right - 1, y, "›", if selected { t.fg(Role::Accent) } else { t.fg(Role::Dim) }, right);
        };
        for (index, link) in MAKER_LINKS.iter().enumerate() {
            link_row(buf, y, link.label, link.detail, Role::Link, index);
            y += 1;
        }
        y += 1;
        if y < last {
            put(buf, inner.x, y, "ABOUT THE APP", t.fg(Role::Dim), right);
        }
        y += 1;
        for (index, link) in APP_LINKS.iter().enumerate() {
            link_row(buf, y, link.label, link.detail, Role::Dim, MAKER_LINKS.len() + index);
            y += 1;
        }
        let hint = "↑↓ move · enter open in your browser · c copy the link · esc close";
        put(buf, inner.x, last, &text::truncate(hint, inner.width as usize), t.fg(Role::Dim), right);
    }

    /// Returns whether the dialog stays open.
    pub fn key(&mut self, app: &mut App, key: KeyEvent) -> bool {
        let count = about::links().count();
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return false,
            KeyCode::Up | KeyCode::Char('k') => self.selection = (self.selection + count - 1) % count,
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => self.selection = (self.selection + 1) % count,
            KeyCode::Enter => {
                if let Some(link) = about::links().nth(self.selection) {
                    open(app, link.url);
                }
            }
            KeyCode::Char('c') => {
                if let Some(link) = about::links().nth(self.selection) {
                    match super::tasks::copy_to_clipboard(link.url) {
                        Ok(()) => app.ok(format!("Copied {}", link.url)),
                        Err(error) => app.fail(format!("Could not copy: {error}")),
                    }
                }
            }
            _ => {}
        }
        true
    }
}

/// Open a link in the browser, or copy it when there is no browser here.
fn open(app: &mut App, url: &str) {
    if crate::oauth::has_browser() && webbrowser::open(url).is_ok() {
        app.ok(format!("Opened {url}"));
        return;
    }
    let copied = super::tasks::copy_to_clipboard(url).is_ok();
    app.notify(Role::Warn, if copied { format!("No browser here · copied {url}") } else { format!("No browser here · {url}") });
}
