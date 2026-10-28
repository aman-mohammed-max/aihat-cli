//! `aihat` on the command line: one prompt, or one command, then exit.
//!
//! Exit codes: 0 ok · 1 error · 2 bad usage · 3 provider unreachable ·
//! 4 auth failed · 130 stopped (ctrl+c).

mod ask;
mod commands;
pub mod prompt;

use crate::config::{AppConfig, Paths};
use crate::db::Database;
use crate::secrets::Secrets;
use crate::ui::ansi::paint;
use crate::ui::theme::{self, Role, theme};
use anyhow::Result;
use clap::{Args, Parser, Subcommand};
use ratatui::text::{Line, Span};
use std::io::IsTerminal;
use std::path::PathBuf;

pub const EXIT_ERROR: i32 = 1;
pub const EXIT_USAGE: i32 = 2;
pub const EXIT_UNREACHABLE: i32 = 3;
pub const EXIT_AUTH: i32 = 4;
pub const EXIT_STOPPED: i32 = 130;

#[derive(Parser, Debug)]
#[command(name = "aihat", disable_help_flag = true, disable_version_flag = true, disable_help_subcommand = true)]
struct Cli {
    /// Model to use
    #[arg(short = 'm', long, global = true, value_name = "ID")]
    model: Option<String>,
    /// Provider to use
    #[arg(short = 'P', long, global = true, value_name = "NAME")]
    provider: Option<String>,
    /// Saved system prompt to use
    #[arg(short = 's', long, global = true, value_name = "NAME")]
    system: Option<String>,
    /// Attach a file (repeatable)
    #[arg(short = 'f', long = "file", global = true, action = clap::ArgAction::Append, value_name = "PATH")]
    files: Vec<PathBuf>,
    /// Turn on an MCP server for this prompt (repeatable)
    #[arg(short = 't', long = "tool", global = true, action = clap::ArgAction::Append, value_name = "SERVER")]
    tools: Vec<String>,
    /// Web search on
    #[arg(short = 'w', long, global = true)]
    web: bool,
    /// Continue the last chat
    #[arg(short = 'c', long = "continue", global = true)]
    continue_chat: bool,
    /// Print JSON: reply, usage, sources
    #[arg(long, global = true)]
    json: bool,
    /// Print the reply when it is finished
    #[arg(long = "no-stream", global = true)]
    no_stream: bool,
    /// Turn colours off
    #[arg(long = "no-color", global = true)]
    no_color: bool,
    /// Draw boxes with + - |
    #[arg(long, global = true)]
    ascii: bool,
    /// Answer yes to confirmations and tool approvals
    #[arg(short = 'y', long, global = true)]
    yes: bool,
    /// Use another database file
    #[arg(long = "db-path", global = true, value_name = "PATH", hide = true)]
    db_path: Option<PathBuf>,
    #[arg(short = 'h', long, global = true)]
    help: bool,
    #[arg(short = 'v', long)]
    version: bool,
    #[command(subcommand)]
    command: Option<Top>,
    #[arg(value_name = "PROMPT")]
    prompt: Vec<String>,
}

#[derive(Subcommand, Debug)]
enum Top {
    /// Open the full-screen chat
    Chat,
    /// List, add, test, set-default, set-key, remove endpoints
    Providers(Sub<commands::ProviderAction>),
    /// List models · set the default model
    Models(Sub<commands::ModelAction>),
    /// List, show and add system prompts
    Prompts(Sub<commands::PromptAction>),
    /// List, show, continue, export past chats
    Chats(Sub<commands::ChatAction>),
    /// List, add, remove MCP servers
    Mcp(Sub<commands::McpAction>),
    /// List, enable, disable skills
    Skills(Sub<commands::SkillAction>),
    /// Get and set options
    Config(Sub<commands::ConfigAction>),
    /// Cross-chat memory
    Memory(Sub<commands::MemoryAction>),
    /// Back up or restore everything as JSON
    Backup(Sub<commands::BackupAction>),
    /// Save a Brave or Tavily search key
    SearchKey(Sub<commands::SearchKeyAction>),
    /// Draw a picture
    Image {
        prompt: Vec<String>,
        #[arg(long, short = 'n')]
        count: Option<usize>,
    },
}

