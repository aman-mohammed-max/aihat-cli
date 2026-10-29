//! The folder aihat runs in. Once the person allows it, the assistant can
//! list, read and search its files (never outside it), and ask to run a
//! command there; every command waits for the person's yes. Decisions are
//! kept per folder in the database.

use crate::db::Database;
use serde_json::{Value, json};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const KEY: &str = "cliFolders";
const MAX_READ: usize = 60_000;
const MAX_OUTPUT: usize = 20_000;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Allowed,
    Denied,
}

fn key_for(dir: &Path) -> String {
    dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf()).to_string_lossy().into_owned()
}

fn all(db: &Database) -> Value {
    db.get_kv(KEY).ok().flatten().and_then(|raw| serde_json::from_str(&raw).ok()).filter(Value::is_object).unwrap_or_else(|| json!({}))
}

fn save(db: &Database, value: &Value) -> anyhow::Result<()> {
    db.set_kv(KEY, &value.to_string())
}

/// What the person decided for this folder, if they were asked.
pub fn access(db: &Database, dir: &Path) -> Option<Access> {
    match all(db).pointer(&format!("/{}/access", escape(&key_for(dir)))).and_then(Value::as_str) {
        Some("allowed") => Some(Access::Allowed),
        Some("denied") => Some(Access::Denied),
        _ => None,
    }
}

