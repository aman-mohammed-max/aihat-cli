use anyhow::{Context, Result};
use std::path::Path;

pub(crate) fn parse_skill(input: &str, fallback_name: &str) -> (String, String, String) {
    let mut name = fallback_name.to_string();
    let mut description = String::new();
    let body = if let Some(rest) = input
        .strip_prefix("---\n")
        .or_else(|| input.strip_prefix("---\r\n"))
    {
        if let Some(end) = rest.find("\n---") {
            let front = &rest[..end];
            for line in front.lines() {
                if let Some(value) = line.strip_prefix("name:") {
                    name = value
                        .trim()
                        .trim_matches(|character| character == '\"' || character == '\'')
                        .to_string();
                }
                if let Some(value) = line.strip_prefix("description:") {
                    description = value
                        .trim()
                        .trim_matches(|character| character == '\"' || character == '\'')
                        .to_string();
                }
            }
            rest[end + 4..].trim().to_string()
        } else {
            input.to_string()
        }
    } else {
        input.to_string()
    };
    (name, description, body)
}

pub(crate) fn read_skill_source(path: &Path) -> Result<Loaded> {
    let source_path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let skill_file = if source_path.is_dir() {
        source_path.join("SKILL.md")
    } else {
        source_path.clone()
    };
    let content = std::fs::read_to_string(&skill_file)
        .with_context(|| format!("reading {}", skill_file.display()))?;
    let fallback = if source_path.is_dir() {
        source_path.file_name().and_then(|name| name.to_str())
    } else {
        source_path
            .parent()
            .and_then(|parent| parent.file_name())
            .and_then(|name| name.to_str())
    }
    .unwrap_or("local-skill");
    let (name, description, body) = parse_skill(&content, fallback);
    if name.trim().is_empty() || body.trim().is_empty() {
        anyhow::bail!("SKILL.md needs a name and a non-empty body");
    }
    let mut files = Vec::new();
    let root = if source_path.is_dir() {
        Some(source_path.as_path())
    } else if source_path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("SKILL.md"))
    {
        source_path.parent()
    } else {
        None
    };
    if let Some(root) = root {
        let mut total_bytes = 0;
        collect_skill_files(root, root, &mut files, &mut total_bytes)?;
    } else if let Some(filename) = skill_file.file_name().and_then(|name| name.to_str()) {
        files.push((filename.to_string(), content));
    }
    Ok((
        name,
        description,
        body,
        source_path.to_string_lossy().into_owned(),
        files,
    ))
}

fn collect_skill_files(
    root: &Path,
    current: &Path,
    output: &mut Vec<(String, String)>,
    total_bytes: &mut usize,
) -> Result<()> {
    const MAX_FILES: usize = 256;
    const MAX_FILE_BYTES: u64 = 1024 * 1024;
    const MAX_TOTAL_BYTES: usize = 8 * 1024 * 1024;
    const SKIP_DIRS: &[&str] = &[
        ".git",
        "node_modules",
        "target",
        "dist",
        "build",
        ".venv",
        "venv",
        "__pycache__",
    ];

    let mut entries = std::fs::read_dir(current)
        .with_context(|| format!("reading skill directory {}", current.display()))?
        .collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        if output.len() >= MAX_FILES {
            anyhow::bail!("skill folder contains more than {MAX_FILES} text files");
        }
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let path = entry.path();
        if file_type.is_dir() {
            if SKIP_DIRS.iter().any(|excluded| name == *excluded) {
                continue;
            }
            collect_skill_files(root, &path, output, total_bytes)?;
            continue;
        }
        if !file_type.is_file() || entry.metadata()?.len() > MAX_FILE_BYTES {
            continue;
        }
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::InvalidData => continue,
            Err(error) => {
                return Err(error).with_context(|| format!("reading {}", path.display()));
            }
        };
        *total_bytes = total_bytes.saturating_add(text.len());
        if *total_bytes > MAX_TOTAL_BYTES {
            anyhow::bail!("skill folder contains more than 8 MiB of text files");
        }
        let relative = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        output.push((relative, text));
    }
    Ok(())
}

/// A skill as read from its source: name, description, body, where it came
/// from, and its text files.
pub type Loaded = (String, String, String, String, Vec<(String, String)>);

