//! State, key handling, and the pure row logic behind `agents-ps top`.
//!
//! Everything in this module is read-only. The only side effects are sending
//! commands to the sampler thread and writing the copy line to `pbcopy`.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::widgets::TableState;

use super::{Ctl, DEFAULT_INTERVAL_SECS, Msg, Options};
use crate::human;
use crate::report::{OrphanView, PortRow, Row, SessionView};
use crate::snapshot::Snapshot;

/// Samples kept per process for the detail sparkline.
const HISTORY_CAP: usize = 120;
/// Samples shown in the table HISTORY column.
pub const TABLE_HISTORY: usize = 8;
/// Samples shown in the header total-CPU sparkline.
pub const HEADER_HISTORY: usize = 16;
/// How long a copy toast stays in the footer.
const TOAST_DURATION: Duration = Duration::from_secs(2);
/// CPU history levels, smallest first.
const LEVELS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// Which tab is on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Sessions,
    Orphans,
    Ports,
}

impl View {
    pub fn index(self) -> usize {
        match self {
            View::Sessions => 0,
            View::Orphans => 1,
            View::Ports => 2,
        }
    }

    pub fn next(self) -> View {
        match self {
            View::Sessions => View::Orphans,
            View::Orphans => View::Ports,
            View::Ports => View::Sessions,
        }
    }

    pub fn previous(self) -> View {
        match self {
            View::Sessions => View::Ports,
            View::Orphans => View::Sessions,
            View::Ports => View::Orphans,
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            View::Sessions => "Sessions",
            View::Orphans => "Orphans",
            View::Ports => "Ports",
        }
    }

    pub fn empty_text(self, interval_secs: u64) -> String {
        match self {
            View::Sessions => format!(
                "No agent sessions running. Start claude, codex, aider… — checking every {interval_secs}s"
            ),
            View::Orphans => "✓ Nothing left behind — no orphaned agent processes.".to_string(),
            View::Ports => "No agent-owned listening ports.".to_string(),
        }
    }
}

/// Table sort columns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortKey {
    Cpu,
    Rss,
    Pid,
    Age,
}

impl SortKey {
    fn next(self) -> SortKey {
        match self {
            SortKey::Cpu => SortKey::Rss,
            SortKey::Rss => SortKey::Pid,
            SortKey::Pid => SortKey::Age,
            SortKey::Age => SortKey::Cpu,
        }
    }

    fn previous(self) -> SortKey {
        match self {
            SortKey::Cpu => SortKey::Age,
            SortKey::Rss => SortKey::Cpu,
            SortKey::Pid => SortKey::Rss,
            SortKey::Age => SortKey::Pid,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            SortKey::Cpu => "CPU",
            SortKey::Rss => "RSS",
            SortKey::Pid => "PID",
            SortKey::Age => "AGE",
        }
    }
}

/// One line of the Sessions view: a session header or one process below it.
#[derive(Clone, Debug)]
pub struct SessRow {
    /// Index into `Snapshot::sessions`.
    pub session: usize,
    /// Index into `SessionView::processes`; `None` is the session header row.
    pub proc_index: Option<usize>,
    /// Precomputed tree glyphs, empty for a header row.
    pub prefix: String,
}

/// Stable identity used to keep the selection across samples.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelKey {
    Proc { pid: i32, start_us: u64 },
    Port { port: u16, pid: i32 },
}

/// Derived row lists for all three views.
#[derive(Default)]
struct Rows {
    sessions: Vec<SessRow>,
    orphans: Vec<usize>,
    ports: Vec<usize>,
}

/// Everything the detail pane needs, copied out so the pane never borrows the
/// snapshot while the frame is drawn.
#[derive(Clone, Debug)]
pub struct DetailView {
    pub pid: i32,
    pub start_us: u64,
    pub agent_id: String,
    pub agent_name: String,
    pub name: String,
    pub ppid: Option<i32>,
    pub age: Option<String>,
    pub tty: Option<String>,
    pub cwd: Option<String>,
    pub argv: String,
    pub ports: Vec<u16>,
    pub env_keys: Vec<String>,
    pub likely: Option<String>,
    pub evidence: Option<String>,
    pub note: Option<String>,
}

/// The interactive application.
pub struct App {
    pub opts: Options,
    pub view: View,
    pub snap: Option<Snapshot>,
    pub session_rows: Vec<SessRow>,
    pub orphan_order: Vec<usize>,
    pub port_order: Vec<usize>,
    pub table_state: TableState,
    pub filter: String,
    pub filter_typing: bool,
    pub sort: SortKey,
    pub desc: bool,
    pub collapsed: HashSet<(i32, u64)>,
    /// Detail pane on wide terminals.
    pub detail: bool,
    /// Detail pane forced full-width on narrow terminals.
    pub narrow_detail: bool,
    /// Last rendered width; decides what `Enter` toggles.
    pub width: u16,
    pub help: bool,
    pub interval_secs: u64,
    pub paused: bool,
    /// `(text, is_error, shown_at)`.
    pub toast: Option<(String, bool, Instant)>,
    history: HashMap<(i32, u64), VecDeque<f32>>,
    pub total_history: VecDeque<f32>,
    quit: bool,
}

