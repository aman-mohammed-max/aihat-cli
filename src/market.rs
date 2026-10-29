//! The marketplace: where skills and MCP servers come from, the same places
//! the AI Hat app uses.
//!
//! - MCP servers: a few hosted servers that work straight away, and the
//!   official MCP Registry (registry.modelcontextprotocol.io). Unlike the
//!   phone, the terminal can also run servers published as a program (npm,
//!   PyPI, Docker).
//! - Skills: claude-plugins.dev (popular, with descriptions), skills.sh
//!   (search), any public GitHub repository with SKILL.md files, and websites
//!   that publish skills themselves at /.well-known/agent-skills (the open
//!   Agent Skills discovery standard), checked against their checksums.

use crate::api::SendRetry;
use crate::skills::{GithubRef, Loaded};
use anyhow::{Context, Result, bail};
use serde_json::Value;

fn get_json(url: &str, what: &str) -> Result<Value> {
    let response = crate::api::client(20_000)?
        .get(url)
        .header("Accept", "application/json")
        .send_retry()
        .with_context(|| format!("Couldn't reach {what}. Check your connection."))?;
    match response.status().as_u16() {
        403 | 429 => bail!("{what} is limiting requests right now. Try again in a minute."),
        code if code >= 400 => bail!("{what} answered with an error ({code})."),
        _ => {}
    }
    response.json().with_context(|| format!("{what} sent something unreadable"))
}

/// 5400 → "5.4k"
pub fn compact(n: u64) -> String {
    let short = |value: f64, unit: &str| {
        let text = format!("{value:.1}");
        format!("{}{unit}", text.trim_end_matches(".0"))
    };
    match n {
        1_000_000.. => short(n as f64 / 1e6, "M"),
        1_000.. => short(n as f64 / 1e3, "k"),
        _ => n.to_string(),
    }
}

// ------------------------------------------------------------------ skills

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SkillFrom {
    Popular,
    SkillsSh,
    GitHub,
    Site,
}

impl SkillFrom {
    pub fn label(&self) -> &'static str {
        match self {
            SkillFrom::Popular => "claude-plugins.dev",
            SkillFrom::SkillsSh => "skills.sh",
            SkillFrom::GitHub => "GitHub",
            SkillFrom::Site => "the website itself",
        }
    }
}

#[derive(Clone, Debug)]
pub struct SkillListing {
    pub name: String,
    pub description: Option<String>,
    /// `owner/repo` on GitHub.
    pub source: String,
    /// The skill's folder in the repository, when known (`skills/pdf`).
    pub dir: Option<String>,
    pub installs: Option<u64>,
    pub stars: Option<u64>,
    pub from: SkillFrom,
    /// skills.sh knows a skill's folder name, not its path.
    pub skill_id: Option<String>,
    /// Where a website's skill downloads from.
    pub site: Option<SiteEntry>,
}

/// Whether a listed skill is already in the library. Lists that know the
/// folder match it exactly; skills.sh only knows the skill's name.
pub fn skill_added(library: &[crate::models::Skill], skill: &SkillListing) -> bool {
    let squash = |s: &str| s.to_lowercase().chars().filter(char::is_ascii_alphanumeric).collect::<String>();
    library.iter().any(|saved| match (&skill.from, &skill.dir) {
        (SkillFrom::Site, _) => saved.source == format!("{}/{}", skill.source, skill.name),
        (_, Some(dir)) if dir.is_empty() => saved.source == format!("github.com/{}", skill.source),
        (_, Some(dir)) => saved.source == format!("github.com/{}/{dir}", skill.source),
        (_, None) => {
            saved.source.starts_with(&format!("github.com/{}", skill.source))
                && squash(&saved.name) == squash(skill.skill_id.as_deref().unwrap_or(&skill.name))
        }
    })
}

/// "anthropics/skills · 5.4k installs · ★ 25k"
pub fn skill_meta(item: &SkillListing) -> String {
    let mut parts = vec![item.source.clone()];
    if let Some(installs) = item.installs.filter(|n| *n > 0) {
        parts.push(format!("{} installs", compact(installs)));
    }
    if let Some(stars) = item.stars.filter(|n| *n > 0) {
        parts.push(format!("★ {}", compact(stars)));
    }
    parts.join(" · ")
}

/// What to type after `aihat skills add` for this listing.
pub fn skill_ref(item: &SkillListing) -> String {
    match (&item.dir, &item.skill_id) {
        (Some(dir), _) if !dir.is_empty() => format!("github.com/{}/tree/HEAD/{dir}", item.source),
        (_, Some(id)) => format!("{}/{id}", item.source),
        _ => format!("github.com/{}", item.source),
    }
}

/// Popular skills (no query) or a search, from claude-plugins.dev.
pub fn popular_skills(query: &str, offset: usize, limit: usize) -> Result<(Vec<SkillListing>, u64)> {
    let mut url = format!("https://claude-plugins.dev/api/skills?limit={limit}&offset={offset}");
    if !query.trim().is_empty() {
        url.push_str(&format!("&q={}", urlencoding::encode(query.trim())));
    }
    let data = get_json(&url, "claude-plugins.dev")?;
    let items: Vec<SkillListing> = data
        .get("skills")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|skill| {
            let meta = skill.get("metadata")?;
            let owner = meta.get("repoOwner")?.as_str()?;
            let repo = meta.get("repoName")?.as_str()?;
            Some(SkillListing {
                name: skill.get("name")?.as_str()?.to_string(),
                description: skill.get("description").and_then(Value::as_str).map(|d| d.trim().to_string()).filter(|d| !d.is_empty()),
                source: format!("{owner}/{repo}"),
                dir: Some(meta.get("directoryPath").and_then(Value::as_str).unwrap_or("").trim_matches('/').to_string()),
                installs: skill.get("installs").and_then(Value::as_u64),
                stars: skill.get("stars").and_then(Value::as_u64),
                from: SkillFrom::Popular,
                skill_id: None,
                site: None,
            })
        })
        .collect();
    let total = data.get("total").and_then(Value::as_u64).unwrap_or(items.len() as u64);
    Ok((items, total))
}

