//! The full-screen chat: `aihat` with no arguments.

mod app;
mod composer;
mod dialogs;
mod draw;
mod files;
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
pub use tasks::test_endpoint;

pub fn run(db: Database, paths: Paths, config: AppConfig, secrets: Secrets, initial_chat: Option<String>) -> Result<()> {
    let mut app = app::App::new(db, paths, config, secrets, initial_chat)?;
    let enhanced = terminal::supports_keyboard_enhancement().unwrap_or(false);
    setup(enhanced)?;
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore(enhanced);
        previous_hook(info);
    }));
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    terminal.hide_cursor()?;
    let result = event_loop(&mut terminal, &mut app);
    restore(enhanced);
    result
}

fn setup(enhanced: bool) -> Result<()> {
    terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    // Mouse capture stays off so the terminal's own text selection works.
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableBracketedPaste,
        SetTitle(format!("aihat — {}", crate::ui::text::current_dir_label()))
    )?;
    if enhanced {
        // Lets the terminal report shift+enter separately from enter.
        execute!(
            stdout,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )?;
    }
    Ok(())
}

fn restore(enhanced: bool) {
    let mut stdout = io::stdout();
    if enhanced {
        let _ = execute!(stdout, PopKeyboardEnhancementFlags);
    }
    let _ = execute!(stdout, DisableBracketedPaste, LeaveAlternateScreen, crossterm::cursor::Show);
    let _ = terminal::disable_raw_mode();
}

fn event_loop(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, app: &mut app::App) -> Result<()> {
    let mut dirty = true;
    let mut last_draw = Instant::now();
    loop {
        if app.notice.as_ref().is_some_and(|notice| Instant::now() > notice.until) {
            app.notice = None;
            dirty = true;
        }
        // Timers ("Thinking… 3s") tick while something is running.
        let animating = app.turn.is_some() || app.busy_task.is_some();
        if dirty || (animating && last_draw.elapsed() >= Duration::from_millis(250)) {
            terminal.draw(|frame| view::draw(app, frame))?;
            last_draw = Instant::now();
            dirty = false;
        }
        if app.quit {
            return Ok(());
        }
        if event::poll(Duration::from_millis(40))? {
            // Handle everything that is waiting before drawing again, so a
            // paste or fast typing does not redraw once per character.
            loop {
                match event::read()? {
                    Event::Key(key) => app.on_key(key),
                    Event::Paste(text) => app.on_paste(&text),
                    Event::Resize(..) => {
                        app.cache.clear();
                        terminal.autoresize()?;
                    }
                    _ => {}
                }
                if app.quit || !event::poll(Duration::ZERO)? {
                    break;
                }
            }
            dirty = true;
        }
        while let Ok(message) = app.rx.try_recv() {
            app.on_worker(message);
            dirty = true;
        }
    }
}
