//! Markdown → terminal rows, using pulldown-cmark for parsing.
//!
//! Every returned `Line` is exactly one terminal row: wrapping, list markers,
//! quote bars and code gutters are all resolved here so the TUI and the CLI
//! print the same thing. Styling follows the design: numbered list markers in
//! the accent colour, `[n]` citations in cyan, code blocks as `1 │ code` with
//! dim line numbers and role-coloured highlighting.

use super::highlight::CodeHighlighter;
use super::text::width;
use super::theme::{Role, theme};
use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Copy, Debug)]
pub struct MdOptions {
    /// Total columns a row may use, indentation included.
    pub width: usize,
    /// Columns before body text.
    pub indent: usize,
    /// Extra columns before top-level lists and code blocks.
    pub block_indent: usize,
    /// Columns code blocks and tables may use: usually the whole terminal,
    /// since wrapped code is hard to read.
    pub wide: usize,
}

pub fn render(markdown: &str, options: MdOptions) -> Vec<Line<'static>> {
    let mut renderer = Renderer::new(options);
    let parser = Parser::new_ext(
        markdown,
        Options::ENABLE_TABLES
            | Options::ENABLE_STRIKETHROUGH
            | Options::ENABLE_TASKLISTS
            | Options::ENABLE_FOOTNOTES,
    );
    for event in parser {
        renderer.event(event);
    }
    renderer.finish()
}

#[derive(Clone, Debug)]
enum Prefix {
    Quote,
    Item {
        lead: usize,
        marker: Option<(String, Style)>,
        width: usize,
    },
}

struct ListState {
    next: Option<u64>,
}

struct CodeBlock {
    language: String,
    text: String,
}

#[derive(Default)]
struct TableState {
    alignments: Vec<Alignment>,
    rows: Vec<Vec<Vec<(String, Style)>>>,
    header_rows: usize,
    in_head: bool,
}

struct Renderer {
    options: MdOptions,
    out: Vec<Line<'static>>,
    inline: Vec<(String, Style)>,
    styles: Vec<Style>,
    prefixes: Vec<Prefix>,
    lists: Vec<ListState>,
    heading: Option<HeadingLevel>,
    code: Option<CodeBlock>,
    table: Option<TableState>,
    link_start: Vec<(usize, String)>,
    blank_pending: bool,
}

impl Renderer {
    fn new(options: MdOptions) -> Self {
        Self {
            options,
            out: Vec::new(),
            inline: Vec::new(),
            styles: Vec::new(),
            prefixes: Vec::new(),
            lists: Vec::new(),
            heading: None,
            code: None,
            table: None,
            link_start: Vec::new(),
            blank_pending: false,
        }
    }

    fn style(&self) -> Style {
        self.styles
            .last()
            .copied()
            .unwrap_or_else(|| theme().fg(Role::Text))
    }

    fn push_style(&mut self, f: impl FnOnce(Style) -> Style) {
        let next = f(self.style());
        self.styles.push(next);
    }