/// Search skills.sh (it needs at least two characters).
pub fn skills_sh(query: &str) -> Result<Vec<SkillListing>> {
    let query = query.trim();
    if query.chars().count() < 2 {
        return Ok(Vec::new());
    }
    let data = get_json(&format!("https://skills.sh/api/search?q={}&limit=30", urlencoding::encode(query)), "skills.sh")?;
    Ok(data
        .get("skills")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|skill| {
            let source = skill.get("source")?.as_str()?;
            // Only GitHub-hosted skills have files to read.
            let mut parts = source.split('/');
            let (owner, repo) = (parts.next()?, parts.next()?);
            if parts.next().is_some() || owner.is_empty() || repo.is_empty() {
                return None;
            }
            let skill_id = skill.get("skillId").and_then(Value::as_str).map(str::to_owned);
            Some(SkillListing {
                name: skill.get("name").and_then(Value::as_str).map(str::to_owned).or_else(|| skill_id.clone())?,
                description: None,
                source: source.to_string(),
                dir: None,
                installs: skill.get("installs").and_then(Value::as_u64),
                stars: None,
                from: SkillFrom::SkillsSh,
                skill_id,
                site: None,
            })
        })
        .collect())
}

/// Every skill in a GitHub repository (or one folder of it), named and
/// described by their SKILL.md (the first 24 are read, six at a time).
pub fn repo_skills(found: &GithubRef) -> Result<Vec<SkillListing>> {
    let git_ref = found.git_ref.clone().unwrap_or_else(|| "HEAD".into());
    let files = crate::skills::repo_files(found, &git_ref)?;
    let mut dirs: Vec<String> = files
        .iter()
        .filter(|(path, _)| path == "SKILL.md" || path.ends_with("/SKILL.md"))
        .map(|(path, _)| path.trim_end_matches("SKILL.md").trim_end_matches('/').to_string())
        .filter(|dir| found.path.is_empty() || dir == &found.path || dir.starts_with(&format!("{}/", found.path)))
        .collect();
    dirs.sort_by_key(|dir| (dir.len(), dir.clone()));
    if dirs.is_empty() {
        bail!(if found.path.is_empty() { "No SKILL.md files were found in that repository." } else { "No SKILL.md was found in that folder." });
    }
    let source = format!("{}/{}", found.owner, found.repo);
    let mut items: Vec<SkillListing> = dirs
        .iter()
        .map(|dir| SkillListing {
            name: dir.rsplit('/').next().filter(|s| !s.is_empty()).unwrap_or(&found.repo).to_string(),
            description: None,
            source: source.clone(),
            dir: Some(dir.clone()),
            installs: None,
            stars: None,
            from: SkillFrom::GitHub,
            skill_id: None,
            site: None,
        })
        .collect();
    let shown = items.len().min(24);
    for batch in items[..shown].chunks_mut(6) {
        std::thread::scope(|scope| {
            for item in batch.iter_mut() {
                let git_ref = &git_ref;
                scope.spawn(move || {
                    let dir = item.dir.clone().unwrap_or_default();
                    let path = if dir.is_empty() { "SKILL.md".to_string() } else { format!("{dir}/SKILL.md") };
                    if let Some(md) = crate::skills::raw_text(found, git_ref, &path) {
                        let (name, description, _) = crate::skills::parse_skill(&md, &item.name);
                        item.name = name;
                        item.description = Some(description).filter(|d| !d.is_empty());
                    }
                });
            }
        });
    }
    Ok(items)
}

/// Folders where repositories usually keep a skill, tried before reading
/// the repository's file list.
fn guesses(id: &str) -> Vec<String> {
    vec![
        format!("skills/{id}"),
        id.to_string(),
        format!("skills/.curated/{id}"),
        format!(".claude/skills/{id}"),
        format!(".agents/skills/{id}"),
        format!("skills/.system/{id}"),
        format!(".codex/skills/{id}"),
    ]
}

fn squash(text: &str) -> String {
    text.to_lowercase().chars().filter(|c| c.is_ascii_alphanumeric()).collect()
}

