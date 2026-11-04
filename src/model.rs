//! Minimal read-only process model.

use std::path::{Path, PathBuf};

/// Stable process identity: a pid alone is reused, `start_us` is not.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProcKey {
    pub pid: i32,
    pub start_us: u64,
}

/// One process, as read from the kernel.
#[derive(Clone, Debug, Default)]
pub struct ProcInfo {
    pub key: ProcKey,
    pub ppid: i32,
    pub uid: Option<u32>,
    /// sysinfo name (usually the executable basename).
    pub name: String,
    pub exe: Option<PathBuf>,
    /// Lossy UTF-8 argv.
    pub cmd: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub tty: Option<String>,
    /// `KEY=VALUE` entries whose key is an agent marker (same uid only).
    pub env_markers: Vec<String>,
    /// Percent of one core; may exceed 100.
    pub cpu_pct: f32,
    pub rss_bytes: u64,
    /// Listening TCP ports, sorted, deduped.
    pub ports: Vec<u16>,
}

impl ProcInfo {
    /// Basename of `cmd[0]` when present, else the sysinfo name.
    pub fn label(&self) -> String {
        match self.cmd.first() {
            Some(c0) if !c0.is_empty() => Path::new(c0)
                .file_name()
                .map_or_else(|| c0.clone(), |s| s.to_string_lossy().into_owned()),
            _ => self.name.clone(),
        }
    }

    /// Short human command: label plus the first meaningful argument, truncated.
    pub fn short_cmd(&self) -> String {
        let label = self.label();
        let extra = self
            .cmd
            .iter()
            .skip(1)
            .find(|arg| !arg.starts_with('-') && !arg.is_empty())
            .map(|arg| {
                Path::new(arg)
                    .file_name()
                    .map_or_else(|| arg.clone(), |s| s.to_string_lossy().into_owned())
            })
            .filter(|base| *base != label);
        let text = match extra {
            Some(extra) => format!("{label} {extra}"),
            None => label,
        };
        truncate(&text, 44)
    }

    /// Age in seconds, clamped to zero when the clock or the start time is odd.
    pub fn age_secs(&self, now_us: u64) -> u64 {
        now_us.saturating_sub(self.key.start_us) / 1_000_000
    }
}

/// Truncate to `max` characters, appending `…` when cut.
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc_with(cmd: &[&str]) -> ProcInfo {
        ProcInfo {
            cmd: cmd.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn label_prefers_cmd_basename() {
        let p = proc_with(&["/usr/local/bin/node", "server.js"]);
        assert_eq!(p.label(), "node");
    }

    #[test]
    fn short_cmd_skips_flags() {
        let p = proc_with(&["node", "--experimental", "server.js"]);
        assert_eq!(p.short_cmd(), "node server.js");
    }

    #[test]
    fn short_cmd_without_args() {
        let p = proc_with(&["codex"]);
        assert_eq!(p.short_cmd(), "codex");
    }

    #[test]
    fn truncate_is_char_safe() {
        assert_eq!(truncate("abcdef", 4), "abc…");
        assert_eq!(truncate("abc", 4), "abc");
    }

    #[test]
    fn age_clamps() {
        let p = ProcInfo {
            key: ProcKey {
                pid: 7,
                start_us: 10_000_000,
            },
            ..Default::default()
        };
        assert_eq!(p.age_secs(12_500_000), 2);
        assert_eq!(p.age_secs(1), 0);
    }
}
