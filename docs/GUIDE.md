# AI Hat in the terminal: full guide

AI Hat in the terminal: chat with any OpenAI-compatible model. Running `aihat` opens the full-screen chat; `aihat "prompt"` and `aihat <command>` print and exit, so they work in scripts and pipes.

This is the detailed reference. For downloads and a quick start, see the [README](../README.md).

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

## Marketplace

`/market` (or `/market mcp`) finds things to add, as on the phone app. **Tab** switches between skills and MCP servers, **←/→** picks where skills come from, typing searches, **enter** adds the selected one, and the right side shows it in full before you add it, its SKILL.md drawn like a reply (**PgUp/PgDn** scroll it):

- **Popular**: the most-installed skills in public GitHub repositories (claude-plugins.dev).
- **skills.sh**: the open skills directory.
- **Repo or site**: `owner/repo`, a GitHub link, or a website that publishes skills itself at `/.well-known/agent-skills` (vercel.com, developers.cloudflare.com, docs.stripe.com, supabase.com). Downloads from websites are checked against the checksum the site gives.
- **MCP servers**: hosted servers that work straight away (DeepWiki, Context7, Microsoft Learn…), then the official MCP registry, including servers that run on this computer (npm, PyPI, Docker). A server that needs a token or settings asks for them first; one with a browser sign-in opens it.

**Ctrl+G** on a skill installs it for every coding agent on the computer (`~/.agents/skills`, linked into `~/.claude/skills` and `~/.codex/skills` when those exist). From the command line:

    aihat skills search pdf                 # popular skills; --from skills.sh
    aihat skills add anthropics/skills/pdf  # or a GitHub link, a folder, or just a name
    aihat skills add pdf --global           # for every agent on the computer
    aihat mcp search notion
    aihat mcp add notion                    # a name from the search, or NAME URL

## Skills

A skill is a folder with a `SKILL.md`, from this computer or from GitHub:

    aihat skills add ./my-skill
    aihat skills add https://github.com/anthropics/skills/tree/main/skills/xlsx
    aihat skills show github.com/anthropics/skills/tree/main/skills/pdf     # read it first
    aihat skills update                                                      # read every skill again

The files next to `SKILL.md` (references, templates, scripts) come along as text so the model can read them; scripts are never run. In `/skills`, `a` adds a folder or a GitHub link and `r` reads the selected skill again.

Global skills, the ones other coding agents use, work here too: `~/.agents/skills` (the shared folder the `npx skills` installer fills), `~/.claude/skills` (Claude Code), `~/.codex/skills` (Codex) and `~/.config/agents/skills`, plus `.agents/skills`, `.claude/skills` and `.codex/skills` in the current folder. They're read where they are, nothing is copied. `/skills` lists them under their own heading, and space turns one off; `"skills": { "use_global": false }` in settings.json turns them all off.

## System prompts

`/prompt` (or **Ctrl+S**) picks one. `/prompt a patient Rust tutor who explains with small examples` has the model write a new one instead: it is saved (tagged *generated*), used in this chat, and shown so you can read it (`e` opens it in your editor). The last row of the prompt list, **Write a new one with AI**, does the same, and **Ctrl+E** there edits the selected prompt in `$EDITOR`. From the command line:

    aihat prompts new a patient Rust tutor --use    # --use: new chats use it

## Reasoning effort

`/effort` (or `/think`) sets how much a reasoning model thinks before it answers: off (the model decides), low, medium or high, sent as `reasoning_effort`; models without reasoning ignore it. It applies to the open chat, or to new chats when none is open, and the status line shows it when it isn't off. `/effort high` sets it straight away. For one prompt: `aihat -e high "why does this deadlock?"`.

## About

`/about` and `aihat about` show who made AI Hat (Aman Mohammed), with links to the portfolio, GitHub, the phone app, and a place to report a problem; enter opens the selected link. Ask the assistant who made it and it says both: which model it is and who made that model, and that this app was made by Aman Mohammed.

## Your folder

Nothing is asked when aihat opens. The first time the assistant wants to look at the files in the folder you started it in, a box asks in the chat: **Let the assistant read this folder?** with **Allow until I quit**, **Always allow this folder** or **Don't allow**. You can also answer ahead of time by typing `/folder allow` (always), `/folder session` (until you quit) or `/folder off`; `/folder` alone shows the answer.

