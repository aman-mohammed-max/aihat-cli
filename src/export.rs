//! Chats as Markdown or JSON files.

use crate::db::Database;
use crate::engine::split_reasoning;
use crate::models::Chat;
use anyhow::Result;
use serde_json::{Value, json};

pub fn chat_markdown(db: &Database, chat: &Chat) -> Result<String> {
    let provider = chat
        .provider_id
        .as_deref()
        .and_then(|id| db.provider(id).ok().flatten())
        .map(|p| p.name)
        .unwrap_or_default();
    let model = chat.model_id.clone().unwrap_or_default();
    let date = chrono::DateTime::from_timestamp_millis(chat.created_at)
        .map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default();
    let mut out = format!("# {}\n\n*{model} · {provider} · {date}*\n\n", chat.title);
    for message in db.messages(&chat.id)? {
        match message.role.as_str() {
            "user" => out.push_str(&format!("## You\n\n{}\n\n", message.content.trim())),
            "assistant" => {
                let (_, answer) = split_reasoning(&message.content);
                out.push_str(&format!("## {}\n\n{}\n\n", if model.is_empty() { "AI Hat" } else { &model }, answer.trim()));
                if let Some(citations) = message.citations.as_array().filter(|c| !c.is_empty()) {
                    out.push_str("Sources:\n\n");
                    for (index, citation) in citations.iter().enumerate() {
                        let url = citation.get("url").and_then(Value::as_str).unwrap_or("");
                        let title = citation.get("title").and_then(Value::as_str).unwrap_or(url);
                        out.push_str(&format!("{}. [{title}]({url})\n", index + 1));
                    }
                    out.push('\n');
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

pub fn chat_json(db: &Database, chat: &Chat) -> Result<Value> {
    let messages: Vec<Value> = db
        .messages(&chat.id)?
        .into_iter()
        .filter(|m| matches!(m.role.as_str(), "user" | "assistant"))
        .map(|m| {
            let (reasoning, answer) = split_reasoning(&m.content);
            json!({
                "role": m.role,
                "content": answer,
                "reasoning": if reasoning.is_empty() { Value::Null } else { json!(reasoning) },
                "citations": m.citations,
                "tools": m.tool_calls,
                "tokens": m.tokens,
                "ms": m.ms,
                "created_at": m.created_at,
            })
        })
        .collect();
    Ok(json!({
        "id": chat.id,
        "title": chat.title,
        "model": chat.model_id,
        "provider_id": chat.provider_id,
        "created_at": chat.created_at,
        "updated_at": chat.updated_at,
        "messages": messages,
    }))
}

/// A file-name-safe version of a chat title.
pub fn slug(title: &str) -> String {
    let mut out = String::new();
    for c in title.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-').chars().take(60).collect::<String>();
    if out.is_empty() { "chat".into() } else { out }
}

#[cfg(test)]
mod tests {
    #[test]
    fn slug_keeps_words() {
        assert_eq!(super::slug("QEMU wrapper leaking fds!"), "qemu-wrapper-leaking-fds");
        assert_eq!(super::slug("???"), "chat");
    }
}
