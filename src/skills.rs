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

pub(crate) fn read_skill_source(
    path: &Path,
) -> Result<(String, String, String, String, Vec<(String, String)>)> {
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

#[cfg(test)]
mod tests {
    use super::{parse_skill, read_skill_source};

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