/// JSON pointer escaping for a folder path used as a key.
fn escape(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

pub fn set_access(db: &Database, dir: &Path, access: Option<Access>) -> anyhow::Result<()> {
    let mut folders = all(db);
    let key = key_for(dir);
    match access {
        Some(access) => {
            let entry = folders.as_object_mut().expect("an object").entry(key).or_insert_with(|| json!({}));
            entry["access"] = json!(if access == Access::Allowed { "allowed" } else { "denied" });
            entry["at"] = json!(chrono::Utc::now().timestamp_millis());
        }
        None => {
            folders.as_object_mut().expect("an object").remove(&key);
        }
    }
    save(db, &folders)
}

/// Commands the person said to always run in this folder.
pub fn allowed_commands(db: &Database, dir: &Path) -> Vec<String> {
    all(db)
        .pointer(&format!("/{}/commands", escape(&key_for(dir))))
        .and_then(Value::as_array)
        .map(|list| list.iter().filter_map(Value::as_str).map(str::to_owned).collect())
        .unwrap_or_default()
}

pub fn allow_command(db: &Database, dir: &Path, command: &str) -> anyhow::Result<()> {
    let mut folders = all(db);
    let entry = folders.as_object_mut().expect("an object").entry(key_for(dir)).or_insert_with(|| json!({}));
    let mut commands: Vec<Value> = entry.get("commands").and_then(Value::as_array).cloned().unwrap_or_default();
    if !commands.iter().any(|c| c.as_str() == Some(command.trim())) {
        commands.push(json!(command.trim()));
    }
    entry["commands"] = json!(commands);
    save(db, &folders)
}

pub fn forget_commands(db: &Database, dir: &Path) -> anyhow::Result<()> {
    let mut folders = all(db);
    if let Some(entry) = folders.get_mut(key_for(dir)) {
        entry["commands"] = json!([]);
    }
    save(db, &folders)
}

/// What the model is told about the folder.
pub fn instruction(dir: &Path, allowed: bool) -> String {
    let asking = if allowed { "" } else { " The first time you look, the user is asked to allow it." };
    format!(
        "The user is running AI Hat in the folder {} on their computer. When their request is about files there, use list_files, read_file and search_files (paths are relative to the folder).{asking} Use run_command to run a shell command there: the user approves every command before it runs, so give a short reason. Don't run commands that delete or change files unless the user asked for that. Don't look at the folder for questions that have nothing to do with it.",
        crate::ui::text::home_relative(dir)
    )
}

pub fn tool_definitions() -> Vec<Value> {
    let tool = |name: &str, description: &str, properties: Value, required: &[&str]| {
        json!({"type": "function", "function": {"name": name, "description": description,
            "parameters": {"type": "object", "properties": properties, "required": required}}})
    };
    vec![
        tool(
            "list_files",
            "List the files and folders in the user's folder (or a folder inside it), skipping what .gitignore leaves out.",
            json!({"path": {"type": "string", "description": "A folder relative to the user's folder; . for the folder itself"},
                   "depth": {"type": "integer", "description": "How many levels down, 1 to 6 (default 2)"}}),
            &[],
        ),
        tool(
            "read_file",
            "Read a text file in the user's folder, with line numbers.",
            json!({"path": {"type": "string", "description": "The file, relative to the user's folder"},
                   "offset": {"type": "integer", "description": "The first line to read (default 1)"},
                   "limit": {"type": "integer", "description": "How many lines (default 400)"}}),
            &["path"],
        ),
        tool(
            "search_files",
            "Find lines containing some text in the files of the user's folder (case doesn't matter).",
            json!({"query": {"type": "string", "description": "The text to find"},
                   "path": {"type": "string", "description": "A folder or file to search in (default: the whole folder)"}}),
            &["query"],
        ),
        tool(
            "run_command",
            "Run a shell command in the user's folder and get its output. The user approves each command first.",
            json!({"command": {"type": "string", "description": "The command line, as typed in a terminal"},
                   "reason": {"type": "string", "description": "Why you want to run it, in a few words"}}),
            &["command", "reason"],
        ),
    ]
}

/// A path inside the folder; anything that leads outside it is refused.
fn inside(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let relative = relative.trim();
    let candidate = match relative {
        "" | "." | "./" => root.to_path_buf(),
        _ if Path::new(relative).is_absolute() => PathBuf::from(relative),
        _ => root.join(relative),
    };
    let real = candidate.canonicalize().map_err(|_| format!("{relative} isn't in this folder"))?;
    let base = root.canonicalize().map_err(|e| e.to_string())?;
    if !real.starts_with(&base) {
        return Err(format!("{relative} is outside the folder you can read"));
    }
    Ok(real)
}

fn relative_to(root: &Path, path: &Path) -> String {
    let base = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    path.strip_prefix(&base).unwrap_or(path).to_string_lossy().replace('\\', "/")
}

/// The read-only tools: list_files, read_file, search_files.
pub fn read_tool(root: &Path, name: &str, args: &Value) -> Result<String, String> {
    let text = |key: &str| args.get(key).and_then(Value::as_str).unwrap_or("").to_string();
    let number = |key: &str| args.get(key).and_then(Value::as_u64);
    match name {
        "list_files" => {
            let dir = inside(root, &text("path"))?;
            let depth = number("depth").unwrap_or(2).clamp(1, 6) as usize;
            let mut lines = Vec::new();
            let mut more = 0;
            for entry in ignore::WalkBuilder::new(&dir).max_depth(Some(depth)).hidden(false).filter_entry(|e| e.file_name() != ".git").build().flatten() {
                if entry.depth() == 0 {
                    continue;
                }
                if lines.len() >= 400 {
                    more += 1;
                    continue;
                }
                let rel = relative_to(root, entry.path());
                match entry.metadata() {
                    Ok(meta) if meta.is_dir() => lines.push(format!("{rel}/")),
                    Ok(meta) => lines.push(format!("{rel}  {}", crate::ui::picture::size_label(meta.len() as usize))),
                    Err(_) => lines.push(rel),
                }
            }
            if more > 0 {
                lines.push(format!("… {more} more"));
            }
            Ok(if lines.is_empty() { "The folder is empty.".into() } else { lines.join("\n") })
        }
        "read_file" => {
            let path = inside(root, &text("path"))?;
            if path.is_dir() {
                return Err(format!("{} is a folder; use list_files", text("path")));
            }
            let mut bytes = Vec::new();
            std::fs::File::open(&path).and_then(|f| f.take(4_000_000).read_to_end(&mut bytes)).map_err(|e| e.to_string())?;
            if bytes.iter().take(8000).any(|b| *b == 0) {
                return Err(format!("{} isn't a text file", text("path")));
            }
            let content = String::from_utf8_lossy(&bytes);
            let lines: Vec<&str> = content.lines().collect();
            let first = number("offset").unwrap_or(1).max(1) as usize;
            let count = number("limit").unwrap_or(400).clamp(1, 2000) as usize;
            let mut out = String::new();
            for (index, line) in lines.iter().enumerate().skip(first - 1).take(count) {
                if out.len() > MAX_READ {
                    out.push_str("… (cut; read on with offset)\n");
                    break;
                }
                out.push_str(&format!("{:>5}  {line}\n", index + 1));
            }
            let last = (first - 1 + count).min(lines.len());
            Ok(format!("{} · lines {first}–{last} of {}\n{out}", text("path"), lines.len()))
        }
        "search_files" => {
            let query = text("query");
            if query.trim().is_empty() {
                return Err("Say what to search for".into());
            }
            let wanted = query.to_lowercase();
            let start = inside(root, &text("path"))?;
            let mut found = Vec::new();
            'files: for entry in ignore::WalkBuilder::new(&start).hidden(false).filter_entry(|e| e.file_name() != ".git").build().flatten() {
                if !entry.file_type().is_some_and(|t| t.is_file()) || entry.metadata().is_ok_and(|m| m.len() > 1_000_000) {
                    continue;
                }
                let Ok(content) = std::fs::read_to_string(entry.path()) else { continue };
                for (number, line) in content.lines().enumerate() {
                    if line.to_lowercase().contains(&wanted) {
                        found.push(format!("{}:{}: {}", relative_to(root, entry.path()), number + 1, crate::ui::text::truncate(line.trim(), 200)));
                        if found.len() >= 100 {
                            found.push("… stopped at 100 matches".into());
                            break 'files;
                        }
                    }
                }
            }
            Ok(if found.is_empty() { format!("No lines contain {query:?}.") } else { found.join("\n") })
        }
        other => Err(format!("Unknown tool: {other}")),
    }
}

