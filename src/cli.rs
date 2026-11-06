//! Argument parsing. `lexopt` keeps the dependency footprint small and the help
//! text explicit.

use lexopt::prelude::*;

pub const HELP: &str = "\
agtop — see what AI coding agents are running on your Mac

USAGE:
    agtop [COMMAND] [OPTIONS]

COMMANDS:
    (none)     interactive full-screen view in a terminal (sessions, orphans,
               ports); prints the sessions report when piped or with --json
    sessions   live agent sessions, their process trees and listening ports
    orphans    processes left behind by agents that already exited
    ports      listening ports owned by agent trees or orphans

OPTIONS:
    --json       machine-readable output (stable schema, no hint line)
    --no-hint    hide the single line about Agents Monitor
    -h, --help   show this help
    -V, --version   show the version

Read-only: agtop never kills, signals or modifies anything.
Exit code 0 on success, 2 on a usage error.
";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Command {
    /// No command given: the interactive view on a terminal, else `Sessions`.
    #[default]
    Auto,
    Sessions,
    Orphans,
    Ports,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Args {
    pub command: Command,
    pub json: bool,
    pub no_hint: bool,
    pub help: bool,
    pub version: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub struct UsageError(pub String);

impl std::fmt::Display for UsageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Parse a full argv, program name included (lexopt skips the first item).
pub fn parse<I>(args: I) -> Result<Args, UsageError>
where
    I: IntoIterator,
    I::Item: Into<std::ffi::OsString>,
{
    let mut out = Args::default();
    let mut parser = lexopt::Parser::from_iter(args);
    let mut command_seen = false;
    while let Some(arg) = parser.next().map_err(|e| UsageError(e.to_string()))? {
        match arg {
            Short('h') | Long("help") => out.help = true,
            Short('V') | Long("version") => out.version = true,
            Long("json") => out.json = true,
            Long("no-hint") => out.no_hint = true,
            Value(value) => {
                if command_seen {
                    return Err(UsageError(format!(
                        "unexpected argument: {}",
                        value.to_string_lossy()
                    )));
                }
                command_seen = true;
                out.command = match value.to_string_lossy().as_ref() {
                    "orphans" => Command::Orphans,
                    "ports" => Command::Ports,
                    "sessions" | "status" => Command::Sessions,
                    other => {
                        return Err(UsageError(format!(
                            "unknown command: {other} (expected sessions, orphans or ports)"
                        )));
                    }
                };
            }
            other => {
                return Err(UsageError(other.unexpected().to_string()));
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_auto() {
        let args = parse::<[&str; 0]>([]).unwrap();
        assert_eq!(args.command, Command::Auto);
        assert!(!args.json && !args.no_hint && !args.help);
        assert_eq!(
            parse(["agtop", "sessions"]).unwrap().command,
            Command::Sessions
        );
    }

    #[test]
    fn subcommands_and_flags() {
        let args = parse(["agtop", "orphans", "--json"]).unwrap();
        assert_eq!(args.command, Command::Orphans);
        assert!(args.json);

        let args = parse(["agtop", "--no-hint", "ports"]).unwrap();
        assert_eq!(args.command, Command::Ports);
        assert!(args.no_hint);
    }

    #[test]
    fn help_and_version() {
        assert!(parse(["agtop", "--help"]).unwrap().help);
        assert!(parse(["agtop", "-h"]).unwrap().help);
        assert!(parse(["agtop", "--version"]).unwrap().version);
        assert!(parse(["agtop", "-V"]).unwrap().version);
    }

    #[test]
    fn unknown_command_is_an_error() {
        let err = parse(["agtop", "kill"]).unwrap_err();
        assert!(err.0.contains("unknown command"));
    }

    #[test]
    fn second_positional_is_an_error() {
        assert!(parse(["agtop", "ports", "orphans"]).is_err());
    }

    #[test]
    fn unknown_flag_is_an_error() {
        assert!(parse(["agtop", "--kill"]).is_err());
    }
}