#[derive(Args, Debug)]
struct Sub<T: Subcommand> {
    #[command(subcommand)]
    action: Option<T>,
}

/// A failure with a friendly message and an exit code.
#[derive(Debug)]
pub struct Failure {
    pub code: i32,
    pub lines: Vec<Line<'static>>,
}

impl Failure {
    pub fn new(code: i32, headline: impl Into<String>) -> Self {
        Self {
            code,
            lines: vec![Line::styled(format!("✗ {}", headline.into()), theme().fg(Role::Error))],
        }
    }

    /// An indented second line.
    pub fn detail(mut self, spans: Vec<Span<'static>>) -> Self {
        let mut line = vec![Span::raw("  ")];
        line.extend(spans);
        self.lines.push(Line::from(line));
        self
    }

    pub fn note(self, text: impl Into<String>) -> Self {
        self.detail(vec![Span::styled(text.into(), theme().fg(Role::Text))])
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text: Vec<String> = self.lines.iter().map(|line| line.to_string()).collect();
        f.write_str(&text.join("\n"))
    }
}

impl std::error::Error for Failure {}

/// Run the command line and return the exit code.
pub fn main() -> i32 {
    match run() {
        Ok(code) => code,
        Err(error) => {
            report(&error);
            exit_code(&error)
        }
    }
}

fn report(error: &anyhow::Error) {
    if let Some(failure) = error.downcast_ref::<Failure>() {
        for line in &failure.lines {
            eprintln!("{}", crate::ui::ansi::line(line));
        }
        return;
    }
    if let Some(clap_error) = error.downcast_ref::<clap::Error>() {
        let _ = clap_error.print();
        return;
    }
    let t = theme();
    let text = match crate::api::api_error(error) {
        Some(api) => api.to_string(),
        None => format!("{error:#}"),
    };
    let mut lines = text.lines();
    if let Some(first) = lines.next() {
        eprintln!("{}", paint(&format!("✗ {first}"), t.fg(Role::Error)));
    }
    for line in lines {
        eprintln!("  {line}");
    }
}

fn exit_code(error: &anyhow::Error) -> i32 {
    if let Some(failure) = error.downcast_ref::<Failure>() {
        return failure.code;
    }
    if let Some(clap_error) = error.downcast_ref::<clap::Error>() {
        return clap_error.exit_code();
    }
    match crate::api::api_error(error).map(|e| e.kind) {
        Some(crate::api::ErrorKind::Unreachable) => EXIT_UNREACHABLE,
        Some(crate::api::ErrorKind::Auth) => EXIT_AUTH,
        _ => EXIT_ERROR,
    }
}

pub struct Env {
    pub paths: Paths,
    pub config: AppConfig,
    pub secrets: Secrets,
    pub db: Database,
    pub yes: bool,
}

