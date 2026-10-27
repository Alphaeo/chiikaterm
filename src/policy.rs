//! The project's policy for its agents, `.chiika/policy.toml`: permission rules for the whole
//! project, a team or a role, and roles of its own. chiika writes the rules into each agent's
//! Claude Code settings, and Claude enforces them (deny before ask before allow).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::agent::ROLES;

/// Written by `≡ menu › open policy` when the project has none.
pub const TEMPLATE: &str = r#"# chiika policy: what this project's agents may do. Commit it with the project.
# Rules use Claude Code's permission syntax: Bash(cargo test:*), Read(./.env), Edit(./docs/**)…
# Claude applies them itself: deny wins over ask, ask over allow. They apply to agents started
# or resumed after a change.

[permissions]          # every agent of the project
deny = ["Bash(git push:*)", "Read(./.env)", "Read(./.env.*)"]
ask = []
allow = []

# [roles.tester.permissions]   # one role (lead, coder, reviewer, tester, or one of yours)
# allow = ["Bash(cargo test:*)"]

# [roles.security]             # a role of your own, offered by "+ agent"
# duty = "You audit the team's changes for security issues and report each one."

# [teams.backend.permissions]  # one team, by name
# deny = ["Edit(./frontend/**)"]
"#;

/// Permission rules, in Claude Code's syntax.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Rules {
    pub allow: Vec<String>,
    pub ask: Vec<String>,
    pub deny: Vec<String>,
}

impl Rules {
    pub fn len(&self) -> usize {
        self.allow.len() + self.ask.len() + self.deny.len()
    }

    /// Adds `other`'s rules that are not there yet.
    fn extend(&mut self, other: &Rules) {
        for (mine, theirs) in [
            (&mut self.allow, &other.allow),
            (&mut self.ask, &other.ask),
            (&mut self.deny, &other.deny),
        ] {
            for rule in theirs {
                if !mine.contains(rule) {
                    mine.push(rule.clone());
                }
            }
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RolePolicy {
    /// What the role does, in its system prompt; replaces a built-in role's.
    duty: Option<String>,
    permissions: Rules,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct TeamPolicy {
    permissions: Rules,
}

/// `.chiika/policy.toml`; a project without one has the empty policy. Unknown fields are
/// errors: a misspelt `dney` must not silently allow everything.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Policy {
    permissions: Rules,
    roles: BTreeMap<String, RolePolicy>,
    teams: BTreeMap<String, TeamPolicy>,
}

pub fn file(data_dir: &Path) -> PathBuf {
    data_dir.join("policy.toml")
}

impl Policy {
    /// The policy of the project whose data is in `data_dir`. An unreadable file is an error,
    /// never the empty policy: agents must not start without the rules their team wrote.
    pub fn load(data_dir: &Path) -> Result<Policy, String> {
        let path = file(data_dir);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Policy::default()),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// The rules of an agent: the project's, its team's and its role's.
    pub fn rules(&self, team: &str, role: &str) -> Rules {
        let mut rules = self.permissions.clone();
        if let Some(team) = self.teams.get(team) {
            rules.extend(&team.permissions);
        }
        if let Some(role) = self.roles.get(role) {
            rules.extend(&role.permissions);
        }
        rules
    }

    /// The roles "+ agent" offers: the built-in ones, then the project's own.
    pub fn roles(&self) -> Vec<String> {
        let builtin = ROLES.iter().map(|(name, _)| name.to_string());
        let own = self
            .roles
            .iter()
            .filter(|(name, role)| role.duty.is_some() && !ROLES.iter().any(|(r, _)| r == name))
            .map(|(name, _)| name.clone());
        builtin.chain(own).collect()
    }

    /// What `role` does, for its system prompt.
    pub fn duty(&self, role: &str) -> String {
        self.roles
            .get(role)
            .and_then(|r| r.duty.clone())
            .or_else(|| {
                ROLES
                    .iter()
                    .find(|(name, _)| *name == role)
                    .map(|(_, duty)| duty.to_string())
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("chiika-policy-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn parse(text: &str) -> Policy {
        toml::from_str(text).unwrap()
    }

    #[test]
    fn no_file_is_the_empty_policy() {
        let policy = Policy::load(&dir("none")).unwrap();
        assert_eq!(policy, Policy::default());
        assert_eq!(policy.rules("t", "coder").len(), 0);
        assert_eq!(policy.roles(), ["lead", "coder", "reviewer", "tester"]);
    }

    #[test]
    fn project_team_and_role_rules_add_up_once() {
        let policy = parse(
            r#"
            [permissions]
            deny = ["Bash(git push:*)"]
            allow = ["Bash(cargo test:*)"]
            [teams.backend.permissions]
            deny = ["Edit(./frontend/**)", "Bash(git push:*)"]
            [roles.tester.permissions]
            allow = ["Bash(cargo nextest:*)"]
            "#,
        );
        let rules = policy.rules("backend", "tester");
        assert_eq!(rules.deny, ["Bash(git push:*)", "Edit(./frontend/**)"]);
        assert_eq!(rules.allow, ["Bash(cargo test:*)", "Bash(cargo nextest:*)"]);
        assert_eq!(rules.len(), 4);
        let other = policy.rules("frontend", "coder");
        assert_eq!(other.deny, ["Bash(git push:*)"]);
        assert_eq!(other.len(), 2);
    }

    #[test]
    fn roles_of_the_project_are_offered_with_their_duty() {
        let policy = parse(
            r#"
            [roles.security]
            duty = "Audit."
            [roles.coder]
            duty = "Code in Rust."
            [roles.tester.permissions]
            allow = ["Bash(cargo test:*)"]
            "#,
        );
        assert_eq!(
            policy.roles(),
            ["lead", "coder", "reviewer", "tester", "security"]
        );
        assert_eq!(policy.duty("security"), "Audit.");
        assert_eq!(policy.duty("coder"), "Code in Rust.");
        assert!(policy.duty("tester").starts_with("You are the tester"));
        assert_eq!(policy.duty("nobody"), "");
    }

    #[test]
    fn a_broken_or_misspelt_policy_is_an_error() {
        let broken = dir("broken");
        std::fs::write(file(&broken), "[permissions\n").unwrap();
        let error = Policy::load(&broken).unwrap_err();
        assert!(error.contains("policy.toml"), "{error}");

        let misspelt = dir("misspelt");
        std::fs::write(file(&misspelt), "[permissions]\ndney = [\"Bash\"]\n").unwrap();
        let error = Policy::load(&misspelt).unwrap_err();
        assert!(error.contains("dney"), "{error}");
    }

    #[test]
    fn the_template_is_a_valid_policy() {
        let policy = parse(TEMPLATE);
        assert!(
            policy
                .rules("t", "coder")
                .deny
                .contains(&"Read(./.env)".to_string())
        );
    }
}
