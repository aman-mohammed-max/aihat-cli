use crate::config::Paths;
use anyhow::{Context, Result};
use keyring::Entry;
use serde_json::{Map, Value};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

const SERVICE: &str = "AI Hat";

/// macOS and Windows keychains keep secrets for good. Elsewhere there is no
/// keychain that is always there (Linux's kernel keyring forgets everything
/// at restart; the BSDs have none in this build), so keys live in the
/// private file.
const KEYCHAIN_LASTS: bool = cfg!(any(target_os = "macos", target_os = "windows"));

/// Where a new key is kept, for the hint next to key fields.
pub fn where_keys_go(system: bool) -> &'static str {
    match (KEYCHAIN_LASTS, system) {
        (true, true) => "saved in your system keychain",
        (true, false) => "saved in your keychain",
        (false, _) => "saved in a private file only you can read",
    }
}

#[derive(Clone)]
pub struct Secrets {
    file: std::path::PathBuf,
}

impl Secrets {
    pub fn new(paths: &Paths) -> Self {
        Self {
            file: paths.keys_file.clone(),
        }
    }

    /// Secrets that hold nothing: for testing an endpoint before it is saved.
    pub fn detached() -> Self {
        Self {
            file: std::env::temp_dir().join("aihat-no-saved-keys.json"),
        }
    }

    pub fn get(&self, name: &str) -> Option<String> {
        if KEYCHAIN_LASTS
            && let Ok(value) = Entry::new(SERVICE, name).and_then(|entry| entry.get_password())
        {
            return Some(value);
        }
        if let Some(value) = self.read_file().get(name).and_then(Value::as_str) {
            return Some(value.to_owned());
        }
        // Older builds put Linux keys in the kernel keyring; move one over
        // while it is still there.
        let entry = Entry::new(SERVICE, name).ok()?;
        let value = entry.get_password().ok()?;
        if self.set(name, &value).is_ok() {
            let _ = entry.delete_credential();
        }
        Some(value)
    }

    pub fn set(&self, name: &str, value: &str) -> Result<&'static str> {
        if KEYCHAIN_LASTS
            && let Ok(entry) = Entry::new(SERVICE, name)
            && entry.set_password(value).is_ok()
        {
            return Ok("system credential store");
        }
        let mut all = self.read_file().as_object().cloned().unwrap_or_default();
        all.insert(name.to_string(), Value::String(value.to_string()));
        self.write_file(&Value::Object(all))?;
        Ok("private keys.json file")
    }

    pub fn delete(&self, name: &str) -> Result<()> {
        if let Ok(entry) = Entry::new(SERVICE, name) {
            let _ = entry.delete_credential();
        }
        let mut all: Map<String, Value> = self.read_file().as_object().cloned().unwrap_or_default();
        all.remove(name);
        self.write_file(&Value::Object(all))
    }

    pub fn provider_key(&self, provider_id: &str) -> Option<String> {
        let env_id = provider_id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_uppercase()
                } else {
                    '_'
                }
            })
            .collect::<String>();
        std::env::var(format!("AIHAT_KEY_{env_id}"))
            .ok()
            .or_else(|| std::env::var("AIHAT_API_KEY").ok())
            .or_else(|| self.get(&format!("apikey.{provider_id}")))
    }

    /// Where a provider's key comes from: "keychain", "keys file",
    /// "environment", or `None` when it has no key. Slow on some systems
    /// (it asks the keychain), so callers should cache the answer.
    pub fn provider_key_location(&self, provider_id: &str) -> Option<&'static str> {
        let name = format!("apikey.{provider_id}");
        if KEYCHAIN_LASTS
            && Entry::new(SERVICE, &name)
                .and_then(|entry| entry.get_password())
                .is_ok()
        {
            return Some("keychain");
        }
        if self.read_file().get(&name).is_some() {
            return Some("keys file");
        }
        self.provider_key(provider_id).map(|_| "environment")
    }

    pub fn mcp_token(&self, server_id: &str) -> Option<String> {
        self.get(&format!("mcp.{server_id}"))
    }

    pub fn set_mcp_token(&self, server_id: &str, value: &str) -> Result<&'static str> {
        self.set(&format!("mcp.{server_id}"), value)
    }

    fn read_file(&self) -> Value {
        fs::read_to_string(&self.file)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_else(|| Value::Object(Map::new()))
    }

    fn write_file(&self, value: &Value) -> Result<()> {
        if let Some(parent) = self.file.parent() {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        let temp = self.file.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(value)?;
        write_private(&temp, &bytes)?;
        fs::rename(&temp, &self.file).with_context(|| format!("saving {}", self.file.display()))?;
        set_private_permissions(&self.file)?;
        Ok(())
    }
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(bytes)?;
    }
    #[cfg(not(unix))]
    {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(path)?;
        file.write_all(bytes)?;
    }
    Ok(())
}

fn set_private_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}
