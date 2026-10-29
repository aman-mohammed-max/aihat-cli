//! Drawing the chat screen: header (2 rows), chat (fills), message box
//! (3 rows, up to 8), status line (1 row), plus popups and dialogs.

use super::app::{ActiveTurn, App, Page, Popup};
use super::draw::{self, put, put_line};
use super::keys::{self, Action};
use super::transcript::{self, Layout};
use crate::ui::text::{self, width};
use crate::ui::theme::{Role, theme};
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use std::rc::Rc;

pub struct Command {
    pub name: &'static str,
    pub description: &'static str,
    pub shortcut: Option<Action>,
    pub takes_args: bool,
}

const fn command(name: &'static str, description: &'static str, shortcut: Option<Action>, takes_args: bool) -> Command {
    Command {
        name,
        description,
        shortcut,
        takes_args,
    }
}

pub const COMMANDS: &[Command] = &[
    command("model", "switch model", Some(Action::SwitchModel), false),
    command("prompt", "pick a system prompt", Some(Action::SwitchPrompt), false),
    command("providers", "list, add, test endpoints", None, false),
    command("tools", "web search, image, MCP tools for this chat", None, false),
    command("mcp", "MCP servers and sign-in", None, false),
    command("skills", "turn skills on or off", None, false),
    command("new", "start a new chat", Some(Action::NewChat), false),
    command("resume", "open a past chat", Some(Action::PastChats), false),
    command("compact", "summarise to free context", None, false),
    command("export", "save as Markdown or JSON", None, false),
    command("image", "improve a prompt, then draw", None, true),
    command("search", "search the web, then answer", None, true),
    command("refresh", "reload models from your providers", None, false),
    command("rename", "rename this chat", None, true),
    command("copy", "copy the last reply", Some(Action::CopyReply), false),
    command("clear", "clear the screen", Some(Action::ClearScreen), false),
    command("memory", "what models remember · on or off for this chat", None, false),
    command("keys", "show every key", Some(Action::ShowKeys), false),
    command("settings", "open settings.json", None, false),
    // After the design's list: commands added since.
    command("market", "find skills and MCP servers to add", None, false),
    command("effort", "how much the model thinks: off, low, medium, high", None, false),
    command("newprompt", "describe an assistant · the model writes its system prompt", None, true),
    command("about", "who made AI Hat · links", None, false),
    command("quit", "leave aihat", Some(Action::Quit), false),
];

/// "ctrl+p", or "ctrl+c ×2" for quit.
fn shortcut_label(command: &Command) -> String {
    match command.shortcut {
        Some(Action::Quit) => format!("{} ×2", keys::label(Action::Quit)),
        Some(action) => keys::label(action).to_string(),
        None => String::new(),
    }
}

/// Commands matching what is typed after `/`: prefix matches first, then
/// fuzzy matches on the name and description.
pub fn matching_commands(input: &str) -> Vec<&'static Command> {
    let query = input.trim_start_matches('/').to_lowercase();
    if query.is_empty() {
        return COMMANDS.iter().collect();
    }
    let mut prefix: Vec<&Command> = COMMANDS.iter().filter(|c| c.name.starts_with(&query)).collect();
    let mut matcher = nucleo_matcher::Matcher::new(nucleo_matcher::Config::DEFAULT);
    let pattern = nucleo_matcher::pattern::Pattern::parse(
        &query,
        nucleo_matcher::pattern::CaseMatching::Ignore,
        nucleo_matcher::pattern::Normalization::Smart,
    );
    let mut fuzzy: Vec<(u32, &Command)> = COMMANDS
        .iter()
        .filter(|c| !c.name.starts_with(&query))
        .filter_map(|c| {
            let mut buf = Vec::new();
            let haystack = nucleo_matcher::Utf32Str::new(c.name, &mut buf);
            pattern.score(haystack, &mut matcher).map(|score| (score, c))
        })
        .collect();
    fuzzy.sort_by(|a, b| b.0.cmp(&a.0));
    prefix.extend(fuzzy.into_iter().map(|(_, c)| c));
    prefix
}

