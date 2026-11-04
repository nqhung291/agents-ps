//! Rendering for the interactive view: one header, three tabs, three tables, a
//! detail pane, the footer and the help popup.
//!
//! Colours come from the 16 ANSI names; a background is never set, and the
//! only reversed style is the selected row (plus the header badge when colour
//! is on). Without colour every style falls back to the terminal default.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Cell, Clear, HighlightSpacing, Paragraph, Row as TableRow, Scrollbar,
    ScrollbarOrientation, ScrollbarState, Sparkline, Table, Tabs, Wrap,
};

use super::app::{App, DetailView, HEADER_HISTORY, TABLE_HISTORY, View};
use crate::human;
use crate::report::{OrphanView, PortRow};
use crate::snapshot::Snapshot;

/// MiB and GiB as f64, matching `human::bytes`.
const MIB: f64 = 1024.0 * 1024.0;
const GIB: f64 = MIB * 1024.0;

const SESSION_WIDTHS: [Constraint; 7] = [
    Constraint::Length(9),
    Constraint::Fill(1),
    Constraint::Length(6),
    Constraint::Length(8),
    Constraint::Length(9),
    Constraint::Length(10),
    Constraint::Length(6),
];
const PORT_WIDTHS: [Constraint; 5] = [
    Constraint::Length(6),
    Constraint::Length(8),
    Constraint::Fill(1),
    Constraint::Length(26),
    Constraint::Length(9),
];

/// Orphan columns: every fixed column is exactly as wide as its content, so
/// PROCESS (`Fill`) gets the whole rest of the table. LIKELY FROM covers the
/// header and any agent name, and never shrinks below twelve cells.
fn orphan_widths(snapshot: Option<&Snapshot>) -> [Constraint; 7] {
    let agent_name = snapshot.map_or(0, |snapshot| {
        snapshot
            .orphans
            .iter()
            .filter_map(|orphan| orphan.agent_name.as_deref())
            .map(|name| name.chars().count())
            .max()
            .unwrap_or(0)
    });
    let likely = agent_name.max("LIKELY FROM".chars().count()).max(12) as u16;
    [
        Constraint::Length(7),
        Constraint::Fill(1),
        Constraint::Length(9),
        Constraint::Length(10),
        Constraint::Length(10),
        Constraint::Length(likely),
        Constraint::Length(12),
    ]
}

/// Style source: colour on means ANSI names, off means plain styles.
#[derive(Clone, Copy)]
struct Palette {
    color: bool,
}

impl Palette {
    fn new(color: bool) -> Self {
        Palette { color }
    }

    fn fg(self, color: Color) -> Style {
        if self.color {
            Style::new().fg(color)
        } else {
            Style::new()
        }
    }

    fn bold(self) -> Style {
        Style::new().add_modifier(Modifier::BOLD)
    }

    fn dim(self) -> Style {
        Style::new().add_modifier(Modifier::DIM)
    }

    fn selection(self) -> Style {
        Style::new().add_modifier(Modifier::REVERSED | Modifier::BOLD)
    }

    fn focus_border(self) -> Style {
        self.fg(Color::Cyan)
    }

    fn header_row(self) -> Style {
        self.dim().add_modifier(Modifier::BOLD)
    }

    /// CPU%: dim under 1, green under 25, yellow under 50, red at 100 or more.
    fn cpu(self, pct: f32) -> Style {
        if pct < 1.0 {
            if self.color { self.dim() } else { Style::new() }
        } else if pct < 25.0 {
            self.fg(Color::Green)
        } else if pct < 50.0 {
            self.fg(Color::Yellow)
        } else if pct < 100.0 {
            self.fg(Color::Red)
        } else {
            self.fg(Color::Red).add_modifier(Modifier::BOLD)
        }
    }

    /// RSS: dim under 100 MiB, default under 1 GiB, yellow under 4 GiB, red above.
    fn rss(self, bytes: u64) -> Style {
        let value = bytes as f64;
        if value < 100.0 * MIB {
            if self.color { self.dim() } else { Style::new() }
        } else if value < GIB {
            Style::new()
        } else if value < 4.0 * GIB {
            self.fg(Color::Yellow).add_modifier(Modifier::BOLD)
        } else {
            self.fg(Color::Red).add_modifier(Modifier::BOLD)
        }
    }

    fn ports(self) -> Style {
        self.fg(Color::Magenta)
    }

    fn badge(self, agent_id: &str) -> Style {
        let (_, color) = badge_of(agent_id);
        if self.color {
            Style::new()
                .fg(color)
                .add_modifier(Modifier::REVERSED | Modifier::BOLD)
        } else {
            self.bold()
        }
    }

    fn evidence(self, evidence: &str) -> Style {
        match evidence {
            "env marker" => self.fg(Color::Green),
            "same project" => self.fg(Color::Yellow),
            _ => self.dim(),
        }
    }
}

