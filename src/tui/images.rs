//! A new picture from `/image`: a preview over the dimmed chat, then Save,
//! Save as…, Copy or Discard; v shows it on the whole screen. Nothing is
//! written until you choose.
//!
//! ```text
//! ╭─ Save this picture? ─────────────────────────────────────── 1 of 2 · ← → ─╮
//! │ ▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀   a lighthouse in a storm                         │
//! │ ▀▀▀▀▀▀▀ preview ▀▀▀▀▀▀▀▀▀                                                   │
//! │ ▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀   Prompt used                                     │
//! │                             A lone lighthouse on black rocks, waves …       │
//! │                                                                             │
//! │                             gpt-image-1 · OpenAI · 6.2s                     │
//! │                             1024×1024 · PNG · 1.4 MB                        │
//! │                                                                             │
//! │                              › Save to ~/Downloads     enter                │
//! │                                Save as…                s                    │
//! │                                Copy the picture        c                    │
//! │                                Discard                 esc                  │
//! ╰─────────────────────────────────────────────────────────────────────────────╯
//! ```

use super::app::App;
use super::composer::Composer;
use super::draw::{self, put, put_line};
use crate::ui::markdown::wrap_segments;
use crate::ui::picture::{self, Picture, Preview};
use crate::ui::text;
use crate::ui::theme::{Role, theme};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use image::GenericImageView;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui_image::protocol::Protocol;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// What the image service sent back, before anything is saved.
pub struct Drawn {
    pub pictures: Vec<Picture>,
    /// The prompt the picture was drawn from (improved, unless --raw).
    pub used: String,
    /// "gpt-image-1 · OpenAI"
    pub label: String,
}

enum Outcome {
    Saved(PathBuf),
    Discarded,
}

struct Item {
    picture: Picture,
    outcome: Option<Outcome>,
}

/// Which picture, at how many columns and rows.
type PreviewKey = (usize, u16, u16);

enum Rendered {
    Blocks(Vec<Line<'static>>),
    Graphics(Protocol),
    Words,
}

pub struct ImageViewer {
    chat_id: String,
    prompt: String,
    used: String,
    label: String,
    ms: u64,
    stamp: String,
    items: Vec<Item>,
    index: usize,
    selection: usize,
    save_as: Option<Composer>,
    error: Option<String>,
    /// The picture on the whole screen.
    full: bool,
    /// Previews by (picture, columns, rows), kept between frames.
    cache: RefCell<HashMap<PreviewKey, Rendered>>,
}

const OPTIONS: [(&str, &str); 5] = [("", "enter"), ("Save as…", "s"), ("Copy the picture", "c"), ("View full screen", "v"), ("Discard", "esc")];

/// Room the words beside the preview need.
const WORDS: u16 = 46;

impl ImageViewer {
    pub fn new(chat_id: String, prompt: String, drawn: Drawn, ms: u64) -> Self {
        Self {
            chat_id,
            prompt,
            used: drawn.used,
            label: drawn.label,
            ms,
            stamp: chrono::Local::now().format("%Y%m%d-%H%M%S").to_string(),
            items: drawn.pictures.into_iter().map(|picture| Item { picture, outcome: None }).collect(),
            index: 0,
            selection: 0,
            save_as: None,
            error: None,
            full: false,
            cache: RefCell::new(HashMap::new()),
        }
    }

    fn file_name(&self, index: usize) -> String {
        let suffix = if self.items.len() > 1 { format!("-{}", index + 1) } else { String::new() };
        format!("aihat-{}{suffix}.{}", self.stamp, self.items[index].picture.extension())
    }