impl App {
    pub fn new(opts: Options) -> Self {
        App {
            opts,
            view: View::Sessions,
            snap: None,
            session_rows: Vec::new(),
            orphan_order: Vec::new(),
            port_order: Vec::new(),
            table_state: TableState::default(),
            filter: String::new(),
            filter_typing: false,
            sort: SortKey::Rss,
            desc: true,
            collapsed: HashSet::new(),
            detail: true,
            narrow_detail: false,
            width: 120,
            help: false,
            interval_secs: DEFAULT_INTERVAL_SECS,
            paused: false,
            toast: None,
            history: HashMap::new(),
            total_history: VecDeque::new(),
            quit: false,
        }
    }

    /// Draw, then block on the message channel until something changes.
    pub fn run(
        mut self,
        terminal: &mut DefaultTerminal,
        rx: &Receiver<Msg>,
        ctl: &Sender<Ctl>,
    ) -> std::io::Result<()> {
        terminal.draw(|frame| super::ui::draw(frame, &mut self))?;
        loop {
            // A toast needs one redraw when it expires, even while paused and
            // idle, so wait no longer than its deadline.
            let deadline = self
                .toast
                .as_ref()
                .map(|(_, _, shown)| TOAST_DURATION.saturating_sub(shown.elapsed()));
            let message = match deadline {
                Some(wait) => match rx.recv_timeout(wait) {
                    Ok(message) => Some(message),
                    Err(RecvTimeoutError::Timeout) => {
                        self.toast = None;
                        terminal.draw(|frame| super::ui::draw(frame, &mut self))?;
                        continue;
                    }
                    Err(RecvTimeoutError::Disconnected) => None,
                },
                None => rx.recv().ok(),
            };
            let Some(message) = message else { break };
            match message {
                Msg::Input(event) => self.on_event(event, ctl),
                Msg::Data(snapshot) => self.on_data(*snapshot),
                Msg::Quit => break,
            }
            if self.quit {
                break;
            }
            terminal.draw(|frame| super::ui::draw(frame, &mut self))?;
        }
        Ok(())
    }

    fn on_data(&mut self, snapshot: Snapshot) {
        let key = self.selected_key();
        self.record_history(&snapshot);
        self.snap = Some(snapshot);
        self.rebuild();
        if let Some(key) = key {
            self.select_key(key);
        }
    }

    fn record_history(&mut self, snapshot: &Snapshot) {
        let mut seen: HashSet<(i32, u64)> = HashSet::new();
        for session in &snapshot.sessions {
            push_history(
                &mut self.history,
                &mut seen,
                (session.pid, session.start_us),
                session.cpu_pct,
            );
            for row in &session.processes {
                let start_us = snapshot.details.get(&row.pid).map_or(0, |d| d.start_us);
                push_history(
                    &mut self.history,
                    &mut seen,
                    (row.pid, start_us),
                    row.cpu_pct,
                );
            }
        }
        self.total_history.push_back(snapshot.totals.cpu_pct);
        while self.total_history.len() > HEADER_HISTORY {
            self.total_history.pop_front();
        }
        self.history.retain(|key, _| seen.contains(key));
    }

