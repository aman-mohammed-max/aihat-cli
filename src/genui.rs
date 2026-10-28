use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::{Map, Value, json};
use std::collections::HashSet;
use unicode_width::UnicodeWidthStr;

const MAX_SPEC_BYTES: usize = 128_000;
const MAX_ELEMENTS: usize = 48;
const MAX_DEPTH: usize = 10;
const COMPONENTS: &[&str] = &[
    "Card",
    "Container",
    "Column",
    "Row",
    "Spacer",
    "Divider",
    "Pressable",
    "Heading",
    "Paragraph",
    "Label",
    "Badge",
    "Chip",
    "Image",
    "Button",
    "TextInput",
    "Switch",
    "Checkbox",
    "Slider",
    "ProgressBar",
    "ListItem",
    "CodeBlock",
];

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ParsedReply {
    pub text: String,
    pub spec: Option<Value>,
    pub raw: String,
    pub open: bool,
    pub has_block: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct UiAction {
    pub label: String,
    pub action: String,
    pub params: Value,
}

pub(crate) fn wants_ui(text: &str) -> bool {
    let text = text.to_lowercase();
    let asks = [
        "build", "make", "create", "generate", "design", "show", "give", "want", "need",
    ]
    .iter()
    .any(|verb| {
        text.split_whitespace()
            .any(|word| word.trim_matches(|ch: char| !ch.is_alphanumeric()) == *verb)
    });
    let interface = [
        "ui",
        "interface",
        "form",
        "checklist",
        "check-list",
        "dashboard",
        "calculator",
        "tracker",
        "planner",
        "widget",
        "mini-app",
        "mini app",
        "screen",
        "settings page",
        "survey",
        "quiz",
        "to-do list",
        "todo list",
        "table",
    ]
    .iter()
    .any(|needle| text.contains(needle));
    (asks && interface)
        || text.trim_start().starts_with("/ui")
        || text.trim_start().starts_with("ui:")
}

pub(crate) fn system_prompt() -> &'static str {
    "You can answer normally, or build a small interactive terminal UI when that helps. Write one or two sentences of ordinary Markdown, then put the UI in a ```spec fenced block as one JSON object with this shape: {\"root\":\"card\",\"elements\":{\"card\":{\"type\":\"Card\",\"props\":{\"title\":\"Title\"},\"children\":[\"heading\"]},\"heading\":{\"type\":\"Heading\",\"props\":{\"text\":\"Hello\"},\"children\":[]}},\"state\":{}}. Every element needs a type, props object, and children array. The available component types are Card, Container, Column, Row, Spacer, Divider, Pressable, Heading, Paragraph, Label, Badge, Chip, Image, Button, TextInput, Switch, Checkbox, Slider, ProgressBar, ListItem, and CodeBlock. Keep the spec under 25 elements, seed realistic state for controls, and use an on.press binding with action \"sendMessage\" and params {\"text\":\"...\"} for buttons that should ask the assistant something. Do not use model-chosen colors or unsupported components. Only emit a spec when the user asked for a form, checklist, comparison, dashboard, settings view, or small tool; otherwise answer in plain text."
}

pub(crate) fn parse_reply(content: &str, base: Option<&Value>) -> ParsedReply {
    if content.len() > MAX_SPEC_BYTES + 32_000 {
        return ParsedReply {
            text: content.to_string(),
            ..ParsedReply::default()
        };
    }

    if let Some(opening) = content.find("```") {
        let after_ticks = opening + 3;
        if let Some(header_end_relative) = content[after_ticks..].find('\n') {
            let header_end = after_ticks + header_end_relative;
            let language = content[after_ticks..header_end]
                .trim()
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_ascii_lowercase();
            if matches!(
                language.as_str(),
                "spec" | "json-render" | "jsonrender" | "jsonl" | "ui"
            ) {
                let body_start = header_end + 1;
                let remainder = &content[body_start..];
                let closing = remainder
                    .match_indices("\n```")
                    .find(|(index, _)| {
                        let tail = &remainder[index + 4..];
                        tail.trim_start().is_empty() || tail.trim_start().starts_with('\n')
                    })
                    .map(|(index, _)| index);
                let (raw, end, open) = if let Some(end) = closing {
                    (&remainder[..end], body_start + end + 4, false)
                } else {
                    (remainder, content.len(), true)
                };
                let mut text = String::new();
                text.push_str(&content[..opening]);
                if !open {
                    text.push_str(&content[end..]);
                }
                return ParsedReply {
                    text: text.trim().to_string(),
                    spec: compile_spec(raw, base),
                    raw: raw.trim().to_string(),
                    open,
                    has_block: true,
                };
            }
        }
    }

    let patch_lines = content.lines().filter(|line| is_patch_line(line)).count();
    if patch_lines >= if base.is_some() { 1 } else { 2 } {
        let raw = content
            .lines()
            .filter(|line| is_patch_line(line))
            .collect::<Vec<_>>()
            .join("\n");
        let text = content
            .lines()
            .filter(|line| !is_patch_line(line))
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string();
        return ParsedReply {
            spec: compile_spec(&raw, base),
            raw,
            text,
            open: false,
            has_block: true,
        };
    }

    ParsedReply {
        text: content.to_string(),
        ..ParsedReply::default()
    }
}