pub fn draw(app: &mut App, frame: &mut Frame<'_>) {
    let screen = frame.area();
    let buf = frame.buffer_mut();
    draw::fill(buf, screen, theme().base());
    // The design is a 112-column window; wider terminals get it centred.
    let max = app.config.width;
    let area = if max >= 80 && screen.width > max {
        Rect::new(screen.x + (screen.width - max) / 2, screen.y, max, screen.height)
    } else {
        screen
    };
    app.width = area.width;
    if area.width < 40 || area.height < 10 {
        put(buf, 0, 0, "Make the terminal at least 80×24.", theme().fg(Role::Warn), area.right());
        return;
    }
    let composer_rows = composer_height(app);
    let header = Rect::new(area.x, area.y, area.width, 2);
    let status = Rect::new(area.x, area.bottom() - 1, area.width, 1);
    let composer = Rect::new(area.x, status.y - composer_rows, area.width, composer_rows);
    let chat = Rect::new(area.x, header.bottom(), area.width, composer.y.saturating_sub(header.bottom()));

    draw_header(app, buf, header);
    if matches!(app.page, Page::Providers(_)) {
        super::providers::draw(app, buf, chat);
    } else if matches!(app.page, Page::Market(_)) {
        super::market::draw(app, buf, chat);
    } else if app.is_welcome() {
        draw_welcome(app, buf, chat);
    } else {
        draw_transcript(app, buf, chat);
    }
    draw_composer(app, buf, composer);
    draw_status(app, buf, status);
    if app.dialog.is_none() {
        match app.popup() {
            Some(Popup::Commands) => draw_commands(app, buf, chat),
            Some(Popup::Attach) => draw_attach(app, buf, chat),
            None => {}
        }
    }
    if app.dialog.is_some() {
        draw::dim(buf, screen);
        super::dialogs::draw(app, buf, area);
    }
}

fn narrow(area: Rect) -> bool {
    area.width < 100
}

fn draw_header(app: &App, buf: &mut Buffer, area: Rect) {
    let t = theme();
    let mut left = vec![
        Span::raw(" "),
        Span::styled("◆ AI Hat", t.bold(Role::Accent)),
    ];
    let selection = app.current_selection();
    let mut right = Vec::new();
    let welcome = matches!(app.page, Page::Chat) && app.is_welcome();
    match (&selection, welcome) {
        (_, true) | (None, _) => right.push(Span::styled("new chat", t.fg(Role::Dim))),
        (Some((provider, model)), false) => {
            right.push(Span::styled(model.clone(), t.fg(Role::Strong)));
            if !narrow(area) {
                right.push(Span::raw(" "));
                right.push(Span::styled(format!("· {}", provider.name), t.fg(Role::Dim)));
            }
        }
    }
    right.push(Span::raw(" "));
    if !narrow(area) {
        let version = format!("{} · ", env!("CARGO_PKG_VERSION"));
        let right_width: usize = right.iter().map(|s| width(&s.content)).sum();
        let room = (area.width as usize).saturating_sub(10 + width(&version) + right_width + 2);
        left.push(Span::raw(" "));
        left.push(Span::styled(
            format!("{version}{}", text::truncate_left(&text::current_dir_label(), room)),
            t.fg(Role::Dim),
        ));
    }
    let line = text::spread(left, right, area.width as usize);
    put_line(buf, area.x, area.y, &line, area.right());
    draw::rule(buf, area.x, area.y + 1, area.width, t.fg(Role::Dim));
}

const LOGO: [(&str, usize); 4] = [("▗▄▄▄▄▖", 3), ("▟██████▙", 2), ("▜██████▛", 2), ("▀▀▀▀", 4)];

