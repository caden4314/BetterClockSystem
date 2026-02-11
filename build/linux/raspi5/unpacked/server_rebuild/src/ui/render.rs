use chrono::{DateTime, Local, Timelike};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Clear, LineGauge, Paragraph, Row, Table};

use crate::alarm::model::AlarmSettings;
use crate::alarm::scheduler::{
    AlarmStatus, RuntimeAlarm, TimeDisplayMode, format_next_occurrence_with_mode,
};
use crate::diagnostics::FrameStats;
use crate::time_provider::TimeSample;

pub struct UiSnapshot<'a> {
    pub sample: &'a TimeSample,
    pub local_now: DateTime<Local>,
    pub alarms: &'a [RuntimeAlarm],
    pub sim_stats: &'a FrameStats,
    pub render_stats: &'a FrameStats,
    pub resolution_hint_ps: u64,
    pub source_label: &'a str,
    pub fallback_reason: Option<&'a str>,
    pub status_message: Option<&'a str>,
    pub sim_target_fps: u16,
    pub render_target_fps: u16,
    pub use_24h: bool,
    pub command_mode: bool,
    pub command_buffer: &'a str,
    pub selected_alarm_index: usize,
    pub alarm_settings: &'a AlarmSettings,
    pub menu: Option<MenuOverlay<'a>>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum MenuScreen {
    Main,
    AddTimer,
    RemoveTimer,
}

pub struct MenuOverlay<'a> {
    pub screen: MenuScreen,
    pub selected: usize,
    pub duration_input: &'a str,
    pub id_input: &'a str,
    pub auto_acknowledge: bool,
}

const PANEL_BG: Color = Color::Rgb(16, 24, 34);
const PANEL_ALT_BG: Color = Color::Rgb(12, 20, 30);
const BORDER: Color = Color::Rgb(68, 98, 122);
const ACCENT: Color = Color::Rgb(89, 204, 184);
const CLOCK_MAIN: Color = Color::Rgb(255, 204, 96);
const MUTED: Color = Color::Rgb(150, 171, 191);
const OK: Color = Color::Rgb(104, 218, 131);
const WARN: Color = Color::Rgb(255, 187, 99);
const ALERT: Color = Color::Rgb(255, 106, 106);

pub fn draw(frame: &mut Frame<'_>, snapshot: &UiSnapshot<'_>) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([
            Constraint::Length(8),
            Constraint::Min(10),
            Constraint::Length(6),
        ])
        .split(frame.area());

    let top_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(68), Constraint::Percentage(32)])
        .split(chunks[0]);
    let bottom_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(65), Constraint::Percentage(35)])
        .split(chunks[2]);

    render_clock_panel(frame, top_chunks[0], snapshot);
    render_system_panel(frame, top_chunks[1], snapshot);
    render_alarm_table(frame, chunks[1], snapshot);
    render_runtime_panel(frame, bottom_chunks[0], snapshot);
    render_fps_panel(frame, bottom_chunks[1], snapshot);
    if let Some(menu) = &snapshot.menu {
        render_menu_overlay(frame, snapshot, menu);
    }
}

