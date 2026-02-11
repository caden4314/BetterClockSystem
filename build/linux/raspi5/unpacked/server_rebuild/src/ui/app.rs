use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use chrono::{DateTime, Local, NaiveDateTime, NaiveTime, Timelike, Weekday};
use eframe::egui::{
    self, Align, Color32, Layout, RichText, ScrollArea, TextEdit, TopBottomPanel, Ui,
};

use crate::alarm::model::{Alarm, AlarmSchedule, AlarmSettings, save_alarm_config};
use crate::alarm::scheduler::{
    AlarmScheduler, AlarmStatus, TimeDisplayMode, format_next_occurrence_with_mode,
};
use crate::api::{ApiSharedState, DEFAULT_CONNECTED_CLIENT_TTL_MS, PublicClient};
use crate::diagnostics::{FrameStats, sleep_until};
use crate::time_provider::{SelectedTimeProvider, TimeSample};

const MAX_SIM_STEPS_PER_UPDATE: usize = 1024;
const SIM_FPS_ROLLING_WINDOW: Duration = Duration::from_secs(2);

pub fn run_gui(
    selected_provider: SelectedTimeProvider,
    scheduler: AlarmScheduler,
    sim_fps: u16,
    render_fps: u16,
    alarm_file: PathBuf,
    settings: AlarmSettings,
    api_state: Option<Arc<Mutex<ApiSharedState>>>,
    api_bind: String,
    api_port: u16,
) -> Result<()> {
    let native_options = eframe::NativeOptions {
        vsync: false,
        viewport: egui::ViewportBuilder::default()
            .with_title("BetterClock")
            .with_inner_size([1360.0, 860.0])
            .with_min_inner_size([1080.0, 700.0]),
        ..Default::default()
    };

    let app = BetterClockApp::new(
        selected_provider,
        scheduler,
        sim_fps,
        render_fps,
        alarm_file,
        settings,
        api_state,
        api_bind,
        api_port,
    )?;

    eframe::run_native(
        "BetterClock",
        native_options,
        Box::new(move |cc| {
            configure_theme(&cc.egui_ctx);
            Ok(Box::new(app))
        }),
    )
    .map_err(|err| anyhow::anyhow!("failed to launch BetterClock GUI: {err}"))?;

    Ok(())
}

fn configure_theme(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.override_text_color = Some(Color32::from_rgb(226, 234, 246));
    visuals.panel_fill = Color32::from_rgb(8, 16, 26);
    visuals.window_fill = Color32::from_rgb(12, 20, 32);
    visuals.widgets.noninteractive.bg_fill = Color32::from_rgb(10, 18, 30);
    visuals.widgets.inactive.bg_fill = Color32::from_rgb(16, 24, 38);
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(26, 42, 62);
    visuals.widgets.active.bg_fill = Color32::from_rgb(34, 60, 88);
    visuals.selection.bg_fill = Color32::from_rgb(43, 148, 178);
    visuals.hyperlink_color = Color32::from_rgb(95, 220, 208);
    ctx.set_visuals(visuals);
}

#[derive(Debug, Clone)]
struct AlarmRow {
    id: String,
    status: AlarmStatus,
    next_occurrence_text: String,
    late_trigger_ms: u64,
    ring_duration_ms: u64,
    kind_text: &'static str,
}

struct BetterClockApp {
    selected_provider: SelectedTimeProvider,
    scheduler: AlarmScheduler,
    sim_target_fps: u16,
    render_target_fps: u16,
    sim_cap_enabled: bool,
    render_cap_enabled: bool,
    alarm_file: PathBuf,
    settings: AlarmSettings,
    use_24h: bool,
    selected_alarm_index: usize,
    status_message: Option<(String, Instant)>,
    next_alarm_id: u64,
    latest_sample: TimeSample,
    latest_now_local: DateTime<Local>,
    sim_step: Duration,
    render_step: Duration,
    next_sim_tick: Instant,
    next_render_tick: Instant,
    sim_rate_last: Instant,
    sim_rate_window: VecDeque<(Instant, u32)>,
    sim_instant_fps: f64,
    sim_rolling_fps: f64,
    sim_stats: FrameStats,
    render_stats: FrameStats,
    last_sim_done: Option<Instant>,
    last_render_done: Option<Instant>,
    timer_duration_input: String,
    timer_id_input: String,
    timer_ring_duration_ms: u64,
    once_datetime_input: String,
    once_id_input: String,
    once_late_trigger_ms: u64,
    once_ring_duration_ms: u64,
    daily_time_input: String,
    daily_id_input: String,
    daily_late_trigger_ms: u64,
    daily_ring_duration_ms: u64,
    daily_weekdays: [bool; 7],
    api_state: Option<Arc<Mutex<ApiSharedState>>>,
    api_bind: String,
    api_port: u16,
    next_api_publish: Instant,
}

impl BetterClockApp {
    fn new(
        selected_provider: SelectedTimeProvider,
        scheduler: AlarmScheduler,
        sim_target_fps: u16,
        render_target_fps: u16,
        alarm_file: PathBuf,
        settings: AlarmSettings,
        api_state: Option<Arc<Mutex<ApiSharedState>>>,
        api_bind: String,
        api_port: u16,
    ) -> Result<Self> {
        let sample = selected_provider.provider.now()?;
        let now_local = sample.to_local_datetime()?;
        let now_instant = Instant::now();
        let safe_sim_target = sim_target_fps.max(1);
        let safe_render_target = render_target_fps.max(1);
        Ok(Self {
            selected_provider,
            scheduler,
            sim_target_fps: safe_sim_target,
            render_target_fps: safe_render_target,
            sim_cap_enabled: true,
            render_cap_enabled: true,
            alarm_file,
            settings,
            use_24h: true,
            selected_alarm_index: 0,
            status_message: None,
            next_alarm_id: 1,
            latest_sample: sample,
            latest_now_local: now_local,
            sim_step: Duration::from_secs_f64(1.0 / f64::from(safe_sim_target)),
            render_step: Duration::from_secs_f64(1.0 / f64::from(safe_render_target)),
            next_sim_tick: now_instant,
            next_render_tick: now_instant,
            sim_rate_last: now_instant,
            sim_rate_window: VecDeque::with_capacity(256),
            sim_instant_fps: 0.0,
            sim_rolling_fps: 0.0,
            sim_stats: FrameStats::new(
                300,
                Duration::from_secs_f64(1.0 / f64::from(safe_sim_target)),
            ),
            render_stats: FrameStats::new(
                300,
                Duration::from_secs_f64(1.0 / f64::from(safe_render_target)),
            ),
            last_sim_done: None,
            last_render_done: None,
            timer_duration_input: "10s".to_string(),
            timer_id_input: String::new(),
            timer_ring_duration_ms: 5_000,
            once_datetime_input: Local::now().format("%Y-%m-%dT%H:%M:%S").to_string(),
            once_id_input: String::new(),
            once_late_trigger_ms: 0,
            once_ring_duration_ms: 5_000,
            daily_time_input: "09:30:00".to_string(),
            daily_id_input: String::new(),
            daily_late_trigger_ms: 0,
            daily_ring_duration_ms: 5_000,
            daily_weekdays: [true, true, true, true, true, false, false],
            api_state,
            api_bind,
            api_port,
            next_api_publish: Instant::now(),
        })
    }