/// Short badge tag and colour for an agent id.
pub fn badge_of(agent_id: &str) -> (&'static str, Color) {
    match agent_id {
        "claude" => ("CC", Color::Magenta),
        "amp" => ("AMP", Color::Magenta),
        "codex" => ("CDX", Color::Cyan),
        "opencode" => ("OC", Color::Cyan),
        "cursor-agent" => ("CUR", Color::Blue),
        "gemini" => ("GEM", Color::Blue),
        "omp" => ("OMP", Color::Blue),
        "aider" => ("AID", Color::Green),
        "goose" => ("GSE", Color::Green),
        "ollama" => ("OLL", Color::Green),
        _ => ("?", Color::Reset),
    }
}

/// Draw one full frame.
pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    app.width = area.width;
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .split(area);
    draw_header(frame, app, chunks[0]);
    draw_tabs(frame, app, chunks[1]);
    draw_body(frame, app, chunks[2]);
    draw_footer(frame, app, chunks[3]);
    if app.help {
        draw_help(frame, app, area);
    }
}

fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    frame.render_widget(Clear, area);
    let palette = Palette::new(app.opts.color);
    let mut left: Vec<Span<'static>> = Vec::new();
    let badge_style = if app.opts.color {
        Style::new().add_modifier(Modifier::REVERSED | Modifier::BOLD)
    } else {
        palette.bold()
    };
    left.push(Span::styled(" agents-ps ", badge_style));

    match app.snap.as_ref() {
        Some(snapshot) => {
            left.push(Span::raw(format!(
                " {} {} · {} {} · CPU {} ",
                snapshot.totals.sessions,
                plural(snapshot.totals.sessions, "session", "sessions"),
                snapshot.totals.processes,
                plural(snapshot.totals.processes, "proc", "procs"),
                human::cpu(snapshot.totals.cpu_pct),
            )));
            let history: Vec<f32> = app.total_history.iter().copied().collect();
            // Only as wide as the samples collected so far, so the newest bar
            // sits directly after the CPU figure instead of behind blank cells.
            left.push(Span::raw(header_spark(&history)));
            left.push(Span::raw(format!(
                " · RSS {} · ",
                human::bytes(snapshot.totals.rss_bytes)
            )));
            let orphans = snapshot.orphans.len();
            if orphans == 0 {
                left.push(Span::styled("✓ no orphans", palette.fg(Color::Green)));
            } else {
                left.push(Span::styled(
                    format!("⚠ {} {}", orphans, plural(orphans, "orphan", "orphans")),
                    palette.fg(Color::Red).add_modifier(Modifier::BOLD),
                ));
            }
        }
        None => left.push(Span::styled(" sampling…", palette.dim())),
    }

    let mut right: Vec<Span<'static>> = Vec::new();
    if app.paused {
        let pause_style = if app.opts.color {
            Style::new()
                .fg(Color::Yellow)
                .add_modifier(Modifier::REVERSED)
        } else {
            palette.bold()
        };
        right.push(Span::styled(" ⏸ PAUSED ", pause_style));
        right.push(Span::raw(format!("  {} ", app.sample_clock())));
    } else {
        right.push(Span::styled(
            format!("⟳ {}s", app.interval_secs),
            palette.dim(),
        ));
        right.push(Span::raw(format!("  {} ", app.sample_clock())));
    }

    let right_width = line_width(&right);
    if right_width + 12 < area.width as usize {
        let parts =
            Layout::horizontal([Constraint::Min(10), Constraint::Length(right_width as u16)])
                .split(area);
        frame.render_widget(Paragraph::new(Line::from(left)), parts[0]);
        frame.render_widget(
            Paragraph::new(Line::from(right).alignment(Alignment::Right)),
            parts[1],
        );
    } else {
        frame.render_widget(Paragraph::new(Line::from(left)), area);
    }
}

