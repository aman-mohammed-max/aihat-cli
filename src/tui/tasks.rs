//! Work that runs off the UI thread: images, clipboard, endpoint tests.

use crate::db::Database;
use crate::engine;
use crate::models::Provider;
use base64::Engine;
use serde_json::{Value, json};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Improve the prompt with the chat model (unless `raw`), then draw it.
pub fn draw_image(
    ctx: &engine::Context,
    selection: Option<(Provider, String)>,
    prompt: &str,
    raw: bool,
    downloads: &Path,
) -> Result<(Vec<PathBuf>, String), String> {
    let mut used = prompt.to_string();
    if !raw {
        if let Some((provider, model)) = &selection {
            let request = vec![
                json!({"role": "system", "content": "You write prompts for image models. Rewrite the idea as one vivid, specific image prompt under 80 words: subject, setting, style, lighting, composition. Reply with the prompt only."}),
                json!({"role": "user", "content": prompt}),
            ];
            if let Ok(reply) = crate::api::complete(provider, &ctx.secrets, model, &request, &json!({"temperature": 0.8})) {
                let improved = reply.content.trim().trim_matches('"').to_string();
                if !improved.is_empty() {
                    used = improved;
                }
            }
        }
    }
    let db = Database::open(&ctx.db_path).map_err(|e| e.to_string())?;
    let images = crate::imagegen::generate(&db, &ctx.secrets, &used, None, None, None).map_err(|e| format!("{e:#}"))?;
    if images.is_empty() {
        return Err("the image service returned no pictures".into());
    }
    std::fs::create_dir_all(downloads).map_err(|e| e.to_string())?;
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let mut files = Vec::new();
    for (index, image) in images.iter().enumerate() {
        let bytes = image_bytes(image)?;
        let path = downloads.join(format!("aihat-{stamp}-{}.png", index + 1));
        std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
        files.push(path);
    }
    Ok((files, used))
}

fn image_bytes(image: &Value) -> Result<Vec<u8>, String> {
    if let Some(data) = image.get("b64_json").and_then(Value::as_str) {
        let data = data.split_once("base64,").map(|(_, rest)| rest).unwrap_or(data);
        return base64::engine::general_purpose::STANDARD
            .decode(data.trim())
            .map_err(|e| format!("could not decode the image: {e}"));
    }
    if let Some(url) = image.get("url").and_then(Value::as_str) {
        let response = crate::api::client(120_000)
            .map_err(|e| e.to_string())?
            .get(url)
            .send()
            .map_err(|e| e.to_string())?;
        return response.bytes().map(|b| b.to_vec()).map_err(|e| e.to_string());
    }
    Err("the image service returned no image data".into())
}

/// Copy to the system clipboard; over SSH or without a display, fall back to
/// the terminal's OSC 52 clipboard.
pub fn copy_to_clipboard(text: &str) -> Result<(), String> {
    // X11 and Wayland clipboards are served by the owning process, so the
    // clipboard object stays alive for the whole session.
    static CLIPBOARD: OnceLock<Mutex<Option<arboard::Clipboard>>> = OnceLock::new();
    let slot = CLIPBOARD.get_or_init(|| Mutex::new(arboard::Clipboard::new().ok()));
    if let Ok(mut guard) = slot.lock() {
        if let Some(clipboard) = guard.as_mut() {
            if clipboard.set_text(text.to_string()).is_ok() {
                return Ok(());
            }
        }
    }
    let encoded = base64::engine::general_purpose::STANDARD.encode(text);
    let mut stdout = std::io::stdout();
    write!(stdout, "\x1b]52;c;{encoded}\x07").map_err(|e| e.to_string())?;
    stdout.flush().map_err(|e| e.to_string())
}

/// Try an endpoint that is not saved yet: list its models with the given key.
pub fn test_endpoint(url: &str, key: &str, headers: &Value) -> Result<(u64, Vec<String>), String> {
    let mut all_headers = headers.as_object().cloned().unwrap_or_default();
    if !key.trim().is_empty() {
        all_headers.insert("Authorization".into(), json!(format!("Bearer {}", key.trim())));
    }
    let provider = Provider {
        id: "unsaved".into(),
        name: "this endpoint".into(),
        base_url: url.trim().trim_end_matches('/').to_string(),
        headers: Value::Object(all_headers),
        timeout_ms: 15_000,
        is_default: false,
        status: "unknown".into(),
        latency_ms: None,
    };
    let secrets = crate::secrets::Secrets::detached();
    match crate::api::list_models(&provider, &secrets) {
        Ok((models, latency)) => Ok((
            latency,
            models
                .iter()
                .filter_map(|m| m.get("id").and_then(Value::as_str).map(str::to_owned))
                .collect(),
        )),
        Err(error) => Err(crate::tui::app::describe_error(&error)),
    }
}

/// Test a saved provider and record its status, like the app does.
pub fn test_saved(ctx: &engine::Context, provider_id: &str) -> Result<(u64, usize), String> {
    let db = Database::open(&ctx.db_path).map_err(|e| e.to_string())?;
    let provider = db
        .provider(provider_id)
        .map_err(|e| e.to_string())?
        .ok_or("the provider was removed")?;
    let summary = crate::providers::refresh_one(&db, &ctx.secrets, Some(&provider.id));
    let fresh = db.provider(provider_id).ok().flatten();
    match summary {
        Ok(summary) if summary.failures.is_empty() => Ok((
            fresh.and_then(|p| p.latency_ms).unwrap_or(0),
            summary.model_count,
        )),
        Ok(summary) => Err(summary.failures.join("; ")),
        Err(error) => Err(crate::tui::app::describe_error(&error)),
    }
}
