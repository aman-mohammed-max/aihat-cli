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
            config_file: config_dir.join("settings.json"),
            keys_file,
            downloads_dir,
        })
    }
}

/// Everything in `~/.config/aihat/settings.json`. Every field has a default,
/// so the file only needs the options someone wants to change.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    /// The accent colour, like the app's accent setting.
    pub accent: String,
    /// auto, truecolor, 256, 16 or none.
    pub colors: String,
    /// Draw boxes with + - | instead of ╭─╮.
    pub ascii: bool,
    /// 0 fills the whole terminal. A number (80 or more) keeps the chat
    /// that many columns wide, centred, like the design's 112-column window.
    pub width: u16,
    pub show_reasoning: bool,
    pub show_stats: bool,
    /// The mouse wheel scrolls the chat and buttons can be clicked. Hold
    /// shift to select text with the mouse while this is on.
    pub mouse: bool,
    pub images: ImageConfig,
    pub search: SearchConfig,
    pub keys: KeyConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ImageConfig {
    /// How pictures are previewed: auto, kitty, sixel, iterm2, blocks or off.
    pub preview: String,
    /// Where "Save" puts a new picture.
    pub save_to: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchConfig {
    /// provider, brave, tavily or searxng.
    pub engine: String,
    pub url: String,
    pub results: usize,
}

/// Key bindings for the full-screen chat. Each action takes one key like
/// "ctrl+p" or a list of them.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct KeyConfig {
    pub send: Keys,
    pub new_line: Keys,
    pub stop: Keys,
    pub switch_model: Keys,
    pub switch_prompt: Keys,
    pub past_chats: Keys,
    pub new_chat: Keys,
    pub clear_screen: Keys,
    pub toggle_reasoning: Keys,
    pub copy_reply: Keys,
    pub show_keys: Keys,
    pub quit: Keys,
}

/// One key or several: `"ctrl+p"` or `["shift+enter", "alt+enter"]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Keys(pub Vec<String>);

impl Keys {
    fn one(key: &str) -> Self {
        Self(vec![key.to_string()])
    }
}

impl Serialize for Keys {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        match self.0.as_slice() {
            [one] => serializer.serialize_str(one),
            many => many.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for Keys {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum OneOrMany {
            One(String),
            Many(Vec<String>),
        }
        Ok(match OneOrMany::deserialize(deserializer)? {
            OneOrMany::One(key) => Keys(vec![key]),
            OneOrMany::Many(keys) => Keys(keys),
        })
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            accent: "#C6F46B".into(),
            colors: "auto".into(),
            ascii: false,
            width: 0,
            show_reasoning: false,
            show_stats: true,
            mouse: true,
            images: ImageConfig::default(),
            search: SearchConfig::default(),
            keys: KeyConfig::default(),
        }
    }
}

impl Default for ImageConfig {
    fn default() -> Self {
        Self {
            preview: "auto".into(),
            save_to: "~/Downloads".into(),
        }
    }
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            engine: "provider".into(),
            url: String::new(),
            results: 6,
        }
    }
}

impl Default for KeyConfig {
    fn default() -> Self {
        Self {
            send: Keys::one("enter"),
            new_line: Keys(vec!["shift+enter".into(), "alt+enter".into(), "ctrl+j".into()]),
            stop: Keys::one("esc"),
            switch_model: Keys::one("ctrl+p"),
            switch_prompt: Keys::one("ctrl+s"),
            past_chats: Keys::one("ctrl+r"),
            new_chat: Keys::one("ctrl+n"),
            clear_screen: Keys::one("ctrl+l"),
            toggle_reasoning: Keys::one("ctrl+o"),
            copy_reply: Keys::one("ctrl+y"),
            show_keys: Keys::one("?"),
            quit: Keys::one("ctrl+c"),
        }
    }
}

/// The options of `config.toml`, read once to move them into settings.json.
#[derive(Deserialize, Default)]
#[serde(default)]
struct LegacyConfig {
    accent: Option<String>,
    no_color: bool,
    ascii: bool,
    show_stats: Option<bool>,
    show_reasoning: Option<bool>,
    search_url: Option<String>,
    search_results: Option<usize>,
    search_engine: Option<String>,
}

impl AppConfig {
    pub fn load(paths: &Paths) -> Result<Self> {
        if !paths.config_file.exists() {
            let config = Self::migrate_toml(paths).unwrap_or_default();
            config.save(paths)?;
            return Ok(config);
        }
        let raw = fs::read_to_string(&paths.config_file)
            .with_context(|| format!("reading {}", paths.config_file.display()))?;
        let mut config: Self = if raw.trim().is_empty() {
            Self::default()
        } else {
            serde_json::from_str(&raw).map_err(|error| {
                anyhow::anyhow!(
                    "{} has a mistake on line {}: {}",
                    crate::ui::text::home_relative(&paths.config_file),
                    error.line(),
                    describe_json_error(&error)
                )
            })?
        };
        if config.search.engine.eq_ignore_ascii_case("searxng")
            && is_default_local_search_url(&config.search.url)
        {
            // Older builds silently assumed that each user had a SearXNG
            // server listening on localhost:8080. Switch only that baked-in
            // default; custom SearXNG URLs remain untouched.
            config.search.engine = "provider".into();
            config.search.url.clear();
            config.save(paths)?;
        }
        Ok(config)
    }