fn render_clock_panel(frame: &mut Frame<'_>, area: Rect, snapshot: &UiSnapshot<'_>) {
    let ms = snapshot.sample.nanos / 1_000_000;
    let us = (snapshot.sample.nanos / 1_000) % 1_000;
    let ns = snapshot.sample.nanos % 1_000;
    let (hour_text, meridiem_text) = if snapshot.use_24h {
        (format!("{:02}", snapshot.local_now.hour()), "")
    } else {
        let (is_pm, hour12) = snapshot.local_now.hour12();
        (format!("{:02}", hour12), if is_pm { " PM" } else { " AM" })
    };

    let title = Line::from(vec![
        Span::styled(
            " BetterClock ",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(
            format!("[{}]", snapshot.source_label),
            Style::default().fg(CLOCK_MAIN).add_modifier(Modifier::BOLD),
        ),
    ]);

    let date_line = Line::from(Span::styled(
        snapshot.local_now.format("%A, %B %d %Y").to_string(),
        Style::default().fg(MUTED),
    ));

    let clock_line = Line::from(vec![
        Span::styled(
            format!(
                "{}:{:02}:{:02}{}",
                hour_text,
                snapshot.local_now.minute(),
                snapshot.local_now.second(),
                meridiem_text
            ),
            Style::default().fg(CLOCK_MAIN).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(format!("ms {:03}", ms), Style::default().fg(ACCENT)),
        Span::raw("  "),
        Span::styled(format!("us {:03}", us), Style::default().fg(ACCENT)),
        Span::raw("  "),
        Span::styled(format!("ns {:03}", ns), Style::default().fg(ACCENT)),
    ]);

    let precision_mode = if snapshot.sample.is_measured_picos {
        "Hardware timing"
    } else {
        "Software timing"
    };
    let time_mode = if snapshot.use_24h {
        "24-hour display"
    } else {
        "12-hour display"
    };
    let mode_line = Line::from(vec![
        Span::styled(precision_mode, Style::default().fg(MUTED)),
        Span::raw(" | "),
        Span::styled(time_mode, Style::default().fg(ACCENT)),
    ]);

    let panel = Paragraph::new(vec![date_line, Line::default(), clock_line, mode_line])
        .alignment(Alignment::Center)
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(BORDER))
                .style(Style::default().bg(PANEL_BG)),
        );
    frame.render_widget(panel, area);
}

