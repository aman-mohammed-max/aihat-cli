use crate::db::Database;
use anyhow::{Context, Result, bail};
use rusqlite::types::Value as SqlValue;
use serde_json::{Map, Value, json};
use std::collections::HashSet;
use std::path::Path;

// Matches the Expo backup table names, plus CLI-owned commands and file indexes.
const TABLES: &[&str] = &[
    "chats",
    "messages",
    "prompts",
    "skills",
    "skill_files",
    "mcp_servers",
    "providers",
    "models",
    "commands",
    "rag_chunks",
];

pub fn export(db: &Database) -> Result<Value> {
    let mut data = Map::new();
    for table in TABLES {
        data.insert((*table).to_owned(), read_table(db, table)?);
    }
    Ok(json!({
        "app": "ai-chat",
        "version": 1,
        "exportedAt": chrono::Utc::now().to_rfc3339(),
        "data": data,
        "settings": db.settings()
    }))
}

pub fn export_to(db: &Database, path: &Path) -> Result<usize> {
    let payload = serde_json::to_vec_pretty(&export(db)?)?;
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating backup directory {}", parent.display()))?;
    }
    std::fs::write(path, &payload).with_context(|| format!("writing backup {}", path.display()))?;
    Ok(payload.len())
}

pub fn import_from(db: &mut Database, path: &Path) -> Result<usize> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading backup {}", path.display()))?;
    let payload: Value = serde_json::from_str(&raw).context("parsing backup JSON")?;
    if payload.get("app").and_then(Value::as_str) != Some("ai-chat") {
        bail!("This is not an AI Hat backup.");
    }
    let Some(data) = payload.get("data").and_then(Value::as_object) else {
        bail!("The backup does not contain a data object.");
    };

    let table_names = TABLES.iter().copied().collect::<HashSet<_>>();
    let tx = db.conn.transaction().context("starting backup restore")?;
    let mut imported_tables = 0;
    for (table, rows) in data {
        if !table_names.contains(table.as_str()) {
            continue;
        }
        let Some(rows) = rows.as_array() else {
            continue;
        };
        let columns = table_columns(&tx, table)?;
        for row in rows {
            let Some(row) = row.as_object() else {
                continue;
            };
            let keys = row
                .keys()
                .filter(|key| columns.contains(key.as_str()))
                .cloned()
                .collect::<Vec<_>>();
            if keys.is_empty() {
                continue;
            }
            let quoted_columns = keys
                .iter()
                .map(|key| format!("\"{key}\""))
                .collect::<Vec<_>>()
                .join(",");
            let placeholders = (1..=keys.len())
                .map(|index| format!("?{index}"))
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "INSERT OR REPLACE INTO \"{table}\" ({quoted_columns}) VALUES ({placeholders})"
            );
            let values = keys
                .iter()
                .map(|key| sql_value(&row[key]))
                .collect::<Vec<_>>();
            tx.execute(&sql, rusqlite::params_from_iter(values))
                .with_context(|| format!("restoring row in {table}"))?;
        }
        imported_tables += 1;
    }
    if let Some(settings) = payload.get("settings") {
        tx.execute(
            "INSERT OR REPLACE INTO kv (key,value) VALUES ('settings',?1)",
            [settings.to_string()],
        )?;
    }
    tx.commit().context("committing backup restore")?;
    Ok(imported_tables)
}

fn read_table(db: &Database, table: &str) -> Result<Value> {
    let mut statement = db
        .conn
        .prepare(&format!("SELECT * FROM \"{table}\""))
        .with_context(|| format!("reading {table}"))?;
    let names = statement
        .column_names()
        .iter()
        .map(|name| (*name).to_owned())
        .collect::<Vec<_>>();
    let mut rows = statement.query([])?;
    let mut values = Vec::new();
    while let Some(row) = rows.next()? {
        let mut object = Map::new();
        for (index, name) in names.iter().enumerate() {
            let value = match row.get_ref(index)? {
                rusqlite::types::ValueRef::Null => Value::Null,
                rusqlite::types::ValueRef::Integer(value) => json!(value),
                rusqlite::types::ValueRef::Real(value) => json!(value),
                rusqlite::types::ValueRef::Text(value) => {
                    let text = String::from_utf8_lossy(value).into_owned();
                    if table == "providers" && name == "headers" {
                        redact_headers(&text)
                    } else {
                        Value::String(text)
                    }
                }
                rusqlite::types::ValueRef::Blob(value) => {
                    Value::Array(value.iter().map(|byte| json!(byte)).collect())
                }
            };
            object.insert(name.clone(), value);
        }
        if table == "mcp_servers" {
            // OAuth and custom auth values may contain credentials; they are not portable secrets.
            object.insert("auth_header".into(), Value::Null);
            object.insert("auth_template".into(), Value::Null);
        }
        values.push(Value::Object(object));
    }
    Ok(Value::Array(values))
}