/// Where in its repository a listed skill lives.
pub fn locate_skill(item: &SkillListing) -> Result<GithubRef> {
    let (owner, repo) = item.source.split_once('/').context("not a GitHub source")?;
    let at = |dir: &str| GithubRef { owner: owner.into(), repo: repo.into(), git_ref: None, path: dir.to_string() };
    if let Some(dir) = &item.dir {
        return Ok(at(dir));
    }
    let id = item.skill_id.clone().unwrap_or_else(|| item.name.clone());
    let named = |md: &str| squash(&crate::skills::parse_skill(md, "").0) == squash(&id);
    let probe = at("");
    for dir in guesses(&id) {
        if let Some(md) = crate::skills::raw_text(&probe, "HEAD", &format!("{dir}/SKILL.md"))
            && (named(&md) || dir.rsplit('/').next() == Some(id.as_str()))
        {
            return Ok(at(&dir));
        }
    }
    if let Some(md) = crate::skills::raw_text(&probe, "HEAD", "SKILL.md")
        && named(&md)
    {
        return Ok(at(""));
    }
    // Read the repository's file list and find the folder named like the skill.
    let files = crate::skills::repo_files(&probe, "HEAD")?;
    let mut dirs: Vec<(u8, String)> = files
        .iter()
        .filter(|(path, _)| path.ends_with("/SKILL.md"))
        .map(|(path, _)| path.trim_end_matches("/SKILL.md").to_string())
        .filter_map(|dir| {
            let base = squash(dir.rsplit('/').next().unwrap_or(&dir));
            let want = squash(&id);
            let score = if base == want {
                100
            } else if base.len() >= 4 && (want.ends_with(&base) || base.ends_with(&want)) {
                60
            } else if base.len() >= 4 && (want.contains(&base) || base.contains(&want)) {
                40
            } else {
                return None;
            };
            Some((score, dir))
        })
        .collect();
    dirs.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.len().cmp(&b.1.len())));
    dirs.into_iter()
        .next()
        .map(|(_, dir)| at(&dir))
        .with_context(|| format!("Couldn't find {id}'s SKILL.md in {}. It may have been moved or removed.", item.source))
}

/// Read a listed skill and the files that come with it.
pub fn load_skill(item: &SkillListing) -> Result<Loaded> {
    if item.site.is_some() {
        return load_site_skill(item);
    }
    crate::skills::fetch_github(&locate_skill(item)?, true)
}

/// A listed skill's instructions and how many files come with it, for
/// reading before adding.
pub fn preview_skill(item: &SkillListing) -> Result<(String, usize)> {
    if item.site.is_some() {
        let (_, _, body, _, files) = load_site_skill(item)?;
        return Ok((body, files.len()));
    }
    let found = locate_skill(item)?;
    let git_ref = found.git_ref.clone().unwrap_or_else(|| "HEAD".into());
    let prefix = if found.path.is_empty() { String::new() } else { format!("{}/", found.path) };
    let md = crate::skills::raw_text(&found, &git_ref, &format!("{prefix}SKILL.md")).context("Couldn't read its SKILL.md.")?;
    let files = crate::skills::repo_files(&found, &git_ref)
        .map(|all| all.iter().filter(|(path, _)| path.starts_with(&prefix) && !path.ends_with("SKILL.md")).count())
        .unwrap_or(0);
    Ok((crate::skills::parse_skill(&md, &item.name).2, files))
}

// ------------------------------------------------------------------ skills websites publish

/// A skill in a website's index: one file or an archive to download, or
/// (the older index) the list of its files.
#[derive(Clone, Debug, Default)]
pub struct SiteEntry {
    pub index_url: String,
    /// `skill-md` or `archive`.
    pub kind: String,
    pub url: Option<String>,
    /// `sha256:…`, checked before anything is used.
    pub digest: Option<String>,
    pub files: Vec<String>,
}

const WELL_KNOWN: [&str; 2] = [".well-known/agent-skills", ".well-known/skills"];
const MAX_DOWNLOAD: u64 = 20_000_000;
const MAX_UNPACKED: u64 = 80_000_000;

/// `vercel.com`, `https://docs.stripe.com/` → `https://vercel.com`. None for
/// GitHub links and things that aren't a website.
pub fn parse_site_ref(input: &str) -> Option<String> {
    let t = input.trim().trim_end_matches('/');
    if t.is_empty() || t.contains(char::is_whitespace) {
        return None;
    }
    if t.starts_with("http://") || t.starts_with("https://") {
        let url = reqwest::Url::parse(t).ok()?;
        let host = url.host_str()?;
        if host == "github.com" || host.ends_with(".github.com") {
            return None;
        }
        return Some(url.origin().ascii_serialization());
    }
    let host = t.split('/').next()?;
    let (name, port) = host.split_once(':').map_or((host, None), |(h, p)| (h, Some(p)));
    let labels: Vec<&str> = name.split('.').collect();
    let looks_like_host = labels.len() >= 2
        && labels.iter().all(|l| !l.is_empty() && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
        && labels.last().is_some_and(|tld| tld.len() >= 2 && tld.chars().all(|c| c.is_ascii_alphabetic()))
        && port.is_none_or(|p| p.chars().all(|c| c.is_ascii_digit()));
    (looks_like_host && !name.eq_ignore_ascii_case("github.com")).then(|| format!("https://{host}"))
}

/// The skills a website publishes, from its index (the newer folder first).
pub fn site_skills(input: &str) -> Result<Vec<SkillListing>> {
    let origin = parse_site_ref(input).context("That doesn't look like a website address.")?;
    let host = reqwest::Url::parse(&origin).ok().and_then(|u| u.host_str().map(str::to_owned)).unwrap_or_default();
    for dir in WELL_KNOWN {
        let index_url = format!("{origin}/{dir}/index.json");
        let Ok(index) = get_json(&index_url, &host) else {
            continue;
        };
        let entries = index.get("skills").and_then(Value::as_array).cloned().unwrap_or_default();
        let items: Vec<SkillListing> = entries
            .iter()
            .filter_map(|entry| {
                let name = entry.get("name")?.as_str()?;
                if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c)) {
                    return None;
                }
                let text = |key: &str| entry.get(key).and_then(Value::as_str).map(str::to_owned);
                Some(SkillListing {
                    name: name.to_string(),
                    description: text("description").map(|d| d.trim().to_string()).filter(|d| !d.is_empty()),
                    source: host.clone(),
                    dir: Some(name.to_string()),
                    installs: None,
                    stars: None,
                    from: SkillFrom::Site,
                    skill_id: None,
                    site: Some(SiteEntry {
                        index_url: index_url.clone(),
                        kind: text("type").unwrap_or_default(),
                        url: text("url"),
                        digest: text("digest"),
                        files: entry.get("files").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect(),
                    }),
                })
            })
            .collect();
        if !items.is_empty() {
            return Ok(items);
        }
    }
    bail!("{host} doesn't publish skills this way (no /.well-known/agent-skills/index.json).")
}