Allowed, the assistant can list, read and search the files in the folder and the folders inside it (what `.gitignore` leaves out is skipped, and nothing outside the folder can be read). Running a command is asked every time, in a **Run this command?** box with the command and why: **Run it once**, **Always run this here** (that exact command, in this folder), **Edit the command**, or **Don't run it**. Commands stop after two minutes or when you press esc. Skills that come with scripts go through the same box. `/folder` lists the commands you always allow, and `/folder clear` (or `c` there) clears them.

From the shell, `aihat --folder "why do the tests fail?"` lets it read the folder for that prompt; without `--folder` it asks on the terminal the first time it looks. Commands always ask (`--yes` allows them).

## Speaking

**Ctrl+T** (or `/voice`) starts listening; Ctrl+T again or enter stops, esc cancels. The words land in the message box, to check before sending. From the shell, `aihat --voice` records until you press enter, then asks the model, and `aihat transcribe recording.wav` prints what a file says.

Recording uses what the computer has: PipeWire (`pw-record`), PulseAudio (`parecord`), ALSA (`arecord`), SoX or ffmpeg; `voice.record` in settings.json can name another command that records to `{file}`. On a Chromebook, turn on Settings › Linux › Allow Linux to access your microphone.

Speech becomes text one of two ways, chosen in `/voice` (or `aihat voice use local|endpoint|auto`):

