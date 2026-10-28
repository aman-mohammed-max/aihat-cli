use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

const APP_ID: &str = "com.amanm.aihat";

#[derive(Clone, Debug)]
pub struct Paths {
    pub data_dir: PathBuf,
    pub db_file: PathBuf,
    pub config_file: PathBuf,
    pub keys_file: PathBuf,
    pub downloads_dir: PathBuf,
}

impl Paths {
    pub fn resolve(db_override: Option<PathBuf>) -> Result<Self> {
        let config_dir = dirs::config_dir()
            .unwrap_or_else(|| {
                dirs::home_dir()
                    .unwrap_or_else(|| PathBuf::from("."))
                    .join(".config")
            })
            .join("aihat");

        let env_data = std::env::var_os("AIHAT_DATA_DIR").map(PathBuf::from);
        let override_db =
            db_override.or_else(|| std::env::var_os("AIHAT_DB_PATH").map(PathBuf::from));

        let data_dir = if let Some(dir) = env_data {
            dir
        } else if let Some(db) = &override_db {
            db.parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."))
                .to_path_buf()
        } else {
            dirs::data_local_dir()
                .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")))
                .join(APP_ID)
        };
        let db_file = override_db.unwrap_or_else(|| data_dir.join("aihat.db"));
        let keys_file = data_dir.join("keys.json");
        let downloads_dir = dirs::download_dir().unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("Downloads")
        });

        fs::create_dir_all(&config_dir)
            .with_context(|| format!("creating {}", config_dir.display()))?;
        fs::create_dir_all(&data_dir)
            .with_context(|| format!("creating {}", data_dir.display()))?;
        Ok(Self {
            data_dir,
            db_file,
            config_file: config_dir.join("config.toml"),
            keys_file,
            downloads_dir,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub theme: String,
    pub accent: String,
    pub no_color: bool,
    pub ascii: bool,
    pub show_stats: bool,
    pub show_reasoning: bool,
    pub search_url: String,
    pub search_results: usize,
    pub search_engine: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            theme: "dark".into(),
            accent: "#C6F46B".into(),
            no_color: false,
            ascii: false,
            show_stats: true,
            show_reasoning: false,
            search_url: String::new(),
            search_results: 6,
            search_engine: "provider".into(),
        }
    }
}

impl AppConfig {
    pub fn load(paths: &Paths) -> Result<Self> {
        if !paths.config_file.exists() {
            let config = Self::default();
            config.save(paths)?;
            return Ok(config);
        }
        let raw = fs::read_to_string(&paths.config_file)
            .with_context(|| format!("reading {}", paths.config_file.display()))?;
        let mut config: Self = toml::from_str(&raw).context("parsing config.toml")?;
        if config.search_engine.eq_ignore_ascii_case("searxng")
            && is_default_local_search_url(&config.search_url)
        {
            // Older builds silently assumed that each user had a SearXNG
            // server listening on localhost:8080. Switch only that baked-in
            // default; custom SearXNG URLs remain untouched.
            config.search_engine = "provider".into();
            config.search_url.clear();
            config.save(paths)?;
        }
        Ok(config)
    }

    pub fn save(&self, paths: &Paths) -> Result<()> {
        let content = toml::to_string_pretty(self).context("encoding config.toml")?;
        fs::write(&paths.config_file, content)
            .with_context(|| format!("writing {}", paths.config_file.display()))
    }
}