fn draw_welcome(app: &App, buf: &mut Buffer, area: Rect) {
    let t = theme();
    let right = area.right();
    let mut y = area.y + 1;
    let selection = app.current_selection();
    let model_line = match &selection {
        Some((provider, model)) => {
            let prompt = app.current_prompt_name().unwrap_or_else(|| "none".into());
            format!("{model} · {} · system prompt: {prompt}", provider.name)
        }
        None => "No model yet · type /providers to add an endpoint".into(),
    };
    let beside = [
        Line::styled("AI Hat", t.bold(Role::Strong)),
        Line::styled("Chat with any model, on your terms", t.fg(Role::Dim)),
        Line::styled(model_line, t.fg(Role::Dim)),
        Line::default(),
    ];
    for (row, ((logo, indent), text_line)) in LOGO.iter().zip(beside.iter()).enumerate() {
        let x = put(buf, area.x + *indent as u16, y + row as u16, logo, t.fg(Role::Accent), right);
        let _ = x;
        put_line(buf, area.x + 13, y + row as u16, text_line, right);
    }
    y += 5;
    if y >= area.bottom() {
        return;
    }
    let c = |s: &'static str| Span::styled(s, t.fg(Role::Link));
    let p = |s: &'static str| Span::styled(s, t.fg(Role::Text));
    let bullet = || Span::styled("· ", t.fg(Role::Dim));
    put_line(buf, area.x + 2, y, &Line::styled("Tips", t.fg(Role::Strong)), right);
    let settings = app.db.settings();
    let synced = settings
        .get("lastBackupAt")
        .and_then(serde_json::Value::as_i64)
        .filter(|at| *at > 0)
        .map(|at| format!(" (synced {})", text::relative_time(at)))
        .unwrap_or_else(|| " (shared data folder)".into());
    let tips = [
        Line::from(vec![
            bullet(),
            p("Type a message and press enter. "),
            c("/"),
            p(" shows commands, "),
            c("@"),
            p(" attaches a file, "),
            c("!"),
            p(" runs a shell command"),
        ]),
        Line::from(vec![
            bullet(),
            c(keys::label(Action::SwitchModel)),
            p(" switches model, "),
            c(keys::label(Action::SwitchPrompt)),
            p(" switches system prompt, "),
            c(keys::label(Action::ShowKeys)),
            p(" shows every key"),
        ]),
        Line::from(vec![
            bullet(),
            p("Same providers, prompts, skills and chats as the phone app"),
            Span::styled(synced, t.fg(Role::Dim)),
        ]),
    ];
    for (index, tip) in tips.iter().enumerate() {
        put_line(buf, area.x + 2, y + 1 + index as u16, tip, right);
    }
    y += 5;
    if y + 1 >= area.bottom() {
        return;
    }
    let block = (area.width as usize).min(80) as u16;
    let block_right = area.x + block;
    put_line(buf, area.x + 2, y, &Line::styled("Recent chats", t.fg(Role::Strong)), right);
    let hint = format!("{} for all", keys::label(Action::PastChats));
    put(buf, block_right - width(&hint) as u16, y, &hint, t.fg(Role::Dim), right);
    let chats = app.db.chats(false).unwrap_or_default();
    if chats.is_empty() {
        put(buf, area.x + 4, y + 1, "No chats yet. Ask anything below.", t.fg(Role::Dim), right);
        return;
    }
    let time_x = block_right.saturating_sub(13);
    for (index, chat) in chats.iter().take(3).enumerate() {
        let row = y + 1 + index as u16;
        if row >= area.bottom() {
            break;
        }
        if index == app.recent_selection {
            put(buf, area.x + 2, row, "›", t.fg(Role::Accent), right);
        }
        let title = text::truncate(&chat.title, (time_x - area.x - 5) as usize);
        put(buf, area.x + 4, row, &title, t.fg(Role::Text), time_x);
        put(buf, time_x, row, &text::relative_time(chat.updated_at), t.fg(Role::Dim), right);
    }
}