    fn set_status(&mut self, text: impl Into<String>, ttl: Duration) {
        self.status_message = Some((text.into(), Instant::now() + ttl));
    }

    fn persist_scheduler(&self) -> Result<()> {
        let alarms = self.scheduler.export_alarms();
        save_alarm_config(&self.alarm_file, &alarms, &self.settings)
    }

    fn update_targets(&mut self) {
        self.sim_target_fps = self.sim_target_fps.max(1);
        self.render_target_fps = self.render_target_fps.max(1);

        self.sim_step = Duration::from_secs_f64(1.0 / f64::from(self.sim_target_fps));
        self.render_step = Duration::from_secs_f64(1.0 / f64::from(self.render_target_fps));
        self.sim_stats.set_target_frame(self.sim_step);
        self.render_stats.set_target_frame(self.render_step);
        self.reset_pacing_anchors();
    }

    fn reset_pacing_anchors(&mut self) {
        let now = Instant::now();
        self.next_sim_tick = now + self.sim_step;
        self.next_render_tick = now + self.render_step;
        self.sim_rate_last = now;
        self.sim_rate_window.clear();
        self.sim_instant_fps = 0.0;
        self.sim_rolling_fps = 0.0;
    }

    fn enforce_render_cap(&mut self) {
        if !self.render_cap_enabled {
            self.next_render_tick = Instant::now();
            return;
        }

        let now = Instant::now();
        if now < self.next_render_tick {
            sleep_until(self.next_render_tick);
        }

        let wake = Instant::now();
        while self.next_render_tick <= wake {
            self.next_render_tick += self.render_step;
        }
    }

    fn simulate_single_step(&mut self, now: Instant) -> Result<()> {
        self.latest_sample = self.selected_provider.provider.now()?;
        self.latest_now_local = self.latest_sample.to_local_datetime()?;
        let warning_outcome = self
            .scheduler
            .refresh_warnings(self.latest_now_local, &self.settings);
        let outcome = self.scheduler.tick(self.latest_now_local);
        if outcome.triggered > 0 || outcome.auto_acknowledged > 0 {
            let text = match (outcome.triggered, outcome.auto_acknowledged) {
                (t, a) if t > 0 && a > 0 => format!("{t} bells triggered, {a} auto-cleared."),
                (t, _) if t > 0 => format!("{t} bell(s) triggered."),
                (_, a) => format!("{a} bell(s) auto-cleared."),
            };
            self.set_status(text, Duration::from_secs(3));
        } else if warning_outcome.pulses > 0 && warning_outcome.active > 0 {
            self.set_status(
                format!(
                    "Warning pulse: {} bell(s) approaching trigger window.",
                    warning_outcome.active
                ),
                Duration::from_millis(self.settings.warning_pulse_time_ms.max(350)),
            );
        }

        if now >= self.next_api_publish {
            self.publish_api_state()?;
            self.next_api_publish = now + Duration::from_millis(50);
        }

        let sim_done = Instant::now();
        if self.sim_cap_enabled {
            self.sim_stats.record_frame(self.sim_step);
        } else if let Some(previous_done) = self.last_sim_done {
            self.sim_stats
                .record_frame(sim_done.saturating_duration_since(previous_done));
        }
        self.last_sim_done = Some(sim_done);
        Ok(())
    }

    fn record_sim_rate(&mut self, steps: u32, now: Instant) {
        let delta = now.saturating_duration_since(self.sim_rate_last);
        if !delta.is_zero() {
            self.sim_instant_fps = steps as f64 / delta.as_secs_f64();
        }
        self.sim_rate_last = now;
        self.sim_rate_window.push_back((now, steps));

        let cutoff = now.checked_sub(SIM_FPS_ROLLING_WINDOW).unwrap_or(now);
        while let Some((stamp, _)) = self.sim_rate_window.front() {
            if *stamp < cutoff {
                let _ = self.sim_rate_window.pop_front();
            } else {
                break;
            }
        }

        if let Some((oldest_stamp, _)) = self.sim_rate_window.front() {
            let span = now.saturating_duration_since(*oldest_stamp).as_secs_f64();
            let total_steps = self
                .sim_rate_window
                .iter()
                .map(|(_, step_count)| u64::from(*step_count))
                .sum::<u64>();
            self.sim_rolling_fps = if span > 0.0 {
                total_steps as f64 / span
            } else {
                self.sim_instant_fps
            };
        } else {
            self.sim_rolling_fps = 0.0;
        }
    }

    fn next_generated_id(&mut self, prefix: &str) -> String {
        let id = format!(
            "{prefix}-{}-{}",
            self.latest_now_local.timestamp(),
            self.next_alarm_id
        );
        self.next_alarm_id += 1;
        id
    }

    fn add_alarm(&mut self, alarm: Alarm) -> Result<()> {
        if self
            .scheduler
            .export_alarms()
            .iter()
            .any(|existing| existing.id == alarm.id)
        {
            bail!("bell id '{}' already exists", alarm.id);
        }
        self.scheduler.add_alarm(alarm, self.latest_now_local);
        self.persist_scheduler()?;
        self.selected_alarm_index = self.scheduler.len().saturating_sub(1);
        Ok(())
    }