    /// settings.json replaced config.toml: carry its options over once and
    /// keep the old file as config.toml.bak.
    fn migrate_toml(paths: &Paths) -> Option<Self> {
        let old = paths.config_file.with_file_name("config.toml");
        let raw = fs::read_to_string(&old).ok()?;
        let legacy: LegacyConfig = toml::from_str(&raw).ok()?;
        let mut config = Self::default();
        if let Some(accent) = legacy.accent {
            config.accent = accent;
        }
        if legacy.no_color {
            config.colors = "none".into();
        }
        config.ascii = legacy.ascii;
        config.show_stats = legacy.show_stats.unwrap_or(config.show_stats);
        config.show_reasoning = legacy.show_reasoning.unwrap_or(config.show_reasoning);
        config.search.engine = legacy.search_engine.unwrap_or(config.search.engine);
        config.search.url = legacy.search_url.unwrap_or_default();
        config.search.results = legacy.search_results.unwrap_or(config.search.results);
        if config.search.engine.eq_ignore_ascii_case("searxng") && is_default_local_search_url(&config.search.url) {
            config.search.engine = "provider".into();
            config.search.url.clear();
        }
        let _ = fs::rename(&old, old.with_extension("toml.bak"));
        Some(config)
    }

    pub fn save(&self, paths: &Paths) -> Result<()> {
        let mut content = serde_json::to_string_pretty(self).context("encoding settings.json")?;
        content.push('\n');
        fs::write(&paths.config_file, content)
            .with_context(|| format!("writing {}", paths.config_file.display()))
    }

    /// The folder "Save" puts pictures in, with `~` expanded.
    pub fn image_folder(&self, paths: &Paths) -> PathBuf {
        let folder = self.images.save_to.trim();
        if folder.is_empty() {
            return paths.downloads_dir.clone();
        }
        match folder.strip_prefix("~/").zip(dirs::home_dir()) {
            Some((rest, home)) => home.join(rest),
            None if folder == "~" => dirs::home_dir().unwrap_or_else(|| paths.downloads_dir.clone()),
            None => PathBuf::from(folder),
        }
    }
}

/// Old option names from config.toml, still accepted by `aihat config`.
pub fn canonical_key(key: &str) -> String {
    match key {
        "search_engine" => "search.engine".into(),
        "search_url" => "search.url".into(),
        "search_results" => "search.results".into(),
        "image_preview" => "images.preview".into(),
        other => other.to_string(),
    }
}

fn describe_json_error(error: &serde_json::Error) -> String {
    let text = error.to_string();
    // serde_json appends " at line N column M"; the line is already shown.
    match text.rfind(" at line ") {
        Some(at) => text[..at].to_string(),
        None => text,
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
            config_file: dir.join("settings.json"),
            keys_file: dir.join("keys.json"),
            downloads_dir: dir.join("Downloads"),
        }
    }

    #[test]
    fn new_config_uses_the_configured_provider_for_web_search() {
        let config = AppConfig::default();
        assert_eq!(config.search.engine, "provider");
        assert!(config.search.url.is_empty());
    }

    #[test]
    fn migrates_only_the_old_local_searxng_default() {
        let paths = test_paths();
        std::fs::write(
            &paths.config_file,
            r#"{"search": {"engine": "searxng", "url": "http://localhost:8080/"}}"#,
        )
        .unwrap();

        let config = AppConfig::load(&paths).unwrap();
        assert_eq!(config.search.engine, "provider");
        assert!(config.search.url.is_empty());

        let saved = std::fs::read_to_string(&paths.config_file).unwrap();
        assert!(saved.contains("\"engine\": \"provider\""));
        let _ = std::fs::remove_dir_all(paths.data_dir);
    }

    #[test]
    fn preserves_a_custom_searxng_endpoint() {
        let paths = test_paths();
        std::fs::write(
            &paths.config_file,
            r#"{"search": {"engine": "searxng", "url": "https://search.example.test"}}"#,
        )
        .unwrap();

        let config = AppConfig::load(&paths).unwrap();
        assert_eq!(config.search.engine, "searxng");
        assert_eq!(config.search.url, "https://search.example.test");
        let _ = std::fs::remove_dir_all(paths.data_dir);
    }

    #[test]
    fn moves_config_toml_into_settings_json() {
        let paths = test_paths();
        let old = paths.config_file.with_file_name("config.toml");
        std::fs::write(&old, "accent = \"#7DD3FC\"\nno_color = true\nsearch_engine = 'brave'\nsearch_results = 8\n").unwrap();

        let config = AppConfig::load(&paths).unwrap();
        assert_eq!(config.accent, "#7DD3FC");
        assert_eq!(config.colors, "none");
        assert_eq!(config.search.engine, "brave");
        assert_eq!(config.search.results, 8);
        assert!(paths.config_file.exists());
        assert!(!old.exists());
        assert!(old.with_extension("toml.bak").exists());
        let _ = std::fs::remove_dir_all(paths.data_dir);
    }

    #[test]
    fn keys_take_one_key_or_a_list() {
        let config: AppConfig = serde_json::from_str(r#"{"keys": {"switch_model": "ctrl+m", "new_line": ["alt+enter"]}}"#).unwrap();
        assert_eq!(config.keys.switch_model.0, vec!["ctrl+m"]);
        assert_eq!(config.keys.new_line.0, vec!["alt+enter"]);
        assert_eq!(config.keys.send.0, vec!["enter"]);
        let saved = serde_json::to_string(&config.keys).unwrap();
        assert!(saved.contains("\"switch_model\":\"ctrl+m\""));
    }

    #[test]
    fn explains_a_broken_settings_file_by_line() {
        let paths = test_paths();
        std::fs::write(&paths.config_file, "{\n  \"accent\": \"#fff\"\n  \"ascii\": true\n}").unwrap();
        let error = AppConfig::load(&paths).unwrap_err().to_string();
        assert!(error.contains("line 3"), "{error}");
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
