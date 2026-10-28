//! Files for the `@` picker: the current folder, respecting .gitignore,
//! fuzzy-matched on the path.

use crate::ui::text;
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

const MAX_FILES: usize = 20_000;

pub struct FileMatch {
    pub path: String,
    pub size: String,
    pub modified: String,
}

#[derive(Default)]
pub struct FileIndex {
    files: Vec<(String, i64)>,
    built: Option<Instant>,
    sizes: HashMap<String, String>,
}

impl FileIndex {
    fn ensure(&mut self) {
        if self.built.is_some_and(|at| at.elapsed().as_secs() < 30) {
            return;
        }
        self.files.clear();
        self.sizes.clear();
        let walker = ignore::WalkBuilder::new(".")
            .hidden(true)
            .git_ignore(true)
            .max_depth(Some(12))
            .build();
        for entry in walker.flatten() {
            if !entry.file_type().is_some_and(|kind| kind.is_file()) {
                continue;
            }
            let path = entry.path().strip_prefix("./").unwrap_or(entry.path());
            let modified = entry
                .metadata()
                .ok()
                .and_then(|meta| meta.modified().ok())
                .map(millis)
                .unwrap_or(0);
            self.files.push((path.to_string_lossy().into_owned(), modified));
            if self.files.len() >= MAX_FILES {
                break;
            }
        }
        self.built = Some(Instant::now());
    }

    pub fn matches(&mut self, query: &str, limit: usize) -> Vec<FileMatch> {
        if query.starts_with("http://") || query.starts_with("https://") {
            return Vec::new();
        }
        self.ensure();
        let picked: Vec<(String, i64)> = if query.is_empty() {
            let mut recent = self.files.clone();
            recent.sort_by(|a, b| b.1.cmp(&a.1));
            recent.into_iter().take(limit).collect()
        } else {
            let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
            let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
            let mut buf = Vec::new();
            let mut scored: Vec<(u32, &(String, i64))> = self
                .files
                .iter()
                .filter_map(|file| {
                    pattern
                        .score(Utf32Str::new(&file.0, &mut buf), &mut matcher)
                        .map(|score| (score, file))
                })
                .collect();
            scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.0.len().cmp(&b.1.0.len())));
            scored.into_iter().take(limit).map(|(_, file)| file.clone()).collect()
        };
        picked
            .into_iter()
            .map(|(path, modified)| {
                let size = self
                    .sizes
                    .entry(path.clone())
                    .or_insert_with(|| describe_size(Path::new(&path)))
                    .clone();
                FileMatch {
                    modified: if modified > 0 { text::relative_time(modified) } else { String::new() },
                    path,
                    size,
                }
            })
            .collect()
    }
}

fn millis(time: SystemTime) -> i64 {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// "412 lines" for text files, "1.2 MB" for everything else.
fn describe_size(path: &Path) -> String {
    let Ok(meta) = std::fs::metadata(path) else {
        return String::new();
    };
    let bytes = meta.len();
    if bytes <= 5_000_000 {
        if let Ok(data) = std::fs::read(path) {
            let head = &data[..data.len().min(8192)];
            if !head.contains(&0) && std::str::from_utf8(head).is_ok() {
                let lines = data.iter().filter(|byte| **byte == b'\n').count()
                    + usize::from(!data.is_empty() && !data.ends_with(b"\n"));
                return text::plural(lines, "line", "lines");
            }
        }
    }
    match bytes {
        0..1_000 => format!("{bytes} B"),
        1_000..1_000_000 => format!("{:.0} KB", bytes as f64 / 1000.0),
        _ => format!("{:.1} MB", bytes as f64 / 1_000_000.0),
    }
}

/// `~/notes.md` → the home folder path.
pub fn expand(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(path)
}
