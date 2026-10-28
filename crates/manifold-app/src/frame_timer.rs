use std::time::{Duration, Instant};

use manifold_core::Seconds;
use manifold_playback::frame_grid::FrameGrid;

/// What one content tick consumed: the grid frames the transport steps, and
/// the measured wall interval for telemetry.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TickFrames {
    pub frames: u32,
    pub wall_dt: Seconds,
}

/// Frame pacing and timing statistics.
///
/// Timer-based pacing at `target_fps`. On macOS, uses `mach_wait_until`
/// for kernel-assisted frame deadlines followed by a short spin wait.
/// Presentation timing is handled independently by CAMetalLayer.
///
/// Two deadline rules. The UI thread's timer (`new`) free-runs: the next
/// deadline is one period after the last tick. The content thread's timer
/// (`new_grid`) waits on an anchored grid and counts the frames each tick
/// consumed (docs/FRAME_EXACT_TRANSPORT_DESIGN.md D2).
///
/// FPS is measured via exponentially weighted moving average (EWMA) on
/// frame time — updates every frame with ~0.3s response time, producing
/// a smooth readout that reacts quickly to frame drops without flickering
/// on single-frame variance.
pub struct FrameTimer {
    target_fps: f64,
    target_frame_duration: Duration,
    /// The content tick's deadline grid; `None` on the UI thread's timer.
    grid: Option<FrameGrid>,
    last_tick_time: Instant,
    app_start_time: Instant,
    last_dt: f64,
    // Wall-clock pacing is independent of the optional deterministic engine clock.
    #[cfg(any(feature = "profiling", test))]
    last_wall_interval: Option<f64>,
    #[cfg(any(feature = "profiling", test))]
    has_previous_tick: bool,

    /// EWMA-smoothed frame time in seconds. FPS derived as 1/smoothed_dt.
    smoothed_dt: f64,
    /// Current FPS derived from smoothed_dt. Updated every frame.
    current_fps: f64,

    /// Whole tick intervals skipped. Free-running: floor(wall interval /
    /// target) - 1. Grid: the frames the tick consumed - 1.
    /// This count does not establish deadline compliance or presentation.
    missed_ticks: u64,

    /// BUG-jbxt (rt-capture frame clock): when true, `consume_tick` returns
    /// exactly the target frame duration, `consume_frames_at` returns one
    /// frame, and `realtime_since_start`
    /// advances by the same fixed step — headless repro harnesses render
    /// slower than realtime, and wall-clock dt compresses beat-driven
    /// drivers (a 32-beat sawtooth into ~13 frames at debug res), making
    /// driver-based motion repros unusable. Wall-clock bookkeeping
    /// (FPS stats, missed ticks, deadlines) stays wall-honest.
    frame_clocked: bool,
    /// Fixed-step engine-time accumulator, read only when `frame_clocked`.
    frame_clock_seconds: f64,

    /// Mach timebase for converting nanoseconds ↔ mach absolute time units.
    /// Cached at construction — the timebase never changes at runtime.
    #[cfg(target_os = "macos")]
    mach_timebase: MachTimebase,

    /// Target FPS for the most recent thread-policy attempt. Cache failed
    /// attempts too, so invalid input cannot retry on every frame.
    last_policy_target_fps_bits: Option<u64>,
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

    /// Convert a duration to a native policy field without overflowing u32.
    fn duration_to_mach_units_u32(self, d: Duration) -> Option<u32> {
        if self.numer == 0 || self.denom == 0 {
            return None;
        }
        let units = d
            .as_nanos()
            .checked_mul(self.denom as u128)?
            .checked_div(self.numer as u128)?;
        u32::try_from(units).ok().filter(|units| *units > 0)
    }

    fn mach_units_to_millis(self, units: u32) -> f64 {
        units as f64 * self.numer as f64 / self.denom as f64 / 1_000_000.0
    }
}