pub(crate) fn normalize_spec(input: &Value) -> Option<Value> {
    let root = input.get("root")?.as_str()?;
    let original = input.get("elements")?.as_object()?;
    if original.is_empty() || original.len() > MAX_ELEMENTS {
        return None;
    }
    let mut elements = Map::new();
    for (id, element) in original {
        let Some(kind) = element.get("type").and_then(Value::as_str) else {
            continue;
        };
        if !COMPONENTS.contains(&kind) {
            continue;
        }
        let props = element
            .get("props")
            .filter(|value| value.is_object())
            .cloned()
            .unwrap_or_else(|| json!({}));
        let children = element
            .get("children")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|child| {
                child
                    .as_str()
                    .is_some_and(|child_id| original.contains_key(child_id))
            })
            .collect::<Vec<_>>();
        let mut normalized = element.clone();
        normalized["props"] = props;
        normalized["children"] = Value::Array(children);
        elements.insert(id.clone(), normalized);
    }
    if !elements.contains_key(root) {
        return None;
    }
    let valid_ids = elements.keys().cloned().collect::<HashSet<_>>();
    for element in elements.values_mut() {
        if let Some(children) = element.get_mut("children").and_then(Value::as_array_mut) {
            children.retain(|child| {
                child
                    .as_str()
                    .is_some_and(|child_id| valid_ids.contains(child_id))
            });
        }
    }
    let reachable = reachable_elements(root, &elements);
    elements.retain(|id, _| reachable.contains(id));
    let state = input
        .get("state")
        .filter(|value| value.is_object())
        .cloned()
        .unwrap_or_else(|| json!({}));
    let mut result = input.clone();
    result["root"] = json!(root);
    result["elements"] = Value::Object(elements);
    result["state"] = state;
    Some(result)
}

pub(crate) fn compile_spec(raw: &str, base: Option<&Value>) -> Option<Value> {
    if raw.len() > MAX_SPEC_BYTES {
        return None;
    }
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(value) = serde_json::from_str::<Value>(trimmed)
        && value.get("root").is_some()
        && value.get("elements").is_some()
    {
        return normalize_spec(&value);
    }

    let mut spec = base
        .cloned()
        .unwrap_or_else(|| json!({"root":"", "elements":{}, "state":{}}));
    let mut applied = 0;
    for line in trimmed
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let Ok(patch) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if apply_patch(&mut spec, &patch).is_ok() {
            applied += 1;
        }
    }
    (applied > 0).then_some(())?;
    normalize_spec(&spec)
}

fn is_patch_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with('{')
        && serde_json::from_str::<Value>(trimmed)
            .ok()
            .and_then(|value| value.get("op").and_then(Value::as_str).map(str::to_string))
            .is_some()
}

fn reachable_elements(root: &str, elements: &Map<String, Value>) -> HashSet<String> {
    let mut seen = HashSet::new();
    let mut stack = vec![root.to_string()];
    while let Some(id) = stack.pop() {
        if seen.len() >= MAX_ELEMENTS || !seen.insert(id.clone()) {
            continue;
        }
        if let Some(children) = elements
            .get(&id)
            .and_then(|element| element.get("children"))
            .and_then(Value::as_array)
        {
            stack.extend(
                children
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string),
            );
        }
    }
    seen
}

