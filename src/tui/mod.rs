//! The full-screen chat: `aihat` with no arguments.

mod about;
mod anim;
mod app;
mod composer;
mod dialogs;
mod draw;
mod files;
mod folder;
mod images;
mod keys;
mod market;
mod providers;
mod tasks;
mod transcript;
mod view;

use crate::config::{AppConfig, Paths};
use crate::db::Database;
use crate::secrets::Secrets;
use anyhow::Result;
use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen, SetTitle};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use std::io;
use std::time::{Duration, Instant};

pub use app::describe_error;
pub use dialogs::model_caps_long;
pub use keys::parse_key;
pub use tasks::test_endpoint;

pub fn run(db: Database, paths: Paths, config: AppConfig, secrets: Secrets, initial_chat: Option<String>) -> Result<()> {
    let problems = keys::init(&config.keys);
    crate::ui::picture::settle(&config.images.preview);
    anim::set_enabled(config.animations);
    let mut app = app::App::new(db, paths, config, secrets, initial_chat)?;
    if let Some(problem) = problems.first() {
        app.notify(crate::ui::theme::Role::Warn, problem.clone());
    }
    // Asking the terminal whether it supports the keyboard protocol can
    // stall for seconds on terminals that never answer, so the request is
    // sent unconditionally; terminals without it ignore the sequence.
    let enhanced = !cfg!(windows);
    MOUSE.store(app.config.mouse, std::sync::atomic::Ordering::Relaxed);
    setup(enhanced)?;
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore(enhanced);
        previous_hook(info);
    }));
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    terminal.hide_cursor()?;
    let result = event_loop(&mut terminal, &mut app, enhanced);
    restore(enhanced);
    crate::mcp_stdio::stop(None);
    result
}

/// Mouse reporting is on (settings.json "mouse").
static MOUSE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn setup(enhanced: bool) -> Result<()> {
    terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    // Mouse capture stays off so the terminal's own text selection works.
    // The window's title is kept to put back at the end.
    {
        use std::io::Write;
        let _ = write!(stdout, "\x1b[22;0t");
    }
    execute!(stdout, EnterAlternateScreen, EnableBracketedPaste, SetTitle(window_title()))?;
    if enhanced {
        // Lets the terminal report shift+enter separately from enter.
        let _ = execute!(
            stdout,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        );
    }
    if MOUSE.load(std::sync::atomic::Ordering::Relaxed) {
        // Clicks and the wheel only (no movement reports), in the SGR format.
        // Shift+drag still selects text in most terminals.
        use std::io::Write;
        let _ = write!(stdout, "\x1b[?1000h\x1b[?1006h");
        let _ = stdout.flush();
    }
    Ok(())
}

fn restore(enhanced: bool) {
    let mut stdout = io::stdout();
    if MOUSE.load(std::sync::atomic::Ordering::Relaxed) {
        use std::io::Write;
        let _ = write!(stdout, "\x1b[?1006l\x1b[?1000l");
    }
    if enhanced {
        let _ = execute!(stdout, PopKeyboardEnhancementFlags);
    }
    let _ = execute!(stdout, DisableBracketedPaste, LeaveAlternateScreen, crossterm::cursor::Show);
    {
        use std::io::Write;
        let _ = write!(stdout, "\x1b[23;0t");
        let _ = stdout.flush();
    }
    let _ = terminal::disable_raw_mode();
}

/// "aihat — ~/projects/fabric-mod · 112×34", as the design's title bar.
fn window_title() -> String {
    let (cols, rows) = terminal::size().unwrap_or((0, 0));
    format!("aihat — {} · {cols}×{rows}", crate::ui::text::current_dir_label())
}

/// Draw every cell again on the next frame: wipe the screen and draw an
/// empty frame first. (Terminal::clear would ask the terminal where its
/// cursor is, and stop the app if no answer came.)
fn repaint(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    use ratatui::backend::Backend;
    terminal.backend_mut().clear_region(ratatui::backend::ClearType::All)?;
    terminal.draw(|_| {})?;
    Ok(())
}

