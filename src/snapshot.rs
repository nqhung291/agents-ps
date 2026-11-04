//! One sampled snapshot of the machine, rendered into every report view.
//!
//! Sampling is the only expensive step; the text and JSON reports and the
//! interactive TUI all read the same `Snapshot` so they can never disagree.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crate::model::{ProcInfo, ProcKey};
use crate::orphans;
use crate::report::{OrphanView, PortRow, SessionView, Totals};
use crate::rules::AgentRule;
use crate::source;
use crate::tree;

/// Extra facts about one process that only a detail pane needs.
#[derive(Clone, Debug)]
pub struct ProcDetail {
    pub ppid: i32,
    pub start_us: u64,
    /// Full argv joined with spaces (lossy UTF-8).
    pub argv: String,
    pub cwd: Option<String>,
    pub tty: Option<String>,
    /// Agent env marker KEYS only (e.g. "CLAUDECODE"); never the values (may contain secrets).
    pub env_marker_keys: Vec<String>,
}

impl ProcDetail {
    fn of(p: &ProcInfo) -> Self {
        ProcDetail {
            ppid: p.ppid,
            start_us: p.key.start_us,
            argv: if p.cmd.is_empty() {
                p.name.clone()
            } else {
                p.cmd.join(" ")
            },
            cwd: p
                .cwd
                .as_deref()
                .map(|dir| dir.to_string_lossy().into_owned()),
            tty: p.tty.clone(),
            env_marker_keys: marker_keys(&p.env_markers),
        }
    }
}

/// The `KEY` half of each `KEY=VALUE` marker, first occurrence wins.
fn marker_keys(markers: &[String]) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    for entry in markers {
        let key = entry.split_once('=').map_or(entry.as_str(), |(key, _)| key);
        if !keys.iter().any(|seen| seen == key) {
            keys.push(key.to_string());
        }
    }
    keys
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub sessions: Vec<SessionView>,
    pub totals: Totals,
    pub orphans: Vec<OrphanView>,
    pub ports: Vec<PortRow>,
    /// One entry per pid appearing in any session's `processes` or in `orphans`.
    pub details: HashMap<i32, ProcDetail>,
    pub sample_ms: u64,
    pub now_us: u64,
}

impl Snapshot {
    /// Samples once (blocks ~300 ms) and builds every view.
    pub fn collect(sampler: &mut source::Sampler, rules: &[AgentRule]) -> Snapshot {
        let sample = sampler.sample();
        let mut procs = sample.procs;
        let now_us = sample.now_us;

        let mut trees = tree::build_live_trees(&procs, rules);
        let member_keys: HashSet<ProcKey> = trees
            .iter()
            .flat_map(|t| t.members.iter().copied())
            .collect();

        // SAFETY: getuid takes no arguments, cannot fail, and has no side effects.
        let own_uid = unsafe { libc::getuid() };
        let live_pids: HashSet<i32> = procs.iter().map(|p| p.key.pid).collect();

        // Probe ports for tree members and for launchd-adopted candidates only: one
        // extra syscall per pid instead of one per process on the machine.
        let mut probe: HashSet<i32> = member_keys.iter().map(|k| k.pid).collect();
        for p in &procs {
            let parent_gone = p.ppid == 1 || !live_pids.contains(&p.ppid);
            if parent_gone && p.uid == Some(own_uid) && !member_keys.contains(&p.key) {
                probe.insert(p.key.pid);
            }
        }
        source::fill_ports(&mut procs, &probe);

        for tree in &mut trees {
            tree::recompute_aggregates(tree, &procs);
        }

        let home = std::env::var_os("HOME").map(PathBuf::from);
        let candidates = orphans::classify(&procs, rules, &trees, own_uid, home.as_deref());

        let mut by_key: HashMap<ProcKey, &ProcInfo> = HashMap::new();
        let mut by_pid: HashMap<i32, &ProcInfo> = HashMap::new();
        for p in &procs {
            by_key.insert(p.key, p);
            by_pid.insert(p.key.pid, p);
        }

        let mut sessions: Vec<SessionView> = trees
            .iter()
            .map(|t| SessionView::build(t, &by_key, now_us))
            .collect();
        sessions.sort_by(|a, b| {
            b.rss_bytes
                .cmp(&a.rss_bytes)
                .then_with(|| a.pid.cmp(&b.pid))
        });
        let totals = Totals::of(&sessions);

        let mut orphans: Vec<OrphanView> = candidates
            .iter()
            .filter_map(|c| OrphanView::build(c, &by_key, now_us))
            .collect();
        orphans.sort_by(|a, b| {
            b.rss_bytes
                .cmp(&a.rss_bytes)
                .then_with(|| a.pid.cmp(&b.pid))
        });

        let mut ports: Vec<PortRow> = Vec::new();
        for tree in &trees {
            for key in &tree.members {
                let Some(p) = by_key.get(key) else { continue };
                for port in &p.ports {
                    ports.push(PortRow {
                        port: *port,
                        pid: p.key.pid,
                        cmd: p.short_cmd(),
                        agent_id: Some(tree.agent_id.clone()),
                        agent_name: Some(tree.agent_name.clone()),
                        kind: "session".to_string(),
                        session_pid: Some(tree.root.pid),
                    });
                }
            }
        }
        for candidate in &candidates {
            let Some(p) = by_key.get(&candidate.key) else {
                continue;
            };
            for port in &p.ports {
                ports.push(PortRow {
                    port: *port,
                    pid: p.key.pid,
                    cmd: p.short_cmd(),
                    agent_id: candidate.agent_id.clone(),
                    agent_name: candidate.agent_name.clone(),
                    kind: "orphan".to_string(),
                    session_pid: None,
                });
            }
        }
        ports.sort_by(|a, b| {
            (a.kind != "session")
                .cmp(&(b.kind != "session"))
                .then_with(|| a.port.cmp(&b.port))
                .then_with(|| a.pid.cmp(&b.pid))
        });

        let mut details: HashMap<i32, ProcDetail> = HashMap::new();
        for session in &sessions {
            for row in &session.processes {
                if let Some(p) = by_pid.get(&row.pid) {
                    details.entry(row.pid).or_insert_with(|| ProcDetail::of(p));
                }
            }
        }
        for orphan in &orphans {
            if let Some(p) = by_pid.get(&orphan.pid) {
                details
                    .entry(orphan.pid)
                    .or_insert_with(|| ProcDetail::of(p));
            }
        }

        Snapshot {
            sessions,
            totals,
            orphans,
            ports,
            details,
            sample_ms: sample.sample_ms,
            now_us,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(cmd: &[&str], name: &str) -> ProcInfo {
        ProcInfo {
            name: name.to_string(),
            cmd: cmd.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn argv_falls_back_to_the_name() {
        assert_eq!(ProcDetail::of(&info(&[], "node")).argv, "node");
        assert_eq!(
            ProcDetail::of(&info(&["node", "server.js"], "node")).argv,
            "node server.js"
        );
    }

    #[test]
    fn marker_keys_are_deduped_and_never_carry_values() {
        let markers = vec![
            "CLAUDECODE=1".to_string(),
            "CLAUDE_CODE_SSE_PORT=41210".to_string(),
            "CLAUDECODE=1".to_string(),
        ];
        assert_eq!(
            marker_keys(&markers),
            vec!["CLAUDECODE", "CLAUDE_CODE_SSE_PORT"]
        );
    }
}