fn render_system_panel(frame: &mut Frame<'_>, area: Rect, snapshot: &UiSnapshot<'_>) {
    let mut enabled = 0usize;
    let mut triggered = 0usize;
    for alarm in snapshot.alarms {
        match alarm.status() {
            AlarmStatus::Triggered => {
                enabled += 1;
                triggered += 1;
            }
            AlarmStatus::Disabled => {}
            AlarmStatus::Armed | AlarmStatus::Next => enabled += 1,
        }
    }
    let quantization_ns = snapshot.resolution_hint_ps as f64 / 1_000.0;
    let warning_status = if snapshot.alarm_settings.warning_enabled {
        "ON"
    } else {
        "OFF"
    };

    let source_line = Line::from(vec![
        Span::styled("Source: ", Style::default().fg(MUTED)),
        Span::styled(
            snapshot.sample.source,
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
    ]);
    let alarms_line = Line::from(vec![
        Span::styled("Armed: ", Style::default().fg(MUTED)),
        Span::styled(
            enabled.to_string(),
            Style::default().fg(OK).add_modifier(Modifier::BOLD),
        ),
        Span::raw("   "),
        Span::styled("Triggered: ", Style::default().fg(MUTED)),
        Span::styled(
            triggered.to_string(),
            Style::default().fg(ALERT).add_modifier(Modifier::BOLD),
        ),
    ]);
    let precision_line = Line::from(vec![
        Span::styled("Resolution hint: ", Style::default().fg(MUTED)),
        Span::styled(
            format!("{quantization_ns:.1} ns"),
            Style::default().fg(WARN),
        ),
    ]);
    let quantized_line = Line::from(vec![
        Span::styled("Observed step: ", Style::default().fg(MUTED)),
        Span::styled(
            format!("~{:.1} ns increments", quantization_ns),
            Style::default().fg(ACCENT),
        ),
    ]);
    let warning_line = Line::from(vec![
        Span::styled("Warning cfg: ", Style::default().fg(MUTED)),
        Span::styled(
            format!(
                "{warning_status} | lead {} ms | pulse {} ms",
                snapshot.alarm_settings.warning_lead_time_ms,
                snapshot.alarm_settings.warning_pulse_time_ms
            ),
            Style::default().fg(CLOCK_MAIN),
        ),
    ]);

    let mode_badge = if snapshot.sample.is_measured_picos {
        Span::styled(
            "HW_PICO",
            Style::default()
                .fg(Color::Black)
                .bg(OK)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(
            "SW_NANO_DERIVED",
            Style::default()
                .fg(Color::Black)
                .bg(WARN)
                .add_modifier(Modifier::BOLD),
        )
    };
    let mode_line = Line::from(vec![
        Span::styled("Mode: ", Style::default().fg(MUTED)),
        mode_badge,
    ]);

    let panel = Paragraph::new(vec![
        source_line,
        mode_line,
        alarms_line,
        precision_line,
        quantized_line,
        warning_line,
    ])
    .block(
        Block::default()
            .title(Line::from(Span::styled(
                " System ",
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            )))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(BORDER))
            .style(Style::default().bg(PANEL_ALT_BG)),
    );
    frame.render_widget(panel, area);
}

fn render_alarm_table(frame: &mut Frame<'_>, area: Rect, snapshot: &UiSnapshot<'_>) {
    let time_mode = if snapshot.use_24h {
        TimeDisplayMode::Hour24
    } else {
        TimeDisplayMode::Hour12
    };
    let header = Row::new(vec![
        "ID",
        "STATUS",
        "ACK",
        "NEXT OCCURRENCE (LOCAL)",
        "TYPE",
    ])
    .style(
        Style::default()
            .fg(CLOCK_MAIN)
            .bg(PANEL_ALT_BG)
            .add_modifier(Modifier::BOLD),
    );
    let blink_on = snapshot.local_now.nanosecond() / 250_000_000 % 2 == 0;
    let rows = snapshot.alarms.iter().enumerate().map(|(idx, alarm)| {
        let (status_label, status_style) = match alarm.status() {
            AlarmStatus::Disabled => ("DISABLED", Style::default().fg(MUTED)),
            AlarmStatus::Armed => ("ARMED", Style::default().fg(ACCENT)),
            AlarmStatus::Next => ("NEXT", Style::default().fg(OK).add_modifier(Modifier::BOLD)),
            AlarmStatus::Triggered => {
                let base = Style::default().fg(Color::Black).bg(ALERT);
                if blink_on {
                    ("TRIGGERED", base.add_modifier(Modifier::BOLD))
                } else {
                    ("TRIGGERED", base)
                }
            }
        };

        let kind = match alarm.alarm.schedule {
            crate::alarm::model::AlarmSchedule::OneTime { .. } => "ONE_TIME",
            crate::alarm::model::AlarmSchedule::Recurring { .. } => "RECURRING",
        };
        let ack = if alarm.alarm.auto_acknowledge {
            "AUTO"
        } else {
            "MANUAL"
        };

        let base_row_style = if idx % 2 == 0 {
            Style::default().bg(PANEL_BG)
        } else {
            Style::default().bg(PANEL_ALT_BG)
        };
        let row_style = if idx == snapshot.selected_alarm_index {
            base_row_style
                .fg(Color::White)
                .add_modifier(Modifier::REVERSED)
        } else {
            base_row_style
        };

        Row::new(vec![
            Cell::from(Span::styled(
                alarm.alarm.id.clone(),
                Style::default().fg(MUTED).add_modifier(Modifier::BOLD),
            )),
            Cell::from(Span::styled(status_label, status_style)),
            Cell::from(Span::styled(
                ack,
                if alarm.alarm.auto_acknowledge {
                    Style::default().fg(OK)
                } else {
                    Style::default().fg(MUTED)
                },
            )),
            Cell::from(Span::styled(
                format_next_occurrence_with_mode(alarm.next_occurrence, time_mode),
                Style::default().fg(Color::White),
            )),
            Cell::from(Span::styled(kind, Style::default().fg(ACCENT))),
        ])
        .style(row_style)
    });

    let table = Table::new(
        rows,
        [
            Constraint::Length(24),
            Constraint::Length(12),
            Constraint::Length(8),
            Constraint::Min(30),
            Constraint::Length(12),
        ],
    )
    .header(header)
    .block(
        Block::default()
            .title(Line::from(Span::styled(
                " Alarms (read-only from alarms.json) ",
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            )))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(BORDER)),
    )
    .column_spacing(2);
    frame.render_widget(table, area);
}

