//! `/market`: find skills and MCP servers to add, as on the phone app's
//! Marketplace, in the two-pane layout of `/providers`: results on the
//! left, the selected one on the right.

use super::app::{App, Page, Task};
use super::dialogs::{self, Dialog};
use super::draw::{self, put, put_line};
use crate::market::{McpListing, SkillListing, skill_added};
use crate::ui::markdown::wrap_segments;
use crate::ui::text::{self, width};
use crate::ui::theme::{Role, theme};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use std::collections::HashMap;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Skills,
    Mcp,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Popular,
    SkillsSh,
    /// A GitHub repository, or a website that publishes skills.
    Site,
}

const SOURCES: [&str; 3] = ["Popular", "skills.sh", "Repo or site"];
const PAGE: usize = 25;
const IDEAS: [&str; 8] = ["pdf", "react", "testing", "design", "git", "writing", "sql", "docs"];

/// Skills published by the companies themselves: (site, name, what).
const SITES: [(&str, &str, &str); 5] = [
    ("vercel.com", "Vercel", "Deploy, React and web design"),
    ("developers.cloudflare.com", "Cloudflare", "Workers, agents, storage and more"),
    ("docs.stripe.com", "Stripe", "Payments, Connect and billing"),
    ("supabase.com", "Supabase", "Postgres and Supabase projects"),
    ("agentskills.io", "Agent Skills", "Writing and testing skills"),
];

/// Repositories that publish skills: (owner/repo, name, what).
const REPOS: [(&str, &str, &str); 4] = [
    ("anthropics/skills", "Anthropic", "Documents, design, MCP builder and more"),
    ("openai/skills", "OpenAI Codex", "Curated skills for Codex"),
    ("vercel-labs/agent-skills", "Vercel", "React, web design and deploy"),
    ("obra/superpowers", "Superpowers", "Planning, debugging, testing workflows"),
];

#[derive(Clone)]
pub enum Item {
    Skill(SkillListing),
    Mcp(McpListing),
    /// Something to search for: an idea, a website or a repository.
    Suggestion { title: String, sub: String, meta: String, query: String },
}

impl Item {
    fn key(&self) -> String {
        match self {
            Item::Skill(s) => format!("s:{}:{}:{}", s.source, s.dir.clone().unwrap_or_default(), s.skill_id.clone().unwrap_or_default()),
            Item::Mcp(m) => format!("m:{}", m.id),
            Item::Suggestion { query, .. } => format!("q:{query}"),
        }
    }

    fn name(&self) -> String {
        match self {
            Item::Skill(s) => s.name.clone(),
            Item::Mcp(m) => m.title.clone(),
            Item::Suggestion { title, .. } => title.clone(),
        }
    }

    fn sub(&self) -> String {
        match self {
            Item::Skill(s) => s.description.clone().unwrap_or_else(|| s.source.clone()),
            Item::Mcp(m) => m.description.clone(),
            Item::Suggestion { sub, .. } => sub.clone(),
        }
    }
}

/// A page of results and what to call them.
pub struct Found {
    pub items: Vec<Item>,
    pub title: String,
    /// Where the next page starts, when there is one.
    pub more: Option<usize>,
    /// Added after the results already shown.
    pub append: bool,
}

/// What the right pane shows once it has been looked up.
#[derive(Clone)]
pub enum Detail {
    Loading,
    /// The skill's instructions and how many files come with it.
    Skill { body: String, files: usize },
    /// How the server wants you to sign in.
    Mcp(crate::oauth::Need),
    Failed(String),
}

pub struct MarketPage {
    pub tab: Tab,
    pub source: Source,
    pub query: String,
    /// The query the shown results are for.
    applied: String,
    pub selection: usize,
    pub items: Vec<Item>,
    title: String,
    more: Option<usize>,
    pub loading: bool,
    error: Option<String>,
    /// Bumped for every search, so a late answer to an old one is ignored.
    token: u64,
    search_due: Option<Instant>,
    detail_due: Option<Instant>,
    /// Rows the details pane is scrolled down (PgUp/PgDn).
    detail_scroll: usize,
    details: HashMap<String, Detail>,
}