    fn event(&mut self, event: Event<'_>) {
        let t = theme();
        match event {
            Event::Start(tag) => match tag {
                Tag::Paragraph => self.flush(),
                Tag::Heading { level, .. } => {
                    self.flush();
                    self.heading = Some(level);
                    let style = if level == HeadingLevel::H1 {
                        t.bold(Role::Accent)
                    } else {
                        t.bold(Role::Strong)
                    };
                    self.styles.push(style);
                }
                Tag::BlockQuote(_) => {
                    self.flush();
                    self.prefixes.push(Prefix::Quote);
                }
                Tag::CodeBlock(kind) => {
                    self.flush();
                    let language = match kind {
                        CodeBlockKind::Fenced(info) => {
                            info.split_whitespace().next().unwrap_or("").to_string()
                        }
                        CodeBlockKind::Indented => String::new(),
                    };
                    self.code = Some(CodeBlock {
                        language,
                        text: String::new(),
                    });
                }
                Tag::List(start) => {
                    self.flush();
                    self.lists.push(ListState { next: start });
                }
                Tag::Item => {
                    self.flush();
                    let depth = self.lists.len();
                    let marker = match self.lists.last_mut().and_then(|list| list.next.as_mut()) {
                        Some(number) => {
                            let text = format!("{number}.");
                            *number += 1;
                            text
                        }
                        None => match depth {
                            0 | 1 => "•".to_string(),
                            2 => "◦".to_string(),
                            _ => "▪".to_string(),
                        },
                    };
                    let top_level = !self
                        .prefixes
                        .iter()
                        .any(|prefix| matches!(prefix, Prefix::Item { .. }));
                    let marker_width = width(&marker) + 1;
                    self.prefixes.push(Prefix::Item {
                        lead: if top_level { self.options.block_indent } else { 0 },
                        marker: Some((marker, t.fg(Role::Accent))),
                        width: marker_width,
                    });
                }
                Tag::Table(alignments) => {
                    self.flush();
                    self.table = Some(TableState {
                        alignments,
                        ..TableState::default()
                    });
                }
                Tag::TableHead => {
                    if let Some(table) = self.table.as_mut() {
                        table.in_head = true;
                        table.rows.push(Vec::new());
                    }
                }
                Tag::TableRow => {
                    if let Some(table) = self.table.as_mut() {
                        table.rows.push(Vec::new());
                    }
                }
                Tag::TableCell => self.inline.clear(),
                Tag::Emphasis => self.push_style(|s| s.add_modifier(Modifier::ITALIC)),
                Tag::Strong => {
                    let strong = t.color(Role::Strong);
                    self.push_style(|s| {
                        // Bold text inside headings keeps the heading colour.
                        let s = if s.fg == Some(t.color(Role::Text)) { s.fg(strong) } else { s };
                        s.add_modifier(Modifier::BOLD)
                    })
                }
                Tag::Strikethrough => self.push_style(|s| s.add_modifier(Modifier::CROSSED_OUT)),
                Tag::Link { dest_url, .. } => {
                    self.link_start.push((self.inline.len(), dest_url.to_string()));
                    let link = t.color(Role::Link);
                    self.push_style(|s| s.fg(link).add_modifier(Modifier::UNDERLINED));
                }
                Tag::Image { dest_url, .. } => {
                    self.inline.push(("[image: ".into(), t.fg(Role::Dim)));
                    self.link_start.push((self.inline.len(), dest_url.to_string()));
                    self.push_style(|_| t.fg(Role::Dim));
                }
                Tag::FootnoteDefinition(label) => {
                    self.flush();
                    self.inline.push((format!("[^{label}] "), t.fg(Role::Link)));
                }
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Paragraph => {
                    self.flush();
                    self.blank_pending = true;
                }
                TagEnd::Heading(_) => {
                    self.flush();
                    self.styles.pop();
                    self.heading = None;
                    self.blank_pending = true;
                }
                TagEnd::BlockQuote(_) => {
                    self.flush();
                    self.prefixes.pop();
                    self.blank_pending = true;
                }
                TagEnd::CodeBlock => {
                    if let Some(code) = self.code.take() {
                        self.render_code(&code);
                    }
                    self.blank_pending = true;
                }
                TagEnd::List(_) => {
                    self.flush();
                    self.lists.pop();
                    if self.lists.is_empty() {
                        self.blank_pending = true;
                    }
                }
                TagEnd::Item => {
                    self.flush();
                    self.prefixes.pop();
                }
                TagEnd::TableCell => {
                    let cell = std::mem::take(&mut self.inline);
                    if let Some(row) = self.table.as_mut().and_then(|table| table.rows.last_mut()) {
                        row.push(cell);
                    }
                }
                TagEnd::TableHead => {
                    if let Some(table) = self.table.as_mut() {
                        table.in_head = false;
                        table.header_rows = table.rows.len();
                    }
                }
                TagEnd::Table => {
                    if let Some(table) = self.table.take() {
                        self.render_table(table);
                    }
                    self.blank_pending = true;
                }
                TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => {
                    self.styles.pop();
                }
                TagEnd::Link => {
                    self.styles.pop();
                    if let Some((start, url)) = self.link_start.pop() {
                        let label: String = self.inline[start..]
                            .iter()
                            .map(|(text, _)| text.as_str())
                            .collect();
                        let same = label.trim() == url
                            || url.trim_start_matches("mailto:") == label.trim()
                            || url.starts_with('#');
                        if !same && !url.is_empty() {
                            self.inline.push((format!(" ({url})"), t.fg(Role::Dim)));
                        }
                    }
                }
                TagEnd::Image => {
                    self.styles.pop();
                    self.link_start.pop();
                    self.inline.push(("]".into(), t.fg(Role::Dim)));
                }
                TagEnd::FootnoteDefinition => {
                    self.flush();
                    self.blank_pending = true;
                }
                _ => {}
            },
            Event::Text(text) => {
                if let Some(code) = self.code.as_mut() {
                    code.text.push_str(&text);
                } else {
                    let style = self.style();
                    self.inline.push((text.to_string(), style));
                }
            }
            Event::Code(text) => {
                let role = if text.contains('(') { Role::Warn } else { Role::Link };
                self.inline.push((text.to_string(), t.fg(role)));
            }
            Event::InlineMath(text) | Event::DisplayMath(text) => {
                self.inline.push((text.to_string(), t.fg(Role::Code)));
            }
            Event::Html(html) | Event::InlineHtml(html) => {
                let text = html.trim_end_matches('\n').to_string();
                if !text.is_empty() {
                    self.inline.push((text, t.fg(Role::Dim)));
                }
            }
            Event::FootnoteReference(label) => {
                self.inline.push((format!("[^{label}]"), t.fg(Role::Link)));
            }
            Event::SoftBreak => self.inline.push((" ".into(), self.style())),
            Event::HardBreak => self.inline.push(("\n".into(), self.style())),
            Event::Rule => {
                self.flush();
                let available = self.available();
                let rule = theme().glyphs().horizontal.repeat(available.min(60));
                self.emit(vec![Span::styled(rule, t.fg(Role::Dim))]);
                self.blank_pending = true;
            }
            Event::TaskListMarker(checked) => {
                let (glyph, role) = if checked { ("■ ", Role::Accent) } else { ("□ ", Role::Dim) };
                self.inline.push((glyph.into(), t.fg(role)));
            }
        }
    }


