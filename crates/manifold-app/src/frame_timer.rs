use std::time::{Duration, Instant};

/// Frame pacing and timing statistics.
///
/// Timer-based pacing at `target_fps`. On macOS, uses `mach_wait_until`
/// for precise kernel-level frame deadlines with zero CPU overhead.
/// Presentation timing is handled independently by CAMetalLayer.
///
/// FPS is measured via exponentially weighted moving average (EWMA) on
/// frame time — updates every frame with ~0.3s response time, producing
/// a smooth readout that reacts quickly to frame drops without flickering
/// on single-frame variance.
pub struct FrameTimer {
    target_fps: f64,
    target_frame_duration: Duration,
    last_tick_time: Instant,
    app_start_time: Instant,
    last_dt: f64,

    /// EWMA-smoothed frame time in seconds. FPS derived as 1/smoothed_dt.
    smoothed_dt: f64,
    /// Current FPS derived from smoothed_dt. Updated every frame.
    current_fps: f64,

    /// Number of ticks missed this frame (dt exceeded 2× target = frame drop).
    missed_ticks: u64,

    /// Mach timebase for converting nanoseconds ↔ mach absolute time units.
    /// Cached at construction — the timebase never changes at runtime.
    #[cfg(target_os = "macos")]
    mach_timebase: MachTimebase,
}

/// Cached Mach timebase info for nanosecond ↔ mach unit conversion.
#[cfg(target_os = "macos")]
#[derive(Clone, Copy)]
struct MachTimebase {
    numer: u32,
    denom: u32,
}

#[cfg(target_os = "macos")]
#[repr(C)]
struct MachTimebaseInfo {
    numer: u32,
    denom: u32,
}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn mach_timebase_info(info: *mut MachTimebaseInfo) -> i32;
    fn mach_absolute_time() -> u64;
    fn mach_wait_until(deadline: u64) -> i32;
}

#[cfg(target_os = "macos")]
impl MachTimebase {
    fn query() -> Self {
        let mut info = MachTimebaseInfo { numer: 0, denom: 0 };
        unsafe {
            mach_timebase_info(&mut info);
        }
        Self {
            numer: info.numer,
            denom: info.denom,
        }
    }

    /// Convert a Duration to mach absolute time units.
    fn duration_to_mach_units(self, d: Duration) -> u64 {
        let nanos = d.as_nanos() as u64;
        // mach_units = nanos * denom / numer
        // Use u128 to avoid overflow on large durations.
        ((nanos as u128 * self.denom as u128) / self.numer as u128) as u64
    }
}

/// EWMA smoothing time constant in seconds. Controls how quickly the
/// FPS readout responds to changes. 0.3s settles in ~5 frames at 60fps —
/// fast enough to show frame drops, slow enough to filter jitter.
const EWMA_TAU: f64 = 0.3;

impl FrameTimer {
    pub fn new(target_fps: f64) -> Self {
        let now = Instant::now();
        let initial_dt = 1.0 / target_fps;
        Self {
            target_fps,
            target_frame_duration: Duration::from_secs_f64(initial_dt),
            last_tick_time: now,
            app_start_time: now,
            last_dt: 0.0,
            smoothed_dt: initial_dt,
            current_fps: target_fps,
            missed_ticks: 0,
            #[cfg(target_os = "macos")]
            mach_timebase: MachTimebase::query(),
        }
    }

    /// Returns true if enough time has passed for the next frame.
    pub fn should_tick(&self) -> bool {
        self.last_tick_time.elapsed() >= self.target_frame_duration
    }

    /// Time remaining until next frame deadline.
    /// Returns Duration::ZERO if already past the deadline.
    pub fn time_until_next_tick(&self) -> Duration {
        self.target_frame_duration
            .saturating_sub(self.last_tick_time.elapsed())
    }

    /// Block until the next frame deadline.
    ///
    /// On macOS: `mach_wait_until` for the bulk of the wait (zero CPU),
    /// then spin for the final ~2ms to hit the deadline precisely.
    /// `mach_wait_until` is a software timer with ~1ms wake resolution —
    /// the spin bridges the gap using the nanosecond-resolution clock.
    /// Total CPU: ~2ms/frame = 12% of one core at 60fps.
    pub fn wait_for_deadline(&self) {
        let remaining = self.time_until_next_tick();
        if remaining.is_zero() {
            return;
        }

        #[cfg(target_os = "macos")]
        {
            const SPIN_MARGIN: Duration = Duration::from_micros(2000);
            if remaining > SPIN_MARGIN {
                let coarse = remaining - SPIN_MARGIN;
                let now_mach = unsafe { mach_absolute_time() };
                let wait_mach = self.mach_timebase.duration_to_mach_units(coarse);
                unsafe {
                    mach_wait_until(now_mach + wait_mach);
                }
            }
            // Sub-microsecond spin for the final edge.
            while !self.should_tick() {
                std::hint::spin_loop();
            }
        }

        #[cfg(not(target_os = "macos"))]
        {
            if remaining > Duration::from_millis(4) {
                std::thread::sleep(remaining - Duration::from_millis(3));
            }
            while !self.should_tick() {
                std::hint::spin_loop();
            }
        }
    }