fn redact_headers(raw: &str) -> Value {
    let Ok(mut headers) = serde_json::from_str::<Value>(raw) else {
        return Value::String("{}".into());
    };
    if let Some(map) = headers.as_object_mut() {
        map.retain(|name, _| {
            let key = name.to_ascii_lowercase();
            ![
                "authorization",
                "api-key",
                "apikey",
                "token",
                "secret",
                "password",
            ]
            .iter()
            .any(|needle| key.contains(needle))
        });
    }
    Value::String(headers.to_string())
}

fn table_columns(conn: &rusqlite::Connection, table: &str) -> Result<HashSet<String>> {
    let mut statement = conn.prepare(&format!("PRAGMA table_info(\"{table}\")"))?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<HashSet<_>>>()?;
    Ok(columns)
}

fn sql_value(value: &Value) -> SqlValue {
    match value {
        Value::Null => SqlValue::Null,
        Value::Bool(value) => SqlValue::Integer(i64::from(*value)),
        Value::Number(value) => value
            .as_i64()
            .map(SqlValue::Integer)
            .or_else(|| value.as_f64().map(SqlValue::Real))
            .unwrap_or(SqlValue::Null),
        Value::String(value) => SqlValue::Text(value.clone()),
        Value::Array(_) | Value::Object(_) => SqlValue::Text(value.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_file(name: &str) -> std::path::PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("aihat-{name}-{stamp}.db"))
    }

    #[test]
    fn backup_round_trip_preserves_chats_settings_and_custom_tables() {
        let source_path = temp_file("backup-source");
        let backup_path = temp_file("backup-json");
        let target_path = temp_file("backup-target");
        let source = Database::open(&source_path).unwrap();
        let chat = source.create_chat(None).unwrap();
        source
            .save_message(
                &chat.id,
                "user",
                "persist this chat",
                None,
                None,
                &json!([]),
                &json!([]),
                &json!([]),
            )
            .unwrap();
        source
            .set_kv(
                "settings",
                &json!({"memoryItems":[{"id":"m1","title":"Preference","text":"Use Rust"}]})
                    .to_string(),
            )
            .unwrap();
        source.conn.execute("INSERT INTO commands (id,name,description,template,mode,updated_at) VALUES ('cmd','hello','test','say {input}','send',1)", []).unwrap();
        export_to(&source, &backup_path).unwrap();

        let mut target = Database::open(&target_path).unwrap();
        let count = import_from(&mut target, &backup_path).unwrap();
        assert_eq!(count, TABLES.len());
        assert_eq!(
            target.chat(&chat.id).unwrap().unwrap().title,
            "persist this chat"
        );
        assert_eq!(
            target.messages(&chat.id).unwrap()[0].content,
            "persist this chat"
        );
        assert_eq!(target.settings()["memoryItems"][0]["text"], "Use Rust");
        assert_eq!(
            target
                .conn
                .query_row("SELECT name FROM commands WHERE id='cmd'", [], |row| row
                    .get::<_, String>(
                    0
                ))
                .unwrap(),
            "hello"
        );

        for path in [source_path, target_path, backup_path] {
            let _ = std::fs::remove_file(path);
        }
    }

    #[test]
    fn export_redacts_provider_and_mcp_credentials() {
        let path = temp_file("backup-secrets");
        let db = Database::open(&path).unwrap();
        db.insert_provider(
            "test",
            "https://example.test/v1",
            &json!({"X-API-Key":"secret","X-Team":"ok"}),
            1000,
        )
        .unwrap();
        let server_id = db
            .add_mcp_server("test", "https://example.test/mcp", "http", "none")
            .unwrap();
        db.conn
            .execute(
                "UPDATE mcp_servers SET auth_header='secret',auth_template='Bearer secret' WHERE id=?1",
                [&server_id],
            )
            .unwrap();
        let payload = export(&db).unwrap();
        let headers: Value =
            serde_json::from_str(payload["data"]["providers"][0]["headers"].as_str().unwrap())
                .unwrap();
        assert_eq!(headers["X-Team"], "ok");
        assert!(headers.get("X-API-Key").is_none());
        assert!(payload["data"]["mcp_servers"][0]["auth_header"].is_null());
        let _ = std::fs::remove_file(path);
    }
}