fn apply_patch(document: &mut Value, patch: &Value) -> Result<(), ()> {
    let op = patch.get("op").and_then(Value::as_str).ok_or(())?;
    let path = patch.get("path").and_then(Value::as_str).ok_or(())?;
    match op {
        "add" => set_pointer(document, path, patch.get("value").cloned().ok_or(())?, true),
        "replace" => {
            if get_pointer(document, path).is_none() {
                return Err(());
            }
            set_pointer(
                document,
                path,
                patch.get("value").cloned().ok_or(())?,
                false,
            )
        }
        "remove" => {
            remove_pointer(document, path)?;
            Ok(())
        }
        "copy" | "move" => {
            let from = patch.get("from").and_then(Value::as_str).ok_or(())?;
            let value = get_pointer(document, from).cloned().ok_or(())?;
            if op == "move" {
                remove_pointer(document, from)?;
            }
            set_pointer(document, path, value, true)
        }
        "test" => (get_pointer(document, path) == patch.get("value"))
            .then_some(())
            .ok_or(()),
        _ => Err(()),
    }
}

fn pointer_parts(pointer: &str) -> Result<Vec<String>, ()> {
    if pointer.is_empty() {
        return Ok(Vec::new());
    }
    if !pointer.starts_with('/') {
        return Err(());
    }
    pointer[1..]
        .split('/')
        .map(|part| {
            let mut decoded = String::with_capacity(part.len());
            let mut chars = part.chars();
            while let Some(ch) = chars.next() {
                if ch == '~' {
                    match chars.next().ok_or(())? {
                        '0' => decoded.push('~'),
                        '1' => decoded.push('/'),
                        _ => return Err(()),
                    }
                } else {
                    decoded.push(ch);
                }
            }
            Ok(decoded)
        })
        .collect()
}