- **On this computer, with Phonon-2**, an English speech model (Fermion Research, from NVIDIA's Parakeet TDT 0.6B v3, CC-BY-4.0). `/voice` downloads it from Hugging Face (164 MB, checked against its published checksums), or `aihat voice download`. AI Hat runs it itself, in Rust: no Python, nothing else to install, and no network once it is downloaded. It needs about 250 MB of memory while in use and loads while you are still speaking. `aihat voice remove` deletes it.
- **An endpoint**: any OpenAI-compatible `/audio/transcriptions` (your default provider, or an address and model set in `/voice`).

`auto` (the default) uses Phonon-2 once it is downloaded, else the endpoint. `aihat transcribe recording.wav` (WAV or FLAC) prints what a file says.

```json
"voice": { "engine": "auto", "url": "", "model": "", "record": "" }
```

## Pasting

**Ctrl+V** pastes from the clipboard: a picture goes with your next message (the message box says `+ picture · esc removes`), files copied in a file manager are attached, and text is typed in. Dragging a picture file into the terminal attaches it too.

## Memory

Memory is shared by every chat, like in the app. Models that can use tools keep it themselves (they save, correct and forget single memories, never all of them); for models that can't, a short request after each reply picks out what is worth keeping. `/memory` lists what is saved: `m` turns it on or off for this chat, `g` everywhere, `a` adds one and `d` forgets one. From the command line: `aihat memory`, `aihat memory add "I work in Rust"`, `aihat memory forget 2`, `aihat memory off`.

The layout works from 80×24 up; under 100 columns the header drops the folder and dialogs go full width. The chat fills the whole terminal, and replies keep the design's share of the width (90 of 112 columns). To get the design's 112-column window instead, centred, set `"width": 112` in settings.json (`0` fills the terminal). The window's title reads like the design's: `aihat — ~/projects/fabric-mod · 112×34`.

## Pictures

`/image a lighthouse in a storm` improves the idea into a fuller prompt, draws it, and shows it over the chat with **Save to ~/Downloads**, **Save as…**, **Copy the picture**, **View full screen** (`v`) or **Discard**. Nothing is written until you choose; Save never overwrites a file. Saved pictures show as small previews in the chat, and so do pictures you attach with `@`. The preview grows with the terminal.

The first time a picture is shown, aihat asks the terminal what it can draw. Terminals that show real pictures (kitty, Ghostty, WezTerm, iTerm2, foot, Windows Terminal with sixel, and any other with sixel or the kitty protocol) get the picture itself. The rest get a drawing in block symbols: each cell takes the half, quarter or eighth block and the two colours that match its part of the picture best, which keeps edges and small details sharp. Inside tmux or screen, and on Windows consoles that don't pass answers back, it uses blocks. To compare, then choose:

    aihat preview picture.png --as sixel       # or iterm2, kitty, blocks, halfblocks
    aihat config set images.preview sixel      # auto (the default), kitty, sixel, iterm2, blocks, halfblocks, off

## Commands

    aihat --help
    aihat providers [list|add|test|set-default|set-key|set-url|remove]
    aihat models [list [--all]|refresh|set-default MODEL|add MODEL] [-P PROVIDER]
    aihat prompts [list [--search WORD]|show|add|new DESCRIPTION [--use]|use|delete]
    aihat chats [list|show N|open N|export N [--json] [-o FILE]|rename|archive|delete]
    aihat mcp [list|add NAME URL|add NAME -- COMMAND|add NAME|search WORDS|login|logout|start|refresh|set-key|allow|ask|remove]
    aihat skills [list|add SOURCE [--global]|search WORDS [--from skills.sh]|update|enable|disable|show|remove]
    aihat config [path|get [KEY]|set KEY VALUE|edit]
    aihat memory [list|add TEXT|forget N|on|off|clear]
    aihat backup [export [-o FILE]|import FILE]
    aihat search-key [set|remove] brave|tavily
    aihat image "a lighthouse in a storm" --count 2 [-o FILE_OR_FOLDER]
    aihat preview PICTURE [--as kitty|sixel|iterm2|blocks|halfblocks]
    aihat about

`aihat image` shows each picture's preview and asks where to save it; with `-o`, `--yes`, or output to a pipe it saves without asking (to `-o`, or the `images.save_to` folder) and prints the path.

Chats are numbered in the order they were made (`aihat chats` shows `#41`), so `aihat chats export 41 > notes.md` works.

## One prompt

    aihat "what changed in the Fabric API this month?" -w
    cat crash.log | aihat -s "Rust Systems Mentor" "why does this panic?"
    aihat -c "add a timeout on the join too"
    aihat -e high "prove the loop terminates"     # reasoning effort: off, low, medium, high
    aihat "summarise" -f notes.md --json | jq .usage
    aihat "open an issue for this" -t github --yes

Options: `-m` model, `-P` provider, `-s` saved system prompt, `-f` file (repeatable; piped stdin is attached too), `-w` web search, `-c` continue the last chat, `-t` MCP server (repeatable), `--json`, `--no-stream`, `-y` approve tools and confirmations, `--no-color`, `--ascii`.

The reply goes to stdout and progress goes to stderr, so `aihat "…" > answer.md` saves only the answer. When stdout is not a terminal there are no colours or spinners, just the reply text. `--json` prints the reply, usage, sources and tool calls.

Exit codes: 0 ok · 1 error · 2 bad usage · 3 provider unreachable · 4 key rejected · 130 stopped with ctrl+c (the partial reply is saved; `aihat -c` continues it).

## Web search

Web search uses the provider by default: its `/search` endpoint, or, for FreeLLMAPI, Google Search grounding through a Gemini model. aihat picks the newest Gemini Flash model FreeLLMAPI can run right now, and checks that the answer really came from Gemini: FreeLLMAPI hands a request to another provider when a model is retired or out of quota, and those can't search. Up to four Gemini models are tried; if none can search, the message says why. To use another engine:

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
  "mouse": false,
  "animations": true,
  "images": { "preview": "auto", "save_to": "~/Downloads" },
  "skills": { "use_global": true, "off": [] },
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

[The build workflow](../.github/workflows/release.yml) runs on every push and pull request: it tests the CLI, then builds and packages it for every platform, so a broken target shows up right away. Pushing a version tag such as `v0.1.0` also publishes a GitHub Release with every file and SHA-256 checksums:

| Platform | Files |
| --- | --- |
| Windows | `aihat-windows-x64.exe`, `-x86.exe`, `-arm64.exe` (portable, nothing to install) |
| macOS | `aihat-macos-universal.dmg`, `-apple-silicon.dmg`, `-intel.dmg` |
| Debian, Ubuntu, Mint, Raspberry Pi OS… | `aihat-debian-{amd64,i386,arm64,armhf,riscv64}.deb` |
| Fedora, RHEL, openSUSE… | `aihat-fedora-{x86_64,i686,aarch64,armv7hl,riscv64}.rpm` |
| Arch, Manjaro, Arch Linux ARM… | `aihat-arch-{x86_64,i686,aarch64,armv7h,riscv64}.pkg.tar.zst` |
| Alpine | `aihat-alpine-{x86_64,x86,aarch64,armv7,riscv64}.apk` |
| Any Linux | `aihat-linux-*.tar.gz` (the `musl-*` ones are static and run on any distribution) |
| FreeBSD | `aihat-freebsd-{amd64,aarch64}.pkg`, `aihat-freebsd-i386.tar.gz` |
| OpenBSD | `aihat-openbsd-{amd64,arm64}.tgz` (`pkg_add -D unsigned`) |
| NetBSD | `aihat-netbsd-{amd64,arm64}.tgz` |
