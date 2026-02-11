use std::time::{SystemTime, UNIX_EPOCH};

use std::sync::Mutex;
use std::sync::atomic::{AtomicI64, Ordering};
#[cfg(not(windows))]
use std::time::Instant;
#[cfg(all(windows, target_arch = "x86_64"))]
use std::{arch::x86_64::_rdtsc, hint::spin_loop};

use anyhow::{Context, Result, anyhow};

use crate::time_provider::{TimeProvider, TimeSample, validate_picosecond_sample};

const PICOSECONDS_PER_SECOND: i128 = 1_000_000_000_000;
const SYNC_INTERVAL_PS: i128 = 250_000_000_000; // 250 ms
const HARD_RESYNC_THRESHOLD_PS: i128 = 150_000_000_000; // 150 ms
const MAX_SLEW_PS_PER_SECOND: i128 = 4_000_000_000; // 4 ms/s

pub struct SoftwareTimeProvider {
    wall_anchor_ps: i128,
    correction_ps: AtomicI64,
    last_sync_elapsed_ps: AtomicI64,
    last_output_ps: Mutex<i128>,
    #[cfg(windows)]
    last_elapsed_ps: AtomicI64,
    #[cfg(windows)]
    qpc_anchor: i64,
    #[cfg(windows)]
    qpc_frequency: i64,
    #[cfg(all(windows, target_arch = "x86_64"))]
    tsc_anchor: u64,
    #[cfg(all(windows, target_arch = "x86_64"))]
    tsc_ticks_per_qpc: u64,
    #[cfg(not(windows))]
    monotonic_anchor: Instant,
}

impl SoftwareTimeProvider {
    pub fn new() -> Result<Self> {
        let wall_anchor_ps = system_time_to_ps(SystemTime::now())?;

        #[cfg(windows)]
        {
            let qpc_frequency = query_performance_frequency()?;
            let qpc_anchor = query_performance_counter()?;
            #[cfg(all(windows, target_arch = "x86_64"))]
            let (tsc_anchor, tsc_ticks_per_qpc) =
                calibrate_tsc_ticks_per_qpc(qpc_frequency)?.unwrap_or((0, 0));
            Ok(Self {
                wall_anchor_ps,
                correction_ps: AtomicI64::new(0),
                last_sync_elapsed_ps: AtomicI64::new(0),
                last_output_ps: Mutex::new(wall_anchor_ps),
                last_elapsed_ps: AtomicI64::new(0),
                qpc_anchor,
                qpc_frequency,
                #[cfg(all(windows, target_arch = "x86_64"))]
                tsc_anchor,
                #[cfg(all(windows, target_arch = "x86_64"))]
                tsc_ticks_per_qpc,
            })
        }

        #[cfg(not(windows))]
        {
            Ok(Self {
                wall_anchor_ps,
                correction_ps: AtomicI64::new(0),
                last_sync_elapsed_ps: AtomicI64::new(0),
                last_output_ps: Mutex::new(wall_anchor_ps),
                monotonic_anchor: Instant::now(),
            })
        }
    }
}

impl TimeProvider for SoftwareTimeProvider {
    fn now(&self) -> Result<TimeSample> {
        #[cfg(windows)]
        let elapsed_ps = {
            let counter_now = query_performance_counter()?;
            let delta_counts = (counter_now - self.qpc_anchor) as i128;
            if delta_counts.is_negative() {
                0
            } else {
                let coarse_ps =
                    (delta_counts * PICOSECONDS_PER_SECOND) / i128::from(self.qpc_frequency);

                #[cfg(all(windows, target_arch = "x86_64"))]
                let refined_ps = if self.tsc_ticks_per_qpc > 0 {
                    let tsc_now = read_tsc();
                    let delta_tsc = tsc_now.saturating_sub(self.tsc_anchor);
                    let remainder_tsc = delta_tsc % self.tsc_ticks_per_qpc;
                    let qpc_tick_ps =
                        (PICOSECONDS_PER_SECOND / i128::from(self.qpc_frequency)).max(1);
                    let fine_ps = (i128::from(remainder_tsc) * qpc_tick_ps)
                        / i128::from(self.tsc_ticks_per_qpc);
                    coarse_ps + fine_ps
                } else {
                    coarse_ps
                };

                #[cfg(not(all(windows, target_arch = "x86_64")))]
                let refined_ps = coarse_ps;

                clamp_monotonic_ps(&self.last_elapsed_ps, refined_ps)
            }
        };

        #[cfg(not(windows))]
        let elapsed_ps = {
            let elapsed = self.monotonic_anchor.elapsed();
            (elapsed.as_nanos() as i128) * 1_000
        };

        let correction_ps = i128::from(self.correction_ps.load(Ordering::Relaxed));
        let estimated_now_ps = self.wall_anchor_ps + elapsed_ps + correction_ps;
        let correction_delta_ps =
            self.maybe_update_sync_correction(elapsed_ps, estimated_now_ps)?;
        let mut now_ps = estimated_now_ps + correction_delta_ps;
        now_ps = self.clamp_output_monotonic(now_ps)?;
        let unix_seconds = now_ps.div_euclid(PICOSECONDS_PER_SECOND) as i64;
        let subsecond_ps = now_ps.rem_euclid(PICOSECONDS_PER_SECOND);
        let nanos = (subsecond_ps / 1_000) as u32;
        let picos = (subsecond_ps % 1_000) as u32;

        let sample = TimeSample {
            unix_seconds,
            nanos,
            picos,
            source: "SW_NANO_DERIVED",
            is_measured_picos: false,
        };
        validate_picosecond_sample(&sample)?;
        Ok(sample)
    }

