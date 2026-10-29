//! The design's colour roles, mapped to whatever the terminal can show.
//!
//! Truecolor terminals get the exact hex values from the design, 256-colour
//! terminals get the nearest xterm index, and 16-colour terminals get the
//! named fallback from the spec. `NO_COLOR` and `--no-color` turn colour off;
//! meaning never depends on colour alone because every state also has a glyph.

use ratatui::style::{Color, Modifier, Style};
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Role {
    /// Accent, prompt, success.
    Accent,
    /// Body text.
    Text,
    /// Emphasis: headings, names, the user's own words.
    Strong,
    /// Hints, borders, metadata.
    Dim,
    /// User marker, commands, links, citations.
    Link,
    /// Warnings, flags, function names in code.
    Warn,
    Error,
    /// Code keywords and reasoning.
    Code,
}

impl Role {
    const ALL: [Role; 8] = [
        Role::Accent,
        Role::Text,
        Role::Strong,
        Role::Dim,
        Role::Link,
        Role::Warn,
        Role::Error,
        Role::Code,
    ];

    fn rgb(self, accent: (u8, u8, u8)) -> (u8, u8, u8) {
        match self {
            Role::Accent => accent,
            Role::Text => (0xD4, 0xD7, 0xDC),
            Role::Strong => (0xF4, 0xF5, 0xF7),
            Role::Dim => (0x7A, 0x80, 0x8A),
            Role::Link => (0x7D, 0xD3, 0xFC),
            Role::Warn => (0xFD, 0xBA, 0x74),
            Role::Error => (0xF2, 0xA2, 0xA2),
            Role::Code => (0xC4, 0xB5, 0xFD),
        }
    }

    fn indexed(self, accent: (u8, u8, u8)) -> u8 {
        match self {
            Role::Accent if accent == DEFAULT_ACCENT => 191,
            Role::Accent => nearest_xterm(accent),
            Role::Text => 252,
            Role::Strong => 255,
            Role::Dim => 244,
            Role::Link => 117,
            Role::Warn => 215,
            Role::Error => 217,
            Role::Code => 183,
        }
    }

    fn named(self) -> Color {
        match self {
            Role::Accent => Color::LightGreen,
            Role::Text => Color::Reset,
            Role::Strong => Color::White,
            Role::Dim => Color::DarkGray,
            Role::Link => Color::LightCyan,
            Role::Warn => Color::Yellow,
            Role::Error => Color::LightRed,
            Role::Code => Color::Magenta,
        }
    }

    /// A colour that stands for this role inside syntax-highlighting themes.
    /// It never reaches the terminal; `Role::from_sentinel` maps it back.
    pub fn sentinel(self) -> (u8, u8, u8) {
        let index = Role::ALL.iter().position(|role| *role == self).unwrap_or(1);
        (index as u8 + 1, 0, 0)
    }

    pub fn from_sentinel(rgb: (u8, u8, u8)) -> Option<Role> {
        if rgb.1 != 0 || rgb.2 != 0 || rgb.0 == 0 {
            return None;
        }
        Role::ALL.get(rgb.0 as usize - 1).copied()
    }
}

const DEFAULT_ACCENT: (u8, u8, u8) = (0xC6, 0xF4, 0x6B);
const SELECTED_BG: (u8, u8, u8) = (0x26, 0x2A, 0x30);
const BACKGROUND: (u8, u8, u8) = (0x0B, 0x0C, 0x0E);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Depth {
    None,
    Ansi16,
    Ansi256,
    TrueColor,
}

#[derive(Clone, Debug)]
pub struct Theme {
    pub depth: Depth,
    pub ascii: bool,
    accent: (u8, u8, u8),
}

static THEME: OnceLock<Theme> = OnceLock::new();

/// Set the process-wide theme once, before anything is drawn.
pub fn init(accent: &str, depth: Depth, ascii: bool) {
    let _ = THEME.set(Theme {
        depth,
        ascii,
        accent: parse_hex(accent).unwrap_or(DEFAULT_ACCENT),
    });
}

pub fn theme() -> &'static Theme {
    THEME.get_or_init(|| Theme {
        depth: Depth::None,
        ascii: false,
        accent: DEFAULT_ACCENT,
    })
}

/// How many colours the given stream supports, honouring NO_COLOR, FORCE_COLOR
/// and the `colors` setting (auto, truecolor, 256, 16 or none). `None` when
/// colour is off or the stream is not a terminal.
pub fn detect_depth(stream: supports_color::Stream, no_color: bool, setting: &str) -> Depth {
    if no_color || std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty()) {
        return Depth::None;
    }
    let forced = match setting.trim().to_lowercase().as_str() {
        "truecolor" | "24bit" | "16m" => Some(Depth::TrueColor),
        "256" => Some(Depth::Ansi256),
        "16" => Some(Depth::Ansi16),
        "none" | "off" => Some(Depth::None),
        _ => None,
    };
    let level = supports_color::on(stream);
    match (forced, level) {
        (_, None) => Depth::None,
        (Some(depth), Some(_)) => depth,
        (None, Some(level)) if level.has_16m => Depth::TrueColor,
        (None, Some(level)) if level.has_256 && likely_truecolor() => Depth::TrueColor,
        (None, Some(level)) if level.has_256 => Depth::Ansi256,
        (None, Some(_)) => Depth::Ansi16,
    }
}

/// Many terminals show 24-bit colour without setting COLORTERM (the ChromeOS
/// terminal, xterm, Konsole, Windows Terminal). The ones that cannot are
/// rarer: older macOS Terminal, the Linux console and GNU screen.
fn likely_truecolor() -> bool {
    let var = |name: &str| std::env::var(name).unwrap_or_default();
    let term = var("TERM");
    if var("TERM_PROGRAM") == "Apple_Terminal" {
        return false;
    }
    if term.starts_with("screen") && var("TMUX").is_empty() {
        return false;
    }
    term.contains("256color") || term.contains("direct") || !var("WT_SESSION").is_empty()
}