fn get_pointer<'a>(document: &'a Value, pointer: &str) -> Option<&'a Value> {
    let mut current = document;
    for part in pointer_parts(pointer).ok()? {
        current = match current {
            Value::Object(object) => object.get(&part)?,
            Value::Array(array) => array.get(part.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(current)
}

fn set_pointer(document: &mut Value, pointer: &str, value: Value, add: bool) -> Result<(), ()> {
    let parts = pointer_parts(pointer)?;
    if parts.is_empty() {
        *document = value;
        return Ok(());
    }
    let (key, parents) = parts.split_last().ok_or(())?;
    let mut parent = document;
    for part in parents {
        parent = match parent {
            Value::Object(object) => object.get_mut(part).ok_or(())?,
            Value::Array(array) => array
                .get_mut(part.parse::<usize>().map_err(|_| ())?)
                .ok_or(())?,
            _ => return Err(()),
        };
    }
    match parent {
        Value::Object(object) => {
            if !add && !object.contains_key(key) {
                return Err(());
            }
            object.insert(key.clone(), value);
            Ok(())
        }
        Value::Array(array) => {
            if add && key == "-" {
                array.push(value);
                return Ok(());
            }
            let index = key.parse::<usize>().map_err(|_| ())?;
            if add {
                if index > array.len() {
                    return Err(());
                }
                array.insert(index, value);
            } else {
                *array.get_mut(index).ok_or(())? = value;
            }
            Ok(())
        }
        _ => Err(()),
    }
}

fn remove_pointer(document: &mut Value, pointer: &str) -> Result<Value, ()> {
    let parts = pointer_parts(pointer)?;
    if parts.is_empty() {
        return Err(());
    }
    let (key, parents) = parts.split_last().ok_or(())?;
    let mut parent = document;
    for part in parents {
        parent = match parent {
            Value::Object(object) => object.get_mut(part).ok_or(())?,
            Value::Array(array) => array
                .get_mut(part.parse::<usize>().map_err(|_| ())?)
                .ok_or(())?,
            _ => return Err(()),
        };
    }
    match parent {
        Value::Object(object) => object.remove(key).ok_or(()),
        Value::Array(array) => {
            let index = key.parse::<usize>().map_err(|_| ())?;
            (index < array.len()).then(|| array.remove(index)).ok_or(())
        }
        _ => Err(()),
    }
}

pub(crate) fn actions(spec: &Value) -> Vec<UiAction> {
    let Some(root) = spec.get("root").and_then(Value::as_str) else {
        return Vec::new();
    };
    let Some(elements) = spec.get("elements").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut ordered = Vec::new();
    collect_action_elements(root, elements, &mut HashSet::new(), &mut ordered, 0);
    ordered
        .into_iter()
        .filter_map(|element| {
            let props = element.get("props")?;
            let action_binding = element
                .get("on")
                .and_then(|on| on.get("press"))
                .or_else(|| props.get("onPress"));
            if let Some(action) = action_binding
                .and_then(|binding| binding.get("action"))
                .or_else(|| props.get("action"))
                .and_then(Value::as_str)
            {
                let params = action_binding
                    .and_then(|binding| binding.get("params"))
                    .or_else(|| props.get("actionParams"))
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                let label = ["label", "text", "title"]
                    .iter()
                    .find_map(|key| props.get(*key).and_then(Value::as_str))
                    .unwrap_or("Action")
                    .to_string();
                return Some(UiAction {
                    label,
                    action: action.to_string(),
                    params,
                });
            }

            let kind = element.get("type").and_then(Value::as_str)?;
            if !matches!(kind, "Switch" | "Checkbox") {
                return None;
            }
            let binding = props.get("checked").or_else(|| props.get("value"))?;
            let path = binding
                .get("$state")
                .or_else(|| binding.get("$bindState"))?
                .as_str()?;
            path.starts_with('/').then(|| UiAction {
                label: props
                    .get("label")
                    .or_else(|| props.get("text"))
                    .and_then(Value::as_str)
                    .map(|label| format!("Toggle {label}"))
                    .unwrap_or_else(|| "Toggle option".into()),
                action: "toggleState".into(),
                params: json!({"statePath": path}),
            })
        })
        .take(9)
        .collect()
}

pub(crate) fn resolve_action_string(value: &Value, spec: &Value) -> Option<String> {
    resolve(value, spec.get("state").unwrap_or(&Value::Null))?
        .as_str()
        .map(str::to_string)
}

pub(crate) fn apply_state_action(spec: &Value, action: &UiAction) -> Option<Value> {
    let path = action
        .params
        .get("statePath")
        .or_else(|| action.params.get("path"))
        .and_then(Value::as_str)?;
    if !path.starts_with('/') {
        return None;
    }
    let mut updated = spec.clone();
    if !updated.get("state").is_some_and(Value::is_object) {
        updated["state"] = json!({});
    }
    let state = updated.get_mut("state")?;
    match action.action.as_str() {
        "setState" => {
            let value = action.params.get("value")?.clone();
            let add = get_pointer(state, path).is_none();
            set_pointer(state, path, value, add).ok()?;
        }
        "pushState" => {
            let value = action.params.get("value")?.clone();
            let existing = get_pointer(state, path)
                .cloned()
                .unwrap_or_else(|| json!([]));
            let mut values = existing.as_array()?.clone();
            values.push(value);
            let add = get_pointer(state, path).is_none();
            set_pointer(state, path, Value::Array(values), add).ok()?;
        }
        "toggleState" => {
            let current = get_pointer(state, path)
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let add = get_pointer(state, path).is_none();
            set_pointer(state, path, json!(!current), add).ok()?;
        }
        "removeState" => {
            remove_pointer(state, path).ok()?;
        }
        _ => return None,
    }
    Some(updated)
}

fn collect_action_elements<'a>(
    id: &str,
    elements: &'a Map<String, Value>,
    seen: &mut HashSet<String>,
    output: &mut Vec<&'a Value>,
    depth: usize,
) {
    if depth > MAX_DEPTH || !seen.insert(id.to_string()) {
        return;
    }
    let Some(element) = elements.get(id) else {
        return;
    };
    output.push(element);
    if let Some(children) = element.get("children").and_then(Value::as_array) {
        for child in children.iter().filter_map(Value::as_str) {
            collect_action_elements(child, elements, seen, output, depth + 1);
        }
    }
}

pub(crate) fn render_spec(
    spec: &Value,
    width: u16,
    body: Color,
    accent: Color,
    dim: Color,
) -> Vec<Line<'static>> {
    let Some(root) = spec.get("root").and_then(Value::as_str) else {
        return Vec::new();
    };
    let Some(elements) = spec.get("elements").and_then(Value::as_object) else {
        return Vec::new();
    };
    let inner = width.saturating_sub(4).max(18) as usize;
    let title = fit("Generated UI", inner.saturating_sub(4));
    let title_width = title_width(&title);
    let mut lines = vec![Line::from(vec![
        Span::styled("╭─ ", Style::default().fg(accent)),
        Span::styled(
            title,
            Style::default().fg(body).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            " ".repeat(inner.saturating_sub(title_width + 4)),
            Style::default().fg(accent),
        ),
        Span::styled("╮", Style::default().fg(accent)),
    ])];
    let mut seen = HashSet::new();
    render_element(
        root,
        elements,
        spec.get("state").unwrap_or(&Value::Null),
        &mut seen,
        &mut lines,
        0,
        inner,
        body,
        accent,
        dim,
    );
    let elements_count = elements.len();
    lines.push(Line::from(vec![
        Span::styled("╰─ ", Style::default().fg(accent)),
        Span::styled(
            format!("Generated UI · {elements_count} elements"),
            Style::default().fg(dim),
        ),
        Span::styled(
            " ".repeat(inner.saturating_sub(23 + digits(elements_count))),
            Style::default().fg(accent),
        ),
        Span::styled("╯", Style::default().fg(accent)),
    ]));
    lines
}

