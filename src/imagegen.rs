use crate::api;
use crate::api::SendRetry;
use crate::db::Database;
use crate::providers;
use crate::secrets::Secrets;
use anyhow::{Context, Result, bail};
use base64::Engine;
use serde_json::Value;

pub fn generate(
    db: &Database,
    secrets: &Secrets,
    prompt: &str,
    provider_override: Option<&str>,
    model_override: Option<&str>,
    count_override: Option<usize>,
) -> Result<Vec<Value>> {
    generate_with_options(
        db,
        secrets,
        prompt,
        provider_override,
        model_override,
        count_override,
        None,
    )
}

pub fn generate_with_options(
    db: &Database,
    secrets: &Secrets,
    prompt: &str,
    provider_override: Option<&str>,
    model_override: Option<&str>,
    count_override: Option<usize>,
    aspect_override: Option<&str>,
) -> Result<Vec<Value>> {
    let settings = db.settings();
    let engine = settings
        .get("imageEngine")
        .and_then(Value::as_str)
        .unwrap_or("provider");
    let count = count_override
        .or_else(|| {
            settings
                .get("imageCount")
                .and_then(Value::as_u64)
                .map(|value| value as usize)
        })
        .unwrap_or(1)
        .clamp(1, 4);
    let (width, height) = match aspect_override
        .or_else(|| settings.get("imageAspect").and_then(Value::as_str))
        .unwrap_or("1:1")
    {
        "3:4" => (768, 1024),
        "4:3" => (1024, 768),
        "16:9" => (1344, 768),
        "9:16" => (768, 1344),
        _ => (1024, 1024),
    };
    let negative = settings
        .get("imageNegative")
        .and_then(Value::as_str)
        .unwrap_or("");
    let model = model_override
        .or_else(|| settings.get("imageModel").and_then(Value::as_str))
        .unwrap_or("");

    match engine {
        "provider" => {
            let (provider, model) =
                providers::resolve_image_provider_model(db, provider_override, model_override)?;
            let size = format!("{width}x{height}");
            api::image_generation_sized(&provider, secrets, &model, prompt, count, Some(&size))
        }
        "a1111" => api::image_a1111(
            setting_url(&settings),
            prompt,
            negative,
            model,
            count,
            width,
            height,
        ),
        "comfyui" => api::image_comfy(
            setting_url(&settings),
            settings
                .get("imageWorkflow")
                .and_then(Value::as_str)
                .unwrap_or(""),
            prompt,
            negative,
            model,
            count,
            width,
            height,
        ),
        other => bail!("unsupported image engine {other:?}; choose provider, a1111, or comfyui"),
    }
}

fn setting_url(settings: &Value) -> &str {
    settings
        .get("imageUrl")
        .and_then(Value::as_str)
        .unwrap_or("http://localhost:8188")
}

/// The bytes of one picture from an image response: inline base64 or a URL
/// to fetch.
pub fn picture_bytes(image: &Value) -> Result<Vec<u8>> {
    if let Some(data) = image.get("b64_json").and_then(Value::as_str) {
        let data = data.split_once("base64,").map(|(_, rest)| rest).unwrap_or(data);
        return base64::engine::general_purpose::STANDARD
            .decode(data.trim())
            .context("could not decode the picture");
    }
    if let Some(url) = image.get("url").and_then(Value::as_str) {
        if let Some((_, data)) = url.split_once("base64,") {
            return base64::engine::general_purpose::STANDARD
                .decode(data.trim())
                .context("could not decode the picture");
        }
        let response = api::client(120_000)?.get(url).send_retry()?.error_for_status()?;
        return Ok(response.bytes()?.to_vec());
    }
    bail!("the image service returned no picture data")
}

/// Who draws pictures: "gpt-image-1 · OpenAI", "Automatic1111" or "ComfyUI".
pub fn engine_label(db: &Database) -> String {
    let settings = db.settings();
    match settings.get("imageEngine").and_then(Value::as_str).unwrap_or("provider") {
        "a1111" => "Automatic1111".into(),
        "comfyui" => "ComfyUI".into(),
        _ => providers::resolve_image_provider_model(db, None, None)
            .map(|(provider, model)| format!("{model} · {}", provider.name))
            .unwrap_or_default(),
    }
}