impl MarketPage {
    pub fn new(tab: Tab) -> Self {
        Self {
            tab,
            source: Source::Popular,
            query: String::new(),
            applied: String::new(),
            selection: 0,
            items: Vec::new(),
            title: String::new(),
            more: None,
            loading: false,
            error: None,
            token: 0,
            // The first list loads straight away.
            search_due: Some(Instant::now()),
            detail_due: None,
            detail_scroll: 0,
            details: HashMap::new(),
        }
    }

    fn selected(&self) -> Option<&Item> {
        self.items.get(self.selection)
    }

    /// Typing waits a moment so every key doesn't become a request. A
    /// repository or website is looked up on enter instead.
    fn typed(&mut self) {
        if self.tab == Tab::Skills && self.source == Source::Site && !self.query.trim().is_empty() {
            self.search_due = None;
        } else {
            self.search_due = Some(Instant::now() + Duration::from_millis(350));
        }
    }

    fn search_now(&mut self) {
        self.search_due = Some(Instant::now());
    }

    fn reset(&mut self) {
        self.query.clear();
        self.items.clear();
        self.selection = 0;
        self.error = None;
        self.more = None;
        self.search_now();
    }

    fn detail_soon(&mut self) {
        self.detail_due = Some(Instant::now() + Duration::from_millis(300));
    }
}

/// Start a search or a lookup whose time has come. Returns whether the
/// screen changed.
pub fn tick(app: &mut App) -> bool {
    let Page::Market(page) = &mut app.page else {
        return false;
    };
    let now = Instant::now();
    if page.search_due.is_some_and(|due| now >= due) {
        page.search_due = None;
        start_search(app, 0);
        return true;
    }
    if page.detail_due.is_some_and(|due| now >= due) {
        page.detail_due = None;
        let Some(item) = page.selected().cloned() else {
            return false;
        };
        let key = item.key();
        if matches!(item, Item::Suggestion { .. }) || page.details.contains_key(&key) {
            return false;
        }
        page.details.insert(key.clone(), Detail::Loading);
        app.spawn_task(move || Task::MarketDetail { key, detail: look_up(&item) });
        return true;
    }
    false
}

/// Search from the start (`offset` 0) or load the next page.
fn start_search(app: &mut App, offset: usize) {
    let Page::Market(page) = &mut app.page else {
        return;
    };
    page.token += 1;
    page.loading = true;
    page.error = None;
    page.applied = page.query.trim().to_string();
    let (token, tab, source, query) = (page.token, page.tab, page.source, page.applied.clone());
    app.spawn_task(move || Task::MarketResults { token, result: search(tab, source, &query, offset).map_err(|e| format!("{e:#}")) });
}

fn suggestion(title: &str, sub: &str, meta: &str, query: &str) -> Item {
    Item::Suggestion { title: title.into(), sub: sub.into(), meta: meta.into(), query: query.into() }
}

fn search(tab: Tab, source: Source, query: &str, offset: usize) -> anyhow::Result<Found> {
    let found = |items: Vec<Item>, title: &str| Found { items, title: title.into(), more: None, append: false };
    Ok(match (tab, source) {
        (Tab::Mcp, _) => {
            let items = crate::market::find_mcp(query)?.into_iter().map(Item::Mcp).collect();
            found(items, if query.is_empty() { "Featured servers, then the MCP registry" } else { "Results" })
        }
        (Tab::Skills, Source::Popular) => {
            let (items, total) = crate::market::popular_skills(query, offset, PAGE)?;
            let next = offset + items.len();
            let more = (!items.is_empty() && (next as u64) < total).then_some(next);
            let title = if query.is_empty() { "Popular skills".to_string() } else { format!("Results · {}", thousands(total)) };
            Found { items: items.into_iter().map(Item::Skill).collect(), title, more, append: offset > 0 }
        }
        (Tab::Skills, Source::SkillsSh) if query.chars().count() < 2 => {
            found(IDEAS.iter().map(|idea| suggestion(idea, "", "", idea)).collect(), "Try searching for")
        }
        (Tab::Skills, Source::SkillsSh) => found(crate::market::skills_sh(query)?.into_iter().map(Item::Skill).collect(), "Results"),
        (Tab::Skills, Source::Site) if query.is_empty() => {
            let sites = SITES.iter().map(|(site, name, what)| suggestion(name, what, site, site));
            let repos = REPOS.iter().map(|(repo, name, what)| suggestion(name, what, repo, repo));
            found(sites.chain(repos).collect(), "Published by the companies themselves, and on GitHub")
        }
        (Tab::Skills, Source::Site) => {
            if let Some(repo) = crate::skills::parse_github(query) {
                found(crate::market::repo_skills(&repo)?.into_iter().map(Item::Skill).collect(), "Skills in this repository")
            } else if crate::market::parse_site_ref(query).is_some() {
                let items: Vec<Item> = crate::market::site_skills(query)?.into_iter().map(Item::Skill).collect();
                let host = items.first().map(|item| match item {
                    Item::Skill(skill) => skill.source.clone(),
                    _ => String::new(),
                });
                found(items, &format!("Skills from {}", host.unwrap_or_default()))
            } else {
                anyhow::bail!("That doesn't look like a GitHub repository, a folder link or a website address.")
            }
        }
    })
}

