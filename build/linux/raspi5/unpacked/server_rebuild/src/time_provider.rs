use anyhow::{Result, anyhow, bail};
use chrono::{DateTime, Local, TimeZone};

use crate::time_hardware_stub::HardwareTimeProvider;
use crate::time_software::SoftwareTimeProvider;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TimingSourceKind {
    Auto,
    Software,
    Hardware,
}

#[derive(Clone, Debug)]
pub struct TimeSample {
    pub unix_seconds: i64,
    pub nanos: u32,
    pub picos: u32,
    pub source: &'static str,
    pub is_measured_picos: bool,
}

impl TimeSample {
    pub fn to_local_datetime(&self) -> Result<DateTime<Local>> {
        Local
            .timestamp_opt(self.unix_seconds, self.nanos)
            .single()
            .ok_or_else(|| anyhow!("failed to convert sample into local datetime"))
    }
}

pub trait TimeProvider: Send + Sync {
    fn now(&self) -> Result<TimeSample>;
    fn resolution_hint_ps(&self) -> u64;
    fn is_hardware_backed(&self) -> bool;
}

pub struct SelectedTimeProvider {
    pub provider: Box<dyn TimeProvider>,
    pub label: &'static str,
    pub fallback_reason: Option<String>,
}

pub fn select_provider(kind: TimingSourceKind) -> Result<SelectedTimeProvider> {
    match kind {
        TimingSourceKind::Software => Ok(SelectedTimeProvider {
            provider: Box::new(SoftwareTimeProvider::new()?),
            label: "SW_NANO_DERIVED",
            fallback_reason: None,
        }),
        TimingSourceKind::Hardware => {
            let hardware = HardwareTimeProvider::try_new()
                .map_err(|err| anyhow!("hardware timing source unavailable: {err}"))?;
            Ok(SelectedTimeProvider {
                provider: Box::new(hardware),
                label: "HW_PICO",
                fallback_reason: None,
            })
        }
        TimingSourceKind::Auto => match HardwareTimeProvider::try_new() {
            Ok(hardware) => Ok(SelectedTimeProvider {
                provider: Box::new(hardware),
                label: "HW_PICO",
                fallback_reason: None,
            }),
            Err(err) => Ok(SelectedTimeProvider {
                provider: Box::new(SoftwareTimeProvider::new()?),
                label: "SW_NANO_DERIVED",
                fallback_reason: Some(format!(
                    "Hardware provider not detected, using software timing: {err}"
                )),
            }),
        },
    }
}

pub fn validate_picosecond_sample(sample: &TimeSample) -> Result<()> {
    if sample.picos > 999 {
        bail!("picos field out of range: {}", sample.picos);
    }
    if sample.nanos > 999_999_999 {
        bail!("nanos field out of range: {}", sample.nanos);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::thread;
    use std::time::Duration;

    use super::*;
    use crate::time_software::SoftwareTimeProvider;

    #[test]
    fn software_provider_is_monotonic() {
        let provider = SoftwareTimeProvider::new().expect("provider should initialize");
        let first = provider.now().expect("first sample");
        thread::sleep(Duration::from_millis(2));
        let second = provider.now().expect("second sample");

        let first_tuple = (first.unix_seconds, first.nanos, first.picos);
        let second_tuple = (second.unix_seconds, second.nanos, second.picos);
        assert!(second_tuple >= first_tuple);
    }

    #[test]
    fn software_provider_does_not_claim_measured_picos() {
        let provider = SoftwareTimeProvider::new().expect("provider should initialize");
        let sample = provider.now().expect("sample");
        assert!(!sample.is_measured_picos);
        assert_eq!(sample.source, "SW_NANO_DERIVED");
        validate_picosecond_sample(&sample).expect("sample should stay in valid range");
    }
}