fn draw_tabs(frame: &mut Frame, app: &App, area: Rect) {
    frame.render_widget(Clear, area);
    let palette = Palette::new(app.opts.color);
    let (sessions, orphans, ports) = match app.snap.as_ref() {
        Some(snapshot) => (
            snapshot.totals.sessions,
            snapshot.orphans.len(),
            snapshot.ports.len(),
        ),
        None => (0, 0, 0),
    };

    let orphan_count_style = if orphans > 0 {
        palette.fg(Color::Red)
    } else {
        palette.dim()
    };
    let titles = vec![
        Line::from(vec![
            Span::raw("1 Sessions "),
            Span::styled(format!("({sessions})"), palette.dim()),
        ]),
        Line::from(vec![
            Span::raw("2 Orphans "),
            Span::styled(format!("({orphans})"), orphan_count_style),
        ]),
        Line::from(vec![
            Span::raw("3 Ports "),
            Span::styled(format!("({ports})"), palette.dim()),
        ]),
    ];
    let tabs = Tabs::new(titles)
        .select(app.view.index())
        .divider(Span::styled("│", palette.dim()))
        .highlight_style(if app.opts.color {
            Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD)
        } else {
            palette.bold()
        });

    let mut right: Vec<Span<'static>> = vec![
        Span::styled("sort ", palette.dim()),
        Span::styled(app.sort.label(), palette.bold()),
        Span::styled(if app.desc { "▼" } else { "▲" }, palette.dim()),
    ];
    if !app.filter.is_empty() || app.filter_typing {
        right.push(Span::styled("   filter: ", palette.dim()));
        right.push(Span::raw(app.filter.clone()));
        if app.filter_typing {
            right.push(Span::styled("▏", palette.fg(Color::Cyan)));
        }
    }
    right.push(Span::raw(" "));

    let right_width = line_width(&right);
    if right_width + 20 < area.width as usize {
        let parts =
            Layout::horizontal([Constraint::Min(20), Constraint::Length(right_width as u16)])
                .split(area);
        frame.render_widget(tabs, parts[0]);
        frame.render_widget(
            Paragraph::new(Line::from(right).alignment(Alignment::Right)),
            parts[1],
        );
    } else {
        frame.render_widget(tabs, area);
    }
}

fn draw_body(frame: &mut Frame, app: &mut App, area: Rect) {
    if app.snap.is_none() {
        frame.render_widget(Clear, area);
        let palette = Palette::new(app.opts.color);
        render_message(frame, area, "Sampling processes…", palette.dim());
        return;
    }
    let wide = area.width >= 110;
    if wide && app.detail {
        let parts = Layout::horizontal([Constraint::Fill(3), Constraint::Fill(2)]).split(area);
        draw_view(frame, app, parts[0]);
        draw_detail(frame, app, parts[1]);
    } else if !wide && app.narrow_detail {
        draw_detail(frame, app, area);
    } else {
        draw_view(frame, app, area);
    }
}

fn draw_view(frame: &mut Frame, app: &mut App, area: Rect) {
    let palette = Palette::new(app.opts.color);
    let block = view_block(app, palette);
    let inner = block.inner(area);

    if app.rows_len() == 0 {
        frame.render_widget(block, area);
        frame.render_widget(Clear, inner);
        let filtering = !app.filter.trim().is_empty();
        let text = if filtering {
            format!("No matches for “{}”.", app.filter.trim())
        } else {
            app.view.empty_text(app.interval_secs)
        };
        let style = if app.view == View::Orphans && !filtering {
            palette.fg(Color::Green)
        } else {
            palette.dim()
        };
        render_message(frame, inner, &text, style);
        return;
    }

    let table = match app.view {
        View::Sessions => session_table(app, palette, block, inner.width),
        View::Orphans => orphan_table(app, palette, block, inner.width),
        View::Ports => port_table(app, palette, block, inner.width),
    };
    frame.render_stateful_widget(table, area, &mut app.table_state);

    let scroll_area = area.inner(Margin {
        vertical: 1,
        horizontal: 0,
    });
    if app.rows_len() > scroll_area.height.saturating_sub(1) as usize {
        let mut state =
            ScrollbarState::new(app.rows_len()).position(app.table_state.selected().unwrap_or(0));
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .style(palette.dim());
        frame.render_stateful_widget(scrollbar, scroll_area, &mut state);
    }
}

fn view_block(app: &App, palette: Palette) -> Block<'static> {
    let position = app.table_state.selected().map_or(0, |index| index + 1);
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(palette.focus_border())
        .title(Line::from(Span::styled(
            format!(" {} ", app.view.title()),
            palette.bold(),
        )))
        .title_bottom(
            Line::from(Span::styled(
                format!(" {position}/{} ", app.rows_len()),
                palette.dim(),
            ))
            .alignment(Alignment::Right),
        )
}

