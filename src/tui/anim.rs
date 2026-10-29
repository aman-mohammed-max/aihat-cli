//! Small animations while something is happening: a spinner on running
//! tools and tasks, a light that sweeps across "Thinking…", a pulsing
//! streaming dot, a blinking reply cursor, and one sweep over the logo when
//! aihat opens. `"animations": false` in settings.json keeps everything still.

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

static START: OnceLock<Instant> = OnceLock::new();
static ON: AtomicBool = AtomicBool::new(true);

/// How long the logo's opening sweep lasts.
const INTRO_MS: u128 = 1400;
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub fn set_enabled(on: bool) {
    ON.store(on, Ordering::Relaxed);
    START.get_or_init(Instant::now);
}

pub fn enabled() -> bool {
    ON.load(Ordering::Relaxed)
}

fn ms() -> u128 {
    START.get_or_init(Instant::now).elapsed().as_millis()
}

/// A spinner frame, or the design's ● when animations are off.
pub fn spinner() -> &'static str {
    if enabled() { SPINNER[(ms() / 80) as usize % SPINNER.len()] } else { "●" }
}

/// On for half a second, off for half a second.
pub fn pulse() -> bool {
    !enabled() || (ms() / 500) % 2 == 0
}

/// The reply cursor blinks while text arrives.
pub fn cursor_on() -> bool {
    !enabled() || ms() % 1000 < 600
}

/// `text` with a light passing across it, left to right, then a pause.
pub fn shimmer(text: &str, base: Style, light: Style) -> Vec<Span<'static>> {
    if !enabled() {
        return vec![Span::styled(text.to_string(), base)];
    }
    let chars: Vec<char> = text.chars().collect();
    let span = chars.len() as i64 + 10;
    let at = (ms() / 70) as i64 % span - 3;
    chars
        .iter()
        .enumerate()
        .map(|(i, c)| Span::styled(c.to_string(), if (i as i64 - at).abs() <= 1 { light } else { base }))
        .collect()
}

/// Give the first span holding `word` a passing light.
pub fn shimmer_in(line: &mut Line<'static>, word: &str, light: Style) {
    let Some(index) = line.spans.iter().position(|span| span.content.contains(word)) else {
        return;
    };
    let span = line.spans.remove(index);
    let content = span.content.to_string();
    let (before, rest) = content.split_at(content.find(word).unwrap_or(0));
    let (middle, after) = rest.split_at(word.len().min(rest.len()));
    let mut parts = Vec::new();
    if !before.is_empty() {
        parts.push(Span::styled(before.to_string(), span.style));
    }
    parts.extend(shimmer(middle, span.style, light));
    if !after.is_empty() {
        parts.push(Span::styled(after.to_string(), span.style));
    }
    for (offset, part) in parts.into_iter().enumerate() {
        line.spans.insert(index + offset, part);
    }
}

/// The logo column lit by the opening sweep, while it runs.
pub fn intro_column() -> Option<u16> {
    let t = ms();
    (enabled() && t < INTRO_MS).then(|| (t * 14 / INTRO_MS) as u16)
}

/// Whether anything here needs the screen drawn again soon.
pub fn intro_running() -> bool {
    enabled() && ms() < INTRO_MS + 100
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shimmer_keeps_the_text() {
        let spans = shimmer("Thinking…", Style::default(), Style::default());
        assert_eq!(spans.iter().map(|s| s.content.as_ref()).collect::<String>(), "Thinking…");
        let mut line = Line::from(vec![Span::raw(" ◇ "), Span::raw("Thinking… 3s · ctrl+o to show")]);
        shimmer_in(&mut line, "Thinking…", Style::default());
        assert_eq!(line.to_string(), " ◇ Thinking… 3s · ctrl+o to show");
    }
}