/// Hand the terminal to $VISUAL or $EDITOR for a file, then come back.
/// Returns the editor's name when it could not start.
fn run_editor(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, enhanced: bool, file: &std::path::Path) -> Result<std::result::Result<(), String>> {
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| if cfg!(windows) { "notepad".into() } else { "nano".into() });
    let mut parts = editor.split_whitespace();
    let program = parts.next().unwrap_or("nano").to_string();
    let args: Vec<String> = parts.map(str::to_owned).collect();
    restore(enhanced);
    let status = std::process::Command::new(&program).args(&args).arg(file).status();
    setup(enhanced)?;
    repaint(terminal)?;
    Ok(status.map(|_| ()).map_err(|error| format!("Could not open {program}: {error} · set EDITOR")))
}

fn edit_settings(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, app: &mut app::App, enhanced: bool) -> Result<()> {
    match run_editor(terminal, enhanced, &app.paths.config_file.clone())? {
        Ok(()) => app.reload_settings(),
        Err(error) => app.fail(format!("{error}, or edit {}", crate::ui::text::home_relative(&app.paths.config_file))),
    }
    Ok(())
}

/// Edit a system prompt's text in the editor; saved when it changed.
fn edit_prompt(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, app: &mut app::App, enhanced: bool, id: &str) -> Result<()> {
    let Some(prompt) = app.db.prompts().unwrap_or_default().into_iter().find(|p| p.id == id) else {
        return Ok(());
    };
    let file = std::env::temp_dir().join(format!("aihat-prompt-{}.md", id.chars().take(8).collect::<String>()));
    std::fs::write(&file, &prompt.body)?;
    let edited = run_editor(terminal, enhanced, &file)?;
    let text = std::fs::read_to_string(&file).unwrap_or_default();
    let _ = std::fs::remove_file(&file);
    match edited {
        Err(error) => app.fail(error),
        Ok(()) if text.trim().is_empty() => app.fail(format!("{} was left as it was: the text was empty", prompt.name)),
        Ok(()) if text.trim() == prompt.body.trim() => app.notify(crate::ui::theme::Role::Dim, format!("{} is unchanged", prompt.name)),
        Ok(()) => match app.db.set_prompt_body(id, text.trim()) {
            Ok(()) => app.ok(format!("Saved {}", prompt.name)),
            Err(error) => app.fail(error.to_string()),
        },
    }
    Ok(())
}

fn event_loop(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, app: &mut app::App, enhanced: bool) -> Result<()> {
    let mut dirty = true;
    let mut last_draw = Instant::now();
    loop {
        if app.notice.as_ref().is_some_and(|notice| Instant::now() > notice.until) {
            app.notice = None;
            dirty = true;
        }
        // Timers ("Thinking… 3s") and spinners move while something runs.
        let animating = app.turn.is_some() || app.busy_task.is_some() || app.drawing.is_some() || app.recording.is_some() || anim::intro_running();
        let frame = if anim::enabled() { 90 } else { 250 };
        if dirty || (animating && last_draw.elapsed() >= Duration::from_millis(frame)) {
            terminal.draw(|frame| view::draw(app, frame))?;
            last_draw = Instant::now();
            dirty = false;
            // Asking the terminal about pictures may have left marks.
            if crate::ui::picture::take_asked() {
                repaint(terminal)?;
                dirty = true;
            }
        }
        if app.quit {
            return Ok(());
        }
        if app.edit_settings {
            app.edit_settings = false;
            edit_settings(terminal, app, enhanced)?;
            dirty = true;
            continue;
        }
        if let Some(id) = app.edit_prompt.take() {
            edit_prompt(terminal, app, enhanced, &id)?;
            dirty = true;
            continue;
        }
        if event::poll(Duration::from_millis(40))? {
            // Handle everything that is waiting before drawing again, so a
            // paste or fast typing does not redraw once per character.
            let mut changed = false;
            loop {
                match event::read()? {
                    Event::Key(key) => {
                        app.on_key(key);
                        changed = true;
                    }
                    Event::Paste(text) => {
                        app.on_paste(&text);
                        changed = true;
                    }
                    // A click elsewhere changes nothing, so no redraw.
                    Event::Mouse(mouse) => changed |= app.on_mouse(mouse),
                    Event::Resize(..) => {
                        app.cache.clear();
                        terminal.autoresize()?;
                        let _ = execute!(io::stdout(), SetTitle(window_title()));
                        changed = true;
                    }
                    _ => {}
                }
                if app.quit || !event::poll(Duration::ZERO)? {
                    break;
                }
            }
            dirty |= changed;
        }
        while let Ok(message) = app.rx.try_recv() {
            app.on_worker(message);
            dirty = true;
        }
        // A marketplace search starts once typing pauses.
        dirty |= market::tick(app);
    }
}