    pub fn draw(&self, app: &App, buf: &mut Buffer, screen: Rect) {
        if self.full {
            self.draw_full(app, buf, screen);
            return;
        }
        let t = theme();
        let item = &self.items[self.index];
        // The design's 20 rows on a 34-row window; taller windows get a
        // bigger picture, and the box grows to hold it.
        let height = screen.height.saturating_sub(14).clamp(20, 44).min(screen.height.saturating_sub(6)).max(10);
        let (w, h) = item.picture.image.dimensions();
        let max_cols = screen.width.saturating_sub(WORDS + 11).max(16);
        let (cols, rows) = picture::fit(w, h, max_cols, height - 2);
        let width = (cols + WORDS + 7).max(100).min(screen.width.saturating_sub(4).max(40));
        let area = draw::dialog_rect(screen, width, height, 3);
        let count = self.items.len();
        let right = if count > 1 { format!("{} of {count} · ← →", self.index + 1) } else { String::new() };
        draw::boxed(buf, area, t.fg(Role::Accent), "Save this picture?", &right);
        let inner = draw::inner(area);
        let (cols, rows) = if cols + WORDS + 3 > inner.width { picture::fit(w, h, inner.width.saturating_sub(WORDS + 3).max(12), inner.height) } else { (cols, rows) };

        // The preview on the left.
        self.draw_preview(app, buf, Rect::new(inner.x, inner.y, cols, rows.min(inner.height)));

        // Words on the right.
        let x = inner.x + cols + 3;
        let right_edge = inner.right();
        let column = right_edge.saturating_sub(x) as usize;
        let mut y = inner.y;
        let write = |buf: &mut Buffer, y: &mut u16, line: Line<'static>| {
            if *y < inner.bottom() {
                put_line(buf, x, *y, &text::clip_line(line, column), right_edge);
            }
            *y += 1;
        };
        for row in wrap_segments(&[(self.prompt.clone(), t.fg(Role::Strong))], column).into_iter().take(2) {
            write(buf, &mut y, Line::from(row));
        }
        if self.used.trim() != self.prompt.trim() {
            y += 1;
            write(buf, &mut y, Line::styled("Prompt used", t.fg(Role::Dim)));
            let rows = wrap_segments(&[(self.used.clone(), t.fg(Role::Text))], column);
            let shown = rows.len().min(4);
            for (index, row) in rows.into_iter().take(shown).enumerate() {
                let mut line = Line::from(row);
                if index + 1 == shown && shown == 4 {
                    line = text::clip_line(line, column.saturating_sub(1));
                    line.spans.push(Span::styled("…", t.fg(Role::Dim)));
                }
                write(buf, &mut y, line);
            }
        }
        y += 1;
        let mut meta = self.label.clone();
        if !meta.is_empty() {
            meta.push_str(" · ");
        }
        meta.push_str(&text::duration(self.ms));
        write(buf, &mut y, Line::styled(meta, t.fg(Role::Dim)));
        write(buf, &mut y, Line::styled(item.picture.describe(), t.fg(Role::Dim)));
        y += 1;

        if let Some(Outcome::Saved(path)) = &item.outcome {
            write(buf, &mut y, Line::from(vec![
                Span::styled("✓ Saved ", t.fg(Role::Accent)),
                Span::styled(text::home_relative(path), t.fg(Role::Link)),
            ]));
        } else if let Some(editor) = &self.save_as {
            write(buf, &mut y, Line::styled("Save as", t.fg(Role::Strong)));
            if y < inner.bottom() {
                put(buf, x, y, "›", t.fg(Role::Accent), right_edge);
                let field = column.saturating_sub(3);
                let value = editor.text();
                let shown = text::truncate_left(value, field.saturating_sub(1));
                let end = put(buf, x + 2, y, &shown, t.fg(Role::Strong), right_edge);
                put(buf, end, y, " ", t.cursor(), right_edge);
            }
            y += 1;
            write(buf, &mut y, Line::styled("enter saves · esc back · missing folders are made", t.fg(Role::Dim)));
        } else {
            let folder = text::home_relative(&app.config.image_folder(&app.paths));
            for (index, (label, key)) in OPTIONS.iter().enumerate() {
                let label = if index == 0 { format!("Save to {folder}") } else { label.to_string() };
                let selected = index == self.selection;
                let marker = if selected { "› " } else { "  " };
                let style = if selected { t.selected(Role::Accent) } else { t.fg(Role::Text) };
                let label_width = column.saturating_sub(12).min(28);
                write(buf, &mut y, Line::from(vec![
                    Span::styled(format!(" {marker}{}", text::pad(&text::truncate_left(&label, label_width), label_width)), style),
                    Span::raw(" "),
                    Span::styled(key.to_string(), t.fg(Role::Dim)),
                ]));
            }
        }
        if let Some(error) = &self.error {
            y += 1;
            write(buf, &mut y, Line::styled(format!("✗ {error}"), t.fg(Role::Error)));
        }
    }

