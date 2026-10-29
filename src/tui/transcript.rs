//! Chat history → terminal rows, following the chat artboard:
//!
//! ```text
//!  › what changed in the Fabric API this month? check the web
//!
//!  ● web_search "fabric api changes october 2026"
//!    └ 8 results · read 6 pages · 1.8s
//!
//!  ● Two releases landed this month.
//!
//!    Sources  [1] fabricmc.net · …   [2] github.com · …
//!    412 tokens · 182 tok/s · 2.3s
//! ```

use crate::engine::split_reasoning;
use crate::models::Message;
use crate::ui::markdown::{self, MdOptions, wrap_segments};
use crate::ui::picture::{self, Picture};
use crate::ui::text::{self, width};
use crate::ui::theme::{Role, theme};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use serde_json::Value;

/// Prose in the design's 112-column window is 90 columns wide; wider
/// windows keep that share of the width.
pub const MAX_TEXT_WIDTH: usize = 90;

/// How wide prose runs in a window `width` columns wide.
pub fn prose_width(width: usize) -> usize {
    MAX_TEXT_WIDTH.max(width * MAX_TEXT_WIDTH / 112)
}
/// Under 100 columns prose wraps sooner, as in the small-terminal design.
const NARROW_TEXT_WIDTH: usize = 60;

#[derive(Clone, Copy)]
pub struct Layout {
    pub width: usize,
    pub narrow: bool,
    pub show_reasoning: bool,
    pub show_stats: bool,
    /// Draw small previews of pictures in the chat.
    pub pictures: bool,
}

impl Layout {
    fn text_width(&self) -> usize {
        let max = if self.narrow { NARROW_TEXT_WIDTH } else { prose_width(self.width) };
        self.width.saturating_sub(1).min(max)
    }
}

pub fn user_lines(message: &Message, layout: Layout) -> Vec<Line<'static>> {
    let t = theme();
    let mut out = prefixed(
        " › ",
        t.fg(Role::Link),
        &[(message.content.trim_end().to_string(), t.fg(Role::Strong))],
        layout.text_width(),
    );
    if let Some(items) = message.attachments.as_array() {
        for item in items {
            let name = item.get("name").and_then(Value::as_str).unwrap_or("file");
            let meta = item.get("meta").and_then(Value::as_str).unwrap_or("");
            let mut text = format!("   + {name}");
            if !meta.is_empty() {
                text.push_str(&format!(" · {meta}"));
            }
            out.push(Line::styled(text::truncate(&text, layout.width), t.fg(Role::Dim)));
            if let Some((rows, _)) = picture_rows(item, layout, 24, 8) {
                out.extend(rows);
            }
        }
    }
    out
}

/// A small preview of an attached or saved picture, indented under the
/// message, and its description ("1024×1024 · PNG · 1.4 MB").
fn picture_rows(item: &Value, layout: Layout, max_cols: u16, max_rows: u16) -> Option<(Vec<Line<'static>>, String)> {
    if !layout.pictures || item.get("kind").and_then(Value::as_str) != Some("image") {
        return None;
    }
    let uri = item.get("uri").and_then(Value::as_str)?;
    let picture = match uri.split_once("base64,") {
        Some((_, data)) => {
            let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, data.trim()).ok()?;
            Picture::decode(bytes).ok()?
        }
        None if uri.starts_with("http://") || uri.starts_with("https://") => return None,
        None => Picture::open(std::path::Path::new(uri)).ok()?,
    };
    let (w, h) = image::GenericImageView::dimensions(&picture.image);
    let max_cols = max_cols.min(layout.text_width().saturating_sub(3) as u16);
    let (cols, rows) = picture::fit(w, h, max_cols, max_rows);
    let lines = picture::chat_cells(&picture.image, cols, rows)?;
    let indented = lines
        .into_iter()
        .map(|line| {
            let mut spans = vec![Span::raw("   ")];
            spans.extend(line.spans);
            Line::from(spans)
        })
        .collect();
    Some((indented, picture.describe()))
}

/// `marker` on the first row, body wrapped and indented under it.
pub fn prefixed(
    marker: &str,
    marker_style: Style,
    segments: &[(String, Style)],
    text_width: usize,
) -> Vec<Line<'static>> {
    let indent = width(marker);
    let rows = wrap_segments(segments, text_width.saturating_sub(indent).max(8));
    rows.into_iter()
        .enumerate()
        .map(|(index, row)| {
            let mut spans = if index == 0 {
                vec![Span::styled(marker.to_string(), marker_style)]
            } else {
                vec![Span::raw(" ".repeat(indent))]
            };
            spans.extend(row);
            Line::from(spans)
        })
        .collect()
}