/// 12345 → "12,345"
fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

fn look_up(item: &Item) -> Detail {
    match item {
        Item::Skill(skill) => match crate::market::preview_skill(skill) {
            Ok((body, files)) => Detail::Skill { body, files },
            Err(error) => Detail::Failed(format!("{error:#}")),
        },
        Item::Mcp(server) => match &server.remote {
            Some(remote) if remote.kind == "streamable-http" => Detail::Mcp(crate::oauth::probe(&remote.url)),
            _ => Detail::Mcp(crate::oauth::Need::Nothing),
        },
        Item::Suggestion { .. } => Detail::Loading,
    }
}

/// Results of a search arrived.
pub fn on_results(app: &mut App, token: u64, result: Result<Found, String>) {
    let Page::Market(page) = &mut app.page else {
        return;
    };
    if page.token != token {
        return;
    }
    page.loading = false;
    match result {
        Ok(found) => {
            if found.append {
                page.items.extend(found.items);
            } else {
                page.items = found.items;
                page.selection = 0;
                page.detail_scroll = 0;
                page.title = found.title;
            }
            page.more = found.more;
            page.error = None;
            page.detail_soon();
        }
        Err(error) => {
            page.items.clear();
            page.more = None;
            page.error = Some(error);
        }
    }
}

pub fn on_detail(app: &mut App, key: String, detail: Detail) {
    if let Page::Market(page) = &mut app.page {
        page.details.insert(key, detail);
    }
}

fn mcp_added(servers: &[crate::models::McpServer], server: &McpListing) -> bool {
    let plan = crate::market::plan_mcp(server);
    !plan.target.is_empty() && servers.iter().any(|s| s.url.trim_end_matches('/') == plan.target.trim_end_matches('/'))
}