    fn simulate(&mut self) -> Result<u32> {
        let mut now = Instant::now();
        let mut sim_steps = 0usize;
        if self.sim_cap_enabled {
            while now >= self.next_sim_tick && sim_steps < MAX_SIM_STEPS_PER_UPDATE {
                self.simulate_single_step(now)?;
                self.next_sim_tick += self.sim_step;
                sim_steps += 1;
                now = Instant::now();
            }

            if sim_steps == MAX_SIM_STEPS_PER_UPDATE && now >= self.next_sim_tick {
                let backlog = now.saturating_duration_since(self.next_sim_tick);
                let step_ns = self.sim_step.as_nanos().max(1);
                let skipped = (backlog.as_nanos() / step_ns) as u64;
                if skipped > 0 {
                    self.sim_stats.add_dropped(skipped);
                }
                self.next_sim_tick = now + self.sim_step;
            }
        } else {
            let budget_end = now + Duration::from_millis(2);
            while sim_steps < MAX_SIM_STEPS_PER_UPDATE {
                self.simulate_single_step(now)?;
                sim_steps += 1;
                now = Instant::now();
                if now >= budget_end {
                    break;
                }
            }
            self.next_sim_tick = now + self.sim_step;
        }

        Ok(sim_steps as u32)
    }

    fn publish_api_state(&self) -> Result<()> {
        let Some(shared) = &self.api_state else {
            return Ok(());
        };

        let mut triggered_count = 0usize;
        let mut armed_count = 0usize;
        let mut warning_active_count = 0usize;
        for alarm in self.scheduler.alarms() {
            match alarm.status() {
                AlarmStatus::Triggered => triggered_count += 1,
                AlarmStatus::Disabled => {}
                AlarmStatus::Warning => {
                    warning_active_count += 1;
                    armed_count += 1;
                }
                AlarmStatus::Armed | AlarmStatus::Next => armed_count += 1,
            }
        }

        let pulse_ms = self.settings.warning_pulse_time_ms.max(1);
        let pulse_on = self.settings.warning_enabled
            && warning_active_count > 0
            && (self
                .latest_now_local
                .timestamp_millis()
                .div_euclid(pulse_ms as i64)
                % 2
                == 0);

        let mut guard = shared
            .lock()
            .map_err(|_| anyhow::anyhow!("failed to lock API state"))?;
        guard.runtime.iso_local = self.latest_now_local.to_rfc3339();
        guard.runtime.hour = self.latest_now_local.hour();
        guard.runtime.minute = self.latest_now_local.minute();
        guard.runtime.second = self.latest_now_local.second();
        guard.runtime.source_label = self.selected_provider.label.to_string();
        guard.runtime.warning_enabled = self.settings.warning_enabled;
        guard.runtime.warning_active_count = warning_active_count;
        guard.runtime.warning_pulse_on = pulse_on;
        guard.runtime.warning_lead_time_ms = self.settings.warning_lead_time_ms;
        guard.runtime.warning_pulse_time_ms = self.settings.warning_pulse_time_ms;
        guard.runtime.triggered_count = triggered_count;
        guard.runtime.armed_count = armed_count;
        guard.runtime.updated_unix_ms = self.latest_now_local.timestamp_millis();
        Ok(())
    }