fn session_table(
    app: &App,
    palette: Palette,
    block: Block<'static>,
    area_width: u16,
) -> Table<'static> {
    let Some(snapshot) = app.snap.as_ref() else {
        return Table::new(Vec::<TableRow<'static>>::new(), SESSION_WIDTHS);
    };
    let widths = column_widths(&SESSION_WIDTHS, area_width);
    let (max_cpu, max_rss) = visible_maxima(app);
    let mut rows: Vec<TableRow<'static>> = Vec::with_capacity(app.session_rows.len());

    for sess_row in &app.session_rows {
        let Some((session, process)) = app.row_parts(sess_row) else {
            continue;
        };
        let collapsed = app.collapsed.contains(&(session.pid, session.start_us));
        let (cpu, rss, ports) = match process {
            None => (session.cpu_pct, session.rss_bytes, session.ports.as_slice()),
            Some(row) => (row.cpu_pct, row.rss_bytes, row.ports.as_slice()),
        };
        let start_us = match process {
            None => session.start_us,
            Some(row) => snapshot
                .details
                .get(&row.pid)
                .map_or(0, |detail| detail.start_us),
        };
        let pid = process.map_or(session.pid, |row| row.pid);
        let history: Vec<f32> = app.history_for(pid, start_us);

        let pid_cell = match process {
            None => Cell::from(
                Line::from(vec![
                    Span::styled(if collapsed { "[+] " } else { "[-] " }, palette.dim()),
                    Span::raw(session.pid.to_string()),
                ])
                .alignment(Alignment::Right),
            ),
            Some(row) => Cell::from(Line::from(row.pid.to_string()).alignment(Alignment::Right)),
        };

        let process_cell = match process {
            None => {
                let mut parts = vec![
                    (
                        format!("{} ", badge_of(&session.agent_id).0),
                        palette.badge(&session.agent_id),
                    ),
                    (session.agent_name.clone(), palette.bold()),
                ];
                if let Some(tty) = &session.tty {
                    parts.push((format!("  {tty}"), palette.dim()));
                }
                if collapsed {
                    parts.push((
                        format!("  ({} procs)", session.processes.len()),
                        palette.dim(),
                    ));
                }
                Cell::from(fit_line(parts, widths[1]))
            }
            Some(row) => {
                let (head, rest) = split_first_word(&row.cmd);
                Cell::from(fit_line(
                    vec![
                        (sess_row.prefix.clone(), palette.dim()),
                        (head, palette.bold()),
                        (rest, Style::new()),
                    ],
                    widths[1],
                ))
            }
        };

        let mut cpu_style = palette.cpu(cpu);
        if max_cpu > 0.0 && cpu >= max_cpu {
            cpu_style = cpu_style.add_modifier(Modifier::BOLD);
        }
        let mut rss_style = palette.rss(rss);
        if max_rss > 0 && rss >= max_rss {
            rss_style = rss_style.add_modifier(Modifier::BOLD);
        }

        let ports_cell = if ports.is_empty() {
            right_cell("—".to_string(), palette.dim())
        } else {
            right_cell(ports_text(ports, widths[5]), palette.ports())
        };

        let up_cell = match process {
            None => Cell::from(
                Line::from(human::duration(session.uptime_secs)).alignment(Alignment::Right),
            ),
            Some(_) => Cell::from(""),
        };

        let mut table_row = TableRow::new(vec![
            pid_cell,
            process_cell,
            right_cell(human::cpu(cpu).trim_end_matches('%').to_string(), cpu_style),
            Cell::from(spark(&history, TABLE_HISTORY)),
            right_cell(human::bytes(rss), rss_style),
            ports_cell,
            up_cell,
        ]);
        if process.is_none() {
            table_row = table_row.style(palette.bold());
        }
        rows.push(table_row);
    }

    let header = TableRow::new(vec![
        right_cell("PID".to_string(), Style::new()),
        Cell::from("PROCESS"),
        right_cell("CPU%".to_string(), Style::new()),
        Cell::from("HISTORY"),
        right_cell("RSS".to_string(), Style::new()),
        right_cell("PORTS".to_string(), Style::new()),
        right_cell("UP".to_string(), Style::new()),
    ])
    .style(palette.header_row());

    Table::new(rows, SESSION_WIDTHS)
        .header(header)
        .block(block)
        .column_spacing(1)
        .row_highlight_style(palette.selection())
        .highlight_symbol("▌")
        .highlight_spacing(HighlightSpacing::Always)
}

fn orphan_table(
    app: &App,
    palette: Palette,
    block: Block<'static>,
    area_width: u16,
) -> Table<'static> {
    let widths = orphan_widths(app.snap.as_ref());
    let Some(snapshot) = app.snap.as_ref() else {
        return Table::new(Vec::<TableRow<'static>>::new(), widths);
    };
    let cells = column_widths(&widths, area_width);
    let mut rows: Vec<TableRow<'static>> = Vec::with_capacity(app.orphan_order.len());
    for &index in &app.orphan_order {
        let Some(orphan) = snapshot.orphans.get(index) else {
            continue;
        };
        rows.push(orphan_row(orphan, palette, &cells));
    }

    let header = TableRow::new(vec![
        right_cell("PID".to_string(), Style::new()),
        Cell::from("PROCESS"),
        right_cell("AGE".to_string(), Style::new()),
        right_cell("RSS".to_string(), Style::new()),
        right_cell("PORTS".to_string(), Style::new()),
        Cell::from("LIKELY FROM"),
        Cell::from("EVIDENCE"),
    ])
    .style(palette.header_row());

    Table::new(rows, widths)
        .header(header)
        .block(block)
        .column_spacing(1)
        .row_highlight_style(palette.selection())
        .highlight_symbol("▌")
        .highlight_spacing(HighlightSpacing::Always)
}

