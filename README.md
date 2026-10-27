# chiika ✿

A terminal workspace for your shells **and your coding agents**. Like tmux, but with a canvas:
every shell is a little mascot you drag into teams, and Claude Code agents work together inside
them while you watch.

```
╭ ✿ chiika ◂ ────────────╮ ╭ backend · 3 ────────────────────────╮ ╭ coder · backend ───────────────╮
│ ▣ terminals │ ◈ canvas │ │ Rust API, tests first.              │ │ ● needs you                    │
│                        │ │                                     │ │ $ cargo test                   │
│ ▾ backend            3 │ │  ▀▀▄  ▄▀▀    ▀▀▄  ▄▀▀ !  ▀▀▄  ▄▀▀   │ │ asks permission to use Bash    │
│   ● lead      ✎ api.rs │ │ ▀▀▀▀▀▀▀▀▀▀  ▀▀▀▀▀▀▀▀▀▀  ▀▀▀▀▀▀▀▀▀▀  │ │                                │
│   ● coder  ! needs you │ │ ▀▀▀▀▀▀▀▀▀▀  ▀▀▀▀▀▀▀▀▀▀  ▀▀▀▀▀▀▀▀▀▀  │ │ [ allow ] [ deny ] [ in term ] │
│   ● server       ▸ 12m │ │    lead        coder      server    │ │                                │
╰────────────────────────╯ ╰─────────────────────────────────────╯ ╰────────────────────────────────╯
```

## Features

- **Terminals**: real shells (PowerShell, cmd, WSL, bash…) in split panes, with scrollback,
  mouse, renaming, and a command palette (`:`).
- **Shell integration** (PowerShell, bash, zsh, WSL): chiika knows when a command starts, ends,
  how long it took and whether it failed; a notification pops when a long command ends in
  another shell, and `Alt+PgUp` / `Alt+PgDn` jump from command to command in the history.
  Your profile and `~/.bashrc` / `~/.zshrc` are loaded as usual and never modified.
- **Canvas** (`Tab`): every shell is a mascot showing its real state; drag them into **teams**,
  each with its own instructions (`TEAM.md`).
- **Claude Code agents**: add a `lead`, `coder`, `reviewer` or `tester` to a team. Each agent
  gets the team instructions, its own git worktree, and chiika tools to talk to its teammates
  (`team`, `send_to`, `read_screen`, `post`, `board`). Answer their permission requests from the
  canvas.
- **Daemon**: shells and agents keep running when you close chiika or its window; run `chiika`
  again in the same folder to get them back, with your panes and their scrollback.

## Install

chiika is developed and tested on **Windows 10/11** with Windows Terminal. Linux and macOS
builds compile in CI but are not tested by hand yet.

### From a release

No Rust needed. In PowerShell:

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://github.com/Alphaeo/chiikaterm/releases/latest/download/chiika-installer.ps1 | iex"
```

On Linux and macOS:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/Alphaeo/chiikaterm/releases/latest/download/chiika-installer.sh | sh
```

