//! `aihat providers`, `models`, `prompts`, `chats`, `mcp`, `skills` and friends.

use super::ask::cli_name;
use super::prompt;
use super::{EXIT_AUTH, EXIT_ERROR, EXIT_UNREACHABLE, EXIT_USAGE, Env, Failure};
use crate::db::Database;
use crate::models::{Model, Provider};
use crate::ui::ansi::paint;
use crate::ui::text::{self, pad, pad_left, width};
use crate::ui::theme::{Role, theme};
use anyhow::{Context, Result};
use clap::Subcommand;
use ratatui::text::Span;
use serde_json::{Value, json};
use std::io::{IsTerminal, Write};
use std::path::PathBuf;

pub(super) fn terminal_width() -> usize {
    crossterm::terminal::size().map(|(w, _)| w as usize).unwrap_or(80).saturating_sub(1)
}

fn ok(message: impl AsRef<str>) {
    println!("{} {}", paint("✓", theme().fg(Role::Accent)), message.as_ref());
}

fn dim(text: &str) -> String {
    paint(text, theme().fg(Role::Dim))
}

fn strong(text: &str) -> String {
    paint(text, theme().fg(Role::Strong))
}

fn link(text: &str) -> String {
    paint(text, theme().fg(Role::Link))
}

fn badge(text: &str) -> String {
    paint(&format!(" {text} "), theme().badge())
}

