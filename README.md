# AI Hat CLI

AI Hat in the terminal: chat with any OpenAI-compatible model. Running `aihat` opens the full-screen chat; `aihat "prompt"` and `aihat <command>` print and exit, so they work in scripts and pipes.

It uses the same SQLite database, providers, models, system prompts, skills, MCP servers and chats as the AI Hat app, and follows the terminal screens in the AI Hat design.

## Build

Install a current Rust toolchain, then:

    cargo build --release
    cargo install --path .

The binary is named `aihat`.

## First run

    aihat providers add      # guided: preset, name, URL, key, connection test, models
    aihat "hello"            # one reply, printed
    aihat                    # the full-screen chat

For scripts, add an endpoint without questions:

    aihat providers add "Home vLLM" --url http://192.168.1.40:8000/v1 \
        --key-env VLLM_KEY --models qwen3-32b,qwen3-vl-32b --default

Presets fill in the URL for OpenRouter, Groq, Ollama, LM Studio, vLLM and FreeLLMAPI. API keys go to the system keychain on macOS and Windows. On Linux they go to a private `keys.json` in AI Hat's data folder that only you can read: the Linux kernel keyring forgets everything at restart, so older builds lost keys that way. A key such a build left there is moved over the first time it's used. Keys are never written to SQLite or `settings.json`.

## The full-screen chat

The screen is a header, the conversation, the message box and a status line. Replies are rendered as Markdown: headings, lists, tables, quotes, links, `[1]` citations, and code blocks with line numbers and syntax highlighting. Tool calls show as `● web_search "…"` with a `└` summary, reasoning as `◇ Thought`, and each reply ends with its sources and token stats.

