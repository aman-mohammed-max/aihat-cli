use crate::api;
use crate::db::Database;
use crate::models::Provider;
use crate::secrets::Secrets;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::time::Instant;

pub const PRESETS: &[(&str, &str)] = &[
    ("vLLM", "http://localhost:8000/v1"),
    ("Ollama", "http://localhost:11434/v1"),
    ("LM Studio", "http://localhost:1234/v1"),
    ("OpenRouter", "https://openrouter.ai/api/v1"),
    ("Groq", "https://api.groq.com/openai/v1"),
    (
        "FreeLLMAPI",
        "https://freellmapi-production-5248.up.railway.app/v1",
    ),
    ("Custom", ""),
];

pub fn preset_url(name: &str) -> Option<&'static str> {
    PRESETS
        .iter()
        .find(|(label, _)| label.eq_ignore_ascii_case(name))
        .map(|x| x.1)
}

pub fn resolve_provider_model(
    db: &Database,
    provider_name: Option<&str>,
    model_name: Option<&str>,
) -> Result<(Provider, String)> {
    let provider = if let Some(name) = provider_name {
        db.provider(name)?
            .with_context(|| format!("no provider named {name:?}; run aihat providers"))?
    } else {
        db.default_provider()?
            .context("add an endpoint first: aihat providers add")?
    };
    let model = if let Some(model) = model_name {
        model.to_string()
    } else {
        let settings = db.settings();
        let configured = settings.get("defaultModelId").and_then(Value::as_str);
        let local = db.models(Some(&provider.id))?;
        local
            .iter()
            .find(|m| m.enabled && m.is_chat() && Some(m.id.as_str()) == configured)
            .or_else(|| {
                provider
                    .name
                    .eq_ignore_ascii_case("FreeLLMAPI")
                    .then(|| {
                        local
                            .iter()
                            .find(|m| m.enabled && m.is_chat() && m.id.eq_ignore_ascii_case("auto"))
                    })
                    .flatten()
            })
            .or_else(|| local.iter().find(|m| m.enabled && m.is_chat()))
            .map(|m| m.id.clone())
            .context(format!(
                "no chat model listed for {}. Run: aihat models refresh {}",
                provider.name, provider.name
            ))?
    };
    Ok((provider, model))
}