fn render_element(
    id: &str,
    elements: &Map<String, Value>,
    state: &Value,
    seen: &mut HashSet<String>,
    lines: &mut Vec<Line<'static>>,
    depth: usize,
    width: usize,
    body: Color,
    accent: Color,
    dim: Color,
) {
    if depth > MAX_DEPTH || seen.len() >= MAX_ELEMENTS || !seen.insert(id.to_string()) {
        return;
    }
    let Some(element) = elements.get(id) else {
        return;
    };
    let Some(kind) = element.get("type").and_then(Value::as_str) else {
        return;
    };
    let props = element.get("props").unwrap_or(&Value::Null);
    let indent = if depth == 0 {
        "│ ".to_string()
    } else {
        format!("│ {}", "  ".repeat(depth.min(5)))
    };
    let available = width.saturating_sub(indent.len() + 2).max(8);
    let children = element
        .get("children")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let text_prop = |keys: &[&str]| {
        keys.iter().find_map(|key| {
            resolve(props.get(*key)?, state)?
                .as_str()
                .map(str::to_string)
        })
    };
    match kind {
        "Card" => {
            if let Some(title) = text_prop(&["title"]) {
                push_text(lines, &indent, &title, available, accent, true);
            }
            if let Some(subtitle) = text_prop(&["subtitle"]) {
                push_text(lines, &indent, &subtitle, available, dim, false);
            }
            render_children(
                &children, elements, state, seen, lines, depth, width, body, accent, dim,
            );
        }
        "Column" | "Container" | "Pressable" => {
            render_children(
                &children, elements, state, seen, lines, depth, width, body, accent, dim,
            );
        }
        "Row" => {
            let mut names = Vec::new();
            for child in &children {
                if let Some(child_id) = child.as_str()
                    && let Some(child_element) = elements.get(child_id)
                    && let Some(text) = simple_element_text(child_element, state)
                {
                    names.push(text);
                    seen.insert(child_id.to_string());
                } else if let Some(child_id) = child.as_str() {
                    render_element(
                        child_id,
                        elements,
                        state,
                        seen,
                        lines,
                        depth + 1,
                        width,
                        body,
                        accent,
                        dim,
                    );
                }
            }
            if !names.is_empty() {
                push_text(lines, &indent, &names.join("   "), available, body, false);
            }
        }
        "Spacer" => lines.push(Line::default()),
        "Divider" => lines.push(Line::from(Span::styled(
            format!("{indent}{}", "─".repeat(available.min(48))),
            Style::default().fg(dim),
        ))),
        "Heading" => {
            if let Some(text) = text_prop(&["text"]) {
                push_text(lines, &indent, &text, available, body, true);
            }
        }
        "Paragraph" => {
            if let Some(text) = text_prop(&["text"]) {
                push_text(lines, &indent, &text, available, body, false);
            }
        }
        "Label" => {
            if let Some(text) = text_prop(&["text"]) {
                push_text(lines, &indent, &text, available, dim, false);
            }
        }
        "Badge" | "Chip" => {
            if let Some(text) = text_prop(&["label", "text"]) {
                push_text(
                    lines,
                    &indent,
                    &format!("[{text}]"),
                    available,
                    accent,
                    false,
                );
            }
        }
        "Button" => {
            if let Some(label) = text_prop(&["label", "text"]) {
                let action_marker = if element.get("on").is_some() || props.get("action").is_some()
                {
                    "  · /ui <number>"
                } else {
                    ""
                };
                push_text(
                    lines,
                    &indent,
                    &format!("[ {label} ]{action_marker}"),
                    available,
                    accent,
                    true,
                );
            }
        }
        "ListItem" => {
            if let Some(title) = text_prop(&["title"]) {
                let subtitle = text_prop(&["subtitle"]).unwrap_or_default();
                let trailing = text_prop(&["trailing"]).unwrap_or_default();
                let text = match (subtitle.is_empty(), trailing.is_empty()) {
                    (true, true) => format!("• {title}"),
                    (false, true) => format!("• {title} · {subtitle}"),
                    (true, false) => format!("• {title}  {trailing}"),
                    (false, false) => format!("• {title} · {subtitle}  {trailing}"),
                };
                push_text(lines, &indent, &text, available, body, false);
            }
        }
        "TextInput" => {
            let label = text_prop(&["label"]).unwrap_or_else(|| "Input".into());
            let value = text_prop(&["value", "placeholder"]).unwrap_or_default();
            let value = if props.get("secureTextEntry").and_then(Value::as_bool) == Some(true) {
                "•".repeat(value.chars().count().min(16))
            } else {
                value
            };
            push_text(
                lines,
                &indent,
                &format!("{label}:  {value}"),
                available,
                body,
                false,
            );
        }
        "Switch" | "Checkbox" => {
            let label = text_prop(&["label", "text"]).unwrap_or_else(|| "Option".into());
            let checked = props
                .get("checked")
                .or_else(|| props.get("value"))
                .and_then(|value| resolve(value, state))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let mark = if checked { "☑" } else { "☐" };
            push_text(
                lines,
                &indent,
                &format!("{mark} {label}"),
                available,
                body,
                false,
            );
        }
        "Slider" => {
            let label = text_prop(&["label"]).unwrap_or_else(|| "Value".into());
            let value = props
                .get("value")
                .and_then(|value| resolve(value, state))
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
            let min = props
                .get("min")
                .and_then(|value| resolve(value, state))
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
            let max = props
                .get("max")
                .and_then(|value| resolve(value, state))
                .and_then(Value::as_f64)
                .unwrap_or(100.0);
            push_text(
                lines,
                &indent,
                &format!("{label}: {value:.0}  ({min:.0}–{max:.0})"),
                available,
                body,
                false,
            );
        }
        "ProgressBar" => {
            let progress = props
                .get("progress")
                .and_then(|value| resolve(value, state))
                .and_then(Value::as_f64)
                .unwrap_or(0.0)
                .clamp(0.0, 1.0);
            let filled = (progress * 16.0).round() as usize;
            let label = text_prop(&["label"]).unwrap_or_default();
            let text = format!(
                "{}[{}{}] {:>3.0}%",
                if label.is_empty() {
                    String::new()
                } else {
                    format!("{label} ")
                },
                "█".repeat(filled),
                "░".repeat(16 - filled),
                progress * 100.0
            );
            push_text(lines, &indent, &text, available, accent, false);
        }
        "CodeBlock" => {
            if let Some(code) = text_prop(&["code"]) {
                let language = text_prop(&["language"]).unwrap_or_else(|| "code".into());
                push_text(
                    lines,
                    &indent,
                    &format!("┌─ {language}"),
                    available,
                    dim,
                    false,
                );
                for row in code.lines().take(10) {
                    push_text(
                        lines,
                        &format!("{indent}│ "),
                        row,
                        available.saturating_sub(2),
                        body,
                        false,
                    );
                }
                push_text(lines, &indent, "└────────────────", available, dim, false);
            }
        }
        "Image" => {
            let alt = text_prop(&["alt", "src", "url"]).unwrap_or_else(|| "image".into());
            push_text(lines, &indent, &format!("▧ {alt}"), available, dim, false);
        }
        _ => {}
    }
}