fn draw_transcript(app: &mut App, buf: &mut Buffer, area: Rect) {
    let blocks = app.transcript_blocks();
    let mut rows: Vec<&Line<'static>> = Vec::new();
    let blank = Line::default();
    // Small terminals start the chat right under the header.
    if !narrow(area) {
        rows.push(&blank);
    }
    for (index, block) in blocks.iter().enumerate() {
        if index > 0 {
            rows.push(&blank);
        }
        rows.extend(block.iter());
    }
    let total = rows.len();
    let height = area.height as usize;
    // Keep the view still while new rows arrive below a scrolled position.
    if app.scroll > 0 && total > app.last_total {
        app.scroll += total - app.last_total;
    }
    app.last_total = total;
    let max_scroll = total.saturating_sub(height);
    app.scroll = app.scroll.min(max_scroll);
    let start = total.saturating_sub(height + app.scroll);
    for (offset, line) in rows[start..].iter().take(height).enumerate() {
        put_line(buf, area.x, area.y + offset as u16, line, area.right());
    }
    app.jump_button = None;
    if app.scroll > 0 {
        // A button back to the newest message; click it or press end.
        let t = theme();
        let pill = t.selected(Role::Text);
        let below = text::plural(app.scroll, "line", "lines");
        let label = format!(" ↓ Jump to bottom · {below} below   end ");
        let w = width(&label) as u16;
        let x = area.right().saturating_sub(w + 1);
        let y = area.bottom() - 1;
        let mut cursor = put(buf, x, y, " ↓ ", pill.fg(t.color(Role::Accent)), area.right());
        cursor = put(buf, cursor, y, "Jump to bottom", pill, area.right());
        cursor = put(buf, cursor, y, &format!(" · {below} below   "), pill.fg(t.color(Role::Dim)), area.right());
        put(buf, cursor, y, "end ", pill.fg(t.color(Role::Dim)), area.right());
        app.jump_button = Some(Rect::new(x, y, w, 1));
    }
}

/// Rows for the reply that is streaming now.
pub fn turn_blocks(turn: &ActiveTurn, layout: Layout) -> Vec<Rc<Vec<Line<'static>>>> {
    let t = theme();
    let mut blocks = Vec::new();
    let mut tools = Vec::new();
    for (label, started, log) in &turn.tools {
        match log {
            Some(log) => tools.extend(transcript::tool_lines(log, layout)),
            // The approval box below already says what is waiting.
            None if turn.approval.is_some() => {}
            None => tools.extend(transcript::running_tool_lines(
                &label.name,
                &label.detail,
                started.elapsed().as_millis() as u64,
                turn.approval.is_some(),
                layout,
            )),
        }
    }
    if let Some((server, since)) = &turn.signing_in {
        tools.push(Line::from(vec![
            Span::styled(" ● ", t.fg(Role::Warn)),
            Span::styled(format!("{server} · sign in"), t.fg(Role::Dim)),
        ]));
        tools.push(Line::styled(
            format!(
                "   └ waiting for you to sign in, in your browser · {} cancels · {}",
                keys::label(Action::Stop),
                text::seconds(since.elapsed().as_millis() as u64)
            ),
            t.fg(Role::Dim),
        ));
    }
    if !tools.is_empty() {
        blocks.push(Rc::new(tools));
    }
    let mut body = Vec::new();
    let elapsed = turn.started.elapsed().as_millis() as u64;
    let answering = !turn.content.trim().is_empty();
    let waiting_on_tool = turn.tools.iter().any(|(_, _, log)| log.is_none()) || turn.approval.is_some() || turn.signing_in.is_some();
    if !turn.reasoning.trim().is_empty() {
        let label = match turn.reasoning_ms {
            Some(ms) => format!("Thought for {}", text::seconds(ms)),
            None => format!("Thinking… {}", text::seconds(elapsed)),
        };
        body.extend(transcript::reasoning_lines(&turn.reasoning, &label, layout));
    } else if !answering && !waiting_on_tool {
        body.push(Line::from(vec![
            Span::styled(" ◇ ", t.fg(Role::Code)),
            Span::styled(format!("Thinking… {}", text::seconds(elapsed)), t.fg(Role::Dim)),
        ]));
    }
    if answering {
        let mut answer = transcript::answer_lines(&turn.content, layout);
        // The block cursor that shows the reply is still arriving.
        if let Some(last) = answer.last_mut().filter(|_| turn.approval.is_none()) {
            last.spans.push(Span::styled(" ", t.cursor()));
        }
        body.extend(answer);
    }
    if !body.is_empty() {
        blocks.push(Rc::new(body));
    }
    if let Some(approval) = &turn.approval {
        blocks.push(Rc::new(super::dialogs::approval_lines(approval, layout.width)));
    }
    blocks
}

