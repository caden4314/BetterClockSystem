use std::collections::HashSet;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use chrono::{NaiveDateTime, NaiveTime, Weekday};
use serde::Deserialize;
use serde_json::{Map, Value, json};

#[derive(Debug, Clone)]
pub struct AlarmConfig {
    #[allow(dead_code)]
    pub version: u32,
    pub settings: AlarmSettings,
    pub alarms: Vec<Alarm>,
}

#[derive(Debug, Clone)]
pub struct AlarmSettings {
    pub warning_enabled: bool,
    pub warning_lead_time_ms: u64,
    pub warning_pulse_time_ms: u64,
}

impl Default for AlarmSettings {
    fn default() -> Self {
        Self {
            warning_enabled: false,
            warning_lead_time_ms: 0,
            warning_pulse_time_ms: 250,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Alarm {
    pub id: String,
    pub enabled: bool,
    pub auto_acknowledge: bool,
    pub late_trigger_ms: u64,
    pub ring_duration_ms: u64,
    pub schedule: AlarmSchedule,
}

#[derive(Debug, Clone)]
pub enum AlarmSchedule {
    OneTime {
        local_datetime: NaiveDateTime,
    },
    Recurring {
        time_local: NaiveTime,
        days_of_week: Vec<Weekday>,
    },
}

pub fn load_alarm_config(path: &Path) -> Result<AlarmConfig> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("unable to read alarm file {}", path.display()))?;
    parse_alarm_config_text(&content)
}

pub fn parse_alarm_config_text(content: &str) -> Result<AlarmConfig> {
    let raw = serde_json::from_str::<AlarmConfigFile>(content).map_err(|err| {
        let line = err.line();
        let column = err.column();
        anyhow::anyhow!("invalid JSON at line {line}, column {column}: {err}")
    })?;

    if raw.version != 1 {
        bail!(
            "unsupported alarm config version {}; expected version 1",
            raw.version
        );
    }

    let mut ids = HashSet::new();
    let mut alarms = Vec::with_capacity(raw.alarms.len());
    for alarm in raw.alarms {
        if !ids.insert(alarm.id.clone()) {
            bail!("duplicate alarm id found: {}", alarm.id);
        }
        if alarm.ring_duration_ms == 0 {
            bail!("alarm '{}' must have ring_duration_ms > 0", alarm.id);
        }

        let schedule = match alarm.schedule {
            AlarmScheduleFile::OneTime { local_datetime } => AlarmSchedule::OneTime {
                local_datetime: parse_local_datetime(&local_datetime)?,
            },
            AlarmScheduleFile::Recurring {
                time_local,
                days_of_week,
            } => {
                if days_of_week.is_empty() {
                    bail!(
                        "recurring alarm '{}' must include at least one day_of_week",
                        alarm.id
                    );
                }
                AlarmSchedule::Recurring {
                    time_local: parse_local_time(&time_local)?,
                    days_of_week: days_of_week
                        .into_iter()
                        .map(WeekdayToken::to_chrono)
                        .collect(),
                }
            }
        };

        alarms.push(Alarm {
            id: alarm.id,
            enabled: alarm.enabled,
            auto_acknowledge: alarm.auto_acknowledge,
            late_trigger_ms: alarm.late_trigger_ms,
            ring_duration_ms: alarm.ring_duration_ms,
            schedule,
        });
    }

    Ok(AlarmConfig {
        version: raw.version,
        settings: AlarmSettings {
            warning_enabled: raw.settings.warning_enabled,
            warning_lead_time_ms: raw.settings.warning_lead_time_ms,
            warning_pulse_time_ms: raw.settings.warning_pulse_time_ms,
        },
        alarms,
    })
}

pub fn save_alarm_config(path: &Path, alarms: &[Alarm], settings: &AlarmSettings) -> Result<()> {
    let mut serialized_alarms = Vec::with_capacity(alarms.len());
    for alarm in alarms {
        let mut alarm_obj = Map::new();
        alarm_obj.insert("id".to_string(), Value::String(alarm.id.clone()));
        alarm_obj.insert("enabled".to_string(), Value::Bool(alarm.enabled));
        alarm_obj.insert(
            "auto_acknowledge".to_string(),
            Value::Bool(alarm.auto_acknowledge),
        );
        alarm_obj.insert(
            "late_trigger_ms".to_string(),
            Value::Number(alarm.late_trigger_ms.into()),
        );
        alarm_obj.insert(
            "ring_duration_ms".to_string(),
            Value::Number(alarm.ring_duration_ms.into()),
        );

        match &alarm.schedule {
            AlarmSchedule::OneTime { local_datetime } => {
                alarm_obj.insert("kind".to_string(), Value::String("one_time".to_string()));
                alarm_obj.insert(
                    "local_datetime".to_string(),
                    Value::String(local_datetime.format("%Y-%m-%dT%H:%M:%S%.9f").to_string()),
                );
            }
            AlarmSchedule::Recurring {
                time_local,
                days_of_week,
            } => {
                alarm_obj.insert("kind".to_string(), Value::String("recurring".to_string()));
                alarm_obj.insert(
                    "time_local".to_string(),
                    Value::String(time_local.format("%H:%M:%S%.9f").to_string()),
                );
                let days = days_of_week
                    .iter()
                    .map(|day| Value::String(weekday_to_token(*day).to_string()))
                    .collect::<Vec<_>>();
                alarm_obj.insert("days_of_week".to_string(), Value::Array(days));
            }
        }

        serialized_alarms.push(Value::Object(alarm_obj));
    }

    let payload = json!({
        "version": 1,
        "settings": {
            "warning_enabled": settings.warning_enabled,
            "warning_lead_time_ms": settings.warning_lead_time_ms,
            "warning_pulse_time_ms": settings.warning_pulse_time_ms
        },
        "alarms": serialized_alarms,
    });
    let text = serde_json::to_string_pretty(&payload)?;
    fs::write(path, format!("{text}\n"))
        .with_context(|| format!("unable to write alarm file {}", path.display()))?;
    Ok(())
}

fn parse_local_datetime(input: &str) -> Result<NaiveDateTime> {
    NaiveDateTime::parse_from_str(input, "%Y-%m-%dT%H:%M:%S%.f")
        .or_else(|_| NaiveDateTime::parse_from_str(input, "%Y-%m-%dT%H:%M:%S"))
        .with_context(|| format!("invalid local_datetime '{input}', expected ISO local datetime"))
}

fn parse_local_time(input: &str) -> Result<NaiveTime> {
    NaiveTime::parse_from_str(input, "%H:%M:%S%.f")
        .or_else(|_| NaiveTime::parse_from_str(input, "%H:%M:%S"))
        .with_context(|| format!("invalid time_local '{input}', expected HH:MM:SS(.nnnnnnnnn)"))
}

#[derive(Debug, Deserialize)]
struct AlarmConfigFile {
    version: u32,
    #[serde(default)]
    settings: AlarmSettingsFile,
    alarms: Vec<AlarmFile>,
}

#[derive(Debug, Deserialize, Default)]
struct AlarmSettingsFile {
    #[serde(default)]
    warning_enabled: bool,
    #[serde(default)]
    warning_lead_time_ms: u64,
    #[serde(default = "default_warning_pulse_time_ms")]
    warning_pulse_time_ms: u64,
}

#[derive(Debug, Deserialize)]
struct AlarmFile {
    id: String,
    #[serde(default = "default_enabled")]
    enabled: bool,
    #[serde(default)]
    auto_acknowledge: bool,
    #[serde(default)]
    late_trigger_ms: u64,
    #[serde(default = "default_ring_duration_ms")]
    ring_duration_ms: u64,
    #[serde(flatten)]
    schedule: AlarmScheduleFile,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum AlarmScheduleFile {
    OneTime {
        local_datetime: String,
    },
    Recurring {
        time_local: String,
        days_of_week: Vec<WeekdayToken>,
    },
}

#[derive(Debug, Deserialize)]
enum WeekdayToken {
    Mon,
    Tue,
    Wed,
    Thu,
    Fri,
    Sat,
    Sun,
}

impl WeekdayToken {
    fn to_chrono(self) -> Weekday {
        match self {
            WeekdayToken::Mon => Weekday::Mon,
            WeekdayToken::Tue => Weekday::Tue,
            WeekdayToken::Wed => Weekday::Wed,
            WeekdayToken::Thu => Weekday::Thu,
            WeekdayToken::Fri => Weekday::Fri,
            WeekdayToken::Sat => Weekday::Sat,
            WeekdayToken::Sun => Weekday::Sun,
        }
    }
}

fn default_enabled() -> bool {
    true
}

fn default_warning_pulse_time_ms() -> u64 {
    250
}

fn default_ring_duration_ms() -> u64 {
    5_000
}

fn weekday_to_token(day: Weekday) -> &'static str {
    match day {
        Weekday::Mon => "Mon",
        Weekday::Tue => "Tue",
        Weekday::Wed => "Wed",
        Weekday::Thu => "Thu",
        Weekday::Fri => "Fri",
        Weekday::Sat => "Sat",
        Weekday::Sun => "Sun",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_alarm_config() {
        let json = r#"
{
  "version": 1,
  "settings": {
    "warning_enabled": true,
    "warning_lead_time_ms": 30000,
    "warning_pulse_time_ms": 400
  },
  "alarms": [
    {
      "id": "wake-1",
      "enabled": true,
      "late_trigger_ms": 1500,
      "ring_duration_ms": 7000,
      "kind": "one_time",
      "local_datetime": "2026-02-07T07:30:00.000000000"
    },
    {
      "id": "standup-weekdays",
      "enabled": true,
      "auto_acknowledge": true,
      "kind": "recurring",
      "time_local": "09:30:00.000000000",
      "days_of_week": ["Mon", "Tue", "Wed", "Thu", "Fri"]
    }
  ]
}
"#;

        let config = parse_alarm_config_text(json).expect("valid config");
        assert_eq!(config.version, 1);
        assert!(config.settings.warning_enabled);
        assert_eq!(config.settings.warning_lead_time_ms, 30_000);
        assert_eq!(config.settings.warning_pulse_time_ms, 400);
        assert_eq!(config.alarms.len(), 2);
        assert!(!config.alarms[0].auto_acknowledge);
        assert!(config.alarms[1].auto_acknowledge);
        assert_eq!(config.alarms[0].late_trigger_ms, 1_500);
        assert_eq!(config.alarms[0].ring_duration_ms, 7_000);
        assert_eq!(config.alarms[1].late_trigger_ms, 0);
        assert_eq!(config.alarms[1].ring_duration_ms, 5_000);
    }

    #[test]
    fn rejects_invalid_timestamp() {
        let json = r#"
{
  "version": 1,
  "alarms": [
    {
      "id": "bad",
      "enabled": true,
      "kind": "one_time",
      "local_datetime": "not-a-time"
    }
  ]
}
"#;
        let err = parse_alarm_config_text(json).expect_err("invalid timestamp should fail");
        assert!(err.to_string().contains("invalid local_datetime"));
    }

    #[test]
    fn rejects_duplicate_ids() {
        let json = r#"
{
  "version": 1,
  "alarms": [
    {
      "id": "dup",
      "enabled": true,
      "kind": "one_time",
      "local_datetime": "2026-02-07T07:30:00"
    },
    {
      "id": "dup",
      "enabled": true,
      "kind": "one_time",
      "local_datetime": "2026-02-07T08:30:00"
    }
  ]
}
"#;
        let err = parse_alarm_config_text(json).expect_err("duplicate ids should fail");
        assert!(err.to_string().contains("duplicate alarm id"));
    }

    #[test]
    fn rejects_invalid_weekday() {
        let json = r#"
{
  "version": 1,
  "alarms": [
    {
      "id": "bad-day",
      "enabled": true,
      "kind": "recurring",
      "time_local": "09:30:00",
      "days_of_week": ["Funday"]
    }
  ]
}
"#;
        let err = parse_alarm_config_text(json).expect_err("invalid weekday should fail");
        assert!(err.to_string().contains("invalid JSON"));
    }

    #[test]
    fn missing_warning_timing_fields_use_defaults() {
        let json = r#"
{
  "version": 1,
  "settings": {
    "warning_enabled": true
  },
  "alarms": [
    {
    "id": "wake-1",
      "enabled": true,
      "kind": "one_time",
      "local_datetime": "2099-02-07T07:30:00.000000000"
    }
  ]
}
"#;

        let config = parse_alarm_config_text(json).expect("valid config");
        assert!(config.settings.warning_enabled);
        assert_eq!(config.settings.warning_lead_time_ms, 0);
        assert_eq!(config.settings.warning_pulse_time_ms, 250);
        assert_eq!(config.alarms[0].late_trigger_ms, 0);
        assert_eq!(config.alarms[0].ring_duration_ms, 5_000);
    }
}