fn run() -> Result<i32> {
    let args = match Cli::try_parse() {
        Ok(args) => args,
        Err(error) => {
            init_theme(false, false, None);
            if matches!(error.kind(), clap::error::ErrorKind::DisplayHelp) {
                print_help();
                return Ok(0);
            }
            let message = error.to_string();
            let first = message
                .lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or("bad usage")
                .trim_start_matches("error: ")
                .to_string();
            return Err(Failure::new(EXIT_USAGE, first)
                .detail(vec![
                    Span::styled("See ", theme().fg(Role::Dim)),
                    Span::styled("aihat --help", theme().fg(Role::Link)),
                ])
                .into());
        }
    };
    let paths = Paths::resolve(args.db_path.clone())?;
    let config = AppConfig::load(&paths)?;
    init_theme(args.no_color || config.no_color, args.ascii || config.ascii, Some(&config.accent));
    if args.version {
        println!("aihat {}", env!("CARGO_PKG_VERSION"));
        return Ok(0);
    }
    if args.help {
        match &args.command {
            None => print_help(),
            Some(_) => print_command_help(&std::env::args().collect::<Vec<_>>()),
        }
        return Ok(0);
    }
    let secrets = Secrets::new(&paths);
    let db = Database::open(&paths.db_file)?;
    let mut env = Env {
        paths,
        config,
        secrets,
        db,
        yes: args.yes,
    };
    let provider = args.provider.clone();
    match args.command {
        Some(Top::Chat) => open_tui(env, None),
        Some(Top::Providers(sub)) => commands::providers(&mut env, sub.action),
        Some(Top::Models(sub)) => commands::models(&mut env, sub.action, provider.as_deref()),
        Some(Top::Prompts(sub)) => commands::prompts(&mut env, sub.action),
        Some(Top::Chats(sub)) => match commands::chats(&mut env, sub.action)? {
            commands::ChatOutcome::Done(code) => Ok(code),
            commands::ChatOutcome::Open(id) => open_tui(env, Some(id)),
        },
        Some(Top::Mcp(sub)) => commands::mcp(&mut env, sub.action),
        Some(Top::Skills(sub)) => commands::skills(&mut env, sub.action),
        Some(Top::Config(sub)) => commands::config(&mut env, sub.action),
        Some(Top::Memory(sub)) => commands::memory(&mut env, sub.action),
        Some(Top::Backup(sub)) => commands::backup(&mut env, sub.action),
        Some(Top::SearchKey(sub)) => commands::search_key(&mut env, sub.action),
        Some(Top::Image { prompt, count }) => commands::image(&mut env, &prompt.join(" "), provider.as_deref(), args.model.as_deref(), count),
        None => {
            let prompt = args.prompt.join(" ");
            let interactive = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
            if prompt.trim().is_empty() && args.files.is_empty() && interactive {
                let initial = if args.continue_chat {
                    env.db.chats(false)?.first().map(|chat| chat.id.clone())
                } else {
                    None
                };
                return open_tui(env, initial);
            }
            ask::run(
                &mut env,
                ask::Options {
                    prompt,
                    files: args.files,
                    tools: args.tools,
                    provider: args.provider,
                    model: args.model,
                    system: args.system,
                    web: args.web,
                    json: args.json,
                    stream: !args.no_stream,
                    continue_chat: args.continue_chat,
                },
            )
        }
    }
}

fn open_tui(env: Env, chat: Option<String>) -> Result<i32> {
    if !std::io::stdout().is_terminal() {
        return Err(Failure::new(EXIT_USAGE, "The full-screen chat needs a terminal")
            .note("Pass a prompt to print one reply: aihat \"hello\"")
            .into());
    }
    // The chat draws its own screen, so it uses the colours the terminal has.
    crate::tui::run(env.db, env.paths, env.config, env.secrets, chat)?;
    Ok(0)
}

fn init_theme(no_color: bool, ascii: bool, accent: Option<&str>) {
    let depth = theme::detect_depth(supports_color::Stream::Stdout, no_color);
    let depth = if depth == theme::Depth::None && std::io::stdout().is_terminal() && !no_color && std::env::var_os("NO_COLOR").is_none() {
        theme::Depth::Ansi16
    } else {
        depth
    };
    theme::init(accent.unwrap_or("#C6F46B"), depth, ascii);
}