pub fn draw(app: &App, buf: &mut Buffer, area: Rect) {
    let Page::Market(page) = &app.page else {
        return;
    };
    let t = theme();
    let top = area.y + 1;
    let height = area.bottom().saturating_sub(top);
    let left_width = (area.width * 11 / 20).clamp(46.min(area.width), 76);
    let left = Rect::new(area.x, top, left_width, height);
    draw::boxed(buf, left, t.fg(Role::Accent), "Marketplace", "");
    let inner = draw::inner(left);
    let w = inner.width as usize;
    let mut y = inner.y;

    // Skills · MCP servers, the search box, and for skills where they come from.
    let chosen = if page.tab == Tab::Skills { 0 } else { 1 };
    dialogs::draw_choices(buf, inner.x, y, inner.right(), &["Skills", "MCP servers"], chosen);
    y += 2;

    // The search box.
    let placeholder = match (page.tab, page.source) {
        (Tab::Mcp, _) => "Search MCP servers",
        (Tab::Skills, Source::Popular) => "Search skills",
        (Tab::Skills, Source::SkillsSh) => "Search skills.sh",
        (Tab::Skills, Source::Site) => "owner/repo, a GitHub link or vercel.com",
    };
    put(buf, inner.x, y, "›", t.fg(Role::Accent), inner.right());
    if page.query.is_empty() {
        put(buf, inner.x + 2, y, placeholder, t.fg(Role::Dim), inner.right());
        if let Some(cell) = buf.cell_mut((inner.x + 2, y)) {
            cell.set_style(t.cursor());
        }
    } else {
        let shown = text::truncate_left(&page.query, w.saturating_sub(4));
        let end = put(buf, inner.x + 2, y, &shown, t.fg(Role::Strong), inner.right());
        put(buf, end, y, " ", t.cursor(), inner.right());
        if page.tab == Tab::Skills && page.source == Source::Site && page.query.trim() != page.applied {
            let hint = "enter to look";
            put(buf, inner.right().saturating_sub(width(hint) as u16), y, hint, t.fg(Role::Dim), inner.right());
        }
    }
    y += 1;
    if page.tab == Tab::Skills {
        let source = match page.source {
            Source::Popular => 0,
            Source::SkillsSh => 1,
            Source::Site => 2,
        };
        y += 1;
        dialogs::draw_choices(buf, inner.x, y, inner.right(), &SOURCES, source);
        y += 1;
    }
    y += 1;
    let caption = match (page.tab, page.source) {
        (Tab::Mcp, _) => "Hosted servers that work straight away, then the official MCP Registry. Add only servers you trust.",
        (Tab::Skills, Source::Popular) => "The most-installed skills from public GitHub repositories, listed by claude-plugins.dev. You see the full text before adding anything.",
        (Tab::Skills, Source::SkillsSh) => "Search skills.sh, the open skills directory from Vercel. You see the full text before adding anything.",
        (Tab::Skills, Source::Site) => "A public GitHub repository or folder with SKILL.md files, or a website that publishes skills itself.",
    };
    for row in wrap_segments(&[(caption.to_string(), t.fg(Role::Dim))], w).into_iter().take(2) {
        put_line(buf, inner.x, y, &Line::from(row), inner.right());
        y += 1;
    }
    y += 1;

    // Results, under their title.
    let hint_y = inner.bottom().saturating_sub(1);
    let title = if page.error.is_some() { "" } else { page.title.as_str() };
    put(buf, inner.x, y, title, t.bold(Role::Strong), inner.right());
    if page.loading {
        let loading = if page.items.is_empty() { "Loading…" } else { "loading…" };
        put(buf, inner.right().saturating_sub(width(loading) as u16), y, loading, t.fg(Role::Dim), inner.right());
    }
    y += 1;
    let rows = hint_y.saturating_sub(y + 1) as usize;
    if let Some(error) = &page.error {
        for (offset, row) in wrap_segments(&[(error.clone(), t.fg(Role::Warn))], w).into_iter().take(3).enumerate() {
            put_line(buf, inner.x, y + offset as u16, &Line::from(row), inner.right());
        }
    } else if page.items.is_empty() && !page.loading {
        put(buf, inner.x, y, "Nothing found", t.fg(Role::Dim), inner.right());
    }
    let library = app.db.skills().unwrap_or_default();
    let servers = app.db.mcp_servers().unwrap_or_default();
    let first = (page.selection + 1).saturating_sub(rows.max(1));
    for (offset, item) in page.items.iter().enumerate().skip(first).take(rows) {
        let selected = offset == page.selection;
        let (badge, badge_role) = match item {
            Item::Skill(skill) if skill_added(&library, skill) => ("added".to_string(), Role::Accent),
            Item::Mcp(server) if mcp_added(&servers, server) => ("added".to_string(), Role::Accent),
            Item::Suggestion { meta, .. } => (meta.clone(), Role::Dim),
            _ => (String::new(), Role::Dim),
        };
        let name_style = if selected { t.fg(Role::Accent) } else { t.fg(Role::Strong) };
        let badge_width = if badge.is_empty() { 0 } else { width(&badge).min(24) + 2 };
        let room = w.saturating_sub(22 + badge_width);
        let sub = item.sub().replace('\n', " ");
        let left = vec![
            Span::styled(text::pad(&text::truncate(&item.name(), 20), 22), name_style),
            Span::styled(text::truncate(&sub, room), if selected { t.fg(Role::Text) } else { t.fg(Role::Dim) }),
        ];
        let right = vec![Span::styled(text::truncate(&badge, 24), t.fg(badge_role))];
        draw::row(buf, inner, y + (offset - first) as u16, text::spread(left, right, w), selected);
    }
    let hint = match page.tab {
        Tab::Skills => "tab skills/MCP · ←→ source · enter add · ctrl+g add for all agents · esc back",
        Tab::Mcp => "tab skills/MCP · ↑↓ move · enter add and connect · esc back",
    };
    put(buf, inner.x, hint_y, &text::truncate(hint, w), t.fg(Role::Dim), inner.right());

    let right_x = left.right() + 1;
    if right_x + 20 < area.right() {
        draw_detail(app, page, buf, Rect::new(right_x, top, area.right() - right_x, height));
    }
}

