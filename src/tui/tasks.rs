//! Work that runs off the UI thread: images, clipboard, endpoint tests.

use crate::db::Database;
use crate::engine;
use crate::models::Provider;
use crate::ui::picture::Picture;
use base64::Engine;
use serde_json::{Value, json};
use std::io::Write;
use std::sync::{Mutex, OnceLock};

/// Improve the prompt with the chat model (unless `raw`), then draw it.
/// Nothing is saved here: the person chooses where in the picture dialog.
pub fn draw_image(
    ctx: &engine::Context,
    selection: Option<(Provider, String)>,
    prompt: &str,
    raw: bool,
) -> Result<super::images::Drawn, String> {
    let mut used = prompt.to_string();
    if !raw
        && let Some((provider, model)) = &selection {
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
    let db = Database::open(&ctx.db_path).map_err(|e| e.to_string())?;
    let images = crate::imagegen::generate(&db, &ctx.secrets, &used, None, None, None).map_err(|e| crate::tui::app::describe_error(&e))?;
    if images.is_empty() {
        return Err("the image service returned no pictures".into());
    }
    let pictures = images
        .iter()
        .map(|image| crate::imagegen::picture_bytes(image).and_then(Picture::decode).map_err(|e| format!("{e:#}")))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(super::images::Drawn {
        pictures,
        used,
        label: crate::imagegen::engine_label(&db),
    })
}

/// Put a picture on the system clipboard as an image.
pub fn copy_image(picture: &Picture) -> Result<(), String> {
    let rgba = picture.image.to_rgba8();
    let (width, height) = rgba.dimensions();
    let mut clipboard = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    clipboard
        .set_image(arboard::ImageData {
            width: width as usize,
            height: height as usize,
            bytes: rgba.into_raw().into(),
        })
        .map_err(|e| e.to_string())
}

/// Copy to the system clipboard; over SSH or without a display, fall back to
/// the terminal's OSC 52 clipboard.
pub fn copy_to_clipboard(text: &str) -> Result<(), String> {
    // X11 and Wayland clipboards are served by the owning process, so the
    // clipboard object stays alive for the whole session.
    static CLIPBOARD: OnceLock<Mutex<Option<arboard::Clipboard>>> = OnceLock::new();
    let slot = CLIPBOARD.get_or_init(|| Mutex::new(arboard::Clipboard::new().ok()));
    if let Ok(mut guard) = slot.lock()
        && let Some(clipboard) = guard.as_mut()
            && clipboard.set_text(text.to_string()).is_ok() {
                return Ok(());
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
