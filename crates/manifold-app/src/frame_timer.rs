use std::time::{Duration, Instant};

/// Frame pacing and timing statistics.
#[allow(dead_code)]
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
}

const FPS_SAMPLE_INTERVAL: f64 = 2.0;

#[allow(dead_code)]
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
        }
    }

    /// Returns true if enough time has passed for the next frame.
    pub fn should_tick(&self) -> bool {
        self.last_tick_time.elapsed() >= self.target_frame_duration
    }

    /// Consume the tick, returning delta time in seconds.
    pub fn consume_tick(&mut self) -> f64 {
        let now = Instant::now();
        let dt = (now - self.last_tick_time).as_secs_f64();
        self.last_tick_time = now;
        self.last_dt = dt;
        self.fps_frame_count += 1;
        self.update_fps_counter(now);
        dt
    }

    /// Seconds since application start.
    pub fn realtime_since_start(&self) -> f64 {
        self.app_start_time.elapsed().as_secs_f64()
    }

    /// Last frame's delta time in seconds.
    pub fn last_dt(&self) -> f64 {
        self.last_dt
    }

    /// Current measured FPS (updated every 2 seconds).
    pub fn current_fps(&self) -> f64 {
        self.current_fps
    }

    /// Change target FPS at runtime.
    pub fn set_target_fps(&mut self, fps: f64) {
        self.target_fps = fps;
        self.target_frame_duration = Duration::from_secs_f64(1.0 / fps);
    }

    pub fn target_fps(&self) -> f64 {
        self.target_fps
    }

    fn update_fps_counter(&mut self, now: Instant) {
        let elapsed = (now - self.fps_sample_start).as_secs_f64();
        if elapsed >= FPS_SAMPLE_INTERVAL {
            self.current_fps = self.fps_frame_count as f64 / elapsed;
            log::info!("FPS: {:.1}", self.current_fps);
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
}