/// The selected item, with what it would add.
fn draw_detail(app: &App, page: &MarketPage, buf: &mut Buffer, area: Rect) {
    let t = theme();
    let Some(item) = page.selected() else {
        draw::boxed(buf, area, t.fg(Role::Dim), "Details", "");
        return;
    };
    draw::boxed(buf, area, t.fg(Role::Dim), &item.name(), "");
    let inner = draw::inner(area);
    let w = inner.width as usize;
    let last = inner.bottom().saturating_sub(1);
    let mut y = inner.y;
    let write = |buf: &mut Buffer, y: &mut u16, line: Line<'static>| {
        if *y < last.saturating_sub(1) {
            put_line(buf, inner.x, *y, &text::clip_line(line, w), inner.right());
        }
        *y += 1;
    };
    let wrap = |text: &str, role: Role, max: usize| -> Vec<Line<'static>> {
        wrap_segments(&[(text.replace('\n', " "), t.fg(role))], w).into_iter().take(max).map(Line::from).collect()
    };
    let warn = |buf: &mut Buffer, y: &mut u16, text: &str| {
        for (index, mut line) in wrap(text, Role::Dim, 4).into_iter().enumerate() {
            if index == 0 {
                line.spans.insert(0, Span::styled("! ", t.fg(Role::Warn)));
            }
            write(buf, y, line);
        }
    };
    let markdown = |source: &str| {
        crate::ui::markdown::render(source, crate::ui::markdown::MdOptions { width: w, indent: 0, block_indent: 1, wide: w })
    };
    let detail = page.details.get(&item.key()).cloned();
    match item {
        Item::Suggestion { title, sub, meta, .. } => {
            let place = if meta.contains('/') { format!("github.com/{meta}") } else { meta.clone() };
            write(buf, &mut y, Line::styled(place, t.fg(Role::Link)));
            y += 1;
            if !sub.is_empty() {
                for line in wrap(sub, Role::Text, 3) {
                    write(buf, &mut y, line);
                }
                y += 1;
            }
            let what = if meta.is_empty() { format!("Search skills.sh for “{title}”.") } else { format!("Show the skills {title} publishes.") };
            write(buf, &mut y, Line::styled(what, t.fg(Role::Dim)));
            put(buf, inner.x, last, "enter look", t.fg(Role::Dim), inner.right());
        }
        Item::Skill(skill) => {
            write(buf, &mut y, Line::styled(crate::market::skill_meta(skill), t.fg(Role::Dim)));
            write(buf, &mut y, Line::styled(format!("Listed on {}", skill.from.label()), t.fg(Role::Dim)));
            y += 1;
            if let Some(description) = &skill.description {
                for line in markdown(description).into_iter().take(5) {
                    write(buf, &mut y, line);
                }
                y += 1;
            }
            warn(buf, &mut y, "Anyone can publish a skill, and its text steers how the assistant behaves. Read it before you add it. Files that come with it are kept as text; scripts never run.");
            y += 1;
            match detail {
                Some(Detail::Skill { body, files }) => {
                    let title = if files > 0 { format!("Instructions (SKILL.md) · {}", text::plural(files, "file", "files")) } else { "Instructions (SKILL.md)".into() };
                    write(buf, &mut y, Line::styled(title, t.fg(Role::Strong)));
                    y += 1;
                    // SKILL.md drawn as in a reply: headings, lists, tables, code.
                    let rendered = markdown(&body);
                    let room = last.saturating_sub(y + 1) as usize;
                    let skip = page.detail_scroll.min(rendered.len().saturating_sub(room.max(1)));
                    let total = rendered.len();
                    for line in rendered.into_iter().skip(skip) {
                        if y + 1 >= last {
                            break;
                        }
                        write(buf, &mut y, line);
                    }
                    if total > room {
                        let shown = format!("{}–{} of {} rows · pgup/pgdn", skip + 1, (skip + room).min(total), total);
                        put(buf, inner.right().saturating_sub(text::width(&shown) as u16), area.y, &format!(" {shown} "), t.fg(Role::Dim), inner.right() + 1);
                    }
                }
                Some(Detail::Failed(error)) => {
                    for line in wrap(&error, Role::Warn, 3) {
                        write(buf, &mut y, line);
                    }
                }
                _ => write(buf, &mut y, Line::styled("Reading SKILL.md…", t.fg(Role::Dim))),
            }
            put(buf, inner.x, last, &text::truncate("enter add · ctrl+g add for all agents (~/.agents/skills)", w), t.fg(Role::Dim), inner.right());
        }
        Item::Mcp(server) => {
            let plan = crate::market::plan_mcp(server);
            let by = match &server.category {
                Some(category) => format!("{} · {category}", server.publisher),
                None => server.publisher.clone(),
            };
            write(buf, &mut y, Line::styled(by, t.fg(Role::Dim)));
            y += 1;
            for line in markdown(&server.description).into_iter().take(6) {
                write(buf, &mut y, line);
            }
            y += 1;
            let pair = |label: &str, value: String| Line::from(vec![Span::styled(text::pad(label, 12), t.fg(Role::Dim)), Span::styled(value, t.fg(Role::Text))]);
            if plan.supported {
                let label = if plan.transport == "stdio" { "Command" } else { "Address" };
                write(buf, &mut y, pair(label, plan.target.clone()));
                write(buf, &mut y, pair("Connection", server.connection_label()));
                let sign_in = match (&plan.token, &detail) {
                    (Some((_, true)), _) => "an access token",
                    (_, Some(Detail::Mcp(crate::oauth::Need::SignIn))) => "your browser (opens when you add it)",
                    (_, Some(Detail::Mcp(crate::oauth::Need::Token))) => "an access token",
                    (Some((_, false)), _) => "none · a token unlocks more",
                    (_, Some(Detail::Mcp(_))) => "none",
                    _ => "checking…",
                };
                write(buf, &mut y, pair("Sign-in", sign_in.to_string()));
                for (var, about) in plan.env.iter().filter(|(_, v)| v.required) {
                    let what = if about.description.is_empty() { var.clone() } else { format!("{var} · {}", about.description) };
                    write(buf, &mut y, pair("Needs", what));
                }
            } else {
                for line in wrap(&format!("Can't be added: {}", plan.reason), Role::Warn, 3) {
                    write(buf, &mut y, line);
                }
            }
            y += 1;
            warn(buf, &mut y, "Anyone can publish to the registry. Add only servers you trust: what the assistant sends to a tool goes to that server. Tools that write or delete ask you first.");
            put(buf, inner.x, last, "enter add and connect", t.fg(Role::Dim), inner.right());
        }
    }
    let _ = app;
}

