//! Orphan classification: processes adopted by launchd that an agent left behind.
//!
//! `aipm-core` tracks orphans across ticks with a ledger: an orphan is a
//! remembered member whose root is gone. A one-shot CLI has no history, so it
//! uses the same rules and the same definition of an orphan (a process that lost
//! its parent, belongs to no live agent tree, and is owned by the invoking user)
//! and adds the evidence an agent leaves on its children:
//!
//! 1. **Environment marker** — agents export `CLAUDECODE=1`, `CURSOR_TRACE_ID`,
//!    `CODEX_*`, … and children inherit them. This names the agent outright.
//! 2. **Project match** — a dev-runtime process or port listener whose cwd is
//!    the cwd of a live agent session: same project, likely the same agent.
//! 3. **Bare heuristic** — a dev-runtime process or port listener adopted by
//!    launchd under the user's home with no better evidence.
//!
//! System daemons are excluded, and nothing here can act on a process.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::model::{ProcInfo, ProcKey};
use crate::rules::AgentRule;
use crate::tree::LiveTree;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Confidence {
    /// The process carries an environment marker an agent exported.
    High,
    /// The process runs in the cwd of a live agent session.
    Medium,
    /// The process looks like a leftover dev runtime, but nothing ties it to an agent.
    Low,
}