fn composer_height(app: &App) -> u16 {
    let cols = app.composer_cols();
    let rows = app.composer.rows(cols).len().clamp(1, 6);
    rows as u16 + 2
}

fn placeholder(app: &App, narrow: bool) -> String {
    if let Page::Providers(_) = app.page {
        return "/providers · esc back to chat".into();
    }
    if let Page::Market(_) = app.page {
        return "/market · type to search · esc back to chat".into();
    }
    if let Some(turn) = &app.turn {
        if turn.approval.is_some() {
            return "Waiting for your answer…".into();
        }
        if turn.signing_in.is_some() {
            return "Waiting for you to sign in, in your browser…".into();
        }
        return format!("Streaming… {} to stop", keys::label(Action::Stop));
    }
    if app.busy_task.is_some() {
        return "Working… you can keep typing".into();
    }
    if narrow {
        "Ask anything · / commands".into()
    } else {
        "Ask anything · / commands · @ attach a file · ! run a shell command".into()
    }
}

fn draw_composer(app: &App, buf: &mut Buffer, area: Rect) {
    let t = theme();
    let active = !app.composer.is_empty() && app.dialog.is_none();
    let border = if active { t.fg(Role::Accent) } else { t.fg(Role::Dim) };
    draw::boxed(buf, area, border, "", "");
    let x = area.x + 2;
    let y = area.y + 1;
    let right = area.right() - 2;
    put(buf, x, y, "›", t.fg(Role::Accent), right);
    let text_x = x + 2;
    if app.composer.is_empty() {
        let hint = placeholder(app, narrow(area));
        if active || app.dialog.is_none() && matches!(app.page, Page::Chat) && app.turn.is_none() {
            // The cursor sits at the start of an empty box.
        }
        put(buf, text_x, y, &hint, t.fg(Role::Dim), right);
        return;
    }
    let cols = app.composer_cols();
    let rows = app.composer.rows(cols);
    let (cursor_row, cursor_col) = app.composer.position(&rows);
    let visible = (area.height - 2) as usize;
    let first = (cursor_row + 1).saturating_sub(visible);
    let text = app.composer.text();
    for (offset, range) in rows.iter().skip(first).take(visible).enumerate() {
        let row_y = y + offset as u16;
        put(buf, text_x, row_y, &text[range.clone()], t.fg(Role::Strong), right + 1);
    }
    if app.dialog.is_none() {
        let cy = y + (cursor_row - first) as u16;
        let cx = text_x + cursor_col as u16;
        if let Some(cell) = buf.cell_mut((cx.min(right), cy)) {
            cell.set_style(t.cursor());
        }
    }
    if let Some(turn) = app.turn.as_ref().filter(|turn| turn.editing_args) {
        let _ = turn;
        let hint = " enter runs with these arguments · esc cancels ";
        put(buf, area.right().saturating_sub(width(hint) as u16 + 2), area.y, hint, t.fg(Role::Accent), area.right() - 1);
    }
}