fn orphan_row(orphan: &OrphanView, palette: Palette, widths: &[usize]) -> TableRow<'static> {
    let (head, rest) = split_first_word(&orphan.cmd);
    let process = if head.is_empty() {
        Cell::from(fit_line(
            vec![(orphan.name.clone(), palette.bold())],
            widths[1],
        ))
    } else {
        Cell::from(fit_line(
            vec![(head, palette.bold()), (rest, Style::new())],
            widths[1],
        ))
    };
    let ports = if orphan.ports.is_empty() {
        right_cell("—".to_string(), palette.dim())
    } else {
        right_cell(ports_text(&orphan.ports, widths[4]), palette.ports())
    };
    let likely = match orphan.agent_name.as_deref() {
        Some(name) => Cell::from(Span::raw(name.to_string())),
        None => Cell::from(Span::styled("—", palette.dim())),
    };
    TableRow::new(vec![
        right_cell(orphan.pid.to_string(), palette.fg(Color::Red)),
        process,
        right_cell(human::duration(orphan.age_secs), palette.dim()),
        right_cell(
            human::bytes(orphan.rss_bytes),
            palette.rss(orphan.rss_bytes),
        ),
        ports,
        likely,
        Cell::from(Span::styled(
            orphan.evidence.clone(),
            palette.evidence(&orphan.evidence),
        )),
    ])
}

fn port_table(
    app: &App,
    palette: Palette,
    block: Block<'static>,
    area_width: u16,
) -> Table<'static> {
    let Some(snapshot) = app.snap.as_ref() else {
        return Table::new(Vec::<TableRow<'static>>::new(), PORT_WIDTHS);
    };
    let widths = column_widths(&PORT_WIDTHS, area_width);
    let mut rows: Vec<TableRow<'static>> = Vec::with_capacity(app.port_order.len());
    for &index in &app.port_order {
        let Some(row) = snapshot.ports.get(index) else {
            continue;
        };
        rows.push(port_row(row, palette, &widths));
    }

    let header = TableRow::new(vec![
        right_cell("PORT".to_string(), Style::new()),
        right_cell("PID".to_string(), Style::new()),
        Cell::from("PROCESS"),
        Cell::from("OWNER"),
        Cell::from("KIND"),
    ])
    .style(palette.header_row());

    Table::new(rows, PORT_WIDTHS)
        .header(header)
        .block(block)
        .column_spacing(1)
        .row_highlight_style(palette.selection())
        .highlight_symbol("▌")
        .highlight_spacing(HighlightSpacing::Always)
}

fn port_row(row: &PortRow, palette: Palette, widths: &[usize]) -> TableRow<'static> {
    let owner = match (row.agent_name.as_deref(), row.kind.as_str()) {
        (Some(name), "session") => format!(
            "{name} (session pid {})",
            row.session_pid.unwrap_or(row.pid)
        ),
        (Some(name), _) => format!("{name} · orphan"),
        (None, _) => "unknown · orphan".to_string(),
    };
    let (head, rest) = split_first_word(&row.cmd);
    let process = if head.is_empty() {
        Cell::from("")
    } else {
        Cell::from(fit_line(
            vec![(head, palette.bold()), (rest, Style::new())],
            widths[2],
        ))
    };
    let kind_style = if row.kind == "orphan" {
        palette.fg(Color::Red)
    } else {
        palette.dim()
    };
    TableRow::new(vec![
        right_cell(row.port.to_string(), palette.ports()),
        right_cell(row.pid.to_string(), Style::new()),
        process,
        Cell::from(fit_line(vec![(owner, Style::new())], widths[3])),
        Cell::from(Span::styled(row.kind.clone(), kind_style)),
    ])
}

fn draw_detail(frame: &mut Frame, app: &App, area: Rect) {
    let palette = Palette::new(app.opts.color);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(palette.dim())
        .title(Line::from(Span::styled(" Details ", palette.bold())));

    let inner = block.inner(area);
    frame.render_widget(Clear, inner);
    let Some(detail) = app.selected_detail() else {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "Select a row to see details.",
                palette.dim(),
            )))
            .block(block)
            .wrap(Wrap { trim: false }),
            area,
        );
        return;
    };
    let lines = detail_lines(&detail, palette, inner.width);
    let info_height = lines.len() as u16;
    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });

    if inner.height < 10 {
        frame.render_widget(paragraph.block(block), area);
        return;
    }

    let spark_height = 4.min(inner.height);
    let copy_height = 4.min(inner.height.saturating_sub(spark_height));
    let info_height = info_height.min(inner.height.saturating_sub(spark_height + copy_height));
    let parts = Layout::vertical([
        Constraint::Length(info_height),
        Constraint::Length(spark_height),
        Constraint::Fill(1),
        Constraint::Length(copy_height),
    ])
    .split(inner);

    frame.render_widget(block, area);
    frame.render_widget(paragraph, parts[0]);
    if parts[1].height > 0 {
        draw_detail_spark(frame, app, &detail, parts[1], palette);
    }
    if parts[3].height > 0 {
        draw_copy_box(frame, &detail, parts[3], palette);
    }
}