    /// The picture as large as the screen allows, with the keys under it.
    fn draw_full(&self, app: &App, buf: &mut Buffer, screen: Rect) {
        let t = theme();
        draw::fill(buf, screen, t.base());
        let (w, h) = self.items[self.index].picture.image.dimensions();
        let (cols, rows) = picture::fit(w, h, screen.width, screen.height.saturating_sub(2));
        let x = screen.x + (screen.width - cols) / 2;
        self.draw_preview(app, buf, Rect::new(x, screen.y, cols, rows));
        let count = self.items.len();
        let which = if count > 1 { format!("{} of {count} · ← → · ", self.index + 1) } else { String::new() };
        let hint = format!("{which}{} · v or esc back · enter save · s save as · c copy", self.items[self.index].picture.describe());
        let hint = text::truncate(&hint, screen.width as usize);
        let hint_x = screen.x + (screen.width.saturating_sub(text::width(&hint) as u16)) / 2;
        put(buf, hint_x, screen.bottom() - 1, &hint, t.fg(Role::Dim), screen.right());
    }

    fn draw_preview(&self, app: &App, buf: &mut Buffer, area: Rect) {
        let t = theme();
        let key = (self.index, area.width, area.height);
        let mut cache = self.cache.borrow_mut();
        if !cache.contains_key(&key) {
            let image = &self.items[self.index].picture.image;
            let how = picture::preview(&app.config.images.preview);
            let rendered = match how {
                Preview::Off => Rendered::Words,
                Preview::Graphics(picker) => picture::graphics(picker, image, area.width, area.height)
                    .map(Rendered::Graphics)
                    .unwrap_or(Rendered::Words),
                cells => cells.cells(image, area.width, area.height).map(Rendered::Blocks).unwrap_or(Rendered::Words),
            };
            if cache.len() >= 6 {
                cache.clear();
            }
            cache.insert(key, rendered);
        }
        match cache.get(&key) {
            Some(Rendered::Blocks(lines)) => {
                for (offset, line) in lines.iter().enumerate() {
                    put_line(buf, area.x, area.y + offset as u16, line, area.right());
                }
            }
            Some(Rendered::Graphics(protocol)) => picture::render_graphics(protocol, area, buf),
            _ => {
                let (w, h) = self.items[self.index].picture.image.dimensions();
                put(buf, area.x, area.y, &format!("▣ {w}×{h} picture"), t.fg(Role::Dim), area.right());
                put(buf, area.x, area.y + 1, "(no preview in this terminal)", t.fg(Role::Dim), area.right());
            }
        }
    }

    /// Returns false once every picture has been saved or discarded.
    pub fn key(&mut self, app: &mut App, key: KeyEvent) -> bool {
        self.error = None;
        if let Some(editor) = self.save_as.as_mut() {
            let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
            match key.code {
                KeyCode::Esc => self.save_as = None,
                KeyCode::Enter => {
                    let typed = editor.text().trim().to_string();
                    match self.save_to(app, &typed) {
                        Ok(()) => {
                            self.save_as = None;
                            return self.advance(app);
                        }
                        Err(error) => self.error = Some(error),
                    }
                }
                KeyCode::Char('u') if ctrl => editor.delete_to_start(),
                KeyCode::Char('w') if ctrl => editor.delete_word(),
                KeyCode::Char(c) if !ctrl => editor.insert(c.encode_utf8(&mut [0; 4])),
                KeyCode::Backspace => editor.backspace(),
                KeyCode::Delete => editor.delete(),
                KeyCode::Left => editor.left(),
                KeyCode::Right => editor.right(),
                KeyCode::Home => editor.home(),
                KeyCode::End => editor.end(),
                _ => {}
            }
            return true;
        }
        if self.items[self.index].outcome.is_some() {
            // A saved picture: any key moves on.
            return self.advance(app);
        }
        if self.full && matches!(key.code, KeyCode::Esc | KeyCode::Char('v') | KeyCode::Char('q')) {
            self.full = false;
            return true;
        }
        match key.code {
            KeyCode::Up => self.selection = self.selection.saturating_sub(1),
            KeyCode::Down | KeyCode::Tab => self.selection = (self.selection + 1).min(OPTIONS.len() - 1),
            KeyCode::Left if self.items.len() > 1 => self.index = (self.index + self.items.len() - 1) % self.items.len(),
            KeyCode::Right if self.items.len() > 1 => self.index = (self.index + 1) % self.items.len(),
            KeyCode::Enter => return self.choose(app, self.selection),
            KeyCode::Char('s') => return self.choose(app, 1),
            KeyCode::Char('c') => return self.choose(app, 2),
            KeyCode::Char('v') => return self.choose(app, 3),
            KeyCode::Esc | KeyCode::Char('d') => return self.choose(app, 4),
            _ => {}
        }
        true
    }