    fn on_event(&mut self, event: Event, ctl: &Sender<Ctl>) {
        let Event::Key(key) = event else {
            // Resize and mouse events only need the redraw the loop already does.
            return;
        };
        if key.kind != KeyEventKind::Press {
            return;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        if self.help {
            self.handle_help_key(key);
            return;
        }
        if self.filter_typing {
            self.handle_filter_key(key);
            return;
        }
        self.handle_key(key, ctl);
    }

    fn handle_help_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('?') => self.help = false,
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.quit = true;
            }
            _ => {}
        }
    }

    fn handle_filter_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.filter.clear();
                self.filter_typing = false;
                self.rebuild();
            }
            KeyCode::Enter => self.filter_typing = false,
            KeyCode::Backspace => {
                self.filter.pop();
                self.rebuild();
            }
            KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.filter.push(ch);
                self.rebuild();
            }
            _ => {}
        }
    }

    fn handle_key(&mut self, key: KeyEvent, ctl: &Sender<Ctl>) {
        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.quit = true;
            }
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Esc => {
                if self.filter.is_empty() {
                    self.quit = true;
                } else {
                    self.filter.clear();
                    self.rebuild();
                }
            }
            KeyCode::Tab => self.set_view(self.view.next()),
            KeyCode::BackTab => self.set_view(self.view.previous()),
            KeyCode::Char('1') => self.set_view(View::Sessions),
            KeyCode::Char('2') => self.set_view(View::Orphans),
            KeyCode::Char('3') => self.set_view(View::Ports),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Home | KeyCode::Char('g') => self.select_edge(false),
            KeyCode::End | KeyCode::Char('G') => self.select_edge(true),
            KeyCode::PageDown => self.move_selection(10),
            KeyCode::PageUp => self.move_selection(-10),
            KeyCode::Char(' ') => self.toggle_fold(),
            KeyCode::Left | KeyCode::Char('h') => self.set_fold(false),
            KeyCode::Right | KeyCode::Char('l') => self.set_fold(true),
            KeyCode::Char('E') => self.toggle_all_folds(),
            KeyCode::Char('/') => self.filter_typing = true,
            KeyCode::Char('s') => {
                self.sort = self.sort.next();
                self.rebuild();
            }
            KeyCode::Char('S') => {
                self.sort = self.sort.previous();
                self.rebuild();
            }
            KeyCode::Char('r') => {
                self.desc = !self.desc;
                self.rebuild();
            }
            KeyCode::Enter => {
                if self.width >= 110 {
                    self.detail = !self.detail;
                } else {
                    self.narrow_detail = !self.narrow_detail;
                }
            }
            KeyCode::Char('y') => self.copy_selected(),
            KeyCode::Char('R') => {
                let _ = ctl.send(Ctl::RefreshNow);
            }
            KeyCode::Char('p') => {
                self.paused = !self.paused;
                let _ = ctl.send(Ctl::Pause(self.paused));
            }
            KeyCode::Char('+') | KeyCode::Char('=') => {
                self.set_interval(self.interval_secs + 1, ctl);
            }
            KeyCode::Char('-') | KeyCode::Char('_') => {
                self.set_interval(self.interval_secs.saturating_sub(1), ctl);
            }
            KeyCode::Char('?') => self.help = true,
            _ => {}
        }
    }

    fn set_interval(&mut self, secs: u64, ctl: &Sender<Ctl>) {
        let secs = secs.clamp(1, 10);
        if secs == self.interval_secs {
            return;
        }
        self.interval_secs = secs;
        let _ = ctl.send(Ctl::SetInterval(Duration::from_secs(secs)));
    }

    fn set_view(&mut self, view: View) {
        if self.view == view {
            return;
        }
        self.view = view;
        if self.rows_len() > 0 {
            self.table_state.select(Some(0));
        } else {
            self.table_state.select(None);
        }
    }

    pub fn rows_len(&self) -> usize {
        match self.view {
            View::Sessions => self.session_rows.len(),
            View::Orphans => self.orphan_order.len(),
            View::Ports => self.port_order.len(),
        }
    }

    fn ensure_selection(&mut self) {
        let len = self.rows_len();
        if len == 0 {
            self.table_state.select(None);
            return;
        }
        match self.table_state.selected() {
            Some(index) if index < len => {}
            Some(_) => self.table_state.select(Some(len - 1)),
            None => self.table_state.select(Some(0)),
        }
    }

    fn move_selection(&mut self, delta: isize) {
        let len = self.rows_len();
        if len == 0 {
            self.table_state.select(None);
            return;
        }
        let current = self.table_state.selected().unwrap_or(0) as isize;
        let next = (current + delta).clamp(0, len as isize - 1);
        self.table_state.select(Some(next as usize));
    }

    fn select_edge(&mut self, last: bool) {
        let len = self.rows_len();
        if len == 0 {
            self.table_state.select(None);
            return;
        }
        self.table_state
            .select(Some(if last { len - 1 } else { 0 }));
    }

    fn selected_session(&self) -> Option<usize> {
        if self.view != View::Sessions {
            return None;
        }
        let index = self.table_state.selected()?;
        self.session_rows.get(index).map(|row| row.session)
    }

    fn session_identity(&self, index: usize) -> Option<(i32, u64)> {
        let snap = self.snap.as_ref()?;
        let session = snap.sessions.get(index)?;
        Some((session.pid, session.start_us))
    }

    fn toggle_fold(&mut self) {
        let Some(session) = self.selected_session() else {
            return;
        };
        let Some(identity) = self.session_identity(session) else {
            return;
        };
        if !self.collapsed.remove(&identity) {
            self.collapsed.insert(identity);
        }
        self.rebuild();
        self.select_key(SelKey::Proc {
            pid: identity.0,
            start_us: identity.1,
        });
    }

    fn set_fold(&mut self, expand: bool) {
        let Some(session) = self.selected_session() else {
            return;
        };
        let Some(identity) = self.session_identity(session) else {
            return;
        };
        if expand {
            self.collapsed.remove(&identity);
        } else {
            self.collapsed.insert(identity);
        }
        self.rebuild();
        self.select_key(SelKey::Proc {
            pid: identity.0,
            start_us: identity.1,
        });
    }

    fn toggle_all_folds(&mut self) {
        if self.view != View::Sessions {
            return;
        }
        let key = self.selected_key();
        let session = self.selected_session();
        let collapsible: Vec<(i32, u64)> = self
            .snap
            .as_ref()
            .map(|snap| {
                snap.sessions
                    .iter()
                    .filter(|session| session.processes.len() > 1)
                    .map(|session| (session.pid, session.start_us))
                    .collect()
            })
            .unwrap_or_default();
        let all_collapsed = !collapsible.is_empty()
            && collapsible
                .iter()
                .all(|identity| self.collapsed.contains(identity));
        if all_collapsed {
            self.collapsed.clear();
            self.rebuild();
            if let Some(key) = key {
                self.select_key(key);
            }
        } else {
            self.collapsed.extend(collapsible);
            self.rebuild();
            if let Some(index) = session
                && let Some(identity) = self.session_identity(index)
            {
                self.select_key(SelKey::Proc {
                    pid: identity.0,
                    start_us: identity.1,
                });
            }
        }
    }

    fn copy_selected(&mut self) {
        let Some(pid) = self.selected_pid() else {
            self.toast = Some(("Nothing selected".to_string(), true, Instant::now()));
            return;
        };
        let text = format!("kill -TERM {pid}");
        match copy_to_clipboard(&text) {
            Ok(()) => {
                self.toast = Some((
                    "Copied — agents-ps never runs it".to_string(),
                    false,
                    Instant::now(),
                ));
            }
            Err(err) => {
                self.toast = Some((format!("copy failed: {err}"), true, Instant::now()));
            }
        }
    }

    pub fn toast_active(&self) -> bool {
        self.toast
            .as_ref()
            .is_some_and(|(_, _, shown)| shown.elapsed() < TOAST_DURATION)
    }

    pub fn selected_pid(&self) -> Option<i32> {
        let index = self.table_state.selected()?;
        let snap = self.snap.as_ref()?;
        match self.view {
            View::Sessions => {
                let row = self.session_rows.get(index)?;
                let session = snap.sessions.get(row.session)?;
                match row.proc_index {
                    None => Some(session.pid),
                    Some(proc_index) => session.processes.get(proc_index).map(|row| row.pid),
                }
            }
            View::Orphans => {
                let orphan = snap.orphans.get(*self.orphan_order.get(index)?)?;
                Some(orphan.pid)
            }
            View::Ports => {
                let port = snap.ports.get(*self.port_order.get(index)?)?;
                Some(port.pid)
            }
        }
    }

    fn selected_key(&self) -> Option<SelKey> {
        let index = self.table_state.selected()?;
        let snap = self.snap.as_ref()?;
        match self.view {
            View::Sessions => {
                let row = self.session_rows.get(index)?;
                let session = snap.sessions.get(row.session)?;
                match row.proc_index {
                    None => Some(SelKey::Proc {
                        pid: session.pid,
                        start_us: session.start_us,
                    }),
                    Some(proc_index) => {
                        let process = session.processes.get(proc_index)?;
                        let start_us = snap
                            .details
                            .get(&process.pid)
                            .map_or(0, |detail| detail.start_us);
                        Some(SelKey::Proc {
                            pid: process.pid,
                            start_us,
                        })
                    }
                }
            }
            View::Orphans => {
                let orphan = snap.orphans.get(*self.orphan_order.get(index)?)?;
                Some(SelKey::Proc {
                    pid: orphan.pid,
                    start_us: orphan.start_us,
                })
            }
            View::Ports => {
                let port = snap.ports.get(*self.port_order.get(index)?)?;
                Some(SelKey::Port {
                    port: port.port,
                    pid: port.pid,
                })
            }
        }
    }

    fn select_key(&mut self, key: SelKey) {
        let Some(snap) = self.snap.as_ref() else {
            return;
        };
        let found = match (self.view, key) {
            (View::Sessions, SelKey::Proc { pid, start_us }) => {
                self.session_rows.iter().position(|row| {
                    let Some(session) = snap.sessions.get(row.session) else {
                        return false;
                    };
                    match row.proc_index {
                        None => session.pid == pid && session.start_us == start_us,
                        Some(proc_index) => {
                            session.processes.get(proc_index).is_some_and(|process| {
                                process.pid == pid
                                    && snap
                                        .details
                                        .get(&pid)
                                        .is_none_or(|detail| detail.start_us == start_us)
                            })
                        }
                    }
                })
            }
            (View::Orphans, SelKey::Proc { pid, start_us }) => {
                self.orphan_order.iter().position(|&index| {
                    snap.orphans
                        .get(index)
                        .is_some_and(|orphan| orphan.pid == pid && orphan.start_us == start_us)
                })
            }
            (View::Ports, SelKey::Port { port, pid }) => {
                self.port_order.iter().position(|&index| {
                    snap.ports
                        .get(index)
                        .is_some_and(|row| row.port == port && row.pid == pid)
                })
            }
            _ => None,
        };
        if let Some(index) = found {
            self.table_state.select(Some(index));
        }
    }

    fn rebuild(&mut self) {
        let rows = match self.snap.as_ref() {
            Some(snap) => build_rows(snap, &self.filter, self.sort, self.desc, &self.collapsed),
            None => Rows::default(),
        };
        self.session_rows = rows.sessions;
        self.orphan_order = rows.orphans;
        self.port_order = rows.ports;
        self.ensure_selection();
    }

    /// CPU history for one process identity, oldest first.
    pub fn history_for(&self, pid: i32, start_us: u64) -> Vec<f32> {
        self.history
            .get(&(pid, start_us))
            .map(|values| values.iter().copied().collect())
            .unwrap_or_default()
    }

    /// Session header and optional process row behind a Sessions table line.
    pub fn row_parts(&self, row: &SessRow) -> Option<(&SessionView, Option<&Row>)> {
        let snap = self.snap.as_ref()?;
        let session = snap.sessions.get(row.session)?;
        let process = row
            .proc_index
            .and_then(|index| session.processes.get(index));
        Some((session, process))
    }

    /// Everything the detail pane shows for the current selection.
    pub fn selected_detail(&self) -> Option<DetailView> {
        let index = self.table_state.selected()?;
        let snap = self.snap.as_ref()?;
        match self.view {
            View::Sessions => {
                let row = self.session_rows.get(index)?;
                let session = snap.sessions.get(row.session)?;
                match row.proc_index {
                    None => {
                        let detail = snap.details.get(&session.pid);
                        Some(DetailView {
                            pid: session.pid,
                            start_us: session.start_us,
                            agent_id: session.agent_id.clone(),
                            agent_name: session.agent_name.clone(),
                            name: session.agent_name.clone(),
                            ppid: detail.map(|detail| detail.ppid),
                            age: Some(format!("up {}", human::duration(session.uptime_secs))),
                            tty: session
                                .tty
                                .clone()
                                .or_else(|| detail.and_then(|detail| detail.tty.clone())),
                            cwd: detail.and_then(|detail| detail.cwd.clone()),
                            argv: detail.map(|detail| detail.argv.clone()).unwrap_or_default(),
                            ports: session.ports.clone(),
                            env_keys: detail
                                .map(|detail| detail.env_marker_keys.clone())
                                .unwrap_or_default(),
                            likely: None,
                            evidence: None,
                            note: None,
                        })
                    }
                    Some(proc_index) => {
                        let process = session.processes.get(proc_index)?;
                        let detail = snap.details.get(&process.pid);
                        Some(DetailView {
                            pid: process.pid,
                            start_us: detail.map_or(0, |detail| detail.start_us),
                            agent_id: session.agent_id.clone(),
                            agent_name: session.agent_name.clone(),
                            name: process.name.clone(),
                            ppid: detail.map(|detail| detail.ppid).or(Some(process.ppid)),
                            age: None,
                            tty: detail.and_then(|detail| detail.tty.clone()),
                            cwd: detail.and_then(|detail| detail.cwd.clone()),
                            argv: detail
                                .map(|detail| detail.argv.clone())
                                .unwrap_or_else(|| process.cmd.clone()),
                            ports: process.ports.clone(),
                            env_keys: detail
                                .map(|detail| detail.env_marker_keys.clone())
                                .unwrap_or_default(),
                            likely: None,
                            evidence: None,
                            note: Some(format!("member of {}", session.agent_name)),
                        })
                    }
                }
            }
            View::Orphans => {
                let orphan = snap.orphans.get(*self.orphan_order.get(index)?)?;
                let detail = snap.details.get(&orphan.pid);
                Some(DetailView {
                    pid: orphan.pid,
                    start_us: orphan.start_us,
                    agent_id: orphan.agent_id.clone().unwrap_or_default(),
                    agent_name: orphan
                        .agent_name
                        .clone()
                        .unwrap_or_else(|| "Unknown agent".to_string()),
                    name: orphan.name.clone(),
                    ppid: Some(orphan.ppid),
                    age: Some(format!("age {}", human::duration(orphan.age_secs))),
                    tty: detail.and_then(|detail| detail.tty.clone()),
                    cwd: detail.and_then(|detail| detail.cwd.clone()),
                    argv: detail
                        .map(|detail| detail.argv.clone())
                        .unwrap_or_else(|| orphan.cmd.clone()),
                    ports: orphan.ports.clone(),
                    env_keys: detail
                        .map(|detail| detail.env_marker_keys.clone())
                        .unwrap_or_default(),
                    likely: Some(
                        orphan
                            .agent_name
                            .clone()
                            .unwrap_or_else(|| "an unknown agent".to_string()),
                    ),
                    evidence: Some(orphan.evidence.clone()),
                    note: None,
                })
            }
            View::Ports => {
                let row = snap.ports.get(*self.port_order.get(index)?)?;
                let detail = snap.details.get(&row.pid);
                Some(DetailView {
                    pid: row.pid,
                    start_us: detail.map_or(0, |detail| detail.start_us),
                    agent_id: row.agent_id.clone().unwrap_or_default(),
                    agent_name: row
                        .agent_name
                        .clone()
                        .unwrap_or_else(|| "Unknown agent".to_string()),
                    name: row.cmd.clone(),
                    ppid: detail.map(|detail| detail.ppid),
                    age: None,
                    tty: detail.and_then(|detail| detail.tty.clone()),
                    cwd: detail.and_then(|detail| detail.cwd.clone()),
                    argv: detail
                        .map(|detail| detail.argv.clone())
                        .unwrap_or_else(|| row.cmd.clone()),
                    ports: vec![row.port],
                    env_keys: detail
                        .map(|detail| detail.env_marker_keys.clone())
                        .unwrap_or_default(),
                    likely: None,
                    evidence: None,
                    note: Some(if row.kind == "orphan" {
                        format!("orphan · port {}", row.port)
                    } else {
                        format!(
                            "session pid {} · port {}",
                            row.session_pid.unwrap_or(row.pid),
                            row.port
                        )
                    }),
                })
            }
        }
    }

    /// Local wall-clock `HH:MM:SS` of the last sample.
    pub fn sample_clock(&self) -> String {
        match self.snap.as_ref() {
            Some(snapshot) => local_hhmmss(snapshot.now_us),
            None => "--:--:--".to_string(),
        }
    }
}