/// A saved assistant reply.
pub fn assistant_lines(message: &Message, layout: Layout) -> Vec<Line<'static>> {
    let t = theme();
    let (reasoning, answer) = split_reasoning(&message.content);
    let answer = crate::genui::parse_reply(&answer, None).text;
    let mut out = Vec::new();
    let logs: Vec<&Value> = message
        .tool_calls
        .as_array()
        .map(|logs| logs.iter().filter(|log| log.get("name").is_some()).collect())
        .unwrap_or_default();
    for log in &logs {
        out.extend(tool_lines(log, layout));
    }
    let has_body = !reasoning.is_empty() || !answer.trim().is_empty();
    if !logs.is_empty() && has_body {
        out.push(Line::default());
    }
    if !reasoning.is_empty() {
        out.extend(reasoning_lines(&reasoning, "Thought", layout));
    }
    if !answer.trim().is_empty() {
        out.extend(answer_lines(&answer, layout));
    } else if logs.is_empty() && reasoning.is_empty() && message.attachments.as_array().is_none_or(Vec::is_empty) {
        out.push(Line::from(vec![
            Span::styled(" ● ", t.fg(Role::Accent)),
            Span::styled("(empty reply)", t.fg(Role::Dim)),
        ]));
    }
    if message.ui_spec.is_some() {
        out.push(Line::styled(
            "   ▣ This reply includes generated UI · open it in the app",
            t.fg(Role::Dim),
        ));
    }
    if let Some(items) = message.attachments.as_array() {
        for item in items {
            let path = item
                .get("uri")
                .or_else(|| item.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("image");
            let shown = if path.starts_with("data:") {
                item.get("name").and_then(Value::as_str).unwrap_or("image").to_string()
            } else {
                text::home_relative(std::path::Path::new(path))
            };
            if !out.is_empty() {
                out.push(Line::default());
            }
            out.push(Line::from(vec![
                Span::styled(" ● ", t.fg(Role::Accent)),
                Span::styled("Saved ", t.fg(Role::Text)),
                Span::styled(shown, t.fg(Role::Link)),
            ]));
            match picture_rows(item, layout, 28, 12) {
                Some((rows, about)) => {
                    out.extend(rows);
                    out.push(Line::styled(format!("   {about}"), t.fg(Role::Dim)));
                }
                None if !path.starts_with("data:") && !std::path::Path::new(path).exists() => {
                    out.push(Line::styled("   └ the file has been moved or deleted", t.fg(Role::Dim)));
                }
                None => {}
            }
        }
    }
    let citations = message.citations.as_array().cloned().unwrap_or_default();
    let stats = (layout.show_stats && !layout.narrow)
        .then(|| stats_line(message.tokens, message.ms))
        .flatten();
    if !citations.is_empty() || stats.is_some() {
        out.push(Line::default());
    }
    if !citations.is_empty() {
        out.extend(sources_lines(&citations, layout));
    }
    if let Some(stats) = stats {
        out.push(stats);
    }
    out
}

/// The answer as Markdown, with ` ● ` on its first row.
pub fn answer_lines(answer: &str, layout: Layout) -> Vec<Line<'static>> {
    let mut lines = markdown::render(
        answer,
        MdOptions {
            width: layout.text_width(),
            indent: 3,
            block_indent: 0,
            wide: layout.width.saturating_sub(1),
        },
    );
    mark_first(&mut lines, " ● ", theme().fg(Role::Accent));
    lines
}

/// Replace the three leading spaces of the first row with `marker`.
pub fn mark_first(lines: &mut [Line<'static>], marker: &str, style: Style) {
    let Some(first) = lines.first_mut() else {
        return;
    };
    if let Some(span) = first.spans.first_mut()
        && span.content.starts_with("   ") {
            let rest = span.content[3..].to_string();
            span.content = rest.into();
        }
    first.spans.insert(0, Span::styled(marker.to_string(), style));
}

pub fn reasoning_lines(reasoning: &str, label: &str, layout: Layout) -> Vec<Line<'static>> {
    let t = theme();
    let key = super::keys::label(super::keys::Action::ToggleReasoning);
    let hint = if layout.show_reasoning {
        format!("{key} to hide")
    } else {
        format!("{key} to show")
    };
    let mut out = vec![Line::from(vec![
        Span::styled(" ◇ ", t.fg(Role::Code)),
        Span::styled(format!("{label} · {hint}"), t.fg(Role::Dim)),
    ])];
    if layout.show_reasoning {
        let segments = [(reasoning.trim().to_string(), t.fg(Role::Code))];
        for paragraph in segments[0].0.split("\n\n") {
            let rows = wrap_segments(
                &[(paragraph.replace('\n', " "), t.fg(Role::Code))],
                layout.text_width().saturating_sub(3),
            );
            for row in rows {
                let mut spans = vec![Span::raw("   ")];
                spans.extend(row);
                out.push(Line::from(spans));
            }
        }
        out.push(Line::default());
    }
    out
}