fn render_children(
    children: &[Value],
    elements: &Map<String, Value>,
    state: &Value,
    seen: &mut HashSet<String>,
    lines: &mut Vec<Line<'static>>,
    depth: usize,
    width: usize,
    body: Color,
    accent: Color,
    dim: Color,
) {
    for child in children.iter().filter_map(Value::as_str) {
        render_element(
            child,
            elements,
            state,
            seen,
            lines,
            depth + 1,
            width,
            body,
            accent,
            dim,
        );
    }
}

fn simple_element_text(element: &Value, state: &Value) -> Option<String> {
    let props = element.get("props")?;
    let kind = element.get("type")?.as_str()?;
    let key = if kind == "Button" || kind == "Chip" || kind == "Badge" {
        "label"
    } else if kind == "Heading" || kind == "Paragraph" || kind == "Label" {
        "text"
    } else {
        return None;
    };
    resolve(props.get(key)?, state)?
        .as_str()
        .map(str::to_string)
}

fn resolve<'a>(value: &'a Value, state: &'a Value) -> Option<&'a Value> {
    if let Some(path) = value
        .get("$state")
        .or_else(|| value.get("$bindState"))
        .and_then(Value::as_str)
    {
        return value_at_pointer(state, path);
    }
    Some(value)
}

fn value_at_pointer<'a>(value: &'a Value, pointer: &str) -> Option<&'a Value> {
    if !pointer.starts_with('/') {
        return value.get(pointer);
    }
    pointer_parts(pointer)
        .ok()?
        .into_iter()
        .try_fold(value, |current, part| current.get(part))
}

