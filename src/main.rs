//! agents-ps: see what AI coding agents are running on your Mac. Read-only.

mod cli;
mod darwin;
mod human;
mod model;
mod orphans;
mod report;
mod rules;
mod snapshot;
mod source;
mod tree;
mod tui;

use std::io::{IsTerminal, Write};

use cli::{Args, Command};
use human::Style;

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

    if args.command == Command::Top {
        return run_top(&args);
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

/// The interactive view needs a real terminal on both ends: it reads keys from
/// stdin and paints on stdout.
fn run_top(args: &Args) -> i32 {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        eprintln!("agents-ps: top needs an interactive terminal");
        return 2;
    }
    let opts = tui::Options {
        no_hint: args.no_hint,
        color: std::env::var_os("NO_COLOR").is_none(),
    };
    match tui::run(opts) {
        Ok(()) => 0,
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
    let snapshot = snapshot::Snapshot::collect(&mut sampler, &rules);

    let style = Style {
        color: color_enabled(),
    };
    let generated_at_unix_ms = snapshot.now_us / 1000;

    let text = match args.command {
        Command::Sessions => {
            if args.json {
                return Ok(report::sessions_json(
                    &snapshot.sessions,
                    snapshot.totals,
                    snapshot.sample_ms,
                    generated_at_unix_ms,
                ));
            }
            report::sessions_text(
                &snapshot.sessions,
                snapshot.totals,
                snapshot.sample_ms,
                style,
            )
        }
        Command::Orphans => {
            if args.json {
                return Ok(report::orphans_json(
                    &snapshot.orphans,
                    snapshot.sample_ms,
                    generated_at_unix_ms,
                ));
            }
            report::orphans_text(&snapshot.orphans, style)
        }
        Command::Ports => {
            if args.json {
                return Ok(report::ports_json(
                    &snapshot.ports,
                    snapshot.sample_ms,
                    generated_at_unix_ms,
                ));
            }
            report::ports_text(&snapshot.ports, style)
        }
        Command::Top => unreachable!("top is dispatched before sampling"),
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