    fn show_header(&mut self, ui: &mut Ui) {
        let ms = self.latest_sample.nanos / 1_000_000;
        let us = (self.latest_sample.nanos / 1_000) % 1_000;
        let ns = self.latest_sample.nanos % 1_000;
        let warning_count = self
            .scheduler
            .alarms()
            .iter()
            .filter(|alarm| alarm.warning_active)
            .count();
        let (hour, suffix) = if self.use_24h {
            (format!("{:02}", self.latest_now_local.hour()), "")
        } else {
            let (is_pm, hour12) = self.latest_now_local.hour12();
            (format!("{:02}", hour12), if is_pm { " PM" } else { " AM" })
        };

        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new("BetterClock")
                    .size(26.0)
                    .color(Color32::from_rgb(96, 228, 206))
                    .strong(),
            );
            ui.separator();
            ui.label(
                RichText::new(format!(
                    "{}:{:02}:{:02}{suffix}",
                    hour,
                    self.latest_now_local.minute(),
                    self.latest_now_local.second(),
                ))
                .size(30.0)
                .color(Color32::from_rgb(255, 214, 117))
                .strong(),
            );
            ui.label(
                RichText::new(format!("ms {ms:03}  us {us:03}  ns {ns:03}"))
                    .size(18.0)
                    .color(Color32::from_rgb(114, 220, 205)),
            );
            ui.separator();
            ui.label(
                RichText::new(self.latest_now_local.format("%A, %B %d %Y").to_string())
                    .size(18.0)
                    .color(Color32::from_rgb(169, 188, 209)),
            );
        });

        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("Timing: {}", self.selected_provider.label))
                    .color(Color32::from_rgb(102, 211, 171))
                    .strong(),
            );
            ui.label(
                RichText::new(if self.latest_sample.is_measured_picos {
                    "Measured picos"
                } else {
                    "Derived picos"
                })
                .color(if self.latest_sample.is_measured_picos {
                    Color32::from_rgb(108, 228, 138)
                } else {
                    Color32::from_rgb(255, 183, 95)
                })
                .strong(),
            );
            if warning_count > 0 {
                ui.label(
                    RichText::new(format!("Warning active: {warning_count}"))
                        .color(Color32::from_rgb(255, 183, 95))
                        .strong(),
                );
            }
            if ui
                .button(if self.use_24h {
                    "Switch to 12h"
                } else {
                    "Switch to 24h"
                })
                .clicked()
            {
                self.use_24h = !self.use_24h;
            }
            if ui.button("Acknowledge Triggered Bells").clicked() {
                let acknowledged = self.scheduler.acknowledge_triggered(self.latest_now_local);
                self.set_status(
                    format!("Acknowledged {} bell(s).", acknowledged),
                    Duration::from_secs(2),
                );
            }
        });

        if let Some(reason) = self.selected_provider.fallback_reason.as_deref() {
            ui.label(
                RichText::new(format!("Fallback: {reason}"))
                    .color(Color32::from_rgb(255, 183, 95))
                    .strong(),
            );
        }
        if let Some((msg, _)) = &self.status_message {
            ui.label(
                RichText::new(msg)
                    .color(Color32::from_rgb(111, 228, 134))
                    .strong(),
            );
        }
    }

    fn show_alarm_list(&mut self, ui: &mut Ui) {
        if self.scheduler.is_empty() {
            ui.label(
                RichText::new("No bell schedule configured.")
                    .color(Color32::from_rgb(255, 190, 106))
                    .strong(),
            );
            return;
        }

        self.selected_alarm_index = self
            .selected_alarm_index
            .min(self.scheduler.len().saturating_sub(1));
        let display_mode = if self.use_24h {
            TimeDisplayMode::Hour24
        } else {
            TimeDisplayMode::Hour12
        };

        let rows: Vec<AlarmRow> = self
            .scheduler
            .alarms()
            .iter()
            .map(|runtime| AlarmRow {
                id: runtime.alarm.id.clone(),
                status: runtime.status(),
                next_occurrence_text: format_next_occurrence_with_mode(
                    runtime.next_occurrence,
                    display_mode,
                ),
                late_trigger_ms: runtime.alarm.late_trigger_ms,
                ring_duration_ms: runtime.alarm.ring_duration_ms,
                kind_text: match runtime.alarm.schedule {
                    AlarmSchedule::OneTime { .. } => "ONE_TIME",
                    AlarmSchedule::Recurring { .. } => "RECURRING",
                },
            })
            .collect();

        ui.heading(
            RichText::new("Bell Schedule")
                .color(Color32::from_rgb(104, 221, 205))
                .strong(),
        );
        ui.add_space(4.0);

        let mut remove_index: Option<usize> = None;
        ScrollArea::vertical()
            .id_salt("alarms_scroll")
            .show(ui, |ui| {
                egui::Grid::new("alarms_grid")
                    .striped(true)
                    .num_columns(7)
                    .show(ui, |ui| {
                        ui.label(RichText::new("ID").strong());
                        ui.label(RichText::new("Status").strong());
                        ui.label(RichText::new("Type").strong());
                        ui.label(RichText::new("Next Occurrence").strong());
                        ui.label(RichText::new("Late (ms)").strong());
                        ui.label(RichText::new("Duration (ms)").strong());
                        ui.label(RichText::new("Remove").strong());
                        ui.end_row();

                        for (index, row) in rows.iter().enumerate() {
                            let selected = index == self.selected_alarm_index;
                            if ui.selectable_label(selected, row.id.clone()).clicked() {
                                self.selected_alarm_index = index;
                            }
                            let (status_text, status_color) = match row.status {
                                AlarmStatus::Disabled => {
                                    ("DISABLED", Color32::from_rgb(146, 160, 177))
                                }
                                AlarmStatus::Armed => ("ARMED", Color32::from_rgb(109, 206, 197)),
                                AlarmStatus::Next => ("NEXT", Color32::from_rgb(108, 228, 138)),
                                AlarmStatus::Warning => {
                                    ("WARNING", Color32::from_rgb(255, 183, 95))
                                }
                                AlarmStatus::Triggered => {
                                    ("TRIGGERED", Color32::from_rgb(255, 101, 101))
                                }
                            };
                            ui.colored_label(status_color, status_text);
                            ui.label(row.kind_text);
                            ui.label(row.next_occurrence_text.clone());
                            ui.label(
                                RichText::new(format!("{:06}", row.late_trigger_ms)).monospace(),
                            );
                            ui.label(
                                RichText::new(format!("{:06}", row.ring_duration_ms)).monospace(),
                            );
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new("Delete")
                                            .color(Color32::from_rgb(255, 124, 124))
                                            .strong(),
                                    )
                                    .fill(Color32::from_rgb(51, 20, 24)),
                                )
                                .clicked()
                            {
                                remove_index = Some(index);
                            }
                            ui.end_row();
                        }
                    });
            });

        if let Some(index) = remove_index
            && let Some(removed) = self.scheduler.remove_alarm_at(index)
        {
            if let Err(err) = self.persist_scheduler() {
                self.set_status(format!("Persist failed: {err}"), Duration::from_secs(4));
            } else {
                self.set_status(
                    format!("Removed bell '{}'.", removed.id),
                    Duration::from_secs(3),
                );
                if !self.scheduler.is_empty() {
                    self.selected_alarm_index =
                        self.selected_alarm_index.min(self.scheduler.len() - 1);
                } else {
                    self.selected_alarm_index = 0;
                }
            }
        }
    }

    fn connected_clients_snapshot(&self) -> (Vec<PublicClient>, u64) {
        let Some(shared) = &self.api_state else {
            return (vec![], 0);
        };
        let guard = match shared.lock() {
            Ok(guard) => guard,
            Err(_) => return (vec![], 0),
        };
        (
            guard.connected_clients(
                self.latest_now_local.timestamp_millis(),
                DEFAULT_CONNECTED_CLIENT_TTL_MS,
            ),
            guard.total_requests(),
        )
    }

    fn read_debug_ui_enabled(&self) -> bool {
        let Some(shared) = &self.api_state else {
            return false;
        };
        let guard = match shared.lock() {
            Ok(guard) => guard,
            Err(_) => return false,
        };
        guard.debug_ui_enabled()
    }

    fn set_debug_ui_enabled(&self, enabled: bool) -> Result<()> {
        let Some(shared) = &self.api_state else {
            return Ok(());
        };
        let mut guard = shared
            .lock()
            .map_err(|_| anyhow::anyhow!("failed to lock API state"))?;
        guard.set_debug_ui_enabled(enabled);
        Ok(())
    }

    fn set_client_debug_mode(
        &self,
        client_id: &str,
        instance_id: &str,
        enabled: bool,
    ) -> Result<bool> {
        let Some(shared) = &self.api_state else {
            return Ok(false);
        };
        let mut guard = shared
            .lock()
            .map_err(|_| anyhow::anyhow!("failed to lock API state"))?;
        Ok(guard.set_client_debug_mode(client_id, instance_id, enabled))
    }

    fn show_connected_clients_panel(&mut self, ui: &mut Ui) {
        let (clients, total_requests) = self.connected_clients_snapshot();

        ui.heading(
            RichText::new("Connected Clients")
                .color(Color32::from_rgb(104, 221, 205))
                .strong(),
        );
        ui.add_space(4.0);

        if clients.is_empty() {
            ui.label(
                RichText::new("No connected clients in the last 15s.")
                    .color(Color32::from_rgb(255, 190, 106))
                    .strong(),
            );
            return;
        }

        let ping_values = clients
            .iter()
            .filter_map(|client| client.last_rtt_ms)
            .collect::<Vec<_>>();
        let avg_ping = if ping_values.is_empty() {
            None
        } else {
            Some(ping_values.iter().sum::<f64>() / ping_values.len() as f64)
        };
        let offset_values = clients
            .iter()
            .filter_map(|client| client.last_offset_ms)
            .collect::<Vec<_>>();
        let avg_offset = if offset_values.is_empty() {
            None
        } else {
            Some(offset_values.iter().sum::<f64>() / offset_values.len() as f64)
        };
        let desync_values = clients
            .iter()
            .filter_map(|client| client.last_desync_ms)
            .collect::<Vec<_>>();
        let avg_desync = if desync_values.is_empty() {
            None
        } else {
            Some(desync_values.iter().sum::<f64>() / desync_values.len() as f64)
        };
        let now_ms = self.latest_now_local.timestamp_millis();

        ui.label(
            RichText::new(format!(
                "Connected: {:03}   Total Requests: {:08}   Avg Ping: {} ms   Avg Offset: {} ms   Avg Desync: {} ms",
                clients.len(),
                total_requests,
                avg_ping
                    .map(|v| format!("{v:07.1}"))
                    .unwrap_or_else(|| "   -.--".to_string()),
                avg_offset
                    .map(|v| format!("{v:+08.1}"))
                    .unwrap_or_else(|| "   -.--".to_string()),
                avg_desync
                    .map(|v| format!("{v:+08.1}"))
                    .unwrap_or_else(|| "   -.--".to_string())
            ))
            .monospace()
            .color(Color32::from_rgb(180, 190, 204)),
        );
        ui.add_space(4.0);

        ScrollArea::vertical()
            .id_salt("clients_scroll")
            .show(ui, |ui| {
                egui::Grid::new("clients_grid")
                    .striped(true)
                    .num_columns(11)
                    .show(ui, |ui| {
                        ui.label(RichText::new("Client ID").strong());
                        ui.label(RichText::new("Instance").strong());
                        ui.label(RichText::new("Debug").strong());
                        ui.label(RichText::new("IP").strong());
                        ui.label(RichText::new("Ping (ms)").strong());
                        ui.label(RichText::new("Offset (ms)").strong());
                        ui.label(RichText::new("Desync (ms)").strong());
                        ui.label(RichText::new("Req/s").strong());
                        ui.label(RichText::new("Conn Time").strong());
                        ui.label(RichText::new("Last Seen").strong());
                        ui.label(RichText::new("Requests").strong());
                        ui.end_row();

                        let mut pending_debug_updates: Vec<(String, String, bool)> = Vec::new();
                        for client in clients {
                            let age_ms = now_ms.saturating_sub(client.last_seen_unix_ms);
                            let connection_ms = now_ms.saturating_sub(client.first_seen_unix_ms);
                            let last_seen_text = format!("{:06.1} s ago", age_ms as f64 / 1000.0);
                            let connection_text = format_duration_hms(connection_ms);
                            let connection_secs = (connection_ms as f64 / 1000.0).max(0.1);
                            let req_per_sec = client.request_count as f64 / connection_secs;
                            let req_per_sec_text = format!("{req_per_sec:06.1}");
                            let ping_text = client
                                .last_rtt_ms
                                .map(|value| format!("{value:07.1}"))
                                .unwrap_or_else(|| "   -.--".to_string());
                            let offset_text = client
                                .last_offset_ms
                                .map(|value| format!("{value:+08.1}"))
                                .unwrap_or_else(|| "   -.--".to_string());
                            let desync_text = client
                                .last_desync_ms
                                .map(|value| format!("{value:+08.1}"))
                                .unwrap_or_else(|| "   -.--".to_string());
                            let ping_color = if let Some(value) = client.last_rtt_ms {
                                if value <= 25.0 {
                                    Color32::from_rgb(108, 228, 138)
                                } else if value <= 80.0 {
                                    Color32::from_rgb(255, 214, 117)
                                } else {
                                    Color32::from_rgb(255, 124, 124)
                                }
                            } else {
                                Color32::from_rgb(180, 190, 204)
                            };
                            let offset_color = if let Some(value) = client.last_offset_ms {
                                if value.abs() <= 20.0 {
                                    Color32::from_rgb(108, 228, 138)
                                } else if value.abs() <= 80.0 {
                                    Color32::from_rgb(255, 214, 117)
                                } else {
                                    Color32::from_rgb(255, 124, 124)
                                }
                            } else {
                                Color32::from_rgb(180, 190, 204)
                            };
                            let desync_color = if let Some(value) = client.last_desync_ms {
                                if value.abs() <= 12.0 {
                                    Color32::from_rgb(108, 228, 138)
                                } else if value.abs() <= 50.0 {
                                    Color32::from_rgb(255, 214, 117)
                                } else {
                                    Color32::from_rgb(255, 124, 124)
                                }
                            } else {
                                Color32::from_rgb(180, 190, 204)
                            };

                            let client_id = client.id.clone();
                            let client_instance = client.instance_id.clone();
                            ui.label(client_id.as_str());
                            let instance_text = if client_instance.is_empty() {
                                "default".to_string()
                            } else {
                                client_instance.clone()
                            };
                            ui.label(RichText::new(instance_text).monospace());
                            let mut debug_mode = client.debug_mode;
                            if ui.checkbox(&mut debug_mode, "").changed() {
                                pending_debug_updates.push((client_id, client_instance, debug_mode));
                            }
                            ui.label(client.ip);
                            ui.colored_label(ping_color, RichText::new(ping_text).monospace());
                            ui.colored_label(offset_color, RichText::new(offset_text).monospace());
                            ui.colored_label(desync_color, RichText::new(desync_text).monospace());
                            ui.label(RichText::new(req_per_sec_text).monospace());
                            ui.label(RichText::new(connection_text).monospace());
                            ui.label(RichText::new(last_seen_text).monospace());
                            ui.label(
                                RichText::new(format!("{:08}", client.request_count)).monospace(),
                            );
                            ui.end_row();
                        }

                        for (client_id, instance_id, enabled) in pending_debug_updates {
                            match self.set_client_debug_mode(&client_id, &instance_id, enabled) {
                                Ok(true) => {}
                                Ok(false) => self.set_status(
                                    format!(
                                        "Could not update debug mode for {client_id}/{instance_id}"
                                    ),
                                    Duration::from_secs(3),
                                ),
                                Err(err) => self.set_status(
                                    format!(
                                        "Failed to set debug mode for {client_id}/{instance_id}: {err}"
                                    ),
                                    Duration::from_secs(4),
                                ),
                            }
                        }
                    });
            });
    }

    fn show_controls(&mut self, ui: &mut Ui) {
        ui.heading(
            RichText::new("Controls")
                .color(Color32::from_rgb(104, 221, 205))
                .strong(),
        );
        ui.separator();

        let sim_width = fps_padding_width(self.sim_target_fps);
        let render_width = fps_padding_width(self.render_target_fps);
        let sim_instant = format_fps_padded(
            self.sim_instant_fps,
            sim_width,
            if self.sim_cap_enabled {
                Some(self.sim_target_fps)
            } else {
                None
            },
        );
        let sim_rolling = format_fps_padded(
            self.sim_rolling_fps,
            sim_width,
            if self.sim_cap_enabled {
                Some(self.sim_target_fps)
            } else {
                None
            },
        );
        let render_instant = format_fps_padded(
            self.render_stats.instant_fps(),
            render_width,
            if self.render_cap_enabled {
                Some(self.render_target_fps)
            } else {
                None
            },
        );
        let render_rolling = format_fps_padded(
            self.render_stats.rolling_fps(),
            render_width,
            if self.render_cap_enabled {
                Some(self.render_target_fps)
            } else {
                None
            },
        );
        ui.label(
            RichText::new(format!("SIM FPS   {sim_instant}/{sim_rolling}"))
                .color(Color32::from_rgb(255, 214, 117))
                .strong(),
        );
        ui.label(
            RichText::new(format!("RENDER FPS {render_instant}/{render_rolling}"))
                .color(Color32::from_rgb(114, 220, 205))
                .strong(),
        );
        ui.label(
            RichText::new(format!("SIM drops {}", self.sim_stats.dropped_frames()))
                .color(Color32::from_rgb(255, 187, 99)),
        );
        ui.add_space(8.0);

        let mut targets_changed = false;
        let mut cap_changed = false;
        cap_changed |= ui
            .checkbox(&mut self.sim_cap_enabled, "Enable sim FPS cap")
            .changed();
        cap_changed |= ui
            .checkbox(&mut self.render_cap_enabled, "Enable render FPS cap")
            .changed();
        if cap_changed {
            self.reset_pacing_anchors();
            self.set_status(
                format!(
                    "Caps updated: sim={} render={}",
                    if self.sim_cap_enabled { "ON" } else { "OFF" },
                    if self.render_cap_enabled { "ON" } else { "OFF" }
                ),
                Duration::from_secs(2),
            );
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label("Sim target");
            targets_changed |= ui
                .add(egui::DragValue::new(&mut self.sim_target_fps).range(1..=20_000))
                .changed();
        });
        ui.horizontal(|ui| {
            ui.label("Render target");
            targets_changed |= ui
                .add(egui::DragValue::new(&mut self.render_target_fps).range(1..=1_000))
                .changed();
        });
        if targets_changed {
            self.update_targets();
            self.set_status(
                format!(
                    "Updated targets: sim {} FPS, render {} FPS",
                    self.sim_target_fps, self.render_target_fps
                ),
                Duration::from_secs(3),
            );
        }

        ui.separator();
        ui.label(RichText::new("Warning Settings").strong());
        let mut settings_changed = false;
        settings_changed |= ui
            .checkbox(
                &mut self.settings.warning_enabled,
                "Enable warning pre-trigger pulse",
            )
            .changed();
        ui.horizontal(|ui| {
            ui.label("Lead time (ms)");
            settings_changed |= ui
                .add(
                    egui::DragValue::new(&mut self.settings.warning_lead_time_ms)
                        .range(0..=3_600_000),
                )
                .changed();
        });
        ui.horizontal(|ui| {
            ui.label("Pulse time (ms)");
            settings_changed |= ui
                .add(
                    egui::DragValue::new(&mut self.settings.warning_pulse_time_ms)
                        .range(0..=60_000),
                )
                .changed();
        });
        if settings_changed {
            if let Err(err) = self.persist_scheduler() {
                self.set_status(format!("Persist failed: {err}"), Duration::from_secs(4));
            } else {
                self.set_status("Warning settings saved.", Duration::from_secs(2));
            }
        }

        ui.separator();
        ui.label(RichText::new("Server Debug Web UI").strong());
        let mut debug_ui_enabled = self.read_debug_ui_enabled();
        if ui
            .checkbox(
                &mut debug_ui_enabled,
                "Enable /debug web UI (local network only)",
            )
            .changed()
        {
            if let Err(err) = self.set_debug_ui_enabled(debug_ui_enabled) {
                self.set_status(
                    format!("Debug UI toggle failed: {err}"),
                    Duration::from_secs(4),
                );
            } else {
                self.set_status(
                    if debug_ui_enabled {
                        "Debug web UI enabled."
                    } else {
                        "Debug web UI disabled."
                    },
                    Duration::from_secs(2),
                );
            }
        }
        if debug_ui_enabled {
            ui.label(
                RichText::new(format!(
                    "Debug URL: http://{}:{}/debug",
                    self.api_bind, self.api_port
                ))
                .color(Color32::from_rgb(120, 205, 192))
                .monospace(),
            );
        }

        ui.separator();
        ui.label(RichText::new("Add Test Timer (One-Time)").strong());
        ui.horizontal(|ui| {
            ui.label("Duration");
            ui.add(TextEdit::singleline(&mut self.timer_duration_input).desired_width(90.0));
        });
        ui.horizontal(|ui| {
            ui.label("ID");
            ui.add(TextEdit::singleline(&mut self.timer_id_input).desired_width(130.0));
        });
        ui.horizontal(|ui| {
            ui.label("Bell duration (ms)");
            ui.add(
                egui::DragValue::new(&mut self.timer_ring_duration_ms)
                    .range(100..=300_000)
                    .speed(100),
            );
        });
        if ui
            .add(
                egui::Button::new(RichText::new("Create Test Timer").strong())
                    .fill(Color32::from_rgb(22, 78, 89))
                    .min_size(egui::vec2(190.0, 26.0)),
            )
            .clicked()
        {
            match self.create_test_timer_from_form() {
                Ok(msg) => self.set_status(msg, Duration::from_secs(3)),
                Err(err) => self.set_status(
                    format!("Add test timer failed: {err}"),
                    Duration::from_secs(4),
                ),
            }
        }

        ui.separator();
        ui.label(RichText::new("Add One-Time Bell").strong());
        ui.horizontal(|ui| {
            ui.label("Local datetime");
            ui.add(TextEdit::singleline(&mut self.once_datetime_input).desired_width(180.0));
        });
        ui.horizontal(|ui| {
            ui.label("ID");
            ui.add(TextEdit::singleline(&mut self.once_id_input).desired_width(130.0));
        });
        ui.horizontal(|ui| {
            ui.label("Late trigger (ms)");
            ui.add(
                egui::DragValue::new(&mut self.once_late_trigger_ms)
                    .range(0..=3_600_000)
                    .speed(50),
            );
        });
        ui.horizontal(|ui| {
            ui.label("Bell duration (ms)");
            ui.add(
                egui::DragValue::new(&mut self.once_ring_duration_ms)
                    .range(100..=300_000)
                    .speed(100),
            );
        });
        if ui
            .add(
                egui::Button::new(RichText::new("Create One-Time Bell").strong())
                    .fill(Color32::from_rgb(34, 64, 108))
                    .min_size(egui::vec2(190.0, 26.0)),
            )
            .clicked()
        {
            match self.create_one_time_alarm_from_form() {
                Ok(msg) => self.set_status(msg, Duration::from_secs(3)),
                Err(err) => self.set_status(
                    format!("Add one-time bell failed: {err}"),
                    Duration::from_secs(4),
                ),
            }
        }

        ui.separator();
        ui.label(RichText::new("Add Daily Bell").strong());
        ui.horizontal(|ui| {
            ui.label("Time");
            ui.add(TextEdit::singleline(&mut self.daily_time_input).desired_width(90.0));
        });
        ui.horizontal(|ui| {
            ui.label("ID");
            ui.add(TextEdit::singleline(&mut self.daily_id_input).desired_width(130.0));
        });
        ui.horizontal(|ui| {
            ui.label("Late trigger (ms)");
            ui.add(
                egui::DragValue::new(&mut self.daily_late_trigger_ms)
                    .range(0..=3_600_000)
                    .speed(50),
            );
        });
        ui.horizontal(|ui| {
            ui.label("Bell duration (ms)");
            ui.add(
                egui::DragValue::new(&mut self.daily_ring_duration_ms)
                    .range(100..=300_000)
                    .speed(100),
            );
        });
        ui.horizontal_wrapped(|ui| {
            ui.label("Days:");
            let labels = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
            for (index, label) in labels.iter().enumerate() {
                ui.checkbox(&mut self.daily_weekdays[index], *label);
            }
        });
        if ui
            .add(
                egui::Button::new(RichText::new("Create Daily Bell").strong())
                    .fill(Color32::from_rgb(28, 82, 67))
                    .min_size(egui::vec2(190.0, 26.0)),
            )
            .clicked()
        {
            match self.create_daily_alarm_from_form() {
                Ok(msg) => self.set_status(msg, Duration::from_secs(3)),
                Err(err) => self.set_status(
                    format!("Add daily bell failed: {err}"),
                    Duration::from_secs(4),
                ),
            }
        }
    }

    fn create_test_timer_from_form(&mut self) -> Result<String> {
        let delay = parse_duration_token(self.timer_duration_input.trim())?;
        let local_datetime = (self.latest_now_local + delay).naive_local();
        let id = if self.timer_id_input.trim().is_empty() {
            self.next_generated_id("timer")
        } else {
            self.timer_id_input.trim().to_string()
        };
        let ring_duration_ms = self.timer_ring_duration_ms.max(100);
        let alarm = Alarm {
            id: id.clone(),
            enabled: true,
            auto_acknowledge: false,
            late_trigger_ms: 0,
            ring_duration_ms,
            schedule: AlarmSchedule::OneTime { local_datetime },
        };
        self.add_alarm(alarm)?;
        Ok(format!(
            "Added test timer '{}' -> {} (duration {} ms)",
            id,
            local_datetime.format("%Y-%m-%d %H:%M:%S"),
            ring_duration_ms
        ))
    }

    fn create_one_time_alarm_from_form(&mut self) -> Result<String> {
        let local_datetime = parse_local_datetime_command(self.once_datetime_input.trim())?;
        let id = if self.once_id_input.trim().is_empty() {
            self.next_generated_id("bell")
        } else {
            self.once_id_input.trim().to_string()
        };
        let alarm = Alarm {
            id: id.clone(),
            enabled: true,
            auto_acknowledge: false,
            late_trigger_ms: self.once_late_trigger_ms,
            ring_duration_ms: self.once_ring_duration_ms.max(100),
            schedule: AlarmSchedule::OneTime { local_datetime },
        };
        self.add_alarm(alarm)?;
        Ok(format!(
            "Added one-time bell '{}' at {} (late {} ms, duration {} ms)",
            id,
            local_datetime.format("%Y-%m-%d %H:%M:%S"),
            self.once_late_trigger_ms,
            self.once_ring_duration_ms.max(100)
        ))
    }

    fn create_daily_alarm_from_form(&mut self) -> Result<String> {
        let time_local = parse_local_time_command(self.daily_time_input.trim())?;
        let mut days = Vec::with_capacity(7);
        let map = [
            Weekday::Mon,
            Weekday::Tue,
            Weekday::Wed,
            Weekday::Thu,
            Weekday::Fri,
            Weekday::Sat,
            Weekday::Sun,
        ];
        for (index, enabled) in self.daily_weekdays.iter().enumerate() {
            if *enabled {
                days.push(map[index]);
            }
        }
        if days.is_empty() {
            bail!("select at least one weekday");
        }

        let id = if self.daily_id_input.trim().is_empty() {
            self.next_generated_id("bell-daily")
        } else {
            self.daily_id_input.trim().to_string()
        };
        let alarm = Alarm {
            id: id.clone(),
            enabled: true,
            auto_acknowledge: false,
            late_trigger_ms: self.daily_late_trigger_ms,
            ring_duration_ms: self.daily_ring_duration_ms.max(100),
            schedule: AlarmSchedule::Recurring {
                time_local,
                days_of_week: days,
            },
        };
        self.add_alarm(alarm)?;
        Ok(format!(
            "Added daily bell '{}' at {} (late {} ms, duration {} ms)",
            id,
            time_local.format("%H:%M:%S"),
            self.daily_late_trigger_ms,
            self.daily_ring_duration_ms.max(100)
        ))
    }
}