fn print_help() {
    let t = theme();
    let strong = |s: &str| paint(s, t.bold(Role::Strong));
    let dim = |s: &str| paint(s, t.fg(Role::Dim));
    let cmd = |s: &str| paint(s, t.fg(Role::Link));
    let flag = |s: &str| paint(s, t.fg(Role::Warn));
    let accent = |s: &str| paint(s, t.fg(Role::Accent));
    let pad = |s: &str, w: usize| crate::ui::text::pad(s, w);
    let db = Database::open(&Paths::resolve(None).map(|p| p.db_file).unwrap_or_default()).ok();
    let (model, provider) = db
        .as_ref()
        .and_then(|db| crate::providers::resolve_provider_model(db, None, None).ok())
        .map(|(p, m)| (m, p.name))
        .unwrap_or_else(|| ("none yet".into(), "none yet".into()));
    let config = crate::ui::text::home_relative(&dirs::config_dir().unwrap_or_default().join("aihat/config.toml"));
    let mut out = String::new();
    let mut line = |s: String| {
        out.push_str(&s);
        out.push('\n');
    };
    line(format!("{}  {}", strong(&format!("aihat {}", env!("CARGO_PKG_VERSION"))), dim("chat with any OpenAI-compatible model from your terminal")));
    line(String::new());
    line(strong("Usage"));
    line(format!("  {}{}{}", accent("aihat"), " ".repeat(26), format!("open the full-screen chat {}", dim("(same as aihat chat)"))));
    line(format!("  {} {}{}ask once, print the reply, exit", accent("aihat"), cmd("\"<prompt>\""), " ".repeat(15)));
    line(format!("  {} {} {}", accent("aihat"), cmd("<command>"), dim("[options]")));
    line(String::new());
    line(strong("Commands"));
    let commands = [
        ("providers", "list, add, test, set-default, set-key, remove endpoints", ""),
        ("models", "list models · set the default model", ""),
        ("prompts", "list, show, use and add system prompts", ""),
        ("chats", "list, show, continue, export past chats", ""),
        ("mcp", "list, add, remove MCP servers", ""),
        ("skills", "list, enable, disable skills", ""),
        ("config", "get and set options ", &*format!("({config})")),
        ("backup", "export or restore everything ", "(same data as the phone app)"),
        ("image", "draw a picture with your image model", ""),
    ];
    for (name, what, note) in commands {
        line(format!("  {}{what}{}", cmd(&pad(name, 12)), dim(note)));
    }
    line(String::new());
    line(strong("Options for one prompt"));
    let options: [(&str, &str, &str, String); 9] = [
        ("-m, --model", "<id>", "model to use ", format!("(default: {model})")),
        ("-P, --provider", "<name>", "provider to use ", format!("(default: {provider})")),
        ("-s, --system", "<name>", "saved system prompt to use", String::new()),
        ("-f, --file", "<path>", "attach a file, repeatable ", "· piped stdin is attached too".into()),
        ("-w, --web", "", "web search on", String::new()),
        ("-c, --continue", "", "continue the last chat", String::new()),
        ("-t, --tool", "<server>", "turn on an MCP server, repeatable", String::new()),
        ("    --json", "", "print JSON: reply, usage, sources", String::new()),
        ("    --no-stream", "", "print the reply when it is finished", String::new()),
    ];
    for (name, value, what, note) in options {
        let left_width = crate::ui::text::width(name) + if value.is_empty() { 0 } else { 1 + crate::ui::text::width(value) };
        let gap = " ".repeat(25usize.saturating_sub(left_width));
        let value = if value.is_empty() { String::new() } else { format!(" {}", dim(value)) };
        line(format!("  {}{value}{gap}{what}{}", flag(name), dim(&note)));
    }
    line(format!("  {}   {}   {}", flag("-y, --yes"), flag("-h, --help"), flag("-v, --version")));
    line(String::new());
    line(format!("{} aihat <command> --help {}", dim("Run"), dim("for a command's own options.")));
    print!("{out}");
}

fn print_command_help(argv: &[String]) {
    use clap::CommandFactory;
    let mut command = Cli::command();
    let names: Vec<&str> = argv
        .iter()
        .skip(1)
        .filter(|arg| !arg.starts_with('-'))
        .map(String::as_str)
        .collect();
    let mut current = &mut command;
    for name in names {
        match current.find_subcommand_mut(name) {
            Some(sub) => current = sub,
            None => break,
        }
    }
    let _ = current.print_long_help();
}