/// Build every derived row list in one pass.
fn build_rows(
    snapshot: &Snapshot,
    filter: &str,
    sort: SortKey,
    desc: bool,
    collapsed: &HashSet<(i32, u64)>,
) -> Rows {
    let needle = filter.trim().to_lowercase();
    let mut rows = Rows::default();

    let mut sessions: Vec<usize> = (0..snapshot.sessions.len()).collect();
    sessions
        .sort_by(|&a, &b| session_cmp(&snapshot.sessions[a], &snapshot.sessions[b], sort, desc));
    for index in sessions {
        let session = &snapshot.sessions[index];
        let Some(mask) = session_mask(session, &needle) else {
            continue;
        };
        rows.sessions.push(SessRow {
            session: index,
            proc_index: None,
            prefix: String::new(),
        });
        if collapsed.contains(&(session.pid, session.start_us)) {
            continue;
        }
        let order = visible_order(&session.processes, &mask, sort, desc);
        let depths: Vec<u16> = order
            .iter()
            .map(|&proc_index| session.processes[proc_index].depth)
            .collect();
        let glyphs = prefixes(&depths);
        for (position, &proc_index) in order.iter().enumerate() {
            rows.sessions.push(SessRow {
                session: index,
                proc_index: Some(proc_index),
                prefix: glyphs[position].clone(),
            });
        }
    }

    let mut orphans: Vec<usize> = (0..snapshot.orphans.len()).collect();
    if !needle.is_empty() {
        orphans.retain(|&index| orphan_matches(&snapshot.orphans[index], &needle));
    }
    orphans.sort_by(|&a, &b| orphan_cmp(&snapshot.orphans[a], &snapshot.orphans[b], sort, desc));
    rows.orphans = orphans;

    let mut ports: Vec<usize> = (0..snapshot.ports.len()).collect();
    if !needle.is_empty() {
        ports.retain(|&index| port_matches(&snapshot.ports[index], &needle));
    }
    rows.ports = ports;

    rows
}