/// ` ● web_search "query"` and `   └ 8 results · read 6 pages · 1.8s`.
pub fn tool_lines(log: &Value, layout: Layout) -> Vec<Line<'static>> {
    let t = theme();
    let name = log
        .get("label")
        .or_else(|| log.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("tool")
        .to_string();
    let detail = log
        .get("detail")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| crate::engine::argument_summary(log.get("args").unwrap_or(&Value::Null)));
    let (summary, failed) = tool_summary(log);
    let summary_style = if failed { t.fg(Role::Error) } else { t.fg(Role::Dim) };
    if layout.narrow {
        return vec![text::clip_line(
            Line::from(vec![
                Span::styled(" ● ", t.fg(Role::Accent)),
                Span::styled(name, t.fg(Role::Dim)),
                Span::styled(format!(" · {summary}"), summary_style),
            ]),
            layout.width,
        )];
    }
    let mut first = vec![
        Span::styled(" ● ", t.fg(Role::Accent)),
        Span::styled(name, t.fg(Role::Dim)),
    ];
    if !detail.is_empty() {
        first.push(Span::raw(" "));
        first.push(Span::styled(detail, t.fg(Role::Text)));
    }
    vec![
        text::clip_line(Line::from(first), layout.width),
        text::clip_line(
            Line::from(vec![Span::raw("   "), Span::styled(format!("└ {summary}"), summary_style)]),
            layout.width,
        ),
    ]
}

pub fn running_tool_lines(name: &str, detail: &str, elapsed_ms: u64, waiting: bool, layout: Layout) -> Vec<Line<'static>> {
    let t = theme();
    let summary = if waiting {
        "waiting for your answer".to_string()
    } else {
        format!("running… {}", text::seconds(elapsed_ms))
    };
    if layout.narrow {
        return vec![Line::from(vec![
            Span::styled(" ● ", t.fg(Role::Warn)),
            Span::styled(format!("{name} · {summary}"), t.fg(Role::Dim)),
        ])];
    }
    let mut first = vec![
        Span::styled(" ● ", t.fg(Role::Warn)),
        Span::styled(name.to_string(), t.fg(Role::Dim)),
    ];
    if !detail.is_empty() {
        first.push(Span::raw(" "));
        first.push(Span::styled(detail.to_string(), t.fg(Role::Text)));
    }
    vec![
        text::clip_line(Line::from(first), layout.width),
        Line::styled(format!("   └ {summary}"), t.fg(Role::Dim)),
    ]
}

fn tool_summary(log: &Value) -> (String, bool) {
    let status = log.get("status").and_then(Value::as_str).unwrap_or("done");
    let result = log.get("result").and_then(Value::as_str).unwrap_or("");
    let elapsed = log
        .get("elapsedMs")
        .and_then(Value::as_u64)
        .map(text::duration);
    let with_time = |text: String| match &elapsed {
        Some(elapsed) if !text.is_empty() => format!("{text} · {elapsed}"),
        Some(elapsed) => elapsed.clone(),
        None => text,
    };
    match status {
        "error" | "unavailable" => {
            let first = result.lines().next().unwrap_or("failed");
            (format!("✗ {}", text::truncate(first, 90)), true)
        }
        "rejected" => ("denied".into(), false),
        _ if log.get("resultCount").is_some() => {
            let count = log["resultCount"].as_u64().unwrap_or(0) as usize;
            let pages = log.get("pagesRead").and_then(Value::as_u64).unwrap_or(0) as usize;
            let mut text = text::plural(count, "result", "results");
            if pages > 0 {
                text.push_str(&format!(" · read {}", text::plural(pages, "page", "pages")));
            }
            (with_time(text), false)
        }
        _ => {
            let first = result
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .unwrap_or("done");
            (with_time(text::truncate(first, 60)), false)
        }
    }
}

