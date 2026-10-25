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
  again in the same folder to get them back.

## Install

chiika is developed and tested on **Windows 10/11** with Windows Terminal. Linux and macOS
builds compile in CI but are not tested by hand yet.

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

## Build from source

```powershell
git clone https://github.com/Alphaeo/chiikaterm
cd chiikaterm
cargo run
```

`cargo test`, `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` are what CI
runs.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
