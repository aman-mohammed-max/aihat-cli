//! Key bindings from settings.json: `"switch_model": "ctrl+p"`.

use crate::config::{KeyConfig, Keys};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Send,
    NewLine,
    Stop,
    SwitchModel,
    SwitchPrompt,
    PastChats,
    NewChat,
    ClearScreen,
    ToggleReasoning,
    CopyReply,
    ShowKeys,
    Quit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Key {
    code: KeyCode,
    modifiers: KeyModifiers,
}

impl Key {
    /// A printable key without ctrl or alt: it only acts on an empty message
    /// box, otherwise it types.
    fn types_text(&self) -> bool {
        matches!(self.code, KeyCode::Char(_)) && (self.modifiers - KeyModifiers::SHIFT).is_empty()
    }

    fn matches(&self, event: &KeyEvent) -> bool {
        let mut modifiers = event.modifiers & (KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT | KeyModifiers::SUPER);
        let code = match event.code {
            KeyCode::BackTab => {
                modifiers |= KeyModifiers::SHIFT;
                KeyCode::Tab
            }
            KeyCode::Char(c) => {
                // The character already carries shift ("?" is shift+/).
                if !self.modifiers.contains(KeyModifiers::SHIFT) {
                    modifiers.remove(KeyModifiers::SHIFT);
                }
                if modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
                    KeyCode::Char(c.to_ascii_lowercase())
                } else {
                    KeyCode::Char(c)
                }
            }
            other => other,
        };
        code == self.code && modifiers == self.modifiers
    }
}

/// "ctrl+p", "shift+enter", "alt+up", "f2", "?".
pub fn parse_key(text: &str) -> Result<Key, String> {
    let lower = text.trim().to_lowercase();
    if lower.is_empty() {
        return Err("A key can't be empty".into());
    }
    // "+" itself and "ctrl++" end in a plus sign that is the key.
    let (mods_part, key_part) = match lower.strip_suffix("++") {
        Some(rest) => (format!("{rest}+"), "+".to_string()),
        None if lower == "+" => (String::new(), "+".to_string()),
        None => match lower.rsplit_once('+') {
            Some((mods, key)) => (format!("{mods}+"), key.to_string()),
            None => (String::new(), lower.clone()),
        },
    };
    let mut modifiers = KeyModifiers::NONE;
    for part in mods_part.split('+').filter(|part| !part.is_empty()) {
        modifiers |= match part {
            "ctrl" | "control" | "c" => KeyModifiers::CONTROL,
            "alt" | "option" | "opt" | "meta" | "m" => KeyModifiers::ALT,
            "shift" | "s" => KeyModifiers::SHIFT,
            "super" | "cmd" | "win" => KeyModifiers::SUPER,
            other => return Err(format!("\"{text}\" is not a key: {other} is not ctrl, alt, shift or super")),
        };
    }
    let code = match key_part.as_str() {
        "enter" | "return" => KeyCode::Enter,
        "esc" | "escape" => KeyCode::Esc,
        "tab" => KeyCode::Tab,
        "space" => KeyCode::Char(' '),
        "backspace" => KeyCode::Backspace,
        "delete" | "del" => KeyCode::Delete,
        "insert" | "ins" => KeyCode::Insert,
        "up" | "↑" => KeyCode::Up,
        "down" | "↓" => KeyCode::Down,
        "left" | "←" => KeyCode::Left,
        "right" | "→" => KeyCode::Right,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pgup" | "pageup" => KeyCode::PageUp,
        "pgdn" | "pagedown" => KeyCode::PageDown,
        f if f.len() > 1 && f.starts_with('f') && f[1..].parse::<u8>().is_ok_and(|n| (1..=24).contains(&n)) => {
            KeyCode::F(f[1..].parse().unwrap_or(1))
        }
        single if single.chars().count() == 1 => {
            let c = single.chars().next().unwrap_or(' ');
            // Keep the case the person typed for plain characters.
            let typed = text.trim().chars().last().unwrap_or(c);
            if modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) { KeyCode::Char(c) } else { KeyCode::Char(typed) }
        }
        other => return Err(format!("\"{text}\" is not a key: try names like ctrl+p, shift+enter, alt+up, f2 (not \"{other}\")")),
    };
    Ok(Key { code, modifiers })
}

struct Binding {
    action: Action,
    keys: Vec<Key>,
    label: String,
}

pub struct Keymap {
    bindings: Vec<Binding>,
}

static KEYMAP: OnceLock<Keymap> = OnceLock::new();