    /// Columns used by the current indentation and prefixes.
    fn prefix_width(&self) -> usize {
        self.options.indent
            + self
                .prefixes
                .iter()
                .map(|prefix| match prefix {
                    Prefix::Quote => 2,
                    Prefix::Item { lead, width, .. } => lead + width,
                })
                .sum::<usize>()
    }

    fn available(&self) -> usize {
        self.options.width.saturating_sub(self.prefix_width()).max(8)
    }

    fn available_wide(&self) -> usize {
        self.options.wide.max(self.options.width).saturating_sub(self.prefix_width()).max(8)
    }

    /// Build this row's prefix. The first row of a list item shows its marker.
    fn take_prefix(&mut self) -> Vec<Span<'static>> {
        let dim = theme().fg(Role::Dim);
        let mut spans = vec![Span::raw(" ".repeat(self.options.indent))];
        for prefix in &mut self.prefixes {
            match prefix {
                Prefix::Quote => spans.push(Span::styled("│ ", dim)),
                Prefix::Item {
                    lead,
                    marker,
                    width: item_width,
                } => {
                    spans.push(Span::raw(" ".repeat(*lead)));
                    match marker.take() {
                        Some((text, style)) => {
                            let gap = item_width.saturating_sub(width(&text));
                            spans.push(Span::styled(text, style));
                            spans.push(Span::raw(" ".repeat(gap)));
                        }
                        None => spans.push(Span::raw(" ".repeat(*item_width))),
                    }
                }
            }
        }
        spans
    }

    fn emit(&mut self, content: Vec<Span<'static>>) {
        if self.blank_pending && !self.out.is_empty() {
            self.out.push(Line::default());
        }
        self.blank_pending = false;
        let mut spans = self.take_prefix();
        spans.extend(content);
        self.out.push(Line::from(spans));
    }

    fn flush(&mut self) {
        if self.inline.is_empty() || self.table.is_some() {
            return;
        }
        let segments = cite(std::mem::take(&mut self.inline));
        if segments.iter().all(|(text, _)| text.trim().is_empty()) {
            return;
        }
        let available = self.available();
        for row in wrap_segments(&segments, available) {
            self.emit(row);
        }
    }

    fn render_code(&mut self, code: &CodeBlock) {
        let t = theme();
        let dim = t.fg(Role::Dim);
        let text = code.text.strip_suffix('\n').unwrap_or(&code.text);
        let lines: Vec<&str> = text.split('\n').collect();
        let number_width = lines.len().to_string().len();
        let top_level = self.prefixes.is_empty();
        let lead = if top_level { self.options.block_indent } else { 0 };
        let gutter = number_width + 3;
        let available = self.available_wide().saturating_sub(lead + gutter).max(8);
        let mut highlighter = CodeHighlighter::new(&code.language);
        let bar = t.glyphs().vertical;
        for (index, line) in lines.iter().enumerate() {
            let line = line.replace('\t', "    ");
            let spans = highlighter.line(&line);
            let chunks = split_spans(spans, available);
            for (chunk_index, chunk) in chunks.into_iter().enumerate() {
                let number = if chunk_index == 0 {
                    format!("{:>number_width$} {bar} ", index + 1)
                } else {
                    format!("{:>number_width$} {bar} ", "")
                };
                let mut row = vec![Span::raw(" ".repeat(lead)), Span::styled(number, dim)];
                row.extend(chunk);
                self.emit(row);
            }
        }
    }

    fn render_table(&mut self, table: TableState) {
        let t = theme();
        let columns = table.rows.iter().map(Vec::len).max().unwrap_or(0);
        if columns == 0 {
            return;
        }
        let plain = |cell: &Vec<(String, Style)>| -> String {
            cell.iter().map(|(text, _)| text.as_str()).collect::<String>()
        };
        let mut widths = vec![1usize; columns];
        for row in &table.rows {
            for (index, cell) in row.iter().enumerate() {
                widths[index] = widths[index].max(width(plain(cell).trim()));
            }
        }
        let available = self.available_wide().saturating_sub(self.options.block_indent);
        let separators = 3 * (columns - 1);
        while widths.iter().sum::<usize>() + separators > available {
            let Some((widest, _)) = widths.iter().enumerate().max_by_key(|(_, w)| **w) else {
                break;
            };
            if widths[widest] <= 4 {
                break;
            }
            widths[widest] -= 1;
        }
        let dim = t.fg(Role::Dim);
        let bar = format!(" {} ", t.glyphs().vertical);
        let lead = " ".repeat(self.options.block_indent);
        for (row_index, row) in table.rows.iter().enumerate() {
            let header = row_index < table.header_rows;
            let mut spans = vec![Span::raw(lead.clone())];
            for column in 0..columns {
                if column > 0 {
                    spans.push(Span::styled(bar.clone(), dim));
                }
                let empty = Vec::new();
                let cell = row.get(column).unwrap_or(&empty);
                let text = super::text::truncate(plain(cell).trim(), widths[column]);
                let gap = widths[column].saturating_sub(width(&text));
                let (left, right) = match table.alignments.get(column) {
                    Some(Alignment::Right) => (gap, 0),
                    Some(Alignment::Center) => (gap / 2, gap - gap / 2),
                    _ => (0, gap),
                };
                let style = if header {
                    t.bold(Role::Strong)
                } else {
                    cell.iter()
                        .find(|(text, _)| !text.trim().is_empty())
                        .map(|(_, style)| *style)
                        .unwrap_or_else(|| t.fg(Role::Text))
                };
                spans.push(Span::raw(" ".repeat(left)));
                spans.push(Span::styled(text, style));
                spans.push(Span::raw(" ".repeat(right)));
            }
            self.emit(spans);
            if header && row_index + 1 == table.header_rows {
                let h = t.glyphs().horizontal;
                let cross = if t.ascii { "-+-" } else { "─┼─" };
                let rule = widths
                    .iter()
                    .map(|w| h.repeat(*w))
                    .collect::<Vec<_>>()
                    .join(cross);
                self.emit(vec![Span::raw(lead.clone()), Span::styled(rule, dim)]);
            }
        }
    }

    fn finish(mut self) -> Vec<Line<'static>> {
        if let Some(code) = self.code.take() {
            self.render_code(&code);
        }
        if let Some(table) = self.table.take() {
            self.render_table(table);
        }
        self.flush();
        while self.out.last().is_some_and(|line| line.spans.iter().all(|s| s.content.trim().is_empty())) {
            self.out.pop();
        }
        self.out
    }
}