    /// Consume the tick, returning delta time in seconds.
    pub fn consume_tick(&mut self) -> f64 {
        let now = Instant::now();
        let dt = (now - self.last_tick_time).as_secs_f64();
        self.last_tick_time = now;
        self.last_dt = dt;
        // Detect missed ticks: if dt exceeds 2× target, we dropped frames
        let target_secs = self.target_frame_duration.as_secs_f64();
        self.missed_ticks = if target_secs > 0.0 {
            ((dt / target_secs).floor() as u64).saturating_sub(1)
        } else {
            0
        };
        self.update_fps(dt);
        dt
    }

    /// Number of ticks missed this frame (0 = on time, 1+ = frame drops).
    #[cfg(feature = "profiling")]
    pub fn missed_ticks(&self) -> u64 {
        self.missed_ticks
    }

    /// Seconds since application start.
    pub fn realtime_since_start(&self) -> f64 {
        self.app_start_time.elapsed().as_secs_f64()
    }

    /// Last frame's delta time in seconds.
    pub fn last_dt(&self) -> f64 {
        self.last_dt
    }

    /// Current measured FPS (EWMA, updated every frame).
    pub fn current_fps(&self) -> f64 {
        self.current_fps
    }

    /// Change target FPS at runtime.
    pub fn set_target_fps(&mut self, fps: f64) {
        self.target_fps = fps;
        self.target_frame_duration = Duration::from_secs_f64(1.0 / fps);
    }

    #[allow(dead_code)]
    pub fn target_fps(&self) -> f64 {
        self.target_fps
    }

    /// Update EWMA-smoothed FPS from the latest frame's dt.
    ///
    /// Uses adaptive alpha: `alpha = 1 - exp(-dt / tau)`. This makes the
    /// smoothing time constant independent of frame rate — the readout
    /// settles in ~tau seconds whether running at 30fps or 120fps.
    fn update_fps(&mut self, dt: f64) {
        if dt <= 0.0 {
            return;
        }
        // Adaptive alpha from time constant. At 60fps (dt=16.6ms, tau=0.3s):
        // alpha ≈ 0.054 → ~5% weight on new sample, 95% on history.
        let alpha = 1.0 - (-dt / EWMA_TAU).exp();
        self.smoothed_dt = alpha * dt + (1.0 - alpha) * self.smoothed_dt;
        self.current_fps = 1.0 / self.smoothed_dt;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn should_tick_respects_target_fps() {
        let timer = FrameTimer::new(60.0);
        thread::sleep(Duration::from_millis(17));
        assert!(timer.should_tick());
    }

    #[test]
    fn consume_tick_returns_positive_dt() {
        let mut timer = FrameTimer::new(60.0);
        thread::sleep(Duration::from_millis(10));
        let dt = timer.consume_tick();
        assert!(dt > 0.0);
        assert!(dt < 1.0);
    }

    #[test]
    fn realtime_advances() {
        let timer = FrameTimer::new(60.0);
        thread::sleep(Duration::from_millis(10));
        assert!(timer.realtime_since_start() > 0.005);
    }

    #[test]
    fn set_target_fps_changes_interval() {
        let mut timer = FrameTimer::new(60.0);
        timer.set_target_fps(30.0);
        assert_eq!(timer.target_fps(), 30.0);
        thread::sleep(Duration::from_millis(17));
        assert!(!timer.should_tick());
        thread::sleep(Duration::from_millis(20));
        assert!(timer.should_tick());
    }

    #[test]
    fn wait_for_deadline_returns_at_deadline() {
        let mut timer = FrameTimer::new(60.0);
        timer.consume_tick();
        let start = Instant::now();
        timer.wait_for_deadline();
        let elapsed = start.elapsed();
        assert!(
            elapsed >= Duration::from_millis(14),
            "Returned too early: {elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_millis(30),
            "Returned too late: {elapsed:?}"
        );
    }

    #[test]
    fn ewma_fps_converges() {
        let mut timer = FrameTimer::new(60.0);
        // Simulate 30 frames at 60fps
        for _ in 0..30 {
            thread::sleep(Duration::from_millis(16));
            timer.consume_tick();
        }
        // EWMA should have converged near 60fps (within tolerance for
        // test runner scheduling jitter).
        let fps = timer.current_fps();
        assert!(fps > 40.0, "FPS too low: {fps:.1}");
        assert!(fps < 80.0, "FPS too high: {fps:.1}");
    }

    #[test]
    fn ewma_responds_to_frame_drop() {
        let mut timer = FrameTimer::new(60.0);
        // Establish baseline at 60fps
        for _ in 0..20 {
            thread::sleep(Duration::from_millis(16));
            timer.consume_tick();
        }
        let baseline = timer.current_fps();
        // Simulate a frame drop (2× frame time)
        thread::sleep(Duration::from_millis(33));
        timer.consume_tick();
        let after_drop = timer.current_fps();
        // FPS should have decreased
        assert!(
            after_drop < baseline,
            "FPS should decrease after frame drop: baseline={baseline:.1}, after={after_drop:.1}"
        );
    }
}