pub fn resolve_image_provider_model(
    db: &Database,
    provider_name: Option<&str>,
    model_name: Option<&str>,
) -> Result<(Provider, String)> {
    let settings = db.settings();
    let route = settings.pointer("/routes/images");
    let provider_name = provider_name.or_else(|| {
        route
            .and_then(|r| r.get("providerId"))
            .and_then(Value::as_str)
    });
    let configured_model = model_name.or_else(|| {
        route
            .and_then(|r| r.get("model"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
    });
    let provider = if let Some(name) = provider_name {
        db.provider(name)?
            .with_context(|| format!("no provider named {name:?}; run aihat providers"))?
    } else {
        db.default_provider()?
            .context("add an endpoint first: aihat providers add")?
    };
    let model = if let Some(model) = configured_model {
        model.to_string()
    } else {
        db.models(Some(&provider.id))?
            .into_iter()
            .find(|item| {
                item.enabled && item.caps.get("kind").and_then(Value::as_str) == Some("image")
            })
            .map(|item| item.id)
            .unwrap_or_default()
    };
    Ok((provider, model))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefreshSummary {
    pub connected: usize,
    pub provider_count: usize,
    pub added: usize,
    pub model_count: usize,
    pub failures: Vec<String>,
}

impl RefreshSummary {
    pub fn marker(&self) -> &'static str {
        if self.failures.is_empty() {
            "✓"
        } else {
            "⚠"
        }
    }

    pub fn describe(&self) -> String {
        let mut text = format!(
            "{}/{} providers · {} models · {} new",
            self.connected, self.provider_count, self.model_count, self.added
        );
        if !self.failures.is_empty() {
            text.push_str(" · failed: ");
            text.push_str(&self.failures.join("; "));
        }
        text
    }
}

pub fn refresh_one(db: &Database, secrets: &Secrets, name: Option<&str>) -> Result<RefreshSummary> {
    let providers = if let Some(name) = name {
        vec![
            db.provider(name)?
                .with_context(|| format!("no provider named {name:?}"))?,
        ]
    } else {
        db.providers()?
    };
    if providers.is_empty() {
        bail!("no providers configured; run aihat providers add");
    }
    let mut connected = 0;
    let mut added = 0;
    let mut model_total = 0;
    let provider_count = providers.len();
    let mut failures = Vec::new();
    for provider in providers {
        let started = Instant::now();
        match api::list_models(&provider, secrets) {
            Ok((models, latency)) => {
                let mut ids = Vec::new();
                for raw in &models {
                    let Some(id) = raw.get("id").and_then(Value::as_str) else {
                        continue;
                    };
                    ids.push(id.to_string());
                    if db.upsert_model(&provider.id, id, raw)? {
                        added += 1;
                    }
                }
                db.remove_stale_models(&provider.id, &ids)?;
                let latency = if latency == 0 {
                    started.elapsed().as_millis() as u64
                } else {
                    latency
                };
                db.set_provider_status(&provider.id, "online", Some(latency))?;
                model_total += ids.len();
                connected += 1;
            }
            Err(error) => {
                let text = error.to_string();
                let rejected = text.contains("401") || text.contains("403");
                db.set_provider_status(
                    &provider.id,
                    if rejected { "rejected" } else { "offline" },
                    None,
                )?;
                failures.push(refresh_failure(&provider, &error));
            }
        }
    }
    summarize_refresh(
        connected,
        provider_count,
        added,
        model_total,
        failures,
        name,
    )
}

fn summarize_refresh(
    connected: usize,
    provider_count: usize,
    added: usize,
    model_count: usize,
    failures: Vec<String>,
    selected_provider: Option<&str>,
) -> Result<RefreshSummary> {
    if !failures.is_empty() && (connected == 0 || selected_provider.is_some()) {
        bail!("model refresh failed: {}", failures.join("; "));
    }
    Ok(RefreshSummary {
        connected,
        provider_count,
        added,
        model_count,
        failures,
    })
}

fn refresh_failure(provider: &crate::models::Provider, error: &anyhow::Error) -> String {
    if provider.name.eq_ignore_ascii_case("FreeLLMAPI") {
        let detail = error.to_string();
        let lower = detail.to_ascii_lowercase();
        if lower.contains("401") || lower.contains("403") || lower.contains("unauthorized") {
            return format!(
                "FreeLLMAPI rejected the key at {}. Set the unified API key from its dashboard",
                provider.base_url
            );
        }
        if error
            .downcast_ref::<reqwest::Error>()
            .is_some_and(reqwest::Error::is_connect)
        {
            let advice = if is_local_endpoint(&provider.base_url) {
                "Start the FreeLLMAPI server or update the provider URL"
            } else {
                "Check that this hosted deployment is running and that its public URL is correct"
            };
            return format!(
                "cannot connect to FreeLLMAPI at {}: {:#}. {advice}",
                provider.base_url, error
            );
        }
        return format!("FreeLLMAPI at {}: {detail}", provider.base_url);
    }
    format!("{}: {error:#}", provider.name)
}

fn is_local_endpoint(url: &str) -> bool {
    let url = url.to_ascii_lowercase();
    ["localhost", "127.0.0.1", "[::1]"]
        .iter()
        .any(|host| url.contains(host))
}

#[cfg(test)]
mod tests {
    use super::{is_local_endpoint, preset_url, summarize_refresh};
    use crate::db::Database;
    use serde_json::json;

    #[test]
    fn endpoint_hint_distinguishes_local_and_hosted_servers() {
        assert!(is_local_endpoint("http://localhost:3001/v1"));
        assert!(is_local_endpoint("http://127.0.0.1:3001/v1"));
        assert!(is_local_endpoint("http://[::1]:3001/v1"));
        assert!(!is_local_endpoint("https://freellmapi.example.com/v1"));
    }

    #[test]
    fn freellmapi_preset_uses_the_hosted_service_url() {
        assert_eq!(
            preset_url("FreeLLMAPI"),
            Some("https://freellmapi-production-5248.up.railway.app/v1")
        );
    }

    #[test]
    fn refresh_summary_reports_partial_provider_failures() {
        let summary =
            summarize_refresh(2, 3, 4, 19, vec!["FreeLLMAPI: unreachable".into()], None).unwrap();
        let text = summary.describe();
        assert!(text.contains("2/3 providers"));
        assert!(text.contains("19 models"));
        assert!(text.contains("FreeLLMAPI: unreachable"));
        assert_eq!(summary.marker(), "⚠");
        assert!(summarize_refresh(0, 3, 0, 0, vec!["all offline".into()], None).is_err());
        assert!(summarize_refresh(0, 1, 0, 0, vec!["offline".into()], Some("Local")).is_err());
    }

    #[test]
    fn saved_hosted_freellmapi_url_is_preserved() {
        let dir =
            std::env::temp_dir().join(format!("aihat-provider-url-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Database::open(&dir.join("aihat.db")).unwrap();
        let id = db
            .insert_provider(
                "FreeLLMAPI",
                "https://freellmapi-production-5248.up.railway.app/v1",
                &json!({}),
                1000,
            )
            .unwrap();
        let hosted_url = "https://freellmapi-production-5248.up.railway.app/v1";
        db.set_provider_url(&id, hosted_url).unwrap();
        assert_eq!(db.provider(&id).unwrap().unwrap().base_url, hosted_url);
        drop(db);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

pub fn add_model(
    db: &Database,
    provider: &Provider,
    id: &str,
    name: Option<&str>,
    kind: &str,
) -> Result<()> {
    if id.trim().is_empty() {
        bail!("enter a model id exactly as the provider names it");
    }
    let caps = json!({
        "kind": kind,
        "kindLocked": true,
        "custom": true,
        "tools": kind == "chat",
        "vision": false,
        "embeddings": kind == "embedding"
    });
    db.conn.execute(
        "INSERT INTO models (id,provider_id,name,enabled,caps,defaults) VALUES (?1,?2,?3,1,?4,'{}')",
        rusqlite::params![id.trim(), provider.id, name.unwrap_or(id).trim(), caps.to_string()],
    ).context("saving custom model")?;
    Ok(())
}
