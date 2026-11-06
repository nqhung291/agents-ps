//! Report views and their two renderings: aligned human text and stable JSON.

use std::collections::HashMap;

use serde::Serialize;

use crate::human::{Style, bytes, cpu, duration, pad, pad_left, ports};
use crate::model::{ProcInfo, ProcKey};
use crate::orphans::OrphanCandidate;
use crate::tree::LiveTree;

/// The one and only promotional line; printed only on a TTY, never with `--json`.
pub const HINT: &str = "Live monitoring, alerts and one-click cleanup: Agents Monitor → \
https://agentmonitor.haziapp.com/?utm_source=agtop";

/// Machine-readable schema version. Bumped only on incompatible changes.
pub const SCHEMA: u32 = 1;

#[derive(Clone, Debug, Serialize)]
pub struct Row {
    pub pid: i32,
    pub ppid: i32,
    pub depth: u16,
    pub name: String,
    pub cmd: String,
    pub cpu_pct: f32,
    pub rss_bytes: u64,
    pub ports: Vec<u16>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SessionView {
    pub agent_id: String,
    pub agent_name: String,
    pub pid: i32,
    pub start_us: u64,
    pub tty: Option<String>,
    pub uptime_secs: u64,
    pub cpu_pct: f32,
    pub rss_bytes: u64,
    pub ports: Vec<u16>,
    pub processes: Vec<Row>,
}

impl SessionView {
    pub fn build(tree: &LiveTree, by_key: &HashMap<ProcKey, &ProcInfo>, now_us: u64) -> Self {
        let mut processes = Vec::with_capacity(tree.members.len());
        for (idx, key) in tree.members.iter().enumerate() {
            let Some(p) = by_key.get(key) else {
                continue;
            };
            processes.push(Row {
                pid: p.key.pid,
                ppid: p.ppid,
                depth: tree.depths.get(idx).copied().unwrap_or(0),
                name: p.label(),
                cmd: p.short_cmd(),
                cpu_pct: p.cpu_pct,
                rss_bytes: p.rss_bytes,
                ports: p.ports.clone(),
            });
        }
        SessionView {
            agent_id: tree.agent_id.clone(),
            agent_name: tree.agent_name.clone(),
            pid: tree.root.pid,
            start_us: tree.root.start_us,
            tty: by_key.get(&tree.root).and_then(|p| p.tty.clone()),
            uptime_secs: now_us.saturating_sub(tree.root.start_us) / 1_000_000,
            cpu_pct: tree.cpu_pct,
            rss_bytes: tree.rss_bytes,
            ports: tree.ports.clone(),
            processes,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct OrphanView {
    pub pid: i32,
    pub ppid: i32,
    pub start_us: u64,
    pub age_secs: u64,
    pub name: String,
    pub cmd: String,
    pub rss_bytes: u64,
    pub ports: Vec<u16>,
    pub agent_id: Option<String>,
    pub agent_name: Option<String>,
    /// `env marker`, `same project` or `heuristic`.
    pub evidence: String,
}

impl OrphanView {
    pub fn build(
        candidate: &OrphanCandidate,
        by_key: &HashMap<ProcKey, &ProcInfo>,
        now_us: u64,
    ) -> Option<Self> {
        let p = by_key.get(&candidate.key)?;
        Some(OrphanView {
            pid: p.key.pid,
            ppid: p.ppid,
            start_us: p.key.start_us,
            age_secs: p.age_secs(now_us),
            name: p.label(),
            cmd: p.short_cmd(),
            rss_bytes: p.rss_bytes,
            ports: p.ports.clone(),
            agent_id: candidate.agent_id.clone(),
            agent_name: candidate.agent_name.clone(),
            evidence: candidate.confidence.label().to_string(),
        })
    }

    fn unlikely_agent(&self) -> &str {
        self.agent_name.as_deref().unwrap_or("unknown agent")
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct PortRow {
    pub port: u16,
    pub pid: i32,
    pub cmd: String,
    pub agent_id: Option<String>,
    pub agent_name: Option<String>,
    /// `session` or `orphan`.
    pub kind: String,
    /// The session root pid for `session` rows.
    pub session_pid: Option<i32>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Totals {
    pub sessions: usize,
    pub processes: usize,
    pub cpu_pct: f32,
    pub rss_bytes: u64,
}

impl Totals {
    pub fn of(sessions: &[SessionView]) -> Self {
        Totals {
            sessions: sessions.len(),
            processes: sessions.iter().map(|s| s.processes.len()).sum(),
            cpu_pct: sessions.iter().map(|s| s.cpu_pct).sum(),
            rss_bytes: sessions.iter().map(|s| s.rss_bytes).sum(),
        }
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// Header line plus one block per session.
pub fn sessions_text(
    sessions: &[SessionView],
    totals: Totals,
    sample_ms: u64,
    style: Style,
) -> String {
    let mut out = String::new();
    let summary = format!(
        "{} · {} · {} CPU · {} · sampled in {sample_ms} ms",
        plural(totals.sessions, "session", "sessions"),
        plural(totals.processes, "process", "processes"),
        cpu(totals.cpu_pct),
        bytes(totals.rss_bytes),
    );
    out.push_str(&format!(
        "{}  {}\n",
        style.bold("agtop"),
        style.dim(&summary)
    ));

    if sessions.is_empty() {
        out.push_str(&format!(
            "\n{}\n",
            style.dim("No AI agent sessions running.")
        ));
        return out;
    }

    for session in sessions {
        out.push('\n');
        let meta = match session.tty.as_deref() {
            Some(tty) => format!(
                "pid {} · {tty} · up {} · {} · {} CPU · {}",
                session.pid,
                duration(session.uptime_secs),
                plural(session.processes.len(), "process", "processes"),
                cpu(session.cpu_pct),
                bytes(session.rss_bytes),
            ),
            None => format!(
                "pid {} · up {} · {} · {} CPU · {}",
                session.pid,
                duration(session.uptime_secs),
                plural(session.processes.len(), "process", "processes"),
                cpu(session.cpu_pct),
                bytes(session.rss_bytes),
            ),
        };
        out.push_str(&format!(
            "{}  {}\n",
            style.cyan(&session.agent_name),
            style.dim(&meta)
        ));
        out.push_str(&session_table(session, style));
    }
    out
}

fn session_table(session: &SessionView, style: Style) -> String {
    const MAX_PROCESS: usize = 34;
    let header = ["PID", "PROCESS", "CPU", "RSS", "PORTS"];
    let pid_w = session
        .processes
        .iter()
        .map(|r| r.pid.to_string().len())
        .chain(std::iter::once(header[0].len()))
        .max()
        .unwrap_or(3);
    let proc_cells: Vec<String> = session
        .processes
        .iter()
        .map(|r| {
            let indent = "  ".repeat(usize::from(r.depth));
            let text = format!("{indent}{}", r.cmd);
            crate::model::truncate(&text, MAX_PROCESS)
        })
        .collect();
    let proc_w = proc_cells
        .iter()
        .map(|c| c.chars().count())
        .chain(std::iter::once(header[1].len()))
        .max()
        .unwrap_or(7);
    let rss_w = session
        .processes
        .iter()
        .map(|r| bytes(r.rss_bytes).len())
        .chain(std::iter::once(bytes(session.rss_bytes).len()))
        .chain(std::iter::once(header[3].len()))
        .max()
        .unwrap_or(3);

    let mut out = String::new();
    out.push_str(&format!(
        "  {}  {}  {}  {}  {}\n",
        style.dim(&pad(header[0], pid_w)),
        style.dim(&pad(header[1], proc_w)),
        style.dim(&pad_left(header[2], 6)),
        style.dim(&pad_left(header[3], rss_w)),
        style.dim(header[4]),
    ));
    for (row, cell) in session.processes.iter().zip(proc_cells.iter()) {
        out.push_str(&format!(
            "  {}  {}  {}  {}  {}\n",
            pad(&row.pid.to_string(), pid_w),
            pad(cell, proc_w),
            pad_left(&cpu(row.cpu_pct), 6),
            pad_left(&bytes(row.rss_bytes), rss_w),
            ports(&row.ports),
        ));
    }
    out.push_str(&format!(
        "  {}  {}  {}  {}  {}\n",
        style.dim(&pad("total", pid_w)),
        style.dim(&pad("", proc_w)),
        style.dim(&pad_left(&cpu(session.cpu_pct), 6)),
        style.dim(&pad_left(&bytes(session.rss_bytes), rss_w)),
        style.dim(&ports(&session.ports)),
    ));
    out
}

/// Orphan table plus a copy-pasteable cleanup hint. Nothing is ever signalled.
pub fn orphans_text(orphans: &[OrphanView], style: Style) -> String {
    let mut out = String::new();
    if orphans.is_empty() {
        out.push_str(&format!(
            "{}  {}\n",
            style.bold("agtop orphans"),
            style.dim("no leftover agent processes found")
        ));
        return out;
    }

    out.push_str(&format!(
        "{}  {}\n\n",
        style.bold("agtop orphans"),
        style.dim(&format!(
            "{} left behind by agents that already exited",
            plural(orphans.len(), "process", "processes")
        ))
    ));

    let header = [
        "PID",
        "PROCESS",
        "AGE",
        "RSS",
        "PORTS",
        "LIKELY FROM",
        "EVIDENCE",
    ];
    let pid_w = orphans
        .iter()
        .map(|o| o.pid.to_string().len())
        .chain(std::iter::once(header[0].len()))
        .max()
        .unwrap_or(3);
    let proc_cells: Vec<String> = orphans
        .iter()
        .map(|o| crate::model::truncate(&o.cmd, 30))
        .collect();
    let proc_w = proc_cells
        .iter()
        .map(|c| c.chars().count())
        .chain(std::iter::once(header[1].len()))
        .max()
        .unwrap_or(7);
    let age_cells: Vec<String> = orphans.iter().map(|o| duration(o.age_secs)).collect();
    let age_w = age_cells
        .iter()
        .map(|c| c.len())
        .chain(std::iter::once(header[2].len()))
        .max()
        .unwrap_or(3);
    let rss_cells: Vec<String> = orphans.iter().map(|o| bytes(o.rss_bytes)).collect();
    let rss_w = rss_cells
        .iter()
        .map(|c| c.len())
        .chain(std::iter::once(header[3].len()))
        .max()
        .unwrap_or(3);
    let port_cells: Vec<String> = orphans.iter().map(|o| ports(&o.ports)).collect();
    let port_w = port_cells
        .iter()
        .map(|c| c.chars().count())
        .chain(std::iter::once(header[4].len()))
        .max()
        .unwrap_or(5);
    let agent_cells: Vec<String> = orphans
        .iter()
        .map(|o| o.unlikely_agent().to_string())
        .collect();
    let agent_w = agent_cells
        .iter()
        .map(|c| c.chars().count())
        .chain(std::iter::once(header[5].len()))
        .max()
        .unwrap_or(11);

    out.push_str(&format!(
        "  {}  {}  {}  {}  {}  {}  {}\n",
        style.dim(&pad(header[0], pid_w)),
        style.dim(&pad(header[1], proc_w)),
        style.dim(&pad(header[2], age_w)),
        style.dim(&pad_left(header[3], rss_w)),
        style.dim(&pad(header[4], port_w)),
        style.dim(&pad(header[5], agent_w)),
        style.dim(header[6]),
    ));
    for i in 0..orphans.len() {
        out.push_str(&format!(
            "  {}  {}  {}  {}  {}  {}  {}\n",
            pad(&orphans[i].pid.to_string(), pid_w),
            pad(&proc_cells[i], proc_w),
            pad(&age_cells[i], age_w),
            pad_left(&rss_cells[i], rss_w),
            pad(&port_cells[i], port_w),
            pad(&agent_cells[i], agent_w),
            orphans[i].evidence,
        ));
    }

    let pids: Vec<String> = orphans.iter().map(|o| o.pid.to_string()).collect();
    out.push_str(&format!(
        "\n{}\n  {}\n",
        style.dim("agtop never kills anything. To stop them yourself:"),
        style.yellow(&format!("kill -TERM {}", pids.join(" "))),
    ));
    out
}

/// One row per listening port, sessions first.
pub fn ports_text(rows: &[PortRow], style: Style) -> String {
    let mut out = String::new();
    if rows.is_empty() {
        out.push_str(&format!(
            "{}  {}\n",
            style.bold("agtop ports"),
            style.dim("no listening ports in agent trees or orphans")
        ));
        return out;
    }
    out.push_str(&format!(
        "{}  {}\n\n",
        style.bold("agtop ports"),
        style.dim(&format!(
            "{} owned by agent processes",
            plural(rows.len(), "listening port", "listening ports")
        ))
    ));

    let header = ["PORT", "PID", "PROCESS", "OWNER"];
    let port_cells: Vec<String> = rows.iter().map(|r| r.port.to_string()).collect();
    let port_w = port_cells
        .iter()
        .map(|c| c.len())
        .chain(std::iter::once(header[0].len()))
        .max()
        .unwrap_or(4);
    let pid_cells: Vec<String> = rows.iter().map(|r| r.pid.to_string()).collect();
    let pid_w = pid_cells
        .iter()
        .map(|c| c.len())
        .chain(std::iter::once(header[1].len()))
        .max()
        .unwrap_or(3);
    let proc_cells: Vec<String> = rows
        .iter()
        .map(|r| crate::model::truncate(&r.cmd, 28))
        .collect();
    let proc_w = proc_cells
        .iter()
        .map(|c| c.chars().count())
        .chain(std::iter::once(header[2].len()))
        .max()
        .unwrap_or(7);

    out.push_str(&format!(
        "  {}  {}  {}  {}\n",
        style.dim(&pad(header[0], port_w)),
        style.dim(&pad(header[1], pid_w)),
        style.dim(&pad(header[2], proc_w)),
        style.dim(header[3]),
    ));
    for i in 0..rows.len() {
        let owner = match (rows[i].kind.as_str(), rows[i].session_pid) {
            ("session", Some(root)) => format!(
                "{} (session pid {root})",
                rows[i].agent_name.as_deref().unwrap_or("agent")
            ),
            _ => format!(
                "{} (orphan)",
                rows[i].agent_name.as_deref().unwrap_or("unknown agent")
            ),
        };
        out.push_str(&format!(
            "  {}  {}  {}  {}\n",
            pad(&port_cells[i], port_w),
            pad(&pid_cells[i], pid_w),
            pad(&proc_cells[i], proc_w),
            owner,
        ));
    }
    out
}

#[derive(Serialize)]
struct JsonTotals {
    sessions: usize,
    processes: usize,
    cpu_pct: f32,
    rss_bytes: u64,
}

#[derive(Serialize)]
struct JsonSessions<'a> {
    schema: u32,
    generated_at_unix_ms: u64,
    sample_ms: u64,
    totals: JsonTotals,
    sessions: &'a [SessionView],
}

#[derive(Serialize)]
struct JsonCleanup {
    /// Pids a manual `kill -TERM` line would target; agtop never signals them.
    terminate: Vec<i32>,
}

#[derive(Serialize)]
struct JsonOrphans<'a> {
    schema: u32,
    generated_at_unix_ms: u64,
    sample_ms: u64,
    orphans: &'a [OrphanView],
    cleanup: JsonCleanup,
}

#[derive(Serialize)]
struct JsonPorts<'a> {
    schema: u32,
    generated_at_unix_ms: u64,
    sample_ms: u64,
    ports: &'a [PortRow],
}

/// Pretty-printed JSON with a trailing newline.
fn to_json<T: Serialize>(value: &T) -> String {
    match serde_json::to_string_pretty(value) {
        Ok(text) => format!("{text}\n"),
        // Serialization of these plain structs cannot fail; stay silent if it somehow does.
        Err(_) => "{}\n".to_string(),
    }
}

pub fn sessions_json(
    sessions: &[SessionView],
    totals: Totals,
    sample_ms: u64,
    generated_at_unix_ms: u64,
) -> String {
    to_json(&JsonSessions {
        schema: SCHEMA,
        generated_at_unix_ms,
        sample_ms,
        totals: JsonTotals {
            sessions: totals.sessions,
            processes: totals.processes,
            cpu_pct: totals.cpu_pct,
            rss_bytes: totals.rss_bytes,
        },
        sessions,
    })
}

pub fn orphans_json(orphans: &[OrphanView], sample_ms: u64, generated_at_unix_ms: u64) -> String {
    to_json(&JsonOrphans {
        schema: SCHEMA,
        generated_at_unix_ms,
        sample_ms,
        orphans,
        cleanup: JsonCleanup {
            terminate: orphans.iter().map(|o| o.pid).collect(),
        },
    })
}

pub fn ports_json(rows: &[PortRow], sample_ms: u64, generated_at_unix_ms: u64) -> String {
    to_json(&JsonPorts {
        schema: SCHEMA,
        generated_at_unix_ms,
        sample_ms,
        ports: rows,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(pid: i32, depth: u16, cmd: &str, rss: u64) -> Row {
        Row {
            pid,
            ppid: 1,
            depth,
            name: cmd.to_string(),
            cmd: format!("{}{cmd}", "  ".repeat(usize::from(depth))),
            cpu_pct: 1.0,
            rss_bytes: rss,
            ports: Vec::new(),
        }
    }

    fn session() -> SessionView {
        SessionView {
            agent_id: "claude".into(),
            agent_name: "Claude Code".into(),
            pid: 100,
            start_us: 1_000_000,
            tty: Some("ttys001".into()),
            uptime_secs: 90,
            cpu_pct: 2.0,
            rss_bytes: 3 * 1024 * 1024,
            ports: vec![5173],
            processes: vec![row(100, 0, "claude", 1024), row(101, 1, "node", 2048)],
        }
    }

    #[test]
    fn empty_sessions_say_so() {
        let text = sessions_text(&[], Totals::default(), 300, Style::default());
        assert!(text.contains("No AI agent sessions running."));
        // Colour is off in tests, so no escapes leak into non-TTY output.
        assert!(!text.contains('\u{1b}'));
    }

    #[test]
    fn sessions_text_has_header_rows_and_totals() {
        let sessions = vec![session()];
        let text = sessions_text(&sessions, Totals::of(&sessions), 300, Style::default());
        assert!(text.contains("Claude Code"));
        assert!(text.contains("pid 100"));
        assert!(text.contains("1m 30s"));
        assert!(text.contains("total"));
        assert!(text.contains("5173"));
    }

    #[test]
    fn totals_aggregate_sessions() {
        let totals = Totals::of(&[session(), session()]);
        assert_eq!(totals.sessions, 2);
        assert_eq!(totals.processes, 4);
        assert_eq!(totals.rss_bytes, 6 * 1024 * 1024);
    }

    #[test]
    fn orphans_text_prints_a_manual_command_only() {
        let orphans = vec![OrphanView {
            pid: 3311,
            ppid: 1,
            start_us: 0,
            age_secs: 4200,
            name: "node".into(),
            cmd: "node vite".into(),
            rss_bytes: 120 * 1024 * 1024,
            ports: vec![5173],
            agent_id: Some("claude".into()),
            agent_name: Some("Claude Code".into()),
            evidence: "env marker".into(),
        }];
        let text = orphans_text(&orphans, Style::default());
        assert!(text.contains("kill -TERM 3311"));
        assert!(text.contains("Claude Code"));
        assert!(text.contains("1h 10m"));
        assert!(text.contains("never kills anything"));
    }

    #[test]
    fn empty_orphans_text_is_quiet() {
        let text = orphans_text(&[], Style::default());
        assert!(text.contains("no leftover agent processes found"));
        assert!(!text.contains("kill"));
    }

    #[test]
    fn ports_text_labels_orphans() {
        let rows = vec![
            PortRow {
                port: 5173,
                pid: 101,
                cmd: "node".into(),
                agent_id: Some("claude".into()),
                agent_name: Some("Claude Code".into()),
                kind: "session".into(),
                session_pid: Some(100),
            },
            PortRow {
                port: 8080,
                pid: 3311,
                cmd: "node vite".into(),
                agent_id: None,
                agent_name: None,
                kind: "orphan".into(),
                session_pid: None,
            },
        ];
        let text = ports_text(&rows, Style::default());
        assert!(text.contains("Claude Code (session pid 100)"));
        assert!(text.contains("unknown agent (orphan)"));
    }

    #[test]
    fn json_is_stable_and_parsable() {
        let sessions = vec![session()];
        let text = sessions_json(&sessions, Totals::of(&sessions), 302, 1_700_000_000_000);
        let value: serde_json::Value = serde_json::from_str(&text).expect("valid json");
        assert_eq!(value["schema"], 1);
        assert_eq!(value["sessions"][0]["agent_id"], "claude");
        assert_eq!(value["sessions"][0]["processes"][1]["depth"], 1);
        assert_eq!(value["totals"]["processes"], 2);
        assert!(value.get("hint").is_none());
    }
}