impl eframe::App for BetterClockApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some((_, expires_at)) = &self.status_message
            && Instant::now() >= *expires_at
        {
            self.status_message = None;
        }

        let sim_steps = match self.simulate() {
            Ok(steps) => steps,
            Err(err) => {
                self.set_status(format!("Simulation error: {err}"), Duration::from_secs(4));
                0
            }
        };
        self.record_sim_rate(sim_steps, Instant::now());

        self.enforce_render_cap();

        let render_done = Instant::now();
        if let Some(previous_done) = self.last_render_done {
            self.render_stats
                .record_frame(render_done.saturating_duration_since(previous_done));
        }
        self.last_render_done = Some(render_done);

        TopBottomPanel::top("header")
            .resizable(false)
            .show(ctx, |ui| self.show_header(ui));

        TopBottomPanel::bottom("footer")
            .resizable(false)
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new(format!(
                            "Source: {} | Resolution hint: {} ps",
                            self.latest_sample.source,
                            self.selected_provider.provider.resolution_hint_ps()
                        ))
                        .color(Color32::from_rgb(161, 180, 201)),
                    );
                    ui.separator();
                    ui.label(
                        RichText::new(
                            "Mouse + keyboard enabled. Bell schedule persists to alarms.json on each change.",
                        )
                        .color(Color32::from_rgb(161, 180, 201)),
                    );
                    ui.separator();
                    ui.label(
                        RichText::new(format!(
                            "API http://{}:{}/v1 | state /v1/state | spec /openapi.yaml",
                            self.api_bind, self.api_port
                        ))
                            .color(Color32::from_rgb(120, 205, 192)),
                    );
                });
            });

        egui::SidePanel::right("controls_panel")
            .resizable(true)
            .min_width(340.0)
            .default_width(380.0)
            .show(ctx, |ui| self.show_controls(ui));

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.with_layout(Layout::top_down(Align::Min), |ui| {
                let total_height = ui.available_height();
                let alarm_height = (total_height * 0.62).max(180.0);
                ui.allocate_ui_with_layout(
                    egui::vec2(ui.available_width(), alarm_height),
                    Layout::top_down(Align::Min),
                    |ui| self.show_alarm_list(ui),
                );
                ui.separator();
                ui.allocate_ui_with_layout(
                    egui::vec2(ui.available_width(), ui.available_height()),
                    Layout::top_down(Align::Min),
                    |ui| self.show_connected_clients_panel(ui),
                );
            });
        });

        if !self.render_cap_enabled {
            ctx.request_repaint();
        } else {
            let wait = self
                .next_render_tick
                .saturating_duration_since(Instant::now());
            ctx.request_repaint_after(wait);
        }
    }
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