fn download(url: &str, max: u64) -> Result<Vec<u8>> {
    use std::io::Read;
    let host = reqwest::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_owned)).unwrap_or_else(|| url.to_string());
    let response = crate::api::client(60_000)?.get(url).send_retry().with_context(|| format!("Couldn't reach {host}."))?;
    if !response.status().is_success() {
        bail!("Could not download from {host} ({}).", response.status().as_u16());
    }
    let mut bytes = Vec::new();
    response.take(max + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max {
        bail!("That download is too large.");
    }
    Ok(bytes)
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// Download a website's skill, check it against the checksum the site gave,
/// and read it with the text files that come with it.
pub fn load_site_skill(item: &SkillListing) -> Result<Loaded> {
    let entry = item.site.as_ref().context("This skill has no download address.")?;
    let index = reqwest::Url::parse(&entry.index_url)?;
    let label = format!("{}/{}", item.source, item.name);
    if let Some(url) = &entry.url {
        let url = index.join(url)?.to_string();
        let bytes = download(&url, MAX_DOWNLOAD)?;
        if let Some(want) = &entry.digest {
            let want = want.trim_start_matches("sha256:").trim_start_matches("SHA256:").to_lowercase();
            if sha256_hex(&bytes) != want {
                bail!("The download doesn't match the checksum the website published, so it was not used.");
            }
        }
        let files = if entry.kind == "archive" { unpack(&bytes)? } else { vec![("SKILL.md".to_string(), bytes)] };
        return skill_from_files(&files, &item.name, &label);
    }
    // The older index lists every file of the skill.
    let base = index.join(&format!("{}/", item.name))?;
    let mut names: Vec<String> = if entry.files.is_empty() { vec!["SKILL.md".into()] } else { entry.files.clone() };
    names.retain(|rel| !rel.is_empty() && !rel.contains("..") && !rel.starts_with('/'));
    names.truncate(crate::skills::MAX_REMOTE_FILES + 1);
    let mut files = Vec::new();
    for batch in names.chunks(12) {
        let fetched: Vec<Option<Vec<u8>>> = std::thread::scope(|scope| {
            let jobs: Vec<_> = batch
                .iter()
                .map(|rel| {
                    let url = base.join(rel).map(|u| u.to_string());
                    scope.spawn(move || url.ok().and_then(|url| download(&url, crate::skills::MAX_REMOTE_FILE * 2).ok()))
                })
                .collect();
            jobs.into_iter().map(|job| job.join().ok().flatten()).collect()
        });
        // A missing companion file is left out.
        files.extend(batch.iter().zip(fetched).filter_map(|(rel, bytes)| Some((rel.clone(), bytes?))));
    }
    skill_from_files(&files, &item.name, &label)
}

/// SKILL.md (the one nearest the top) and the text files beside it.
fn skill_from_files(all: &[(String, Vec<u8>)], fallback: &str, source: &str) -> Result<Loaded> {
    let (md_path, md) = all
        .iter()
        .filter(|(path, _)| path.rsplit('/').next().is_some_and(|name| name.eq_ignore_ascii_case("SKILL.md")))
        .min_by_key(|(path, _)| path.matches('/').count())
        .context("There is no SKILL.md in that download.")?;
    let dir = md_path.rsplit_once('/').map(|(dir, _)| format!("{dir}/")).unwrap_or_default();
    let md = String::from_utf8_lossy(md).replace("\r\n", "\n");
    let (name, description, body) = crate::skills::parse_skill(&md, fallback);
    if name.trim().is_empty() || body.trim().is_empty() {
        bail!("That SKILL.md needs a name and some instructions");
    }
    let mut candidates: Vec<(&str, &Vec<u8>)> = all
        .iter()
        .filter(|(path, data)| {
            let rel = path.strip_prefix(&dir).unwrap_or("");
            let ext = rel.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
            !rel.is_empty()
                && path != md_path
                && crate::skills::TEXT_EXT.contains(&ext.as_str())
                && data.len() > 1
                && data.len() as u64 <= crate::skills::MAX_REMOTE_FILE
                && !data.contains(&0)
        })
        .map(|(path, data)| (&path[dir.len()..], data))
        .collect();
    let rank = |rel: &str| if body.contains(rel) { 0 } else if rel.ends_with(".md") || rel.ends_with(".txt") { 1 } else { 2 };
    candidates.sort_by(|a, b| rank(a.0).cmp(&rank(b.0)).then(a.0.matches('/').count().cmp(&b.0.matches('/').count())).then(a.0.cmp(b.0)));
    let mut total = 0;
    let mut files = Vec::new();
    for (rel, data) in candidates {
        let Ok(text) = std::str::from_utf8(data) else { continue };
        if files.len() >= crate::skills::MAX_REMOTE_FILES || total + data.len() as u64 > crate::skills::MAX_REMOTE_TOTAL {
            continue;
        }
        total += data.len() as u64;
        files.push((rel.to_string(), text.replace("\r\n", "\n")));
    }
    Ok((name, description, body, source.to_string(), files))
}

/// The files of a downloaded archive: .tar.gz, .tgz, .tar or .zip.
fn unpack(bytes: &[u8]) -> Result<Vec<(String, Vec<u8>)>> {
    use std::io::Read;
    if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut tar = Vec::new();
        flate2::read::MultiGzDecoder::new(bytes).take(MAX_UNPACKED + 1).read_to_end(&mut tar).context("That archive is damaged.")?;
        if tar.len() as u64 > MAX_UNPACKED {
            bail!("That archive is too large once unpacked.");
        }
        return Ok(untar(&tar));
    }
    if bytes.starts_with(b"PK") {
        return unzip(bytes);
    }
    Ok(untar(bytes))
}

