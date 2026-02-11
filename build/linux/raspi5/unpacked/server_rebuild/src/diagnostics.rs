use std::collections::VecDeque;
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::time_provider::SelectedTimeProvider;

pub struct FrameStats {
    total_frames: u64,
    dropped_frames: u64,
    last_frame: Duration,
    target_frame: Duration,
    window: VecDeque<Duration>,
    frame_time_histogram: [u64; 6],
}

impl FrameStats {
    pub fn new(window_size: usize, target_frame: Duration) -> Self {
        Self {
            total_frames: 0,
            dropped_frames: 0,
            last_frame: Duration::ZERO,
            target_frame,
            window: VecDeque::with_capacity(window_size),
            frame_time_histogram: [0; 6],
        }
    }

    pub fn record_frame(&mut self, frame_time: Duration) {
        self.total_frames += 1;
        self.last_frame = frame_time;
        if frame_time > self.target_frame {
            self.dropped_frames += 1;
        }

        if self.window.len() == self.window.capacity() {
            let _ = self.window.pop_front();
        }
        self.window.push_back(frame_time);
        self.update_histogram(frame_time);
    }

    pub fn instant_fps(&self) -> f64 {
        if self.last_frame.is_zero() {
            return 0.0;
        }
        1.0 / self.last_frame.as_secs_f64()
    }

    pub fn rolling_fps(&self) -> f64 {
        if self.window.is_empty() {
            return 0.0;
        }
        let total_secs: f64 = self.window.iter().map(Duration::as_secs_f64).sum();
        if total_secs == 0.0 {
            return 0.0;
        }
        self.window.len() as f64 / total_secs
    }

    pub fn dropped_frames(&self) -> u64 {
        self.dropped_frames
    }

    pub fn add_dropped(&mut self, count: u64) {
        self.dropped_frames = self.dropped_frames.saturating_add(count);
    }

    pub fn set_target_frame(&mut self, target_frame: Duration) {
        self.target_frame = target_frame;
    }

    pub fn total_frames(&self) -> u64 {
        self.total_frames
    }

    pub fn histogram(&self) -> [u64; 6] {
        self.frame_time_histogram
    }

    fn update_histogram(&mut self, frame_time: Duration) {
        let ms = frame_time.as_secs_f64() * 1_000.0;
        let bucket = if ms <= 2.0 {
            0
        } else if ms <= 4.17 {
            1
        } else if ms <= 8.33 {
            2
        } else if ms <= 16.67 {
            3
        } else if ms <= 33.33 {
            4
        } else {
            5
        };
        self.frame_time_histogram[bucket] += 1;
    }
}

pub fn run_diagnostics(selected: &SelectedTimeProvider, fps: u16) -> Result<()> {
    let target = Duration::from_secs_f64(1.0 / f64::from(fps));
    println!("BetterClock diagnostics");
    println!("Requested target FPS: {fps}");
    println!("Selected timing source: {}", selected.label);
    println!(
        "Hardware-backed timing: {}",
        selected.provider.is_hardware_backed()
    );
    println!(
        "Resolution hint (ps): {}",
        selected.provider.resolution_hint_ps()
    );
    if let Some(reason) = selected.fallback_reason.as_deref() {
        println!("Fallback reason: {reason}");
    }

    println!("Running 3 second pacing benchmark...");
    let mut stats = FrameStats::new(512, target);
    let bench_start = Instant::now();
    let bench_end = bench_start + Duration::from_secs(3);
    let mut next_frame = bench_start + target;
    while Instant::now() < bench_end {
        let frame_start = Instant::now();
        let _ = selected.provider.now()?;
        sleep_until(next_frame);
        let frame_duration = frame_start.elapsed();
        stats.record_frame(frame_duration);
        next_frame += target;
    }

    println!("Benchmark summary:");
    println!("  Frames: {}", stats.total_frames());
    println!("  Dropped: {}", stats.dropped_frames());
    println!("  Instant FPS: {:.1}", stats.instant_fps());
    println!("  Rolling FPS: {:.1}", stats.rolling_fps());
    println!("  Frame-time histogram buckets (<=2, <=4.17, <=8.33, <=16.67, <=33.33, >33.33 ms):");
    println!("  {:?}", stats.histogram());
    Ok(())
}

pub fn sleep_until(deadline: Instant) {
    let now = Instant::now();
    if now >= deadline {
        return;
    }

    let mut remaining = deadline.saturating_duration_since(now);
    if remaining > Duration::from_millis(1) {
        std::thread::sleep(remaining - Duration::from_micros(250));
    }

    loop {
        let current = Instant::now();
        if current >= deadline {
            break;
        }
        remaining = deadline.saturating_duration_since(current);
        if remaining > Duration::from_micros(50) {
            std::thread::yield_now();
        } else {
            std::hint::spin_loop();
        }
    }
}
