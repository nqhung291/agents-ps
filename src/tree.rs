//! Live agent trees: group processes under the topmost matching agent root.
//! Ported from `aipm-core/src/tree.rs`, without the desktop app's sticky roots.

use std::collections::{HashMap, HashSet};

use crate::model::{ProcInfo, ProcKey};
use crate::rules::{AgentRule, match_rule};

/// One agent session: its root process and everything below it.
#[derive(Clone, Debug, Default)]
pub struct LiveTree {
    pub agent_id: String,
    pub agent_name: String,
    pub root: ProcKey,
    /// Depth-first preorder, ancestors before descendants.
    pub members: Vec<ProcKey>,
    /// Same length as `members`.
    pub depths: Vec<u16>,
    pub cpu_pct: f32,
    pub rss_bytes: u64,
    /// Union of member ports, sorted and deduped.
    pub ports: Vec<u16>,
}

/// Walk the ppid chain above `start` and report whether a root candidate is
/// among its ancestors, plus how many hops were taken.
fn ancestor_scan<F>(
    procs: &[ProcInfo],
    by_pid: &HashMap<i32, usize>,
    start: &ProcInfo,
    is_candidate: F,
) -> (bool, usize)
where
    F: Fn(&ProcInfo) -> bool,
{
    let mut visited: HashSet<i32> = HashSet::new();
    visited.insert(start.key.pid);
    let mut depth = 0usize;
    let mut ppid = start.ppid;
    loop {
        if ppid <= 1 {
            return (false, depth);
        }
        // Cycle guard: a corrupted or racing ppid chain must not loop forever.
        if !visited.insert(ppid) {
            return (false, depth);
        }
        let Some(&idx) = by_pid.get(&ppid) else {
            return (false, depth);
        };
        let ancestor = &procs[idx];
        depth += 1;
        if is_candidate(ancestor) {
            return (true, depth);
        }
        if ancestor.key.pid == ancestor.ppid {
            return (false, depth);
        }
        ppid = ancestor.ppid;
    }
}

/// Build one tree per agent root.
///
/// A root is a process matching a rule that has no ancestor which is itself a
/// rule match, so `claude` spawned by `omp` stays inside omp's tree.
pub fn build_live_trees(procs: &[ProcInfo], rules: &[AgentRule]) -> Vec<LiveTree> {
    let mut children: HashMap<i32, Vec<usize>> = HashMap::new();
    let mut by_pid: HashMap<i32, usize> = HashMap::new();
    for (idx, p) in procs.iter().enumerate() {
        by_pid.insert(p.key.pid, idx);
        children.entry(p.ppid).or_default().push(idx);
    }
    for kids in children.values_mut() {
        kids.sort_by_key(|&i| procs[i].key.pid);
    }

    let is_candidate = |p: &ProcInfo| match_rule(rules, p).is_some();

    // (proc index, ancestor hops)
    let mut roots: Vec<(usize, usize)> = Vec::new();
    for (idx, p) in procs.iter().enumerate() {
        if match_rule(rules, p).is_none() {
            continue;
        }
        let (has_candidate_ancestor, depth) = ancestor_scan(procs, &by_pid, p, is_candidate);
        if !has_candidate_ancestor {
            roots.push((idx, depth));
        }
    }

    // Shallow roots first so an ancestor root claims its subtree before a
    // deeper root can start a second tree inside it.
    roots.sort_by(|a, b| {
        a.1.cmp(&b.1)
            .then_with(|| procs[a.0].key.pid.cmp(&procs[b.0].key.pid))
    });

    let mut visited: HashSet<i32> = HashSet::new();
    let mut trees: Vec<LiveTree> = Vec::new();
    for (root_idx, _) in roots {
        if visited.contains(&procs[root_idx].key.pid) {
            continue;
        }
        let rule = match_rule(rules, &procs[root_idx]).expect("root matched a rule");
        let mut members: Vec<ProcKey> = Vec::new();
        let mut depths: Vec<u16> = Vec::new();
        let mut stack: Vec<(usize, u16)> = vec![(root_idx, 0)];
        while let Some((idx, depth)) = stack.pop() {
            let pid = procs[idx].key.pid;
            if !visited.insert(pid) {
                continue;
            }
            members.push(procs[idx].key);
            depths.push(depth);
            if let Some(kids) = children.get(&pid) {
                for &kid in kids.iter().rev() {
                    stack.push((kid, depth.saturating_add(1)));
                }
            }
        }

        let mut tree = LiveTree {
            agent_id: rule.id.to_string(),
            agent_name: rule.name.to_string(),
            root: procs[root_idx].key,
            members,
            depths,
            ..Default::default()
        };
        recompute_aggregates(&mut tree, procs);
        trees.push(tree);
    }

    trees.sort_by(|a, b| {
        a.agent_id
            .cmp(&b.agent_id)
            .then_with(|| a.root.pid.cmp(&b.root.pid))
    });
    trees
}