/// Tree glyphs for a depth-first list of depths. A depth of zero is the
/// session header and gets no prefix.
pub fn prefixes(depths: &[u16]) -> Vec<String> {
    let mut prefixes = Vec::with_capacity(depths.len());
    for (index, &depth) in depths.iter().enumerate() {
        if depth == 0 {
            prefixes.push(String::new());
            continue;
        }
        let mut prefix = String::new();
        for level in 1..depth {
            let ancestor = (0..index).rev().find(|&j| depths[j] == level);
            let has_later_sibling = ancestor.is_some_and(|ancestor| {
                depths[ancestor + 1..]
                    .iter()
                    .find(|&&candidate| candidate <= level)
                    == Some(&level)
            });
            prefix.push_str(if has_later_sibling { "│  " } else { "   " });
        }
        let mut next = index + 1;
        while next < depths.len() && depths[next] > depth {
            next += 1;
        }
        let last = next >= depths.len() || depths[next] < depth;
        prefix.push_str(if last { "└─ " } else { "├─ " });
        prefixes.push(prefix);
    }
    prefixes
}

/// Process indices in depth-first order, with only kept rows emitted and
/// siblings sorted. Index 0 is the session root and is not emitted.
pub fn visible_order(rows: &[Row], keep: &[bool], sort: SortKey, desc: bool) -> Vec<usize> {
    let mut order = Vec::new();
    if rows.len() < 2 {
        return order;
    }
    let mut by_pid: HashMap<i32, usize> = HashMap::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        by_pid.insert(row.pid, index);
    }
    let mut children: HashMap<usize, Vec<usize>> = HashMap::new();
    for (index, row) in rows.iter().enumerate().skip(1) {
        let parent = by_pid.get(&row.ppid).copied().unwrap_or(0);
        children.entry(parent).or_default().push(index);
    }
    for kids in children.values_mut() {
        kids.sort_by(|&a, &b| row_cmp(&rows[a], &rows[b], sort, desc));
    }

    fn walk(
        index: usize,
        children: &HashMap<usize, Vec<usize>>,
        keep: &[bool],
        order: &mut Vec<usize>,
    ) {
        let Some(kids) = children.get(&index) else {
            return;
        };
        for &kid in kids {
            if !keep.get(kid).copied().unwrap_or(false) {
                continue;
            }
            order.push(kid);
            walk(kid, children, keep, order);
        }
    }

    walk(0, &children, keep, &mut order);
    order
}