impl Confidence {
    pub fn label(self) -> &'static str {
        match self {
            Confidence::High => "env marker",
            Confidence::Medium => "same project",
            Confidence::Low => "heuristic",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrphanCandidate {
    pub key: ProcKey,
    pub agent_id: Option<String>,
    pub agent_name: Option<String>,
    pub confidence: Confidence,
}

/// Executables agents commonly leave running: dev servers, watchers, build tools.
const DEV_RUNTIMES: &[&str] = &[
    "node",
    "bun",
    "deno",
    "python",
    "python3",
    "python3.11",
    "python3.12",
    "python3.13",
    "uv",
    "uvicorn",
    "gunicorn",
    "vite",
    "esbuild",
    "webpack",
    "next",
    "jest",
    "vitest",
    "pytest",
    "tsc",
    "tsx",
    "npm",
    "pnpm",
    "yarn",
    "ruby",
    "rails",
    "php",
    "java",
    "go",
    "cargo",
    "ollama",
    "mlx_lm",
    "llama-server",
];

/// Paths whose processes are macOS machinery, never an agent's leftovers.
const SYSTEM_PREFIXES: &[&str] = &[
    "/System/",
    "/usr/libexec/",
    "/usr/sbin/",
    "/usr/lib/",
    "/sbin/",
    "/Library/Apple/",
    "/Applications/",
];

fn is_system(exe: Option<&Path>) -> bool {
    exe.and_then(Path::to_str).is_some_and(|path| {
        SYSTEM_PREFIXES
            .iter()
            .any(|prefix| path.starts_with(prefix))
    })
}

fn is_dev_runtime(p: &ProcInfo) -> bool {
    let label = p.label();
    DEV_RUNTIMES.contains(&label.as_str())
}

/// Classify orphans from one snapshot.
///
/// A candidate must be owned by `own_uid`, must not be part of a live tree, and
/// must have lost its parent: `ppid == 1` means launchd adopted it, and a ppid
/// that is no longer in the table means the parent is gone.
pub fn classify(
    procs: &[ProcInfo],
    rules: &[AgentRule],
    trees: &[LiveTree],
    own_uid: u32,
    home: Option<&Path>,
) -> Vec<OrphanCandidate> {
    let by_key: HashMap<ProcKey, &ProcInfo> = procs.iter().map(|p| (p.key, p)).collect();
    let attributed: HashSet<ProcKey> = trees
        .iter()
        .flat_map(|t| t.members.iter().copied())
        .collect();
    let live_pids: HashSet<i32> = procs.iter().map(|p| p.key.pid).collect();

    // cwd -> (agent id, agent name), from the live session roots.
    let mut project_agents: HashMap<&Path, (&str, &str)> = HashMap::new();
    for tree in trees {
        if let Some(cwd) = by_key.get(&tree.root).and_then(|p| p.cwd.as_deref()) {
            project_agents
                .entry(cwd)
                .or_insert((&tree.agent_id, &tree.agent_name));
        }
    }

    let mut out: Vec<OrphanCandidate> = Vec::new();
    for p in procs {
        if p.key.pid <= 1 || attributed.contains(&p.key) || p.uid != Some(own_uid) {
            continue;
        }
        let parent_gone = p.ppid == 1 || !live_pids.contains(&p.ppid);
        if !parent_gone {
            continue;
        }
        if is_system(p.exe.as_deref()) {
            continue;
        }

        // 1. Inherited environment: the strongest evidence there is.
        if let Some(rule) = rules.iter().find(|r| r.env_matches(p)) {
            out.push(OrphanCandidate {
                key: p.key,
                agent_id: Some(rule.id.to_string()),
                agent_name: Some(rule.name.to_string()),
                confidence: Confidence::High,
            });
            continue;
        }

        // 2/3. A dev runtime or a port listener under the user's home.
        let under_home = match (home, p.cwd.as_deref()) {
            (Some(home), Some(cwd)) => cwd.starts_with(home),
            _ => false,
        };
        let interesting = is_dev_runtime(p) || !p.ports.is_empty();
        if !under_home || !interesting {
            continue;
        }
        let project = p.cwd.as_deref().and_then(|cwd| project_agents.get(cwd));
        out.push(OrphanCandidate {
            key: p.key,
            agent_id: project.map(|(id, _)| (*id).to_string()),
            agent_name: project.map(|(_, name)| (*name).to_string()),
            confidence: if project.is_some() {
                Confidence::Medium
            } else {
                Confidence::Low
            },
        });
    }

    out.sort_by(|a, b| a.key.pid.cmp(&b.key.pid));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::{builtin_rules, test_proc};
    use crate::tree::build_live_trees;
    use std::path::PathBuf;

    const ME: u32 = 501;
    const HOME: &str = "/Users/me";

    fn home() -> Option<&'static Path> {
        Some(Path::new(HOME))
    }

    fn owned(mut p: ProcInfo) -> ProcInfo {
        p.uid = Some(ME);
        p
    }

    fn in_dir(mut p: ProcInfo, dir: &str) -> ProcInfo {
        p.cwd = Some(PathBuf::from(dir));
        p
    }

    /// root -> bash -> node: the classic session shape.
    fn session() -> Vec<ProcInfo> {
        vec![
            in_dir(
                owned(test_proc(
                    10,
                    1,
                    &[
                        "node",
                        "/opt/homebrew/lib/node_modules/@anthropic-ai/claude-code/cli.js",
                    ],
                )),
                "/Users/me/proj",
            ),
            owned(test_proc(11, 10, &["bash"])),
            owned(test_proc(12, 11, &["node", "server.js"])),
        ]
    }

    fn classify_all(procs: &[ProcInfo]) -> Vec<OrphanCandidate> {
        let rules = builtin_rules();
        let trees = build_live_trees(procs, &rules);
        classify(procs, &rules, &trees, ME, home())
    }

    #[test]
    fn env_marker_names_the_agent() {
        let mut procs = session();
        let mut orphan = owned(test_proc(90, 1, &["node", "worker.js"]));
        orphan.env_markers = vec!["CLAUDECODE=1".into()];
        procs.push(in_dir(orphan, "/Users/me/other"));
        let out = classify_all(&procs);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].key.pid, 90);
        assert_eq!(out[0].agent_id.as_deref(), Some("claude"));
        assert_eq!(out[0].confidence, Confidence::High);
    }

    #[test]
    fn same_project_dev_server_is_medium() {
        let mut procs = session();
        procs.push(owned(test_proc(20, 1, &["node", "vite"])));
        procs[3] = in_dir(procs[3].clone(), "/Users/me/proj");
        let out = classify_all(&procs);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].agent_id.as_deref(), Some("claude"));
        assert_eq!(out[0].confidence, Confidence::Medium);
    }

    #[test]
    fn bare_dev_server_is_low_confidence() {
        let mut procs = session();
        procs.push(in_dir(
            owned(test_proc(21, 1, &["python3", "manage.py", "runserver"])),
            "/Users/me/elsewhere",
        ));
        let out = classify_all(&procs);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].agent_id, None);
        assert_eq!(out[0].confidence, Confidence::Low);
    }

    #[test]
    fn system_daemons_are_ignored() {
        let procs = vec![
            ProcInfo {
                exe: Some("/usr/libexec/containermanagerd".into()),
                ..owned(test_proc(30, 1, &["containermanagerd", "--runmode=agent"]))
            },
            ProcInfo {
                exe: Some("/System/Library/Frameworks/Metal.framework/MTLCompilerService".into()),
                ..in_dir(owned(test_proc(31, 1, &["MTLCompilerService"])), HOME)
            },
        ];
        assert!(classify_all(&procs).is_empty());
    }

    #[test]
    fn non_dev_process_without_evidence_is_ignored() {
        let procs = vec![in_dir(
            owned(test_proc(32, 1, &["my-custom-daemon"])),
            "/Users/me/bin",
        )];
        assert!(classify_all(&procs).is_empty());
    }

    #[test]
    fn dev_server_outside_home_is_ignored() {
        let procs = vec![in_dir(
            owned(test_proc(33, 1, &["node", "server.js"])),
            "/opt/builds/site",
        )];
        assert!(classify_all(&procs).is_empty());
    }

    #[test]
    fn other_users_processes_are_ignored() {
        let mut p = in_dir(test_proc(34, 1, &["node", "server.js"]), HOME);
        p.uid = Some(0);
        assert!(classify_all(&[p]).is_empty());
    }

    #[test]
    fn live_tree_members_are_not_orphans() {
        let procs = session();
        assert!(classify_all(&procs).is_empty());
    }

    #[test]
    fn parent_still_alive_is_not_an_orphan() {
        let procs = vec![
            owned(test_proc(40, 1, &["zsh"])),
            in_dir(owned(test_proc(41, 40, &["node", "vite"])), HOME),
        ];
        assert!(classify_all(&procs).is_empty());
    }
}
