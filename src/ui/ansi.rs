//! Turn styled rows into text for stdout/stderr. When colour is off (pipes,
//! files, NO_COLOR) the escape codes are left out entirely.

use super::theme::{Depth, theme};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use std::fmt::Write as _;

pub fn line(line: &Line<'_>) -> String {
    let mut out = String::new();
    for span in &line.spans {
        out.push_str(&span_text(span));
    }
    out
}

pub fn lines(lines: &[Line<'_>]) -> String {
    let mut out = String::new();
    for row in lines {
        out.push_str(line(row).trim_end());
        out.push('\n');
    }
    out
}

pub fn span_text(span: &Span<'_>) -> String {
    paint(&span.content, span.style)
}

pub fn paint(text: &str, style: Style) -> String {
    if theme().depth == Depth::None && style.add_modifier.is_empty() || text.is_empty() {
        return text.to_string();
    }
    let codes = sgr(style);
    if codes.is_empty() {
        return text.to_string();
    }
    format!("\x1b[{codes}m{text}\x1b[0m")
}

fn sgr(style: Style) -> String {
    let mut codes = Vec::new();
    let modifiers = style.add_modifier;
    if modifiers.contains(Modifier::BOLD) {
        codes.push("1".to_string());
    }
    if modifiers.contains(Modifier::DIM) {
        codes.push("2".into());
    }
    if modifiers.contains(Modifier::ITALIC) {
        codes.push("3".into());
    }
    if modifiers.contains(Modifier::UNDERLINED) {
        codes.push("4".into());
    }
    if modifiers.contains(Modifier::REVERSED) {
        codes.push("7".into());
    }
    if modifiers.contains(Modifier::CROSSED_OUT) {
        codes.push("9".into());
    }
    if theme().depth != Depth::None {
        if let Some(code) = style.fg.and_then(|color| color_code(color, false)) {
            codes.push(code);
        }
        if let Some(code) = style.bg.and_then(|color| color_code(color, true)) {
            codes.push(code);
        }
    }
    codes.join(";")
}

fn color_code(color: Color, background: bool) -> Option<String> {
    let base = if background { 40 } else { 30 };
    let mut out = String::new();
    match color {
        Color::Reset => return None,
        Color::Rgb(r, g, b) => {
            let _ = write!(out, "{};2;{r};{g};{b}", base + 8);
        }
        Color::Indexed(index) => {
            let _ = write!(out, "{};5;{index}", base + 8);
        }
        named => {
            let (offset, bright) = match named {
                Color::Black => (0, false),
                Color::Red => (1, false),
                Color::Green => (2, false),
                Color::Yellow => (3, false),
                Color::Blue => (4, false),
                Color::Magenta => (5, false),
                Color::Cyan => (6, false),
                Color::Gray => (7, false),
                Color::DarkGray => (0, true),
                Color::LightRed => (1, true),
                Color::LightGreen => (2, true),
                Color::LightYellow => (3, true),
                Color::LightBlue => (4, true),
                Color::LightMagenta => (5, true),
                Color::LightCyan => (6, true),
                Color::White => (7, true),
                _ => return None,
            };
            let _ = write!(out, "{}", if bright { base + 60 + offset } else { base + offset });
        }
    }
    Some(out)
}
