//! Agent rules: decide whether a process is an agent root candidate, and read
//! the environment markers an agent leaves on the processes it spawns.
//!
//! Ported from `aipm-core/src/rules.rs` (same author), minus the desktop-only
//! "mark" rules that pinned an arbitrary binary to a user label.

use crate::model::ProcInfo;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentRule {
    pub id: &'static str,
    pub name: &'static str,
    /// Match against the basename of `exe` or the basename of `cmd[0]`.
    pub exe_names: &'static [&'static str],
    /// Match when any argv element contains the substring.
    pub cmd_contains: &'static [&'static str],
    /// `KEY=VALUE` pairs an agent exports; inherited by every process it spawns.
    pub env_markers: &'static [&'static str],
    /// `KEY_PREFIX` markers, matched against `KEY=` at the start of an env entry.
    pub env_prefixes: &'static [&'static str],
}

impl AgentRule {
    pub fn matches(&self, p: &ProcInfo) -> bool {
        let exe_file = p.exe.as_deref().and_then(|e| e.file_name());
        let cmd0_file = p
            .cmd
            .first()
            .map(|c| std::path::Path::new(c.as_str()))
            .and_then(|c| c.file_name());
        let name_hit = self.exe_names.iter().any(|want| {
            exe_file.is_some_and(|n| n.to_string_lossy() == *want)
                || cmd0_file.is_some_and(|n| n.to_string_lossy() == *want)
        });
        name_hit
            || self
                .cmd_contains
                .iter()
                .any(|sub| p.cmd.iter().any(|arg| arg.contains(sub)))
    }

    /// True when one of the process's marker env entries belongs to this agent.
    pub fn env_matches(&self, p: &ProcInfo) -> bool {
        p.env_markers.iter().any(|entry| {
            self.env_markers.iter().any(|m| m == entry)
                || self
                    .env_prefixes
                    .iter()
                    .any(|prefix| entry.starts_with(prefix))
        })
    }
}

/// Agent rules in match-precedence order. First match wins.
pub fn builtin_rules() -> Vec<AgentRule> {
    vec![
        AgentRule {
            id: "claude",
            name: "Claude Code",
            exe_names: &["claude"],
            cmd_contains: &["@anthropic-ai/claude-code", "claude-code/cli.js"],
            env_markers: &["CLAUDECODE=1"],
            env_prefixes: &["CLAUDE_CODE_"],
        },
        AgentRule {
            id: "codex",
            name: "Codex CLI",
            exe_names: &["codex"],
            cmd_contains: &["@openai/codex"],
            env_markers: &[],
            env_prefixes: &["CODEX_SANDBOX", "CODEX_HOME="],
        },
        AgentRule {
            id: "cursor-agent",
            name: "Cursor Agent",
            exe_names: &["cursor-agent"],
            cmd_contains: &["cursor-agent/index.js"],
            env_markers: &[],
            env_prefixes: &["CURSOR_TRACE_ID="],
        },
        AgentRule {
            id: "aider",
            name: "Aider",
            exe_names: &["aider"],
            cmd_contains: &["/bin/aider"],
            env_markers: &[],
            env_prefixes: &["AIDER_"],
        },
        AgentRule {
            id: "gemini",
            name: "Gemini CLI",
            exe_names: &["gemini"],
            cmd_contains: &["@google/gemini-cli"],
            env_markers: &[],
            env_prefixes: &["GEMINI_CLI"],
        },
        AgentRule {
            id: "opencode",
            name: "opencode",
            exe_names: &["opencode"],
            cmd_contains: &[],
            env_markers: &[],
            env_prefixes: &["OPENCODE_"],
        },
        AgentRule {
            id: "goose",
            name: "Goose",
            exe_names: &["goose"],
            cmd_contains: &[],
            env_markers: &[],
            env_prefixes: &["GOOSE_"],
        },
        AgentRule {
            id: "amp",
            name: "Amp",
            exe_names: &["amp"],
            cmd_contains: &["@sourcegraph/amp"],
            env_markers: &[],
            env_prefixes: &[],
        },
        AgentRule {
            id: "omp",
            name: "omp",
            exe_names: &["omp"],
            cmd_contains: &[],
            env_markers: &[],
            env_prefixes: &["OMP_"],
        },
        AgentRule {
            id: "ollama",
            name: "Ollama",
            exe_names: &["ollama"],
            cmd_contains: &[],
            env_markers: &[],
            env_prefixes: &["OLLAMA_"],
        },
    ]
}

/// First matching rule, or `None`.
pub fn match_rule<'a>(rules: &'a [AgentRule], p: &ProcInfo) -> Option<&'a AgentRule> {
    rules.iter().find(|r| r.matches(p))
}

/// Env keys worth reading from every process; everything else is discarded at sample time.
pub fn marker_keys(rules: &[AgentRule]) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    for rule in rules {
        for marker in rule.env_markers {
            if let Some((key, _)) = marker.split_once('=') {
                keys.push(key.to_string());
            }
        }
        for prefix in rule.env_prefixes {
            keys.push(prefix.trim_end_matches('=').to_string());
        }
    }
    keys.sort();
    keys.dedup();
    keys
}

/// Build a synthetic process row for tests.
#[cfg(test)]
pub(crate) fn test_proc(pid: i32, ppid: i32, cmd: &[&str]) -> ProcInfo {
    ProcInfo {
        key: crate::model::ProcKey {
            pid,
            start_us: pid as u64 * 1000,
        },
        ppid,
        cmd: cmd.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_by_script() {
        let p = test_proc(
            100,
            1,
            &[
                "node",
                "/opt/homebrew/lib/node_modules/@anthropic-ai/claude-code/cli.js",
            ],
        );
        assert_eq!(match_rule(&builtin_rules(), &p).unwrap().id, "claude");
    }

    #[test]
    fn codex_by_exe_name() {
        let p = ProcInfo {
            exe: Some("/usr/local/bin/codex".into()),
            ..test_proc(101, 1, &["codex"])
        };
        assert_eq!(match_rule(&builtin_rules(), &p).unwrap().id, "codex");
    }

    #[test]
    fn plain_node_matches_nothing() {
        let p = test_proc(102, 1, &["node", "server.js"]);
        assert!(match_rule(&builtin_rules(), &p).is_none());
    }

    #[test]
    fn shell_mentioning_agent_is_not_a_root() {
        let p = test_proc(103, 1, &["zsh", "-c", "npx @openai/codex"]);
        // argv contains the package string in a wrapper shell: still a match, by design.
        assert!(match_rule(&builtin_rules(), &p).is_some());
    }

    #[test]
    fn env_marker_attribution() {
        let rules = builtin_rules();
        let mut p = test_proc(104, 1, &["node", "worker.js"]);
        p.env_markers = vec!["CLAUDECODE=1".into()];
        let rule = rules.iter().find(|r| r.env_matches(&p)).unwrap();
        assert_eq!(rule.id, "claude");

        let mut p = test_proc(105, 1, &["node", "worker.js"]);
        p.env_markers = vec!["PATH=/usr/bin".into()];
        assert!(rules.iter().all(|r| !r.env_matches(&p)));
    }

    #[test]
    fn marker_keys_include_prefixes_without_equals() {
        let keys = marker_keys(&builtin_rules());
        assert!(keys.contains(&"CLAUDECODE".to_string()));
        assert!(keys.contains(&"CLAUDE_CODE_".to_string()));
        assert!(keys.contains(&"CODEX_HOME".to_string()));
        assert_eq!(
            keys.len(),
            keys.iter().collect::<std::collections::HashSet<_>>().len()
        );
    }
}
