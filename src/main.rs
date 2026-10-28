mod api;
mod backup;
mod cli;
mod config;
mod db;
mod engine;
mod export;
mod files;
mod genui;
mod imagegen;
mod mcp;
mod memory;
mod models;
mod providers;
mod search;
mod secrets;
mod skills;
mod tui;
mod ui;

fn main() {
    std::process::exit(cli::main());
}