/// Merge neighbouring pieces of the same style, then pick out `[1]`-style
/// citations in the link colour. Merging first matters: the parser hands
/// over `[`, `1` and `]` as separate pieces because they look like a link.
fn cite(segments: Vec<(String, Style)>) -> Vec<(String, Style)> {
    let t = theme();
    let link = t.fg(Role::Link);
    let code = [t.fg(Role::Link), t.fg(Role::Warn)];
    let mut merged: Vec<(String, Style)> = Vec::new();
    for (text, style) in segments {
        match merged.last_mut() {
            Some((last, last_style)) if *last_style == style && text != "\n" && last != "\n" => last.push_str(&text),
            _ => merged.push((text, style)),
        }
    }
    let mut out = Vec::new();
    for (text, style) in merged {
        if code.contains(&style) || !text.contains('[') {
            out.push((text, style));
            continue;
        }
        let bytes = text.as_bytes();
        let (mut last, mut index) = (0, 0);
        while index < bytes.len() {
            if bytes[index] == b'[' {
                let digits = bytes[index + 1..].iter().take_while(|b| b.is_ascii_digit()).count();
                let close = index + 1 + digits;
                if (1..4).contains(&digits) && bytes.get(close) == Some(&b']') {
                    if last < index {
                        out.push((text[last..index].to_string(), style));
                    }
                    out.push((text[index..=close].to_string(), link));
                    index = close + 1;
                    last = index;
                    continue;
                }
            }
            index += 1;
        }
        if last < text.len() {
            out.push((text[last..].to_string(), style));
        }
    }
    out
}