/// Find a provider by name (any case), id, or the start of its name.
pub fn find_provider(db: &Database, name: &str) -> Result<Provider> {
    let providers = db.providers()?;
    let wanted = name.trim().to_lowercase();
    let found = providers
        .iter()
        .find(|p| p.id == name || p.name.to_lowercase() == wanted)
        .or_else(|| {
            let mut matches = providers.iter().filter(|p| p.name.to_lowercase().starts_with(&wanted));
            let first = matches.next();
            if matches.next().is_some() { None } else { first }
        })
        .or_else(|| {
            let mut matches = providers.iter().filter(|p| p.name.to_lowercase().contains(&wanted));
            let first = matches.next();
            if matches.next().is_some() { None } else { first }
        });
    match found {
        Some(provider) => Ok(provider.clone()),
        None => {
            let t = theme();
            let mut failure = Failure::new(EXIT_ERROR, format!("No provider named \"{name}\""));
            let closest = providers
                .iter()
                .max_by(|a, b| {
                    strsim::jaro_winkler(&a.name.to_lowercase(), &wanted)
                        .partial_cmp(&strsim::jaro_winkler(&b.name.to_lowercase(), &wanted))
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .filter(|p| strsim::jaro_winkler(&p.name.to_lowercase(), &wanted) > 0.7);
            let mut detail = Vec::new();
            if let Some(closest) = closest {
                detail.push(Span::styled("Did you mean ", t.fg(Role::Dim)));
                detail.push(Span::styled(closest.name.clone(), t.fg(Role::Text)));
                detail.push(Span::styled("? ", t.fg(Role::Dim)));
            }
            detail.push(Span::styled("List them: ", t.fg(Role::Dim)));
            detail.push(Span::styled("aihat providers", t.fg(Role::Link)));
            failure = failure.detail(detail);
            Err(failure.into())
        }
    }
}

// ------------------------------------------------------------------ providers

#[derive(Subcommand, Debug)]
pub enum ProviderAction {
    /// List endpoints with their status
    List,
    /// Add an endpoint (guided, or with --url for scripts)
    Add {
        name: Option<String>,
        #[arg(long)]
        url: Option<String>,
        /// Read the API key from this environment variable
        #[arg(long = "key-env", value_name = "VAR")]
        key_env: Option<String>,
        /// Models to show in the picker, comma separated
        #[arg(long, value_delimiter = ',')]
        models: Vec<String>,
        /// Make it the default provider
        #[arg(long)]
        default: bool,
    },
    /// Check endpoints (all of them when no name is given)
    Test { name: Option<String> },
    /// Make this the default provider
    SetDefault { name: String },
    /// Save a new API key in the system keychain
    SetKey { name: String },
    /// Change the base URL
    SetUrl { name: String, url: String },
    /// Remove an endpoint; chats that used it stay
    Remove { name: String },
}

pub fn providers(env: &mut Env, action: Option<ProviderAction>) -> Result<i32> {
    match action.unwrap_or(ProviderAction::List) {
        ProviderAction::List => {
            list_providers(env)?;
            Ok(0)
        }
        ProviderAction::Add { name, url, key_env, models, default } => add_provider(env, name, url, key_env, models, default),
        ProviderAction::Test { name } => test_providers(env, name),
        ProviderAction::SetDefault { name } => {
            let provider = find_provider(&env.db, &name)?;
            env.db.set_default_provider(&provider.id)?;
            let model = crate::providers::resolve_provider_model(&env.db, Some(&provider.id), None).map(|(_, m)| m).ok();
            match model {
                Some(model) => {
                    env.db.set_default_model(&provider.id, &model)?;
                    ok(format!("{} is now the default provider. Default model: {}", provider.name, link(&model)));
                }
                None => ok(format!("{} is now the default provider", provider.name)),
            }
            Ok(0)
        }
        ProviderAction::SetKey { name } => {
            let provider = find_provider(&env.db, &name)?;
            let key = read_secret("API key", crate::secrets::where_keys_go(true))?;
            if key.is_empty() {
                return Err(Failure::new(EXIT_USAGE, "The key is empty; nothing was saved").into());
            }
            env.secrets.set(&format!("apikey.{}", provider.id), &key)?;
            match crate::providers::refresh_one(&env.db, &env.secrets, Some(&provider.id)) {
                Ok(summary) if summary.failures.is_empty() => {
                    let latency = env.db.provider(&provider.id)?.and_then(|p| p.latency_ms).unwrap_or(0);
                    println!(
                        "{} Key saved · {}",
                        paint("✓", theme().fg(Role::Accent)),
                        paint(
                            &format!("✓ {} · {}", text::plural(summary.model_count, "model", "models"), text::duration(latency)),
                            theme().fg(Role::Accent)
                        )
                    );
                    Ok(0)
                }
                Ok(summary) => {
                    println!("{} Key saved · {}", paint("✓", theme().fg(Role::Accent)), paint(&format!("✗ {}", summary.failures.join("; ")), theme().fg(Role::Error)));
                    Ok(EXIT_AUTH)
                }
                Err(error) => {
                    println!("{} Key saved · {}", paint("✓", theme().fg(Role::Accent)), paint(&format!("✗ {error:#}"), theme().fg(Role::Error)));
                    Ok(EXIT_ERROR)
                }
            }
        }
        ProviderAction::SetUrl { name, url } => {
            let provider = find_provider(&env.db, &name)?;
            env.db.set_provider_url(&provider.id, &url)?;
            ok(format!("{} uses {url}", provider.name));
            Ok(0)
        }
        ProviderAction::Remove { name } => {
            let provider = find_provider(&env.db, &name)?;
            if !env.yes && !prompt::confirm(&format!("Remove {}? Chats that used it stay.", provider.name))? {
                println!("{}", dim("Kept it."));
                return Ok(0);
            }
            let _ = env.secrets.delete(&format!("apikey.{}", provider.id));
            env.db.remove_provider(&provider.id)?;
            ok("Removed");
            Ok(0)
        }
    }
}

fn list_providers(env: &Env) -> Result<()> {
    let t = theme();
    let providers = env.db.providers()?;
    if providers.is_empty() {
        println!("No providers yet. Add one: {}", link("aihat providers add"));
        return Ok(());
    }
    println!("{}", dim(&format!("  {}{}{}  STATUS", pad("NAME", 23), pad("BASE URL", 34), pad_left("MODELS", 6))));
    let mut total = 0;
    for provider in &providers {
        let models = env.db.models(Some(&provider.id))?.iter().filter(|m| m.enabled).count();
        total += models;
        let marker = if provider.is_default { paint("●", t.fg(Role::Accent)) } else { " ".into() };
        let name_cell = if provider.is_default {
            let room = 23usize.saturating_sub(10);
            format!("{} {}{}", strong(&text::truncate(&provider.name, room)), badge("default"), " ".repeat(room.saturating_sub(width(&text::truncate(&provider.name, room)))))
        } else {
            strong(&pad(&provider.name, 23))
        };
        let (status, role) = match provider.status.as_str() {
            "online" | "ok" => (format!("✓ {}", provider.latency_ms.map(text::duration).unwrap_or_else(|| "ready".into())), Role::Accent),
            "rejected" => ("✗ key rejected (401)".to_string(), Role::Error),
            "offline" => ("! offline".to_string(), Role::Warn),
            _ => ("· not tested".to_string(), Role::Dim),
        };
        let count = if models == 0 { "—".to_string() } else { models.to_string() };
        println!(
            "{marker} {name_cell}{}{}  {}",
            pad(&provider.base_url, 34),
            pad_left(&count, 6),
            paint(&status, t.fg(role))
        );
    }
    println!();
    println!(
        "{}",
        dim(&format!(
            "{} · {} · same data as the app",
            text::plural(providers.len(), "provider", "providers"),
            text::plural(total, "model", "models")
        ))
    );
    Ok(())
}

fn test_providers(env: &Env, name: Option<String>) -> Result<i32> {
    let t = theme();
    let providers = match name {
        Some(name) => vec![find_provider(&env.db, &name)?],
        None => env.db.providers()?,
    };
    if providers.is_empty() {
        println!("No providers yet. Add one: {}", link("aihat providers add"));
        return Ok(0);
    }
    let mut code = 0;
    for provider in providers {
        match crate::api::list_models(&provider, &env.secrets) {
            Ok((models, latency)) => {
                let _ = crate::providers::refresh_one(&env.db, &env.secrets, Some(&provider.id));
                println!(
                    "{} {}  {}  {}",
                    paint("✓", t.fg(Role::Accent)),
                    strong(&provider.name),
                    dim(&provider.base_url),
                    paint(&format!("{} · {}", text::plural(models.len(), "model", "models"), text::duration(latency)), t.fg(Role::Accent))
                );
            }
            Err(error) => {
                let api = crate::api::api_error(&error).cloned();
                let status = match api.as_ref().map(|a| a.kind) {
                    Some(crate::api::ErrorKind::Auth) => "rejected",
                    Some(crate::api::ErrorKind::Unreachable) => "offline",
                    _ => "offline",
                };
                env.db.set_provider_status(&provider.id, status, None)?;
                println!("{} {}  {}", paint("✗", t.fg(Role::Error)), strong(&provider.name), provider.base_url);
                println!("  {}", api.as_ref().map(|a| a.to_string()).unwrap_or_else(|| format!("{error:#}")));
                match api.map(|a| a.kind) {
                    Some(crate::api::ErrorKind::Auth) => {
                        println!("  {} {}", dim("Fix it:"), link(&format!("aihat providers set-key {}", cli_name(&provider.name))));
                        code = code.max(EXIT_AUTH);
                    }
                    Some(crate::api::ErrorKind::Unreachable) => {
                        println!("  {} {}", dim("Check the server is running, or change the URL:"), link(&format!("aihat providers set-url {} URL", cli_name(&provider.name))));
                        code = code.max(EXIT_UNREACHABLE);
                    }
                    _ => code = code.max(EXIT_ERROR),
                }
            }
        }
    }
    Ok(code)
}

fn read_secret(title: &str, hint: &str) -> Result<String> {
    if std::io::stdin().is_terminal() {
        return prompt::input(title, hint, "", true);
    }
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(line.trim().to_string())
}

const PRESETS: [&str; 7] = ["Custom", "OpenRouter", "Groq", "Ollama", "LM Studio", "vLLM", "FreeLLMAPI"];

fn add_provider(
    env: &mut Env,
    name: Option<String>,
    url: Option<String>,
    key_env: Option<String>,
    wanted_models: Vec<String>,
    make_default: bool,
) -> Result<i32> {
    // Without questions, for scripts.
    if let Some(url) = url {
        let name = name.ok_or_else(|| Failure::new(EXIT_USAGE, "Give the endpoint a name: aihat providers add NAME --url URL"))?;
        let key = match &key_env {
            Some(var) => Some(std::env::var(var).map_err(|_| Failure::new(EXIT_USAGE, format!("${var} is not set")))?),
            None => None,
        };
        let id = env.db.insert_provider(&name, &url, &json!({}), 60_000)?;
        if let Some(key) = key.filter(|k| !k.trim().is_empty()) {
            env.secrets.set(&format!("apikey.{id}"), key.trim())?;
        }
        let summary = crate::providers::refresh_one(&env.db, &env.secrets, Some(&id));
        if !wanted_models.is_empty() {
            for model in env.db.models(Some(&id))? {
                env.db.set_model_enabled(&id, &model.id, wanted_models.contains(&model.id))?;
            }
            let provider = env.db.provider(&id)?.context("the new provider was not saved")?;
            for model in &wanted_models {
                if env.db.model(&id, model)?.is_none() {
                    crate::providers::add_model(&env.db, &provider, model, None, "chat")?;
                }
            }
        }
        if make_default {
            env.db.set_default_provider(&id)?;
            if let Ok((_, model)) = crate::providers::resolve_provider_model(&env.db, Some(&id), None) {
                env.db.set_default_model(&id, &model)?;
            }
        }
        match summary {
            Ok(summary) if summary.failures.is_empty() => ok(format!("Added {name} · {}", text::plural(summary.model_count, "model", "models"))),
            Ok(summary) => println!("{} Added {name}, but: {}", paint("!", theme().fg(Role::Warn)), summary.failures.join("; ")),
            Err(error) => println!("{} Added {name}, but loading models failed: {error:#}", paint("!", theme().fg(Role::Warn))),
        }
        return Ok(0);
    }
    if !prompt::interactive() {
        return Err(Failure::new(EXIT_USAGE, "aihat providers add asks questions, so it needs a terminal")
            .note("For scripts: aihat providers add NAME --url URL --key-env VAR")
            .into());
    }
    let t = theme();
    prompt::intro("Add an OpenAI-compatible endpoint");
    let preset = PRESETS[prompt::choose("Preset", &PRESETS, 0)?];
    let default_name = name.unwrap_or_else(|| if preset == "Custom" { String::new() } else { preset.to_string() });
    let name = loop {
        let value = prompt::input("Name", "", &default_name, false)?;
        if !value.is_empty() {
            break value;
        }
    };
    let preset_url = crate::providers::preset_url(preset).unwrap_or("");
    let url = loop {
        let value = prompt::input("Base URL", "ends in /v1", preset_url, false)?;
        if value.starts_with("http://") || value.starts_with("https://") {
            break value;
        }
        eprintln!("{} {}", paint("│", t.fg(Role::Dim)), paint("The URL starts with http:// or https://", t.fg(Role::Error)));
    };
    let key = match &key_env {
        Some(var) => std::env::var(var).unwrap_or_default(),
        None => prompt::input("API key", &format!("{} · leave empty if none", crate::secrets::where_keys_go(true)), "", true)?,
    };
    let tested = crate::tui::test_endpoint(&url, &key, &json!({}));
    let found = match &tested {
        Ok((latency, models)) => {
            prompt::step(
                "Testing connection…",
                &format!("✓ Connected · {} · {}", text::duration(*latency), text::plural(models.len(), "model", "models")),
                Role::Accent,
            );
            models.clone()
        }
        Err(error) => {
            prompt::step("Testing connection…", &format!("✗ {error}"), Role::Error);
            if !prompt::yes_no("Save it anyway?", false)? {
                prompt::outro();
                println!("{}", dim("Nothing was saved."));
                return Ok(EXIT_ERROR);
            }
            Vec::new()
        }
    };
    let id = env.db.insert_provider(&name, &url, &json!({}), 60_000)?;
    if !key.is_empty() {
        env.secrets.set(&format!("apikey.{id}"), &key)?;
    }
    if !found.is_empty() {
        let _ = crate::providers::refresh_one(&env.db, &env.secrets, Some(&id));
        let models = env.db.models(Some(&id))?;
        let items: Vec<(String, String)> = models.iter().map(|m| (m.id.clone(), crate::tui::model_caps_long(m))).collect();
        let mut checked: Vec<bool> = models.iter().map(|m| m.is_chat() || m.caps.get("kind").and_then(Value::as_str) == Some("transcription")).collect();
        prompt::toggles("Models to show in the picker", "space toggles · enter confirms", &items, &mut checked)?;
        for (model, on) in models.iter().zip(checked) {
            if model.enabled != on {
                env.db.set_model_enabled(&id, &model.id, on)?;
            }
        }
    }
    let first = env.db.providers()?.len() == 1;
    if first || prompt::yes_no(&format!("Make {name} the default?"), make_default)? {
        env.db.set_default_provider(&id)?;
        if let Ok((_, model)) = crate::providers::resolve_provider_model(&env.db, Some(&id), None) {
            env.db.set_default_model(&id, &model)?;
        }
    }
    prompt::outro();
    println!("{} Added {name}. Try: {}", paint("✓", t.fg(Role::Accent)), link("aihat \"hello\""));
    Ok(0)
}

// ------------------------------------------------------------------ models

#[derive(Subcommand, Debug)]
pub enum ModelAction {
    /// List models (-P picks the provider)
    List {
        #[arg(long)]
        all: bool,
    },
    /// Reload model lists from providers
    Refresh { name: Option<String> },
    /// Make a model the default
    SetDefault { model: String },
    /// Add a model the endpoint does not list
    Add {
        model: String,
        #[arg(long, default_value = "chat")]
        kind: String,
        #[arg(long)]
        name: Option<String>,
    },
}

pub fn models(env: &mut Env, action: Option<ModelAction>, provider: Option<&str>) -> Result<i32> {
    let t = theme();
    match action.unwrap_or(ModelAction::List { all: false }) {
        ModelAction::List { all } => {
            let provider = match provider {
                Some(name) => find_provider(&env.db, name)?,
                None => env.db.default_provider()?.ok_or_else(|| Failure::new(EXIT_ERROR, "No provider yet").note("Add one: aihat providers add"))?,
            };
            let settings = env.db.settings();
            let default_model = settings.get("defaultModelId").and_then(Value::as_str).filter(|_| provider.is_default);
            let mut models: Vec<Model> = env.db.models(Some(&provider.id))?.into_iter().filter(|m| all || m.enabled).collect();
            models.sort_by_key(|m| (Some(m.id.as_str()) != default_model, !m.is_chat()));
            if models.is_empty() {
                println!("{} has no models yet. Load them: {}", provider.name, link(&format!("aihat models refresh {}", cli_name(&provider.name))));
                return Ok(0);
            }
            println!("{}", dim(&format!("  {}CONTEXT  TOOLS  VISION", pad("ID", 28))));
            let limit = if all { usize::MAX } else { 20 };
            for model in models.iter().take(limit) {
                let is_default = Some(model.id.as_str()) == default_model;
                let marker = if is_default { paint("●", t.fg(Role::Accent)) } else { " ".into() };
                let id_cell = if is_default {
                    let room = 28usize.saturating_sub(10);
                    let id = text::truncate(&model.id, room);
                    format!("{} {}{}", strong(&id), badge("default"), " ".repeat(room.saturating_sub(width(&id))))
                } else {
                    strong(&pad(&model.id, 28))
                };
                let context = model.context_k().map(|k| format!("{k}k")).unwrap_or_default();
                let tick = |on: bool| if on { paint("✓", t.fg(Role::Accent)) } else { " ".into() };
                let vision = model.caps.get("vision").and_then(Value::as_bool).unwrap_or(false);
                let kind = model.caps.get("kind").and_then(Value::as_str).unwrap_or("chat");
                let note = if kind == "chat" { String::new() } else { format!("   {}", dim(kind)) };
                println!(
                    "{marker} {id_cell}{}    {}      {}{note}",
                    pad_left(&context, 6),
                    tick(model.supports_tools() && kind == "chat"),
                    tick(vision)
                );
            }
            if models.len() > limit {
                println!(
                    "  {}",
                    dim(&format!("… {} more · aihat models -P {} --all", models.len() - limit, cli_name(&provider.name)))
                );
            }
            Ok(0)
        }
        ModelAction::Refresh { name } => {
            let summary = crate::providers::refresh_one(&env.db, &env.secrets, name.as_deref().or(provider))?;
            let role = if summary.failures.is_empty() { Role::Accent } else { Role::Warn };
            println!("{} {}", paint(summary.marker(), t.fg(role)), summary.describe());
            Ok(if summary.failures.is_empty() { 0 } else { EXIT_ERROR })
        }
        ModelAction::SetDefault { model } => {
            let provider = match provider {
                Some(name) => find_provider(&env.db, name)?,
                None => {
                    let owner = env.db.models(None)?.into_iter().find(|m| m.id == model).map(|m| m.provider_id);
                    match owner {
                        Some(id) => env.db.provider(&id)?.context("provider not found")?,
                        None => env.db.default_provider()?.ok_or_else(|| Failure::new(EXIT_ERROR, "No provider yet"))?,
                    }
                }
            };
            if env.db.model(&provider.id, &model)?.is_none() {
                return Err(Failure::new(EXIT_ERROR, format!("{} has no model \"{model}\".", provider.name))
                    .detail(vec![Span::styled("List them: ", t.fg(Role::Dim)), Span::styled(format!("aihat models -P {}", cli_name(&provider.name)), t.fg(Role::Link))])
                    .into());
            }
            env.db.set_default_model(&provider.id, &model)?;
            ok(format!("Default model: {model} on {}", provider.name));
            Ok(0)
        }
        ModelAction::Add { model, kind, name } => {
            let provider = match provider {
                Some(name) => find_provider(&env.db, name)?,
                None => env.db.default_provider()?.ok_or_else(|| Failure::new(EXIT_ERROR, "No provider yet"))?,
            };
            crate::providers::add_model(&env.db, &provider, &model, name.as_deref(), &kind)?;
            ok(format!("Added {model} to {}", provider.name));
            Ok(0)
        }
    }
}

// ------------------------------------------------------------------ prompts

#[derive(Subcommand, Debug)]
pub enum PromptAction {
    /// List saved system prompts
    List {
        #[arg(long)]
        search: Option<String>,
        #[arg(long)]
        all: bool,
    },
    /// Print a prompt
    Show { name: String },
    /// Save a new prompt (reads the text from --body or stdin)
    Add {
        name: String,
        #[arg(short, long)]
        body: Option<String>,
    },
    /// Describe an assistant; the model writes its system prompt and saves it
    New {
        #[arg(required = true, num_args = 1.., value_name = "DESCRIPTION")]
        description: Vec<String>,
        /// Also make new chats use it
        #[arg(long = "use")]
        use_it: bool,
    },
    /// New chats use this prompt ("none" to clear)
    Use { name: String },
    /// Delete a prompt
    Delete { name: String },
}

fn find_prompt(env: &Env, name: &str) -> Result<crate::models::Prompt> {
    let wanted = name.to_lowercase();
    let prompts = env.db.prompts()?;
    prompts
        .iter()
        .find(|p| p.id == name || p.name.to_lowercase() == wanted)
        .or_else(|| prompts.iter().find(|p| p.name.to_lowercase().contains(&wanted)))
        .cloned()
        .ok_or_else(|| Failure::new(EXIT_ERROR, format!("No system prompt named \"{name}\"")).note("List them: aihat prompts").into())
}

pub fn prompts(env: &mut Env, action: Option<PromptAction>) -> Result<i32> {
    let t = theme();
    match action.unwrap_or(PromptAction::List { search: None, all: false }) {
        PromptAction::List { search, all } => {
            let settings = env.db.settings();
            let in_use = settings.get("defaultPromptId").and_then(Value::as_str).map(str::to_owned);
            let mut prompts = env.db.prompts()?;
            if let Some(query) = &search {
                let query = query.to_lowercase();
                prompts.retain(|p| p.name.to_lowercase().contains(&query) || p.tags.iter().any(|tag| tag.to_lowercase().contains(&query)));
            }
            prompts.sort_by_key(|p| (Some(&p.id) != in_use.as_ref(), !p.favorite));
            if prompts.is_empty() {
                println!("No saved prompts. Add one: {}", link("aihat prompts add NAME"));
                return Ok(0);
            }
            println!("{}", dim(&format!("  {}{}TOKENS", pad("NAME", 31), pad("TAGS", 16))));
            let limit = if all || search.is_some() { usize::MAX } else { 20 };
            for prompt in prompts.iter().take(limit) {
                let active = Some(&prompt.id) == in_use.as_ref();
                let marker = if active { paint("*", t.fg(Role::Warn)) } else { " ".into() };
                let name_cell = if active {
                    let room = 31usize.saturating_sub(10);
                    let name = text::truncate(&prompt.name, room);
                    format!("{} {}{}", strong(&name), badge("in use"), " ".repeat((room + 1).saturating_sub(width(&name) + 1)))
                } else {
                    strong(&pad(&prompt.name, 31))
                };
                println!("{marker} {name_cell}{}{}", pad(&prompt.tags.join(", "), 16), pad_left(&format!("~{}", prompt.body.len() / 4), 6));
            }
            if prompts.len() > limit {
                println!("  {}", dim(&format!("… {} more · aihat prompts --search WORD", prompts.len() - limit)));
            }
            Ok(0)
        }
        PromptAction::Show { name } => {
            let prompt = find_prompt(env, &name)?;
            println!("{}", paint(&prompt.name, t.bold(Role::Strong)));
            if !prompt.tags.is_empty() {
                println!("{}", dim(&prompt.tags.join(", ")));
            }
            println!();
            println!("{}", prompt.body.trim_end());
            Ok(0)
        }
        PromptAction::Add { name, body } => {
            let body = match body {
                Some(body) => body,
                None if !std::io::stdin().is_terminal() => {
                    let mut text = String::new();
                    std::io::Read::read_to_string(&mut std::io::stdin(), &mut text)?;
                    text
                }
                None => prompt::input("Prompt text", "one line; pipe a file for longer prompts", "", false)?,
            };
            if body.trim().is_empty() {
                return Err(Failure::new(EXIT_USAGE, "The prompt text is empty").into());
            }
            env.db.add_prompt(&name, body.trim())?;
            ok(format!("Saved {name}"));
            Ok(0)
        }
        PromptAction::New { description, use_it } => {
            let description = description.join(" ");
            let (provider, model) = crate::providers::resolve_provider_model(&env.db, None, None)?;
            eprintln!("{}", dim(&format!("Writing a system prompt with {model}…")));
            let (name, body) = crate::engine::write_system_prompt(&env.secrets, &provider, &model, &description)?;
            let id = env.db.add_prompt_tagged(&name, &body, &["generated"])?;
            println!("{}\n", strong(&name));
            println!("{body}\n");
            if use_it {
                let mut settings = env.db.settings();
                settings["defaultPromptId"] = json!(id);
                env.db.set_kv("settings", &settings.to_string())?;
                ok(format!("Saved {name} · new chats use it"));
            } else {
                ok(format!("Saved {name} · use it with: aihat prompts use \"{name}\""));
            }
            Ok(0)
        }
        PromptAction::Use { name } => {
            let mut settings = env.db.settings();
            if matches!(name.to_lowercase().as_str(), "none" | "off") {
                if let Some(map) = settings.as_object_mut() {
                    map.remove("defaultPromptId");
                }
                env.db.set_kv("settings", &settings.to_string())?;
                ok("New chats use no system prompt");
                return Ok(0);
            }
            let prompt = find_prompt(env, &name)?;
            settings["defaultPromptId"] = json!(prompt.id);
            env.db.set_kv("settings", &settings.to_string())?;
            ok(format!("New chats use {}", prompt.name));
            Ok(0)
        }
        PromptAction::Delete { name } => {
            let prompt = find_prompt(env, &name)?;
            if !env.yes && !prompt::confirm(&format!("Delete the prompt {}?", prompt.name))? {
                return Ok(0);
            }
            env.db.delete_prompt(&prompt.id)?;
            ok(format!("Deleted {}", prompt.name));
            Ok(0)
        }
    }
}

// ------------------------------------------------------------------ chats

#[derive(Subcommand, Debug)]
pub enum ChatAction {
    /// List recent chats
    List {
        #[arg(long)]
        all: bool,
        #[arg(long)]
        archived: bool,
    },
    /// Print a chat
    Show { id: String },
    /// Open a chat in the full-screen chat
    Open { id: String },
    /// Write a chat as Markdown (default) or JSON
    Export {
        id: String,
        #[arg(long)]
        md: bool,
        #[arg(long)]
        json: bool,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Rename a chat
    Rename { id: String, title: String },
    /// Archive or unarchive a chat
    Archive { id: String },
    /// Delete a chat for good
    Delete { id: String },
}

pub enum ChatOutcome {
    Done(i32),
    Open(String),
}

/// A chat by number (`41`, `#41`), id, or id prefix.
fn find_chat(db: &Database, id: &str) -> Result<crate::models::Chat> {
    let trimmed = id.trim_start_matches('#');
    if let Ok(number) = trimmed.parse::<usize>()
        && let Some(chat) = db.chat_by_number(number)? {
            return Ok(chat);
        }
    db.chat(id)?
        .ok_or_else(|| Failure::new(EXIT_ERROR, format!("No chat {id}")).note("List them: aihat chats").into())
}

pub fn chats(env: &mut Env, action: Option<ChatAction>) -> Result<ChatOutcome> {
    let t = theme();
    match action.unwrap_or(ChatAction::List { all: false, archived: false }) {
        ChatAction::List { all, archived } => {
            let chats: Vec<_> = env.db.chats(archived)?.into_iter().filter(|c| !archived || c.archived).collect();
            if chats.is_empty() {
                println!("No chats yet. Start one: {}", link("aihat \"hello\""));
                return Ok(ChatOutcome::Done(0));
            }
            println!("{}", dim(&format!("  {}{}{}TITLE", pad("#", 4), pad("UPDATED", 13), pad("MODEL", 15))));
            let limit = if all { usize::MAX } else { 20 };
            for chat in chats.iter().take(limit) {
                let number = env.db.chat_number(&chat.id).unwrap_or(0);
                println!(
                    "  {}{}{}{}",
                    link(&pad(&number.to_string(), 4)),
                    pad(&text::relative_time(chat.updated_at), 13),
                    pad(chat.model_id.as_deref().unwrap_or(""), 15),
                    chat.title
                );
            }
            if chats.len() > limit {
                println!("  {}", dim(&format!("… {} more · aihat chats --all", chats.len() - limit)));
            }
            Ok(ChatOutcome::Done(0))
        }
        ChatAction::Show { id } => {
            let chat = find_chat(&env.db, &id)?;
            let width = crossterm::terminal::size().map(|(w, _)| w as usize).unwrap_or(80).saturating_sub(1).clamp(20, 78);
            println!("{}", paint(&chat.title, t.bold(Role::Strong)));
            for message in env.db.messages(&chat.id)? {
                match message.role.as_str() {
                    "user" => {
                        println!();
                        println!("{} {}", link("›"), strong(message.content.trim()));
                    }
                    "assistant" => {
                        let (_, answer) = crate::engine::split_reasoning(&message.content);
                        println!();
                        let rows = crate::ui::markdown::render(&answer, crate::ui::markdown::MdOptions { width, indent: 0, block_indent: 2, wide: terminal_width() });
                        print!("{}", crate::ui::ansi::lines(&rows));
                    }
                    _ => {}
                }
            }
            Ok(ChatOutcome::Done(0))
        }
        ChatAction::Open { id } => Ok(ChatOutcome::Open(find_chat(&env.db, &id)?.id)),
        ChatAction::Export { id, md: _, json: as_json, output } => {
            let chat = find_chat(&env.db, &id)?;
            let content = if as_json {
                serde_json::to_string_pretty(&crate::export::chat_json(&env.db, &chat)?)?
            } else {
                crate::export::chat_markdown(&env.db, &chat)?
            };
            let count = env.db.messages(&chat.id)?.iter().filter(|m| matches!(m.role.as_str(), "user" | "assistant")).count();
            let target = match output {
                Some(path) => {
                    std::fs::write(&path, &content).with_context(|| format!("writing {}", path.display()))?;
                    Some(path.display().to_string())
                }
                None => {
                    let mut out = std::io::stdout().lock();
                    out.write_all(content.as_bytes())?;
                    out.flush()?;
                    redirected_to()
                }
            };
            if let Some(target) = target {
                eprintln!("{} Wrote {} to {target}", paint("✓", t.fg(Role::Accent)), text::plural(count, "message", "messages"));
            }
            Ok(ChatOutcome::Done(0))
        }
        ChatAction::Rename { id, title } => {
            let chat = find_chat(&env.db, &id)?;
            env.db.rename_chat(&chat.id, &title)?;
            ok(format!("Renamed to {title}"));
            Ok(ChatOutcome::Done(0))
        }
        ChatAction::Archive { id } => {
            let chat = find_chat(&env.db, &id)?;
            env.db.toggle_archive(&chat.id)?;
            ok(if chat.archived { format!("Unarchived {}", chat.title) } else { format!("Archived {}", chat.title) });
            Ok(ChatOutcome::Done(0))
        }
        ChatAction::Delete { id } => {
            let chat = find_chat(&env.db, &id)?;
            if !env.yes && !prompt::confirm(&format!("Delete “{}” for good?", chat.title))? {
                return Ok(ChatOutcome::Done(0));
            }
            env.db.delete_chat(&chat.id)?;
            ok("Deleted");
            Ok(ChatOutcome::Done(0))
        }
    }
}

/// When stdout was redirected to a file, its name (Linux and BSD only).
fn redirected_to() -> Option<String> {
    if std::io::stdout().is_terminal() {
        return None;
    }
    let target = std::fs::read_link("/proc/self/fd/1").ok()?;
    let shown = target.strip_prefix(std::env::current_dir().ok()?).map(|p| p.display().to_string()).unwrap_or_else(|_| target.display().to_string());
    (!shown.starts_with("pipe:") && !shown.starts_with("/dev/")).then_some(shown)
}

// ------------------------------------------------------------------ mcp

#[derive(Subcommand, Debug)]
pub enum McpAction {
    /// List MCP servers
    List,
    /// Add a server: its URL, a command after --, or a name from `aihat mcp search`
    Add {
        name: String,
        url: Option<String>,
        /// Access token, read from this environment variable
        #[arg(long = "token-env", value_name = "VAR")]
        token_env: Option<String>,
        #[arg(last = true)]
        command: Vec<String>,
    },
    /// Find servers to add: hosted ones and the official MCP registry
    Search { query: Vec<String> },
    /// Sign in with your browser
    Login { name: String },
    /// Sign out (the server stays)
    Logout { name: String },
    /// Start a command server and load its tools
    Start { name: String },
    /// Reconnect and reload a server's tools
    Refresh { name: String },
    /// Save an access token for a server
    SetKey { name: String },
    /// Run its write tools without asking
    Allow { name: String },
    /// Ask before running its write tools
    Ask { name: String },
    /// Remove a server
    Remove { name: String },
}

fn find_mcp(env: &Env, name: &str) -> Result<crate::models::McpServer> {
    env.db
        .mcp_servers()?
        .into_iter()
        .find(|s| s.id == name || s.name.eq_ignore_ascii_case(name))
        .ok_or_else(|| Failure::new(EXIT_ERROR, format!("No MCP server named \"{name}\"")).note("List them: aihat mcp").into())
}

/// Connect a server, signing in through the browser when it needs that, and
/// print the outcome: `✓ Signed in · 21 tools added · ask before running: on`.
fn connect_mcp(env: &mut Env, id: &str, name: &str, added: bool) -> Result<i32> {
    let t = theme();
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let mut signed_in = false;
    let mut opened = |url: &str| {
        signed_in = true;
        eprintln!("{} {name} needs you to sign in. Opening your browser…", paint("◇", t.fg(Role::Accent)));
        eprintln!("{} {} {}", paint("│", t.fg(Role::Dim)), dim("If it doesn't open:"), link(&text::truncate(url, terminal_width().saturating_sub(22).max(40))));
    };
    let result = crate::mcp::connect_server(&env.db, &env.secrets, id, &cancel, &mut opened);
    let server = find_mcp(env, id)?;
    match result {
        Ok(count) => {
            let asks = if crate::mcp::server_always_allowed(&env.db, id) { "off" } else { "on" };
            let tools = text::plural(count, "tool", "tools");
            if signed_in {
                ok(format!("Signed in · {tools} added · {}", dim(&format!("ask before running: {asks}"))));
            } else if added {
                println!(
                    "{} Added {name} {} · {tools} · {}",
                    paint("✓", t.fg(Role::Accent)),
                    dim(&format!("({})", server.transport)),
                    dim(&format!("ask before running: {asks}"))
                );
            } else {
                ok(format!("{name} · {tools}"));
            }
            Ok(0)
        }
        Err(error) => {
            let prefix = if added { format!("Added {name}, but it did not connect") } else { format!("{name} did not connect") };
            match crate::mcp::needs_sign_in(&error) {
                Some(need) if !need.browser => {
                    // A server that wants a pasted token: ask for it now.
                    if prompt::interactive() {
                        eprintln!("{} {name} needs an access token.", paint("◇", t.fg(Role::Accent)));
                        let token = read_secret("Access token", crate::secrets::where_keys_go(true))?;
                        if !token.is_empty() {
                            env.secrets.set_mcp_token(id, &token)?;
                            env.db.set_mcp_auth(id, "token")?;
                            return connect_mcp(env, id, name, added);
                        }
                    }
                    println!("{} {prefix}: it needs an access token.", paint("!", t.fg(Role::Warn)));
                    println!("  {} {}", dim("Save it:"), link(&format!("aihat mcp set-key {name}")));
                    Ok(EXIT_AUTH)
                }
                Some(_) => {
                    println!("{} {prefix}: {}", paint("!", t.fg(Role::Warn)), crate::tui::describe_error(&error));
                    Ok(EXIT_AUTH)
                }
                None => {
                    let message = crate::tui::describe_error(&error);
                    println!("{} {prefix}: {message}", paint("!", t.fg(Role::Warn)));
                    let no_browser = message.contains("No web browser");
                    Ok(if no_browser { EXIT_ERROR } else { EXIT_UNREACHABLE })
                }
            }
        }
    }
}

/// `aihat mcp add deepwiki`: find it in the marketplace, ask for what it
/// needs, add it and connect (signing in through the browser if it asks).
fn add_from_market(env: &mut Env, name: &str, token_env: Option<&str>) -> Result<i32> {
    let t = theme();
    let wanted = name.to_lowercase();
    let mut found = crate::market::featured_mcp();
    if !found.iter().any(|f| f.short_name() == wanted || f.id.eq_ignore_ascii_case(name)) {
        found = crate::market::find_mcp(name).map_err(|e| Failure::new(EXIT_UNREACHABLE, e.to_string()))?;
    }
    let item = found
        .iter()
        .find(|f| f.short_name() == wanted || f.id.eq_ignore_ascii_case(name) || f.title.eq_ignore_ascii_case(name))
        .cloned()
        .ok_or_else(|| Failure::new(EXIT_USAGE, format!("No MCP server named \"{name}\" in the marketplace")).note("Find one: aihat mcp search, or give its address: aihat mcp add NAME https://…/mcp"))?;
    let plan = crate::market::plan_mcp(&item);
    if !plan.supported {
        return Err(Failure::new(EXIT_ERROR, format!("{} can't be added: {}", item.title, plan.reason)).into());
    }
    let mut token = token_env.map(|var| std::env::var(var).unwrap_or_default());
    if token.is_none()
        && let Some((what, required)) = &plan.token
    {
        if prompt::interactive() {
            let hint = format!("{what}{}", if *required { "" } else { " · optional, enter skips" });
            let value = read_secret("Access token", &hint)?;
            token = Some(value);
        } else if *required {
            return Err(Failure::new(EXIT_USAGE, format!("{} needs an access token", item.title)).note(format!("aihat mcp add {} --token-env VAR", item.short_name())).into());
        }
    }
    let mut values = Vec::new();
    for (var, about) in &plan.env {
        if let Ok(value) = std::env::var(var) {
            values.push((var.clone(), value));
            continue;
        }
        if !about.required {
            continue;
        }
        if !prompt::interactive() {
            return Err(Failure::new(EXIT_USAGE, format!("{} needs {var} ({})", item.title, about.description)).note(format!("Set {var} in your shell, then add it again")).into());
        }
        let value = prompt::input(var, &about.description, "", about.secret)?;
        values.push((var.clone(), value));
    }
    let id = crate::market::add_mcp(&env.db, &env.secrets, &item, &plan, token.as_deref(), &values)?;
    let added_name = env.db.mcp_servers()?.into_iter().find(|s| s.id == id).map(|s| s.name).unwrap_or_else(|| item.short_name());
    println!("{} {} {}", paint("◇", t.fg(Role::Accent)), strong(&item.title), dim(&format!("· {}", item.connection_label())));
    connect_mcp(env, &id, &added_name, true)
}

pub fn mcp(env: &mut Env, action: Option<McpAction>) -> Result<i32> {
    let t = theme();
    match action.unwrap_or(McpAction::List) {
        McpAction::List => {
            let servers = env.db.mcp_servers()?;
            if servers.is_empty() {
                println!("No MCP servers yet. Add one: {}", link("aihat mcp add NAME https://…/mcp"));
                return Ok(0);
            }
            println!("{}", dim(&format!("  {}{}{}  STATUS", pad("NAME", 12), pad("TRANSPORT", 11), pad_left("TOOLS", 5))));
            for server in servers {
                let allowed = crate::mcp::server_always_allowed(&env.db, &server.id);
                let ready = server.connected || !server.tools.is_empty();
                let status = if server.auth == "oauth" && !crate::oauth::is_signed_in(&env.secrets, &server.id) {
                    format!("{} {}", paint("! signed out ·", t.fg(Role::Warn)), link(&format!("aihat mcp login {}", server.name)))
                } else if !ready && server.transport == "stdio" {
                    format!("{} {}", paint("! not running ·", t.fg(Role::Warn)), link(&format!("aihat mcp start {}", server.name)))
                } else if !ready {
                    format!("{} {}", paint("! not connected ·", t.fg(Role::Warn)), link(&format!("aihat mcp refresh {}", server.name)))
                } else {
                    let how = if server.auth == "oauth" { "signed in" } else { "ready" };
                    paint(&format!("✓ {how}{}", if allowed { " · runs without asking" } else { "" }), t.fg(Role::Accent))
                };
                println!("  {}{}{}  {status}", strong(&pad(&server.name, 12)), pad(&server.transport, 11), pad_left(&server.tools.len().to_string(), 5));
            }
            Ok(0)
        }
        McpAction::Search { query } => {
            let query = query.join(" ");
            let spinner = super::ask::Spinner::new(std::io::stderr().is_terminal());
            spinner.set("Looking…");
            let found = crate::market::find_mcp(&query);
            spinner.stop();
            let found = found.map_err(|e| Failure::new(EXIT_UNREACHABLE, e.to_string()))?;
            if found.is_empty() {
                println!("Nothing found for \"{query}\".");
                return Ok(0);
            }
            let added: Vec<String> = env.db.mcp_servers()?.into_iter().map(|s| s.url.trim_end_matches('/').to_string()).collect();
            for item in found.iter().take(30) {
                let plan = crate::market::plan_mcp(item);
                let is_added = added.iter().any(|u| *u == plan.target.trim_end_matches('/'));
                let meta = item.category.clone().unwrap_or_else(|| item.publisher.clone());
                println!(
                    "  {}{}{}",
                    strong(&pad(&text::truncate(&item.short_name(), 20), 22)),
                    dim(&pad(&format!("{meta} · {}", item.connection_label()), 46)),
                    if is_added { paint("added", t.fg(Role::Accent)) } else if !plan.supported { dim("can't add") } else { String::new() }
                );
                if !item.description.is_empty() {
                    println!("  {}", dim(&text::truncate(&format!("  {}", item.description.replace('\n', " ")), terminal_width().saturating_sub(4).max(40))));
                }
            }
            println!();
            println!("{} {}", dim("Add one:"), link(&format!("aihat mcp add {}", found[0].short_name())));
            Ok(0)
        }
        McpAction::Add { name, url: None, token_env, command } if command.is_empty() => add_from_market(env, &name, token_env.as_deref()),
        McpAction::Add { name, url, token_env, command } => {
            let (address, transport) = if !command.is_empty() {
                (crate::mcp_stdio::command_line(&command), "stdio")
            } else {
                match url {
                    Some(url) if url.starts_with("http://") || url.starts_with("https://") => (url, "http"),
                    _ => {
                        return Err(Failure::new(EXIT_USAGE, "Give the server's address, or the command that runs it after --")
                            .note("aihat mcp add NAME https://…/mcp  ·  aihat mcp add NAME -- npx -y @scope/server")
                            .into());
                    }
                }
            };
            if env.db.mcp_servers()?.iter().any(|s| s.name.eq_ignore_ascii_case(&name)) {
                return Err(Failure::new(EXIT_USAGE, format!("There is already a server named \"{name}\"")).note("Pick another name, or remove it first: aihat mcp remove NAME").into());
            }
            let auth = if token_env.is_some() { "token" } else { "none" };
            let id = env.db.add_mcp_server(&name, &address, transport, auth)?;
            if let Some(var) = token_env {
                let token = std::env::var(&var).map_err(|_| Failure::new(EXIT_USAGE, format!("${var} is not set")))?;
                env.secrets.set_mcp_token(&id, token.trim())?;
            }
            connect_mcp(env, &id, &name, true)
        }
        McpAction::Login { name } => {
            let server = find_mcp(env, &name)?;
            if server.transport == "stdio" {
                return Err(Failure::new(EXIT_USAGE, format!("{} runs on this computer and needs no sign-in", server.name)).into());
            }
            env.db.set_mcp_auth(&server.id, "oauth")?;
            crate::oauth::sign_out(&env.secrets, &server.id);
            connect_mcp(env, &server.id, &server.name, false)
        }
        McpAction::Logout { name } => {
            let server = find_mcp(env, &name)?;
            crate::oauth::sign_out(&env.secrets, &server.id);
            let _ = env.db.mark_mcp_disconnected(&server.id);
            ok(format!("Signed out of {} · sign in again: {}", server.name, link(&format!("aihat mcp login {}", server.name))));
            Ok(0)
        }
        McpAction::Start { name } | McpAction::Refresh { name } => {
            let server = find_mcp(env, &name)?;
            connect_mcp(env, &server.id, &server.name, false)
        }
        McpAction::SetKey { name } => {
            let server = find_mcp(env, &name)?;
            let token = read_secret("Access token", crate::secrets::where_keys_go(true))?;
            if token.is_empty() {
                return Err(Failure::new(EXIT_USAGE, "The token is empty; nothing was saved").into());
            }
            env.secrets.set_mcp_token(&server.id, &token)?;
            env.db.set_mcp_auth(&server.id, "token")?;
            match crate::mcp::refresh(&env.db, &env.secrets, &server.id) {
                Ok(count) => ok(format!("Token saved · {} ready", text::plural(count, "tool", "tools"))),
                Err(error) => println!("{} Token saved, but: {}", paint("!", t.fg(Role::Warn)), crate::tui::describe_error(&error)),
            }
            Ok(0)
        }
        McpAction::Allow { name } => {
            let mut server = find_mcp(env, &name)?;
            crate::mcp::set_server_always_allowed(&env.db, &mut server, true)?;
            ok(format!("{} runs its tools without asking", server.name));
            Ok(0)
        }
        McpAction::Ask { name } => {
            let mut server = find_mcp(env, &name)?;
            crate::mcp::set_server_always_allowed(&env.db, &mut server, false)?;
            ok(format!("{} asks before running tools that write", server.name));
            Ok(0)
        }
        McpAction::Remove { name } => {
            let server = find_mcp(env, &name)?;
            if !env.yes && !prompt::confirm(&format!("Remove {}?", server.name))? {
                return Ok(0);
            }
            crate::mcp::forget(&env.secrets, &server.id);
            env.db.remove_mcp_server(&server.id)?;
            ok("Removed");
            Ok(0)
        }
    }
}

// ------------------------------------------------------------------ skills

#[derive(Subcommand, Debug)]
pub enum SkillAction {
    /// List skills
    List,
    /// Add a skill: a folder with SKILL.md, a GitHub link, or a name from `aihat skills search`
    Add {
        source: String,
        /// Install it for every agent on this computer (~/.agents/skills), not only AI Hat
        #[arg(long)]
        global: bool,
    },
    /// Find skills to add (popular ones without a query)
    Search {
        query: Vec<String>,
        /// Where to look: popular (claude-plugins.dev) or skills.sh
        #[arg(long, default_value = "popular")]
        from: String,
    },
    /// Read skills again from their folder or GitHub (all, or one)
    Update { name: Option<String> },
    /// Turn a skill on
    Enable { name: String },
    /// Turn a skill off
    Disable { name: String },
    /// Print a skill's instructions (also works with a GitHub link)
    Show { name: String },
    /// Remove a skill
    Remove { name: String },
}

/// Turn a skill on or off: AI Hat's own (in the database), or a global one
/// (remembered in settings.json).
fn set_skill_on(env: &mut Env, name: &str, on: bool) -> Result<i32> {
    if let Ok(skill) = find_skill(env, name) {
        env.db.set_skill_enabled(&skill.id, on)?;
        ok(format!("{} is {}", skill.name, if on { "on" } else { "off" }));
        return Ok(0);
    }
    let Some(global) = crate::skills::globals().into_iter().find(|g| g.name.eq_ignore_ascii_case(name)) else {
        return Err(Failure::new(EXIT_ERROR, format!("No skill named \"{name}\"")).note("List them: aihat skills").into());
    };
    let off = &mut env.config.skills.off;
    off.retain(|n| !n.eq_ignore_ascii_case(&global.name));
    if !on {
        off.push(global.name.clone());
    }
    env.config.save(&env.paths)?;
    ok(format!("{} is {} {}", global.name, if on { "on" } else { "off" }, dim(&format!("· global skill in {}", global.place))));
    Ok(0)
}

fn find_skill(env: &Env, name: &str) -> Result<crate::models::Skill> {
    env.db
        .skills()?
        .into_iter()
        .find(|s| s.id == name || s.name.eq_ignore_ascii_case(name))
        .ok_or_else(|| Failure::new(EXIT_ERROR, format!("No skill named \"{name}\"")).note("List them: aihat skills").into())
}

pub fn skills(env: &mut Env, action: Option<SkillAction>) -> Result<i32> {
    let t = theme();
    match action.unwrap_or(SkillAction::List) {
        SkillAction::List => {
            let skills = env.db.skills()?;
            let globals: Vec<_> = crate::skills::globals()
                .into_iter()
                .filter(|g| !skills.iter().any(|s| s.name.eq_ignore_ascii_case(&g.name)))
                .collect();
            if skills.is_empty() && globals.is_empty() {
                println!("No skills yet. Find some: {} · or add one: {}", link("aihat skills search"), link("aihat skills add ./my-skill"));
                return Ok(0);
            }
            let row = |on: bool, name: &str, description: &str, tail: &str| {
                let description = text::truncate(&description.replace('\n', " "), 60);
                if on {
                    println!("  {} {}{}{}", paint("✓", t.fg(Role::Accent)), strong(&pad(name, 18)), dim(&description), dim(tail));
                } else {
                    println!("  {}", dim(&format!("○ {}{description}{tail}", pad(name, 18))));
                }
            };
            for skill in &skills {
                row(skill.enabled, &skill.name, &skill.description, "");
            }
            if !globals.is_empty() {
                if !skills.is_empty() {
                    println!();
                }
                println!("  {}", dim("Global skills · from the agents on this computer"));
                for g in &globals {
                    row(!crate::skills::global_is_off(&g.name), &g.name, &g.description, &format!("  {}", g.place));
                }
            }
            Ok(0)
        }
        SkillAction::Search { query, from } => {
            let query = query.join(" ");
            let spinner = super::ask::Spinner::new(std::io::stderr().is_terminal());
            spinner.set("Looking…");
            let found = match from.as_str() {
                "skills.sh" | "skillssh" => crate::market::skills_sh(&query),
                _ => crate::market::popular_skills(&query, 0, 20).map(|(items, _)| items),
            };
            spinner.stop();
            let found = found.map_err(|e| Failure::new(EXIT_UNREACHABLE, e.to_string()))?;
            if found.is_empty() {
                println!("Nothing found{}.", if query.is_empty() { String::new() } else { format!(" for \"{query}\"") });
                return Ok(0);
            }
            let installed = env.db.skills()?;
            for item in &found {
                let added = crate::market::skill_added(&installed, item);
                let meta = crate::market::skill_meta(item);
                println!(
                    "  {}{}{}",
                    strong(&pad(&text::truncate(&item.name, 20), 22)),
                    dim(&pad(&meta, 46)),
                    if added { paint("added", t.fg(Role::Accent)) } else { String::new() }
                );
                if let Some(description) = &item.description {
                    println!("  {}", dim(&text::truncate(&format!("  {}", description.replace('\n', " ")), terminal_width().saturating_sub(4).max(40))));
                }
            }
            println!();
            println!("{} {}", dim("Add one:"), link(&format!("aihat skills add {}", crate::market::skill_ref(&found[0]))));
            Ok(0)
        }
        SkillAction::Add { source, global } => {
            let spinner = super::ask::Spinner::new(std::io::stderr().is_terminal() && !std::path::Path::new(&source).exists());
            spinner.set("Reading the skill…");
            let loaded = crate::market::load_skill_ref(&source);
            spinner.stop();
            let loaded = loaded.map_err(|e| Failure::new(EXIT_ERROR, format!("{e:#}")))?;
            if global {
                let dir = crate::skills::install_global(&loaded).map_err(|e| Failure::new(EXIT_ERROR, format!("{e:#}")))?;
                ok(format!("Added {} for every agent on this computer {}", loaded.0, dim(&format!("· {}", text::home_relative(&dir)))));
                return Ok(0);
            }
            let (name, description, body, source, files) = loaded;
            if env.db.skills()?.iter().any(|s| s.name.eq_ignore_ascii_case(&name)) {
                return Err(Failure::new(EXIT_USAGE, format!("There is already a skill named {name}")).note(format!("Update it instead: aihat skills update {name}")).into());
            }
            let id = env.db.save_skill_with_files(&name, &description, &body, &source, &files)?;
            let github = crate::skills::is_github(&source);
            if github {
                env.db.set_skill_auto_update(&id, true)?;
            }
            let extra = if files.is_empty() { String::new() } else { format!(" · {}", text::plural(files.len(), "file", "files")) };
            ok(format!("Added {name}{extra}{}", if github { dim(" · from GitHub") } else { String::new() }));
            Ok(0)
        }
        SkillAction::Update { name } => {
            let skills = match &name {
                Some(name) => vec![find_skill(env, name)?],
                None => env.db.skills()?.into_iter().filter(|s| s.source != "local" && !s.source.is_empty()).collect(),
            };
            if skills.is_empty() {
                println!("No skills come from a folder or GitHub, so there is nothing to update.");
                return Ok(0);
            }
            let mut failed = 0;
            for skill in skills {
                match crate::skills::reload(&skill.source) {
                    Ok((new_name, description, body, source, files)) => {
                        let changed = body != skill.body || description != skill.description;
                        env.db.update_skill_source_with_files(&skill.id, &new_name, &description, &body, &source, &files)?;
                        ok(format!("{new_name} {}", dim(if changed { "· updated" } else { "· already up to date" })));
                    }
                    Err(error) => {
                        failed += 1;
                        println!("{} {}: {}", paint("✗", t.fg(Role::Error)), skill.name, format!("{error:#}"));
                    }
                }
            }
            Ok(if failed > 0 { EXIT_ERROR } else { 0 })
        }
        SkillAction::Enable { name } => set_skill_on(env, &name, true),
        SkillAction::Disable { name } => set_skill_on(env, &name, false),
        SkillAction::Show { name } => {
            let skill = match find_skill(env, &name) {
                Ok(skill) => skill,
                Err(error) => match crate::skills::parse_github(&name) {
                    // A global skill, read from its folder.
                    _ if crate::skills::globals().iter().any(|g| g.name.eq_ignore_ascii_case(&name)) => {
                        let g = crate::skills::globals().into_iter().find(|g| g.name.eq_ignore_ascii_case(&name)).expect("checked");
                        crate::models::Skill {
                            id: String::new(),
                            name: g.name,
                            description: g.description,
                            version: String::new(),
                            source: text::home_relative(&g.dir),
                            body: g.body,
                            mode: "auto".into(),
                            enabled: true,
                            can_run_scripts: false,
                            auto_update: false,
                        }
                    }
                    // Not installed: read it from GitHub without adding it.
                    Some(found) if name.contains('/') => {
                        let (skill_name, description, body, source, _) = crate::skills::fetch_github(&found, false)?;
                        crate::models::Skill {
                            id: String::new(),
                            name: skill_name,
                            description,
                            version: String::new(),
                            source,
                            body,
                            mode: "auto".into(),
                            enabled: false,
                            can_run_scripts: false,
                            auto_update: false,
                        }
                    }
                    _ => return Err(error),
                },
            };
            println!("{}", paint(&skill.name, t.bold(Role::Strong)));
            println!("{}", dim(&skill.description));
            if !skill.source.is_empty() && skill.source != "local" {
                println!("{}", dim(&format!("from {}", skill.source)));
            }
            println!();
            let rows = crate::ui::markdown::render(&skill.body, crate::ui::markdown::MdOptions { width: 78, indent: 0, block_indent: 2, wide: terminal_width() });
            print!("{}", crate::ui::ansi::lines(&rows));
            Ok(0)
        }
        SkillAction::Remove { name } => {
            let skill = find_skill(env, &name)?;
            if !env.yes && !prompt::confirm(&format!("Remove the skill {}?", skill.name))? {
                return Ok(0);
            }
            env.db.remove_skill(&skill.id)?;
            ok("Removed");
            Ok(0)
        }
    }
}

// ------------------------------------------------------------------ config, memory, backup, keys, image

#[derive(Subcommand, Debug)]
pub enum ConfigAction {
    /// Where the files are
    Path,
    /// Print one option, or all of them
    Get { key: Option<String> },
    /// Change an option, like keys.switch_model f2
    Set { key: String, value: String },
    /// Open settings.json in your editor
    Edit,
}

pub fn config(env: &mut Env, action: Option<ConfigAction>) -> Result<i32> {
    match action.unwrap_or(ConfigAction::Path) {
        ConfigAction::Path => {
            println!("{}  {}", dim(&pad("Settings", 9)), env.paths.config_file.display());
            println!("{}  {}", dim(&pad("Database", 9)), env.paths.db_file.display());
            println!("{}  {}", dim(&pad("Data", 9)), env.paths.data_dir.display());
            Ok(0)
        }
        ConfigAction::Get { key } => {
            let value = serde_json::to_value(&env.config)?;
            match key {
                Some(key) => {
                    let key = crate::config::canonical_key(&key);
                    match lookup(&value, &key) {
                        Some(Value::String(text)) => println!("{text}"),
                        Some(found @ (Value::Object(_) | Value::Array(_))) => println!("{}", serde_json::to_string_pretty(found)?),
                        Some(found) => println!("{found}"),
                        None => return Err(unknown_option(&key).into()),
                    }
                }
                None => println!("{}", serde_json::to_string_pretty(&value)?),
            }
            Ok(0)
        }
        ConfigAction::Set { key, value } => {
            let key = crate::config::canonical_key(&key);
            let mut settings = serde_json::to_value(&env.config)?;
            let Some(slot) = lookup_mut(&mut settings, &key) else {
                return Err(unknown_option(&key).into());
            };
            *slot = match &*slot {
                Value::Bool(_) => Value::Bool(match value.to_lowercase().as_str() {
                    "true" | "on" | "yes" | "1" => true,
                    "false" | "off" | "no" | "0" => false,
                    _ => return Err(Failure::new(EXIT_USAGE, format!("{key} is on or off, not \"{value}\"")).into()),
                }),
                Value::Number(_) => Value::Number(
                    value
                        .parse::<u64>()
                        .map_err(|_| Failure::new(EXIT_USAGE, format!("{key} is a number, not \"{value}\"")))?
                        .into(),
                ),
                Value::Object(_) => return Err(Failure::new(EXIT_USAGE, format!("{key} is a group; set one of its options, like {key}.{}", first_child(slot))).into()),
                _ if key.starts_with("keys.") => {
                    let keys: Vec<String> = value.split(',').map(|key| key.trim().to_string()).filter(|key| !key.is_empty()).collect();
                    for one in &keys {
                        crate::tui::parse_key(one).map_err(|error| Failure::new(EXIT_USAGE, error))?;
                    }
                    match keys.as_slice() {
                        [one] => Value::String(one.clone()),
                        _ => json!(keys),
                    }
                }
                _ => Value::String(value.clone()),
            };
            let next: crate::config::AppConfig = serde_json::from_value(settings)?;
            if let Some(problem) = check_setting(&next, &key) {
                return Err(Failure::new(EXIT_USAGE, problem).into());
            }
            next.save(&env.paths)?;
            ok(format!("{key} = {value}"));
            Ok(0)
        }
        ConfigAction::Edit => {
            let editor = std::env::var("VISUAL")
                .or_else(|_| std::env::var("EDITOR"))
                .unwrap_or_else(|_| if cfg!(windows) { "notepad".into() } else { "nano".into() });
            let mut parts = editor.split_whitespace();
            let program = parts.next().unwrap_or("nano");
            let status = std::process::Command::new(program)
                .args(parts)
                .arg(&env.paths.config_file)
                .status()
                .map_err(|error| Failure::new(EXIT_ERROR, format!("Could not open {program}: {error}")).note("Set EDITOR, or edit the file at: aihat config path"))?;
            if !status.success() {
                return Ok(status.code().unwrap_or(1));
            }
            match crate::config::AppConfig::load(&env.paths) {
                Ok(_) => ok("Saved settings.json"),
                Err(error) => return Err(Failure::new(EXIT_ERROR, error.to_string()).note("Fix it with: aihat config edit").into()),
            }
            Ok(0)
        }
    }
}

fn lookup<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    key.split('.').try_fold(value, |node, part| node.get(part))
}

fn lookup_mut<'a>(value: &'a mut Value, key: &str) -> Option<&'a mut Value> {
    key.split('.').try_fold(value, |node, part| node.get_mut(part))
}