fn draw_status(app: &App, buf: &mut Buffer, area: Rect) {
    let t = theme();
    let narrow = narrow(area);
    let dim = t.fg(Role::Dim);
    let left: Line = match &app.notice {
        Some(notice) => Line::styled(notice.text.clone(), t.fg(notice.role)),
        None => match (&app.page, narrow) {
            (Page::Providers(_), _) => Line::styled("↑↓ move · tab switch pane · a add · t test · d default · esc back", dim),
            (Page::Market(_), _) => Line::styled("type to search · ↑↓ move · tab skills/MCP · enter add · esc back", dim),
            (_, true) => Line::styled(format!("{} keys · {} model", keys::label(Action::ShowKeys), keys::label(Action::SwitchModel)), dim),
            _ if app.editing_last.is_some() => Line::styled(format!("{} resend · esc cancel the edit", keys::label(Action::Send)), dim),
            _ => Line::styled(
                format!(
                    "{} send · {} line · {} stop · {} model · {} keys",
                    keys::label(Action::Send),
                    keys::label(Action::NewLine),
                    keys::label(Action::Stop),
                    keys::label(Action::SwitchModel),
                    keys::label(Action::ShowKeys)
                ),
                dim,
            ),
        },
    };
    let mut right: Vec<Span> = Vec::new();
    match &app.page {
        Page::Providers(_) => {
            let providers = app.db.providers().map(|p| p.len()).unwrap_or(0);
            let models = app
                .db
                .models(None)
                .map(|m| m.iter().filter(|m| m.enabled).count())
                .unwrap_or(0);
            right.push(Span::styled(
                format!(
                    "{} · {}",
                    text::plural(providers, "provider", "providers"),
                    text::plural(models, "model", "models")
                ),
                dim,
            ));
        }
        Page::Market(page) => {
            if let Some(task) = &app.busy_task {
                right.push(Span::styled(format!("● {task}"), t.fg(Role::Warn)));
            } else if page.loading {
                right.push(Span::styled("● searching", t.fg(Role::Warn)));
            } else {
                right.push(Span::styled(text::plural(page.items.len(), "result", "results"), dim));
            }
        }
        Page::Chat => {
            let context = context_label(app, narrow);
            if let Some(turn) = &app.turn {
                let state = if turn.approval.is_some() || turn.signing_in.is_some() { "● waiting for you" } else { "● streaming" };
                right.push(Span::styled(state, t.fg(Role::Warn)));
                if !context.is_empty() {
                    right.push(Span::styled(format!(" · {context}"), dim));
                }
            } else if let Some(task) = &app.busy_task {
                right.push(Span::styled(format!("● {task}"), t.fg(Role::Warn)));
            } else {
                let latency = app
                    .current_selection()
                    .and_then(|(provider, _)| provider.latency_ms)
                    .filter(|_| !narrow)
                    .map(text::duration);
                let effort = Some(app.current_effort()).filter(|level| level != "off").map(|level| format!("{level} effort"));
                let parts: Vec<String> = [effort, Some(context), latency].into_iter().flatten().filter(|p| !p.is_empty()).collect();
                right.push(Span::styled(parts.join(" · "), dim));
            }
        }
    }
    right.push(Span::raw(" "));
    let mut left_spans = vec![Span::raw(" ")];
    let left_style = left.style;
    left_spans.extend(left.spans.into_iter().map(|span| Span::styled(span.content, left_style.patch(span.style))));
    let line = text::spread(left_spans, right, area.width as usize);
    put_line(buf, area.x, area.y, &line, area.right());
}