/// Greedy word wrap over styled segments. `\n` segments force a break.
pub fn wrap_segments(segments: &[(String, Style)], width_limit: usize) -> Vec<Vec<Span<'static>>> {
    let width_limit = width_limit.max(1);
    // Words are runs of non-space characters; each keeps the styles of its pieces.
    struct Word {
        pieces: Vec<(String, Style)>,
        width: usize,
        space_before: bool,
        hard_break: bool,
    }
    let mut words: Vec<Word> = Vec::new();
    let mut current: Option<Word> = None;
    let mut space = false;
    for (text, style) in segments {
        if text == "\n" {
            if let Some(word) = current.take() {
                words.push(word);
            }
            words.push(Word {
                pieces: Vec::new(),
                width: 0,
                space_before: false,
                hard_break: true,
            });
            space = false;
            continue;
        }
        for grapheme in text.graphemes(true) {
            if grapheme.chars().all(char::is_whitespace) {
                if let Some(word) = current.take() {
                    words.push(word);
                }
                space = true;
                continue;
            }
            let word = current.get_or_insert_with(|| Word {
                pieces: Vec::new(),
                width: 0,
                space_before: std::mem::take(&mut space),
                hard_break: false,
            });
            match word.pieces.last_mut() {
                Some((piece, piece_style)) if piece_style == style => piece.push_str(grapheme),
                _ => word.pieces.push((grapheme.to_string(), *style)),
            }
            word.width += width(grapheme);
        }
    }
    if let Some(word) = current.take() {
        words.push(word);
    }

    let mut rows: Vec<Vec<Span<'static>>> = Vec::new();
    let mut row: Vec<Span<'static>> = Vec::new();
    let mut used = 0;
    let push_pieces = |row: &mut Vec<Span<'static>>, pieces: &[(String, Style)]| {
        for (text, style) in pieces {
            match row.last_mut() {
                Some(last) if last.style == *style => last.content.to_mut().push_str(text),
                _ => row.push(Span::styled(text.clone(), *style)),
            }
        }
    };
    for word in words {
        if word.hard_break {
            rows.push(std::mem::take(&mut row));
            used = 0;
            continue;
        }
        let gap = usize::from(word.space_before && used > 0);
        if used + gap + word.width <= width_limit {
            if gap == 1 {
                push_pieces(&mut row, &[(" ".to_string(), word.pieces[0].1)]);
            }
            push_pieces(&mut row, &word.pieces);
            used += gap + word.width;
            continue;
        }
        if word.width <= width_limit {
            rows.push(std::mem::take(&mut row));
            push_pieces(&mut row, &word.pieces);
            used = word.width;
            continue;
        }
        // A word longer than the row: break it by characters.
        if used > 0 {
            if used + gap < width_limit {
                if gap == 1 {
                    push_pieces(&mut row, &[(" ".to_string(), word.pieces[0].1)]);
                    used += 1;
                }
            } else {
                rows.push(std::mem::take(&mut row));
                used = 0;
            }
        }
        for (text, style) in &word.pieces {
            for grapheme in text.graphemes(true) {
                let w = width(grapheme);
                if used + w > width_limit {
                    rows.push(std::mem::take(&mut row));
                    used = 0;
                }
                push_pieces(&mut row, &[(grapheme.to_string(), *style)]);
                used += w;
            }
        }
    }
    if !row.is_empty() || rows.is_empty() {
        rows.push(row);
    }
    rows
}

