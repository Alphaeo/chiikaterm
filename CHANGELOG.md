# Changelog

Notable changes to chiika, newest first. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and versions follow
[Semantic Versioning](https://semver.org/) (before 1.0, a minor version may break things).

## [Unreleased]

First public version (0.1.0).

### Added

- Terminals: real shells (PowerShell, cmd, WSL, bash, zsh…) in split, resizable panes, with
  scrollback, mouse, renaming and a command palette.
- Shell integration for PowerShell, bash, zsh and WSL: command start, end, duration and failure;
  a notification when a long command ends in another shell; jump from command to command.
- Canvas of teams, each with its instructions (`TEAM.md`), and Claude Code agents (lead, coder,
  reviewer, tester) in their own git worktree, with chiika tools to work together and their
  permission requests answered from the canvas.
- A daemon per project: shells and agents keep running when chiika is closed; panes, scrollback
  and agents' conversations come back on the next `chiika`.
- Settings in `config.toml` (theme, default shell, sidebar, keys).
- Governance: a project policy (`.chiika/policy.toml`) giving agents permission rules per project,
  team and role, and an audit log of what agents did (`.chiika/audit/`).
- `chiika --version` names the commit it was built from.
