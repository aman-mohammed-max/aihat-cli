use crate::api;
use crate::config::{AppConfig, is_default_local_search_url, migrate_legacy_search_settings};
use crate::db::Database;
use crate::secrets::Secrets;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

#[derive(Clone, Debug)]
pub struct SearchHit {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

#[derive(Clone, Debug)]
pub struct SearchOutput {
    pub hits: Vec<SearchHit>,
    pub pages_read: usize,
}

pub fn search(
    db: &Database,
    secrets: &Secrets,
    config: &AppConfig,
    query: &str,
) -> Result<Vec<SearchHit>> {
    Ok(search_with_metrics(db, secrets, config, query)?.hits)
}

pub fn search_with_metrics(
    db: &Database,
    secrets: &Secrets,
    config: &AppConfig,
    query: &str,
) -> Result<SearchOutput> {
    let mut settings = db.settings();
    if migrate_legacy_search_settings(&mut settings)
        && let Ok(serialized) = serde_json::to_string(&settings)
    {
        let _ = db.set_kv("settings", &serialized);
    }
    let configured_engine = settings
        .get("searchEngine")
        .and_then(Value::as_str)
        .unwrap_or(&config.search_engine)
        .trim()
        .to_lowercase();
    let configured_url = settings
        .get("searchUrl")
        .and_then(Value::as_str)
        .unwrap_or(&config.search_url);
    // Keep the retired built-in endpoint from being called if a stale setting
    // survived migration or was loaded from an older config file.
    let engine = if configured_engine == "searxng" && is_default_local_search_url(configured_url) {
        "provider".to_string()
    } else {
        configured_engine
    };
    let count = settings
        .get("searchResults")
        .and_then(Value::as_u64)
        .map(|value| value as usize)
        .unwrap_or(config.search_results)
        .clamp(1, 20);
    let safe = settings
        .get("searchSafe")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let raw = match engine.as_str() {
        "searxng" => {
            if configured_url.trim().is_empty() {
                bail!(
                    "SearXNG is selected but no URL is configured. Set `search_url` or switch `search_engine` to `provider`."
                );
            }
            json!(
                api::search_searxng_safe(configured_url, query, count, safe).with_context(
                    || format!(
                        "searching SearXNG at {}",
                        configured_url.trim_end_matches('/')
                    )
                )?
            )
        }
        "provider" => {
            let route = settings.pointer("/routes/search");
            let provider = route
                .and_then(|item| item.get("providerId"))
                .and_then(Value::as_str)
                .and_then(|id| db.provider(id).ok().flatten())
                .or(db.default_provider()?)
                .context("add an endpoint first (needed for web search)")?;
            let model = route
                .and_then(|item| item.get("model"))
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty());
            if provider_supports_google_grounding(&provider) {
                grounded_response_results(
                    api::search_google_grounded(&provider, secrets, query, count).context(
                        "FreeLLMAPI Google Search needs an available Gemini 2.5 Flash model",
                    )?,
                    query,
                )?
            } else {
                api::search_provider(&provider, secrets, query, count, model)?
            }
        }
        "brave" => {
            let key = std::env::var("BRAVE_SEARCH_API_KEY")
                .ok()
                .or_else(|| secrets.get("search.brave"))
                .context("add your Brave Search API key in Web search settings")?;
            api::search_brave(query, count, safe, &key)?
        }
        "tavily" => {
            let key = std::env::var("TAVILY_API_KEY")
                .ok()
                .or_else(|| secrets.get("search.tavily"))
                .context("add your Tavily API key in Web search settings")?;
            api::search_tavily(query, count, &key)?
        }
        other => bail!("unsupported web search engine {other:?}"),
    };
    let mut hits = normalize_hits(raw)
        .into_iter()
        .filter(|hit| !hit.url.is_empty())
        .filter(|hit| is_http_url(&hit.url))
        .collect::<Vec<_>>();
    let blocked = settings
        .get("searchBlocklist")
        .and_then(Value::as_str)
        .unwrap_or("")
        .split(|ch: char| ch.is_whitespace() || ch == ',')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    hits.retain(|hit| !blocked.iter().any(|part| hit.url.contains(part)));
    hits.truncate(count);

    let mut pages_read = 0;
    if settings
        .get("searchFullPages")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        for hit in hits.iter_mut().take(3) {
            if let Ok(text) = read_page(&hit.url)
                && text.chars().count() > 200
            {
                hit.snippet = text.chars().take(3500).collect();
                pages_read += 1;
            }
        }
    }
    Ok(SearchOutput { hits, pages_read })
}

fn provider_supports_google_grounding(provider: &crate::models::Provider) -> bool {
    provider.name.eq_ignore_ascii_case("FreeLLMAPI")
        || provider
            .base_url
            .to_ascii_lowercase()
            .contains("freellmapi")
}