fn first_child(value: &Value) -> String {
    value.as_object().and_then(|map| map.keys().next().cloned()).unwrap_or_default()
}

fn unknown_option(key: &str) -> Failure {
    Failure::new(EXIT_USAGE, format!("Unknown option \"{key}\"")).note("See them all: aihat config get")
}

/// Values with a fixed set of choices.
fn check_setting(config: &crate::config::AppConfig, key: &str) -> Option<String> {
    let choose = |value: &str, allowed: &[&str]| {
        (!allowed.contains(&value)).then(|| format!("{key} is one of {}, not \"{value}\"", allowed.join(", ")))
    };
    match key {
        "colors" => choose(&config.colors, &["auto", "truecolor", "256", "16", "none"]),
        "images.preview" => choose(&config.images.preview, &["auto", "kitty", "sixel", "iterm2", "blocks", "off"]),
        "search.engine" => choose(&config.search.engine, &["provider", "brave", "tavily", "searxng"]),
        "accent" => {
            let hex = config.accent.trim_start_matches('#');
            (hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit())).then(|| format!("accent is a colour like \"#C6F46B\", not \"{}\"", config.accent))
        }
        _ => None,
    }
}

#[derive(Subcommand, Debug)]
pub enum MemoryAction {
    /// List saved memories
    List,
    /// Save a memory by hand
    Add {
        text: String,
        /// A short label (made from the text when left out)
        #[arg(long)]
        title: Option<String>,
    },
    /// Forget one memory: its number, or its title
    Forget { which: String },
    /// Turn memory on
    On,
    /// Turn memory off
    Off,
    /// Forget everything
    Clear,
}

