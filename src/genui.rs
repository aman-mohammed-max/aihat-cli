//! Generated UI blocks in replies. They render in the phone and desktop
//! apps only; the terminal recognises them so it can keep them out of the
//! reply text and save the spec with the message.

use serde_json::{Map, Value, json};
use std::collections::HashSet;

const MAX_SPEC_BYTES: usize = 128_000;
const MAX_ELEMENTS: usize = 48;
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

#[cfg(test)]
mod tests {
    use super::{compile_spec, normalize_spec, parse_reply};
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
}