fn grounded_response_results(response: Value, query: &str) -> Result<Value> {
    let message = response
        .pointer("/choices/0/message")
        .context("FreeLLMAPI returned no chat completion message")?;

    // Preserve citations if a FreeLLMAPI version exposes them as annotations.
    for key in ["results", "citations", "sources"] {
        if let Some(value) = message.get(key).filter(|value| value.is_array()) {
            return Ok(json!({"results": value}));
        }
    }

    let content = message
        .get("content")
        .and_then(Value::as_str)
        .context("FreeLLMAPI Google Search returned an empty answer")?;
    let trimmed = content.trim();
    let candidate = trimmed
        .strip_prefix("```json")
        .and_then(|text| text.strip_suffix("```"))
        .map(str::trim)
        .unwrap_or(trimmed);
    if let Ok(value) = serde_json::from_str::<Value>(candidate)
        && value
            .get("results")
            .or_else(|| value.get("sources"))
            .is_some_and(Value::is_array)
    {
        return Ok(value);
    }

    // Be tolerant of models that add a sentence before/after the requested
    // JSON while still requiring the result list itself to be structured.
    if let (Some(start), Some(end)) = (candidate.find('{'), candidate.rfind('}'))
        && start <= end
        && let Ok(value) = serde_json::from_str::<Value>(&candidate[start..=end])
        && value
            .get("results")
            .or_else(|| value.get("sources"))
            .is_some_and(Value::is_array)
    {
        return Ok(value);
    }

    bail!(
        "FreeLLMAPI grounded search returned no structured source links for {query:?}; try again or choose another search engine"
    )
}

fn normalize_hits(value: Value) -> Vec<SearchHit> {
    let list = value
        .get("results")
        .or_else(|| value.get("sources"))
        .or_else(|| value.get("data"))
        .or_else(|| value.get("items"))
        .or_else(|| value.pointer("/web/results"))
        .or_else(|| value.get("organic"))
        .and_then(Value::as_array)
        .or_else(|| value.as_array());
    list.into_iter()
        .flatten()
        .map(|item| SearchHit {
            title: item
                .get("title")
                .or_else(|| item.get("name"))
                .or_else(|| item.get("url"))
                .and_then(Value::as_str)
                .unwrap_or("Untitled")
                .to_string(),
            url: item
                .get("url")
                .or_else(|| item.get("link"))
                .or_else(|| item.get("href"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            snippet: item
                .get("snippet")
                .or_else(|| item.get("content"))
                .or_else(|| item.get("description"))
                .or_else(|| item.get("text"))
                .or_else(|| item.get("summary"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        })
        .collect()
}

fn is_http_url(url: &str) -> bool {
    url.starts_with("https://") || url.starts_with("http://")
}

fn read_page(url: &str) -> Result<String> {
    let response = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(12))
        .user_agent(concat!("AI-Hat-CLI/", env!("CARGO_PKG_VERSION")))
        .build()?
        .get(url)
        .send()
        .with_context(|| format!("reading page {url}"))?
        .error_for_status()?;
    let html = response.text()?;
    let title = html
        .split_once("<title")
        .and_then(|(_, tail)| tail.split_once('>'))
        .and_then(|(_, tail)| tail.split_once("</title>"))
        .map(|(value, _)| decode_entities(&strip_tags(value)))
        .unwrap_or_default();
    let text = decode_entities(&strip_tags(&html));
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        bail!("page has no readable text");
    }
    if title.is_empty() {
        Ok(text)
    } else {
        Ok(format!("{title}\n\n{text}"))
    }
}

pub(crate) fn strip_tags(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let mut output = String::with_capacity(html.len());
    let mut cursor = 0;
    while cursor < html.len() {
        let script_start = lower[cursor..].find("<script").map(|index| cursor + index);
        let style_start = lower[cursor..].find("<style").map(|index| cursor + index);
        let skip_start = match (script_start, style_start) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(a), None) | (None, Some(a)) => Some(a),
            (None, None) => None,
        };
        let tag_start = html[cursor..].find('<').map(|index| cursor + index);
        if let Some(skip) = skip_start.filter(|skip| tag_start.is_none_or(|tag| *skip <= tag)) {
            let closing = if Some(skip) == script_start {
                "</script>"
            } else {
                "</style>"
            };
            cursor = lower[skip..]
                .find(closing)
                .map(|index| skip + index + closing.len())
                .unwrap_or(html.len());
            output.push(' ');
        } else if let Some(start) = tag_start {
            output.push_str(&html[cursor..start]);
            cursor = html[start..]
                .find('>')
                .map(|index| start + index + 1)
                .unwrap_or(html.len());
            output.push(' ');
        } else {
            output.push_str(&html[cursor..]);
            break;
        }
    }
    output
}