fn render_runtime_panel(frame: &mut Frame<'_>, area: Rect, snapshot: &UiSnapshot<'_>) {
    let sim_width = fps_padding_width(snapshot.sim_target_fps);
    let render_width = fps_padding_width(snapshot.render_target_fps);
    let sim_instant = format_fps_padded(snapshot.sim_stats.instant_fps(), sim_width, None);
    let sim_rolling = format_fps_padded(
        snapshot.sim_stats.rolling_fps(),
        sim_width,
        Some(snapshot.sim_target_fps),
    );
    let render_instant = format_fps_padded(snapshot.render_stats.instant_fps(), render_width, None);
    let render_rolling = format_fps_padded(
        snapshot.render_stats.rolling_fps(),
        render_width,
        Some(snapshot.render_target_fps),
    );
    let mut lines = vec![
        Line::from(vec![
            Span::styled("Sim FPS ", Style::default().fg(MUTED)),
            Span::styled(
                format!("{sim_instant}/{sim_rolling}"),
                Style::default().fg(CLOCK_MAIN).add_modifier(Modifier::BOLD),
            ),
            Span::raw("   "),
            Span::styled("Render FPS ", Style::default().fg(MUTED)),
            Span::styled(
                format!("{render_instant}/{render_rolling}"),
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            Span::raw("   "),
            Span::styled("Sim drops ", Style::default().fg(MUTED)),
            Span::styled(
                snapshot.sim_stats.dropped_frames().to_string(),
                Style::default().fg(WARN).add_modifier(Modifier::BOLD),
            ),
            Span::raw("   "),
            Span::styled("Render target ", Style::default().fg(MUTED)),
            Span::styled(
                snapshot.render_target_fps.to_string(),
                Style::default().fg(CLOCK_MAIN).add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("Keys: ", Style::default().fg(MUTED)),
            Span::styled("[q]", Style::default().fg(CLOCK_MAIN)),
            Span::raw(" quit  "),
            Span::styled("[a]", Style::default().fg(CLOCK_MAIN)),
            Span::raw(" acknowledge  "),
            Span::styled("[r]", Style::default().fg(CLOCK_MAIN)),
            Span::raw(" reload disabled (restart required)"),
            Span::raw("  "),
            Span::styled("[t]", Style::default().fg(CLOCK_MAIN)),
            Span::raw(" 12h/24h"),
            Span::raw("  "),
            Span::styled("[:]", Style::default().fg(CLOCK_MAIN)),
            Span::raw(" command"),
            Span::raw("  "),
            Span::styled("[m]", Style::default().fg(CLOCK_MAIN)),
            Span::raw(" menu"),
        ]),
        Line::from(vec![
            Span::styled("Legend: ", Style::default().fg(MUTED)),
            Span::styled(
                "Sim/Render shown as instant/rolling FPS (rolling clamped to target)",
                Style::default().fg(ACCENT),
            ),
        ]),
    ];

    if let Some(reason) = snapshot.fallback_reason {
        lines.push(Line::from(Span::styled(
            format!("Fallback: {reason}"),
            Style::default().fg(WARN),
        )));
    } else if let Some(message) = snapshot.status_message {
        lines.push(Line::from(Span::styled(
            message,
            Style::default().fg(OK).add_modifier(Modifier::BOLD),
        )));
    }
    if snapshot.command_mode {
        lines.push(Line::from(vec![
            Span::styled(
                "CMD> ",
                Style::default().fg(CLOCK_MAIN).add_modifier(Modifier::BOLD),
            ),
            Span::styled(snapshot.command_buffer, Style::default().fg(Color::White)),
        ]));
    }

    let footer = Paragraph::new(lines).block(
        Block::default()
            .title(Line::from(Span::styled(
                " Runtime ",
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            )))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(BORDER))
            .style(Style::default().bg(PANEL_BG)),
    );
    frame.render_widget(footer, area);
}

fn render_fps_panel(frame: &mut Frame<'_>, area: Rect, snapshot: &UiSnapshot<'_>) {
    let container = Block::default()
        .title(Line::from(Span::styled(
            " FPS ",
            Style::default().fg(CLOCK_MAIN).add_modifier(Modifier::BOLD),
        )))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(BORDER))
        .style(Style::default().bg(PANEL_ALT_BG));
    let inner = container.inner(area);
    frame.render_widget(container, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
        ])
        .split(inner);

    let sim_ratio =
        (snapshot.sim_stats.rolling_fps() / f64::from(snapshot.sim_target_fps)).clamp(0.0, 1.0);
    let sim_width = fps_padding_width(snapshot.sim_target_fps);
    let sim_color = if sim_ratio >= 0.98 {
        OK
    } else if sim_ratio >= 0.80 {
        WARN
    } else {
        ALERT
    };
    let sim_gauge = LineGauge::default()
        .ratio(sim_ratio)
        .label(format!(
            "SIM {}/{}",
            format_fps_padded(
                snapshot.sim_stats.rolling_fps(),
                sim_width,
                Some(snapshot.sim_target_fps)
            ),
            format_fps_padded(f64::from(snapshot.sim_target_fps), sim_width, None)
        ))
        .line_set(ratatui::symbols::line::THICK)
        .filled_style(Style::default().fg(sim_color).add_modifier(Modifier::BOLD))
        .unfilled_style(Style::default().fg(BORDER));
    frame.render_widget(sim_gauge, chunks[0]);

    let render_ratio = (snapshot.render_stats.rolling_fps()
        / f64::from(snapshot.render_target_fps))
    .clamp(0.0, 1.0);
    let render_width = fps_padding_width(snapshot.render_target_fps);
    let render_color = if render_ratio >= 0.98 {
        OK
    } else if render_ratio >= 0.80 {
        WARN
    } else {
        ALERT
    };
    let render_gauge = LineGauge::default()
        .ratio(render_ratio)
        .label(format!(
            "RENDER {}/{}",
            format_fps_padded(
                snapshot.render_stats.rolling_fps(),
                render_width,
                Some(snapshot.render_target_fps)
            ),
            format_fps_padded(f64::from(snapshot.render_target_fps), render_width, None)
        ))
        .line_set(ratatui::symbols::line::THICK)
        .filled_style(
            Style::default()
                .fg(render_color)
                .add_modifier(Modifier::BOLD),
        )
        .unfilled_style(Style::default().fg(BORDER));
    frame.render_widget(render_gauge, chunks[1]);

    let labels = Paragraph::new("SIM (top) / RENDER (bottom)")
        .style(Style::default().fg(MUTED))
        .alignment(Alignment::Center);
    frame.render_widget(labels, chunks[2]);
}

