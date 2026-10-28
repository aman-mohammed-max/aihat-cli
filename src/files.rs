use anyhow::{Context, Result, bail};
use base64::Engine;
use serde_json::{Value, json};
use std::io::Read;
use std::path::Path;

#[derive(Clone, Debug)]
pub struct Attachment {
    pub name: String,
    pub kind: String,
    pub mime: String,
    pub text: Option<String>,
    pub data_uri: Option<String>,
    pub meta: String,
}

impl Attachment {
    pub fn metadata(&self) -> Value {
        json!({
            "id": uuid::Uuid::new_v4().to_string(),
            "kind": self.kind,
            "name": self.name,
            "mime": self.mime,
            "text": self.text,
            "uri": self.data_uri,
            "meta": self.meta
        })
    }
}

pub fn read(path: &Path) -> Result<Attachment> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let name = path
        .file_name()
        .and_then(|x| x.to_str())
        .unwrap_or("file")
        .to_string();
    if bytes.len() > 50 * 1024 * 1024 {
        bail!("{name}: larger than the 50 MB attachment limit");
    }
    let ext = path
        .extension()
        .and_then(|x| x.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let mime = match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "heic" => "image/heic",
        "heif" => "image/heif",
        _ => "",
    };
    if !mime.is_empty() {
        let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
        return Ok(Attachment {
            name: name.clone(),
            kind: "image".into(),
            mime: mime.into(),
            text: None,
            data_uri: Some(format!("data:{mime};base64,{encoded}")),
            meta: "image".into(),
        });
    }
    let text = String::from_utf8(bytes).with_context(|| {
        format!("{name}: this CLI currently reads text, code, Markdown, CSV and JSON files")
    })?;
    let text = text.chars().take(5_000_000).collect::<String>();
    let length = text.chars().count();
    Ok(Attachment {
        name,
        kind: "file".into(),
        mime: "text/plain".into(),
        text: Some(text),
        data_uri: None,
        meta: format!("{length} chars"),
    })
}

pub fn read_url(address: &str) -> Result<Attachment> {
    let url = reqwest::Url::parse(address).context("attachment URL is not valid")?;
    if !matches!(url.scheme(), "http" | "https") {
        bail!("attachments must use an http:// or https:// URL");
    }
    let name = url
        .path_segments()
        .and_then(|mut segments| segments.next_back())
        .filter(|name| !name.is_empty())
        .unwrap_or(url.host_str().unwrap_or("web page"))
        .to_string();
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .user_agent(concat!("AI-Hat-CLI/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let response = client.get(url.clone()).send()?.error_for_status()?;
    if response
        .content_length()
        .is_some_and(|length| length > 50 * 1024 * 1024)
    {
        bail!("{name}: larger than the 50 MB attachment limit");
    }
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let mut bytes = Vec::new();
    response
        .take(50 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .context("reading attachment response")?;
    if bytes.len() > 50 * 1024 * 1024 {
        bail!("{name}: larger than the 50 MB attachment limit");
    }

    let extension = url
        .path_segments()
        .and_then(|mut segments| segments.next_back())
        .and_then(|name| name.rsplit_once('.').map(|(_, extension)| extension))
        .unwrap_or("")
        .to_ascii_lowercase();
    let image_mime = if content_type.starts_with("image/") {
        Some(content_type.as_str())
    } else {
        match extension.as_str() {
            "png" => Some("image/png"),
            "jpg" | "jpeg" => Some("image/jpeg"),
            "gif" => Some("image/gif"),
            "webp" => Some("image/webp"),
            "bmp" => Some("image/bmp"),
            _ => None,
        }
    };
    if let Some(mime) = image_mime {
        let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
        return Ok(Attachment {
            name,
            kind: "image".into(),
            mime: mime.to_string(),
            text: None,
            data_uri: Some(format!("data:{mime};base64,{encoded}")),
            meta: "image URL".into(),
        });
    }

    if matches!(
        content_type.as_str(),
        "application/pdf" | "application/msword"
    ) || content_type.contains("officedocument")
        || matches!(
            extension.as_str(),
            "pdf" | "doc" | "docx" | "ppt" | "pptx" | "xls" | "xlsx"
        )
    {
        bail!("{name}: PDF and Office URL extraction is not available in this CLI yet");
    }
    let body = String::from_utf8(bytes)
        .with_context(|| format!("{name}: this URL is not an image or readable text document"))?;
    let text = if content_type.contains("html") || body.contains("<html") {
        let title = body
            .split_once("<title")
            .and_then(|(_, tail)| tail.split_once('>'))
            .and_then(|(_, tail)| tail.split_once("</title>"))
            .map(|(value, _)| crate::search::decode_entities(&crate::search::strip_tags(value)))
            .unwrap_or_default();
        let page = crate::search::decode_entities(&crate::search::strip_tags(&body));
        let page = page.split_whitespace().collect::<Vec<_>>().join(" ");
        if title.is_empty() {
            page
        } else {
            format!("{title}\n\n{page}")
        }
    } else {
        body
    };
    if text.trim().is_empty() {
        bail!("{name}: the URL contains no readable text");
    }
    Ok(Attachment {
        name,
        kind: "file".into(),
        mime: if content_type.is_empty() {
            "text/plain".into()
        } else {
            content_type
        },
        meta: format!("{} chars · URL", text.chars().count()),
        text: Some(text.chars().take(5_000_000).collect()),
        data_uri: Some(address.to_string()),
    })
}

pub fn message_content(prompt: &str, attachments: &[Attachment]) -> Value {
    let mut parts = Vec::new();
    parts.push(json!({"type":"text","text":prompt}));
    for attachment in attachments {
        if let Some(text) = &attachment.text {
            parts.push(
                json!({"type":"text","text":format!("\n\nFile: {}\n{}", attachment.name, text)}),
            );
        } else if let Some(uri) = &attachment.data_uri {
            parts.push(json!({"type":"image_url","image_url":{"url":uri}}));
        }
    }
    if attachments.iter().any(|a| a.kind == "image") {
        Value::Array(parts)
    } else {
        json!(format!(
            "{}{}",
            prompt,
            attachments
                .iter()
                .filter_map(|a| a
                    .text
                    .as_ref()
                    .map(|text| format!("\n\nFile: {}\n{}", a.name, text)))
                .collect::<String>()
        ))
    }
}

pub fn saved_message_content(prompt: &str, attachments: &Value) -> Value {
    let mut parts = vec![json!({"type":"text","text":prompt})];
    let mut has_image = false;
    let mut text = prompt.to_string();
    if let Some(items) = attachments.as_array() {
        for item in items {
            let name = item.get("name").and_then(Value::as_str).unwrap_or("file");
            if item.get("kind").and_then(Value::as_str) == Some("image") {
                if let Some(uri) = item.get("uri").and_then(Value::as_str) {
                    parts.push(json!({"type":"image_url","image_url":{"url":uri}}));
                    has_image = true;
                }
            } else if let Some(contents) = item.get("text").and_then(Value::as_str) {
                let file_text = format!("\n\nFile: {name}\n{contents}");
                parts.push(json!({"type":"text","text":file_text}));
                text.push_str(&file_text);
            }
        }
    }
    if has_image {
        Value::Array(parts)
    } else {
        json!(text)
    }
}

#[cfg(test)]
mod tests {
    use super::read_url;

    #[test]
    fn url_attachments_reject_non_http_schemes_before_connecting() {
        let error = read_url("file:///etc/passwd").unwrap_err().to_string();
        assert!(error.contains("http:// or https://"));
    }
}