fn push_text(
    output: &mut Vec<Line<'static>>,
    indent: &str,
    text: &str,
    width: usize,
    color: Color,
    bold: bool,
) {
    for (index, row) in wrap_text(text, width).into_iter().enumerate() {
        output.push(Line::from(vec![
            Span::styled(
                if index == 0 {
                    indent.to_string()
                } else {
                    format!("{indent}  ")
                },
                Style::default().fg(Color::Reset),
            ),
            Span::styled(
                row,
                Style::default().fg(color).add_modifier(if bold {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
            ),
        ]));
    }
}

fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let width = width.max(8);
    let mut output = Vec::new();
    for paragraph in text.lines() {
        if paragraph.is_empty() {
            output.push(String::new());
            continue;
        }
        let mut current = String::new();
        let mut current_width = 0;
        for word in paragraph.split_whitespace() {
            let word_width = UnicodeWidthStr::width(word);
            if !current.is_empty() && current_width + 1 + word_width > width {
                output.push(std::mem::take(&mut current));
                current_width = 0;
            }
            if !current.is_empty() {
                current.push(' ');
                current_width += 1;
            }
            for ch in word.chars() {
                let ch_width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
                if current_width + ch_width > width && !current.is_empty() {
                    output.push(std::mem::take(&mut current));
                    current_width = 0;
                }
                current.push(ch);
                current_width += ch_width;
            }
        }
        if !current.is_empty() {
            output.push(current);
        }
    }
    output
}

fn fit(text: &str, width: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let size = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + size > width {
            break;
        }
        used += size;
        out.push(ch);
    }
    out
}

fn title_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

fn digits(value: usize) -> usize {
    value.to_string().len()
}

#[cfg(test)]
mod tests {
    use super::{
        actions, apply_state_action, compile_spec, normalize_spec, parse_reply, render_spec,
        wants_ui,
    };
    use ratatui::style::Color;
    use serde_json::json;

    #[test]
    fn parses_spec_fence_and_removes_it_from_chat_text() {
        let reply = parse_reply(
            "A quick checklist.\n\n```spec\n{\"root\":\"card\",\"elements\":{\"card\":{\"type\":\"Card\",\"props\":{\"title\":\"Trip\"},\"children\":[\"heading\"]},\"heading\":{\"type\":\"Heading\",\"props\":{\"text\":\"Pack\"},\"children\":[]}},\"state\":{}}\n```",
            None,
        );
        assert_eq!(reply.text, "A quick checklist.");
        assert!(!reply.open);
        assert_eq!(reply.spec.as_ref().unwrap()["root"], "card");
    }

    #[test]
    fn streams_json_patch_specs_and_ignores_an_incomplete_last_line() {
        let raw = "{\"op\":\"add\",\"path\":\"/root\",\"value\":\"card\"}\n{\"op\":\"add\",\"path\":\"/elements/card\",\"value\":{\"type\":\"Card\",\"props\":{},\"children\":[\"title\"]}}\n{\"op\":\"add\",\"path\":\"/elements/title\",\"value\":{\"type\":\"Heading\",\"props\":{\"text\":\"Ready\"},\"children\":[]}}\n{\"op\":\"add\",\"path\":\"/state/open\",\"value\":true}\n{\"op\":\"add\",\"path\":\"/elements/incomplete\"";
        let spec = compile_spec(raw, None).unwrap();
        assert_eq!(spec["elements"]["title"]["props"]["text"], "Ready");
        assert_eq!(spec["state"]["open"], true);
        assert!(spec["elements"].get("incomplete").is_none());
    }