pub fn memory(env: &mut Env, action: Option<MemoryAction>) -> Result<i32> {
    let t = theme();
    let mut settings = env.db.settings();
    if !settings.is_object() {
        settings = json!({});
    }
    let action = action.unwrap_or(MemoryAction::List);
    match action {
        MemoryAction::List => {
            let enabled = settings.get("memoryEnabled").and_then(Value::as_bool).unwrap_or(true);
            let items = crate::memory::list(&env.db);
            println!(
                "{} {}",
                if enabled { paint("✓ Memory is on", t.fg(Role::Accent)) } else { paint("○ Memory is off", t.fg(Role::Dim)) },
                dim(&format!("· {} saved · shared by every chat", items.len()))
            );
            let title_width = items.iter().map(|(_, title, _)| width(title)).max().unwrap_or(0).min(28);
            for (index, (_, title, body)) in items.iter().enumerate() {
                let room = terminal_width().saturating_sub(title_width + 10).max(20);
                println!(
                    "  {}  {}  {}",
                    dim(&pad_left(&(index + 1).to_string(), 2)),
                    strong(&pad(&text::truncate(title, title_width), title_width)),
                    dim(&text::truncate(body, room))
                );
            }
            if items.is_empty() {
                println!("  {}", dim("Nothing yet. Models save what's worth keeping as you chat; or: aihat memory add \"I work in Rust\""));
            }
        }
        MemoryAction::Add { text: body, title } => {
            let title = crate::memory::add(&env.db, title.as_deref().unwrap_or(""), &body).map_err(|e| Failure::new(EXIT_USAGE, e.to_string()))?;
            ok(format!("Saved to memory: {title}"));
        }
        MemoryAction::Forget { which } => {
            let title = crate::memory::forget(&env.db, &which).map_err(|e| Failure::new(EXIT_USAGE, e.to_string()).note("See them: aihat memory"))?;
            ok(format!("Forgot {title}"));
        }
        MemoryAction::On | MemoryAction::Off => {
            let on = matches!(action, MemoryAction::On);
            settings["memoryEnabled"] = json!(on);
            env.db.set_kv("settings", &settings.to_string())?;
            ok(if on { "Memory is on" } else { "Memory is off" });
        }
        MemoryAction::Clear => {
            if !env.yes && !prompt::confirm("Forget every saved memory?")? {
                return Ok(0);
            }
            crate::memory::clear(&env.db)?;
            ok("Forgot every saved memory");
        }
    }
    Ok(0)
}

