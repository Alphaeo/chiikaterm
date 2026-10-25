//! Files chiika keeps: the user config, and per project the canvas (teams), the panes and the
//! agents.

use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::layout::Layout;

/// What the user answered when chiika offered to connect an agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentChoice {
    Connected,
    Declined,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Agents {
    /// `None` until the user has been asked.
    pub claude: Option<AgentChoice>,
}

/// Sidebar as chiika opens.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SidebarPref {
    #[default]
    Open,
    Collapsed,
}

/// Keys chiika takes even inside a shell (see `keys::Keys`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct KeysConfig {
    pub leave: String,
    pub pane_left: String,
    pub pane_right: String,
    pub pane_up: String,
    pub pane_down: String,
    pub scroll_up: String,
    pub scroll_down: String,
    pub prev_command: String,
    pub next_command: String,
}

impl Default for KeysConfig {
    fn default() -> KeysConfig {
        KeysConfig {
            leave: "ctrl+q".into(),
            pane_left: "alt+left".into(),
            pane_right: "alt+right".into(),
            pane_up: "alt+up".into(),
            pane_down: "alt+down".into(),
            scroll_up: "shift+pageup".into(),
            scroll_down: "shift+pagedown".into(),
            prev_command: "alt+pageup".into(),
            next_command: "alt+pagedown".into(),
        }
    }
}

/// `config.toml` in the user's config directory. Every field has a default, so an older or
/// partial file still loads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Theme name (`chiika`, `midnight`).
    pub theme: String,
    /// Shell started by `n`, by label (`pwsh`, `cmd`, `wsl`, `bash`…); `None` = first found.
    pub shell: Option<String>,
    pub sidebar: SidebarPref,
    /// Load chiika's shell integration (PowerShell, bash, zsh).
    pub shell_integration: bool,
    pub keys: KeysConfig,
    pub agents: Agents,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            theme: "chiika".into(),
            shell: None,
            sidebar: SidebarPref::Open,
            shell_integration: true,
            keys: KeysConfig::default(),
            agents: Agents::default(),
        }
    }
}

/// One team box of the canvas.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamEntry {
    pub id: u32,
    pub name: String,
    pub x: i32,
    pub y: i32,
    #[serde(default)]
    pub collapsed: bool,
}

/// `.chiika/canvas.toml` in the project.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanvasFile {
    #[serde(default, rename = "team")]
    pub teams: Vec<TeamEntry>,
}

/// `.chiika/layout.toml`: the panes as the TUI left them, for the next attach.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayoutFile {
    pub focused: Option<u32>,
    pub layout: Option<Layout>,
}

/// `.chiika/agents.toml`: the project's agents, to resume them once the daemon is gone.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentsFile {
    #[serde(default, rename = "agent")]
    pub agents: Vec<AgentEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentEntry {
    pub team: u32,
    /// One of `agent::ROLES`.
    pub role: String,
    /// Where it works: its worktree, or the project folder.
    pub folder: PathBuf,
    pub worktree: Option<String>,
    /// Claude Code's conversation id (`--session-id`, `--resume`).
    pub conversation: String,
}

/// Reads a TOML file; a missing file gives the default, an invalid one too (logged).
pub fn load<T: Default + DeserializeOwned>(path: &Path) -> T {
    let Ok(text) = std::fs::read_to_string(path) else {
        return T::default();
    };
    toml::from_str(&text).unwrap_or_else(|e| {
        tracing::warn!("ignoring invalid {}: {e}", path.display());
        T::default()
    })
}

pub fn save<T: Serialize>(path: &Path, value: &T) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, toml::to_string(value)?)?;
    Ok(())
}

/// `%APPDATA%\chiika\config.toml` on Windows, `$XDG_CONFIG_HOME` or `~/.config` elsewhere.
pub fn config_path() -> Option<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
    };
    Some(base?.join("chiika").join("config.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(name: &str) -> PathBuf {
        std::env::temp_dir()
            .join(format!("chiika-test-{}", std::process::id()))
            .join(name)
    }

    #[test]
    fn config_round_trips() {
        let path = temp_file("config.toml");
        let config = Config {
            theme: "midnight".into(),
            shell: Some("cmd".into()),
            sidebar: SidebarPref::Collapsed,
            agents: Agents {
                claude: Some(AgentChoice::Connected),
            },
            ..Config::default()
        };
        save(&path, &config).unwrap();
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("claude = \"connected\"")
        );
        assert_eq!(load::<Config>(&path), config);
    }

    #[test]
    fn an_older_or_partial_config_keeps_the_defaults() {
        let path = temp_file("partial.toml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "[agents]\nclaude = \"declined\"\n[keys]\nleave = \"ctrl+w\"\n",
        )
        .unwrap();
        let config: Config = load(&path);
        assert_eq!(config.agents.claude, Some(AgentChoice::Declined));
        assert_eq!(config.keys.leave, "ctrl+w");
        assert_eq!(config.keys.pane_left, "alt+left");
        assert_eq!(config.theme, "chiika");
        assert!(config.shell_integration);
    }

    #[test]
    fn canvas_round_trips() {
        let path = temp_file("canvas.toml");
        let canvas = CanvasFile {
            teams: vec![TeamEntry {
                id: 3,
                name: "backend".into(),
                x: 2,
                y: -1,
                collapsed: true,
            }],
        };
        save(&path, &canvas).unwrap();
        assert_eq!(load::<CanvasFile>(&path), canvas);
    }

    #[test]
    fn layout_and_agents_round_trip() {
        let path = temp_file("layout.toml");
        let mut layout = Layout::Pane(1);
        layout.split(1, 4, crate::layout::Split::Down);
        let file = LayoutFile {
            focused: Some(4),
            layout: Some(layout),
        };
        save(&path, &file).unwrap();
        assert_eq!(load::<LayoutFile>(&path), file);

        let path = temp_file("agents.toml");
        let file = AgentsFile {
            agents: vec![AgentEntry {
                team: 2,
                role: "coder".into(),
                folder: PathBuf::from("/p/.claude/worktrees/api-coder-3"),
                worktree: Some("api-coder-3".into()),
                conversation: "0f8e5a0c-3b8e-4d7a-9c1e-2a4b6c8d0e1f".into(),
            }],
        };
        save(&path, &file).unwrap();
        assert_eq!(load::<AgentsFile>(&path), file);
        assert_eq!(
            load::<AgentsFile>(&temp_file("none.toml")),
            AgentsFile::default()
        );
    }

    #[test]
    fn missing_or_invalid_files_give_defaults() {
        assert_eq!(load::<Config>(&temp_file("absent.toml")), Config::default());
        let path = temp_file("broken.toml");
        save(&path, &CanvasFile::default()).unwrap();
        std::fs::write(&path, "team = [[[").unwrap();
        assert_eq!(load::<CanvasFile>(&path), CanvasFile::default());
    }
}