/// Run an approved command in the folder: its output and exit code, or why
/// it stopped. `cancel` stops it, as does a two-minute limit.
pub fn run_command(root: &Path, command: &str, cancel: &AtomicBool) -> Result<String, String> {
    let mut process = if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.args(["/C", command]);
        c
    } else {
        let mut c = Command::new("sh");
        c.args(["-c", command]);
        c
    };
    process.current_dir(root).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    // Its own process group, so stopping it stops what it started too.
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut process, 0);
    let mut child = process.spawn().map_err(|e| format!("Couldn't start the command: {e}"))?;
    let collect = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut out = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut out);
            }
            String::from_utf8_lossy(&out).into_owned()
        })
    };
    let stdout = collect(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let stderr = collect(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if cancel.load(Ordering::Relaxed) || started.elapsed() > COMMAND_TIMEOUT => break None,
            Ok(None) => std::thread::sleep(Duration::from_millis(40)),
            Err(error) => return Err(error.to_string()),
        }
    };
    if status.is_none() {
        stop(&mut child);
    }
    let (out, err) = (stdout.join().unwrap_or_default(), stderr.join().unwrap_or_default());
    let mut text = out.trim_end().to_string();
    if !err.trim().is_empty() {
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        text.push_str("stderr:\n");
        text.push_str(err.trim_end());
    }
    if text.chars().count() > MAX_OUTPUT {
        let skip = text.chars().count() - MAX_OUTPUT;
        text = format!("… (first {skip} characters left out)\n{}", text.chars().skip(skip).collect::<String>());
    }
    match status {
        Some(status) => {
            let code = status.code().map(|c| c.to_string()).unwrap_or_else(|| "a signal".into());
            Ok(format!("{}\n\nexit code {code}", if text.is_empty() { "(no output)" } else { &text }))
        }
        None if cancel.load(Ordering::Relaxed) => Err(format!("Stopped.\n{text}")),
        None => Err(format!("Stopped after {} seconds.\n{text}", COMMAND_TIMEOUT.as_secs())),
    }
}

fn stop(child: &mut std::process::Child) {
    #[cfg(unix)]
    // SAFETY: signalling the process group this child leads.
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("aihat-folder-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/main.rs"), "fn main() {\n    println!(\"Hello\");\n}\n").unwrap();
        std::fs::write(dir.join("README.md"), "# Demo\nhello world\n").unwrap();
        dir
    }

    #[test]
    fn reads_and_searches_inside_the_folder_only() {
        let root = folder();
        let listed = read_tool(&root, "list_files", &json!({})).unwrap();
        assert!(listed.contains("src/") && listed.contains("src/main.rs") && listed.contains("README.md"), "{listed}");
        let read = read_tool(&root, "read_file", &json!({"path": "src/main.rs", "offset": 2, "limit": 1})).unwrap();
        assert!(read.contains("    2      println!(\"Hello\");") && read.contains("lines 2–2 of 3"), "{read}");
        let found = read_tool(&root, "search_files", &json!({"query": "HELLO"})).unwrap();
        assert!(found.contains("README.md:2:") && found.contains("src/main.rs:2:"), "{found}");
        assert!(read_tool(&root, "read_file", &json!({"path": "../../etc/passwd"})).is_err());
        assert!(read_tool(&root, "read_file", &json!({"path": "/etc/hostname"})).is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn commands_run_in_the_folder_and_stop_when_asked() {
        let root = folder();
        let cancel = AtomicBool::new(false);
        let out = run_command(&root, "ls src; echo oops >&2; exit 3", &cancel).unwrap();
        assert!(out.contains("main.rs") && out.contains("stderr:\noops") && out.ends_with("exit code 3"), "{out}");
        let cancel = AtomicBool::new(true);
        let started = Instant::now();
        assert!(run_command(&root, "sleep 30", &cancel).is_err());
        assert!(started.elapsed() < Duration::from_secs(5));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn decisions_are_kept_per_folder() {
        let dir = std::env::temp_dir().join(format!("aihat-folder-db-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Database::open(&dir.join("aihat.db")).unwrap();
        let project = folder();
        assert_eq!(access(&db, &project), None);
        set_access(&db, &project, Some(Access::Allowed)).unwrap();
        assert_eq!(access(&db, &project), Some(Access::Allowed));
        allow_command(&db, &project, "cargo test").unwrap();
        assert_eq!(allowed_commands(&db, &project), vec!["cargo test".to_string()]);
        set_access(&db, &project, Some(Access::Denied)).unwrap();
        assert_eq!(access(&db, &project), Some(Access::Denied));
        drop(db);
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(project);
    }
}