    fn choose(&mut self, app: &mut App, option: usize) -> bool {
        match option {
            0 => {
                let folder = app.config.image_folder(&app.paths);
                let path = picture::unique_path(&folder, &self.file_name(self.index));
                match self.write(&path) {
                    Ok(()) => {
                        app.ok(format!("Saved {}", text::home_relative(&path)));
                        self.advance(app)
                    }
                    Err(error) => {
                        self.error = Some(error);
                        true
                    }
                }
            }
            1 => {
                let folder = app.config.image_folder(&app.paths);
                let mut editor = Composer::default();
                editor.set(text::home_relative(&folder.join(self.file_name(self.index))));
                self.save_as = Some(editor);
                true
            }
            2 => {
                match super::tasks::copy_image(&self.items[self.index].picture) {
                    Ok(()) => app.ok("Copied the picture"),
                    Err(error) => self.error = Some(format!("Could not copy it: {error}")),
                }
                true
            }
            3 => {
                self.full = true;
                true
            }
            _ => {
                self.items[self.index].outcome = Some(Outcome::Discarded);
                self.advance(app)
            }
        }
    }

    /// Save as: a folder gets the usual file name, a name without an
    /// extension gets the picture's, and nothing is overwritten.
    fn save_to(&mut self, app: &mut App, typed: &str) -> Result<(), String> {
        if typed.is_empty() {
            return Err("Type a file name or a folder".into());
        }
        let path = picture::save_as_target(typed, &self.file_name(self.index), self.items[self.index].picture.extension());
        if path.exists() {
            return Err(format!("{} already exists · choose another name", text::home_relative(&path)));
        }
        self.write(&path)?;
        app.ok(format!("Saved {}", text::home_relative(&path)));
        Ok(())
    }

    fn write(&mut self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(|error| format!("Could not make {}: {error}", text::home_relative(parent)))?;
        }
        let item = &mut self.items[self.index];
        std::fs::write(path, &item.picture.bytes).map_err(|error| format!("Could not save {}: {error}", text::home_relative(path)))?;
        item.outcome = Some(Outcome::Saved(path.to_path_buf()));
        Ok(())
    }

    /// Show the next picture still waiting for an answer, or finish.
    fn advance(&mut self, app: &mut App) -> bool {
        self.selection = 0;
        self.full = false;
        let count = self.items.len();
        if let Some(next) = (1..=count).map(|step| (self.index + step) % count).find(|i| self.items[*i].outcome.is_none()) {
            self.index = next;
            return true;
        }
        let saved: Vec<PathBuf> = self
            .items
            .iter()
            .filter_map(|item| match &item.outcome {
                Some(Outcome::Saved(path)) => Some(path.clone()),
                _ => None,
            })
            .collect();
        app.finish_image(&self.chat_id, &self.prompt, &self.used, &saved, self.ms);
        false
    }
}

/// Rows shown in the chat while a picture is being drawn.
pub fn drawing_lines(prompt: &str, stage: &str, elapsed_ms: u64, layout: super::transcript::Layout) -> Vec<Line<'static>> {
    let t = theme();
    let detail = text::truncate(&format!("\"{prompt}\""), layout.width.saturating_sub(12));
    vec![
        Line::from(vec![
            Span::styled(" ● ", t.fg(Role::Accent)),
            Span::styled("image ", t.fg(Role::Dim)),
            Span::styled(detail, t.fg(Role::Text)),
        ]),
        Line::styled(format!("   └ {stage}… {}", text::seconds(elapsed_ms)), t.fg(Role::Dim)),
    ]
}
