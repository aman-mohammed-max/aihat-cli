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
            let key = read_secret("API key", "saved in your system keychain")?;
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
            "rejected" => ("✗ key rejected".to_string(), Role::Error),
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
            for model in &wanted_models {
                if env.db.model(&id, model)?.is_none() {
                    crate::providers::add_model(&env.db, &id, model, "chat", None)?;
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
        None => prompt::input("API key", "saved in your system keychain · leave empty if none", "", true)?,
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
            crate::providers::add_model(&env.db, &provider.id, &model, &kind, name.as_deref())?;
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
            env.db.conn.execute("DELETE FROM prompts WHERE id=?1", [&prompt.id])?;
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
    if let Ok(number) = trimmed.parse::<usize>() {
        if let Some(chat) = db.chat_by_number(number)? {
            return Ok(chat);
        }
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
                        let rows = crate::ui::markdown::render(&answer, crate::ui::markdown::MdOptions { width, indent: 0, block_indent: 2 });
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
    /// Add a remote (HTTP) MCP server
    Add {
        name: String,
        url: Option<String>,
        /// Access token, read from this environment variable
        #[arg(long = "token-env", value_name = "VAR")]
        token_env: Option<String>,
        #[arg(last = true)]
        command: Vec<String>,
    },
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

pub fn mcp(env: &mut Env, action: Option<McpAction>) -> Result<i32> {
    let t = theme();
    match action.unwrap_or(McpAction::List) {
        McpAction::List => {
            let servers = env.db.mcp_servers()?;
            if servers.is_empty() {
                println!("No MCP servers yet. Add one: {}", link("aihat mcp add NAME https://…/mcp"));
                return Ok(0);
            }
            println!("{}", dim(&format!("  {}{}{}  STATUS", pad("NAME", 12), pad("TRANSPORT", 9), pad_left("TOOLS", 5))));
            for server in servers {
                let allowed = crate::mcp::server_always_allowed(&env.db, &server.id);
                let status = if server.connected || !server.tools.is_empty() {
                    paint(&format!("✓ ready{}", if allowed { " · runs without asking" } else { "" }), t.fg(Role::Accent))
                } else {
                    format!("{} {}", paint("! not connected ·", t.fg(Role::Warn)), link(&format!("aihat mcp refresh {}", server.name)))
                };
                println!("  {}{}{}  {status}", strong(&pad(&server.name, 12)), pad(&server.transport, 9), pad_left(&server.tools.len().to_string(), 5));
            }
            Ok(0)
        }
        McpAction::Add { name, url, token_env, command } => {
            if !command.is_empty() || url.is_none() {
                return Err(Failure::new(EXIT_USAGE, "Only remote (HTTP) MCP servers are supported in the terminal for now")
                    .note("Give its URL: aihat mcp add NAME https://…/mcp")
                    .into());
            }
            let url = url.unwrap_or_default();
            let id = env.db.add_mcp_server(&name, &url)?;
            if let Some(var) = token_env {
                let token = std::env::var(&var).map_err(|_| Failure::new(EXIT_USAGE, format!("${var} is not set")))?;
                env.secrets.set_mcp_token(&id, token.trim())?;
            }
            match crate::mcp::refresh(&env.db, &env.secrets, &id) {
                Ok(count) => {
                    println!(
                        "{} Added {name} {} · {} · {}",
                        paint("✓", t.fg(Role::Accent)),
                        dim("(http)"),
                        text::plural(count, "tool", "tools"),
                        dim("ask before running: on")
                    );
                    Ok(0)
                }
                Err(error) => {
                    let auth = crate::api::api_error(&error).is_some_and(|e| e.kind == crate::api::ErrorKind::Auth);
                    println!("{} Added {name}, but it did not connect: {}", paint("!", t.fg(Role::Warn)), crate::tui::describe_error(&error));
                    if auth {
                        println!("  {} {}", dim("Save its token:"), link(&format!("aihat mcp set-key {name}")));
                    }
                    Ok(if auth { EXIT_AUTH } else { EXIT_UNREACHABLE })
                }
            }
        }
        McpAction::Refresh { name } => {
            let server = find_mcp(env, &name)?;
            let count = crate::mcp::refresh(&env.db, &env.secrets, &server.id)?;
            ok(format!("{} · {}", server.name, text::plural(count, "tool", "tools")));
            Ok(0)
        }
        McpAction::SetKey { name } => {
            let server = find_mcp(env, &name)?;
            let token = read_secret("Access token", "saved in your system keychain")?;
            if token.is_empty() {
                return Err(Failure::new(EXIT_USAGE, "The token is empty; nothing was saved").into());
            }
            env.secrets.set_mcp_token(&server.id, &token)?;
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
            let _ = env.secrets.delete(&format!("mcp.{}", server.id));
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
    /// Add a skill folder (with SKILL.md) or a SKILL.md file
    Add { path: PathBuf },
    /// Turn a skill on
    Enable { name: String },
    /// Turn a skill off
    Disable { name: String },
    /// Print a skill's instructions
    Show { name: String },
    /// Remove a skill
    Remove { name: String },
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
            if skills.is_empty() {
                println!("No skills yet. Add one: {}", link("aihat skills add ./my-skill"));
                return Ok(0);
            }
            for skill in skills {
                let description = text::truncate(&skill.description.replace('\n', " "), 60);
                if skill.enabled {
                    println!("  {} {}{}", paint("✓", t.fg(Role::Accent)), strong(&pad(&skill.name, 18)), dim(&description));
                } else {
                    println!("  {}", dim(&format!("○ {}{description}", pad(&skill.name, 18))));
                }
            }
            Ok(0)
        }
        SkillAction::Add { path } => {
            let (name, description, body, source, files) = crate::skills::read_skill_source(&path)?;
            env.db.save_skill_with_files(&name, &description, &body, &source, &files)?;
            ok(format!("Added {name}"));
            Ok(0)
        }
        SkillAction::Enable { name } => {
            let skill = find_skill(env, &name)?;
            env.db.set_skill_enabled(&skill.id, true)?;
            ok(format!("{} is on", skill.name));
            Ok(0)
        }
        SkillAction::Disable { name } => {
            let skill = find_skill(env, &name)?;
            env.db.set_skill_enabled(&skill.id, false)?;
            ok(format!("{} is off", skill.name));
            Ok(0)
        }
        SkillAction::Show { name } => {
            let skill = find_skill(env, &name)?;
            println!("{}", paint(&skill.name, t.bold(Role::Strong)));
            println!("{}", dim(&skill.description));
            println!();
            let rows = crate::ui::markdown::render(&skill.body, crate::ui::markdown::MdOptions { width: 78, indent: 0, block_indent: 2 });
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
    /// Change an option
    Set { key: String, value: String },
}

pub fn config(env: &mut Env, action: Option<ConfigAction>) -> Result<i32> {
    match action.unwrap_or(ConfigAction::Path) {
        ConfigAction::Path => {
            println!("{}  {}", dim(&pad("Config", 9)), env.paths.config_file.display());
            println!("{}  {}", dim(&pad("Database", 9)), env.paths.db_file.display());
            println!("{}  {}", dim(&pad("Data", 9)), env.paths.data_dir.display());
            Ok(0)
        }
        ConfigAction::Get { key } => {
            let value = toml::Value::try_from(&env.config)?;
            match key {
                Some(key) => match value.get(&key) {
                    Some(found) => println!("{}", found.to_string().trim_matches('"')),
                    None => return Err(Failure::new(EXIT_USAGE, format!("Unknown option \"{key}\"")).note("See them all: aihat config get").into()),
                },
                None => print!("{}", toml::to_string_pretty(&env.config)?),
            }
            Ok(0)
        }
        ConfigAction::Set { key, value } => {
            let mut next = env.config.clone();
            let flag = |value: &str| -> Result<bool> {
                match value.to_lowercase().as_str() {
                    "true" | "on" | "yes" | "1" => Ok(true),
                    "false" | "off" | "no" | "0" => Ok(false),
                    _ => Err(Failure::new(EXIT_USAGE, format!("\"{value}\" is not on or off")).into()),
                }
            };
            match key.as_str() {
                "theme" => next.theme = value.clone(),
                "accent" => next.accent = value.clone(),
                "no_color" => next.no_color = flag(&value)?,
                "ascii" => next.ascii = flag(&value)?,
                "show_stats" => next.show_stats = flag(&value)?,
                "show_reasoning" => next.show_reasoning = flag(&value)?,
                "search_url" => next.search_url = value.clone(),
                "search_results" => next.search_results = value.parse().map_err(|_| Failure::new(EXIT_USAGE, "search_results is a number"))?,
                "search_engine" => next.search_engine = value.clone(),
                _ => return Err(Failure::new(EXIT_USAGE, format!("Unknown option \"{key}\"")).note("See them all: aihat config get").into()),
            }
            next.save(&env.paths)?;
            ok(format!("{key} = {value}"));
            Ok(0)
        }
    }
}

#[derive(Subcommand, Debug)]
pub enum MemoryAction {
    /// List saved memories
    List,
    /// Turn memory on for new chats
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
    match action.unwrap_or(MemoryAction::List) {
        MemoryAction::List => {
            let enabled = settings.get("memoryEnabled").and_then(Value::as_bool).unwrap_or(true);
            let items = settings.get("memoryItems").and_then(Value::as_array).cloned().unwrap_or_default();
            println!(
                "{} {}",
                if enabled { paint("✓ Memory is on", t.fg(Role::Accent)) } else { paint("○ Memory is off", t.fg(Role::Dim)) },
                dim(&format!("· {} saved", items.len()))
            );
            for item in items {
                let title = item.get("title").and_then(Value::as_str).unwrap_or("Untitled");
                let body = item.get("text").and_then(Value::as_str).unwrap_or("");
                println!("  {}  {}", strong(title), dim(&text::truncate(body, 70)));
            }
        }
        MemoryAction::On | MemoryAction::Off => {
            let on = matches!(action, Some(MemoryAction::On));
            settings["memoryEnabled"] = json!(on);
            env.db.set_kv("settings", &settings.to_string())?;
            ok(if on { "Memory is on" } else { "Memory is off" });
        }
        MemoryAction::Clear => {
            if !env.yes && !prompt::confirm("Forget every saved memory?")? {
                return Ok(0);
            }
            settings["memoryItems"] = json!([]);
            let revision = settings.get("memoryRevision").and_then(Value::as_u64).unwrap_or(0);
            settings["memoryRevision"] = json!(revision + 1);
            env.db.set_kv("settings", &settings.to_string())?;
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
            prompt::input(&format!("{engine} API key"), "saved in your system keychain", "", true)?
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

pub fn image(env: &mut Env, prompt_text: &str, provider: Option<&str>, model: Option<&str>, count: Option<usize>) -> Result<i32> {
    if prompt_text.trim().is_empty() {
        return Err(Failure::new(EXIT_USAGE, "Describe the picture: aihat image \"a lighthouse in a storm\"").into());
    }
    let images = crate::imagegen::generate(&env.db, &env.secrets, prompt_text, provider, model, count)?;
    if images.is_empty() {
        return Err(Failure::new(EXIT_ERROR, "The image service returned no pictures").into());
    }
    std::fs::create_dir_all(&env.paths.downloads_dir)?;
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    for (index, image) in images.iter().enumerate() {
        if let Some(data) = image.get("b64_json").and_then(Value::as_str) {
            let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, data.trim())?;
            let path = env.paths.downloads_dir.join(format!("aihat-{stamp}-{}.png", index + 1));
            std::fs::write(&path, bytes)?;
            ok(format!("Saved {}", text::home_relative(&path)));
        } else if let Some(url) = image.get("url").and_then(Value::as_str) {
            println!("{url}");
        }
    }
    Ok(0)
}
