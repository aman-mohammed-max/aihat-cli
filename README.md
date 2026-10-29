<div align="center">

# ◆ AI Hat

**Chat with any AI model, right in your terminal.**

Bring your own models — OpenRouter, Groq, Ollama, LM Studio, vLLM, or any OpenAI-compatible server — and chat in a fast, good-looking full-screen app. Same providers, prompts, skills and chats as the AI Hat phone and desktop app.

[**Download**](#download) · [Get started](#get-started) · [What it can do](#what-it-can-do) · [Help](#help) · [Full guide](docs/GUIDE.md)

</div>

```text
 ◆ AI Hat 0.1.0 · ~/projects/fabric-mod                                  qwen3-32b · Home vLLM
──────────────────────────────────────────────────────────────────────────────────────────────

 › what changed in the Fabric API this month? check the web

 ● web_search "fabric api changes october 2026"
   └ 8 results · read 6 pages · 1.8s

 ● Two releases landed this month.

   1. Networking now uses payload records. Register each payload once, then send it
      from the server or the client. [1]
   2. Two render hooks are deprecated in favour of the new render events. [2]

   Sources  [1] fabricmc.net · Fabric 0.116 release notes   [2] github.com · changelog
   412 tokens · 182 tok/s · 2.3s
╭────────────────────────────────────────────────────────────────────────────────────────────╮
│ › Ask anything · / commands · @ attach a file · ! run a shell command                      │
╰────────────────────────────────────────────────────────────────────────────────────────────╯
 enter send · shift+enter line · esc stop · ctrl+p model · ? keys          9.8k / 128k · 38 ms
```

## Download

Pick your system. Every file is on the [Releases page](https://github.com/aman-mohammed-max/aihat-cli/releases/latest).

| Your computer | Download |
| --- | --- |
| **Windows** (most PCs) | [aihat-windows-x64.exe](https://github.com/aman-mohammed-max/aihat-cli/releases/latest/download/aihat-windows-x64.exe) |
| Windows on ARM (Surface Pro X, Snapdragon laptops) | [aihat-windows-arm64.exe](https://github.com/aman-mohammed-max/aihat-cli/releases/latest/download/aihat-windows-arm64.exe) |
| Windows, older 32-bit | [aihat-windows-x86.exe](https://github.com/aman-mohammed-max/aihat-cli/releases/latest/download/aihat-windows-x86.exe) |
| **Mac** (any Mac) | [aihat-macos-universal.dmg](https://github.com/aman-mohammed-max/aihat-cli/releases/latest/download/aihat-macos-universal.dmg) |
| **Ubuntu, Debian, Linux Mint, Pop!_OS, Raspberry Pi OS** | [64-bit PC](https://github.com/aman-mohammed-max/aihat-cli/releases/latest/download/aihat-debian-amd64.deb) · [ARM64](https://github.com/aman-mohammed-max/aihat-cli/releases/latest/download/aihat-debian-arm64.deb) · [ARM 32-bit](https://github.com/aman-mohammed-max/aihat-cli/releases/latest/download/aihat-debian-armhf.deb) · [32-bit PC](https://github.com/aman-mohammed-max/aihat-cli/releases/latest/download/aihat-debian-i386.deb) · [RISC-V](https://github.com/aman-mohammed-max/aihat-cli/releases/latest/download/aihat-debian-riscv64.deb) |
| **Fedora, RHEL, Rocky, AlmaLinux, openSUSE** | [64-bit PC](https://github.com/aman-mohammed-max/aihat-cli/releases/latest/download/aihat-fedora-x86_64.rpm) · [ARM64](https://github.com/aman-mohammed-max/aihat-cli/releases/latest/download/aihat-fedora-aarch64.rpm) · [ARM 32-bit](https://github.com/aman-mohammed-max/aihat-cli/releases/latest/download/aihat-fedora-armv7hl.rpm) · [32-bit PC](https://github.com/aman-mohammed-max/aihat-cli/releases/latest/download/aihat-fedora-i686.rpm) · [RISC-V](https://github.com/aman-mohammed-max/aihat-cli/releases/latest/download/aihat-fedora-riscv64.rpm) |
| **Arch Linux, Manjaro, EndeavourOS, Arch Linux ARM** | [64-bit PC](https://github.com/aman-mohammed-max/aihat-cli/releases/latest/download/aihat-arch-x86_64.pkg.tar.zst) · [ARM64](https://github.com/aman-mohammed-max/aihat-cli/releases/latest/download/aihat-arch-aarch64.pkg.tar.zst) · [ARM 32-bit](https://github.com/aman-mohammed-max/aihat-cli/releases/latest/download/aihat-arch-armv7h.pkg.tar.zst) · [32-bit PC](https://github.com/aman-mohammed-max/aihat-cli/releases/latest/download/aihat-arch-i686.pkg.tar.zst) · [RISC-V](https://github.com/aman-mohammed-max/aihat-cli/releases/latest/download/aihat-arch-riscv64.pkg.tar.zst) |
| **Alpine Linux** | [64-bit PC](https://github.com/aman-mohammed-max/aihat-cli/releases/latest/download/aihat-alpine-x86_64.apk) · [ARM64](https://github.com/aman-mohammed-max/aihat-cli/releases/latest/download/aihat-alpine-aarch64.apk) · [ARM 32-bit](https://github.com/aman-mohammed-max/aihat-cli/releases/latest/download/aihat-alpine-armv7.apk) · [32-bit PC](https://github.com/aman-mohammed-max/aihat-cli/releases/latest/download/aihat-alpine-x86.apk) · [RISC-V](https://github.com/aman-mohammed-max/aihat-cli/releases/latest/download/aihat-alpine-riscv64.apk) |
| **Any other Linux** (Gentoo, Void, NixOS, Slackware, ChromeOS Linux…) | [64-bit PC](https://github.com/aman-mohammed-max/aihat-cli/releases/latest/download/aihat-linux-musl-x86_64.tar.gz) · [ARM64](https://github.com/aman-mohammed-max/aihat-cli/releases/latest/download/aihat-linux-musl-aarch64.tar.gz) · [ARM 32-bit](https://github.com/aman-mohammed-max/aihat-cli/releases/latest/download/aihat-linux-musl-armv7.tar.gz) · [32-bit PC](https://github.com/aman-mohammed-max/aihat-cli/releases/latest/download/aihat-linux-musl-x86.tar.gz) · [RISC-V](https://github.com/aman-mohammed-max/aihat-cli/releases/latest/download/aihat-linux-musl-riscv64.tar.gz) |
| **FreeBSD** | [64-bit PC](https://github.com/aman-mohammed-max/aihat-cli/releases/latest/download/aihat-freebsd-amd64.pkg) · [ARM64](https://github.com/aman-mohammed-max/aihat-cli/releases/latest/download/aihat-freebsd-aarch64.pkg) · [32-bit PC](https://github.com/aman-mohammed-max/aihat-cli/releases/latest/download/aihat-freebsd-i386.tar.gz) |
| **OpenBSD** | [64-bit PC](https://github.com/aman-mohammed-max/aihat-cli/releases/latest/download/aihat-openbsd-amd64.tgz) · [ARM64](https://github.com/aman-mohammed-max/aihat-cli/releases/latest/download/aihat-openbsd-arm64.tgz) |
| **NetBSD** | [64-bit PC](https://github.com/aman-mohammed-max/aihat-cli/releases/latest/download/aihat-netbsd-amd64.tgz) · [ARM64](https://github.com/aman-mohammed-max/aihat-cli/releases/latest/download/aihat-netbsd-arm64.tgz) |

<details>
<summary><b>Not sure which one you need?</b></summary>

On Linux, macOS or BSD, open a terminal and type `uname -m`:

| It says | Pick |
| --- | --- |
| `x86_64` or `amd64` | 64-bit PC |
| `aarch64` or `arm64` | ARM64 |
| `armv7l` | ARM 32-bit |
| `i686` or `i386` | 32-bit PC |
| `riscv64` | RISC-V |

On Windows, open **Settings → System → About** and look at **System type**.

</details>

## Install

<details open>
<summary><b>Windows</b></summary>

1. Download the `.exe` for your PC.
2. Rename it to `aihat.exe` and move it into a folder of its own, like `C:\Program Files\AI Hat`.
3. Open **Windows Terminal** and run it: `& "C:\Program Files\AI Hat\aihat.exe"`

To type just `aihat` from anywhere, add that folder to your PATH: search the Start menu for *Edit the system environment variables* → **Environment Variables** → **Path** → **New**.

If Windows SmartScreen says it protected your PC, click **More info → Run anyway**. AI Hat isn't code-signed yet.

</details>

<details>
<summary><b>macOS</b></summary>

1. Open the `.dmg`.
2. In **Terminal**, run the installer inside it:
   ```sh
   sudo /Volumes/"AI Hat CLI"/install.sh
   ```
3. Type `aihat` to start.

If macOS says the app can't be checked for malicious software, run `xattr -d com.apple.quarantine /usr/local/bin/aihat` once. AI Hat isn't notarized yet.

</details>

<details>
<summary><b>Linux</b></summary>

Open a terminal in the folder you downloaded to:

| System | Command |
| --- | --- |
| Ubuntu, Debian, Mint… | `sudo apt install ./aihat-debian-*.deb` |
| Fedora, RHEL… | `sudo dnf install ./aihat-fedora-*.rpm` |
| openSUSE | `sudo zypper install ./aihat-fedora-*.rpm` |
| Arch, Manjaro… | `sudo pacman -U ./aihat-arch-*.pkg.tar.zst` |
| Alpine | `sudo apk add --allow-untrusted ./aihat-alpine-*.apk` |
| Any other Linux | `tar -xzf aihat-linux-*.tar.gz && sudo install aihat /usr/local/bin/` |

Then type `aihat`.

</details>

<details>
<summary><b>FreeBSD, OpenBSD, NetBSD</b></summary>

| System | Command |
| --- | --- |
| FreeBSD | `sudo pkg add ./aihat-freebsd-*.pkg` |
| OpenBSD | `doas pkg_add -D unsigned ./aihat-openbsd-*.tgz` |
| NetBSD | `sudo pkg_add ./aihat-netbsd-*.tgz` |

Then type `aihat`.

</details>

## Get started

**1. Connect a model.** AI Hat talks to any service that speaks the OpenAI API: a cloud service like OpenRouter or Groq, or a model on your own computer with Ollama or LM Studio.

```sh
aihat providers add
```

Pick a preset, paste your API key (if it needs one), and AI Hat tests the connection and finds your models.

**2. Start chatting.**

```sh
aihat
```

Type a message and press **Enter**. Press **?** to see every key.

**3. Or ask a quick question** without opening the full screen:

```sh
aihat "explain this error" -f crash.log
```

Already use the AI Hat app on this computer? Your providers, prompts and chats are already here.

## What it can do

- **💬 Any model, one place.** Switch models with **Ctrl+P**, system prompts with **Ctrl+S**, and pick up old chats with **Ctrl+R**. `/effort high` lets reasoning models think longer.
- **🌐 Searches the web** and shows its sources: type `/search` or turn it on in `/tools`.
- **📎 Reads your files.** Type `@` to attach a file or a web page; pictures too, for models that can see.
- **🎨 Draws pictures.** `/image a lighthouse in a storm` shows the picture right in the terminal (the real picture in terminals that can show one, a detailed block drawing in the rest), then asks where to save it. Press **v** to see it full screen.
- **🛍️ Marketplace.** `/market` finds skills (popular ones, skills.sh, GitHub, and websites like Vercel and Stripe that publish their own) and MCP servers (the official MCP registry), and adds them in one key.
- **🔌 Connects to tools (MCP).** Add GitHub, Linear, Notion, a database and more. Sign-in happens in your browser, automatically.
- **🧩 Learns skills.** Add a skill from the marketplace, a folder or a GitHub link. Skills you installed for Claude Code, Codex or other coding agents (`~/.agents/skills`, `~/.claude/skills`, `~/.codex/skills`) work here too, and **Ctrl+G** in `/market` installs a skill for all of them at once.
- **✍️ Writes system prompts for you.** `/prompt a patient Rust tutor` and the model writes one, saves it and uses it in the chat.
- **🧠 Remembers you.** Your name, projects and preferences carry across chats. See and edit them with `/memory`.
- **⌨️ Works your way.** Every shortcut, colour and option is in one file: `aihat config edit`.
- **📁 Works in your folder.** With your OK, the assistant reads and searches the files in the folder you start it in, and asks before it runs any command there. `/folder` changes your answer.
- **🔒 Private by default.** Chats stay on your computer. API keys go to your system keychain (or a private file only you can read on Linux and BSD). Nothing leaves your computer except what goes to the models, search and tools you set up.

## Keys

| Key | Does |
| --- | --- |
| **Enter** | Send |
| **Shift+Enter** | New line |
| **Esc** | Stop the reply |
| **↑** | Edit your last message |
| **Ctrl+P** | Switch model |
| **Ctrl+S** | Switch system prompt |
| **Ctrl+R** | Past chats |
| **Ctrl+N** | New chat |
| **/** | All commands |
| **@** | Attach a file |
| **Ctrl+V** | Paste a picture or copied files (or drag a picture in) |
| **?** | Every key |
| **Ctrl+C** twice | Quit |

## Help

**"Can't reach the provider"** — check the address with `aihat providers` and test it with `aihat providers test NAME`. For a model on your own computer, make sure Ollama or LM Studio is running.

**"Key rejected (401)"** — save the key again: `aihat providers set-key NAME`.

**Colours look wrong** — your terminal may not tell AI Hat it supports full colour. Run `aihat config set colors truecolor`. For the intended look, use the JetBrains Mono font.

**Pictures look blocky** — your terminal can't show real pictures, so AI Hat draws them with blocks. See which way looks best with `aihat preview picture.png --as sixel` (or `iterm2`, `kitty`, `blocks`, `halfblocks`), then keep it: `aihat config set images.preview sixel`.

**Something else?** [Open an issue](https://github.com/aman-mohammed-max/aihat-cli/issues) and say what you typed and what happened. `/about` (or `aihat about`) has every link.

The [full guide](docs/GUIDE.md) covers every command, setting and feature.

## For developers

Build from source with a current [Rust](https://rustup.rs) toolchain:

```sh
git clone https://github.com/aman-mohammed-max/aihat-cli
cd aihat-cli
cargo build --release      # the program is target/release/aihat
```

Every push is built and packaged for every platform by [the build workflow](.github/workflows/release.yml); pushing a version tag such as `v0.1.0` publishes a release with all the files above and their SHA-256 checksums.

## Made by

[Aman Mohammed](https://amanm.space) · [GitHub](https://github.com/aman-mohammed-max) · [say hi](https://connect.amanm.space). AI Hat is also [an app for phones](https://github.com/aman-mohammed-max/aihat) that shares its chats and settings with this one.

## License

MIT