fn format_duration_hms(duration_ms: i64) -> String {
    let total_secs = duration_ms.max(0) / 1000;
    let hours = total_secs / 3600;
    let minutes = (total_secs % 3600) / 60;
    let seconds = total_secs % 60;
    format!("{hours:03}:{minutes:02}:{seconds:02}")
}

fn parse_duration_token(token: &str) -> Result<chrono::Duration> {
    if let Some(raw) = token.strip_suffix("ms") {
        let value: i64 = raw.parse()?;
        if value <= 0 {
            bail!("duration must be > 0");
        }
        return Ok(chrono::Duration::milliseconds(value));
    }
    if let Some(raw) = token.strip_suffix('s') {
        let value: i64 = raw.parse()?;
        if value <= 0 {
            bail!("duration must be > 0");
        }
        return Ok(chrono::Duration::seconds(value));
    }
    if let Some(raw) = token.strip_suffix('m') {
        let value: i64 = raw.parse()?;
        if value <= 0 {
            bail!("duration must be > 0");
        }
        return Ok(chrono::Duration::minutes(value));
    }
    if let Some(raw) = token.strip_suffix('h') {
        let value: i64 = raw.parse()?;
        if value <= 0 {
            bail!("duration must be > 0");
        }
        return Ok(chrono::Duration::hours(value));
    }

    let secs: i64 = token.parse()?;
    if secs <= 0 {
        bail!("duration must be > 0");
    }
    Ok(chrono::Duration::seconds(secs))
}