fn detail_lines(detail: &DetailView, palette: Palette, width: u16) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let (tag, _) = badge_of(&detail.agent_id);
    lines.push(Line::from(vec![
        Span::styled(format!(" {tag} "), palette.badge(&detail.agent_id)),
        Span::raw(" "),
        Span::styled(detail.agent_name.clone(), palette.bold()),
        Span::styled(format!(" · pid {}", detail.pid), palette.dim()),
    ]));

    let mut meta: Vec<String> = Vec::new();
    if let Some(age) = &detail.age {
        meta.push(age.clone());
    }
    if let Some(ppid) = detail.ppid {
        meta.push(format!("ppid {ppid}"));
    }
    if let Some(tty) = &detail.tty {
        meta.push(tty.clone());
    }
    if !meta.is_empty() {
        lines.push(Line::from(Span::styled(
            format!("  {}", meta.join("   ")),
            palette.dim(),
        )));
    }

    lines.extend(field_lines(
        "cwd",
        palette,
        match &detail.cwd {
            Some(cwd) => Span::raw(abbreviate_home(cwd)),
            None => Span::styled("—", palette.dim()),
        },
        width,
    ));
    lines.extend(field_lines(
        "cmd",
        palette,
        Span::raw(if detail.argv.is_empty() {
            detail.name.clone()
        } else {
            detail.argv.clone()
        }),
        width,
    ));
    lines.extend(field_lines(
        "ports",
        palette,
        if detail.ports.is_empty() {
            Span::styled("—", palette.dim())
        } else {
            Span::styled(human::ports(&detail.ports), palette.ports())
        },
        width,
    ));
    lines.extend(field_lines(
        "env",
        palette,
        if detail.env_keys.is_empty() {
            Span::styled("—", palette.dim())
        } else {
            Span::raw(detail.env_keys.join(", "))
        },
        width,
    ));
    if let Some(likely) = &detail.likely {
        lines.extend(field_lines(
            "likely from",
            palette,
            Span::raw(likely.clone()),
            width,
        ));
    }
    if let Some(evidence) = &detail.evidence {
        lines.extend(field_lines(
            "evidence",
            palette,
            Span::styled(evidence.clone(), palette.evidence(evidence)),
            width,
        ));
    }
    if let Some(note) = &detail.note {
        for chunk in wrap_text(note, (width as usize).saturating_sub(2)) {
            lines.push(Line::from(Span::styled(
                format!("  {chunk}"),
                palette.dim(),
            )));
        }
    }
    lines
}

/// One label/value row: the label column is fixed, and continuation lines are
/// indented to the value column instead of falling back to column zero.
fn field_lines(
    label: &str,
    palette: Palette,
    value: Span<'static>,
    width: u16,
) -> Vec<Line<'static>> {
    let prefix = format!(" {label:<12}");
    let indent = prefix.chars().count();
    let available = (width as usize).saturating_sub(indent);
    let style = value.style;
    let mut lines = Vec::new();
    for (index, chunk) in wrap_text(value.content.as_ref(), available)
        .into_iter()
        .enumerate()
    {
        let lead = if index == 0 {
            prefix.clone()
        } else {
            " ".repeat(indent)
        };
        lines.push(Line::from(vec![
            Span::styled(lead, palette.dim()),
            Span::styled(chunk, style),
        ]));
    }
    lines
}

/// Split `text` into chunks of at most `width` display columns.
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut chunks = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        current.push(ch);
        if Span::raw(current.as_str()).width() > width {
            current.pop();
            chunks.push(std::mem::take(&mut current));
            current.push(ch);
        }
    }
    chunks.push(current);
    chunks
}

fn draw_detail_spark(
    frame: &mut Frame,
    app: &App,
    detail: &DetailView,
    area: Rect,
    palette: Palette,
) {
    let history = app.history_for(detail.pid, detail.start_us);
    let now = history.last().copied().unwrap_or(0.0);
    let peak = history.iter().copied().fold(0.0_f32, f32::max);
    let parts = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).split(area);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!(
                " CPU  now {}  peak {}",
                human::cpu(now.max(0.0)),
                human::cpu(peak.max(0.0))
            ),
            palette.dim(),
        ))),
        parts[0],
    );
    if parts[1].height == 0 {
        return;
    }
    if history.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(" —", palette.dim()))),
            parts[1],
        );
        return;
    }
    // Tenths of a percent: the sparkline needs integers, and a 2% floor keeps
    // idle-but-nonzero activity visible instead of a flat empty row.
    const SCALE: f32 = 10.0;
    let floor = (2.0 * SCALE) as u64;
    let data: Vec<u64> = history
        .iter()
        .map(|value| (value.max(0.0) * SCALE).round() as u64)
        .collect();
    let max = ((peak.max(0.0) * SCALE).ceil() as u64).max(floor);
    let sparkline = Sparkline::default().data(data.as_slice()).max(max);
    frame.render_widget(sparkline, parts[1]);
}

