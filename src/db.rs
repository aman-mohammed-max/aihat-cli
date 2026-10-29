use crate::models::{Chat, McpServer, Message, Model, Prompt, Provider, Skill};
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};
use serde_json::{Value, json};
use std::fs;
use std::path::Path;
use uuid::Uuid;

const SCHEMA: &str = r#"
PRAGMA journal_mode = WAL;
CREATE TABLE IF NOT EXISTS providers (
  id TEXT PRIMARY KEY, name TEXT NOT NULL, base_url TEXT NOT NULL,
  headers TEXT NOT NULL DEFAULT '{}', timeout_ms INTEGER NOT NULL DEFAULT 60000,
  is_default INTEGER NOT NULL DEFAULT 0, status TEXT NOT NULL DEFAULT 'unknown', latency_ms INTEGER
);
CREATE TABLE IF NOT EXISTS models (
  id TEXT NOT NULL, provider_id TEXT NOT NULL, name TEXT NOT NULL, enabled INTEGER NOT NULL DEFAULT 1,
  caps TEXT NOT NULL DEFAULT '{}', defaults TEXT NOT NULL DEFAULT '{}',
  PRIMARY KEY (id, provider_id)
);
CREATE TABLE IF NOT EXISTS chats (
  id TEXT PRIMARY KEY, title TEXT NOT NULL, pinned INTEGER NOT NULL DEFAULT 0,
  provider_id TEXT, model_id TEXT, prompt_id TEXT, params TEXT NOT NULL DEFAULT '{}',
  tool_ids TEXT NOT NULL DEFAULT '[]', search_mode INTEGER NOT NULL DEFAULT 0,
  memory_enabled INTEGER NOT NULL DEFAULT 1,
  created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
  archived INTEGER NOT NULL DEFAULT 0, title_locked INTEGER NOT NULL DEFAULT 0,
  temporary INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS messages (
  id TEXT PRIMARY KEY, chat_id TEXT NOT NULL, role TEXT NOT NULL, content TEXT NOT NULL DEFAULT '',
  attachments TEXT NOT NULL DEFAULT '[]', tool_calls TEXT NOT NULL DEFAULT '[]',
  citations TEXT NOT NULL DEFAULT '[]', ui_spec TEXT, tokens INTEGER, ms INTEGER, created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS messages_chat ON messages(chat_id, created_at);
CREATE TABLE IF NOT EXISTS prompts (
  id TEXT PRIMARY KEY, name TEXT NOT NULL, body TEXT NOT NULL, tags TEXT NOT NULL DEFAULT '[]',
  favorite INTEGER NOT NULL DEFAULT 0, preferred_model_id TEXT, tool_ids TEXT NOT NULL DEFAULT '[]',
  variables TEXT NOT NULL DEFAULT '[]', updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS skills (
  id TEXT PRIMARY KEY, name TEXT NOT NULL, description TEXT NOT NULL DEFAULT '', version TEXT NOT NULL DEFAULT '1.0.0',
  source TEXT NOT NULL DEFAULT 'local', body TEXT NOT NULL DEFAULT '', files TEXT NOT NULL DEFAULT '[]',
  mode TEXT NOT NULL DEFAULT 'auto', enabled INTEGER NOT NULL DEFAULT 1, can_run_scripts INTEGER NOT NULL DEFAULT 0,
  auto_update INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS skill_files (
  skill_id TEXT NOT NULL, path TEXT NOT NULL, content TEXT NOT NULL, size INTEGER NOT NULL,
  PRIMARY KEY (skill_id, path)
);
CREATE TABLE IF NOT EXISTS mcp_servers (
  id TEXT PRIMARY KEY, name TEXT NOT NULL, url TEXT NOT NULL, transport TEXT NOT NULL DEFAULT 'http',
  auth TEXT NOT NULL DEFAULT 'none', tools TEXT NOT NULL DEFAULT '[]', enabled_by_default INTEGER NOT NULL DEFAULT 1,
  is_connector INTEGER NOT NULL DEFAULT 0, connected INTEGER NOT NULL DEFAULT 0,
  auth_header TEXT, auth_template TEXT
);
CREATE TABLE IF NOT EXISTS kv (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS commands (
  id TEXT PRIMARY KEY, name TEXT NOT NULL UNIQUE, description TEXT NOT NULL DEFAULT '',
  template TEXT NOT NULL, mode TEXT NOT NULL DEFAULT 'send', updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS rag_chunks (
  id TEXT PRIMARY KEY, chat_id TEXT NOT NULL, file_name TEXT NOT NULL, idx INTEGER NOT NULL,
  text TEXT NOT NULL, vec TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS rag_chat ON rag_chunks(chat_id);
"#;

pub struct Database {
    pub conn: Connection,
}

impl Database {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        conn.busy_timeout(std::time::Duration::from_secs(3))?;
        conn.execute_batch(SCHEMA)
            .context("creating AI Hat database schema")?;
        ensure_column(&conn, "chats", "archived", "INTEGER NOT NULL DEFAULT 0")?;
        ensure_column(&conn, "chats", "title_locked", "INTEGER NOT NULL DEFAULT 0")?;
        ensure_column(&conn, "chats", "temporary", "INTEGER NOT NULL DEFAULT 0")?;
        ensure_column(
            &conn,
            "chats",
            "memory_enabled",
            "INTEGER NOT NULL DEFAULT 1",
        )?;
        ensure_column(&conn, "mcp_servers", "auth_header", "TEXT")?;
        ensure_column(&conn, "mcp_servers", "auth_template", "TEXT")?;
        ensure_column(&conn, "skills", "auto_update", "INTEGER NOT NULL DEFAULT 0")?;
        let saved_settings: Option<String> = conn
            .query_row("SELECT value FROM kv WHERE key='settings'", [], |row| {
                row.get(0)
            })
            .optional()?;
        if let Some(raw) = saved_settings
            && let Ok(mut settings) = serde_json::from_str::<Value>(&raw)
            && crate::config::migrate_legacy_search_settings(&mut settings)
        {
            conn.execute(
                "INSERT OR REPLACE INTO kv (key,value) VALUES ('settings',?1)",
                [serde_json::to_string(&settings)?],
            )?;
        }
        let memory_migration: Option<String> = conn
            .query_row(
                "SELECT value FROM kv WHERE key='chat_memory_default_on_v1'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        if memory_migration.is_none() {
            conn.execute("UPDATE chats SET memory_enabled=1 WHERE temporary=0", [])?;
            conn.execute(
                "INSERT OR REPLACE INTO kv (key,value) VALUES ('chat_memory_default_on_v1','1')",
                [],
            )?;
        }
        Ok(Self { conn })
    }

    pub fn providers(&self) -> Result<Vec<Provider>> {
        let mut stmt = self.conn.prepare("SELECT id,name,base_url,headers,timeout_ms,is_default,status,latency_ms FROM providers ORDER BY name")?;
        let rows = stmt.query_map([], provider_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn provider(&self, name_or_id: &str) -> Result<Option<Provider>> {
        let providers = self.providers()?;
        Ok(providers
            .into_iter()
            .find(|p| p.id == name_or_id || p.name.eq_ignore_ascii_case(name_or_id)))
    }

    pub fn default_provider(&self) -> Result<Option<Provider>> {
        let providers = self.providers()?;
        Ok(providers
            .iter()
            .find(|p| p.is_default)
            .cloned()
            .or_else(|| providers.first().cloned()))
    }

    pub fn insert_provider(
        &self,
        name: &str,
        base_url: &str,
        api_headers: &Value,
        timeout_ms: u64,
    ) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        let make_default: i64 =
            self.conn
                .query_row("SELECT COUNT(*)=0 FROM providers", [], |r| r.get(0))?;
        self.conn.execute(
            "INSERT INTO providers (id,name,base_url,headers,timeout_ms,is_default) VALUES (?,?,?,?,?,?)",
            params![id, name.trim(), base_url.trim().trim_end_matches('/'), serde_json::to_string(api_headers)?, timeout_ms, make_default],
        )?;
        Ok(id)
    }

    pub fn set_provider_status(&self, id: &str, status: &str, latency: Option<u64>) -> Result<()> {
        self.conn.execute(
            "UPDATE providers SET status=?1,latency_ms=?2 WHERE id=?3",
            params![status, latency, id],
        )?;
        Ok(())
    }

    pub fn set_provider_url(&self, id: &str, url: &str) -> Result<()> {
        let url = url.trim().trim_end_matches('/');
        if url.is_empty() {
            anyhow::bail!("provider URL cannot be empty");
        }
        let updated = self.conn.execute(
            "UPDATE providers SET base_url=?1,status='unknown',latency_ms=NULL WHERE id=?2",
            params![url, id],
        )?;
        if updated == 0 {
            anyhow::bail!("provider not found");
        }
        Ok(())
    }

    pub fn update_provider(&self, id: &str, name: &str, url: &str, headers: &Value) -> Result<()> {
        let name = name.trim();
        let url = url.trim().trim_end_matches('/');
        if name.is_empty() {
            anyhow::bail!("provider name cannot be empty");
        }
        if url.is_empty() {
            anyhow::bail!("provider URL cannot be empty");
        }
        let updated = self.conn.execute(
            "UPDATE providers SET name=?1,base_url=?2,headers=?3,status='unknown',latency_ms=NULL WHERE id=?4",
            params![name, url, serde_json::to_string(headers)?, id],
        )?;
        if updated == 0 {
            anyhow::bail!("provider not found");
        }
        Ok(())
    }

    pub fn update_provider_defaults(
        &self,
        id: &str,
        headers: &Value,
        timeout_ms: u64,
    ) -> Result<()> {
        if !headers.is_object() {
            anyhow::bail!("provider headers must be a JSON object");
        }
        let updated = self.conn.execute(
            "UPDATE providers SET headers=?1,timeout_ms=?2,status='unknown',latency_ms=NULL WHERE id=?3",
            params![serde_json::to_string(headers)?, timeout_ms.max(1000), id],
        )?;
        if updated == 0 {
            anyhow::bail!("provider not found");
        }
        Ok(())
    }

    pub fn set_default_provider(&self, id: &str) -> Result<()> {
        if self.provider(id)?.is_none() {
            anyhow::bail!("provider {id:?} was not found");
        }
        self.conn
            .execute("UPDATE providers SET is_default=(id=?1)", [id])?;
        let mut settings = self.settings();
        settings["defaultProviderId"] = Value::String(id.to_string());
        let selected_model = settings
            .get("defaultModelId")
            .and_then(Value::as_str)
            .and_then(|model_id| self.model(id, model_id).ok().flatten())
            .filter(|model| model.enabled && model.is_chat())
            .map(|model| model.id)
            .or_else(|| {
                self.models(Some(id))
                    .ok()?
                    .into_iter()
                    .find(|model| model.enabled && model.is_chat())
                    .map(|model| model.id)
            });
        if let Some(model_id) = selected_model {
            settings["defaultModelId"] = Value::String(model_id);
        } else {
            settings
                .as_object_mut()
                .map(|object| object.remove("defaultModelId"));
        }
        self.set_kv("settings", &settings.to_string())?;
        Ok(())
    }

    pub fn remove_provider(&self, id: &str) -> Result<()> {
        let was_default = self
            .provider(id)?
            .is_some_and(|provider| provider.is_default)
            || self
                .settings()
                .get("defaultProviderId")
                .and_then(Value::as_str)
                == Some(id);
        self.conn
            .execute("DELETE FROM models WHERE provider_id=?1", [id])?;
        self.conn
            .execute("DELETE FROM providers WHERE id=?1", [id])?;
        if was_default {
            if let Some(provider) = self.providers()?.first() {
                self.set_default_provider(&provider.id)?;
            } else {
                let mut settings = self.settings();
                if let Some(object) = settings.as_object_mut() {
                    object.remove("defaultProviderId");
                    object.remove("defaultModelId");
                }
                self.set_kv("settings", &settings.to_string())?;
            }
        }
        Ok(())
    }

    pub fn models(&self, provider_id: Option<&str>) -> Result<Vec<Model>> {
        let mut out = Vec::new();
        if let Some(pid) = provider_id {
            let mut stmt = self.conn.prepare(
                "SELECT id,provider_id,name,enabled,caps,defaults FROM models WHERE provider_id=?1 ORDER BY name",
            )?;
            out.extend(
                stmt.query_map([pid], model_row)?
                    .collect::<rusqlite::Result<Vec<_>>>()?,
            );
        } else {
            let mut stmt = self.conn.prepare(
                "SELECT id,provider_id,name,enabled,caps,defaults FROM models ORDER BY name",
            )?;
            out.extend(
                stmt.query_map([], model_row)?
                    .collect::<rusqlite::Result<Vec<_>>>()?,
            );
        }
        Ok(out)
    }

    pub fn model(&self, provider_id: &str, model_id: &str) -> Result<Option<Model>> {
        self.conn.query_row(
            "SELECT id,provider_id,name,enabled,caps,defaults FROM models WHERE provider_id=?1 AND id=?2",
            params![provider_id, model_id],
            model_row,
        ).optional().map_err(Into::into)
    }

    pub fn set_model_defaults(
        &self,
        provider_id: &str,
        model_id: &str,
        defaults: &Value,
    ) -> Result<()> {
        if !defaults.is_object() {
            anyhow::bail!("model defaults must be a JSON object");
        }
        let updated = self.conn.execute(
            "UPDATE models SET defaults=?1 WHERE provider_id=?2 AND id=?3",
            params![defaults.to_string(), provider_id, model_id],
        )?;
        if updated == 0 {
            anyhow::bail!("model {model_id:?} was not found for provider {provider_id:?}");
        }
        Ok(())
    }

    pub fn set_model_enabled(
        &self,
        provider_id: &str,
        model_id: &str,
        enabled: bool,
    ) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE models SET enabled=?1 WHERE provider_id=?2 AND id=?3",
            params![enabled, provider_id, model_id],
        )?;
        if changed == 0 {
            anyhow::bail!("model {model_id:?} was not found for provider {provider_id:?}");
        }
        if !enabled {
            let settings = self.settings();
            let is_default = settings.get("defaultProviderId").and_then(Value::as_str)
                == Some(provider_id)
                && settings.get("defaultModelId").and_then(Value::as_str) == Some(model_id);
            if is_default {
                let replacement = self
                    .models(Some(provider_id))?
                    .into_iter()
                    .find(|model| model.enabled && model.is_chat());
                let mut next = settings;
                if let Some(replacement) = replacement {
                    next["defaultModelId"] = Value::String(replacement.id);
                } else if let Some(object) = next.as_object_mut() {
                    object.remove("defaultModelId");
                }
                self.set_kv("settings", &next.to_string())?;
            }
        }
        Ok(())
    }

    pub fn upsert_model(&self, provider_id: &str, id: &str, raw: &Value) -> Result<bool> {
        let existing = self.model(provider_id, id)?;
        let caps = classify_model(id, raw);
        if let Some(old) = existing {
            let mut merged = caps;
            if old
                .caps
                .get("custom")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                merged["custom"] = Value::Bool(true);
            }
            if old
                .caps
                .get("kindLocked")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                for key in ["kind", "kindLocked", "tools", "vision", "embeddings"] {
                    if let Some(value) = old.caps.get(key) {
                        merged[key] = value.clone();
                    }
                }
            }
            self.conn.execute(
                "UPDATE models SET caps=?1 WHERE id=?2 AND provider_id=?3",
                params![merged.to_string(), id, provider_id],
            )?;
            Ok(false)
        } else {
            self.conn.execute(
                "INSERT INTO models (id,provider_id,name,enabled,caps) VALUES (?1,?2,?1,1,?3)",
                params![id, provider_id, caps.to_string()],
            )?;
            Ok(true)
        }
    }

    pub fn remove_stale_models(&self, provider_id: &str, live_ids: &[String]) -> Result<usize> {
        if live_ids.is_empty() {
            return Ok(0);
        }
        let existing = self.models(Some(provider_id))?;
        let mut removed = 0;
        for model in existing {
            let custom = model
                .caps
                .get("custom")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if !custom && !live_ids.iter().any(|id| id == &model.id) {
                removed += self.conn.execute(
                    "DELETE FROM models WHERE id=?1 AND provider_id=?2",
                    params![model.id, provider_id],
                )?;
            }
        }
        Ok(removed)
    }

    pub fn set_default_model(&self, provider_id: &str, model_id: &str) -> Result<()> {
        let settings = self.settings();
        let mut next = settings;
        next["defaultProviderId"] = Value::String(provider_id.to_string());
        next["defaultModelId"] = Value::String(model_id.to_string());
        self.conn
            .execute("UPDATE providers SET is_default=(id=?1)", [provider_id])?;
        self.set_kv("settings", &next.to_string())?;
        Ok(())
    }

    pub fn chats(&self, include_archived: bool) -> Result<Vec<Chat>> {
        let sql = if include_archived {
            "SELECT id,title,pinned,provider_id,model_id,prompt_id,params,tool_ids,search_mode,created_at,updated_at,archived,temporary,title_locked,memory_enabled FROM chats ORDER BY pinned DESC,updated_at DESC"
        } else {
            "SELECT id,title,pinned,provider_id,model_id,prompt_id,params,tool_ids,search_mode,created_at,updated_at,archived,temporary,title_locked,memory_enabled FROM chats WHERE archived=0 ORDER BY pinned DESC,updated_at DESC"
        };
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map([], chat_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn chat(&self, id_or_prefix: &str) -> Result<Option<Chat>> {
        Ok(self
            .chats(true)?
            .into_iter()
            .find(|c| c.id == id_or_prefix || c.id.starts_with(id_or_prefix)))
    }

    pub fn create_chat(&self, init: Option<(&str, &str)>) -> Result<Chat> {
        let now = chrono::Utc::now().timestamp_millis();
        let id = Uuid::new_v4().to_string();
        let provider = self.default_provider()?;
        let settings = self.settings();
        let provider_id = init
            .map(|x| x.0.to_string())
            .or_else(|| settings["defaultProviderId"].as_str().map(str::to_string))
            .or_else(|| provider.as_ref().map(|p| p.id.clone()));
        let model_id = init
            .map(|x| x.1.to_string())
            .or_else(|| settings["defaultModelId"].as_str().map(str::to_string))
            .or_else(|| {
                provider_id.as_ref().and_then(|pid| {
                    self.models(Some(pid))
                        .ok()?
                        .into_iter()
                        .find(|m| m.enabled && m.is_chat())
                        .map(|m| m.id)
                })
            });
        let params = match (provider_id.as_deref(), model_id.as_deref()) {
            (Some(provider), Some(model)) => self.default_params(provider, model),
            _ => settings.get("defaultParams").cloned().unwrap_or_else(|| {
                json!({"temperature": 0.7, "maxTokens": 4096, "topP": 0.95, "reasoning": "off", "stream": true})
            }),
        };
        let temporary = settings
            .get("tempByDefault")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let memory_enabled = settings
            .get("memoryEnabled")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let tool_ids = self
            .mcp_servers()?
            .into_iter()
            .filter(|server| server.enabled_by_default)
            .map(|server| server.id)
            .collect::<Vec<_>>();
        self.conn.execute(
            "INSERT INTO chats (id,title,pinned,provider_id,model_id,prompt_id,params,tool_ids,search_mode,created_at,updated_at,archived,temporary,title_locked,memory_enabled) VALUES (?1,'New chat',0,?2,?3,NULL,?4,?5,0,?6,?6,0,?7,0,?8)",
            params![id, provider_id, model_id, params.to_string(), serde_json::to_string(&tool_ids)?, now, temporary, memory_enabled],
        )?;
        Ok(Chat {
            id,
            title: "New chat".into(),
            pinned: false,
            provider_id,
            model_id,
            prompt_id: None,
            params,
            tool_ids,
            search_mode: false,
            created_at: now,
            updated_at: now,
            archived: false,
            temporary,
            memory_enabled,
            title_locked: false,
        })
    }

    pub fn patch_chat_selection(&self, id: &str, provider_id: &str, model_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE chats SET provider_id=?1,model_id=?2,updated_at=?3 WHERE id=?4",
            params![provider_id, model_id, now_ms(), id],
        )?;
        Ok(())
    }

    pub fn set_chat_tools(&self, id: &str, tool_ids: &[String]) -> Result<()> {
        self.conn.execute(
            "UPDATE chats SET tool_ids=?1,updated_at=?2 WHERE id=?3",
            params![serde_json::to_string(tool_ids)?, now_ms(), id],
        )?;
        Ok(())
    }

    pub fn set_chat_memory(&self, id: &str, enabled: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE chats SET memory_enabled=?1,updated_at=?2 WHERE id=?3",
            params![enabled, now_ms(), id],
        )?;
        Ok(())
    }

    pub fn set_chat_search(&self, id: &str, enabled: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE chats SET search_mode=?1,updated_at=?2 WHERE id=?3",
            params![enabled, now_ms(), id],
        )?;
        Ok(())
    }

    pub fn set_chat_prompt(&self, id: &str, prompt_id: Option<&str>) -> Result<()> {
        self.conn.execute(
            "UPDATE chats SET prompt_id=?1,updated_at=?2 WHERE id=?3",
            params![prompt_id, now_ms(), id],
        )?;
        Ok(())
    }

    pub fn set_chat_params(&self, id: &str, chat_params: &Value) -> Result<()> {
        self.conn.execute(
            "UPDATE chats SET params=?1 WHERE id=?2",
            params![chat_params.to_string(), id],
        )?;
        Ok(())
    }

    /// Request parameters for a chat on this provider and model: the global
    /// defaults, then the provider's, then the model's.
    pub fn default_params(&self, provider_id: &str, model_id: &str) -> Value {
        let settings = self.settings();
        let mut out = settings.get("defaultParams").cloned().unwrap_or_else(|| {
            json!({"temperature": 0.7, "maxTokens": 4096, "topP": 0.95, "reasoning": "off", "stream": true})
        });
        let mut layers = Vec::new();
        if let Some(provider) = settings.pointer(&format!("/providerParams/{provider_id}")) {
            layers.push(provider.clone());
        }
        if let Ok(Some(model)) = self.model(provider_id, model_id) {
            layers.push(model.defaults);
        }
        if let Some(target) = out.as_object_mut() {
            for layer in layers {
                if let Some(layer) = layer.as_object() {
                    target.extend(layer.iter().map(|(k, v)| (k.clone(), v.clone())));
                }
            }
        }
        out
    }

    /// The chat's number in creation order: what `aihat chats` shows as `#41`.
    pub fn chat_number(&self, id: &str) -> Result<usize> {
        let number: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM chats WHERE created_at <= (SELECT created_at FROM chats WHERE id=?1)",
            [id],
            |row| row.get(0),
        )?;
        Ok(number as usize)
    }

    pub fn chat_by_number(&self, number: usize) -> Result<Option<Chat>> {
        let id: Option<String> = self
            .conn
            .query_row(
                "SELECT id FROM chats ORDER BY created_at LIMIT 1 OFFSET ?1",
                [number.saturating_sub(1) as i64],
                |row| row.get(0),
            )
            .optional()?;
        match id {
            Some(id) => self.chat(&id),
            None => Ok(None),
        }
    }

    /// Delete a message and everything after it, for "edit your last message".
    pub fn delete_messages_from(&self, chat_id: &str, created_at: i64) -> Result<()> {
        self.conn.execute(
            "DELETE FROM messages WHERE chat_id=?1 AND created_at>=?2",
            params![chat_id, created_at],
        )?;
        Ok(())
    }

    pub fn toggle_archive(&self, id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE chats SET archived=1-archived,updated_at=?1 WHERE id=?2",
            params![now_ms(), id],
        )?;
        Ok(())
    }

    pub fn rename_chat(&self, id: &str, title: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE chats SET title=?1,title_locked=1,updated_at=?2 WHERE id=?3",
            params![title, now_ms(), id],
        )?;
        Ok(())
    }

    pub fn delete_chat(&self, id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM messages WHERE chat_id=?1", [id])?;
        self.conn
            .execute("DELETE FROM rag_chunks WHERE chat_id=?1", [id])?;
        self.conn.execute("DELETE FROM chats WHERE id=?1", [id])?;
        Ok(())
    }

    pub fn messages(&self, chat_id: &str) -> Result<Vec<Message>> {
        let mut stmt = self.conn.prepare(
            "SELECT id,chat_id,role,content,attachments,tool_calls,citations,ui_spec,tokens,ms,created_at FROM messages WHERE chat_id=?1 ORDER BY created_at",
        )?;
        let rows = stmt.query_map([chat_id], message_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn save_message(
        &self,
        chat_id: &str,
        role: &str,
        content: &str,
        tokens: Option<u64>,
        ms: Option<u64>,
        attachments: &Value,
        citations: &Value,
        tool_calls: &Value,
    ) -> Result<()> {
        self.save_message_with_ui(
            chat_id,
            role,
            content,
            tokens,
            ms,
            attachments,
            citations,
            tool_calls,
            None,
        )
    }

    pub fn save_message_with_ui(
        &self,
        chat_id: &str,
        role: &str,
        content: &str,
        tokens: Option<u64>,
        ms: Option<u64>,
        attachments: &Value,
        citations: &Value,
        tool_calls: &Value,
        ui_spec: Option<&Value>,
    ) -> Result<()> {
        let now = now_ms();
        let id = Uuid::new_v4().to_string();
        self.conn.execute(
            "INSERT INTO messages (id,chat_id,role,content,attachments,tool_calls,citations,ui_spec,created_at,tokens,ms) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![id, chat_id, role, content, attachments.to_string(), tool_calls.to_string(), citations.to_string(), ui_spec.map(Value::to_string), now, tokens, ms],
        )?;
        if role == "user" {
            let chat = self.chat(chat_id)?;
            if let Some(c) = chat {
                if c.title == "New chat" && !c.title_locked {
                    let title = initial_title(content);
                    self.conn.execute(
                        "UPDATE chats SET title=?1,updated_at=?2 WHERE id=?3",
                        params![title, now, chat_id],
                    )?;
                } else {
                    self.conn.execute(
                        "UPDATE chats SET updated_at=?1 WHERE id=?2",
                        params![now, chat_id],
                    )?;
                }
            }
        } else {
            self.conn.execute(
                "UPDATE chats SET updated_at=?1 WHERE id=?2",
                params![now, chat_id],
            )?;
        }
        Ok(())
    }

    pub fn prompts(&self) -> Result<Vec<Prompt>> {
        let mut stmt = self.conn.prepare("SELECT id,name,body,tags,favorite,preferred_model_id,tool_ids,variables FROM prompts ORDER BY favorite DESC,name")?;
        let rows = stmt.query_map([], |r| {
            Ok(Prompt {
                id: r.get(0)?,
                name: r.get(1)?,
                body: r.get(2)?,
                tags: parse_json(r.get(3)?, Vec::new()),
                favorite: r.get::<_, i64>(4)? != 0,
                preferred_model_id: r.get(5)?,
                tool_ids: parse_json(r.get(6)?, Vec::new()),
                variables: parse_json(r.get(7)?, Vec::new()),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn add_prompt(&self, name: &str, body: &str) -> Result<String> {
        self.add_prompt_tagged(name, body, &[])
    }

    /// A new prompt with tags, such as "generated" for one the model wrote.
    pub fn add_prompt_tagged(&self, name: &str, body: &str, tags: &[&str]) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        self.conn.execute(
            "INSERT INTO prompts (id,name,body,tags,favorite,tool_ids,variables,updated_at) VALUES (?1,?2,?3,?4,0,'[]','[]',?5)",
            params![id, name.trim(), body, serde_json::to_string(tags)?, now_ms()],
        )?;
        Ok(id)
    }

    pub fn set_prompt_body(&self, id: &str, body: &str) -> Result<()> {
        self.conn.execute("UPDATE prompts SET body=?2, updated_at=?3 WHERE id=?1", params![id, body, now_ms()])?;
        Ok(())
    }

    pub fn delete_prompt(&self, id: &str) -> Result<()> {
        self.conn.execute("DELETE FROM prompts WHERE id=?1", [id])?;
        Ok(())
    }

    pub fn skills(&self) -> Result<Vec<Skill>> {
        let mut stmt = self.conn.prepare("SELECT id,name,description,version,source,body,mode,enabled,can_run_scripts,auto_update FROM skills ORDER BY name")?;
        let rows = stmt.query_map([], |r| {
            Ok(Skill {
                id: r.get(0)?,
                name: r.get(1)?,
                description: r.get(2)?,
                version: r.get(3)?,
                source: r.get(4)?,
                body: r.get(5)?,
                mode: r.get(6)?,
                enabled: r.get::<_, i64>(7)? != 0,
                can_run_scripts: r.get::<_, i64>(8)? != 0,
                auto_update: r.get::<_, i64>(9)? != 0,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn skill_file_count(&self, id: &str) -> Result<usize> {
        let count = self.conn.query_row(
            "SELECT COUNT(*) FROM skill_files WHERE skill_id=?1",
            [id],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(count.max(0) as usize)
    }

    pub fn save_skill_with_files(
        &mut self,
        name: &str,
        description: &str,
        body: &str,
        source: &str,
        files: &[(String, String)],
    ) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO skills (id,name,description,version,source,body,mode,enabled,can_run_scripts,auto_update) VALUES (?1,?2,?3,'1.0.0',?4,?5,'auto',1,0,0)",
            params![id, name.trim(), description, source, body],
        )?;
        save_skill_files(&tx, &id, files)?;
        tx.commit()?;
        Ok(id)
    }

    /// Skills from GitHub follow their source when this is on.
    pub fn set_skill_auto_update(&self, id: &str, on: bool) -> Result<()> {
        self.conn.execute("UPDATE skills SET auto_update=?1 WHERE id=?2", params![on as i64, id])?;
        Ok(())
    }

    pub fn set_skill_enabled(&self, id: &str, enabled: bool) -> Result<()> {
        let updated = self.conn.execute(
            "UPDATE skills SET enabled=?1 WHERE id=?2",
            params![if enabled { 1i64 } else { 0i64 }, id],
        )?;
        if updated == 0 {
            bail!("skill not found");
        }
        Ok(())
    }

    pub fn set_skill_mode(&self, id: &str, mode: &str) -> Result<()> {
        if !matches!(mode, "auto" | "always" | "onAsk") {
            bail!("skill mode must be auto, always, or onAsk");
        }
        let updated = self
            .conn
            .execute("UPDATE skills SET mode=?1 WHERE id=?2", params![mode, id])?;
        if updated == 0 {
            bail!("skill not found");
        }
        Ok(())
    }

    pub fn update_skill_source_with_files(
        &mut self,
        id: &str,
        name: &str,
        description: &str,
        body: &str,
        source: &str,
        files: &[(String, String)],
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE skills SET name=?1,description=?2,body=?3,source=?4 WHERE id=?5",
            params![name.trim(), description, body, source, id],
        )?;
        tx.execute("DELETE FROM skill_files WHERE skill_id=?1", [id])?;
        save_skill_files(&tx, id, files)?;
        tx.commit()?;
        Ok(())
    }

    pub fn remove_skill(&self, id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM skill_files WHERE skill_id=?1", [id])?;
        self.conn.execute("DELETE FROM skills WHERE id=?1", [id])?;
        Ok(())
    }

    pub fn mcp_servers(&self) -> Result<Vec<McpServer>> {
        let mut stmt = self.conn.prepare("SELECT id,name,url,transport,auth,tools,enabled_by_default,is_connector,connected,auth_header,auth_template FROM mcp_servers ORDER BY name")?;
        let rows = stmt.query_map([], |r| {
            Ok(McpServer {
                id: r.get(0)?,
                name: r.get(1)?,
                url: r.get(2)?,
                transport: r.get(3)?,
                auth: r.get(4)?,
                tools: parse_json(r.get(5)?, Vec::new()),
                enabled_by_default: r.get::<_, i64>(6)? != 0,
                is_connector: r.get::<_, i64>(7)? != 0,
                connected: r.get::<_, i64>(8)? != 0,
                auth_header: r.get(9)?,
                auth_template: r.get(10)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn remove_mcp_server(&self, id: &str) -> Result<()> {
        self.conn.execute("DELETE FROM mcp_servers WHERE id=?1", [id])?;
        self.conn
            .execute("DELETE FROM kv WHERE key=?1", [format!("mcp_always_allow_{id}")])?;
        Ok(())
    }

    /// A new server: `transport` is http (its address in `url`) or stdio
    /// (the command line in `url`); `auth` is none, token or oauth.
    pub fn add_mcp_server(&self, name: &str, url: &str, transport: &str, auth: &str) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        self.conn.execute(
            "INSERT INTO mcp_servers (id,name,url,transport,auth,tools,enabled_by_default,is_connector,connected) VALUES (?1,?2,?3,?4,?5,'[]',1,0,0)",
            params![id, name, url, transport, auth],
        )?;
        Ok(id)
    }

    /// The header a server's token goes in, and its form (`Bearer {token}`).
    pub fn set_mcp_auth_header(&self, id: &str, header: Option<&str>, template: Option<&str>) -> Result<()> {
        self.conn.execute(
            "UPDATE mcp_servers SET auth_header=?1, auth_template=?2 WHERE id=?3",
            params![header, template, id],
        )?;
        Ok(())
    }

    pub fn set_mcp_auth(&self, id: &str, auth: &str) -> Result<()> {
        self.conn.execute("UPDATE mcp_servers SET auth=?1 WHERE id=?2", params![auth, id])?;
        Ok(())
    }

    pub fn update_mcp_tools(&self, id: &str, tools: &Value) -> Result<()> {
        self.conn.execute(
            "UPDATE mcp_servers SET tools=?1,connected=1 WHERE id=?2",
            params![tools.to_string(), id],
        )?;
        Ok(())
    }

    pub fn mark_mcp_disconnected(&self, id: &str) -> Result<()> {
        self.conn
            .execute("UPDATE mcp_servers SET connected=0 WHERE id=?1", [id])?;
        Ok(())
    }

    pub fn set_kv(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO kv (key,value) VALUES (?1,?2)",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn get_kv(&self, key: &str) -> Result<Option<String>> {
        self.conn
            .query_row("SELECT value FROM kv WHERE key=?1", [key], |r| r.get(0))
            .optional()
            .map_err(Into::into)
    }

    pub fn settings(&self) -> Value {
        self.get_kv("settings")
            .ok()
            .flatten()
            .and_then(|v| serde_json::from_str(&v).ok())
            .unwrap_or_else(|| json!({}))
    }
}

fn ensure_column(conn: &Connection, table: &str, col: &str, spec: &str) -> Result<()> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let names = stmt
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if !names.iter().any(|x| x == col) {
        conn.execute(&format!("ALTER TABLE {table} ADD COLUMN {col} {spec}"), [])?;
    }
    Ok(())
}

fn save_skill_files(
    transaction: &Transaction<'_>,
    skill_id: &str,
    files: &[(String, String)],
) -> Result<()> {
    for (path, content) in files {
        transaction.execute(
            "INSERT INTO skill_files (skill_id,path,content,size) VALUES (?1,?2,?3,?4)",
            params![skill_id, path, content, content.len() as i64],
        )?;
    }
    Ok(())
}

fn parse_json<T: serde::de::DeserializeOwned>(raw: String, fallback: T) -> T {
    serde_json::from_str(&raw).unwrap_or(fallback)
}

fn provider_row(r: &Row<'_>) -> rusqlite::Result<Provider> {
    Ok(Provider {
        id: r.get(0)?,
        name: r.get(1)?,
        base_url: r.get(2)?,
        headers: parse_json(r.get(3)?, json!({})),
        timeout_ms: r.get::<_, i64>(4)?.max(0) as u64,
        is_default: r.get::<_, i64>(5)? != 0,
        status: r.get(6)?,
        latency_ms: r.get::<_, Option<i64>>(7)?.map(|x| x.max(0) as u64),
    })
}

fn model_row(r: &Row<'_>) -> rusqlite::Result<Model> {
    Ok(Model {
        id: r.get(0)?,
        provider_id: r.get(1)?,
        name: r.get(2)?,
        enabled: r.get::<_, i64>(3)? != 0,
        caps: parse_json(r.get(4)?, json!({})),
        defaults: parse_json(r.get(5)?, json!({})),
    })
}

fn chat_row(r: &Row<'_>) -> rusqlite::Result<Chat> {
    Ok(Chat {
        id: r.get(0)?,
        title: r.get(1)?,
        pinned: r.get::<_, i64>(2)? != 0,
        provider_id: r.get(3)?,
        model_id: r.get(4)?,
        prompt_id: r.get(5)?,
        params: parse_json(r.get(6)?, json!({})),
        tool_ids: parse_json(r.get(7)?, Vec::new()),
        search_mode: r.get::<_, i64>(8)? != 0,
        created_at: r.get(9)?,
        updated_at: r.get(10)?,
        archived: r.get::<_, i64>(11)? != 0,
        temporary: r.get::<_, i64>(12)? != 0,
        title_locked: r.get::<_, i64>(13)? != 0,
        memory_enabled: r.get::<_, i64>(14)? != 0,
    })
}

fn message_row(r: &Row<'_>) -> rusqlite::Result<Message> {
    Ok(Message {
        id: r.get(0)?,
        chat_id: r.get(1)?,
        role: r.get(2)?,
        content: r.get(3)?,
        attachments: parse_json(r.get(4)?, json!([])),
        tool_calls: parse_json(r.get(5)?, json!([])),
        citations: parse_json(r.get(6)?, json!([])),
        ui_spec: r
            .get::<_, Option<String>>(7)?
            .and_then(|s| serde_json::from_str(&s).ok()),
        tokens: r.get::<_, Option<i64>>(8)?.map(|x| x.max(0) as u64),
        ms: r.get::<_, Option<i64>>(9)?.map(|x| x.max(0) as u64),
        created_at: r.get(10)?,
    })
}

pub fn classify_model(id: &str, raw: &Value) -> Value {
    let lower = id.to_lowercase();
    let metadata = [
        raw.get("type").and_then(Value::as_str),
        raw.get("modality").and_then(Value::as_str),
        raw.get("task").and_then(Value::as_str),
        raw.get("pipeline_tag").and_then(Value::as_str),
        raw.get("category").and_then(Value::as_str),
        raw.get("mode").and_then(Value::as_str),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" ")
    .to_lowercase();

    let is_embedding = metadata.contains("embed")
        || lower.contains("embed")
        || ["bge", "gte", "e5", "minilm", "nomic-embed"]
            .iter()
            .any(|k| lower.contains(k));

    let is_transcription = metadata.contains("transcri")
        || metadata.contains("speech-to-text")
        || metadata.contains("asr")
        || metadata.contains("stt")
        || [
            "whisper",
            "transcribe",
            "parakeet",
            "voxtral",
            "stt",
            "nova",
        ]
        .iter()
        .any(|k| lower.contains(k));

    let is_speech = metadata.contains("text-to-speech")
        || metadata.contains("tts")
        || metadata.contains("->audio")
        || metadata.contains("audio_speech")
        || [
            "tts",
            "text-to-speech",
            "kokoro",
            "playai",
            "orpheus",
            "elevenlabs",
            "bark",
            "-speech",
            "speech-",
            "aura",
            "melotts",
            "simba",
            "speechify",
        ]
        .iter()
        .any(|k| lower.contains(k));

    let is_video = metadata.contains("video")
        || [
            "sora",
            "veo",
            "kling",
            "runway",
            "video-gen",
            "wan2",
            "cogvideo",
        ]
        .iter()
        .any(|k| lower.contains(k));

    let is_image = metadata.contains("->image")
        || metadata.contains("image-generation")
        || metadata.contains("text-to-image")
        || [
            "dall-e",
            "dalle",
            "gpt-image",
            "flux",
            "stable-diffusion",
            "stablediffusion",
            "sdxl",
            "sd-3",
            "imagen",
            "ideogram",
            "recraft",
            "kandinsky",
            "playground-v",
            "midjourney",
        ]
        .iter()
        .any(|k| lower.contains(k));

    let kind = if is_embedding {
        "embedding"
    } else if is_transcription {
        "transcription"
    } else if is_speech {
        "speech"
    } else if is_video {
        "video"
    } else if is_image {
        "image"
    } else {
        "chat"
    };

    let vision = [
        "vl",
        "vision",
        "llava",
        "4o",
        "gemini",
        "claude",
        "pixtral",
        "internvl",
        "minicpm-v",
        "omni",
        "qwen-vl",
        "qwen2-vl",
        "qwen2.5-vl",
    ]
    .iter()
    .any(|x| lower.contains(x));
    let mut caps = json!({
        "kind": kind,
        "tools": kind == "chat",
        "vision": vision,
        "embeddings": kind == "embedding"
    });
    // Context window, as OpenRouter, vLLM, LM Studio and others report it.
    let context = [
        "/context_length",
        "/max_context_length",
        "/max_model_len",
        "/context_window",
        "/top_provider/context_length",
        "/meta/n_ctx_train",
    ]
    .iter()
    .find_map(|pointer| raw.pointer(pointer).and_then(Value::as_u64))
    .filter(|tokens| *tokens >= 1024);
    if let Some(tokens) = context {
        caps["contextK"] = json!(tokens.div_ceil(1024));
    }
    caps
}

/// Rebuild API-compatible assistant tool-call and tool-result messages from
/// the tool execution log stored alongside an assistant reply.
pub fn append_assistant_history(out: &mut Vec<Value>, message: &Message, content: Value) {
    let logs = message.tool_calls.as_array();
    let calls = logs
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(|(index, log)| {
            let name = log
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty())?;
            let args = log.get("args")?;
            if log.get("status").and_then(Value::as_str) == Some("unavailable") {
                return None;
            }
            let call_id = log
                .get("tool_call_id")
                .or_else(|| log.get("call_id"))
                .and_then(Value::as_str)
                .map(str::to_owned)
                // Older logs did not persist IDs. A stable synthetic ID keeps
                // their tool result paired with the reconstructed call.
                .unwrap_or_else(|| format!("call_{}_{}", message.id, index));
            let arguments = args
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| args.to_string());
            let result = log
                .get("result")
                .map(|value| {
                    value
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| value.to_string())
                })
                .unwrap_or_default();
            Some((call_id, name.to_owned(), arguments, result))
        })
        .collect::<Vec<_>>();

    for (call_id, name, arguments, result) in calls {
        out.push(json!({
            "role": "assistant",
            "content": Value::Null,
            "tool_calls": [{
                "id": call_id,
                "type": "function",
                "function": {"name": name, "arguments": arguments}
            }]
        }));
        out.push(json!({"role":"tool", "tool_call_id":call_id, "content":result}));
    }
    if !content.as_str().unwrap_or("").is_empty() || calls_is_empty(logs) {
        out.push(json!({"role":"assistant", "content":content}));
    }
}

fn calls_is_empty(logs: Option<&Vec<Value>>) -> bool {
    !logs.into_iter().flatten().any(|log| {
        log.get("name")
            .and_then(Value::as_str)
            .is_some_and(|name| !name.is_empty())
            && log.get("args").is_some()
            && log.get("status").and_then(Value::as_str) != Some("unavailable")
    })
}

fn initial_title(content: &str) -> String {
    let cleaned = content.lines().next().unwrap_or("New chat").trim();
    let mut chars = cleaned.chars();
    let title: String = chars.by_ref().take(60).collect();
    if chars.next().is_some() {
        format!("{title}…")
    } else {
        title
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[cfg(test)]
mod tests {
    use super::{append_assistant_history, classify_model, initial_title};
    use crate::db::Database;
    use crate::models::Message;
    use rusqlite::Connection;
    use serde_json::{Value, json};

    #[test]
    fn classify_model_recognizes_embeddings_and_image_models() {
        assert_eq!(
            classify_model("text-embedding-3-small", &json!({}))["kind"],
            "embedding"
        );
        assert_eq!(classify_model("flux.1-dev", &json!({}))["kind"], "image");
        assert_eq!(classify_model("gpt-4o", &json!({}))["kind"], "chat");
        assert_eq!(classify_model("qwen3-32b", &json!({"context_length": 131072}))["contextK"], 128);
        assert_eq!(classify_model("x", &json!({"max_model_len": 32768}))["contextK"], 32);
    }

    #[test]
    fn skill_modes_are_validated_and_persisted() {
        let dir = std::env::temp_dir().join(format!("aihat-skill-mode-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut db = Database::open(&dir.join("aihat.db")).unwrap();
        let id = db
            .save_skill_with_files(
                "Reviewer",
                "Review releases",
                "Read the guide.",
                "test",
                &[],
            )
            .unwrap();

        assert_eq!(db.skills().unwrap()[0].mode, "auto");
        db.set_skill_mode(&id, "always").unwrap();
        assert_eq!(db.skills().unwrap()[0].mode, "always");
        db.set_skill_mode(&id, "onAsk").unwrap();
        assert_eq!(db.skills().unwrap()[0].mode, "onAsk");
        assert!(db.set_skill_mode(&id, "sometimes").is_err());
        assert!(db.set_skill_mode("missing", "auto").is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn provider_and_model_defaults_are_saved_and_loaded() {
        let dir =
            std::env::temp_dir().join(format!("aihat-provider-defaults-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Database::open(&dir.join("aihat.db")).unwrap();
        let provider_id = db
            .insert_provider("Local", "http://127.0.0.1:1/v1", &json!({}), 60_000)
            .unwrap();
        db.upsert_model(&provider_id, "chat-model", &json!({"id":"chat-model"}))
            .unwrap();

        db.update_provider_defaults(&provider_id, &json!({"X-Team":"research"}), 45_000)
            .unwrap();
        db.set_model_defaults(
            &provider_id,
            "chat-model",
            &json!({"temperature":0.4,"maxTokens":2048}),
        )
        .unwrap();

        let provider = db.provider(&provider_id).unwrap().unwrap();
        let model = db.model(&provider_id, "chat-model").unwrap().unwrap();
        assert_eq!(provider.timeout_ms, 45_000);
        assert_eq!(provider.headers["X-Team"], "research");
        assert_eq!(model.defaults["temperature"], 0.4);
        assert_eq!(model.defaults["maxTokens"], 2048);
        drop(db);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn initial_title_uses_first_line_and_truncates_by_characters() {
        assert_eq!(initial_title("  hello world  \nsecond"), "hello world");
        assert_eq!(
            initial_title(&"é".repeat(61)),
            format!("{}…", "é".repeat(60))
        );
    }

    #[test]
    fn history_reconstruction_pairs_tool_calls_and_results_by_id() {
        let message = Message {
            id: "message-id".into(),
            chat_id: "chat-id".into(),
            role: "assistant".into(),
            content: "Finished".into(),
            attachments: json!([]),
            tool_calls: json!([{"tool_call_id":"call-123","name":"lookup","args":{"q":"x"},"result":"found","status":"done"}]),
            citations: json!([]),
            ui_spec: None,
            tokens: None,
            ms: None,
            created_at: 0,
        };
        let mut history = Vec::<Value>::new();
        append_assistant_history(&mut history, &message, json!("Finished"));
        assert_eq!(history[0]["tool_calls"][0]["id"], "call-123");
        assert_eq!(history[1]["tool_call_id"], "call-123");
        assert_eq!(history[2]["content"], "Finished");
    }

    #[test]
    fn old_tool_logs_receive_a_stable_synthetic_call_id() {
        let message = Message {
            id: "message-id".into(),
            chat_id: "chat-id".into(),
            role: "assistant".into(),
            content: "Done".into(),
            attachments: json!([]),
            tool_calls: json!([{"name":"lookup","args":{},"result":"ok","status":"done"}]),
            citations: json!([]),
            ui_spec: None,
            tokens: None,
            ms: None,
            created_at: 0,
        };
        let mut history = Vec::new();
        append_assistant_history(&mut history, &message, json!("Done"));
        assert_eq!(history[0]["tool_calls"][0]["id"], "call_message-id_0");
        assert_eq!(
            history[1]["tool_call_id"],
            history[0]["tool_calls"][0]["id"]
        );
    }

    #[test]
    fn first_open_migrates_existing_chats_to_memory_on_once() {
        let dir =
            std::env::temp_dir().join(format!("aihat-memory-migration-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("aihat.db");
        {
            let old = Connection::open(&path).unwrap();
            old.execute_batch(
                "CREATE TABLE chats (
                    id TEXT PRIMARY KEY, title TEXT NOT NULL, pinned INTEGER NOT NULL DEFAULT 0,
                    provider_id TEXT, model_id TEXT, prompt_id TEXT, params TEXT NOT NULL DEFAULT '{}',
                    tool_ids TEXT NOT NULL DEFAULT '[]', search_mode INTEGER NOT NULL DEFAULT 0,
                    created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
                );
                INSERT INTO chats (id,title,created_at,updated_at) VALUES ('existing','Saved chat',1,1);",
            ).unwrap();
        }
        let db = Database::open(&path).unwrap();
        assert!(db.chat("existing").unwrap().unwrap().memory_enabled);
        db.set_chat_memory("existing", false).unwrap();
        drop(db);

        let db = Database::open(&path).unwrap();
        assert!(!db.chat("existing").unwrap().unwrap().memory_enabled);
        drop(db);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn first_open_migrates_the_old_local_web_search_endpoint() {
        let dir =
            std::env::temp_dir().join(format!("aihat-search-migration-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("aihat.db");
        {
            let old = Connection::open(&path).unwrap();
            old.execute_batch("CREATE TABLE kv (key TEXT PRIMARY KEY, value TEXT NOT NULL);")
                .unwrap();
            old.execute(
                "INSERT INTO kv (key,value) VALUES ('settings',?1)",
                [json!({
                    "searchEngine":"SearXNG",
                    "searchUrl":"http://localhost:8080/search?format=json",
                    "searchResults":8
                })
                .to_string()],
            )
            .unwrap();
        }

        let db = Database::open(&path).unwrap();
        let settings = db.settings();
        assert_eq!(settings["searchEngine"], "provider");
        assert!(settings.get("searchUrl").is_none());
        assert_eq!(settings["searchResults"], 8);
        drop(db);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn saved_messages_round_trip_generated_ui_specs() {
        let dir = std::env::temp_dir().join(format!("aihat-ui-message-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Database::open(&dir.join("aihat.db")).unwrap();
        let chat = db.create_chat(None).unwrap();
        let spec = json!({"root":"card","elements":{"card":{"type":"Card"}}});
        db.save_message_with_ui(
            &chat.id,
            "assistant",
            "Here is your checklist.",
            Some(12),
            Some(50),
            &json!([]),
            &json!([]),
            &json!([]),
            Some(&spec),
        )
        .unwrap();
        let saved = db.messages(&chat.id).unwrap();
        assert_eq!(saved[0].ui_spec.as_ref(), Some(&spec));
        drop(db);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