impl Theme {
    pub fn color(&self, role: Role) -> Color {
        match self.depth {
            Depth::TrueColor => {
                let (r, g, b) = role.rgb(self.accent);
                Color::Rgb(r, g, b)
            }
            Depth::Ansi256 => Color::Indexed(role.indexed(self.accent)),
            Depth::Ansi16 => role.named(),
            Depth::None => Color::Reset,
        }
    }

    pub fn fg(&self, role: Role) -> Style {
        Style::default().fg(self.color(role))
    }

    pub fn bold(&self, role: Role) -> Style {
        self.fg(role).add_modifier(Modifier::BOLD)
    }

    /// The highlighted row of a list. Without colour it falls back to reverse
    /// video so the selection is still visible.
    pub fn selected(&self, role: Role) -> Style {
        match self.depth {
            Depth::TrueColor => {
                let (r, g, b) = SELECTED_BG;
                self.fg(role).bg(Color::Rgb(r, g, b))
            }
            Depth::Ansi256 => self.fg(role).bg(Color::Indexed(236)),
            Depth::Ansi16 | Depth::None => self.fg(role).add_modifier(Modifier::REVERSED),
        }
    }

    /// Black text on the accent colour: badges like `default` and primary buttons.
    pub fn badge(&self) -> Style {
        match self.depth {
            Depth::None => Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD),
            Depth::Ansi16 => Style::default()
                .fg(Color::Black)
                .bg(Color::LightGreen)
                .add_modifier(Modifier::BOLD),
            _ => Style::default()
                .fg(self.background())
                .bg(self.color(Role::Accent))
                .add_modifier(Modifier::BOLD),
        }
    }

    /// The block cursor drawn in text fields.
    pub fn cursor(&self) -> Style {
        match self.depth {
            Depth::None | Depth::Ansi16 => Style::default().add_modifier(Modifier::REVERSED),
            _ => Style::default()
                .fg(self.background())
                .bg(self.color(Role::Accent)),
        }
    }

    pub fn background(&self) -> Color {
        match self.depth {
            Depth::TrueColor => {
                let (r, g, b) = BACKGROUND;
                Color::Rgb(r, g, b)
            }
            Depth::Ansi256 => Color::Indexed(233),
            Depth::Ansi16 | Depth::None => Color::Reset,
        }
    }

    pub fn base(&self) -> Style {
        Style::default()
            .fg(self.color(Role::Text))
            .bg(self.background())
    }

    pub fn glyphs(&self) -> &'static Glyphs {
        if self.ascii { &ASCII } else { &UNICODE }
    }
}

/// Box-drawing characters. `--ascii` swaps in `+ - |`.
pub struct Glyphs {
    pub top_left: &'static str,
    pub top_right: &'static str,
    pub bottom_left: &'static str,
    pub bottom_right: &'static str,
    pub horizontal: &'static str,
    pub vertical: &'static str,
}

const UNICODE: Glyphs = Glyphs {
    top_left: "╭",
    top_right: "╮",
    bottom_left: "╰",
    bottom_right: "╯",
    horizontal: "─",
    vertical: "│",
};

const ASCII: Glyphs = Glyphs {
    top_left: "+",
    top_right: "+",
    bottom_left: "+",
    bottom_right: "+",
    horizontal: "-",
    vertical: "|",
};

fn parse_hex(value: &str) -> Option<(u8, u8, u8)> {
    let hex = value.trim().trim_start_matches('#');
    if hex.len() != 6 {
        return None;
    }
    let channel = |range: std::ops::Range<usize>| u8::from_str_radix(&hex[range], 16).ok();
    Some((channel(0..2)?, channel(2..4)?, channel(4..6)?))
}

/// The closest colour in the xterm 256-colour cube or grey ramp.
pub fn nearest_xterm((r, g, b): (u8, u8, u8)) -> u8 {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let nearest_level = |value: u8| {
        LEVELS
            .iter()
            .enumerate()
            .min_by_key(|(_, level)| (**level as i32 - value as i32).abs())
            .map(|(index, _)| index)
            .unwrap_or(0)
    };
    let (ri, gi, bi) = (nearest_level(r), nearest_level(g), nearest_level(b));
    let cube = (LEVELS[ri], LEVELS[gi], LEVELS[bi]);
    let average = (r as u32 + g as u32 + b as u32) / 3;
    let grey_index = ((average.saturating_sub(8)) / 10).min(23);
    let grey = (8 + grey_index * 10) as u8;
    let distance = |(cr, cg, cb): (u8, u8, u8)| {
        let dr = cr as i32 - r as i32;
        let dg = cg as i32 - g as i32;
        let db = cb as i32 - b as i32;
        dr * dr + dg * dg + db * db
    };
    if distance((grey, grey, grey)) < distance(cube) {
        232 + grey_index as u8
    } else {
        16 + (36 * ri + 6 * gi + bi) as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sentinels_round_trip_every_role() {
        for role in Role::ALL {
            assert_eq!(Role::from_sentinel(role.sentinel()), Some(role));
        }
        assert_eq!(Role::from_sentinel((0, 0, 0)), None);
        assert_eq!(Role::from_sentinel((3, 1, 0)), None);
    }

    #[test]
    fn nearest_xterm_matches_the_spec_table() {
        assert_eq!(nearest_xterm((0x7D, 0xD3, 0xFC)), 117);
        assert_eq!(nearest_xterm((0xC6, 0xF4, 0x6B)), 191);
        assert_eq!(nearest_xterm((0x7A, 0x80, 0x8A)), 244);
    }
}