    fn resolution_hint_ps(&self) -> u64 {
        #[cfg(windows)]
        {
            let hint = (PICOSECONDS_PER_SECOND / i128::from(self.qpc_frequency)).max(1);
            return hint as u64;
        }

        #[cfg(not(windows))]
        {
            1_000
        }
    }

    fn is_hardware_backed(&self) -> bool {
        false
    }
}

impl SoftwareTimeProvider {
    fn maybe_update_sync_correction(
        &self,
        elapsed_ps: i128,
        estimated_now_ps: i128,
    ) -> Result<i128> {
        let elapsed_i64 = elapsed_ps.clamp(0, i64::MAX as i128) as i64;
        let previous_sync_elapsed = self.last_sync_elapsed_ps.load(Ordering::Relaxed);
        let delta_since_sync_ps = i128::from(elapsed_i64.saturating_sub(previous_sync_elapsed));
        if delta_since_sync_ps < SYNC_INTERVAL_PS {
            return Ok(0);
        }

        match self.last_sync_elapsed_ps.compare_exchange(
            previous_sync_elapsed,
            elapsed_i64,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => {}
            Err(_) => return Ok(0),
        }

        let wall_now_ps = system_time_to_ps(SystemTime::now())?;
        let error_ps = wall_now_ps - estimated_now_ps;
        let adjustment_ps = if error_ps.abs() >= HARD_RESYNC_THRESHOLD_PS {
            error_ps
        } else {
            let max_step_ps =
                (MAX_SLEW_PS_PER_SECOND * delta_since_sync_ps) / PICOSECONDS_PER_SECOND;
            let damped_ps = (error_ps * 25) / 100;
            damped_ps.clamp(-max_step_ps, max_step_ps)
        };
        if adjustment_ps == 0 {
            return Ok(0);
        }

        let adjustment_i64 = adjustment_ps.clamp(i64::MIN as i128, i64::MAX as i128) as i64;
        let _ = self
            .correction_ps
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                let next = i128::from(current) + i128::from(adjustment_i64);
                Some(next.clamp(i64::MIN as i128, i64::MAX as i128) as i64)
            });
        Ok(i128::from(adjustment_i64))
    }

    fn clamp_output_monotonic(&self, proposed_ps: i128) -> Result<i128> {
        let mut guard = self
            .last_output_ps
            .lock()
            .map_err(|_| anyhow!("failed to lock software time monotonic state"))?;
        if proposed_ps < *guard {
            return Ok(*guard);
        }
        *guard = proposed_ps;
        Ok(proposed_ps)
    }
}

fn system_time_to_ps(system_time: SystemTime) -> Result<i128> {
    let duration = system_time
        .duration_since(UNIX_EPOCH)
        .context("system clock is before UNIX_EPOCH")?;
    Ok(i128::from(duration.as_secs()) * PICOSECONDS_PER_SECOND
        + i128::from(duration.subsec_nanos()) * 1_000)
}

#[cfg(windows)]
fn clamp_monotonic_ps(last_elapsed_ps: &AtomicI64, proposed_ps: i128) -> i128 {
    let mut target = proposed_ps.clamp(0, i64::MAX as i128) as i64;
    loop {
        let observed = last_elapsed_ps.load(Ordering::Relaxed);
        if target < observed {
            target = observed;
        }
        match last_elapsed_ps.compare_exchange(
            observed,
            target,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return i128::from(target),
            Err(current) => {
                if target < current {
                    target = current;
                }
            }
        }
    }
}

#[cfg(windows)]
fn query_performance_counter() -> Result<i64> {
    use windows_sys::Win32::System::Performance::QueryPerformanceCounter;

    let mut value = 0_i64;
    // SAFETY: `value` points to valid writable memory for the duration of the call.
    let ok = unsafe { QueryPerformanceCounter(&mut value) };
    if ok == 0 {
        return Err(anyhow!("QueryPerformanceCounter failed"));
    }
    Ok(value)
}

#[cfg(windows)]
fn query_performance_frequency() -> Result<i64> {
    use windows_sys::Win32::System::Performance::QueryPerformanceFrequency;

    let mut value = 0_i64;
    // SAFETY: `value` points to valid writable memory for the duration of the call.
    let ok = unsafe { QueryPerformanceFrequency(&mut value) };
    if ok == 0 || value <= 0 {
        return Err(anyhow!("QueryPerformanceFrequency failed"));
    }
    Ok(value)
}

#[cfg(all(windows, target_arch = "x86_64"))]
fn read_tsc() -> u64 {
    // SAFETY: `_rdtsc` has no memory safety requirements and is used only for local timing interpolation.
    unsafe { _rdtsc() }
}

#[cfg(all(windows, target_arch = "x86_64"))]
fn calibrate_tsc_ticks_per_qpc(qpc_frequency: i64) -> Result<Option<(u64, u64)>> {
    if qpc_frequency <= 0 {
        return Ok(None);
    }

    let tsc_anchor = read_tsc();
    let qpc_start = query_performance_counter()?;
    let target_delta = (qpc_frequency / 200).max(1);
    let mut qpc_now = qpc_start;
    while qpc_now - qpc_start < target_delta {
        spin_loop();
        qpc_now = query_performance_counter()?;
    }
    let tsc_now = read_tsc();
    let delta_qpc = (qpc_now - qpc_start) as u64;
    let delta_tsc = tsc_now.saturating_sub(tsc_anchor);
    if delta_qpc == 0 || delta_tsc == 0 {
        return Ok(None);
    }

    let ticks_per_qpc = (delta_tsc / delta_qpc).max(1);
    Ok(Some((tsc_anchor, ticks_per_qpc)))
}
