//! Interactive full-screen view (bare `agtop` on a terminal).
//!
//! Read-only by construction: this module only reads process data and shows or
//! copies `kill -TERM <pid>` lines. Nothing here sends a signal.
//!
//! Two threads feed one channel: crossterm events from stdin and snapshots from
//! a single sampler thread (every `darwin` call stays on that thread). The UI
//! loop blocks on the channel, so an idle screen costs no CPU.

mod app;
mod ui;

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::Duration;

use ratatui::crossterm::event;

use crate::rules::builtin_rules;
use crate::snapshot::Snapshot;
use crate::source::Sampler;

/// Default sample interval, in seconds.
pub(crate) const DEFAULT_INTERVAL_SECS: u64 = 2;

/// Options resolved by `main` from flags and the environment.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Hide the promotional footer (same meaning as `--no-hint`).
    pub no_hint: bool,
    /// `NO_COLOR` was unset: use the 16-colour palette.
    pub color: bool,
}

/// Everything the UI loop reacts to.
pub(crate) enum Msg {
    Input(event::Event),
    Data(Box<Snapshot>),
    Quit,
}

/// Commands for the sampler thread; it owns the only `Sampler`.
pub(crate) enum Ctl {
    RefreshNow,
    SetInterval(Duration),
    Pause(bool),
}

/// Run the interactive view until the user quits.
///
/// `ratatui::run` installs a panic hook and restores the terminal on both the
/// normal and the panic path. The worker threads are detached: when the loop
/// returns, `main` exits and they die with the process.
pub fn run(opts: Options) -> std::io::Result<()> {
    ratatui::run(|terminal| {
        let (tx, rx) = mpsc::channel::<Msg>();
        let (ctl_tx, ctl_rx) = mpsc::channel::<Ctl>();
        spawn_input(tx.clone());
        spawn_sampler(tx, ctl_rx);
        let app = app::App::new(opts);
        app.run(terminal, &rx, &ctl_tx)
    })
}

/// Read terminal events forever; a read error ends the UI cleanly.
fn spawn_input(tx: Sender<Msg>) {
    thread::spawn(move || {
        loop {
            match event::read() {
                Ok(event) => {
                    if tx.send(Msg::Input(event)).is_err() {
                        return;
                    }
                }
                Err(_) => {
                    let _ = tx.send(Msg::Quit);
                    return;
                }
            }
        }
    });
}

/// One sampler thread: sample, send, wait. All process inspection happens here.
fn spawn_sampler(tx: Sender<Msg>, ctl: Receiver<Ctl>) {
    thread::spawn(move || {
        let rules = builtin_rules();
        let mut sampler = Sampler::new(&rules);
        let mut interval = Duration::from_secs(DEFAULT_INTERVAL_SECS);
        let mut paused = false;

        // Every pass samples once: the wait below only ends on a trigger
        // (interval tick, `R`, or resume), so `R` while paused takes one fresh
        // sample and the loop goes straight back to waiting, still paused.
        loop {
            let snapshot = Snapshot::collect(&mut sampler, &rules);
            if tx.send(Msg::Data(Box::new(snapshot))).is_err() {
                return;
            }

            // Wait for the next sample trigger, absorbing control messages.
            let mut trigger = false;
            while !trigger {
                if paused {
                    match ctl.recv() {
                        Ok(Ctl::Pause(false)) => {
                            paused = false;
                            trigger = true;
                        }
                        Ok(Ctl::Pause(true)) => {}
                        Ok(Ctl::SetInterval(next)) => interval = next,
                        Ok(Ctl::RefreshNow) => trigger = true,
                        Err(_) => return,
                    }
                } else {
                    match ctl.recv_timeout(interval) {
                        Ok(Ctl::Pause(true)) => paused = true,
                        Ok(Ctl::Pause(false)) => {}
                        Ok(Ctl::SetInterval(next)) => interval = next,
                        Ok(Ctl::RefreshNow) => trigger = true,
                        Err(RecvTimeoutError::Timeout) => trigger = true,
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                }
            }
        }
    });
}