/// Read the bindings once at start. Returns a sentence for each key that
/// could not be read; those actions keep their default key.
pub fn init(config: &KeyConfig) -> Vec<String> {
    let defaults = KeyConfig::default();
    let entries: [(Action, &str, &Keys, &Keys); 12] = [
        (Action::Send, "send", &config.send, &defaults.send),
        (Action::NewLine, "new_line", &config.new_line, &defaults.new_line),
        (Action::Stop, "stop", &config.stop, &defaults.stop),
        (Action::SwitchModel, "switch_model", &config.switch_model, &defaults.switch_model),
        (Action::SwitchPrompt, "switch_prompt", &config.switch_prompt, &defaults.switch_prompt),
        (Action::PastChats, "past_chats", &config.past_chats, &defaults.past_chats),
        (Action::NewChat, "new_chat", &config.new_chat, &defaults.new_chat),
        (Action::ClearScreen, "clear_screen", &config.clear_screen, &defaults.clear_screen),
        (Action::ToggleReasoning, "toggle_reasoning", &config.toggle_reasoning, &defaults.toggle_reasoning),
        (Action::CopyReply, "copy_reply", &config.copy_reply, &defaults.copy_reply),
        (Action::ShowKeys, "show_keys", &config.show_keys, &defaults.show_keys),
        (Action::Quit, "quit", &config.quit, &defaults.quit),
    ];
    let mut problems = Vec::new();
    let mut bindings = Vec::new();
    for (action, name, chosen, default) in entries {
        let mut parsed = Vec::new();
        let mut labels = Vec::new();
        for text in &chosen.0 {
            match parse_key(text) {
                Ok(key) => {
                    parsed.push(key);
                    labels.push(text.trim().to_lowercase());
                }
                Err(error) => problems.push(format!("settings.json keys.{name}: {error}")),
            }
        }
        if parsed.is_empty() {
            parsed = default.0.iter().filter_map(|text| parse_key(text).ok()).collect();
            labels = default.0.clone();
        }
        bindings.push(Binding {
            action,
            keys: parsed,
            label: labels.first().cloned().unwrap_or_default(),
        });
    }
    let _ = KEYMAP.set(Keymap { bindings });
    problems
}

fn keymap() -> &'static Keymap {
    if KEYMAP.get().is_none() {
        let _ = init(&KeyConfig::default());
    }
    KEYMAP.get().expect("keymap is set")
}

/// The action bound to this key. Plain characters like `?` only count
/// when the message box is empty.
pub fn action_for(event: &KeyEvent, box_empty: bool) -> Option<Action> {
    keymap()
        .bindings
        .iter()
        .find(|binding| binding.keys.iter().any(|key| key.matches(event) && (box_empty || !key.types_text())))
        .map(|binding| binding.action)
}

pub fn is(action: Action, event: &KeyEvent) -> bool {
    keymap()
        .bindings
        .iter()
        .any(|binding| binding.action == action && binding.keys.iter().any(|key| key.matches(event)))
}

/// The first key bound to an action, as shown in hints: "ctrl+p".
pub fn label(action: Action) -> &'static str {
    keymap()
        .bindings
        .iter()
        .find(|binding| binding.action == action)
        .map(|binding| binding.label.as_str())
        .unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn parses_and_matches_common_keys() {
        let ctrl_p = parse_key("ctrl+p").unwrap();
        assert!(ctrl_p.matches(&event(KeyCode::Char('p'), KeyModifiers::CONTROL)));
        assert!(!ctrl_p.matches(&event(KeyCode::Char('p'), KeyModifiers::ALT)));
        let question = parse_key("?").unwrap();
        assert!(question.matches(&event(KeyCode::Char('?'), KeyModifiers::SHIFT)));
        assert!(question.types_text());
        let shift_enter = parse_key("Shift+Enter").unwrap();
        assert!(shift_enter.matches(&event(KeyCode::Enter, KeyModifiers::SHIFT)));
        assert!(!shift_enter.matches(&event(KeyCode::Enter, KeyModifiers::NONE)));
        assert!(parse_key("ctrl++").unwrap().matches(&event(KeyCode::Char('+'), KeyModifiers::CONTROL)));
        assert!(parse_key("f2").unwrap().matches(&event(KeyCode::F(2), KeyModifiers::NONE)));
    }

    #[test]
    fn explains_keys_it_cannot_read() {
        assert!(parse_key("hyper+p").unwrap_err().contains("not ctrl, alt, shift or super"));
        assert!(parse_key("ctrl+pp").unwrap_err().contains("try names like"));
    }
}