/// A folder (or SKILL.md) on this computer, or a GitHub link like
/// `https://github.com/anthropics/skills/tree/main/skills/xlsx`.
pub fn load(input: &str) -> Result<Loaded> {
    let trimmed = input.trim();
    let local = expand(trimmed);
    if local.exists() {
        return read_skill_source(&local);
    }
    match parse_github(trimmed) {
        Some(found) => fetch_github(&found, true),
        None => anyhow::bail!("There is no skill folder at {trimmed}, and it isn't a GitHub link"),
    }
}

/// Read a skill again from where it came from (its folder, or GitHub).
pub fn reload(source: &str) -> Result<Loaded> {
    match github_source(source) {
        Some(found) => fetch_github(&found, true),
        None => read_skill_source(&expand(source)),
    }
}

pub fn is_github(source: &str) -> bool {
    github_source(source).is_some()
}

fn expand(path: &str) -> std::path::PathBuf {
    match path.strip_prefix("~/").zip(dirs::home_dir()) {
        Some((rest, home)) => home.join(rest),
        None => std::path::PathBuf::from(path),
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct GithubRef {
    pub owner: String,
    pub repo: String,
    /// A branch, tag or commit; the default branch when missing.
    pub git_ref: Option<String>,
    /// The folder that holds SKILL.md; empty for the repository's root.
    pub path: String,
}

/// `owner/repo`, `github.com/owner/repo`, or a link to a folder or SKILL.md in it.
pub fn parse_github(input: &str) -> Option<GithubRef> {
    let mut t = input.trim();
    for prefix in ["https://", "http://", "www."] {
        t = t.strip_prefix(prefix).unwrap_or(t);
    }
    let t = t.trim_end_matches('/').trim_end_matches(".git");
    let explicit = t.to_ascii_lowercase().starts_with("github.com/");
    let rest = if explicit { &t["github.com/".len()..] } else { t };
    let mut parts = rest.splitn(3, '/');
    let owner = parts.next()?.to_string();
    let repo = parts.next()?.to_string();
    let valid = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c));
    // A bare "owner/repo" has no dots in the owner; "vercel.com/docs" is a website.
    if !valid(&owner) || !valid(&repo) || (!explicit && owner.contains('.')) {
        return None;
    }
    let (git_ref, mut path) = match parts.next() {
        None => (None, String::new()),
        Some(tail) => match tail.split_once('/') {
            Some(("tree" | "blob", after)) => {
                let (r, p) = after.split_once('/').unwrap_or((after, ""));
                (Some(r.to_string()), p.to_string())
            }
            // github.com/owner/repo/folder, as installed skills record it.
            _ if explicit => (None, tail.to_string()),
            _ => return None,
        },
    };
    if path.to_ascii_lowercase().ends_with("skill.md") {
        path = path[..path.len() - "SKILL.md".len()].trim_end_matches('/').to_string();
    }
    Some(GithubRef { owner, repo, git_ref, path: path.trim_matches('/').to_string() })
}

/// `github.com/owner/repo/folder` (how a GitHub skill records its source).
fn github_source(source: &str) -> Option<GithubRef> {
    source.starts_with("github.com/").then(|| parse_github(source)).flatten()
}

const TEXT_EXT: &[&str] = &[
    "md", "markdown", "txt", "rst", "json", "jsonc", "yaml", "yml", "toml", "ini", "cfg", "csv", "tsv", "xml", "html", "htm", "css", "js",
    "mjs", "cjs", "ts", "tsx", "jsx", "py", "sh", "bash", "zsh", "rb", "php", "go", "rs", "java", "kt", "swift", "c", "h", "cpp", "hpp", "cs",
    "sql", "r", "lua", "pl", "tex", "bib",
];
const MAX_REMOTE_FILES: usize = 60;
const MAX_REMOTE_FILE: u64 = 150_000;
const MAX_REMOTE_TOTAL: u64 = 1_200_000;

fn github_get(url: &str) -> Result<reqwest::blocking::Response> {
    use crate::api::SendRetry;
    let response = crate::api::client(20_000)?
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .send_retry()
        .with_context(|| format!("couldn't reach {}", url.split('/').nth(2).unwrap_or("GitHub")))?;
    Ok(response)
}