fn draw_copy_box(frame: &mut Frame, detail: &DetailView, area: Rect, palette: Palette) {
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(palette.dim())
        .title(Line::from(Span::styled(" y copy ", palette.bold())));
    let lines = vec![
        Line::from(format!(" kill -TERM {}", detail.pid)),
        Line::from(Span::styled(" agents-ps never runs it", palette.dim())),
    ];
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    frame.render_widget(Clear, area);
    let palette = Palette::new(app.opts.color);
    if app.toast_active()
        && let Some((text, is_error, _)) = app.toast.as_ref()
    {
        let style = if *is_error {
            palette.fg(Color::Red)
        } else {
            palette.fg(Color::Green)
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(format!(" {text}"), style))),
            area,
        );
        return;
    }

    let mut hints: Vec<Span<'static>> = Vec::new();
    for (index, (key, description)) in [
        ("↑↓", "move"),
        ("space", "fold"),
        ("/", "filter"),
        ("s", "sort"),
        ("Tab", "view"),
        ("y", "copy"),
        ("?", "help"),
        ("q", "quit"),
    ]
    .into_iter()
    .enumerate()
    {
        if index > 0 {
            hints.push(Span::raw(" "));
        }
        hints.push(Span::styled(key, palette.bold()));
        hints.push(Span::raw(" "));
        hints.push(Span::styled(description, palette.dim()));
    }

    let orphans = app
        .snap
        .as_ref()
        .map_or(0, |snapshot| snapshot.orphans.len());
    let right = if orphans > 0 {
        "One-click cleanup: Agents Monitor → agentmonitor.haziapp.com"
    } else {
        "read-only · Agents Monitor → agentmonitor.haziapp.com"
    };
    let show_right = !app.opts.no_hint && area.width >= 100;

    if show_right {
        let right_width = right.chars().count();
        if right_width + 20 < area.width as usize {
            let parts =
                Layout::horizontal([Constraint::Min(20), Constraint::Length(right_width as u16)])
                    .split(area);
            frame.render_widget(Paragraph::new(Line::from(hints)), parts[0]);
            frame.render_widget(
                Paragraph::new(
                    Line::from(Span::styled(right, palette.dim())).alignment(Alignment::Right),
                ),
                parts[1],
            );
            return;
        }
    }
    frame.render_widget(Paragraph::new(Line::from(hints)), area);
}

fn draw_help(frame: &mut Frame, app: &App, area: Rect) {
    let palette = Palette::new(app.opts.color);
    let mut lines: Vec<Line<'static>> = Vec::new();
    for (key, description) in [
        ("q / Ctrl-C", "quit"),
        ("Esc", "close help / clear filter"),
        ("Tab / Shift-Tab", "Sessions / Orphans / Ports"),
        ("1 / 2 / 3", "jump to a view"),
        ("j / k, ↑ / ↓", "move the selection"),
        ("g / G", "first / last row"),
        ("PgUp / PgDn", "move one page"),
        ("Space", "fold a session"),
        ("← / h , → / l", "collapse / expand"),
        ("E", "fold or unfold everything"),
        ("/", "filter rows"),
        ("s / S , r", "sort column / reverse"),
        ("Enter", "toggle the details pane"),
        ("y", "copy kill -TERM <pid>"),
        ("R", "refresh now"),
        ("p", "pause sampling"),
        ("+ / -", "interval 1–10 s"),
        ("?", "close this help"),
    ] {
        lines.push(Line::from(vec![
            Span::styled(
                format!(" {key:<16}"),
                palette.fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
            Span::styled(description, palette.dim()),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        " read-only: agents-ps never sends signals",
        palette.bold(),
    )));
    lines.push(Line::from(Span::styled(
        " Live alerts & one-click cleanup: Agents Monitor — agentmonitor.haziapp.com",
        palette.dim(),
    )));

    let width = 80.min(area.width.saturating_sub(2)).max(20);
    let height = (lines.len() as u16 + 2).min(area.height);
    let popup = centered(area, width, height);
    frame.render_widget(Clear, popup);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(palette.focus_border())
        .title(Line::from(Span::styled(" Help ", palette.bold())));
    frame.render_widget(Paragraph::new(lines).block(block), popup);
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

fn render_message(frame: &mut Frame, area: Rect, text: &str, style: Style) {
    if area.height == 0 {
        return;
    }
    let parts = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Fill(1),
    ])
    .split(area);
    frame.render_widget(
        Paragraph::new(Line::from(text.to_string()).alignment(Alignment::Center)).style(style),
        parts[1],
    );
}

fn right_cell(text: String, style: Style) -> Cell<'static> {
    Cell::from(Line::from(Span::styled(text, style)).alignment(Alignment::Right))
}