/// Sum CPU and RSS over the members and take the sorted, deduped port union.
pub fn recompute_aggregates(tree: &mut LiveTree, procs: &[ProcInfo]) {
    let by_key: HashMap<ProcKey, &ProcInfo> = procs.iter().map(|p| (p.key, p)).collect();
    let mut cpu_pct = 0.0f32;
    let mut rss_bytes = 0u64;
    let mut ports: Vec<u16> = Vec::new();
    for key in &tree.members {
        let Some(p) = by_key.get(key) else {
            continue;
        };
        cpu_pct += p.cpu_pct;
        rss_bytes = rss_bytes.saturating_add(p.rss_bytes);
        ports.extend_from_slice(&p.ports);
    }
    ports.sort_unstable();
    ports.dedup();
    tree.cpu_pct = cpu_pct;
    tree.rss_bytes = rss_bytes;
    tree.ports = ports;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::{builtin_rules, test_proc};

    fn claude_cmd() -> &'static [&'static str] {
        &[
            "node",
            "/opt/homebrew/lib/node_modules/@anthropic-ai/claude-code/cli.js",
        ]
    }

    #[test]
    fn nested_processes_form_one_tree() {
        let procs = vec![
            test_proc(10, 1, claude_cmd()),
            test_proc(11, 10, &["bash"]),
            test_proc(12, 11, &["node", "server.js"]),
            test_proc(13, 12, &["node", "worker.js"]),
        ];
        let trees = build_live_trees(&procs, &builtin_rules());
        assert_eq!(trees.len(), 1);
        assert_eq!(trees[0].agent_id, "claude");
        assert_eq!(trees[0].members.len(), 4);
        assert_eq!(trees[0].depths, vec![0, 1, 2, 3]);
        assert_eq!(trees[0].members[0], procs[0].key);
    }

    #[test]
    fn two_roots_stay_separate() {
        let procs = vec![test_proc(20, 1, &["codex"]), test_proc(30, 1, claude_cmd())];
        let trees = build_live_trees(&procs, &builtin_rules());
        assert_eq!(trees.len(), 2);
        assert_eq!(trees[0].agent_id, "claude");
        assert_eq!(trees[1].agent_id, "codex");
        assert_eq!(trees[0].members.len(), 1);
        assert_eq!(trees[1].members.len(), 1);
    }

    #[test]
    fn agent_inside_agent_is_not_a_second_root() {
        let procs = vec![
            test_proc(40, 1, &["omp"]),
            test_proc(41, 40, claude_cmd()),
            test_proc(42, 41, &["bash"]),
        ];
        let trees = build_live_trees(&procs, &builtin_rules());
        assert_eq!(trees.len(), 1, "only the omp session");
        assert_eq!(trees[0].agent_id, "omp");
        assert_eq!(trees[0].members.len(), 3);
        assert_eq!(trees[0].depths, vec![0, 1, 2]);
    }

    #[test]
    fn cycle_terminates() {
        let procs = vec![
            test_proc(50, 51, claude_cmd()),
            test_proc(51, 50, &["bash"]),
        ];
        let trees = build_live_trees(&procs, &builtin_rules());
        assert_eq!(trees.len(), 1);
        assert_eq!(trees[0].members.len(), 2);
        assert_eq!(trees[0].depths, vec![0, 1]);
    }

    #[test]
    fn aggregates_are_sums() {
        let mut procs = vec![
            test_proc(60, 1, claude_cmd()),
            test_proc(61, 60, &["node", "server.js"]),
        ];
        procs[0].cpu_pct = 1.5;
        procs[0].rss_bytes = 100;
        procs[0].ports = vec![5173];
        procs[1].cpu_pct = 2.5;
        procs[1].rss_bytes = 250;
        procs[1].ports = vec![5173, 8080];
        let trees = build_live_trees(&procs, &builtin_rules());
        assert!((trees[0].cpu_pct - 4.0).abs() < f32::EPSILON);
        assert_eq!(trees[0].rss_bytes, 350);
        assert_eq!(trees[0].ports, vec![5173, 8080]);
    }

    #[test]
    fn no_agents_no_trees() {
        let procs = vec![test_proc(70, 1, &["node", "server.js"])];
        assert!(build_live_trees(&procs, &builtin_rules()).is_empty());
    }
}