/// Every file in the repository with its size, from one GitHub API call.
fn repo_files(found: &GithubRef, git_ref: &str) -> Result<Vec<(String, u64)>> {
    let url = format!("https://api.github.com/repos/{}/{}/git/trees/{git_ref}?recursive=1", found.owner, found.repo);
    let response = github_get(&url)?;
    match response.status().as_u16() {
        404 => anyhow::bail!("GitHub has no repository {}/{} (or it is private)", found.owner, found.repo),
        403 | 429 => anyhow::bail!("GitHub's limit for anonymous requests was reached; try again in a while"),
        code if code >= 400 => anyhow::bail!("GitHub answered {code}"),
        _ => {}
    }
    let tree: serde_json::Value = response.json()?;
    Ok(tree
        .get("tree")
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter(|item| item.get("type").and_then(serde_json::Value::as_str) == Some("blob"))
                .filter_map(|item| Some((item.get("path")?.as_str()?.to_string(), item.get("size").and_then(serde_json::Value::as_u64).unwrap_or(0))))
                .collect()
        })
        .unwrap_or_default())
}

fn raw_text(found: &GithubRef, git_ref: &str, path: &str) -> Option<String> {
    let url = format!("https://raw.githubusercontent.com/{}/{}/{git_ref}/{}", found.owner, found.repo, path.trim_start_matches('/'));
    let response = github_get(&url).ok()?;
    if !response.status().is_success() {
        return None;
    }
    let text = response.text().ok()?;
    (!text.contains('\0')).then(|| text.replace("\r\n", "\n"))
}

/// Read a skill from GitHub: its SKILL.md and (with `with_files`) the text
/// files next to it: references, templates, scripts as text (never run).
pub fn fetch_github(found: &GithubRef, with_files: bool) -> Result<Loaded> {
    let git_ref = found.git_ref.clone().unwrap_or_else(|| "HEAD".into());
    let files = repo_files(found, &git_ref);
    let skill_dirs: Vec<String> = files
        .as_ref()
        .map(|files| {
            let mut dirs: Vec<String> = files
                .iter()
                .filter(|(path, _)| path == "SKILL.md" || path.ends_with("/SKILL.md"))
                .map(|(path, _)| path.trim_end_matches("SKILL.md").trim_end_matches('/').to_string())
                .collect();
            dirs.sort_by_key(|dir| (dir.len(), dir.clone()));
            dirs
        })
        .unwrap_or_default();
    let dir = if !found.path.is_empty() || skill_dirs.is_empty() || skill_dirs.iter().any(|d| d.is_empty()) {
        found.path.clone()
    } else if skill_dirs.len() == 1 {
        skill_dirs[0].clone()
    } else {
        let examples: Vec<&str> = skill_dirs.iter().take(4).map(String::as_str).collect();
        anyhow::bail!(
            "{}/{} has {} skills. Add one of them, like: aihat skills add github.com/{}/{}/{}  (others: {})",
            found.owner,
            found.repo,
            skill_dirs.len(),
            found.owner,
            found.repo,
            skill_dirs[0],
            examples.join(", ")
        );
    };
    let prefix = if dir.is_empty() { String::new() } else { format!("{dir}/") };
    let md = raw_text(found, &git_ref, &format!("{prefix}SKILL.md")).with_context(|| {
        if dir.is_empty() {
            format!("{}/{} has no SKILL.md at its top", found.owner, found.repo)
        } else {
            format!("There is no SKILL.md in {}/{}/{dir}", found.owner, found.repo)
        }
    })?;
    let fallback = dir.rsplit('/').next().filter(|s| !s.is_empty()).unwrap_or(&found.repo).to_string();
    let (name, description, body) = parse_skill(&md, &fallback);
    if name.trim().is_empty() || body.trim().is_empty() {
        anyhow::bail!("That SKILL.md needs a name and some instructions");
    }

    // The files next to SKILL.md, without other skills' folders: the ones
    // SKILL.md mentions first, then documents, then the rest, within limits.
    let mut picked = Vec::new();
    if let (true, Ok(files)) = (with_files, &files) {
        let others: Vec<String> = skill_dirs.iter().filter(|d| **d != dir && d.starts_with(&prefix)).map(|d| format!("{d}/")).collect();
        let mut candidates: Vec<(String, u64)> = files
            .iter()
            .filter(|(path, size)| {
                let name = path.rsplit('/').next().unwrap_or(path);
                let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
                path.starts_with(&prefix)
                    && path.as_str() != format!("{prefix}SKILL.md")
                    && !others.iter().any(|o| path.starts_with(o))
                    && (!dir.is_empty() || body.contains(path.as_str()))
                    && TEXT_EXT.contains(&ext.as_str())
                    && !["node_modules/", ".git/", "__pycache__/", "dist/", "build/", ".venv/", "venv/"].iter().any(|skip| path.contains(skip))
                    && *size > 1
                    && *size <= MAX_REMOTE_FILE
            })
            .map(|(path, size)| (path[prefix.len()..].to_string(), *size))
            .collect();
        let rank = |rel: &str| {
            if body.contains(rel) {
                0
            } else if rel.ends_with(".md") || rel.ends_with(".txt") {
                1
            } else {
                2
            }
        };
        candidates.sort_by(|a, b| rank(&a.0).cmp(&rank(&b.0)).then(a.0.matches('/').count().cmp(&b.0.matches('/').count())).then(a.0.cmp(&b.0)));
        let mut total = 0;
        let mut chosen = Vec::new();
        for (rel, size) in candidates {
            if chosen.len() >= MAX_REMOTE_FILES || total + size > MAX_REMOTE_TOTAL {
                continue;
            }
            total += size;
            chosen.push(rel);
        }
        // Six downloads at a time.
        for batch in chosen.chunks(6) {
            let fetched: Vec<Option<String>> = std::thread::scope(|scope| {
                let jobs: Vec<_> = batch
                    .iter()
                    .map(|rel| {
                        let path = format!("{prefix}{rel}");
                        let git_ref = &git_ref;
                        scope.spawn(move || raw_text(found, git_ref, &path))
                    })
                    .collect();
                jobs.into_iter().map(|job| job.join().ok().flatten()).collect()
            });
            for (rel, text) in batch.iter().zip(fetched) {
                if let Some(text) = text {
                    picked.push((rel.clone(), text));
                }
            }
        }
    }
    let source = if dir.is_empty() {
        format!("github.com/{}/{}", found.owner, found.repo)
    } else {
        format!("github.com/{}/{}/{dir}", found.owner, found.repo)
    };
    Ok((name, description, body, source, picked))
}

