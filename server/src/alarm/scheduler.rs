use chrono::{
    DateTime, Datelike, Days, Local, LocalResult, NaiveDateTime, TimeZone, Timelike, Weekday,
};

use crate::alarm::model::{Alarm, AlarmSchedule, AlarmSettings};

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum AlarmStatus {
    Disabled,
    Armed,
    Next,
    Warning,
    Triggered,
}

#[derive(Debug, Clone)]
pub struct RuntimeAlarm {
    pub alarm: Alarm,
    pub next_occurrence: Option<DateTime<Local>>,
    pub warning_active: bool,
    pub triggered: bool,
    pub triggered_until: Option<DateTime<Local>>,
    last_warning_pulse_slot: Option<i64>,
}

impl RuntimeAlarm {
    pub fn status(&self) -> AlarmStatus {
        if !self.alarm.enabled {
            AlarmStatus::Disabled
        } else if self.triggered {
            AlarmStatus::Triggered
        } else if self.warning_active {
            AlarmStatus::Warning
        } else if self.next_occurrence.is_some() {
            AlarmStatus::Next
        } else {
            AlarmStatus::Armed
        }
    }
}

pub struct AlarmScheduler {
    alarms: Vec<RuntimeAlarm>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct TickOutcome {
    pub triggered: usize,
    pub auto_acknowledged: usize,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct WarningOutcome {
    pub active: usize,
    pub pulses: usize,
}

impl AlarmScheduler {
    pub fn new(alarms: Vec<Alarm>) -> Self {
        Self::new_with_now(alarms, Local::now())
    }

    pub fn new_with_now(alarms: Vec<Alarm>, now: DateTime<Local>) -> Self {
        let runtime_alarms = alarms
            .into_iter()
            .map(|alarm| {
                let next_occurrence = if alarm.enabled {
                    next_occurrence_local(&alarm, &now)
                } else {
                    None
                };
                RuntimeAlarm {
                    alarm,
                    next_occurrence,
                    warning_active: false,
                    triggered: false,
                    triggered_until: None,
                    last_warning_pulse_slot: None,
                }
            })
            .collect();
        Self {
            alarms: runtime_alarms,
        }
    }

    pub fn tick(&mut self, now: DateTime<Local>) -> TickOutcome {
        let mut outcome = TickOutcome::default();
        for runtime_alarm in &mut self.alarms {
            if !runtime_alarm.alarm.enabled {
                runtime_alarm.triggered = false;
                runtime_alarm.triggered_until = None;
                runtime_alarm.warning_active = false;
                runtime_alarm.last_warning_pulse_slot = None;
                continue;
            }

            if runtime_alarm.triggered {
                let still_ringing = runtime_alarm
                    .triggered_until
                    .map(|until| now < until)
                    .unwrap_or(false);
                if still_ringing {
                    continue;
                }

                runtime_alarm.triggered = false;
                runtime_alarm.triggered_until = None;
                runtime_alarm.warning_active = false;
                runtime_alarm.last_warning_pulse_slot = None;
                match runtime_alarm.alarm.schedule {
                    AlarmSchedule::OneTime { .. } => {
                        runtime_alarm.next_occurrence = None;
                    }
                    AlarmSchedule::Recurring { .. } => {
                        let next_probe = now + chrono::Duration::nanoseconds(1);
                        runtime_alarm.next_occurrence =
                            next_occurrence_local(&runtime_alarm.alarm, &next_probe);
                    }
                }
            }

            if runtime_alarm.next_occurrence.is_none() {
                runtime_alarm.next_occurrence = next_occurrence_local(&runtime_alarm.alarm, &now);
            }

            if let Some(trigger_at) = trigger_time_for_runtime_alarm(runtime_alarm)
                && now >= trigger_at
            {
                runtime_alarm.warning_active = false;
                runtime_alarm.last_warning_pulse_slot = None;
                runtime_alarm.triggered = true;
                runtime_alarm.triggered_until =
                    Some(trigger_at + ring_duration_for_alarm(&runtime_alarm.alarm));
                outcome.triggered += 1;
            }
        }
        outcome
    }

    pub fn refresh_warnings(
        &mut self,
        now: DateTime<Local>,
        settings: &AlarmSettings,
    ) -> WarningOutcome {
        let mut outcome = WarningOutcome::default();
        if !settings.warning_enabled {
            self.clear_all_warnings();
            return outcome;
        }

        let lead_ms_u64 = settings.warning_lead_time_ms;
        if lead_ms_u64 == 0 {
            self.clear_all_warnings();
            return outcome;
        }
        let lead_ms = i64::try_from(lead_ms_u64).unwrap_or(i64::MAX);
        let lead = chrono::Duration::milliseconds(lead_ms);

        let pulse_ms_u64 = settings.warning_pulse_time_ms.max(1);
        let pulse_ms = i64::try_from(pulse_ms_u64).unwrap_or(i64::MAX);
        let pulse_slot = now.timestamp_millis().div_euclid(pulse_ms);

        for runtime_alarm in &mut self.alarms {
            if !runtime_alarm.alarm.enabled || runtime_alarm.triggered {
                runtime_alarm.warning_active = false;
                runtime_alarm.last_warning_pulse_slot = None;
                continue;
            }

            if runtime_alarm.next_occurrence.is_none() {
                runtime_alarm.next_occurrence = next_occurrence_local(&runtime_alarm.alarm, &now);
            }

            let mut is_active = false;
            if let Some(trigger_at) = trigger_time_for_runtime_alarm(runtime_alarm)
                && trigger_at > now
            {
                let remaining = trigger_at - now;
                is_active = remaining <= lead;
            }

            runtime_alarm.warning_active = is_active;
            if is_active {
                outcome.active += 1;
                if runtime_alarm.last_warning_pulse_slot != Some(pulse_slot) {
                    runtime_alarm.last_warning_pulse_slot = Some(pulse_slot);
                    outcome.pulses += 1;
                }
            } else {
                runtime_alarm.last_warning_pulse_slot = None;
            }
        }

        outcome
    }

    pub fn acknowledge_triggered(&mut self, now: DateTime<Local>) -> usize {
        let mut acknowledged = 0;
        for runtime_alarm in &mut self.alarms {
            if !runtime_alarm.triggered {
                continue;
            }
            runtime_alarm.triggered = false;
            runtime_alarm.triggered_until = None;
            acknowledged += 1;

            match runtime_alarm.alarm.schedule {
                AlarmSchedule::OneTime { .. } => {
                    runtime_alarm.next_occurrence = None;
                }
                AlarmSchedule::Recurring { .. } => {
                    let next_probe = now + chrono::Duration::nanoseconds(1);
                    runtime_alarm.next_occurrence =
                        next_occurrence_local(&runtime_alarm.alarm, &next_probe);
                }
            }
        }
        acknowledged
    }

    pub fn alarms(&self) -> &[RuntimeAlarm] {
        &self.alarms
    }

    pub fn len(&self) -> usize {
        self.alarms.len()
    }

    pub fn is_empty(&self) -> bool {
        self.alarms.is_empty()
    }

    pub fn add_alarm(&mut self, alarm: Alarm, now: DateTime<Local>) {
        let next_occurrence = if alarm.enabled {
            next_occurrence_local(&alarm, &now)
        } else {
            None
        };
        self.alarms.push(RuntimeAlarm {
            alarm,
            next_occurrence,
            warning_active: false,
            triggered: false,
            triggered_until: None,
            last_warning_pulse_slot: None,
        });
    }

    pub fn export_alarms(&self) -> Vec<Alarm> {
        self.alarms
            .iter()
            .map(|runtime| runtime.alarm.clone())
            .collect()
    }

    pub fn remove_alarm_at(&mut self, index: usize) -> Option<Alarm> {
        if index >= self.alarms.len() {
            return None;
        }
        Some(self.alarms.remove(index).alarm)
    }

    fn clear_all_warnings(&mut self) {
        for runtime_alarm in &mut self.alarms {
            runtime_alarm.warning_active = false;
            runtime_alarm.last_warning_pulse_slot = None;
        }
    }
}

fn clamp_ms_to_duration(ms: u64) -> chrono::Duration {
    let ms_i64 = i64::try_from(ms).unwrap_or(i64::MAX);
    chrono::Duration::milliseconds(ms_i64)
}

fn late_trigger_for_alarm(alarm: &Alarm) -> chrono::Duration {
    clamp_ms_to_duration(alarm.late_trigger_ms)
}

fn ring_duration_for_alarm(alarm: &Alarm) -> chrono::Duration {
    clamp_ms_to_duration(alarm.ring_duration_ms.max(1))
}

fn trigger_time_for_runtime_alarm(runtime_alarm: &RuntimeAlarm) -> Option<DateTime<Local>> {
    let next = runtime_alarm.next_occurrence?;
    Some(next + late_trigger_for_alarm(&runtime_alarm.alarm))
}

fn resolve_local_datetime<Tz>(timezone: &Tz, naive: NaiveDateTime) -> Option<DateTime<Tz>>
where
    Tz: TimeZone,
    Tz::Offset: Copy,
{
    match timezone.from_local_datetime(&naive) {
        LocalResult::Single(dt) => Some(dt),
        LocalResult::Ambiguous(first, _second) => Some(first),
        LocalResult::None => None,
    }
}

fn next_occurrence_local(alarm: &Alarm, now: &DateTime<Local>) -> Option<DateTime<Local>> {
    next_occurrence_for_alarm_in_tz(alarm, now, &Local)
}

pub(crate) fn next_occurrence_for_alarm_in_tz<Tz>(
    alarm: &Alarm,
    now: &DateTime<Tz>,
    timezone: &Tz,
) -> Option<DateTime<Tz>>
where
    Tz: TimeZone,
    Tz::Offset: Copy,
{
    match &alarm.schedule {
        AlarmSchedule::OneTime { local_datetime } => {
            let candidate = resolve_local_datetime(timezone, *local_datetime)?;
            (candidate > *now).then_some(candidate)
        }
        AlarmSchedule::Recurring {
            time_local,
            days_of_week,
        } => next_recurring_occurrence(days_of_week, *time_local, now, timezone),
    }
}

fn next_recurring_occurrence<Tz>(
    days_of_week: &[Weekday],
    time_local: chrono::NaiveTime,
    now: &DateTime<Tz>,
    timezone: &Tz,
) -> Option<DateTime<Tz>>
where
    Tz: TimeZone,
    Tz::Offset: Copy,
{
    for day_offset in 0_u64..14 {
        let date = now.date_naive().checked_add_days(Days::new(day_offset))?;
        if !days_of_week.contains(&date.weekday()) {
            continue;
        }
        let naive = date.and_time(time_local);
        let candidate = match resolve_local_datetime(timezone, naive) {
            Some(value) => value,
            None => continue,
        };

        if candidate > *now {
            return Some(candidate);
        }
    }

    None
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum TimeDisplayMode {
    Hour24,
    Hour12,
}

pub fn format_next_occurrence_with_mode(
    next: Option<DateTime<Local>>,
    mode: TimeDisplayMode,
) -> String {
    match next {
        Some(dt) => match mode {
            TimeDisplayMode::Hour24 => format!(
                "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:09}",
                dt.year(),
                dt.month(),
                dt.day(),
                dt.hour(),
                dt.minute(),
                dt.second(),
                dt.nanosecond()
            ),
            TimeDisplayMode::Hour12 => {
                let (is_pm, hour12) = dt.hour12();
                let meridiem = if is_pm { "PM" } else { "AM" };
                format!(
                    "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:09} {}",
                    dt.year(),
                    dt.month(),
                    dt.day(),
                    hour12,
                    dt.minute(),
                    dt.second(),
                    dt.nanosecond(),
                    meridiem
                )
            }
        },
        None => "-".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use chrono::{Local, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Weekday};
    use chrono_tz::America::New_York;

    use super::*;
    use crate::alarm::model::{Alarm, AlarmSchedule, AlarmSettings};

    #[test]
    fn one_time_future_alarm_triggers_once() {
        let now = Local::now();
        let future = (now + chrono::Duration::seconds(2)).naive_local();
        let alarm = Alarm {
            id: "once".to_string(),
            enabled: true,
            auto_acknowledge: false,
            late_trigger_ms: 0,
            ring_duration_ms: 5_000,
            schedule: AlarmSchedule::OneTime {
                local_datetime: future,
            },
        };
        let mut scheduler = AlarmScheduler::new_with_now(vec![alarm], now);

        let after = now + chrono::Duration::seconds(3);
        let outcome = scheduler.tick(after);
        assert_eq!(outcome.triggered, 1);
        assert_eq!(outcome.auto_acknowledged, 0);

        let acknowledged = scheduler.acknowledge_triggered(after);
        assert_eq!(acknowledged, 1);

        let retriggered = scheduler.tick(after + chrono::Duration::seconds(10));
        assert_eq!(retriggered.triggered, 0);
    }

    #[test]
    fn missed_one_time_alarm_is_skipped() {
        let now = Local::now();
        let past = (now - chrono::Duration::seconds(2)).naive_local();
        let alarm = Alarm {
            id: "past".to_string(),
            enabled: true,
            auto_acknowledge: false,
            late_trigger_ms: 0,
            ring_duration_ms: 5_000,
            schedule: AlarmSchedule::OneTime {
                local_datetime: past,
            },
        };
        let mut scheduler = AlarmScheduler::new_with_now(vec![alarm], now);
        let triggered = scheduler.tick(now + chrono::Duration::seconds(2));
        assert_eq!(triggered.triggered, 0);
    }

    #[test]
    fn recurring_alarm_finds_next_weekday() {
        let now = Local::now();
        let alarm = Alarm {
            id: "recurring".to_string(),
            enabled: true,
            auto_acknowledge: false,
            late_trigger_ms: 0,
            ring_duration_ms: 5_000,
            schedule: AlarmSchedule::Recurring {
                time_local: NaiveTime::from_hms_nano_opt(23, 59, 59, 0).expect("valid time"),
                days_of_week: vec![now.weekday()],
            },
        };
        let scheduler = AlarmScheduler::new_with_now(vec![alarm], now);
        assert!(scheduler.alarms()[0].next_occurrence.is_some());
    }

    #[test]
    fn dst_spring_forward_nonexistent_time_is_skipped() {
        let alarm = Alarm {
            id: "spring".to_string(),
            enabled: true,
            auto_acknowledge: false,
            late_trigger_ms: 0,
            ring_duration_ms: 5_000,
            schedule: AlarmSchedule::Recurring {
                time_local: NaiveTime::from_hms_nano_opt(2, 30, 0, 0).expect("valid"),
                days_of_week: vec![Weekday::Sun],
            },
        };
        let now = New_York
            .with_ymd_and_hms(2026, 3, 8, 0, 30, 0)
            .single()
            .expect("valid");
        let next =
            next_occurrence_for_alarm_in_tz(&alarm, &now, &New_York).expect("next occurrence");
        assert_eq!(
            next.date_naive(),
            NaiveDate::from_ymd_opt(2026, 3, 15).expect("valid date")
        );
    }

    #[test]
    fn dst_fall_back_chooses_first_ambiguous_instance() {
        let alarm = Alarm {
            id: "fall".to_string(),
            enabled: true,
            auto_acknowledge: false,
            late_trigger_ms: 0,
            ring_duration_ms: 5_000,
            schedule: AlarmSchedule::OneTime {
                local_datetime: NaiveDateTime::new(
                    NaiveDate::from_ymd_opt(2026, 11, 1).expect("date"),
                    NaiveTime::from_hms_nano_opt(1, 30, 0, 0).expect("time"),
                ),
            },
        };
        let now = New_York
            .with_ymd_and_hms(2026, 11, 1, 0, 0, 0)
            .single()
            .expect("valid");

        let expected = match New_York.from_local_datetime(&NaiveDateTime::new(
            NaiveDate::from_ymd_opt(2026, 11, 1).expect("date"),
            NaiveTime::from_hms_nano_opt(1, 30, 0, 0).expect("time"),
        )) {
            LocalResult::Ambiguous(first, _second) => first,
            _ => panic!("expected ambiguous local time"),
        };

        let actual = next_occurrence_for_alarm_in_tz(&alarm, &now, &New_York).expect("next");
        assert_eq!(actual, expected);
    }

    #[test]
    fn recurring_bell_auto_clears_after_duration() {
        let now = Local::now();
        let soon = (now + chrono::Duration::seconds(1)).time();
        let alarm = Alarm {
            id: "auto".to_string(),
            enabled: true,
            auto_acknowledge: false,
            late_trigger_ms: 0,
            ring_duration_ms: 500,
            schedule: AlarmSchedule::Recurring {
                time_local: soon,
                days_of_week: vec![now.weekday()],
            },
        };
        let mut scheduler = AlarmScheduler::new_with_now(vec![alarm], now);
        let outcome = scheduler.tick(now + chrono::Duration::seconds(2));
        assert_eq!(outcome.triggered, 1);
        assert!(scheduler.alarms()[0].triggered);

        let after_ring = now + chrono::Duration::seconds(3);
        let next_outcome = scheduler.tick(after_ring);
        assert_eq!(next_outcome.triggered, 0);
        assert!(!scheduler.alarms()[0].triggered);
    }

    #[test]
    fn warning_activates_inside_lead_window() {
        let now = Local::now();
        let future = (now + chrono::Duration::seconds(10)).naive_local();
        let alarm = Alarm {
            id: "warn".to_string(),
            enabled: true,
            auto_acknowledge: false,
            late_trigger_ms: 0,
            ring_duration_ms: 5_000,
            schedule: AlarmSchedule::OneTime {
                local_datetime: future,
            },
        };
        let mut scheduler = AlarmScheduler::new_with_now(vec![alarm], now);
        let settings = AlarmSettings {
            warning_enabled: true,
            warning_lead_time_ms: 5_000,
            warning_pulse_time_ms: 250,
        };

        let before_window =
            scheduler.refresh_warnings(now + chrono::Duration::seconds(4), &settings);
        assert_eq!(before_window.active, 0);
        assert_eq!(before_window.pulses, 0);
        assert!(!scheduler.alarms()[0].warning_active);

        let inside_window =
            scheduler.refresh_warnings(now + chrono::Duration::seconds(6), &settings);
        assert_eq!(inside_window.active, 1);
        assert_eq!(inside_window.pulses, 1);
        assert_eq!(scheduler.alarms()[0].status(), AlarmStatus::Warning);
    }

    #[test]
    fn warning_pulse_counts_once_per_pulse_slot() {
        let now = Local
            .timestamp_opt(1_700_000_000, 0)
            .single()
            .expect("valid epoch");
        let future = (now + chrono::Duration::seconds(10)).naive_local();
        let alarm = Alarm {
            id: "pulse".to_string(),
            enabled: true,
            auto_acknowledge: false,
            late_trigger_ms: 0,
            ring_duration_ms: 5_000,
            schedule: AlarmSchedule::OneTime {
                local_datetime: future,
            },
        };
        let mut scheduler = AlarmScheduler::new_with_now(vec![alarm], now);
        let settings = AlarmSettings {
            warning_enabled: true,
            warning_lead_time_ms: 5_000,
            warning_pulse_time_ms: 200,
        };

        let first = now + chrono::Duration::seconds(6);
        let second_same_slot = first + chrono::Duration::milliseconds(100);
        let third_new_slot = first + chrono::Duration::milliseconds(200);

        let pulse1 = scheduler.refresh_warnings(first, &settings);
        assert_eq!(pulse1.active, 1);
        assert_eq!(pulse1.pulses, 1);

        let pulse2 = scheduler.refresh_warnings(second_same_slot, &settings);
        assert_eq!(pulse2.active, 1);
        assert_eq!(pulse2.pulses, 0);

        let pulse3 = scheduler.refresh_warnings(third_new_slot, &settings);
        assert_eq!(pulse3.active, 1);
        assert_eq!(pulse3.pulses, 1);
    }

    #[test]
    fn warning_window_uses_late_trigger_offset() {
        let now = Local::now();
        let future = (now + chrono::Duration::seconds(10)).naive_local();
        let alarm = Alarm {
            id: "late-offset".to_string(),
            enabled: true,
            auto_acknowledge: false,
            late_trigger_ms: 5_000,
            ring_duration_ms: 5_000,
            schedule: AlarmSchedule::OneTime {
                local_datetime: future,
            },
        };
        let mut scheduler = AlarmScheduler::new_with_now(vec![alarm], now);
        let settings = AlarmSettings {
            warning_enabled: true,
            warning_lead_time_ms: 6_000,
            warning_pulse_time_ms: 250,
        };

        let before = scheduler.refresh_warnings(now + chrono::Duration::seconds(8), &settings);
        assert_eq!(before.active, 0);

        let inside = scheduler.refresh_warnings(now + chrono::Duration::seconds(10), &settings);
        assert_eq!(inside.active, 1);
    }
}