/// Marks rows that match the filter plus every ancestor of a match.
pub fn filter_mask(rows: &[Row], needle: &str) -> Vec<bool> {
    let mut keep = vec![false; rows.len()];
    if needle.is_empty() {
        keep.fill(true);
        return keep;
    }
    for (index, row) in rows.iter().enumerate() {
        if row_matches(row, needle) {
            keep[index] = true;
        }
    }
    for index in 0..rows.len() {
        if !keep[index] {
            continue;
        }
        let mut depth = rows[index].depth;
        let mut from = index;
        while depth > 1 {
            let Some(parent) = (0..from).rev().find(|&j| rows[j].depth == depth - 1) else {
                break;
            };
            keep[parent] = true;
            from = parent;
            depth -= 1;
        }
    }
    keep
}

fn row_matches(row: &Row, needle: &str) -> bool {
    row.name.to_lowercase().contains(needle)
        || row.cmd.to_lowercase().contains(needle)
        || row.pid.to_string().contains(needle)
        || row
            .ports
            .iter()
            .any(|port| port.to_string().contains(needle))
}

/// The mask for one session: `None` hides the whole session.
fn session_mask(session: &SessionView, needle: &str) -> Option<Vec<bool>> {
    let count = session.processes.len();
    if needle.is_empty() {
        return Some(vec![true; count]);
    }
    let header_hit = session.agent_name.to_lowercase().contains(needle)
        || session.agent_id.to_lowercase().contains(needle)
        || session.pid.to_string().contains(needle)
        || session
            .ports
            .iter()
            .any(|port| port.to_string().contains(needle))
        || session
            .processes
            .first()
            .is_some_and(|row| row_matches(row, needle));
    if header_hit {
        return Some(vec![true; count]);
    }
    let mask = filter_mask(&session.processes, needle);
    mask.iter().any(|&keep| keep).then_some(mask)
}