#[cfg(test)]
mod tests {
    use super::{GithubRef, parse_github, parse_skill, read_skill_source};

    #[test]
    fn reads_github_links_like_the_app() {
        let r = |owner: &str, repo: &str, git_ref: Option<&str>, path: &str| GithubRef {
            owner: owner.into(),
            repo: repo.into(),
            git_ref: git_ref.map(str::to_owned),
            path: path.into(),
        };
        assert_eq!(parse_github("https://github.com/anthropics/skills/tree/main/skills/xlsx"), Some(r("anthropics", "skills", Some("main"), "skills/xlsx")));
        assert_eq!(parse_github("github.com/anthropics/skills/blob/main/skills/pdf/SKILL.md"), Some(r("anthropics", "skills", Some("main"), "skills/pdf")));
        assert_eq!(parse_github("anthropics/skills"), Some(r("anthropics", "skills", None, "")));
        assert_eq!(parse_github("github.com/anthropics/skills/skills/xlsx"), Some(r("anthropics", "skills", None, "skills/xlsx")));
        assert_eq!(parse_github("vercel.com/docs"), None);
        assert_eq!(parse_github("https://example.com/a/b"), None);
    }

    #[test]
    fn parses_skill_frontmatter_and_imports_supporting_text_files() {
        let dir = std::env::temp_dir().join(format!("aihat-skill-source-{}", uuid::Uuid::new_v4()));
        let references = dir.join("references");
        std::fs::create_dir_all(&references).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            "---\nname: \"Release Reviewer\"\ndescription: 'Review release notes'\n---\nCheck every release claim.",
        )
        .unwrap();
        std::fs::write(references.join("guide.md"), "The release checklist.").unwrap();
        std::fs::write(dir.join("asset.bin"), [0, 159, 146, 150]).unwrap();

        let (name, description, body, _, files) = read_skill_source(&dir).unwrap();
        assert_eq!(name, "Release Reviewer");
        assert_eq!(description, "Review release notes");
        assert!(body.contains("every release claim"));
        assert!(files.iter().any(|(path, content)| {
            path == "references/guide.md" && content == "The release checklist."
        }));
        assert!(!files.iter().any(|(path, _)| path == "asset.bin"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn skill_without_frontmatter_uses_directory_name() {
        assert_eq!(
            parse_skill("Follow the guide.", "local-review"),
            (
                "local-review".into(),
                String::new(),
                "Follow the guide.".into()
            )
        );
    }

    #[test]
    fn parses_windows_line_endings_in_skill_frontmatter() {
        let (name, description, body) = parse_skill(
            "---\r\nname: Windows Skill\r\ndescription: CRLF source\r\n---\r\nUse the skill.",
            "fallback",
        );
        assert_eq!(name, "Windows Skill");
        assert_eq!(description, "CRLF source");
        assert_eq!(body, "Use the skill.");
    }
}