pub fn on_key(app: &mut App, key: KeyEvent) {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let Page::Market(page) = &mut app.page else {
        return;
    };
    let moved = |page: &mut MarketPage, to: usize| {
        page.detail_scroll = 0;
        page.selection = to.min(page.items.len().saturating_sub(1));
        page.detail_soon();
    };
    match key.code {
        KeyCode::Esc if !page.query.is_empty() => {
            page.query.clear();
            page.typed();
        }
        KeyCode::Esc => app.page = Page::Chat,
        KeyCode::Tab | KeyCode::BackTab => {
            page.tab = if page.tab == Tab::Skills { Tab::Mcp } else { Tab::Skills };
            page.reset();
        }
        KeyCode::Left | KeyCode::Right if page.tab == Tab::Skills => {
            let order = [Source::Popular, Source::SkillsSh, Source::Site];
            let at = order.iter().position(|s| *s == page.source).unwrap_or(0);
            let next = if key.code == KeyCode::Right { (at + 1) % 3 } else { (at + 2) % 3 };
            page.source = order[next];
            page.reset();
        }
        KeyCode::Up => {
            let to = page.selection.saturating_sub(1);
            moved(page, to);
        }
        KeyCode::Down => {
            let to = page.selection + 1;
            moved(page, to);
        }
        // Page keys scroll the details, to read a skill in full.
        KeyCode::PageUp => page.detail_scroll = page.detail_scroll.saturating_sub(10),
        KeyCode::PageDown => page.detail_scroll += 10,
        KeyCode::Backspace => {
            page.query.pop();
            page.typed();
        }
        KeyCode::Char('u') if ctrl => {
            page.query.clear();
            page.typed();
        }
        KeyCode::Char('g') if ctrl => {
            if let Some(Item::Skill(skill)) = page.selected().cloned() {
                add_skill(app, skill, true);
            }
        }
        KeyCode::Char(c) if !ctrl => {
            page.query.push(c);
            page.typed();
        }
        KeyCode::Enter => {
            // Typed but not looked up yet: look it up first.
            if page.search_due.is_some() || page.query.trim() != page.applied {
                page.search_now();
                return;
            }
            match page.selected().cloned() {
                Some(Item::Skill(skill)) => add_skill(app, skill, false),
                Some(Item::Mcp(server)) => add_mcp(app, server),
                Some(Item::Suggestion { query, .. }) => {
                    page.query = query;
                    page.search_now();
                }
                None => page.search_now(),
            }
        }
        _ => {}
    }
    // The end of the list loads the next page.
    if let Page::Market(page) = &app.page
        && let Some(offset) = page.more
        && !page.loading
        && page.selection + 1 >= page.items.len()
        && !page.items.is_empty()
    {
        start_search(app, offset);
    }
}

