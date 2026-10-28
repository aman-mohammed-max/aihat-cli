# AI Hat CLI

AI Hat CLI is the Rust terminal client for the AI Hat app. It follows the terminal mockups in AI Hat.pdf, AI Hat.html, and the PNG exports: one-shot prompts work in scripts, while running aihat opens a full-screen terminal chat.

The client uses the same OpenAI-compatible endpoints, provider/model records, prompt library, chats, SQLite schema and key names as the Expo app. The default desktop data folder matches AI Hat's desktop app folder. AIHAT_DATA_DIR and AIHAT_DB_PATH let you point it at another copy of the app database.

The Rust port covers the terminal workflows: chat, provider and model setup, saved prompts, chat history, memory, archive/unarchive, JSON backup and restore, OpenAI-compatible/Automatic1111/ComfyUI image generation, web search, file/image attachments, MCP tools, skills, and interactive terminal UIs for forms, checklists, dashboards, and small tools. Generated UI specs are rendered as terminal components; `/ui N` activates a generated action, including state-bound switches and checkboxes. Markdown replies render headings, lists, tables, links, and fenced code in the conversation. Remaining Expo features include voice input and playback, PDF/Office text extraction and file RAG, skill-script execution, marketplace browsing, and mobile-only notifications and app lock.

## Build

Install a current Rust toolchain, then run:

    cargo build --release
    cargo install --path .

The binary is named aihat.

## Release builds

Push a version tag such as `v0.1.0` to run [the release workflow](.github/workflows/release.yml). It tests the CLI, builds the platform binaries and packages, then creates or updates the matching GitHub Release with SHA-256 checksums.

The workflow publishes portable Windows executables for x64, x86 and ARM64; Intel, Apple Silicon and universal macOS disk images; Debian packages for amd64, i386, arm64, armhf and riscv64; Fedora RPMs for x86_64 and aarch64; Arch packages for x86_64 and aarch64; Alpine APKs for x86_64, aarch64 and armv7; and native FreeBSD, OpenBSD and NetBSD packages for x86_64 and ARM64. It also includes a portable FreeBSD i386 binary archive.

The Windows `.exe` files are portable CLI programs, not graphical setup wizards. The macOS `.dmg` files contain the binary and an `install.sh` helper. BSD builds use virtual machines; the workflow covers the listed architectures rather than every historical BSD CPU target.

## First run

    aihat providers add
    aihat "Explain this project"
    aihat

Adding FreeLLMAPI from the preset fills in `https://freellmapi-production-5248.up.railway.app/v1`; enter the unified API key from its dashboard. In the TUI, open `/providers`, select an endpoint, and press `e` to edit its name, URL, headers, or API key. Press Enter to edit temperature, token limit, timeout, and headers for the selected model/provider. The CLI also supports `aihat providers set-url NAME URL`. Custom URLs are saved as entered and are not rewritten during refresh. `/refresh` fetches models from all configured providers, and `/refresh NAME` refreshes one provider.

Other provider setup asks for its OpenAI-compatible base URL and API key. Keys go to the operating system credential store when it is available; the fallback is a user-only keys.json file in AI Hat's data folder. Keys are never stored in SQLite or config.toml.

## Commands

    aihat --help
    aihat providers
    aihat providers add
    aihat providers test NAME
    aihat providers set-default NAME
    aihat providers set-url NAME URL
    aihat models
    aihat models refresh [NAME]
    aihat models set-default MODEL [--provider NAME]
    aihat models add MODEL [--provider NAME] [--kind chat|image|embedding]
    aihat chats
    aihat chats show ID
    aihat chats export ID
    aihat chats archive ID
    aihat prompts
    aihat prompts add NAME
    aihat mcp
    aihat skills
    aihat config path
    aihat memory
    aihat memory on
    aihat memory chat ID off
    aihat backup export
    aihat backup import backup.json
    aihat search-key set brave
    aihat config set search_engine brave
    aihat config set search_engine searxng
    aihat config set search_url https://your-searxng.example
    aihat image "a tiny lighthouse in a storm" --count 2
    # In the TUI: /ui build a packing checklist, then /ui 1 to run its first action

Web search defaults to the configured provider. FreeLLMAPI uses Gemini's Google Search grounding, so a Gemini 2.5 Flash model must be available in that service. Other providers can use their `/search` endpoint; alternatively choose `brave`, `tavily`, or `searxng` and configure the matching key or URL. The old implicit `http://localhost:8080` SearXNG default is migrated automatically.

For scripting:

    aihat "Summarize notes.md" --file notes.md --json
    cat crash.log | aihat "Explain this log" --json
    aihat "Continue the last chat" --continue
    aihat "Summarize my issues" --tool GitHub --yes

## Terminal controls

Enter sends, Shift+Enter inserts a line, Up/Down recalls submitted prompts and commands, PageUp/PageDown scrolls the conversation, Ctrl+P opens the model picker, and Ctrl+R or `/history` opens chat history. In History, Enter resumes, Ctrl+D asks before deleting, Ctrl+E exports, and Ctrl+N starts a chat. `/` opens the command palette, `/refresh` reloads provider models, `/compact` summarizes older context while preserving the transcript, @ attaches a file, `#` saves a chat-scoped system note, and ? shows keys. Ctrl+O shows or hides reasoning text when the model provides it. Ctrl+Y copies the latest reply, Ctrl+L clears the visible screen without deleting history, Ctrl+C twice quits, and Esc closes a popup or stops a reply. The model is asked for readable Markdown, and the terminal renders headings, lists, tables, links, code blocks, and inline formatting instead of showing Markdown markers as raw text.

`/skills` opens the skill manager. Press `a` to import a skill folder or `SKILL.md`, Space to enable or disable the selected skill, and `m` to cycle its use mode: Automatic (the model can choose it), Always (its instructions are included in every chat), or When I ask (use its `/skill-name` command). Enter opens the skill details and instructions; Up/Down or PageUp/PageDown scroll long instructions, `r` reloads from source, and `x` removes the skill. Text reference files inside the skill folder are imported with it.

`/mcp` opens the MCP and chat-tools screen. Press `a` to add a remote HTTP server, `r` to refresh its tools, `k` to save its access token, `p` to change its write-tool approval policy, and `x` to remove it after confirmation. Space enables or disables the selected server or skill for the current chat.

In `/providers`, press `e` to edit the selected provider's name, URL, headers, and optional key. Press Enter to edit its request defaults; in the model pane, press `d` to make that model the default. Tab moves between fields, Ctrl+U clears the active field, Enter saves, and Ctrl+T saves and tests the endpoint. Press `t` to test it again from the provider list.

The TUI is designed for 80x24 and wider terminals. The palette follows the supplied spec: near-black background, lime accent, gray body text, cyan commands and links, orange warnings, red errors, and purple code/reasoning.

## Data

The terminal client uses the same SQLite schema as the desktop app. On Linux, its default data directory is ~/.local/share/com.amanm.aihat; on macOS and Windows it uses the platform's normal application data directory under the same app identifier. To use an existing app database:

    AIHAT_DB_PATH=/path/to/aihat.db aihat

The configuration file is ~/.config/aihat/config.toml. API keys are kept separately.