/// Column widths exactly as the table lays them out: one cell for the
/// selection symbol, one space between columns, and `Fill` takes the rest.
fn column_widths(widths: &[Constraint], total: u16) -> Vec<usize> {
    Layout::horizontal(widths.to_vec())
        .spacing(1)
        .split(Rect::new(0, 0, total.saturating_sub(1), 1))
        .iter()
        .map(|rect| rect.width as usize)
        .collect()
}

/// Join styled pieces into one line, cutting at `width` with a trailing `…`.
fn fit_line(parts: Vec<(String, Style)>, width: usize) -> Line<'static> {
    if width == 0 {
        return Line::default();
    }
    let total: usize = parts.iter().map(|(text, _)| text.chars().count()).sum();
    if total <= width {
        return Line::from(
            parts
                .into_iter()
                .map(|(text, style)| Span::styled(text, style))
                .collect::<Vec<_>>(),
        );
    }
    let budget = width - 1;
    let mut spans = Vec::new();
    let mut used = 0;
    for (text, style) in parts {
        if used >= budget {
            break;
        }
        let take = text.chars().count().min(budget - used);
        let cut: String = text.chars().take(take).collect();
        used += take;
        spans.push(Span::styled(cut, style));
    }
    spans.push(Span::styled("…", Style::new()));
    Line::from(spans)
}

/// Whole port numbers that fit `width`, then `+N`; a number is never split.
fn ports_text(ports: &[u16], width: usize) -> String {
    let mut shown = String::new();
    let mut count = 0;
    for (index, port) in ports.iter().enumerate() {
        let hidden = ports.len() - index - 1;
        let mut candidate = shown.clone();
        if !candidate.is_empty() {
            candidate.push_str(", ");
        }
        candidate.push_str(&port.to_string());
        let suffix = if hidden > 0 {
            format!(" +{hidden}")
        } else {
            String::new()
        };
        if candidate.chars().count() + suffix.chars().count() > width {
            break;
        }
        shown = candidate;
        count += 1;
    }
    let hidden = ports.len() - count;
    if count == 0 {
        format!("+{hidden}")
    } else if hidden > 0 {
        format!("{shown} +{hidden}")
    } else {
        shown
    }
}

fn visible_maxima(app: &App) -> (f32, u64) {
    let Some(snapshot) = app.snap.as_ref() else {
        return (0.0, 0);
    };
    let mut max_cpu = 0.0_f32;
    let mut max_rss = 0_u64;
    for row in &app.session_rows {
        let (cpu, rss) = match row.proc_index {
            None => match snapshot.sessions.get(row.session) {
                Some(session) => (session.cpu_pct, session.rss_bytes),
                None => continue,
            },
            Some(index) => match snapshot
                .sessions
                .get(row.session)
                .and_then(|session| session.processes.get(index))
            {
                Some(process) => (process.cpu_pct, process.rss_bytes),
                None => continue,
            },
        };
        if cpu > max_cpu {
            max_cpu = cpu;
        }
        if rss > max_rss {
            max_rss = rss;
        }
    }
    (max_cpu, max_rss)
}

fn split_first_word(command: &str) -> (String, String) {
    match command.find(' ') {
        Some(index) => (command[..index].to_string(), command[index..].to_string()),
        None => (command.to_string(), String::new()),
    }
}

fn abbreviate_home(path: &str) -> String {
    let Some(home) = std::env::var_os("HOME") else {
        return path.to_string();
    };
    let home = home.to_string_lossy();
    if let Some(rest) = path.strip_prefix(home.as_ref()) {
        if rest.is_empty() {
            return "~".to_string();
        }
        if rest.starts_with('/') {
            return format!("~{rest}");
        }
    }
    path.to_string()
}

fn line_width(spans: &[Span<'_>]) -> usize {
    spans.iter().map(|span| span.content.chars().count()).sum()
}

fn plural<'a>(count: usize, one: &'a str, many: &'a str) -> &'a str {
    if count == 1 { one } else { many }
}

/// The table HISTORY column is the same sparkline string, just shorter.
fn spark(points: &[f32], width: usize) -> String {
    super::app::spark(points, width)
}

/// Header sparkline: one cell per sample up to `HEADER_HISTORY`, with no left
/// padding — the header is free-flowing text, not a fixed column.
fn header_spark(points: &[f32]) -> String {
    spark(points, points.len().min(HEADER_HISTORY))
}