fn parse_local_datetime_command(input: &str) -> Result<NaiveDateTime> {
    NaiveDateTime::parse_from_str(input, "%Y-%m-%dT%H:%M:%S%.f")
        .or_else(|_| NaiveDateTime::parse_from_str(input, "%Y-%m-%d %H:%M:%S%.f"))
        .or_else(|_| NaiveDateTime::parse_from_str(input, "%Y-%m-%dT%H:%M:%S"))
        .or_else(|_| NaiveDateTime::parse_from_str(input, "%Y-%m-%d %H:%M:%S"))
        .map_err(|_| anyhow::anyhow!("invalid datetime '{input}'"))
}

fn parse_local_time_command(input: &str) -> Result<NaiveTime> {
    NaiveTime::parse_from_str(input, "%H:%M:%S%.f")
        .or_else(|_| NaiveTime::parse_from_str(input, "%H:%M:%S"))
        .or_else(|_| NaiveTime::parse_from_str(input, "%H:%M"))
        .map_err(|_| anyhow::anyhow!("invalid time '{input}'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_parser_supports_suffixes() {
        assert_eq!(
            parse_duration_token("10s").expect("10s"),
            chrono::Duration::seconds(10)
        );
        assert_eq!(
            parse_duration_token("2m").expect("2m"),
            chrono::Duration::minutes(2)
        );
        assert_eq!(
            parse_duration_token("500ms").expect("500ms"),
            chrono::Duration::milliseconds(500)
        );
    }

    #[test]
    fn parse_local_datetime_accepts_iso_and_space_variants() {
        assert!(parse_local_datetime_command("2026-02-07T07:30:00").is_ok());
        assert!(parse_local_datetime_command("2026-02-07 07:30:00").is_ok());
    }

    #[test]
    fn parse_local_time_accepts_hh_mm() {
        assert!(parse_local_time_command("09:30").is_ok());
        assert!(parse_local_time_command("09:30:15").is_ok());
    }
}