#[derive(Subcommand, Debug)]
pub enum BackupAction {
    /// Save everything to a JSON file (keys are left out)
    Export {
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Restore from a JSON backup
    Import { input: PathBuf },
}

pub fn backup(env: &mut Env, action: Option<BackupAction>) -> Result<i32> {
    match action.unwrap_or(BackupAction::Export { output: None }) {
        BackupAction::Export { output } => {
            let path = output.unwrap_or_else(|| {
                env.paths.downloads_dir.join(format!("aihat-backup-{}.json", chrono::Local::now().format("%Y%m%d-%H%M%S")))
            });
            crate::backup::export_to(&env.db, &path)?;
            ok(format!("Backed up to {}", text::home_relative(&path)));
        }
        BackupAction::Import { input } => {
            if !env.yes && !prompt::confirm(&format!("Restore from {}? It replaces what is here now.", input.display()))? {
                return Ok(0);
            }
            let tables = crate::backup::import_from(&mut env.db, &input)?;
            ok(format!("Restored {} from {}", text::plural(tables, "table", "tables"), input.display()));
        }
    }
    Ok(0)
}

#[derive(Subcommand, Debug)]
pub enum SearchKeyAction {
    /// Save a Brave or Tavily key
    Set { engine: String },
    /// Remove a saved key
    Remove { engine: String },
}

pub fn search_key(env: &mut Env, action: Option<SearchKeyAction>) -> Result<i32> {
    let Some(action) = action else {
        return Err(Failure::new(EXIT_USAGE, "Say which: aihat search-key set brave").into());
    };
    let (set, engine) = match action {
        SearchKeyAction::Set { engine } => (true, engine.to_lowercase()),
        SearchKeyAction::Remove { engine } => (false, engine.to_lowercase()),
    };
    let (key_name, env_name) = match engine.as_str() {
        "brave" => ("search.brave", "BRAVE_SEARCH_API_KEY"),
        "tavily" => ("search.tavily", "TAVILY_API_KEY"),
        _ => return Err(Failure::new(EXIT_USAGE, "The search engine is brave or tavily").into()),
    };
    if set {
        let key = if std::io::stdin().is_terminal() {
            prompt::input(&format!("{engine} API key"), crate::secrets::where_keys_go(true), "", true)?
        } else {
            std::env::var(env_name).map_err(|_| Failure::new(EXIT_USAGE, format!("Set ${env_name} or run this in a terminal")))?
        };
        if key.trim().is_empty() {
            return Err(Failure::new(EXIT_USAGE, "The key is empty; nothing was saved").into());
        }
        env.secrets.set(key_name, key.trim())?;
        ok(format!("{engine} key saved · turn it on: aihat config set search_engine {engine}"));
    } else {
        env.secrets.delete(key_name)?;
        ok(format!("Removed the {engine} key"));
    }
    Ok(0)
}

pub fn image(env: &mut Env, prompt_text: &str, provider: Option<&str>, model: Option<&str>, count: Option<usize>, output: Option<&str>) -> Result<i32> {
    use crate::ui::picture::{self, Picture};
    if prompt_text.trim().is_empty() {
        return Err(Failure::new(EXIT_USAGE, "Describe the picture: aihat image \"a lighthouse in a storm\"").into());
    }
    let t = theme();
    let spinner = super::ask::Spinner::new(std::io::stderr().is_terminal());
    spinner.set("Drawing…");
    let started = std::time::Instant::now();
    let images = crate::imagegen::generate(&env.db, &env.secrets, prompt_text, provider, model, count)?;
    let ms = started.elapsed().as_millis() as u64;
    let pictures = images
        .iter()
        .map(|image| crate::imagegen::picture_bytes(image).and_then(Picture::decode))
        .collect::<Result<Vec<_>>>()?;
    spinner.stop();
    if pictures.is_empty() {
        return Err(Failure::new(EXIT_ERROR, "The image service returned no pictures").into());
    }
    let label = crate::imagegen::engine_label(&env.db);
    let mut about = text::plural(pictures.len(), "picture", "pictures").replacen("1 picture", "a picture", 1);
    if !label.is_empty() {
        about.push_str(&format!(" · {label}"));
    }
    eprintln!("{} Drew {about} · {}", paint("✓", t.fg(Role::Accent)), dim(&text::duration(ms)));

    let folder = env.config.image_folder(&env.paths);
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let ask = output.is_none() && !env.yes && prompt::interactive() && std::io::stdout().is_terminal();
    let save = |picture: &Picture, path: &std::path::Path| -> Result<()> {
        if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).with_context(|| format!("making {}", text::home_relative(parent)))?;
        }
        std::fs::write(path, &picture.bytes).with_context(|| format!("saving {}", text::home_relative(path)))?;
        if std::io::stdout().is_terminal() {
            ok(format!("Saved {}", text::home_relative(path)));
        } else {
            // Scripts get the path.
            println!("{}", path.display());
        }
        Ok(())
    };
    let total = pictures.len();
    for (index, picture) in pictures.iter().enumerate() {
        let suffix = if total > 1 { format!("-{}", index + 1) } else { String::new() };
        let name = format!("aihat-{stamp}{suffix}.{}", picture.extension());
        if !ask {
            let path = match output {
                Some(target) => {
                    let path = picture::save_as_target(target, &name, picture.extension());
                    // Several pictures and one file name: number them.
                    match (total > 1, path.file_stem(), path.extension()) {
                        (true, Some(stem), Some(extension)) if path.file_name() != Some(std::ffi::OsStr::new(&name)) => path.with_file_name(format!(
                            "{}{suffix}.{}",
                            stem.to_string_lossy(),
                            extension.to_string_lossy()
                        )),
                        _ => path,
                    }
                }
                None => picture::unique_path(&folder, &name),
            };
            save(picture, &path)?;
            continue;
        }
        let max_cols = (terminal_width().saturating_sub(4)).min(36) as u16;
        println!();
        let shown = picture::print(picture, &env.config.images.preview, 2, max_cols, 16);
        println!("  {}", dim(&picture.describe()));
        if !shown {
            println!("  {}", dim("No preview in this terminal · images.preview in settings.json picks how"));
        }
        println!();
        let title = if total > 1 { format!("Save picture {} of {total}?", index + 1) } else { "Save it?".to_string() };
        let save_here = format!("Save to {}", text::home_relative(&folder));
        match prompt::choose(&title, &[&save_here, "Save as…", "Discard"], 0)? {
            0 => save(picture, &picture::unique_path(&folder, &name))?,
            1 => {
                let mut initial = text::home_relative(&folder.join(&name));
                loop {
                    let typed = prompt::input("Save as", "a file name or a folder", &initial, false)?;
                    let path = picture::save_as_target(&typed, &name, picture.extension());
                    if path.exists() {
                        eprintln!("{} {} already exists · choose another name", paint("!", t.fg(Role::Warn)), text::home_relative(&path));
                        initial = typed;
                        continue;
                    }
                    save(picture, &path)?;
                    break;
                }
            }
            _ => eprintln!("{}", dim("○ Not saved")),
        }
    }
    Ok(0)
}