/// Regular files of a tar archive (ustar, GNU long names, pax paths).
fn untar(tar: &[u8]) -> Vec<(String, Vec<u8>)> {
    let text = |raw: &[u8]| {
        let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
        String::from_utf8_lossy(&raw[..end]).into_owned()
    };
    let mut out = Vec::new();
    let (mut long_name, mut pax_path): (Option<String>, Option<String>) = (None, None);
    let mut at = 0;
    while at + 512 <= tar.len() {
        let header = &tar[at..at + 512];
        if header.iter().all(|&b| b == 0) {
            break;
        }
        let size = usize::from_str_radix(text(&header[124..136]).trim(), 8).unwrap_or(0);
        let kind = header[156];
        let mut name = text(&header[..100]);
        // Only POSIX ustar headers have a name prefix (GNU keeps times there).
        if &header[257..263] == b"ustar\0" {
            let prefix = text(&header[345..500]);
            if !prefix.is_empty() {
                name = format!("{prefix}/{name}");
            }
        }
        let start = at + 512;
        let body = &tar[start.min(tar.len())..(start + size).min(tar.len())];
        at = start + size.div_ceil(512) * 512;
        match kind {
            b'L' => long_name = Some(text(body)),
            b'x' => {
                pax_path = String::from_utf8_lossy(body).lines().find_map(|record| record.split_once(" path=").map(|(_, path)| path.to_string()));
            }
            b'0' | 0 | b'7' => {
                let name = pax_path.take().or(long_name.take()).unwrap_or(name);
                if !name.ends_with('/') {
                    out.push((name.trim_start_matches("./").to_string(), body.to_vec()));
                }
            }
            _ => {
                long_name = None;
                pax_path = None;
            }
        }
    }
    out
}

/// Files of a zip archive, stored or deflated, each up to 5 MB.
fn unzip(zip: &[u8]) -> Result<Vec<(String, Vec<u8>)>> {
    use std::io::Read;
    let u16_at = |at: usize| zip.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]) as usize);
    let u32_at = |at: usize| zip.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize);
    let end = (0..zip.len().saturating_sub(21)).rev().take(65_557).find(|&at| u32_at(at) == Some(0x0605_4b50)).context("That zip file is damaged.")?;
    let count = u16_at(end + 10).unwrap_or(0);
    let mut at = u32_at(end + 16).unwrap_or(0);
    let mut out = Vec::new();
    let mut unpacked = 0u64;
    for _ in 0..count {
        if u32_at(at) != Some(0x0201_4b50) {
            break;
        }
        let (Some(method), Some(packed), Some(size), Some(name_len), Some(extra_len), Some(comment_len), Some(local)) =
            (u16_at(at + 10), u32_at(at + 20), u32_at(at + 24), u16_at(at + 28), u16_at(at + 30), u16_at(at + 32), u32_at(at + 42))
        else {
            break;
        };
        let name = String::from_utf8_lossy(zip.get(at + 46..at + 46 + name_len).unwrap_or_default()).into_owned();
        at += 46 + name_len + extra_len + comment_len;
        if name.ends_with('/') || size > 5_000_000 {
            continue;
        }
        let (Some(local_name), Some(local_extra)) = (u16_at(local + 26), u16_at(local + 28)) else { continue };
        let data_at = local + 30 + local_name + local_extra;
        let Some(raw) = zip.get(data_at..data_at + packed) else { continue };
        let data = match method {
            0 => raw.to_vec(),
            8 => {
                let mut data = Vec::new();
                if flate2::read::DeflateDecoder::new(raw).take(5_000_001).read_to_end(&mut data).is_err() {
                    continue;
                }
                data
            }
            _ => continue,
        };
        unpacked += data.len() as u64;
        if unpacked > MAX_UNPACKED {
            bail!("That archive is too large once unpacked.");
        }
        out.push((name, data));
    }
    Ok(out)
}

