//! One process-table sample, read through sysinfo and enriched with macOS
//! identity data. Ported from `aipm-core/src/source.rs`.

use std::collections::HashSet;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

use crate::darwin;
use crate::model::{ProcInfo, ProcKey};
use crate::rules::AgentRule;

/// Wait between the two refreshes that produce a CPU percentage.
const CPU_INTERVAL: Duration = Duration::from_millis(300);

pub struct Sample {
    pub procs: Vec<ProcInfo>,
    /// Wall time spent between the two refreshes.
    pub sample_ms: u64,
    /// Unix time in microseconds, taken after the sample.
    pub now_us: u64,
}

pub struct Sampler {
    sys: System,
    marker_keys: Vec<String>,
}

impl Sampler {
    pub fn new(rules: &[AgentRule]) -> Self {
        Self {
            // On macOS `SystemInner::new` always returns `Ok`.
            sys: System::new(),
            marker_keys: crate::rules::marker_keys(rules),
        }
    }

    fn refresh(&mut self) {
        let kind = ProcessRefreshKind::nothing()
            .with_cpu()
            .with_memory()
            .with_cwd(UpdateKind::Always)
            .with_exe(UpdateKind::Always)
            .with_cmd(UpdateKind::Always)
            .with_environ(UpdateKind::Always)
            .without_tasks();
        self.sys
            .refresh_processes_specifics(ProcessesToUpdate::All, true, kind);
    }

    /// Two refreshes: the second one turns cumulative CPU counters into a
    /// percentage over the interval between them.
    pub fn sample(&mut self) -> Sample {
        self.refresh();
        let started = Instant::now();
        std::thread::sleep(CPU_INTERVAL);
        self.refresh();

        let mut procs = Vec::with_capacity(self.sys.processes().len());
        for (pid, p) in self.sys.processes() {
            let pid = pid.as_u32() as i32;
            let bsd = darwin::bsd_info(pid);
            let ppid = bsd
                .as_ref()
                .map_or_else(|| p.parent().map_or(0, |pp| pp.as_u32() as i32), |b| b.ppid);
            let env_markers = if self.marker_keys.is_empty() {
                Vec::new()
            } else {
                p.environ()
                    .iter()
                    .filter_map(|entry| {
                        let entry = entry.to_string_lossy();
                        let key = entry.split_once('=').map(|(k, _)| k)?;
                        self.marker_keys
                            .iter()
                            .any(|marker| marker == key)
                            .then(|| entry.into_owned())
                    })
                    .collect()
            };
            procs.push(ProcInfo {
                key: ProcKey {
                    pid,
                    start_us: bsd
                        .as_ref()
                        .map_or_else(|| p.start_time() * 1_000_000, |b| b.start_us),
                },
                ppid,
                uid: p.user_id().map(|u| **u),
                name: p.name().to_string_lossy().into_owned(),
                exe: p.exe().map(std::path::Path::to_path_buf),
                cmd: p
                    .cmd()
                    .iter()
                    .map(|arg| arg.to_string_lossy().into_owned())
                    .collect(),
                cwd: p.cwd().map(std::path::Path::to_path_buf),
                tty: bsd.as_ref().and_then(|b| b.tty_dev).map(darwin::tty_name),
                env_markers,
                cpu_pct: p.cpu_usage(),
                rss_bytes: p.memory(),
                ports: Vec::new(),
            });
        }

        // Stable order keeps every report and every JSON dump reproducible.
        procs.sort_by_key(|p| p.key);

        Sample {
            procs,
            sample_ms: started.elapsed().as_millis() as u64,
            now_us: unix_us(),
        }
    }
}

/// Unix time in microseconds.
pub fn unix_us() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_micros() as u64)
}

/// Fetch listening TCP ports for the pids that made it into a report.
pub fn fill_ports(procs: &mut [ProcInfo], pids: &HashSet<i32>) {
    for proc in procs {
        if pids.contains(&proc.key.pid) {
            proc.ports = darwin::listening_ports(proc.key.pid);
        }
    }
}