/// `   Sources  [1] host · title   [2] host · title`, wrapped under itself.
pub fn sources_lines(citations: &[Value], layout: Layout) -> Vec<Line<'static>> {
    let dim = theme().fg(Role::Dim);
    let entries: Vec<String> = citations
        .iter()
        .enumerate()
        .map(|(index, citation)| {
            let url = citation.get("url").and_then(Value::as_str).unwrap_or("");
            let host = text::host(url);
            let title = citation
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            if layout.narrow || title.is_empty() {
                format!("[{}] {host}", index + 1)
            } else {
                format!("[{}] {host} · {}", index + 1, text::truncate(&title, 32))
            }
        })
        .collect();
    let (lead, follow) = if layout.narrow {
        ("   ".to_string(), "   ".to_string())
    } else {
        ("   Sources  ".to_string(), " ".repeat(12))
    };
    let limit = layout.width.saturating_sub(1).max(20);
    let mut out = Vec::new();
    let mut row = lead;
    let mut row_has_entry = false;
    for entry in entries {
        let candidate_width = width(&row) + if row_has_entry { 3 } else { 0 } + width(&entry);
        if row_has_entry && candidate_width > limit {
            out.push(Line::styled(std::mem::replace(&mut row, follow.clone()), dim));
            row_has_entry = false;
        }
        if row_has_entry {
            row.push_str("   ");
        }
        row.push_str(&text::truncate(&entry, limit.saturating_sub(12)));
        row_has_entry = true;
    }
    if row_has_entry {
        out.push(Line::styled(row, dim));
    }
    out
}

pub fn stats_line(tokens: Option<u64>, ms: Option<u64>) -> Option<Line<'static>> {
    let mut parts = Vec::new();
    if let Some(tokens) = tokens.filter(|tokens| *tokens > 0) {
        parts.push(format!("{} tokens", tokens));
        if let Some(ms) = ms.filter(|ms| *ms > 0) {
            parts.push(format!("{:.0} tok/s", tokens as f64 / (ms as f64 / 1000.0)));
        }
    }
    if let Some(ms) = ms.filter(|ms| *ms > 0) {
        parts.push(text::duration(ms));
    }
    (!parts.is_empty()).then(|| Line::styled(format!("   {}", parts.join(" · ")), theme().fg(Role::Dim)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn layout() -> Layout {
        Layout {
            width: 112,
            narrow: false,
            show_reasoning: false,
            show_stats: true,
            pictures: false,
        }
    }

    fn message(role: &str, content: &str) -> Message {
        Message {
            id: "m".into(),
            chat_id: "c".into(),
            role: role.into(),
            content: content.into(),
            attachments: json!([]),
            tool_calls: json!([]),
            citations: json!([]),
            ui_spec: None,
            tokens: None,
            ms: None,
            created_at: 0,
        }
    }

    #[test]
    fn user_message_matches_the_mockup() {
        let lines = user_lines(&message("user", "what changed in the Fabric API this month? check the web"), layout());
        assert_eq!(lines[0].to_string(), " › what changed in the Fabric API this month? check the web");
    }

    #[test]
    fn assistant_reply_with_tool_sources_and_stats() {
        let mut reply = message("assistant", "Two releases landed this month.");
        reply.tool_calls = json!([{
            "name": "web_search", "args": {"query": "fabric api changes october 2026"},
            "status": "done", "resultCount": 8, "pagesRead": 6, "elapsedMs": 1800, "result": "…"
        }]);
        reply.citations = json!([
            {"url": "https://fabricmc.net/x", "title": "[release notes title]"},
            {"url": "https://github.com/y", "title": "[changelog title]"}
        ]);
        reply.tokens = Some(412);
        reply.ms = Some(2300);
        let rows: Vec<String> = assistant_lines(&reply, layout()).iter().map(|l| l.to_string()).collect();
        assert_eq!(
            rows,
            vec![
                " ● web_search \"fabric api changes october 2026\"",
                "   └ 8 results · read 6 pages · 1.8s",
                "",
                " ● Two releases landed this month.",
                "",
                "   Sources  [1] fabricmc.net · [release notes title]   [2] github.com · [changelog title]",
                "   412 tokens · 179 tok/s · 2.3s",
            ]
        );
    }

    #[test]
    fn narrow_layout_compacts_tools_and_sources() {
        let narrow = Layout {
            width: 80,
            narrow: true,
            ..layout()
        };
        let log = json!({"name": "web_search", "args": {"query": "q"}, "status": "done", "resultCount": 8, "elapsedMs": 1800});
        assert_eq!(tool_lines(&log, narrow)[0].to_string(), " ● web_search · 8 results · 1.8s");
        let sources = sources_lines(&[json!({"url": "https://fabricmc.net/a"}), json!({"url": "https://github.com/b"})], narrow);
        assert_eq!(sources[0].to_string(), "   [1] fabricmc.net   [2] github.com");
    }
}