/// What `aihat skills add` accepts: a folder, a GitHub link, `owner/repo/skill`
/// (as skills.sh names them) or just a skill's name to look up.
pub fn load_skill_ref(input: &str) -> Result<Loaded> {
    let trimmed = input.trim();
    if std::path::Path::new(trimmed).exists() || trimmed.starts_with("~/") || crate::skills::parse_github(trimmed).is_some() {
        return crate::skills::load(trimmed);
    }
    let parts: Vec<&str> = trimmed.split('/').collect();
    if parts.len() == 3 && parts.iter().all(|p| !p.is_empty()) {
        return load_skill(&SkillListing {
            name: parts[2].into(),
            description: None,
            source: format!("{}/{}", parts[0], parts[1]),
            dir: None,
            installs: None,
            stars: None,
            from: SkillFrom::SkillsSh,
            skill_id: Some(parts[2].into()),
            site: None,
        });
    }
    // A name: the best match on skills.sh.
    let found = skills_sh(trimmed)?;
    let best = found
        .iter()
        .find(|item| item.name.eq_ignore_ascii_case(trimmed) || item.skill_id.as_deref().is_some_and(|id| id.eq_ignore_ascii_case(trimmed)))
        .or(found.first())
        .with_context(|| format!("No skill named {trimmed} was found. Try: aihat skills search {trimmed}"))?;
    load_skill(best)
}

