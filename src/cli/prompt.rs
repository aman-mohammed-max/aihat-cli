//! Guided questions in the style of the design's `providers add` flow:
//!
//! ```text
//! ┌ Add an OpenAI-compatible endpoint
//! │
//! ◇ Name
//! │ Home vLLM
//! │
//! ◆ Models to show in the picker (space toggles · enter confirms)
//! │ ■ qwen3-32b          chat · tools · 128k
//! └
//! ```
//!
//! The question being answered is drawn with `◆` and cyan bars; once answered
//! it is redrawn with `◇` and the answer.

use crate::ui::ansi::paint;
use crate::ui::text::{pad, width};
use crate::ui::theme::{Role, theme};
use anyhow::{Result, bail};
use crossterm::cursor::{MoveToColumn, MoveUp};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{self, Clear, ClearType};
use crossterm::{execute, queue};
use std::io::{IsTerminal, Write};

fn out() -> std::io::Stderr {
    std::io::stderr()
}

fn bar(active: bool) -> String {
    let role = if active { Role::Link } else { Role::Dim };
    paint(theme().glyphs().vertical, theme().fg(role))
}

pub fn interactive() -> bool {
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

pub fn intro(title: &str) {
    let t = theme();
    eprintln!("{} {}", paint("┌", t.fg(Role::Accent)), paint(title, t.bold(Role::Strong)));
    eprintln!("{}", bar(false));
}

pub fn outro() {
    eprintln!("{}", paint("└", theme().fg(Role::Dim)));
}

/// A step that runs while the user waits: `◇ Testing connection… ✓ Connected`.
pub fn step(title: &str, result: &str, role: Role) {
    let t = theme();
    eprintln!("{} {title} {}", paint("◇", t.fg(Role::Accent)), paint(result, t.fg(role)));
    eprintln!("{}", bar(false));
}

struct Raw;

impl Raw {
    fn on() -> Result<Self> {
        terminal::enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for Raw {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
    }
}

/// Redraws a block of lines in place.
struct Block {
    drawn: u16,
}

impl Block {
    fn draw(&mut self, lines: &[String]) -> Result<()> {
        let mut err = out();
        if self.drawn > 0 {
            queue!(err, MoveUp(self.drawn), MoveToColumn(0))?;
        }
        queue!(err, Clear(ClearType::FromCursorDown))?;
        for line in lines {
            write!(err, "{line}\r\n")?;
        }
        err.flush()?;
        self.drawn = lines.len() as u16;
        Ok(())
    }
}

fn read_key() -> Result<KeyEvent> {
    loop {
        if let Event::Key(key) = event::read()?
            && key.kind != KeyEventKind::Release {
                if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                    let _ = terminal::disable_raw_mode();
                    let _ = execute!(out(), MoveToColumn(0));
                    eprintln!("{}", paint("■ Cancelled", theme().fg(Role::Warn)));
                    std::process::exit(130);
                }
                return Ok(key);
            }
    }
}

fn title_line(active: bool, title: &str, hint: &str) -> String {
    let t = theme();
    let (glyph, role) = if active { ("◆", Role::Link) } else { ("◇", Role::Accent) };
    let hint = if hint.is_empty() { String::new() } else { format!(" {}", paint(&format!("({hint})"), t.fg(Role::Dim))) };
    format!("{} {title}{hint}", paint(glyph, t.fg(role)))
}

/// Pick one of a few options shown on one row: `● Custom  ○ OpenRouter  ○ Groq`.
pub fn choose(title: &str, options: &[&str], initial: usize) -> Result<usize> {
    if !interactive() {
        bail!("{title}: this question needs an interactive terminal");
    }
    let t = theme();
    let mut index = initial.min(options.len().saturating_sub(1));
    let render = |index: usize| -> String {
        options
            .iter()
            .enumerate()
            .map(|(i, option)| {
                if i == index {
                    format!("{} {option}", paint("●", t.fg(Role::Accent)))
                } else {
                    paint(&format!("○ {option}"), t.fg(Role::Dim))
                }
            })
            .collect::<Vec<_>>()
            .join("  ")
    };
    let mut block = Block { drawn: 0 };
    {
        let _raw = Raw::on()?;
        loop {
            block.draw(&[title_line(true, title, "← → choose · enter confirms"), format!("{} {}", bar(true), render(index))])?;
            match read_key()?.code {
                KeyCode::Left | KeyCode::Up | KeyCode::BackTab => index = (index + options.len() - 1) % options.len(),
                KeyCode::Right | KeyCode::Down | KeyCode::Tab | KeyCode::Char(' ') => index = (index + 1) % options.len(),
                KeyCode::Enter => break,
                _ => {}
            }
        }
        block.draw(&[title_line(false, title, ""), format!("{} {}", bar(false), render(index)), bar(false)])?;
    }
    Ok(index)
}

/// A line of text. `secret` shows dots instead of the characters.
pub fn input(title: &str, hint: &str, initial: &str, secret: bool) -> Result<String> {
    if !interactive() {
        bail!("{title}: this question needs an interactive terminal");
    }
    let t = theme();
    let mut value = initial.to_string();
    let shown = |value: &str| -> String {
        if secret {
            "•".repeat(value.chars().count())
        } else {
            value.to_string()
        }
    };
    let mut block = Block { drawn: 0 };
    {
        let _raw = Raw::on()?;
        loop {
            let cursor = paint(" ", t.cursor());
            block.draw(&[title_line(true, title, hint), format!("{} {}{cursor}", bar(true), shown(&value)), paint("└", t.fg(Role::Link))])?;
            let key = read_key()?;
            match key.code {
                KeyCode::Enter => break,
                KeyCode::Backspace => {
                    value.pop();
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => value.clear(),
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => value.push(c),
                _ => {}
            }
        }
        let answer = if value.is_empty() { paint("none", t.fg(Role::Dim)) } else { shown(&value) };
        block.draw(&[title_line(false, title, hint), format!("{} {answer}", bar(false)), bar(false)])?;
    }
    Ok(value.trim().to_string())
}

/// Toggle items on and off: `■ qwen3-32b   chat · tools · 128k`.
pub fn toggles(title: &str, hint: &str, items: &[(String, String)], checked: &mut [bool]) -> Result<()> {
    if !interactive() {
        bail!("{title}: this question needs an interactive terminal");
    }
    let t = theme();
    let name_width = items.iter().map(|(name, _)| width(name)).max().unwrap_or(10).min(28) + 3;
    let visible = 12usize;
    let mut cursor = 0usize;
    let mut block = Block { drawn: 0 };
    let render = |cursor: Option<usize>, checked: &[bool]| -> Vec<String> {
        let first = cursor.map(|c| (c + 1).saturating_sub(visible)).unwrap_or(0);
        items
            .iter()
            .enumerate()
            .skip(first)
            .take(if cursor.is_some() { visible } else { items.len() })
            .map(|(i, (name, detail))| {
                let row = if checked[i] {
                    format!("{} {}{}", paint("■", t.fg(Role::Accent)), pad(name, name_width), paint(detail, t.fg(Role::Dim)))
                } else {
                    paint(&format!("□ {}{detail}", pad(name, name_width)), t.fg(Role::Dim))
                };
                let row = if cursor == Some(i) { paint(&strip(&row), t.selected(Role::Accent)) } else { row };
                format!("{} {row}", bar(cursor.is_some()))
            })
            .collect()
    };
    {
        let _raw = Raw::on()?;
        loop {
            let mut lines = vec![title_line(true, title, hint)];
            lines.extend(render(Some(cursor), checked));
            lines.push(paint("└", t.fg(Role::Link)));
            block.draw(&lines)?;
            match read_key()?.code {
                KeyCode::Up => cursor = cursor.saturating_sub(1),
                KeyCode::Down => cursor = (cursor + 1).min(items.len().saturating_sub(1)),
                KeyCode::Char(' ') => {
                    if let Some(value) = checked.get_mut(cursor) {
                        *value = !*value;
                    }
                }
                KeyCode::Char('a') => {
                    let all = checked.iter().all(|c| *c);
                    checked.iter_mut().for_each(|c| *c = !all);
                }
                KeyCode::Enter => break,
                _ => {}
            }
        }
        let mut lines = vec![title_line(false, title, "")];
        let chosen: Vec<&str> = items
            .iter()
            .zip(checked.iter())
            .filter(|(_, on)| **on)
            .map(|(item, _)| item.0.as_str())
            .collect();
        let summary = match chosen.len() {
            0 => paint("none", t.fg(Role::Dim)),
            n if n <= 4 => chosen.join(", "),
            n => format!("{}, … {} in all", chosen[..3].join(", "), n),
        };
        lines.push(format!("{} {summary}", bar(false)));
        lines.push(bar(false));
        block.draw(&lines)?;
    }
    Ok(())
}

fn strip(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for next in chars.by_ref() {
                if next == 'm' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// `◇ Make Home vLLM the default?  ● Yes  ○ No`
pub fn yes_no(title: &str, initial: bool) -> Result<bool> {
    Ok(choose(title, &["Yes", "No"], if initial { 0 } else { 1 })? == 0)
}

/// `? Remove LM Studio (laptop)? Chats that used it stay. (y/N)`
pub fn confirm(question: &str) -> Result<bool> {
    let t = theme();
    if !std::io::stdin().is_terminal() {
        bail!("{question} Pass --yes to confirm without a terminal.");
    }
    eprint!("{} {question} {} ", paint("?", t.fg(Role::Warn)), paint("(y/N)", t.fg(Role::Dim)));
    let _ = std::io::stderr().flush();
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    Ok(matches!(answer.trim().to_lowercase().as_str(), "y" | "yes"))
}