fn orphan_matches(orphan: &OrphanView, needle: &str) -> bool {
    orphan.name.to_lowercase().contains(needle)
        || orphan.cmd.to_lowercase().contains(needle)
        || orphan.pid.to_string().contains(needle)
        || orphan
            .ports
            .iter()
            .any(|port| port.to_string().contains(needle))
        || orphan
            .agent_name
            .as_deref()
            .is_some_and(|name| name.to_lowercase().contains(needle))
        || orphan
            .agent_id
            .as_deref()
            .is_some_and(|id| id.contains(needle))
        || orphan.evidence.to_lowercase().contains(needle)
}

fn port_matches(row: &PortRow, needle: &str) -> bool {
    row.port.to_string().contains(needle)
        || row.pid.to_string().contains(needle)
        || row.cmd.to_lowercase().contains(needle)
        || row
            .agent_name
            .as_deref()
            .is_some_and(|name| name.to_lowercase().contains(needle))
        || row
            .agent_id
            .as_deref()
            .is_some_and(|id| id.contains(needle))
        || row.kind.contains(needle)
}

fn row_cmp(a: &Row, b: &Row, sort: SortKey, desc: bool) -> Ordering {
    let primary = match sort {
        SortKey::Cpu => a.cpu_pct.total_cmp(&b.cpu_pct),
        SortKey::Rss => a.rss_bytes.cmp(&b.rss_bytes),
        SortKey::Pid | SortKey::Age => a.pid.cmp(&b.pid),
    };
    let primary = if desc { primary.reverse() } else { primary };
    primary.then_with(|| a.pid.cmp(&b.pid))
}