    #[test]
    fn streaming_fence_reports_building_without_leaking_partial_json() {
        let reply = parse_reply("Here is a form.\n```spec\n{\"root\":", None);
        assert!(reply.open);
        assert_eq!(reply.text, "Here is a form.");
        assert!(reply.spec.is_none());
    }

    #[test]
    fn unsupported_components_are_removed_and_unreachable_nodes_are_pruned() {
        let spec = json!({
            "root":"root",
            "elements":{
                "root":{"type":"Card","children":["known","bad"],"props":{}},
                "known":{"type":"Heading","props":{"text":"ok"}},
                "bad":{"type":"Script","props":{},"children":[]},
                "unused":{"type":"Paragraph","props":{"text":"hidden"},"children":[]}
            },
            "state":[]
        });
        let clean = normalize_spec(&spec).unwrap();
        assert!(clean["elements"].get("bad").is_none());
        assert!(clean["elements"].get("unused").is_none());
        assert_eq!(clean["elements"]["root"]["children"], json!(["known"]));
        assert_eq!(clean["state"], json!({}));
    }

    #[test]
    fn exposes_safe_actions_and_renders_terminal_components() {
        let spec = json!({
            "root":"card",
            "elements":{
                "card":{"type":"Card","props":{"title":"Daily plan"},"children":["text","button"]},
                "text":{"type":"Paragraph","props":{"text":"A short plan."},"children":[]},
                "button":{"type":"Button","props":{"label":"Start"},"on":{"press":{"action":"sendMessage","params":{"text":"Start my plan"}}},"children":[]}
            },
            "state":{}
        });
        let actions = actions(&spec);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].action, "sendMessage");
        assert_eq!(actions[0].params["text"], "Start my plan");
        let lines = render_spec(&spec, 64, Color::White, Color::Green, Color::DarkGray);
        let rendered = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(rendered.contains("Daily plan"));
        assert!(rendered.contains("A short plan."));
        assert!(rendered.contains("Start"));
    }

    #[test]
    fn state_actions_update_local_spec_state() {
        let spec = json!({
            "root":"root",
            "elements":{"root":{"type":"Card","props":{},"children":[]}},
            "state":{"active":"speed"}
        });
        let action = super::UiAction {
            label: "Context".into(),
            action: "setState".into(),
            params: json!({"statePath":"/active","value":"context"}),
        };
        let updated = apply_state_action(&spec, &action).unwrap();
        assert_eq!(updated["state"]["active"], "context");
        assert_eq!(spec["state"]["active"], "speed");
    }

    #[test]
    fn state_bound_controls_render_current_values_and_toggle_from_the_action_list() {
        let spec = json!({
            "root":"card",
            "elements":{
                "card":{"type":"Card","props":{"title":"Preferences"},"children":["enabled","budget","progress"]},
                "enabled":{"type":"Switch","props":{"label":"Notifications","checked":{"$state":"/enabled"}},"children":[]},
                "budget":{"type":"Slider","props":{"label":"Budget","value":{"$state":"/budget"},"min":0,"max":100},"children":[]},
                "progress":{"type":"ProgressBar","props":{"progress":{"$state":"/progress"}},"children":[]}
            },
            "state":{"enabled":true,"budget":42,"progress":0.75}
        });

        let render = |spec: &serde_json::Value| {
            render_spec(spec, 72, Color::White, Color::Green, Color::DarkGray)
                .iter()
                .flat_map(|line| line.spans.iter())
                .map(|span| span.content.as_ref())
                .collect::<String>()
        };
        let initial = render(&spec);
        assert!(initial.contains("☑ Notifications"));
        assert!(initial.contains("Budget: 42"));
        assert!(initial.contains("75%"));

        let action = actions(&spec).remove(0);
        assert_eq!(action.action, "toggleState");
        let updated = apply_state_action(&spec, &action).unwrap();
        assert_eq!(updated["state"]["enabled"], false);
        assert!(render(&updated).contains("☐ Notifications"));
    }

    #[test]
    fn auto_detection_matches_interface_requests_without_matching_simple_questions() {
        assert!(wants_ui("Create a packing checklist"));
        assert!(wants_ui("show a dashboard for recent usage"));
        assert!(!wants_ui("Explain how a dashboard is rendered"));
    }
}
