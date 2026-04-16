use std::time::{Duration, Instant};

/// Frame pacing and timing statistics.
///
/// Timer-based pacing at `target_fps`. On macOS, uses `mach_wait_until`
/// for precise kernel-level frame deadlines with zero CPU overhead.
/// Presentation timing is handled independently by CAMetalLayer.
pub struct FrameTimer {
    target_fps: f64,
    target_frame_duration: Duration,
    last_tick_time: Instant,
    app_start_time: Instant,
    last_dt: f64,

    // FPS counter
    fps_sample_start: Instant,
    fps_frame_count: u64,
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

const FPS_SAMPLE_INTERVAL: f64 = 1.0;

impl FrameTimer {
    pub fn new(target_fps: f64) -> Self {
        let now = Instant::now();
        Self {
            target_fps,
            target_frame_duration: Duration::from_secs_f64(1.0 / target_fps),
            last_tick_time: now,
            app_start_time: now,
            last_dt: 0.0,
            fps_sample_start: now,
            fps_frame_count: 0,
            current_fps: 0.0,
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

    /// Block until the next frame deadline. Zero CPU overhead on macOS
    /// (kernel-level `mach_wait_until`). Falls back to sleep on other
    /// platforms.
    pub fn wait_for_deadline(&self) {
        let remaining = self.time_until_next_tick();
        if remaining.is_zero() {
            return;
        }

        #[cfg(target_os = "macos")]
        {
            let now_mach = unsafe { mach_absolute_time() };
            let wait_mach = self.mach_timebase.duration_to_mach_units(remaining);
            let deadline = now_mach + wait_mach;
            unsafe {
                mach_wait_until(deadline);
            }
        }

        #[cfg(not(target_os = "macos"))]
        {
            // Fallback: sleep most of the duration, spin-wait the rest
            // to compensate for OS sleep overshoot.
            if remaining > Duration::from_millis(4) {
                std::thread::sleep(remaining - Duration::from_millis(3));
            }
            while !self.should_tick() {
                std::thread::yield_now();
            }
        }
    }

    /// Consume the tick, returning delta time in seconds.
    pub fn consume_tick(&mut self) -> f64 {
        let now = Instant::now();
        let dt = (now - self.last_tick_time).as_secs_f64();
        self.last_tick_time = now;
        self.last_dt = dt;
        self.fps_frame_count += 1;
        // Detect missed ticks: if dt exceeds 2× target, we dropped frames
        let target_secs = self.target_frame_duration.as_secs_f64();
        self.missed_ticks = if target_secs > 0.0 {
            ((dt / target_secs).floor() as u64).saturating_sub(1)
        } else {
            0
        };
        self.update_fps_counter(now);
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

    /// Current measured FPS (updated every second).
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

    fn update_fps_counter(&mut self, now: Instant) {
        let elapsed = (now - self.fps_sample_start).as_secs_f64();
        if elapsed >= FPS_SAMPLE_INTERVAL {
            self.current_fps = self.fps_frame_count as f64 / elapsed;
            log::debug!("FPS: {:.1}", self.current_fps);
            self.fps_sample_start = now;
            self.fps_frame_count = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn should_tick_respects_target_fps() {
        let timer = FrameTimer::new(60.0);
        // Just created — should not tick yet (unless system is very slow)
        // Sleep for one frame duration
        thread::sleep(Duration::from_millis(17)); // ~60fps
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
        // At 30fps, frame duration should be ~33ms
        thread::sleep(Duration::from_millis(17));
        // Should NOT tick yet at 30fps after only 17ms
        assert!(!timer.should_tick());
        thread::sleep(Duration::from_millis(20));
        assert!(timer.should_tick());
    }

    #[test]
    fn wait_for_deadline_returns_at_deadline() {
        let mut timer = FrameTimer::new(60.0);
        timer.consume_tick(); // reset last_tick_time to now
        let start = Instant::now();
        timer.wait_for_deadline();
        let elapsed = start.elapsed();
        // Should have waited approximately one frame (~16.6ms).
        // Wide tolerance: test runner runs many tests in parallel,
        // causing scheduling contention that inflates wait times.
        // mach_wait_until precision is a kernel guarantee verified
        // by the real-time content thread, not by unit tests.
        assert!(
            elapsed >= Duration::from_millis(14),
            "Returned too early: {elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_millis(30),
            "Returned too late: {elapsed:?}"
        );
    }
}