pub(crate) fn decode_entities(text: &str) -> String {
    text.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
}

pub fn format_hits(hits: &[SearchHit]) -> String {
    hits.iter()
        .enumerate()
        .map(|(i, hit)| format!("[{}] {}\n{}\n{}", i + 1, hit.title, hit.url, hit.snippet))
        .collect::<Vec<_>>()
        .join("\n\n")
}

pub fn citations(hits: &[SearchHit]) -> Value {
    json!(
        hits.iter()
            .map(|hit| json!({"title":hit.title,"url":hit.url,"snippet":hit.snippet}))
            .collect::<Vec<_>>()
    )
}

pub fn tool_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "web_search",
            "description": "Search the web and return relevant pages with titles, URLs and snippets.",
            "parameters": {
                "type": "object",
                "properties": {"query": {"type":"string","description":"The search query"}},
                "required": ["query"]
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::{
        grounded_response_results, normalize_hits, provider_supports_google_grounding,
        search as run_search, strip_tags,
    };
    use crate::config::{AppConfig, Paths, is_default_local_search_url};
    use crate::db::Database;
    use crate::models::Provider;
    use crate::secrets::Secrets;
    use serde_json::{Value, json};

    #[test]
    fn normalizes_common_search_shapes_and_fields() {
        let hits = normalize_hits(
            json!({"web":{"results":[{"name":"title","href":"https://example.test","description":"summary"}]}}),
        );
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "title");
        assert_eq!(hits[0].url, "https://example.test");
        assert_eq!(hits[0].snippet, "summary");
    }

    #[test]
    fn page_text_removes_script_style_and_html_tags() {
        let text =
            strip_tags("<h1>hello</h1><script>secret()</script><style>body{}</style><p>world</p>");
        assert!(text.contains("hello"));
        assert!(text.contains("world"));
        assert!(!text.contains("secret"));
        assert!(!text.contains("body"));
    }

    #[test]
    fn recognizes_the_broken_legacy_local_search_default() {
        assert!(is_default_local_search_url("http://localhost:8080/"));
        assert!(is_default_local_search_url("http://127.0.0.1:8080"));
        assert!(is_default_local_search_url("http://localhost:8080/search"));
        assert!(is_default_local_search_url(
            "http://127.0.0.1:8080/search?format=json"
        ));
        assert!(is_default_local_search_url("http://[::1]:8080/search"));
        assert!(is_default_local_search_url(""));
        assert!(!is_default_local_search_url("https://search.example.test"));
        assert!(!is_default_local_search_url(
            "http://search.example.test:8080"
        ));
    }

    #[test]
    fn old_local_searx_settings_do_not_attempt_a_connection() {
        let dir = std::env::temp_dir().join(format!("aihat-search-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let paths = Paths {
            data_dir: dir.clone(),
            db_file: dir.join("aihat.db"),
            config_file: dir.join("config.toml"),
            keys_file: dir.join("keys.json"),
            downloads_dir: dir.join("Downloads"),
        };
        let db = Database::open(&paths.db_file).unwrap();
        db.set_kv(
            "settings",
            &json!({
                "searchUrl":"http://localhost:8080/search?format=json"
            })
            .to_string(),
        )
        .unwrap();
        let error = run_search(&db, &Secrets::new(&paths), &AppConfig::default(), "test")
            .unwrap_err()
            .to_string();
        assert!(error.contains("add an endpoint first"));
        assert!(!error.contains("localhost:8080"));
        let migrated = db.settings();
        assert_eq!(
            migrated.get("searchEngine").and_then(Value::as_str),
            Some("provider")
        );
        assert!(migrated.get("searchUrl").is_none());
        drop(db);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn detects_free_llm_api_provider_for_native_google_grounding() {
        let provider = Provider {
            id: "p1".into(),
            name: "FreeLLMAPI".into(),
            base_url: "https://api.example.test/v1".into(),
            headers: json!({}),
            timeout_ms: 15_000,
            is_default: true,
            status: "connected".into(),
            latency_ms: None,
        };
        assert!(provider_supports_google_grounding(&provider));
    }

    #[test]
    fn extracts_grounded_result_json_from_completion_content() {
        let response = json!({
            "choices": [{
                "message": {
                    "content": "```json\n{\"results\":[{\"title\":\"Rust\",\"url\":\"https://www.rust-lang.org/\",\"snippet\":\"A language empowering everyone.\"}]}\n```"
                }
            }]
        });
        let value = grounded_response_results(response, "Rust").unwrap();
        let hits = normalize_hits(value);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "Rust");
        assert_eq!(hits[0].url, "https://www.rust-lang.org/");
    }
}