Windows also has an `.msi` on the [releases page](https://github.com/Alphaeo/chiikaterm/releases),
with the archives for each system and their SHA-256. The binaries are not signed yet: Windows
SmartScreen asks to confirm the first run (*More info › Run anyway*).

Then [run it](#3-run-it). To build it yourself instead:

### 1. Prerequisites

- **Rust** (1.88 or later):
  ```powershell
  winget install Rustlang.Rustup
  ```
  then open a new terminal so that `cargo` is on the `PATH`.
- **Windows only, the C++ build tools** (the Rust compiler needs the MSVC linker). Install
  *Visual Studio Build Tools* with the **Desktop development with C++** workload:
  ```powershell
  winget install Microsoft.VisualStudio.2022.BuildTools --override "--wait --passive --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
  ```
- **Git** (for agents' worktrees): `winget install Git.Git`
- *Optional, for agents*: [Claude Code](https://docs.anthropic.com/en/docs/claude-code), logged
  in once (`claude`).
- *Recommended*: [Windows Terminal](https://aka.ms/terminal) and PowerShell 7
  (`winget install Microsoft.PowerShell`).

### 2. Install chiika

```powershell
cargo install --git https://github.com/Alphaeo/chiikaterm --locked
```

This builds the `chiika` command and puts it in `%USERPROFILE%\.cargo\bin`, which rustup added
to your `PATH`. Check it:

```powershell
chiika --version
```

### 3. Run it

Open Windows Terminal **in your project folder** and run:

```powershell
chiika
```

You start in a shell. `Ctrl+Q` goes to the workspace (sidebar, canvas, menus), `Enter` goes back
into the shell. Press `?` for every key, or click `≡ menu` at the bottom of the sidebar.

The first time, if Claude Code is installed, chiika offers to connect it (you can change that
later in `≡ menu › settings`).

### Update

```powershell
cargo install --git https://github.com/Alphaeo/chiikaterm --locked --force
```

If a chiika of the previous version is still running in the background for a project, the next
`chiika` there offers to restart it.

### Uninstall

```powershell
cargo uninstall chiika
```

chiika also keeps, per project, a `.chiika` folder (teams, agents' settings, the daemon's files)
and, for you, `%APPDATA%\chiika\config.toml`. Delete them if you want no trace left.

## Essentials

| Where | Keys |
|---|---|
| Shell | `Ctrl+Q` workspace · `Alt+arrows` other pane · `Shift+PgUp/PgDn` or wheel: scroll |
| Workspace | `Enter` shell · `n` new shell · `\|` / `-` split · `w` unsplit · `Tab` canvas · `:` palette · `q` quit or detach |
| Canvas | drag mascots into teams · `t` new team · right-click a team › `add agent…` |

Quitting with `q` asks whether to **detach** (everything keeps running) or **close all**.

Drag the border between two panes to resize them, and drag a shell onto a team in the sidebar
to move it there.

## Settings

`%APPDATA%\chiika\config.toml` (`~/.config/chiika/config.toml` elsewhere) keeps what you change
in chiika (theme, default shell, sidebar). Every line is optional:

```toml
theme = "chiika"            # or "midnight"
shell = "pwsh"              # shell started by `n`: pwsh, powershell, cmd, wsl (bash, zsh… elsewhere)
sidebar = "open"            # or "collapsed"
shell_integration = true

[keys]                      # keys chiika takes even inside a shell
leave = "ctrl+q"
pane_left = "alt+left"      # pane_right, pane_up, pane_down
scroll_up = "shift+pageup"  # scroll_down
prev_command = "alt+pageup" # next_command
```

A key chiika cannot read keeps its default, and the status bar says so.

## Working with agents

1. On the canvas, create a team (`[ + team ]`), then right-click it › `edit instructions` to
   write what the team is about.
2. Click `+ agent` in the team and pick a role. The agent starts in its own git worktree
   (your project must be a git repository with at least one commit, trusted once in Claude Code).
3. Double-click an agent to talk to it; ask the lead to split the work, it will message the
   others with `send_to` and keep notes on the team board.
4. When an agent needs a permission, its mascot raises a `!`: answer from its panel on the
   canvas, or in its terminal.
5. Agents remember their conversation. If they were stopped (`q › close all`, a restart of the
   computer…), the next `chiika` offers to resume them: each one comes back in its team and its
   worktree, where it left off. An agent you close yourself is not offered again.

## Governance

### Teams in the repository

`.chiika/canvas.toml` (the teams) and `.chiika/teams/*/TEAM.md` (their instructions) can be
committed: everyone who clones the project gets the same teams. chiika's local files (daemon,
agents' settings, panes, audit log) are kept out of git by `.chiika/.gitignore`.

### Policy

`.chiika/policy.toml` says what the project's agents may do (`≡ menu › open policy` creates a
commented one). Rules use [Claude Code's permission syntax](https://docs.anthropic.com/en/docs/claude-code/settings#permission-settings):

```toml
[permissions]                    # every agent of the project
deny  = ["Bash(git push:*)", "Read(./.env)", "Read(./.env.*)"]
ask   = ["Bash(rm:*)"]
allow = ["Bash(cargo test:*)"]

[roles.tester.permissions]       # one role
allow = ["Bash(cargo nextest:*)"]

[roles.security]                 # a role of your own, offered by "+ agent"
duty = "You audit the team's changes for security issues and report each one."

[teams.backend.permissions]      # one team, by name
deny = ["Edit(./frontend/**)"]
```

An agent gets the project's rules, plus its team's and its role's, in its Claude Code settings:
**Claude enforces them itself** (deny wins over ask, ask over allow), whether chiika is open or
not. A policy chiika cannot read stops agents from starting until it is fixed; a change applies to
agents started or resumed after it. Each agent's panel on the canvas shows how many rules it got.

To enforce rules on a whole machine, whatever the project says, use Claude Code's
[managed settings](https://docs.anthropic.com/en/docs/claude-code/settings#settings-files): they
take precedence over chiika's.

### Audit log

`.chiika/audit/YYYY-MM-DD.jsonl` (UTC days) gets one JSON line per event: agent started, resumed
or closed, each tool it asked for and ran (with the command or file), each permission request and
its answer (allowed or denied in chiika, sent to the terminal, expired), end of each turn. Every
line has the time (UTC), the OS user, the team, the role and the conversation id:

```json
{"time":"2026-10-25T09:30:00Z","user":"alice","event":"PreToolUse","session":3,"team":"backend","role":"coder","conversation":"…","tool":"Bash","command":"cargo test"}
```

```sh
jq -c 'select(.event | startswith("permission"))' .chiika/audit/*.jsonl   # who allowed what
```

## Build from source

```powershell
git clone https://github.com/Alphaeo/chiikaterm
cd chiikaterm
cargo run
```

`cargo test`, `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` are what CI
runs. See [CONTRIBUTING.md](CONTRIBUTING.md) to send a change, [CHANGELOG.md](CHANGELOG.md) for
what changed, and [SECURITY.md](SECURITY.md) to report a vulnerability privately.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
