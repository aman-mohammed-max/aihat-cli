mod about;
mod api;
mod backup;
mod cli;
mod config;
mod db;
mod engine;
mod export;
mod files;
mod folder;
mod genui;
mod imagegen;
mod market;
mod mcp;
mod mcp_stdio;
mod memory;
mod models;
mod oauth;
mod providers;
mod search;
mod secrets;
mod skills;
mod tui;
mod ui;

fn main() {
    // Exit quietly when a reader like `head` closes the pipe, instead of
    // panicking on the next write.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    std::process::exit(cli::main());
}