fn session_cmp(a: &SessionView, b: &SessionView, sort: SortKey, desc: bool) -> Ordering {
    let primary = match sort {
        SortKey::Cpu => a.cpu_pct.total_cmp(&b.cpu_pct),
        SortKey::Rss => a.rss_bytes.cmp(&b.rss_bytes),
        SortKey::Pid => a.pid.cmp(&b.pid),
        SortKey::Age => a.uptime_secs.cmp(&b.uptime_secs),
    };
    let primary = if desc { primary.reverse() } else { primary };
    primary.then_with(|| a.pid.cmp(&b.pid))
}

fn orphan_cmp(a: &OrphanView, b: &OrphanView, sort: SortKey, desc: bool) -> Ordering {
    let primary = match sort {
        SortKey::Cpu => Ordering::Equal,
        SortKey::Rss => a.rss_bytes.cmp(&b.rss_bytes),
        SortKey::Pid => a.pid.cmp(&b.pid),
        SortKey::Age => a.age_secs.cmp(&b.age_secs),
    };
    let primary = if desc { primary.reverse() } else { primary };
    primary.then_with(|| a.pid.cmp(&b.pid))
}

/// A row of history bars for the last `width` samples, newest on the right.
/// Values under 0.05% are blank; the rest scale against `max(5.0, peak)` so
/// idle-but-nonzero activity stays visible while the tallest bar is kept for
/// the peak. Shorter histories are left-padded so the newest sample sits at
/// the right edge.
pub fn spark(points: &[f32], width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let take = width.min(points.len());
    let visible = &points[points.len() - take..];
    let peak = points
        .iter()
        .copied()
        .filter(|point| point.is_finite())
        .fold(5.0_f32, f32::max);
    let mut text = " ".repeat(width - take);
    for &point in visible {
        if !point.is_finite() || point < 0.05 {
            text.push(' ');
            continue;
        }
        let level = ((point / peak) * LEVELS.len() as f32)
            .ceil()
            .clamp(1.0, LEVELS.len() as f32) as usize;
        text.push(LEVELS[level - 1]);
    }
    text
}

fn push_history(
    history: &mut HashMap<(i32, u64), VecDeque<f32>>,
    seen: &mut HashSet<(i32, u64)>,
    key: (i32, u64),
    value: f32,
) {
    if !seen.insert(key) {
        return;
    }
    let values = history.entry(key).or_default();
    values.push_back(value);
    while values.len() > HISTORY_CAP {
        values.pop_front();
    }
}

/// Put the copy line on the macOS clipboard. `pbcopy` is the same tool the
/// shell uses; the child never touches a process table.
fn copy_to_clipboard(text: &str) -> std::io::Result<()> {
    let mut child = Command::new("pbcopy")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(text.as_bytes())?;
        // Dropping stdin closes the pipe so pbcopy can finish.
    }
    let status = child.wait()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "pbcopy exited with {status}"
        )))
    }
}

/// Local wall-clock time for a Unix microsecond timestamp.
fn local_hhmmss(now_us: u64) -> String {
    let seconds = (now_us / 1_000_000) as libc::time_t;
    // SAFETY: localtime_r fills a caller-owned `tm`; both pointers are valid for
    // the duration of the call and the value is never shared.
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&seconds, &mut tm).is_null() {
            return "--:--:--".to_string();
        }
        format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(pid: i32, ppid: i32, depth: u16, cpu: f32) -> Row {
        Row {
            pid,
            ppid,
            depth,
            name: format!("p{pid}"),
            cmd: format!("p{pid}"),
            cpu_pct: cpu,
            rss_bytes: 0,
            ports: Vec::new(),
        }
    }

    #[test]
    fn prefixes_mark_last_children() {
        let glyphs = prefixes(&[0, 1, 2, 1]);
        assert_eq!(glyphs, vec!["", "├─ ", "│  └─ ", "└─ "]);
    }

    #[test]
    fn siblings_sort_without_orphaning_children() {
        // 1 is the root; 2 and 3 are siblings; 4 belongs to 2.
        let rows = vec![
            row(1, 0, 0, 0.0),
            row(2, 1, 1, 1.0),
            row(4, 2, 2, 9.0),
            row(3, 1, 1, 5.0),
        ];
        let keep = vec![true; rows.len()];
        let order = visible_order(&rows, &keep, SortKey::Cpu, true);
        let pids: Vec<i32> = order.iter().map(|&index| rows[index].pid).collect();
        // Root children sort by CPU, and 2's child follows 2.
        assert_eq!(pids, vec![3, 2, 4]);
    }

    #[test]
    fn filter_keeps_ancestors_of_a_match() {
        let mut rows = vec![row(1, 0, 0, 0.0), row(2, 1, 1, 0.0), row(3, 2, 2, 0.0)];
        rows[2].cmd = "vite --host".to_string();
        let keep = filter_mask(&rows, "vite");
        assert_eq!(keep, vec![false, true, true]);
    }
}
