use anyhow::{Result, bail};

use crate::time_provider::{TimeProvider, TimeSample};

pub struct HardwareTimeProvider;

impl HardwareTimeProvider {
    pub fn try_new() -> Result<Self> {
        bail!("no supported picosecond timing hardware detected")
    }
}

impl TimeProvider for HardwareTimeProvider {
    fn now(&self) -> Result<TimeSample> {
        bail!("hardware timing provider is unavailable")
    }

    fn resolution_hint_ps(&self) -> u64 {
        1
    }

    fn is_hardware_backed(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hardware_stub_reports_unavailable() {
        let result = HardwareTimeProvider::try_new();
        assert!(result.is_err());
    }
}