pub(crate) fn is_default_local_search_url(url: &str) -> bool {
    let normalized = url.trim();
    if normalized.is_empty() {
        return true;
    }
    let Some((scheme, rest)) = normalized.split_once("://") else {
        return false;
    };
    if !scheme.eq_ignore_ascii_case("http") {
        return false;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    matches!(
        authority.to_ascii_lowercase().as_str(),
        "localhost:8080" | "127.0.0.1:8080" | "[::1]:8080"
    )
}

pub(crate) fn migrate_legacy_search_settings(settings: &mut serde_json::Value) -> bool {
    let engine = settings
        .get("searchEngine")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .trim();
    let url = settings
        .get("searchUrl")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    // Older versions stored this local SearXNG URL in settings. It may remain
    // there when the engine field is omitted, so migrate that URL too. An empty
    // URL migrates only when SearXNG was explicitly selected.
    let stale_local_endpoint = (engine.is_empty() || engine.eq_ignore_ascii_case("searxng"))
        && !url.trim().is_empty()
        && is_default_local_search_url(url);
    let stale_empty_searxng =
        engine.eq_ignore_ascii_case("searxng") && is_default_local_search_url(url);
    if !stale_local_endpoint && !stale_empty_searxng {
        return false;
    }
    let Some(object) = settings.as_object_mut() else {
        return false;
    };
    object.insert(
        "searchEngine".into(),
        serde_json::Value::String("provider".into()),
    );
    object.remove("searchUrl");
    true
}

#[cfg(test)]
mod tests {
    use super::{AppConfig, Paths, migrate_legacy_search_settings};
    use serde_json::json;

    fn test_paths() -> Paths {
        let dir = std::env::temp_dir().join(format!("aihat-config-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        Paths {
            data_dir: dir.clone(),
            db_file: dir.join("aihat.db"),
            config_file: dir.join("config.toml"),
            keys_file: dir.join("keys.json"),
            downloads_dir: dir.join("Downloads"),
        }
    }

    #[test]
    fn new_config_uses_the_configured_provider_for_web_search() {
        let config = AppConfig::default();
        assert_eq!(config.search_engine, "provider");
        assert!(config.search_url.is_empty());
    }

    #[test]
    fn migrates_only_the_old_local_searxng_default() {
        let paths = test_paths();
        std::fs::write(
            &paths.config_file,
            "search_engine = 'searxng'\nsearch_url = 'http://localhost:8080/'\n",
        )
        .unwrap();

        let config = AppConfig::load(&paths).unwrap();
        assert_eq!(config.search_engine, "provider");
        assert!(config.search_url.is_empty());

        let saved = std::fs::read_to_string(&paths.config_file).unwrap();
        assert!(saved.contains("search_engine = \"provider\""));
        let _ = std::fs::remove_dir_all(paths.data_dir);
    }

    #[test]
    fn migrates_old_local_searxng_search_path_urls() {
        let paths = test_paths();
        std::fs::write(
            &paths.config_file,
            "search_engine = 'searxng'\nsearch_url = 'http://localhost:8080/search'\n",
        )
        .unwrap();

        let config = AppConfig::load(&paths).unwrap();

        assert_eq!(config.search_engine, "provider");
        assert!(config.search_url.is_empty());
        let _ = std::fs::remove_dir_all(paths.data_dir);
    }

    #[test]
    fn preserves_a_custom_searxng_endpoint() {
        let paths = test_paths();
        std::fs::write(
            &paths.config_file,
            "search_engine = 'searxng'\nsearch_url = 'https://search.example.test'\n",
        )
        .unwrap();

        let config = AppConfig::load(&paths).unwrap();
        assert_eq!(config.search_engine, "searxng");
        assert_eq!(config.search_url, "https://search.example.test");
        let _ = std::fs::remove_dir_all(paths.data_dir);
    }

    #[test]
    fn migrates_legacy_search_settings_without_changing_custom_urls() {
        let mut legacy = json!({
            "searchEngine":" SearXNG ",
            "searchUrl":"http://localhost:8080/search?format=json",
            "searchResults":8
        });
        assert!(migrate_legacy_search_settings(&mut legacy));
        assert_eq!(legacy["searchEngine"], "provider");
        assert!(legacy.get("searchUrl").is_none());
        assert_eq!(legacy["searchResults"], 8);

        let mut custom = json!({
            "searchEngine":"searxng",
            "searchUrl":"https://search.example.test"
        });
        assert!(!migrate_legacy_search_settings(&mut custom));
        assert_eq!(custom["searchEngine"], "searxng");
        assert_eq!(custom["searchUrl"], "https://search.example.test");
    }

    #[test]
    fn migrates_local_search_url_even_when_the_old_engine_field_is_missing() {
        let mut settings = json!({
            "searchUrl":"http://localhost:8080/search?format=json",
            "searchResults":8
        });

        assert!(migrate_legacy_search_settings(&mut settings));
        assert_eq!(settings["searchEngine"], "provider");
        assert!(settings.get("searchUrl").is_none());
        assert_eq!(settings["searchResults"], 8);
    }
}
