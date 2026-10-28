//! Cell-exact drawing helpers. The design is a character grid, so screens are
//! drawn by placing text at exact cells rather than through layout widgets.

use crate::ui::text::{clip_line, line_width, width};
use crate::ui::theme::{Depth, Role, theme};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

/// Write `text` at (x, y), never past `right` (exclusive). Returns the next x.
pub fn put(buf: &mut Buffer, x: u16, y: u16, text: &str, style: Style, right: u16) -> u16 {
    if x >= right || y >= buf.area.bottom() {
        return x;
    }
    let (next, _) = buf.set_stringn(x, y, text, (right - x) as usize, style);
    next
}

pub fn put_line(buf: &mut Buffer, x: u16, y: u16, line: &Line<'_>, right: u16) -> u16 {
    let mut cursor = x;
    for span in &line.spans {
        if cursor >= right {
            break;
        }
        cursor = put(buf, cursor, y, &span.content, span.style, right);
    }
    cursor
}

pub fn put_spans(buf: &mut Buffer, x: u16, y: u16, spans: &[Span<'_>], right: u16) -> u16 {
    let mut cursor = x;
    for span in spans {
        cursor = put(buf, cursor, y, &span.content, span.style, right);
    }
    cursor
}

/// Paint every cell of `area` with spaces in `style`.
pub fn fill(buf: &mut Buffer, area: Rect, style: Style) {
    let area = area.intersection(buf.area);
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.reset();
                cell.set_symbol(" ");
                cell.set_style(style);
            }
        }
    }
}

/// Apply `style` over existing cells without changing their text.
pub fn restyle(buf: &mut Buffer, area: Rect, style: Style) {
    buf.set_style(area.intersection(buf.area), style);
}

/// Dialogs are drawn over a dimmed chat: everything behind turns dim grey.
pub fn dim(buf: &mut Buffer, area: Rect) {
    let t = theme();
    let dim = t.color(Role::Dim);
    let bg = t.background();
    let area = area.intersection(buf.area);
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.modifier = Modifier::empty();
                if t.depth == Depth::None {
                    cell.modifier = Modifier::DIM;
                }
                cell.fg = dim;
                cell.bg = bg;
            }
        }
    }
}

/// A rounded box: `╭─ title ───── right ─╮`, sides, and bottom. The inside
/// is cleared to the background.
pub fn boxed(buf: &mut Buffer, area: Rect, border: Style, title: &str, right: &str) {
    if area.width < 4 || area.height < 2 {
        return;
    }
    let g = theme().glyphs();
    fill(buf, area, theme().base());
    let x0 = area.left();
    let x1 = area.right() - 1;
    let y0 = area.top();
    let y1 = area.bottom() - 1;
    let inner = area.width as usize - 2;

    let mut top = String::new();
    let mut used = 0;
    if !title.is_empty() {
        let title = crate::ui::text::truncate(title, inner.saturating_sub(4));
        top.push_str(&format!("{} {title} ", g.horizontal));
        used = width(&top);
    }
    let right_text = if right.is_empty() || used + width(right) + 4 > inner {
        String::new()
    } else {
        format!(" {right} {}", g.horizontal)
    };
    let fill_count = inner.saturating_sub(used + width(&right_text));
    top.push_str(&g.horizontal.repeat(fill_count));
    top.push_str(&right_text);
    put(buf, x0, y0, g.top_left, border, x1 + 1);
    put(buf, x0 + 1, y0, &top, border, x1);
    put(buf, x1, y0, g.top_right, border, x1 + 1);
    for y in y0 + 1..y1 {
        put(buf, x0, y, g.vertical, border, x1 + 1);
        put(buf, x1, y, g.vertical, border, x1 + 1);
    }
    put(buf, x0, y1, g.bottom_left, border, x1 + 1);
    put(buf, x0 + 1, y1, &g.horizontal.repeat(inner), border, x1);
    put(buf, x1, y1, g.bottom_right, border, x1 + 1);
}

/// The inside of a box drawn by [`boxed`], with one column of padding on each side.
pub fn inner(area: Rect) -> Rect {
    Rect {
        x: area.x + 2,
        y: area.y + 1,
        width: area.width.saturating_sub(4),
        height: area.height.saturating_sub(2),
    }
}

/// A full-width horizontal rule.
pub fn rule(buf: &mut Buffer, x: u16, y: u16, cols: u16, style: Style) {
    let g = theme().glyphs();
    put(buf, x, y, &g.horizontal.repeat(cols as usize), style, x + cols);
}

/// A list row inside a box: the selected row gets the highlight background
/// across the whole inner width.
pub fn row(buf: &mut Buffer, inner: Rect, y: u16, line: Line<'static>, selected: bool) {
    let line = clip_line(line, inner.width as usize);
    if selected {
        let bg = theme().selected(Role::Text);
        let row = Rect::new(inner.x, y, inner.width, 1);
        fill(buf, row, theme().base());
        restyle(buf, row, Style { fg: None, ..bg });
        let mut cursor = inner.x;
        for span in &line.spans {
            let style = span.style.patch(Style {
                bg: bg.bg,
                add_modifier: bg.add_modifier,
                ..Style::default()
            });
            cursor = put(buf, cursor, y, &span.content, style, inner.right());
        }
    } else {
        put_line(buf, inner.x, y, &line, inner.right());
    }
}

/// Left and right parts on one row, right part flush to the edge.
pub fn spread(buf: &mut Buffer, area: Rect, y: u16, left: &Line<'_>, right: &Line<'_>) {
    let right_width = line_width(right) as u16;
    let right_x = area.right().saturating_sub(right_width);
    put_line(buf, area.x, y, left, right_x.saturating_sub(1).max(area.x));
    put_line(buf, right_x.max(area.x), y, right, area.right());
}

/// Centre a box horizontally; under 100 columns dialogs go full width.
pub fn dialog_rect(screen: Rect, width: u16, height: u16, top: u16) -> Rect {
    let narrow = screen.width < 100;
    let width = if narrow { screen.width } else { width.min(screen.width) };
    let height = height.min(screen.height.saturating_sub(top + 4).max(5));
    let x = screen.x + (screen.width - width) / 2;
    let y = (screen.y + top).min(screen.bottom().saturating_sub(height + 4));
    Rect::new(x, y, width, height)
}