fn fps_padding_width(target: u16) -> usize {
    let digits = target.max(1).to_string().len();
    digits.max(3)
}

fn format_fps_padded(value: f64, width: usize, clamp_to_target: Option<u16>) -> String {
    let capped = clamp_to_target
        .map(|target| value.min(f64::from(target)))
        .unwrap_or(value);
    let clamped = capped.clamp(0.0, 99_999.0).round() as u32;
    format!("{clamped:0width$}")
}

fn render_menu_overlay(frame: &mut Frame<'_>, snapshot: &UiSnapshot<'_>, menu: &MenuOverlay<'_>) {
    let area = centered_rect(64, 58, frame.area());
    frame.render_widget(Clear, area);
    let block = Block::default()
        .title(Line::from(Span::styled(
            " Alarm Menu ",
            Style::default().fg(CLOCK_MAIN).add_modifier(Modifier::BOLD),
        )))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT))
        .style(Style::default().bg(PANEL_BG));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    match menu.screen {
        MenuScreen::Main => {
            let rows = vec![
                "Add Timer",
                "Remove Alarm",
                "Toggle Auto-Ack (Selected Alarm)",
                "Close",
            ];
            let mut lines = vec![
                Line::from(Span::styled(
                    "Use Up/Down + Enter. Esc closes.",
                    Style::default().fg(MUTED),
                )),
                Line::default(),
            ];
            for (idx, label) in rows.iter().enumerate() {
                lines.push(menu_button_line(idx == menu.selected, label));
            }
            let panel = Paragraph::new(lines);
            frame.render_widget(panel, inner);
        }
        MenuScreen::AddTimer => {
            let lines = vec![
                Line::from(Span::styled(
                    "Add Timer",
                    Style::default().fg(CLOCK_MAIN).add_modifier(Modifier::BOLD),
                )),
                Line::from(Span::styled(
                    "Edit fields with keyboard. Enter activates selected button.",
                    Style::default().fg(MUTED),
                )),
                Line::default(),
                menu_field_line(menu.selected == 0, "Duration", menu.duration_input),
                menu_field_line(menu.selected == 1, "ID (optional)", menu.id_input),
                menu_toggle_line(
                    menu.selected == 2,
                    "Auto acknowledge",
                    menu.auto_acknowledge,
                ),
                Line::default(),
                menu_button_line(menu.selected == 3, "Create Timer"),
                menu_button_line(menu.selected == 4, "Back"),
            ];
            let panel = Paragraph::new(lines);
            frame.render_widget(panel, inner);
        }
        MenuScreen::RemoveTimer => {
            let mut lines = vec![
                Line::from(Span::styled(
                    "Remove Alarm",
                    Style::default().fg(CLOCK_MAIN).add_modifier(Modifier::BOLD),
                )),
                Line::from(Span::styled(
                    "Select alarm and press Enter to remove.",
                    Style::default().fg(MUTED),
                )),
                Line::default(),
            ];
            if snapshot.alarms.is_empty() {
                lines.push(Line::from(Span::styled(
                    "No alarms available.",
                    Style::default().fg(WARN),
                )));
            } else {
                for (idx, runtime) in snapshot.alarms.iter().enumerate() {
                    lines.push(menu_button_line(
                        idx == menu.selected,
                        &format!(
                            "{} [{}]",
                            runtime.alarm.id,
                            if runtime.alarm.enabled { "on" } else { "off" }
                        ),
                    ));
                }
            }
            lines.push(Line::default());
            lines.push(menu_button_line(
                menu.selected == snapshot.alarms.len(),
                "Back",
            ));
            let panel = Paragraph::new(lines);
            frame.render_widget(panel, inner);
        }
    }
}

fn menu_button_line(selected: bool, text: &str) -> Line<'static> {
    let (prefix, style) = if selected {
        (
            ">> ",
            Style::default()
                .fg(Color::Black)
                .bg(CLOCK_MAIN)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        ("   ", Style::default().fg(ACCENT))
    };
    Line::from(Span::styled(format!("{prefix}{text}"), style))
}

fn menu_field_line(selected: bool, label: &str, value: &str) -> Line<'static> {
    let style = if selected {
        Style::default()
            .fg(Color::Black)
            .bg(CLOCK_MAIN)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(MUTED)
    };
    Line::from(Span::styled(format!("{label}: {value}"), style))
}

fn menu_toggle_line(selected: bool, label: &str, value: bool) -> Line<'static> {
    let style = if selected {
        Style::default()
            .fg(Color::Black)
            .bg(CLOCK_MAIN)
            .add_modifier(Modifier::BOLD)
    } else if value {
        Style::default().fg(OK)
    } else {
        Style::default().fg(WARN)
    };
    let text = if value { "ON" } else { "OFF" };
    Line::from(Span::styled(format!("{label}: {text}"), style))
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}
