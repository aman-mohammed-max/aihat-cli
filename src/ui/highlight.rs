//! Code highlighting with syntect and bat's grammar collection (two-face).
//!
//! The syntect theme is built from the design's colour roles: keywords are
//! purple, function names and numbers orange, strings lime, JSON keys and
//! tags cyan, comments dim. Colours are written as role sentinels and turned
//! into real terminal colours afterwards, so highlighting follows the
//! terminal's colour depth like everything else.

use super::theme::{Role, theme};
use ratatui::style::Style;
use ratatui::text::Span;
use std::str::FromStr;
use std::sync::OnceLock;
use syntect::easy::HighlightLines;
use syntect::highlighting::{
    Color as SynColor, ScopeSelectors, StyleModifier, Theme as SynTheme, ThemeItem, ThemeSettings,
};
use syntect::parsing::{SyntaxReference, SyntaxSet};

fn syntaxes() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(two_face::syntax::extra_newlines)
}

fn role_theme() -> &'static SynTheme {
    static THEME: OnceLock<SynTheme> = OnceLock::new();
    THEME.get_or_init(|| {
        let color = |role: Role| {
            let (r, g, b) = role.sentinel();
            SynColor { r, g, b, a: 255 }
        };
        // Later items win when selectors have the same specificity.
        let rules: &[(&str, Role)] = &[
            ("comment, punctuation.definition.comment", Role::Dim),
            ("string, punctuation.definition.string", Role::Accent),
            ("constant.numeric, constant.character.escape", Role::Warn),
            (
                "keyword, storage, constant.language, variable.language.this, variable.language.self",
                Role::Code,
            ),
            ("keyword.operator", Role::Text),
            ("keyword.operator.word, keyword.operator.logical.python", Role::Code),
            (
                "entity.name.function, support.function, variable.function, meta.function-call.identifier",
                Role::Warn,
            ),
            ("entity.name.tag, meta.mapping.key string, support.type.property-name", Role::Link),
            ("entity.other.attribute-name", Role::Warn),
            ("markup.heading, markup.bold", Role::Strong),
            ("markup.inserted", Role::Accent),
            ("markup.deleted", Role::Error),
            ("markup.raw, markup.underline.link", Role::Link),
        ];
        let scopes = rules
            .iter()
            .filter_map(|(selector, role)| {
                Some(ThemeItem {
                    scope: ScopeSelectors::from_str(selector).ok()?,
                    style: StyleModifier {
                        foreground: Some(color(*role)),
                        background: None,
                        font_style: None,
                    },
                })
            })
            .collect();
        SynTheme {
            name: Some("aihat".into()),
            author: None,
            settings: ThemeSettings {
                foreground: Some(color(Role::Text)),
                ..ThemeSettings::default()
            },
            scopes,
        }
    })
}

fn find_syntax(language: &str) -> Option<&'static SyntaxReference> {
    let language = language.trim().trim_start_matches('.').to_ascii_lowercase();
    if language.is_empty() {
        return None;
    }
    let alias = match language.as_str() {
        "ts" | "typescript" | "tsx" => "TypeScript",
        "js" | "javascript" | "jsx" | "mjs" => "JavaScript",
        "sh" | "shell" | "bash" | "zsh" | "console" => "Bourne Again Shell (bash)",
        "py" | "python3" => "Python",
        "kt" | "kts" => "Kotlin",
        "yml" => "YAML",
        "rs" => "Rust",
        _ => "",
    };
    let set = syntaxes();
    (!alias.is_empty())
        .then(|| set.find_syntax_by_name(alias))
        .flatten()
        .or_else(|| set.find_syntax_by_token(&language))
        .or_else(|| set.find_syntax_by_extension(&language))
}

/// A stateful highlighter for one code block. Feed it the block's lines in order.
pub struct CodeHighlighter {
    inner: Option<HighlightLines<'static>>,
}

impl CodeHighlighter {
    pub fn new(language: &str) -> Self {
        Self {
            inner: find_syntax(language).map(|syntax| HighlightLines::new(syntax, role_theme())),
        }
    }

    /// Highlight one line (without its newline).
    pub fn line(&mut self, text: &str) -> Vec<Span<'static>> {
        let base = theme().fg(Role::Text);
        let Some(inner) = self.inner.as_mut() else {
            return vec![Span::styled(text.to_string(), base)];
        };
        let with_newline = format!("{text}\n");
        match inner.highlight_line(&with_newline, syntaxes()) {
            Ok(ranges) => ranges
                .into_iter()
                .filter_map(|(style, piece)| {
                    let piece = piece.trim_end_matches('\n');
                    if piece.is_empty() {
                        return None;
                    }
                    let fg = style.foreground;
                    let role = Role::from_sentinel((fg.r, fg.g, fg.b)).unwrap_or(Role::Text);
                    Some(Span::styled(piece.to_string(), style_for(role)))
                })
                .collect(),
            Err(_) => vec![Span::styled(text.to_string(), base)],
        }
    }
}

fn style_for(role: Role) -> Style {
    theme().fg(role)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_keywords_and_calls_get_their_roles() {
        let mut highlighter = CodeHighlighter::new("rust");
        let spans = highlighter.line("let vm = Vm::spawn(&cfg)?;");
        let text: String = spans.iter().map(|span| span.content.as_ref()).collect();
        assert_eq!(text, "let vm = Vm::spawn(&cfg)?;");
        assert!(spans.len() > 2, "expected highlighted pieces, got {spans:?}");
    }

    #[test]
    fn unknown_languages_fall_back_to_plain_text() {
        let mut highlighter = CodeHighlighter::new("no-such-language");
        let spans = highlighter.line("plain words");
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content, "plain words");
    }
}