| Key | Does |
| --- | --- |
| enter | send |
| shift+enter, alt+enter, ctrl+j | new line (also `\` then enter) |
| esc | stop the reply (the partial reply is kept) |
| ↑ in an empty box | edit your last message; ↑ again for older prompts |
| ctrl+p | switch model (tab: this chat or the default · ctrl+e: model defaults) |
| ctrl+s | switch system prompt |
| ctrl+r | past chats (ctrl+d delete · ctrl+e export · ctrl+n new) |
| ctrl+n | new chat |
| ctrl+l | clear the screen (the chat stays saved) |
| ctrl+o | show or hide reasoning |
| ctrl+y | copy the last reply (works over SSH too) |
| pgup, pgdn | scroll |
| ctrl+c | clear the box, stop a reply, or press twice to quit |
| ? | every key |

Every key in the first column of this table can be changed in `settings.json` (see [Settings](#settings)).

Typing `/` lists commands: `/model`, `/prompt`, `/providers`, `/tools`, `/mcp`, `/skills`, `/new`, `/resume`, `/compact`, `/export [json]`, `/image <idea>` (improves the prompt, then draws; `--raw` skips that), `/search <query>`, `/refresh`, `/rename`, `/copy`, `/clear`, `/memory` (what models remember; on or off for this chat), `/keys`, `/settings` (opens `settings.json` in your editor), `/quit`.

The mouse wheel scrolls the chat. Scrolled up, a **↓ Jump to bottom** button appears at the bottom right; click it or press end. Hold shift to select text with the mouse (or set `"mouse": false` in settings.json).

- `@path` attaches a file (a picker opens as you type, respecting `.gitignore`); `@https://…` attaches a page.
- `!command` runs a shell command and attaches its output to your next message.
- `#note` saves a note to this chat's system prompt.

`/providers` shows endpoints on the left and the selected one's details on the right: `a` add, `t` test, `d` default, `e` edit, `k` key, `x` remove, `r` refresh models, tab to the model list, space to show or hide a model in the picker, enter to edit defaults (temperature, max tokens, timeout, headers).

`/tools` turns web search, memory, MCP servers and skills on or off for the chat. Tools that write data ask first, with "Allow once", "Always for this server", "Edit the arguments" or "Deny".

## MCP servers

aihat is a full MCP client. A server is either remote (an `https://…/mcp` address) or a program on this computer that talks over stdin and stdout:

    aihat mcp add linear https://mcp.linear.app/mcp
    aihat mcp add postgres -- npx -y @modelcontextprotocol/server-postgres $DB_URL

Servers that need a sign-in get one the way the MCP specification describes it (OAuth with PKCE, registered on the fly). aihat opens your browser, waits for it to come back to a one-off address on 127.0.0.1, keeps the tokens with your keys, and renews them when they run out. This happens on its own when you add a server, refresh it, or use it in a chat. If there is no web browser on the computer (over SSH, say), it says so; servers that take a pasted token ask for one instead (`aihat mcp set-key NAME`, or `k` in `/mcp`). `aihat mcp login NAME` and `logout`, or `s` and `o` in `/mcp`, sign in and out by hand.

Programs added with `--` start the first time they're needed and stop when aihat exits.

## Skills

A skill is a folder with a `SKILL.md`, from this computer or from GitHub:

    aihat skills add ./my-skill
    aihat skills add https://github.com/anthropics/skills/tree/main/skills/xlsx
    aihat skills show github.com/anthropics/skills/tree/main/skills/pdf     # read it first
    aihat skills update                                                      # read every skill again

The files next to `SKILL.md` (references, templates, scripts) come along as text so the model can read them; scripts are never run. In `/skills`, `a` adds a folder or a GitHub link and `r` reads the selected skill again.

## Memory

Memory is shared by every chat, like in the app. Models that can use tools keep it themselves (they save, correct and forget single memories, never all of them); for models that can't, a short request after each reply picks out what is worth keeping. `/memory` lists what is saved: `m` turns it on or off for this chat, `g` everywhere, `a` adds one and `d` forgets one. From the command line: `aihat memory`, `aihat memory add "I work in Rust"`, `aihat memory forget 2`, `aihat memory off`.

The layout works from 80×24 up; under 100 columns the header drops the folder and dialogs go full width. The chat fills the whole terminal; replies wrap at 90 columns so they stay easy to read. To get the design's 112-column window instead, centred, set `"width": 112` in settings.json (`0` fills the terminal).

## Pictures

`/image a lighthouse in a storm` improves the idea into a fuller prompt, draws it, and shows a small preview over the chat with **Save to ~/Downloads**, **Save as…**, **Copy the picture** or **Discard**. Nothing is written until you choose; Save never overwrites a file. Saved pictures show as small previews in the chat, and so do pictures you attach with `@`.

Previews are drawn with coloured half blocks, which work in any terminal with colour. Terminals that can show real pictures (kitty, Ghostty, WezTerm, iTerm2, foot, Konsole, Windows Terminal) get a sharp preview instead. To choose yourself:

    aihat config set images.preview iterm2     # or kitty, sixel, blocks, off

## Commands

    aihat --help
    aihat providers [list|add|test|set-default|set-key|set-url|remove]
    aihat models [list [--all]|refresh|set-default MODEL|add MODEL] [-P PROVIDER]
    aihat prompts [list [--search WORD]|show|add|use|delete]
    aihat chats [list|show N|open N|export N [--json] [-o FILE]|rename|archive|delete]
    aihat mcp [list|add NAME URL|add NAME -- COMMAND|login|logout|start|refresh|set-key|allow|ask|remove]
    aihat skills [list|add PATH_OR_LINK|update|enable|disable|show|remove]
    aihat config [path|get [KEY]|set KEY VALUE|edit]
    aihat memory [list|add TEXT|forget N|on|off|clear]
    aihat backup [export [-o FILE]|import FILE]
    aihat search-key [set|remove] brave|tavily
    aihat image "a lighthouse in a storm" --count 2 [-o FILE_OR_FOLDER]

`aihat image` shows each picture's preview and asks where to save it; with `-o`, `--yes`, or output to a pipe it saves without asking (to `-o`, or the `images.save_to` folder) and prints the path.

Chats are numbered in the order they were made (`aihat chats` shows `#41`), so `aihat chats export 41 > notes.md` works.

## One prompt

    aihat "what changed in the Fabric API this month?" -w
    cat crash.log | aihat -s "Rust Systems Mentor" "why does this panic?"
    aihat -c "add a timeout on the join too"
    aihat "summarise" -f notes.md --json | jq .usage
    aihat "open an issue for this" -t github --yes

Options: `-m` model, `-P` provider, `-s` saved system prompt, `-f` file (repeatable; piped stdin is attached too), `-w` web search, `-c` continue the last chat, `-t` MCP server (repeatable), `--json`, `--no-stream`, `-y` approve tools and confirmations, `--no-color`, `--ascii`.

The reply goes to stdout and progress goes to stderr, so `aihat "…" > answer.md` saves only the answer. When stdout is not a terminal there are no colours or spinners, just the reply text. `--json` prints the reply, usage, sources and tool calls.

Exit codes: 0 ok · 1 error · 2 bad usage · 3 provider unreachable · 4 key rejected · 130 stopped with ctrl+c (the partial reply is saved; `aihat -c` continues it).

## Web search

Web search uses the provider by default: its `/search` endpoint, or Google Search grounding for FreeLLMAPI (a Gemini 2.5 Flash model must be available there). To use another engine:

    aihat search-key set brave
    aihat config set search.engine brave
    aihat config set search.engine searxng
    aihat config set search.url https://your-searxng.example

## Colours

The palette follows the design: near-black background, lime accent, grey text, cyan commands and links, orange warnings, soft red errors, purple code keywords and reasoning. Terminals with 24-bit colour get the exact colours. That includes terminals that support it without saying so through `COLORTERM`, like the ChromeOS terminal. 256-colour and 16-colour terminals get the closest ones. `NO_COLOR` or `--no-color` turns colours off, and every state also has a glyph (✓ ✗ ! ●), so nothing depends on colour alone. `aihat config set accent "#7DD3FC"` changes the accent, and `aihat config set colors 256` picks the colour depth (`auto`, `truecolor`, `256`, `16`, `none`).

For the design's exact look, also set your terminal's font to JetBrains Mono.

## Data

The data folder is the AI Hat desktop app's: `~/.local/share/com.amanm.aihat` on Linux, and the platform's application data folder on macOS and Windows. `AIHAT_DATA_DIR` and `AIHAT_DB_PATH` point at another copy:

    AIHAT_DB_PATH=/path/to/aihat.db aihat

Options live in `~/.config/aihat/settings.json` (`aihat config path` shows every location). An older `config.toml` is moved into it once and kept as `config.toml.bak`.

## Settings

`~/.config/aihat/settings.json` is written with every option and its default the first time aihat runs. Edit it with `aihat config edit` (or `/settings` in the chat), or one option at a time with `aihat config set keys.switch_model f2`.

```json
{
  "accent": "#C6F46B",
  "colors": "auto",
  "ascii": false,
  "width": 0,
  "show_reasoning": false,
  "show_stats": true,
  "mouse": true,
  "images": { "preview": "auto", "save_to": "~/Downloads" },
  "search": { "engine": "provider", "url": "", "results": 6 },
  "keys": {
    "send": "enter",
    "new_line": ["shift+enter", "alt+enter", "ctrl+j"],
    "stop": "esc",
    "switch_model": "ctrl+p",
    "switch_prompt": "ctrl+s",
    "past_chats": "ctrl+r",
    "new_chat": "ctrl+n",
    "clear_screen": "ctrl+l",
    "toggle_reasoning": "ctrl+o",
    "copy_reply": "ctrl+y",
    "show_keys": "?",
    "quit": "ctrl+c"
  }
}
```

A key is written like `ctrl+p`, `alt+enter`, `shift+tab`, `f2` or a single character; give a list for several. The hints on screen and the keys dialog follow your bindings, and ctrl+c always quits. A key aihat can't read is reported when the chat opens, and that action keeps its default. If the file has a JSON mistake, aihat says which line and `aihat config edit` still opens it.

## Release builds

[The build workflow](.github/workflows/release.yml) runs on every push and pull request: it tests the CLI, then builds and packages it for every platform, so a broken target shows up right away. Pushing a version tag such as `v0.1.0` also publishes a GitHub Release with every file and SHA-256 checksums:

| Platform | Files |
| --- | --- |
| Windows | `aihat-windows-x64.exe`, `-x86.exe`, `-arm64.exe` (portable, nothing to install) |
| macOS | `aihat-macos-universal.dmg`, `-apple-silicon.dmg`, `-intel.dmg` |
| Debian, Ubuntu, Mint… | `aihat-debian-{amd64,i386,arm64,armhf,riscv64}.deb` |
| Fedora, RHEL, openSUSE… | `aihat-fedora-{x86_64,aarch64}.rpm` |
| Arch, Manjaro… | `aihat-arch-{x86_64,aarch64}.pkg.tar.zst` |
| Alpine | `aihat-alpine-{x86_64,aarch64,armv7}.apk` |
| Any Linux | `aihat-linux-*.tar.gz` (the `musl-*` ones are static and run on any distribution) |
| FreeBSD | `aihat-freebsd-{amd64,aarch64}.pkg`, `aihat-freebsd-i386.tar.gz` |
| OpenBSD | `aihat-openbsd-{amd64,arm64}.tgz` (`pkg_add -D unsigned`) |
| NetBSD | `aihat-netbsd-{amd64,arm64}.tgz` |