pub fn on_paste(app: &mut App, text: &str) {
    if let Page::Market(page) = &mut app.page {
        page.query.push_str(text.trim());
        page.typed();
    }
}

fn add_skill(app: &mut App, skill: SkillListing, global: bool) {
    if !global && skill_added(&app.db.skills().unwrap_or_default(), &skill) {
        app.notify(Role::Dim, format!("{} is already added · /skills shows it", skill.name));
        return;
    }
    app.busy_task = Some(format!("adding {}", skill.name));
    app.spawn_task(move || {
        let loaded = crate::market::load_skill(&skill).map_err(|e| format!("{}: {e:#}", skill.name));
        if global {
            Task::SkillInstalledGlobally { result: loaded.and_then(|l| crate::skills::install_global(&l).map(|dir| (l.0, dir)).map_err(|e| format!("{e:#}"))) }
        } else {
            Task::SkillLoaded { existing: None, result: loaded }
        }
    });
}

fn add_mcp(app: &mut App, server: McpListing) {
    let plan = crate::market::plan_mcp(&server);
    if !plan.supported {
        app.fail(format!("{} can't be added: {}", server.title, plan.reason));
        return;
    }
    if mcp_added(&app.db.mcp_servers().unwrap_or_default(), &server) {
        app.notify(Role::Dim, format!("{} is already added · /mcp shows it", server.title));
        return;
    }
    let asks = plan.token.is_some() || plan.env.iter().any(|(var, v)| v.required && std::env::var(var).is_err());
    if asks {
        app.dialog = Some(Dialog::Form(dialogs::Form::market_mcp(server, &plan)));
        return;
    }
    let env: Vec<(String, String)> = plan.env.iter().filter_map(|(var, _)| Some((var.clone(), std::env::var(var).ok()?))).collect();
    match crate::market::add_mcp(&app.db, &app.secrets, &server, &plan, None, &env) {
        Ok(id) => {
            let name = app.db.mcp_servers().unwrap_or_default().into_iter().find(|s| s.id == id).map(|s| s.name).unwrap_or_else(|| server.short_name());
            dialogs::refresh_mcp(app, id, name);
        }
        Err(error) => app.fail(error.to_string()),
    }
}
