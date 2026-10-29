use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Provider {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub headers: serde_json::Value,
    pub timeout_ms: u64,
    pub is_default: bool,
    pub status: String,
    pub latency_ms: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Model {
    pub id: String,
    pub provider_id: String,
    pub name: String,
    pub enabled: bool,
    pub caps: serde_json::Value,
    pub defaults: serde_json::Value,
}

impl Model {
    pub fn is_chat(&self) -> bool {
        self.caps
            .get("kind")
            .and_then(|x| x.as_str())
            .is_none_or(|x| x == "chat")
    }

    pub fn supports_tools(&self) -> bool {
        self.caps
            .get("tools")
            .and_then(|x| x.as_bool())
            .unwrap_or(self.is_chat())
    }

    pub fn context_k(&self) -> Option<u64> {
        self.caps.get("contextK").and_then(|x| x.as_u64())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Chat {
    pub id: String,
    pub title: String,
    pub pinned: bool,
    pub provider_id: Option<String>,
    pub model_id: Option<String>,
    pub prompt_id: Option<String>,
    pub params: serde_json::Value,
    pub tool_ids: Vec<String>,
    pub search_mode: bool,
    pub created_at: i64,
    pub updated_at: i64,
    pub archived: bool,
    pub temporary: bool,
    pub memory_enabled: bool,
    pub title_locked: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub chat_id: String,
    pub role: String,
    pub content: String,
    pub attachments: serde_json::Value,
    pub tool_calls: serde_json::Value,
    pub citations: serde_json::Value,
    pub ui_spec: Option<serde_json::Value>,
    pub tokens: Option<u64>,
    pub ms: Option<u64>,
    pub created_at: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Prompt {
    pub id: String,
    pub name: String,
    pub body: String,
    pub tags: Vec<String>,
    pub favorite: bool,
    pub preferred_model_id: Option<String>,
    pub tool_ids: Vec<String>,
    pub variables: Vec<serde_json::Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct McpTool {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_read")]
    pub kind: String,
    #[serde(default = "default_policy")]
    pub policy: String,
    #[serde(default)]
    pub schema: serde_json::Value,
}

fn default_read() -> String {
    "read".into()
}

fn default_policy() -> String {
    "ask".into()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct McpServer {
    pub id: String,
    pub name: String,
    pub url: String,
    pub transport: String,
    pub auth: String,
    pub tools: Vec<McpTool>,
    pub enabled_by_default: bool,
    pub is_connector: bool,
    pub connected: bool,
    pub auth_header: Option<String>,
    pub auth_template: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Skill {
    pub id: String,
    pub name: String,
    pub description: String,
    pub version: String,
    pub source: String,
    pub body: String,
    pub mode: String,
    pub enabled: bool,
    pub can_run_scripts: bool,
    pub auto_update: bool,
}