/// "9.8k / 128k" (or "9.8k/128k" when narrow).
fn context_label(app: &App, narrow: bool) -> String {
    let used = app.context_tokens.unwrap_or_else(|| {
        app.chat
            .as_ref()
            .and_then(|chat| {
                let model = chat.model_id.clone().unwrap_or_default();
                crate::engine::build_messages(&app.db, chat, &model).ok()
            })
            .map(|messages| crate::engine::estimate_tokens(&messages))
            .unwrap_or(0)
    });
    let limit = app.current_selection().and_then(|(provider, model)| {
        app.db
            .model(&provider.id, &model)
            .ok()
            .flatten()
            .and_then(|m| m.context_k())
    });
    match limit {
        Some(k) if used == 0 => format!("{k}k context"),
        Some(k) if narrow => format!("{}/{}k", text::compact(used), k),
        Some(k) => format!("{} / {}k", text::compact(used), k),
        None if used == 0 => String::new(),
        None => format!("{} tokens", text::compact(used)),
    }
}

/// The popup that sits right above the message box.
fn popup_rect(chat: Rect, rows: usize) -> Rect {
    let height = (rows as u16 + 2).min(chat.height);
    Rect::new(chat.x, chat.bottom() - height, chat.width, height)
}

fn draw_commands(app: &App, buf: &mut Buffer, chat: Rect) {
    let t = theme();
    let commands = matching_commands(app.composer.text());
    let visible_rows = commands.len().clamp(1, 11).min(chat.height.saturating_sub(2) as usize);
    let area = popup_rect(chat, visible_rows);
    draw::boxed(buf, area, t.fg(Role::Dim), "commands", "↑↓ move · tab complete · esc close");
    let inner = draw::inner(area);
    if commands.is_empty() {
        put(buf, inner.x, inner.y, "No command matches · esc closes", t.fg(Role::Dim), inner.right());
        return;
    }
    let selection = app.popup_selection.min(commands.len() - 1);
    let first = (selection + 1).saturating_sub(visible_rows);
    for (offset, command) in commands.iter().skip(first).take(visible_rows).enumerate() {
        let selected = first + offset == selection;
        let name_style = if selected { t.fg(Role::Text) } else { t.fg(Role::Link) };
        let left = vec![
            Span::styled(text::pad(&format!("/{}", command.name), 13), name_style),
            Span::styled(command.description, t.fg(Role::Text)),
        ];
        let right = vec![Span::styled(shortcut_label(command), if selected { t.fg(Role::Text) } else { t.fg(Role::Dim) })];
        let line = text::spread(left, right, inner.width as usize);
        draw::row(buf, inner, inner.y + offset as u16, line, selected);
    }
}

fn draw_attach(app: &mut App, buf: &mut Buffer, chat: Rect) {
    let t = theme();
    let matches = app.attach_matches();
    let token = app
        .composer
        .token_at_cursor()
        .map(|(_, token)| token.to_string())
        .unwrap_or_else(|| "@".into());
    let rows = matches.len() + 1;
    let area = popup_rect(chat, rows);
    draw::boxed(buf, area, t.fg(Role::Dim), &token, "tab add · enter add and keep typing");
    let inner = draw::inner(area);
    let selection = app.popup_selection.min(matches.len().saturating_sub(1));
    for (index, found) in matches.iter().enumerate() {
        let selected = index == selection;
        let path_style = if selected { t.fg(Role::Accent) } else { t.fg(Role::Text) };
        // As in the design, the other rows sit two columns in from the chosen one.
        let line = Line::from(vec![
            Span::raw(if selected { "" } else { "  " }),
            Span::styled(text::pad(&found.path, 34), path_style),
            Span::styled(text::pad(&found.size, 11), path_style),
            Span::styled(format!("· {}", found.modified), if selected { path_style } else { t.fg(Role::Dim) }),
        ]);
        draw::row(buf, inner, inner.y + index as u16, line, selected);
    }
    let tail = if matches.is_empty() {
        "no matching files · also: a URL like @https://example.com"
    } else {
        "also: a URL, an image (shown inline where the terminal supports it)"
    };
    put(buf, inner.x + 2, inner.y + matches.len() as u16, tail, t.fg(Role::Dim), inner.right());
}