/// Split already-styled spans into rows of at most `width_limit` columns.
pub fn split_spans(spans: Vec<Span<'static>>, width_limit: usize) -> Vec<Vec<Span<'static>>> {
    let width_limit = width_limit.max(1);
    let mut rows = vec![Vec::new()];
    let mut used = 0;
    for span in spans {
        let mut piece = String::new();
        for grapheme in span.content.graphemes(true) {
            let w = width(grapheme);
            if used + w > width_limit {
                if !piece.is_empty() {
                    rows.last_mut().unwrap().push(Span::styled(std::mem::take(&mut piece), span.style));
                }
                rows.push(Vec::new());
                used = 0;
            }
            piece.push_str(grapheme);
            used += w;
        }
        if !piece.is_empty() {
            rows.last_mut().unwrap().push(Span::styled(piece, span.style));
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(lines: &[Line<'static>]) -> Vec<String> {
        lines.iter().map(|line| line.to_string()).collect()
    }

    const TUI: MdOptions = MdOptions {
        width: 90,
        indent: 3,
        block_indent: 0,
        wide: 90,
    };

    #[test]
    fn paragraphs_and_ordered_lists_match_the_chat_mockup() {
        let text = "Two releases landed this month.\n\n1. Networking now uses payload records. Register each payload once, then send it from the server or the client. [1]\n2. Two render hooks are deprecated in favour of the new render events. [2]\n\nFor your QEMU mod, check the payload.";
        let rows = plain(&render(text, TUI));
        assert_eq!(
            rows,
            vec![
                "   Two releases landed this month.",
                "",
                "   1. Networking now uses payload records. Register each payload once, then send it from",
                "      the server or the client. [1]",
                "   2. Two render hooks are deprecated in favour of the new render events. [2]",
                "",
                "   For your QEMU mod, check the payload.",
            ]
        );
    }

    #[test]
    fn citations_use_the_link_colour() {
        let pieces = vec![
            ("See ".to_string(), Style::default()),
            ("[".to_string(), Style::default()),
            ("1".to_string(), Style::default()),
            ("] and [22], not [x].".to_string(), Style::default()),
        ];
        let link = theme().fg(Role::Link);
        let segments = cite(pieces);
        let cited: Vec<&str> = segments
            .iter()
            .filter(|(_, style)| *style == link)
            .map(|(text, _)| text.as_str())
            .collect();
        assert_eq!(cited, vec!["[1]", "[22]"]);
    }

    #[test]
    fn code_blocks_get_a_numbered_gutter() {
        let text = "```rust\ndrop(old.monitor);\nlet vm = Vm::spawn(&cfg)?;\n```";
        let rows = plain(&render(
            text,
            MdOptions {
                width: 80,
                indent: 0,
                block_indent: 2,
                wide: 80,
            },
        ));
        assert_eq!(
            rows,
            vec!["  1 │ drop(old.monitor);", "  2 │ let vm = Vm::spawn(&cfg)?;"]
        );
    }

    #[test]
    fn nested_bullets_and_tables_render_inside_the_width() {
        let text = "- one\n  - two\n\n| a | b |\n|---|--:|\n| x | 10 |";
        let rows = plain(&render(text, TUI));
        assert_eq!(rows[0], "   • one");
        assert_eq!(rows[1], "     ◦ two");
        assert!(rows.iter().any(|row| row.contains("a │  b")));
        assert!(rows.iter().any(|row| row.contains("x │ 10")));
    }

    #[test]
    fn long_words_break_instead_of_overflowing() {
        let rows = wrap_segments(&[("a".repeat(25), Style::default())], 10);
        assert_eq!(rows.len(), 3);
    }

    #[test]
    fn unfinished_code_fence_still_renders_while_streaming() {
        let rows = plain(&render("Here:\n\n```py\nprint(1)", TUI));
        assert_eq!(rows.last().unwrap(), "   1 │ print(1)");
    }
}
