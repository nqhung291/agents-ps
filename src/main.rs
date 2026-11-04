//! agents-ps: see what AI coding agents are running on your Mac. Read-only.

mod cli;
mod darwin;
mod human;
mod model;
mod orphans;
mod report;
mod rules;
mod source;
mod tree;

use std::collections::{HashMap, HashSet};
use std::io::{IsTerminal, Write};
use std::path::PathBuf;

use cli::{Args, Command};
use human::Style;
use model::{ProcInfo, ProcKey};
use report::{OrphanView, PortRow, SessionView, Totals};

fn main() {
    let code = run();
    std::process::exit(code);
}

fn run() -> i32 {
    let args = match cli::parse(std::env::args_os()) {
        Ok(args) => args,
        Err(err) => {
            eprintln!("agents-ps: {err}");
            eprintln!("\n{}", cli::HELP.trim_end());
            return 2;
        }
    };
    if args.help {
        emit(cli::HELP);
        return 0;
    }
    if args.version {
        emit(&format!("agents-ps {}\n", env!("CARGO_PKG_VERSION")));
        return 0;
    }

    match collect(&args) {
        Ok(text) => {
            emit(&text);
            0
        }
        Err(err) => {
            eprintln!("agents-ps: {err}");
            1
        }
    }
}

/// Sample once, attribute processes, then render the requested command.
fn collect(args: &Args) -> Result<String, String> {
    let rules = rules::builtin_rules();
    let mut sampler = source::Sampler::new(&rules);
    let sample = sampler.sample();
    let mut procs = sample.procs;

    let trees = tree::build_live_trees(&procs, &rules);
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

    let mut trees = trees;
    for tree in &mut trees {
        tree::recompute_aggregates(tree, &procs);
    }

    let home = std::env::var_os("HOME").map(PathBuf::from);
    let candidates = orphans::classify(&procs, &rules, &trees, own_uid, home.as_deref());

    let mut by_key: HashMap<ProcKey, &ProcInfo> = HashMap::new();
    let mut by_pid: HashMap<i32, &ProcInfo> = HashMap::new();
    for p in &procs {
        by_key.insert(p.key, p);
        by_pid.insert(p.key.pid, p);
    }

    let style = Style {
        color: color_enabled(),
    };
    let now_us = sample.now_us;
    let generated_at_unix_ms = now_us / 1000;

    let text = match args.command {
        Command::Sessions => {
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
            if args.json {
                return Ok(report::sessions_json(
                    &sessions,
                    totals,
                    sample.sample_ms,
                    generated_at_unix_ms,
                ));
            }
            report::sessions_text(&sessions, totals, sample.sample_ms, style)
        }
        Command::Orphans => {
            let mut views: Vec<OrphanView> = candidates
                .iter()
                .filter_map(|c| OrphanView::build(c, &by_key, now_us))
                .collect();
            views.sort_by(|a, b| {
                b.rss_bytes
                    .cmp(&a.rss_bytes)
                    .then_with(|| a.pid.cmp(&b.pid))
            });
            if args.json {
                return Ok(report::orphans_json(
                    &views,
                    sample.sample_ms,
                    generated_at_unix_ms,
                ));
            }
            report::orphans_text(&views, style)
        }
        Command::Ports => {
            let mut rows: Vec<PortRow> = Vec::new();
            for tree in &trees {
                for key in &tree.members {
                    let Some(p) = by_key.get(key) else { continue };
                    for port in &p.ports {
                        rows.push(PortRow {
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
                    rows.push(PortRow {
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
            rows.sort_by(|a, b| {
                (a.kind != "session")
                    .cmp(&(b.kind != "session"))
                    .then_with(|| a.port.cmp(&b.port))
                    .then_with(|| a.pid.cmp(&b.pid))
            });
            if args.json {
                return Ok(report::ports_json(
                    &rows,
                    sample.sample_ms,
                    generated_at_unix_ms,
                ));
            }
            report::ports_text(&rows, style)
        }
    };

    Ok(with_hint(text, args, style))
}

/// Append the single promotional line: TTY only, never with `--json` or `--no-hint`.
fn with_hint(text: String, args: &Args, style: Style) -> String {
    if args.json || args.no_hint || !style.color {
        return text;
    }
    format!("{text}\n{}\n", style.dim(report::HINT))
}

/// Colour is for terminals only; `NO_COLOR` disables it everywhere.
fn color_enabled() -> bool {
    std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none()
}

/// Write and ignore errors: a closed pipe (`| head`) is not a failure.
fn emit(text: &str) {
    let mut stdout = std::io::stdout().lock();
    let _ = stdout.write_all(text.as_bytes());
    let _ = stdout.flush();
}