// ------------------------------------------------------------------ MCP servers

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Variable {
    pub description: String,
    pub required: bool,
    pub secret: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HeaderSpec {
    pub name: String,
    pub description: String,
    pub required: bool,
    pub secret: bool,
    /// `Bearer {token}`
    pub value: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Remote {
    /// streamable-http or sse
    pub kind: String,
    pub url: String,
    pub headers: Vec<HeaderSpec>,
}

/// A server published as a program: npm, PyPI or Docker.
#[derive(Clone, Debug, PartialEq)]
pub struct Package {
    pub registry: String,
    pub identifier: String,
    pub version: String,
    pub env: Vec<(String, Variable)>,
    pub args: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct McpListing {
    /// The registry's name (`ai.smithery/github`), unique.
    pub id: String,
    pub title: String,
    pub description: String,
    pub publisher: String,
    pub website: Option<String>,
    pub category: Option<String>,
    pub remote: Option<Remote>,
    pub package: Option<Package>,
    pub featured: bool,
}

impl McpListing {
    /// A short name for the server once added: "deepwiki", "github".
    pub fn short_name(&self) -> String {
        let base = if self.featured { self.title.clone() } else { self.id.rsplit('/').next().unwrap_or(&self.title).to_string() };
        let slug: String = base
            .to_lowercase()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        slug.split('-').filter(|part| !part.is_empty()).collect::<Vec<_>>().join("-")
    }

    /// "Streamable HTTP" · "runs here · npm"
    pub fn connection_label(&self) -> String {
        match (&self.remote, &self.package) {
            (Some(remote), _) if remote.kind == "sse" => "SSE (older)".into(),
            (Some(_), _) => "Streamable HTTP".into(),
            (None, Some(package)) => format!("runs on this computer · {}", package.registry),
            _ => String::new(),
        }
    }
}

fn featured(id: &str, title: &str, category: &str, description: &str, publisher: &str, url: &str, header: Option<HeaderSpec>) -> McpListing {
    McpListing {
        id: format!("featured/{id}"),
        title: title.into(),
        description: description.into(),
        publisher: publisher.into(),
        website: None,
        category: Some(category.into()),
        remote: Some(Remote { kind: "streamable-http".into(), url: url.into(), headers: header.into_iter().collect() }),
        package: None,
        featured: true,
    }
}

/// Hosted servers that work without a browser sign-in (or take a plain
/// token), listed first so there is something useful to add straight away.
pub fn featured_mcp() -> Vec<McpListing> {
    let token = |description: &str, required: bool| HeaderSpec {
        name: "Authorization".into(),
        description: description.into(),
        required,
        secret: true,
        value: Some("Bearer {token}".into()),
    };
    vec![
        featured("deepwiki", "DeepWiki", "Docs", "Ask questions about any public GitHub repository: structure, docs and how things work.", "deepwiki.com", "https://mcp.deepwiki.com/mcp", None),
        featured("context7", "Context7", "Docs", "Up-to-date documentation and code examples for libraries, pulled in as you ask.", "context7.com", "https://mcp.context7.com/mcp", None),
        featured("microsoft-learn", "Microsoft Learn", "Docs", "Search and read official Microsoft and Azure documentation.", "learn.microsoft.com", "https://learn.microsoft.com/api/mcp", None),
        featured("cloudflare-docs", "Cloudflare Docs", "Docs", "Search the Cloudflare developer documentation.", "cloudflare.com", "https://docs.mcp.cloudflare.com/mcp", None),
        featured("exa", "Exa Search", "Search", "Web search and page fetching built for AI assistants. Works without a key at a low rate limit.", "exa.ai", "https://mcp.exa.ai/mcp", None),
        featured("huggingface", "Hugging Face", "AI", "Search models, datasets, Spaces and papers on the Hugging Face Hub. A token unlocks more.", "huggingface.co", "https://huggingface.co/mcp", Some(token("Optional Hugging Face access token (hf_…)", false))),
        featured("coingecko", "CoinGecko", "Data", "Crypto prices, market data and charts.", "coingecko.com", "https://mcp.api.coingecko.com/mcp", None),
        featured("github", "GitHub", "Dev", "Search code, read issues and pull requests, and manage repositories. Needs a personal access token.", "github.com", "https://api.githubcopilot.com/mcp/", Some(token("A GitHub personal access token", true))),
    ]
}

fn reverse_domain(name: &str) -> String {
    let host = name.split('/').next().unwrap_or(name);
    host.split('.').rev().collect::<Vec<_>>().join(".")
}

fn variables(value: Option<&Value>) -> Vec<(String, Variable)> {
    match value {
        Some(Value::Array(list)) => list
            .iter()
            .filter_map(|v| {
                Some((
                    v.get("name")?.as_str()?.to_string(),
                    Variable {
                        description: v.get("description").and_then(Value::as_str).unwrap_or("").to_string(),
                        required: v.get("isRequired").and_then(Value::as_bool).unwrap_or(false),
                        secret: v.get("isSecret").and_then(Value::as_bool).unwrap_or(false),
                    },
                ))
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// One registry entry, or `None` when there is no way to reach it.
pub fn normalize_registry_entry(entry: &Value) -> Option<McpListing> {
    let server = entry.get("server")?;
    let id = server.get("name")?.as_str()?.to_string();
    let remotes = server.get("remotes").and_then(Value::as_array).cloned().unwrap_or_default();
    let remote = remotes
        .iter()
        .find(|r| r.get("type").and_then(Value::as_str) == Some("streamable-http"))
        .or_else(|| remotes.iter().find(|r| r.get("type").and_then(Value::as_str) == Some("sse")))
        .and_then(|r| {
            Some(Remote {
                kind: r.get("type")?.as_str()?.to_string(),
                url: r.get("url")?.as_str()?.to_string(),
                headers: r
                    .get("headers")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|h| {
                        Some(HeaderSpec {
                            name: h.get("name")?.as_str()?.to_string(),
                            description: h.get("description").and_then(Value::as_str).unwrap_or("").to_string(),
                            required: h.get("isRequired").and_then(Value::as_bool).unwrap_or(false),
                            secret: h.get("isSecret").and_then(Value::as_bool).unwrap_or(true),
                            value: h.get("value").and_then(Value::as_str).map(str::to_owned),
                        })
                    })
                    .collect(),
            })
        });
    let package = server
        .get("packages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|p| p.pointer("/transport/type").and_then(Value::as_str).is_none_or(|t| t == "stdio"))
        .find_map(|p| {
            let registry = p.get("registryType")?.as_str()?.to_string();
            if !matches!(registry.as_str(), "npm" | "pypi" | "oci") {
                return None;
            }
            let args = p
                .get("packageArguments")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|a| {
                    let value = a.get("value").and_then(Value::as_str)?;
                    Some(match (a.get("type").and_then(Value::as_str), a.get("name").and_then(Value::as_str)) {
                        (Some("named"), Some(name)) => format!("{name} {value}"),
                        _ => value.to_string(),
                    })
                })
                .collect();
            Some(Package {
                registry,
                identifier: p.get("identifier")?.as_str()?.to_string(),
                version: p.get("version").and_then(Value::as_str).unwrap_or("").to_string(),
                env: variables(p.get("environmentVariables")),
                args,
            })
        });
    if remote.is_none() && package.is_none() {
        return None;
    }
    Some(McpListing {
        title: server
            .get("title")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| id.rsplit('/').next().unwrap_or(&id).to_string()),
        description: server.get("description").and_then(Value::as_str).unwrap_or("").trim().to_string(),
        publisher: reverse_domain(&id),
        website: server.get("websiteUrl").and_then(Value::as_str).map(str::to_owned),
        category: None,
        remote,
        package,
        featured: false,
        id,
    })
}

/// One page of the MCP registry (and the cursor for the next).
pub fn search_mcp(query: &str, cursor: Option<&str>) -> Result<(Vec<McpListing>, Option<String>)> {
    let mut items = Vec::new();
    let mut next = cursor.map(str::to_owned);
    for _ in 0..3 {
        let mut url = "https://registry.modelcontextprotocol.io/v0.1/servers?limit=60&version=latest".to_string();
        if !query.trim().is_empty() {
            url.push_str(&format!("&search={}", urlencoding::encode(query.trim())));
        }
        if let Some(cursor) = &next {
            url.push_str(&format!("&cursor={}", urlencoding::encode(cursor)));
        }
        let data = get_json(&url, "The MCP registry")?;
        for entry in data.get("servers").and_then(Value::as_array).into_iter().flatten() {
            if let Some(item) = normalize_registry_entry(entry)
                && !items.iter().any(|i: &McpListing| i.id == item.id)
            {
                items.push(item);
            }
        }
        next = data.pointer("/metadata/nextCursor").and_then(Value::as_str).map(str::to_owned).filter(|c| !c.is_empty());
        if next.is_none() || items.len() >= 20 {
            break;
        }
    }
    Ok((items, next))
}

/// Featured servers matching a query, then the registry's.
pub fn find_mcp(query: &str) -> Result<Vec<McpListing>> {
    let q = query.to_lowercase();
    let mut items: Vec<McpListing> = featured_mcp()
        .into_iter()
        .filter(|f| q.is_empty() || format!("{} {} {}", f.title, f.description, f.category.clone().unwrap_or_default()).to_lowercase().contains(&q))
        .collect();
    let (registry, _) = search_mcp(query, None)?;
    items.extend(registry);
    Ok(items)
}

/// How a listing would be added: its address or command, the header and
/// token it takes, and the values it needs from you.
#[derive(Clone, Debug, Default)]
pub struct McpPlan {
    pub supported: bool,
    pub reason: String,
    /// http or stdio
    pub transport: String,
    /// The address, or the command line for stdio.
    pub target: String,
    pub header_name: Option<String>,
    pub header_template: Option<String>,
    /// The token the header carries, and whether it is needed.
    pub token: Option<(String, bool)>,
    /// Settings the program reads from its environment: name, what it is, required, secret.
    pub env: Vec<(String, Variable)>,
}

pub fn plan_mcp(item: &McpListing) -> McpPlan {
    if let Some(remote) = &item.remote
        && remote.kind == "streamable-http"
    {
        let headers = &remote.headers;
        let required: Vec<_> = headers.iter().filter(|h| h.required).collect();
        if headers.len() > 1 && required.len() != 1 {
            return McpPlan { reason: "This server needs several custom headers, which AI Hat can't send yet.".into(), ..McpPlan::default() };
        }
        if remote.url.contains('{') {
            return McpPlan { reason: "This server's address needs values filled in; add it by hand with its full address.".into(), ..McpPlan::default() };
        }
        let header = required.first().copied().or(headers.first());
        let (header_name, header_template, token) = match header {
            Some(h) => {
                let template = h.value.clone().map(|v| {
                    let start = v.find('{');
                    let end = v.find('}');
                    match (start, end) {
                        (Some(s), Some(e)) if e > s => format!("{}{{token}}{}", &v[..s], &v[e + 1..]),
                        _ => v,
                    }
                });
                (Some(h.name.clone()), Some(template.unwrap_or_else(|| "{token}".into())), Some((if h.description.is_empty() { h.name.clone() } else { h.description.clone() }, h.required)))
            }
            None => (None, None, None),
        };
        return McpPlan { supported: true, reason: String::new(), transport: "http".into(), target: remote.url.clone(), header_name, header_template, token, env: Vec::new() };
    }
    if let Some(package) = &item.package {
        let pinned = |sep: &str| if package.version.is_empty() { package.identifier.clone() } else { format!("{}{sep}{}", package.identifier, package.version) };
        let mut command = match package.registry.as_str() {
            "npm" => format!("npx -y {}", pinned("@")),
            "pypi" => format!("uvx {}", pinned("==")),
            _ => format!("docker run -i --rm {}", pinned(":")),
        };
        for arg in &package.args {
            command.push(' ');
            command.push_str(arg);
        }
        return McpPlan { supported: true, reason: String::new(), transport: "stdio".into(), target: command, header_name: None, header_template: None, token: None, env: package.env.clone() };
    }
    McpPlan { reason: "This server uses the older SSE connection, which AI Hat doesn't speak yet.".into(), ..McpPlan::default() }
}

/// Add a listing as a server. The token (if any) and the program's settings
/// are kept with your keys. Returns the new server's id; connect it after.
pub fn add_mcp(
    db: &crate::db::Database,
    secrets: &crate::secrets::Secrets,
    item: &McpListing,
    plan: &McpPlan,
    token: Option<&str>,
    env: &[(String, String)],
) -> Result<String> {
    if !plan.supported {
        bail!("{}", plan.reason);
    }
    let mut name = item.short_name();
    let taken: Vec<String> = db.mcp_servers()?.into_iter().map(|s| s.name.to_lowercase()).collect();
    if taken.contains(&name) {
        name = (2..).map(|n| format!("{name}-{n}")).find(|n| !taken.contains(n)).unwrap_or(name);
    }
    let token = token.map(str::trim).filter(|t| !t.is_empty());
    let auth = if token.is_some() { "token" } else { "none" };
    let id = db.add_mcp_server(&name, &plan.target, &plan.transport, auth)?;
    if plan.header_name.is_some() {
        db.set_mcp_auth_header(&id, plan.header_name.as_deref(), plan.header_template.as_deref())?;
    }
    if let Some(token) = token {
        secrets.set_mcp_token(&id, token)?;
    }
    let env: serde_json::Map<String, Value> = env.iter().filter(|(_, v)| !v.trim().is_empty()).map(|(k, v)| (k.clone(), Value::String(v.trim().to_string()))).collect();
    if !env.is_empty() {
        secrets.set(&format!("mcp.{id}.env"), &Value::Object(env).to_string())?;
    }
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn compact_numbers_read_like_the_app() {
        assert_eq!(compact(950), "950");
        assert_eq!(compact(5_400), "5.4k");
        assert_eq!(compact(23_000), "23k");
        assert_eq!(compact(1_200_000), "1.2M");
    }

    #[test]
    fn registry_entries_become_listings() {
        let remote = json!({"server": {"name": "com.notion/mcp", "description": "Notion", "remotes": [{"type": "streamable-http", "url": "https://mcp.notion.com/mcp"}]}});
        let item = normalize_registry_entry(&remote).unwrap();
        assert_eq!(item.publisher, "notion.com");
        let plan = plan_mcp(&item);
        assert!(plan.supported);
        assert_eq!(plan.transport, "http");

        let program = json!({"server": {"name": "io.github.x/files", "packages": [{"registryType": "npm", "identifier": "@x/files", "version": "1.2.0", "transport": {"type": "stdio"},
            "environmentVariables": [{"name": "ROOT", "isRequired": true}]}]}});
        let plan = plan_mcp(&normalize_registry_entry(&program).unwrap());
        assert_eq!(plan.transport, "stdio");
        assert_eq!(plan.target, "npx -y @x/files@1.2.0");
        assert_eq!(plan.env[0].0, "ROOT");

        let token = featured_mcp().into_iter().find(|f| f.title == "GitHub").unwrap();
        let plan = plan_mcp(&token);
        assert_eq!(plan.header_template.as_deref(), Some("Bearer {token}"));
        assert!(plan.token.as_ref().unwrap().1);
        assert_eq!(token.short_name(), "github");
    }
}
