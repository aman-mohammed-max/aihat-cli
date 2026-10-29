//! Width-aware text helpers shared by the TUI and the command line output.

use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub fn width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

pub fn line_width(line: &Line<'_>) -> usize {
    line.spans.iter().map(|span| width(&span.content)).sum()
}

/// Cut `text` to at most `max` columns, ending in `…` when anything was cut.
pub fn truncate(text: &str, max: usize) -> String {
    if width(text) <= max {
        return text.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for grapheme in text.graphemes(true) {
        let w = width(grapheme);
        if used + w > max - 1 {
            break;
        }
        out.push_str(grapheme);
        used += w;
    }
    out.push('…');
    out
}

/// Cut from the start: `…/projects/fabric-mod`.
pub fn truncate_left(text: &str, max: usize) -> String {
    if width(text) <= max {
        return text.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut kept: Vec<&str> = Vec::new();
    let mut used = 1;
    for grapheme in text.graphemes(true).rev() {
        let w = width(grapheme);
        if used + w > max {
            break;
        }
        kept.push(grapheme);
        used += w;
    }
    kept.reverse();
    format!("…{}", kept.concat())
}

/// Truncate, then pad with spaces to exactly `cols` columns.
pub fn pad(text: &str, cols: usize) -> String {
    let mut out = truncate(text, cols);
    let used = width(&out);
    out.extend(std::iter::repeat_n(' ', cols.saturating_sub(used)));
    out
}

/// Right-align `text` in `cols` columns.
pub fn pad_left(text: &str, cols: usize) -> String {
    let text = truncate(text, cols);
    format!("{}{}", " ".repeat(cols.saturating_sub(width(&text))), text)
}

/// Keep the first `max` columns of a styled line.
pub fn clip_line(line: Line<'static>, max: usize) -> Line<'static> {
    let mut used = 0;
    let mut spans = Vec::new();
    let line_style = line.style;
    for span in line.spans {
        let span = Span::styled(span.content, line_style.patch(span.style));
        let w = width(&span.content);
        if used + w <= max {
            used += w;
            spans.push(span);
            continue;
        }
        let mut cut = String::new();
        for grapheme in span.content.graphemes(true) {
            let gw = width(grapheme);
            if used + gw > max {
                break;
            }
            cut.push_str(grapheme);
            used += gw;
        }
        if !cut.is_empty() {
            spans.push(Span::styled(cut, span.style));
        }
        break;
    }
    Line::from(spans)
}

/// Left text, padding, right text: one row of exactly `cols` columns.
/// The left side is truncated first so the right side stays readable.
pub fn spread(
    left: Vec<Span<'static>>,
    right: Vec<Span<'static>>,
    cols: usize,
) -> Line<'static> {
    let right_width: usize = right.iter().map(|span| width(&span.content)).sum();
    let room = cols.saturating_sub(right_width + if right_width > 0 { 1 } else { 0 });
    let left = clip_line(Line::from(left), room);
    let gap = cols.saturating_sub(line_width(&left) + right_width);
    let mut spans = left.spans;
    spans.push(Span::raw(" ".repeat(gap)));
    spans.extend(right);
    clip_line(Line::from(spans), cols)
}

/// "2 min ago", "1 hour ago", "yesterday", "3 days ago", "2 weeks ago", "last month".
pub fn relative_time(timestamp_ms: i64) -> String {
    let now = chrono::Utc::now().timestamp_millis();
    let seconds = (now - timestamp_ms).max(0) / 1000;
    let minutes = seconds / 60;
    let hours = minutes / 60;
    let days = hours / 24;
    let plural = |n: i64, unit: &str| {
        if n == 1 {
            format!("1 {unit} ago")
        } else {
            format!("{n} {unit}s ago")
        }
    };
    match () {
        _ if seconds < 60 => "just now".into(),
        _ if minutes < 60 => format!("{minutes} min ago"),
        _ if hours < 24 => plural(hours, "hour"),
        _ if days < 2 => "yesterday".into(),
        _ if days < 7 => format!("{days} days ago"),
        _ if days < 30 => plural(days / 7, "week"),
        _ if days < 60 => "last month".into(),
        _ if days < 365 => format!("{} months ago", days / 30),
        _ if days < 730 => "last year".into(),
        _ => format!("{} years ago", days / 365),
    }
}

/// 412 → "412", 9_800 → "9.8k", 128_000 → "128k", 1_500_000 → "1.5M".
pub fn compact(count: u64) -> String {
    let scaled = |value: f64, unit: &str| {
        if value >= 100.0 || (value.fract() * 10.0).round() == 0.0 {
            format!("{}{unit}", value.round() as u64)
        } else {
            format!("{value:.1}{unit}")
        }
    };
    match count {
        0..1_000 => count.to_string(),
        1_000..1_000_000 => scaled(count as f64 / 1000.0, "k"),
        _ => scaled(count as f64 / 1_000_000.0, "M"),
    }
}

/// 38 → "38 ms", 1800 → "1.8s", 125_000 → "2m 5s".
pub fn duration(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms} ms")
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        format!("{}m {}s", ms / 60_000, (ms % 60_000) / 1000)
    }
}

/// Seconds only, for live timers: "3s", "1m 4s".
pub fn seconds(ms: u64) -> String {
    if ms < 60_000 {
        format!("{}s", ms / 1000)
    } else {
        format!("{}m {}s", ms / 60_000, (ms % 60_000) / 1000)
    }
}

/// The path with the home folder written as `~`.
pub fn home_relative(path: &std::path::Path) -> String {
    if let Some(home) = dirs::home_dir()
        && let Ok(rest) = path.strip_prefix(&home) {
            return if rest.as_os_str().is_empty() {
                "~".into()
            } else {
                format!("~/{}", rest.display())
            };
        }
    path.display().to_string()
}

pub fn current_dir_label() -> String {
    std::env::current_dir()
        .map(|path| home_relative(&path))
        .unwrap_or_else(|_| ".".into())
}

/// The host of a URL without `www.`.
pub fn host(address: &str) -> String {
    reqwest::Url::parse(address)
        .ok()
        .and_then(|url| url.host_str().map(str::to_string))
        .map(|host| host.trim_start_matches("www.").to_string())
        .unwrap_or_else(|| address.to_string())
}

pub fn plural(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_numbers_match_the_status_line() {
        assert_eq!(compact(412), "412");
        assert_eq!(compact(9_800), "9.8k");
        assert_eq!(compact(10_400), "10.4k");
        assert_eq!(compact(128_000), "128k");
        assert_eq!(compact(1_000), "1k");
    }

    #[test]
    fn durations_read_naturally() {
        assert_eq!(duration(38), "38 ms");
        assert_eq!(duration(1_800), "1.8s");
        assert_eq!(duration(2_300), "2.3s");
    }

    #[test]
    fn truncate_and_pad_respect_display_width() {
        assert_eq!(truncate("hello world", 8), "hello w…");
        assert_eq!(pad("ab", 4), "ab  ");
        assert_eq!(width(&pad("日本語テキスト", 6)), 6);
    }

    #[test]
    fn spread_right_aligns_the_right_side() {
        let line = spread(vec![Span::raw("left")], vec![Span::raw("right")], 12);
        assert_eq!(line.to_string(), "left   right");
    }
}