/// Sleep for `after` against an absolute CLOCK_MONOTONIC deadline (the
/// clock `Instant` reads), so a signal that interrupts the sleep resumes
/// toward the same instant.
#[cfg(not(target_os = "macos"))]
fn sleep_until_monotonic(after: Duration) {
    let mut deadline = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `deadline` is a valid out-pointer.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut deadline) };
    let nanos = deadline.tv_nsec as u64 + u64::from(after.subsec_nanos());
    deadline.tv_sec += (after.as_secs() + nanos / 1_000_000_000) as libc::time_t;
    deadline.tv_nsec = (nanos % 1_000_000_000) as libc::c_long;
    // SAFETY: `deadline` is a valid absolute time, and TIMER_ABSTIME takes
    // no remainder pointer.
    while unsafe {
        libc::clock_nanosleep(
            libc::CLOCK_MONOTONIC,
            libc::TIMER_ABSTIME,
            &deadline,
            std::ptr::null_mut(),
        )
    } == libc::EINTR
    {}
}

/// Real-time scheduling for the calling thread off the Mac: SCHED_FIFO when
/// the user's rtprio limit allows it (an `rtprio` entry in limits.conf),
/// otherwise a raised nice level granted by rtkit. rtkit's real-time grant
/// is not used: it requires RLIMIT_RTTIME, which kills the process when one
/// tick burns that much CPU without sleeping, as a cold shader compile can.
#[cfg(not(target_os = "macos"))]
fn raise_thread_priority() {
    const FIFO_PRIORITY: i32 = 10;
    const RTKIT_NICE: i32 = -15;
    let param = libc::sched_param {
        sched_priority: FIFO_PRIORITY,
    };
    // SAFETY: sets the calling thread's policy with a valid parameter.
    let ret =
        unsafe { libc::pthread_setschedparam(libc::pthread_self(), libc::SCHED_FIFO, &param) };
    if ret == 0 {
        log::info!("[ContentThread] Real-time thread priority set (SCHED_FIFO {FIFO_PRIORITY})");
        return;
    }
    // SAFETY: gettid has no preconditions.
    let tid = unsafe { libc::gettid() } as u64;
    let rtkit = zbus::blocking::Connection::system().and_then(|bus| {
        bus.call_method(
            Some("org.freedesktop.RealtimeKit1"),
            "/org/freedesktop/RealtimeKit1",
            Some("org.freedesktop.RealtimeKit1"),
            "MakeThreadHighPriority",
            &(tid, RTKIT_NICE),
        )
    });
    match rtkit {
        Ok(_) => log::info!(
            "[ContentThread] SCHED_FIFO refused (err={ret}); nice {RTKIT_NICE} through rtkit"
        ),
        Err(err) => log::warn!(
            "[ContentThread] SCHED_FIFO refused (err={ret}) and rtkit failed ({err}); \
             the thread keeps normal priority"
        ),
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
            grid: None,
            last_tick_time: now,
            app_start_time: now,
            last_dt: 0.0,
            #[cfg(any(feature = "profiling", test))]
            last_wall_interval: None,
            #[cfg(any(feature = "profiling", test))]
            has_previous_tick: false,
            smoothed_dt: initial_dt,
            current_fps: target_fps,
            missed_ticks: 0,
            frame_clocked: false,
            frame_clock_seconds: 0.0,
            last_policy_target_fps_bits: None,
            #[cfg(target_os = "macos")]
            mach_timebase: MachTimebase::query(),
        }
    }

    /// The content thread's timer: deadlines on a grid anchored now, and
    /// each tick counts the grid frames it consumed (D2).
    pub fn new_grid(target_fps: f64) -> Self {
        let mut timer = Self::new(target_fps);
        timer.grid = Some(FrameGrid::new(target_fps as f32, timer.last_tick_time));
        timer
    }

    fn reanchor_grid(&mut self, at: Instant) {
        let fps = self.target_fps as f32;
        if let Some(grid) = &mut self.grid {
            grid.reanchor(fps, at);
        }
    }

    /// Test seam: anchor the grid at an injected instant, so a test's
    /// deadlines and the timer's agree.
    #[cfg(test)]
    pub(crate) fn reanchor_grid_at(&mut self, at: Instant) {
        self.last_tick_time = at;
        self.reanchor_grid(at);
    }

    /// Exclude stopped project preparation from the first playback delta:
    /// the grid re-anchors, so the load's wall time is not counted as frames.
    /// Keep the application and deterministic frame clocks intact.
    pub fn resume_after_load(&mut self) {
        self.last_tick_time = Instant::now();
        self.reanchor_grid(self.last_tick_time);
        self.last_dt = 0.0;
        self.smoothed_dt = self.target_frame_duration.as_secs_f64();
        self.current_fps = self.target_fps;
        self.missed_ticks = 0;
        #[cfg(any(feature = "profiling", test))]
        {
            self.last_wall_interval = None;
            self.has_previous_tick = false;
        }
    }

    /// Returns true if enough time has passed for the next frame.
    pub fn should_tick(&self) -> bool {
        self.should_tick_at(Instant::now())
    }

    fn should_tick_at(&self, now: Instant) -> bool {
        match &self.grid {
            Some(grid) => now >= grid.next_deadline(),
            None => {
                now.saturating_duration_since(self.last_tick_time) >= self.target_frame_duration
            }
        }
    }

    /// Time remaining until next frame deadline.
    /// Returns Duration::ZERO if already past the deadline.
    pub fn time_until_next_tick(&self) -> Duration {
        match &self.grid {
            Some(grid) => grid
                .next_deadline()
                .saturating_duration_since(Instant::now()),
            None => self
                .target_frame_duration
                .saturating_sub(self.last_tick_time.elapsed()),
        }
    }

    /// Block until the next frame deadline.
    ///
    /// On macOS: `mach_wait_until` for the bulk of the wait (zero CPU),
    /// then spin for the final ~2ms to hit the deadline precisely.
    /// `mach_wait_until` is a software timer with ~1ms wake resolution —
    /// the spin bridges the gap using the nanosecond-resolution clock.
    /// Total CPU: ~2ms/frame = 12% of one core at 60fps.
    ///
    /// Elsewhere: `clock_nanosleep` on an absolute monotonic deadline wakes
    /// within the kernel's timer slack, so only the final 0.5 ms spins.
    pub fn wait_for_deadline(&self) {
        let remaining = self.time_until_next_tick();
        if remaining.is_zero() {
            return;
        }

        #[cfg(target_os = "macos")]
        {
            // mach_wait_until is a software timer with ~1ms wake resolution.
            // Spin for the final 2ms using the nanosecond-resolution clock.
            // Standard pattern for real-time video on macOS — 12% of one core.
            const SPIN_MARGIN: Duration = Duration::from_millis(2);
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
            const SPIN_MARGIN: Duration = Duration::from_micros(500);
            if remaining > SPIN_MARGIN {
                sleep_until_monotonic(remaining - SPIN_MARGIN);
            }
            while !self.should_tick() {
                std::hint::spin_loop();
            }
        }
    }

    /// Apply the native real-time scheduling policy once per target FPS.
    /// A target change causes one new attempt; repeated frames reuse the
    /// result, including failed attempts.
    pub(crate) fn ensure_thread_policy(&mut self) {
        if self.take_policy_refresh() {
            #[cfg(target_os = "macos")]
            self.apply_thread_policy();
            #[cfg(not(target_os = "macos"))]
            raise_thread_priority();
        }
    }

    fn take_policy_refresh(&mut self) -> bool {
        let target = self.target_fps().to_bits();
        self.last_policy_target_fps_bits.replace(target) != Some(target)
    }

    #[cfg(target_os = "macos")]
    fn apply_thread_policy(&self) {
        #[repr(C)]
        struct ThreadTimeConstraintPolicy {
            period: u32,
            computation: u32,
            constraint: u32,
            preemptible: i32,
        }

        unsafe extern "C" {
            fn thread_policy_set(
                thread: u32,
                flavor: u32,
                policy_info: *const ThreadTimeConstraintPolicy,
                count: u32,
            ) -> i32;
            fn pthread_mach_thread_np(thread: libc::pthread_t) -> u32;
        }

        // THREAD_TIME_CONSTRAINT_POLICY = 2
        const THREAD_TIME_CONSTRAINT_POLICY: u32 = 2;
        // Count = struct size in natural_t (u32) units.
        const POLICY_COUNT: u32 =
            (std::mem::size_of::<ThreadTimeConstraintPolicy>() / std::mem::size_of::<u32>()) as u32;

        let Some(frame_duration) = self
            .target_fps
            .is_finite()
            .then_some(self.target_fps)
            .filter(|fps| *fps > 0.0)
            .and_then(|fps| Duration::try_from_secs_f64(1.0 / fps).ok())
        else {
            log::warn!(
                "[ContentThread] invalid target FPS for THREAD_TIME_CONSTRAINT ({:.4}); \
                 falling back to QOS_CLASS_USER_INTERACTIVE",
                self.target_fps,
            );
            Self::apply_qos_fallback();
            return;
        };

        let Some(period) = self
            .mach_timebase
            .duration_to_mach_units_u32(frame_duration)
        else {
            log::warn!(
                "[ContentThread] invalid THREAD_TIME_CONSTRAINT policy conversion \
                 (fps={:.4}); falling back to QOS_CLASS_USER_INTERACTIVE",
                self.target_fps,
            );
            Self::apply_qos_fallback();
            return;
        };

        // Computation budget: allow up to 75% of the frame for render work.
        // The remaining 25% is headroom for the scheduler.
        let Some(computation) = u64::from(period)
            .checked_mul(3)
            .and_then(|units| units.checked_div(4))
            .and_then(|units| u32::try_from(units).ok())
            .filter(|units| *units > 0)
        else {
            log::warn!(
                "[ContentThread] invalid THREAD_TIME_CONSTRAINT computation \
                 (fps={:.4}); falling back to QOS_CLASS_USER_INTERACTIVE",
                self.target_fps,
            );
            Self::apply_qos_fallback();
            return;
        };

        let policy = ThreadTimeConstraintPolicy {
            period,
            computation,
            constraint: period,
            preemptible: 1,
        };

        let mach_thread = unsafe { pthread_mach_thread_np(libc::pthread_self()) };
        let ret = unsafe {
            thread_policy_set(
                mach_thread,
                THREAD_TIME_CONSTRAINT_POLICY,
                &policy,
                POLICY_COUNT,
            )
        };

        if ret == 0 {
            log::info!(
                "[ContentThread] Real-time thread policy set \
                 (THREAD_TIME_CONSTRAINT: period={:.3}ms, \
                 computation={:.3}ms, timebase={}/{})",
                self.mach_timebase.mach_units_to_millis(period),
                self.mach_timebase.mach_units_to_millis(computation),
                self.mach_timebase.numer,
                self.mach_timebase.denom,
            );
        } else {
            log::warn!(
                "[ContentThread] THREAD_TIME_CONSTRAINT failed (err={}), \
                 falling back to QOS_CLASS_USER_INTERACTIVE",
                ret,
            );
            Self::apply_qos_fallback();
        }
    }

    #[cfg(target_os = "macos")]
    fn apply_qos_fallback() {
        unsafe extern "C" {
            fn pthread_set_qos_class_self_np(qos_class: u32, relative_priority: i32) -> i32;
        }
        let qos_ret = unsafe { pthread_set_qos_class_self_np(0x21, 0) };
        if qos_ret != 0 {
            log::warn!("[ContentThread] QoS fallback also failed (err={})", qos_ret);
        } else {
            log::info!("[ContentThread] QoS set to USER_INTERACTIVE (fallback)");
        }
    }

    /// Consume the tick, returning delta time in seconds.
    pub fn consume_tick(&mut self) -> f64 {
        self.consume_tick_at(Instant::now())
    }

    fn consume_tick_at(&mut self, now: Instant) -> f64 {
        let (wall_dt, dt) = self.record_tick(now);
        // Legacy coarse count, retained for diagnostics only. A late tick can
        // report zero here; actual wall interval/lateness determines pacing.
        // Presentation is not measured by this timer.
        let target_secs = self.target_frame_duration.as_secs_f64();
        self.missed_ticks = if target_secs > 0.0 {
            ((wall_dt / target_secs).floor() as u64).saturating_sub(1)
        } else {
            0
        };
        dt
    }

    /// The content tick at `now`: the grid frames passed since the last tick
    /// (at least 1; exactly 1 when `frame_clocked`), counted by comparing
    /// `now` with the grid's deadlines (D2). Wall telemetry as `consume_tick`.
    pub fn consume_frames_at(&mut self, now: Instant) -> TickFrames {
        let grid_frames = self.grid.as_mut().map_or(1, |grid| grid.consume_at(now));
        let (wall_dt, _) = self.record_tick(now);
        self.missed_ticks = u64::from(grid_frames.saturating_sub(1));
        TickFrames {
            frames: if self.frame_clocked { 1 } else { grid_frames },
            wall_dt: Seconds(wall_dt),
        }
    }

    /// The deadline of the frame the last content tick consumed, not when it
    /// woke; external clocks are read there (D15). `None` on the UI timer.
    pub fn tick_deadline(&self) -> Option<Instant> {
        self.grid.as_ref().map(FrameGrid::consumed_deadline)
    }

    /// Wall bookkeeping shared by both consume paths: returns the wall
    /// interval and the free-running dt (fixed when `frame_clocked`).
    fn record_tick(&mut self, now: Instant) -> (f64, f64) {
        let wall_dt = now
            .saturating_duration_since(self.last_tick_time)
            .as_secs_f64();
        #[cfg(any(feature = "profiling", test))]
        {
            self.last_wall_interval = self.has_previous_tick.then_some(wall_dt);
            self.has_previous_tick = true;
        }
        self.last_tick_time = now;
        let dt = if self.frame_clocked {
            self.target_frame_duration.as_secs_f64()
        } else {
            wall_dt
        };
        self.last_dt = dt;
        self.frame_clock_seconds += dt;
        self.update_fps(wall_dt);
        (wall_dt, dt)
    }

    /// Whole tick intervals skipped. Zero does not mean on time.
    #[cfg(feature = "profiling")]
    pub fn missed_ticks(&self) -> u64 {
        self.missed_ticks
    }

    /// Actual elapsed seconds between tick starts, including work, GPU waits,
    /// profiler overhead, autorelease draining and scheduling. The first tick
    /// after creation/load/target change has no comparable predecessor.
    #[cfg(any(feature = "profiling", test))]
    pub fn last_wall_interval(&self) -> Option<f64> {
        self.last_wall_interval
    }

    /// Seconds since application start.
    pub fn realtime_since_start(&self) -> f64 {
        if self.frame_clocked {
            self.frame_clock_seconds
        } else {
            self.app_start_time.elapsed().as_secs_f64()
        }
    }

    /// BUG-jbxt: pin engine time to the frame count (see `frame_clocked`).
    /// Only the perf-soak headless harnesses (rt-capture) call this.
    #[cfg(any(feature = "perf-soak", test))]
    pub fn set_frame_clocked(&mut self, on: bool) {
        self.frame_clocked = on;
    }

    /// Last frame's delta time in seconds.
    pub fn last_dt(&self) -> f64 {
        self.last_dt
    }

    /// Current measured FPS (EWMA, updated every frame).
    pub fn current_fps(&self) -> f64 {
        self.current_fps
    }

    /// Change target FPS at runtime. A grid timer re-anchors at now.
    pub fn set_target_fps(&mut self, fps: f64) {
        let changed = fps != self.target_fps;
        #[cfg(any(feature = "profiling", test))]
        if changed {
            self.has_previous_tick = false;
            self.last_wall_interval = None;
        }
        self.target_fps = fps;
        self.target_frame_duration = Duration::from_secs_f64(1.0 / fps);
        if changed {
            self.reanchor_grid(Instant::now());
        }
    }

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

    /// Test-only injection point for deterministic EWMA testing.
    /// Simulates a frame with exact delta time, bypassing wall-clock measurement.
    #[cfg(test)]
    fn inject_frame_time(&mut self, dt: f64) {
        self.last_dt = dt;
        self.frame_clock_seconds += dt;
        self.update_fps(dt);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[cfg(target_os = "macos")]
    #[test]
    fn mach_timebase_conversion_uses_numerator_and_denominator() {
        let duration = Duration::from_secs(1);
        assert_eq!(
            (MachTimebase {
                numer: 125,
                denom: 3,
            })
            .duration_to_mach_units_u32(duration),
            Some(24_000_000)
        );
        assert_eq!(
            (MachTimebase { numer: 1, denom: 1 }).duration_to_mach_units_u32(duration),
            Some(1_000_000_000)
        );
    }

    #[test]
    fn thread_policy_refresh_is_once_then_target_change() {
        let mut timer = FrameTimer::new(60.0);
        assert!(timer.take_policy_refresh());
        assert!(!timer.take_policy_refresh());

        timer.set_target_fps(30.0);
        assert!(timer.take_policy_refresh());
        assert!(!timer.take_policy_refresh());

        timer.set_target_fps(30.0);
        assert!(!timer.take_policy_refresh());
    }

    #[test]
    fn load_pause_does_not_advance_playback_or_reset_application_time() {
        let mut timer = FrameTimer::new(24.0);
        timer.last_tick_time = Instant::now() - Duration::from_secs(20);
        timer.app_start_time = Instant::now() - Duration::from_secs(30);
        timer.frame_clock_seconds = 7.0;
        timer.missed_ticks = 100;
        timer.resume_after_load();
        assert_eq!(timer.missed_ticks, 0);
        assert_eq!(timer.frame_clock_seconds, 7.0);
        assert!(timer.realtime_since_start() >= 30.0);
        assert!(
            timer.consume_tick() < 1.0,
            "load time leaked into playback delta"
        );
    }

    #[test]
    fn telemetry_retains_short_hitches_that_coarse_counter_misses() {
        let mut timer = FrameTimer::new(24.0);
        let start = timer.last_tick_time;
        timer.consume_tick_at(start + Duration::from_millis(42));
        assert_eq!(timer.last_wall_interval(), None);
        timer.consume_tick_at(start + Duration::from_millis(112));
        assert_eq!(timer.last_wall_interval(), Some(0.070));
        assert_eq!(timer.missed_ticks, 0);
        let lateness = timer.last_wall_interval().unwrap() - 1.0 / timer.target_fps();
        assert!(lateness > 0.028);
        timer.consume_tick_at(start + Duration::from_millis(710));
        assert_eq!(timer.last_wall_interval(), Some(0.598));
    }

    #[test]
    fn telemetry_uses_wall_time_even_with_deterministic_engine_clock() {
        let mut timer = FrameTimer::new(24.0);
        timer.set_frame_clocked(true);
        let start = timer.last_tick_time;
        timer.consume_tick_at(start);
        let engine_dt = timer.consume_tick_at(start + Duration::from_millis(70));
        assert!((engine_dt - 1.0 / 24.0).abs() < 1e-9);
        assert_eq!(timer.last_wall_interval(), Some(0.070));
        timer.resume_after_load();
        assert_eq!(timer.last_wall_interval(), None);
        timer.consume_tick();
        assert_eq!(timer.last_wall_interval(), None);
        timer.consume_tick();
        assert!(timer.last_wall_interval().is_some());
        timer.set_target_fps(30.0);
        assert_eq!(timer.last_wall_interval(), None);
        timer.consume_tick();
        assert_eq!(timer.last_wall_interval(), None);
    }

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
        // 30fps interval is 33.3ms: 17ms elapsed must not tick, 40ms must.
        let last = timer.last_tick_time;
        assert!(!timer.should_tick_at(last + Duration::from_millis(17)));
        assert!(timer.should_tick_at(last + Duration::from_millis(40)));
    }

    #[test]
    fn wait_for_deadline_returns_at_deadline() {
        let timer = FrameTimer::new(60.0);
        let deadline = timer.last_tick_time + timer.target_frame_duration;
        timer.wait_for_deadline();
        // BUG-wwxh: scheduling delays can make a correct wait return late.
        // Check the timer's actual deadline, not elapsed time from a later
        // sample, and leave wakeup latency to the opt-in performance check.
        assert!(Instant::now() >= deadline, "Returned before the deadline");
    }

    // Run explicitly on an idle machine with --features perf-soak and the
    // wait_for_deadline_wakeup_latency filter. This is a latency measurement,
    // not a correctness requirement under workspace-wide test contention.
    #[cfg(feature = "perf-soak")]
    #[test]
    fn wait_for_deadline_wakeup_latency() {
        let mut timer = FrameTimer::new(60.0);
        timer.consume_tick();
        let deadline = timer.last_tick_time + timer.target_frame_duration;
        timer.wait_for_deadline();
        let returned = Instant::now();
        assert!(returned >= deadline, "Returned before the deadline");
        let elapsed = returned - timer.last_tick_time;
        assert!(
            elapsed < Duration::from_millis(30),
            "Returned too late: {elapsed:?}"
        );
    }

    #[test]
    fn frame_clocked_returns_fixed_dt() {
        let mut timer = FrameTimer::new(60.0);
        timer.set_frame_clocked(true);
        thread::sleep(Duration::from_millis(50)); // slower than realtime
        let dt = timer.consume_tick();
        assert!((dt - 1.0 / 60.0).abs() < 1e-9, "dt={dt}");
        let dt2 = timer.consume_tick(); // no sleep — still fixed
        assert!((dt2 - 1.0 / 60.0).abs() < 1e-9, "dt2={dt2}");
        let rt = timer.realtime_since_start();
        assert!((rt - 2.0 / 60.0).abs() < 1e-9, "realtime={rt}");
    }

    fn after(at: Instant, frames: f64, fps: f64) -> Instant {
        at + Duration::from_secs_f64(frames / fps)
    }

    /// A frame-clocked grid timer always steps one frame, however late the
    /// tick; the missed count stays wall-honest.
    #[test]
    fn frame_clocked_counts_one() {
        let mut timer = FrameTimer::new_grid(60.0);
        let anchor = Instant::now();
        timer.reanchor_grid_at(anchor);
        timer.set_frame_clocked(true);
        assert_eq!(timer.consume_frames_at(after(anchor, 1.0, 60.0)).frames, 1);
        assert_eq!(timer.consume_frames_at(after(anchor, 5.3, 60.0)).frames, 1);
        assert_eq!(timer.missed_ticks, 3);
        timer.set_frame_clocked(false);
        assert_eq!(timer.consume_frames_at(after(anchor, 9.1, 60.0)).frames, 4);
    }

    /// The grid timer's deadline stays on the anchored grid: a late tick does
    /// not push the next deadline out, unlike the UI thread's free-running
    /// timer.
    #[test]
    fn grid_deadline_is_anchored_ui_deadline_free_runs() {
        let anchor = Instant::now() - Duration::from_millis(500);
        let mut grid = FrameTimer::new_grid(60.0);
        grid.reanchor_grid_at(anchor);
        let late = after(anchor, 1.4, 60.0);
        let ticked = grid.consume_frames_at(late);
        assert_eq!(ticked.frames, 1);
        assert_eq!(
            ticked.wall_dt,
            Seconds(late.duration_since(anchor).as_secs_f64())
        );
        assert_eq!(
            grid.grid.as_ref().unwrap().next_deadline(),
            grid.grid.as_ref().unwrap().deadline(2)
        );

        let mut ui = FrameTimer::new(60.0);
        ui.last_tick_time = anchor;
        ui.consume_tick_at(late);
        assert_eq!(
            ui.last_tick_time, late,
            "the UI timer's next deadline is one period after this tick"
        );
        assert!(ui.grid.is_none());
    }

    /// Load and a rate change re-anchor the grid at now, so neither the load's
    /// wall time nor the old rate's deadlines become frames.
    #[test]
    fn load_and_retarget_reanchor_the_grid() {
        let mut timer = FrameTimer::new_grid(60.0);
        timer.reanchor_grid_at(Instant::now() - Duration::from_secs(20));
        timer.resume_after_load();
        assert_eq!(
            timer.consume_frames_at(Instant::now()).frames,
            1,
            "load time leaked into frames"
        );

        timer.reanchor_grid_at(Instant::now() - Duration::from_secs(20));
        timer.set_target_fps(30.0);
        assert_eq!(
            timer.consume_frames_at(Instant::now()).frames,
            1,
            "old deadlines leaked into frames"
        );
        let grid = timer.grid.as_ref().unwrap();
        assert_eq!(
            grid.deadline(1) - grid.deadline(0),
            Duration::from_secs_f64(1.0 / 30.0)
        );
        timer.reanchor_grid_at(Instant::now() - Duration::from_secs(20));
        timer.set_target_fps(30.0);
        assert!(
            timer.consume_frames_at(Instant::now()).frames > 500,
            "an unchanged rate keeps the grid"
        );
    }

    #[test]
    fn ewma_fps_converges() {
        let mut timer = FrameTimer::new(60.0);
        // Simulate 30 frames at exactly 60fps (16.67ms per frame)
        // using deterministic injection — no wall-clock dependency.
        for _ in 0..30 {
            timer.inject_frame_time(1.0 / 60.0);
        }
        // EWMA should have converged near 60fps
        let fps = timer.current_fps();
        assert!(fps > 55.0, "FPS too low: {fps:.1}");
        assert!(fps < 65.0, "FPS too high: {fps:.1}");
    }

    #[test]
    fn ewma_responds_to_frame_drop() {
        let mut timer = FrameTimer::new(60.0);
        // Establish baseline at 60fps
        for _ in 0..20 {
            timer.inject_frame_time(1.0 / 60.0);
        }
        let baseline = timer.current_fps();
        // Simulate a frame drop (2× frame time)
        timer.inject_frame_time(2.0 / 60.0);
        let after_drop = timer.current_fps();
        // FPS should have decreased
        assert!(
            after_drop < baseline,
            "FPS should decrease after frame drop: baseline={baseline:.1}, after={after_drop:.1}"
        );
    }
}
