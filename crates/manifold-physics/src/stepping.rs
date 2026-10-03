//! Exchange with another solver inside the existing rigid-body tick owner.
//!
//! The owner retains clocks, authored input history and once-only events. It
//! prepares rigid targets and queued continuous forces before requesting each
//! interval, applies the exchange, then advances Box3D for exactly that duration.
//!
//! The live timing helpers below port the finite-step rules from FLIP Fluids'
//! `fluidsimulation.cpp` (`_calculateNextTimeStep`, `nextUpdateTimeStep`, and
//! `_getMarkerParticleSpeedLimit`).  FLIP Fluids is by Ryan L. Guy and Dennis
//! Fassbaender, MIT licensed; see `THIRD_PARTY_NOTICES.md`.

use std::convert::Infallible;
use std::fmt::Display;

use crate::{PhysicsWorld, Seconds, TickStamp};

/// The reference engine's CFL denominator epsilon.
pub const LIVE_CFL_EPSILON: f64 = 1e-6;
/// The pinned engine defaults used by live FLIP scheduling.
pub const LIVE_DEFAULT_CFL: f64 = 5.0;
pub const LIVE_DEFAULT_MIN_STEPS: u32 = 1;
pub const LIVE_DEFAULT_MAX_STEPS: u32 = 6;
const NOMINAL_TICK_SECONDS: f64 = 1.0 / 60.0;

/// A half-open interval of simulation time. Events at `end` belong to the
/// following interval, which lets a caller preserve event order when an outer
/// step is stretched or split.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StepInterval {
    pub start: Seconds,
    pub end: Seconds,
}

impl StepInterval {
    #[inline]
    pub const fn new(start: Seconds, end: Seconds) -> Self {
        Self { start, end }
    }

    #[inline]
    pub fn duration(self) -> Seconds {
        Seconds(self.end.0 - self.start.0)
    }

    /// Visit integration portions and hits in timestamp order without
    /// allocating. `hits` must be sorted by timestamp; equal timestamps keep
    /// their input order.
    pub fn visit_hits(self, hits: &[Seconds], mut visit: impl FnMut(StepAction)) {
        let mut at = self.start;
        for (index, &hit) in hits.iter().enumerate() {
            if hit.0 < self.start.0 || hit.0 >= self.end.0 {
                continue;
            }
            if hit.0 > at.0 {
                visit(StepAction::Integrate(Self::new(at, hit)));
            }
            visit(StepAction::Hit(index));
            at = hit;
        }
        if at.0 < self.end.0 {
            visit(StepAction::Integrate(Self::new(at, self.end)));
        }
    }
}

/// The allocation-free action stream produced when a step contains hits.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum StepAction {
    Integrate(StepInterval),
    Hit(usize),
}

/// An allocation-free equal partition of a completed frame interval. The
/// ordinal is only an index; time is always recovered from the two endpoints,
/// so stretched intervals never inherit a nominal-tick identity.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FramePlan {
    pub start: Seconds,
    pub end: Seconds,
    pub intervals: u64,
}

impl FramePlan {
    pub fn new(start: Seconds, end: Seconds, intervals: u64) -> LiveStepOutcome<Self> {
        let mut diagnostic = None;
        let start = if start.0.is_finite() {
            start
        } else {
            diagnostic = Some(LiveStepError::NonFinite("frame-plan endpoint"));
            Seconds::ZERO
        };
        let mut end = if end.0.is_finite() {
            end
        } else {
            diagnostic.get_or_insert(LiveStepError::NonFinite("frame-plan endpoint"));
            start
        };
        if end.0 < start.0 {
            diagnostic.get_or_insert(LiveStepError::InvalidInput(
                "frame-plan end must not precede start",
            ));
            end = start;
        }
        if intervals == 0 && end.0 > start.0 {
            diagnostic.get_or_insert(LiveStepError::InvalidInput(
                "a positive frame span needs an interval",
            ));
            end = start;
        }
        LiveStepOutcome {
            value: Self {
                start,
                end,
                intervals,
            },
            diagnostic,
        }
    }

    #[inline]
    pub fn interval(self, ordinal: u64) -> Option<StepInterval> {
        if ordinal >= self.intervals {
            return None;
        }
        let span = self.end.0 - self.start.0;
        let end = if ordinal + 1 == self.intervals {
            self.end
        } else {
            Seconds(self.start.0 + span * (ordinal + 1) as f64 / self.intervals as f64)
        };
        Some(StepInterval::new(
            Seconds(self.start.0 + span * ordinal as f64 / self.intervals as f64),
            end,
        ))
    }
}

/// A completion fence for one epoch. Submissions carry their epoch and
/// sequence; retirement advances only the next expected sequence, so stale or
/// out-of-order GPU completions cannot claim simulation time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CompletionReceipt {
    pub epoch: u32,
    pub sequence: u64,
    pub interval: StepInterval,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CompletionLedger {
    epoch: u32,
    next_sequence: u64,
    completed: Seconds,
}

impl CompletionLedger {
    pub fn new(epoch: u32, completed: Seconds) -> LiveStepOutcome<Self> {
        let (completed, diagnostic) = if completed.0.is_finite() {
            (completed, None)
        } else {
            (
                Seconds::ZERO,
                Some(LiveStepError::NonFinite("completed time")),
            )
        };
        LiveStepOutcome {
            value: Self {
                epoch,
                next_sequence: 0,
                completed,
            },
            diagnostic,
        }
    }

    #[inline]
    pub fn completed(self) -> Seconds {
        self.completed
    }

    #[inline]
    pub fn next_sequence(self) -> u64 {
        self.next_sequence
    }

    pub fn submit(
        &self,
        epoch: u32,
        sequence: u64,
        interval: StepInterval,
    ) -> LiveStepOutcome<CompletionReceipt> {
        let diagnostic = if !interval.start.0.is_finite() || !interval.end.0.is_finite() {
            Some(LiveStepError::NonFinite("completion interval"))
        } else if interval.end.0 < interval.start.0 {
            Some(LiveStepError::InvalidInput(
                "completion interval end must not precede start",
            ))
        } else {
            None
        };
        LiveStepOutcome {
            value: CompletionReceipt {
                epoch,
                sequence,
                interval,
            },
            diagnostic,
        }
    }

    /// Retire a receipt if it belongs to this epoch and is the next fence in
    /// order. A stale or early receipt is ignored and returns `false`.
    pub fn retire(&mut self, receipt: CompletionReceipt) -> LiveStepOutcome<bool> {
        if !receipt.interval.start.0.is_finite() || !receipt.interval.end.0.is_finite() {
            return LiveStepOutcome::diagnostic(
                false,
                LiveStepError::NonFinite("completion interval"),
            );
        }
        if receipt.interval.end.0 < receipt.interval.start.0 {
            return LiveStepOutcome::diagnostic(
                false,
                LiveStepError::InvalidInput("completion interval end must not precede start"),
            );
        }
        if receipt.epoch != self.epoch || receipt.sequence != self.next_sequence {
            return LiveStepOutcome::ok(false);
        }
        if receipt.interval.start.0 != self.completed.0 {
            return LiveStepOutcome::diagnostic(
                false,
                LiveStepError::InvalidInput(
                    "completion interval does not start at the completed endpoint",
                ),
            );
        }
        self.completed = receipt.interval.end;
        self.next_sequence = self.next_sequence.saturating_add(1);
        LiveStepOutcome::ok(true)
    }

    /// Reset the fence when transport setup/seek starts a new clock epoch.
    pub fn reset(&mut self, epoch: u32, completed: Seconds) -> LiveStepOutcome<()> {
        if !completed.0.is_finite() {
            return LiveStepOutcome::diagnostic((), LiveStepError::NonFinite("completed time"));
        }
        self.epoch = epoch;
        self.next_sequence = 0;
        self.completed = completed;
        LiveStepOutcome::ok(())
    }
}

/// Errors are reserved for a non-finite numerical state or an unusable caller
/// scratch/configuration shape. Exhausting a finite live-step cap is never an
/// error: its last step covers all remaining frame time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiveStepError {
    NonFinite(&'static str),
    InvalidInput(&'static str),
    ScratchTooSmall { required: usize, actual: usize },
}

impl std::fmt::Display for LiveStepError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NonFinite(name) => write!(formatter, "non-finite live-step value: {name}"),
            Self::InvalidInput(message) => write!(formatter, "invalid live-step input: {message}"),
            Self::ScratchTooSmall { required, actual } => write!(
                formatter,
                "speed-limit histogram needs {required} bins, got {actual}"
            ),
        }
    }
}

impl std::error::Error for LiveStepError {}

/// A live numerical result always contains a usable value. Diagnostics are
/// advisory runtime state: callers can surface them while continuing to own
/// transport time and completion ordering.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LiveStepOutcome<T> {
    pub value: T,
    pub diagnostic: Option<LiveStepError>,
}

impl<T> LiveStepOutcome<T> {
    #[inline]
    pub const fn ok(value: T) -> Self {
        Self {
            value,
            diagnostic: None,
        }
    }

    #[inline]
    pub const fn diagnostic(value: T, diagnostic: LiveStepError) -> Self {
        Self {
            value,
            diagnostic: Some(diagnostic),
        }
    }
}

impl<T> std::ops::Deref for LiveStepOutcome<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

/// A reference-engine outer schedule. The caller supplies a freshly measured
/// CFL duration for each call to [`Self::next`].
#[derive(Clone, Copy, Debug)]
pub struct LiveStepSchedule {
    interval: StepInterval,
    cursor: Seconds,
    minimum_step: Seconds,
    max_steps: u32,
    steps_taken: u32,
    diagnostic: Option<LiveStepError>,
}

/// One scheduled outer interval. `hit_cap` is true when the FLIP Fluids final
/// allowed substep branch stretched this interval to the frame endpoint.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScheduledStep {
    pub interval: StepInterval,
    pub hit_cap: bool,
}

impl LiveStepSchedule {
    pub fn new(
        start: Seconds,
        frame_duration: Seconds,
        min_steps: u32,
        max_steps: u32,
    ) -> LiveStepOutcome<Self> {
        let mut diagnostic = None;
        let start = if start.0.is_finite() {
            start
        } else {
            diagnostic = Some(LiveStepError::NonFinite("schedule start"));
            Seconds::ZERO
        };
        let frame_duration = if frame_duration.0.is_finite() && frame_duration.0 > 0.0 {
            frame_duration
        } else {
            diagnostic.get_or_insert(if frame_duration.0.is_finite() {
                LiveStepError::InvalidInput("frame duration")
            } else {
                LiveStepError::NonFinite("frame duration")
            });
            Seconds::ZERO
        };
        let (min_steps, max_steps) = if min_steps == 0 || max_steps == 0 {
            diagnostic.get_or_insert(LiveStepError::InvalidInput(
                "step counts must be positive",
            ));
            (1, 1)
        } else {
            (min_steps, max_steps)
        };
        let end = Seconds(start.0 + frame_duration.0);
        let (end, frame_duration) = if !end.0.is_finite() {
            diagnostic.get_or_insert(LiveStepError::NonFinite("schedule endpoint"));
            (start, Seconds::ZERO)
        } else if end.0 > start.0 {
            (end, frame_duration)
        } else {
            diagnostic.get_or_insert(LiveStepError::InvalidInput(
                "frame duration makes no representable progress",
            ));
            (start, Seconds::ZERO)
        };
        LiveStepOutcome {
            value: Self {
                interval: StepInterval::new(start, end),
                cursor: start,
                minimum_step: Seconds(frame_duration.0 / f64::from(min_steps)),
                max_steps,
                steps_taken: 0,
                diagnostic,
            },
            diagnostic,
        }
    }

    #[inline]
    pub fn steps_taken(self) -> u32 {
        self.steps_taken
    }

    #[inline]
    pub fn is_complete(self) -> bool {
        self.cursor.0 >= self.interval.end.0
    }

    /// Select the next interval using the reference minimum-step schedule.
    /// On the final allowed step, all remaining time is consumed even if it
    /// exceeds the CFL duration. This is the live clock's no-slow-motion rule.
    pub fn next(&mut self, cfl_duration: Seconds) -> LiveStepOutcome<Option<ScheduledStep>> {
        let mut diagnostic = self.diagnostic.take();
        let cfl_duration = if cfl_duration.0.is_finite() && cfl_duration.0 > 0.0 {
            cfl_duration
        } else {
            diagnostic.get_or_insert(if cfl_duration.0.is_finite() {
                LiveStepError::InvalidInput("CFL duration")
            } else {
                LiveStepError::NonFinite("CFL duration")
            });
            self.interval.duration()
        };
        if self.is_complete() {
            return LiveStepOutcome {
                value: None,
                diagnostic,
            };
        }

        let remaining = self.interval.end.0 - self.cursor.0;
        let at_cap = self.steps_taken + 1 >= self.max_steps;
        let duration = if at_cap {
            remaining
        } else {
            let candidate = cfl_duration.0.min(remaining);
            let step_limit =
                self.interval.start.0 + self.minimum_step.0 * f64::from(self.steps_taken + 1);
            if self.cursor.0 + candidate > step_limit {
                self.minimum_step.0.min(remaining)
            } else {
                candidate
            }
        };
        if !duration.is_finite() || duration <= 0.0 {
            return LiveStepOutcome::diagnostic(
                None,
                LiveStepError::NonFinite("scheduled duration"),
            );
        }

        let end = if at_cap || duration >= remaining {
            self.interval.end
        } else {
            let end = self.cursor.0 + duration;
            if end <= self.cursor.0 {
                diagnostic.get_or_insert(LiveStepError::InvalidInput(
                    "scheduled duration makes no representable progress",
                ));
                self.interval.end
            } else {
                Seconds(end)
            }
        };

        let step = ScheduledStep {
            interval: StepInterval::new(self.cursor, end),
            hit_cap: at_cap,
        };
        self.cursor = step.interval.end;
        self.steps_taken += 1;
        LiveStepOutcome {
            value: Some(step),
            diagnostic,
        }
    }
}

/// Optional CFL restrictions copied from `_calculateNextTimeStep`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CflRestrictions {
    pub surface_tension: Option<(f64, f64)>,
    pub color_mixing_rate: Option<f64>,
}

/// Settings used by the live reference CFL rule. Defaults match the vendored
/// FLIP Fluids engine: CFL 5, minimum one step, maximum six steps.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CflPolicy {
    pub cell_size: f64,
    pub cfl: f64,
    pub min_steps: u32,
    pub max_steps: u32,
}

impl Default for CflPolicy {
    fn default() -> Self {
        Self {
            cell_size: 1.0,
            cfl: LIVE_DEFAULT_CFL,
            min_steps: LIVE_DEFAULT_MIN_STEPS,
            max_steps: LIVE_DEFAULT_MAX_STEPS,
        }
    }
}

impl CflPolicy {
    pub fn duration(
        self,
        frame_duration: Seconds,
        max_speed: f64,
        restrictions: CflRestrictions,
    ) -> LiveStepOutcome<Seconds> {
        cfl_step_duration(
            frame_duration,
            self.cell_size,
            self.cfl,
            max_speed,
            restrictions,
        )
    }

    pub fn schedule(
        self,
        start: Seconds,
        frame_duration: Seconds,
    ) -> LiveStepOutcome<LiveStepSchedule> {
        LiveStepSchedule::new(start, frame_duration, self.min_steps, self.max_steps)
    }
}

/// Port of `FluidSimulation::_calculateNextTimeStep`, including its exact
/// epsilon and ceil rule. The caller supplies the current marker/obstacle
/// maximum; no GPU readback or hidden stale fallback occurs here.
pub fn cfl_step_duration(
    frame_duration: Seconds,
    cell_size: f64,
    cfl: f64,
    max_speed: f64,
    restrictions: CflRestrictions,
) -> LiveStepOutcome<Seconds> {
    let mut diagnostic = None;
    let frame_duration = if frame_duration.0.is_finite() && frame_duration.0 > 0.0 {
        frame_duration
    } else {
        diagnostic = Some(if frame_duration.0.is_finite() {
            LiveStepError::InvalidInput("frame duration")
        } else {
            LiveStepError::NonFinite("frame duration")
        });
        Seconds(NOMINAL_TICK_SECONDS)
    };
    let cell_size = if cell_size.is_finite() && cell_size > 0.0 {
        cell_size
    } else {
        diagnostic.get_or_insert(if cell_size.is_finite() {
            LiveStepError::InvalidInput("cell size")
        } else {
            LiveStepError::NonFinite("cell size")
        });
        1.0
    };
    let cfl = if cfl.is_finite() && cfl > 0.0 {
        cfl
    } else {
        diagnostic.get_or_insert(if cfl.is_finite() {
            LiveStepError::InvalidInput("CFL number")
        } else {
            LiveStepError::NonFinite("CFL number")
        });
        LIVE_DEFAULT_CFL
    };
    // A non-finite runtime velocity is reported, then treated as zero for
    // this interval so the transport clock continues and the reference
    // formula remains finite.
    let max_speed = if max_speed.is_finite() && max_speed >= 0.0 {
        max_speed
    } else {
        diagnostic.get_or_insert(if max_speed.is_finite() {
            LiveStepError::InvalidInput("maximum speed")
        } else {
            LiveStepError::NonFinite("maximum speed")
        });
        0.0
    };

    let mut limit = cfl * cell_size / (max_speed + LIVE_CFL_EPSILON);
    if let Some((condition, constant)) = restrictions.surface_tension {
        if !condition.is_finite() || !constant.is_finite() {
            diagnostic.get_or_insert(LiveStepError::NonFinite("surface-tension restriction"));
        } else if condition <= 0.0 || constant < 0.0 {
            diagnostic.get_or_insert(LiveStepError::InvalidInput(
                "surface-tension condition must be positive and constant non-negative",
            ));
        } else {
            limit = limit.min(
                condition
                    * (cell_size * cell_size * cell_size).sqrt()
                    * (1.0 / (constant + LIVE_CFL_EPSILON)).sqrt(),
            );
        }
    }
    if let Some(rate) = restrictions.color_mixing_rate {
        if !rate.is_finite() {
            diagnostic.get_or_insert(LiveStepError::NonFinite("color mixing rate"));
        } else if rate < 0.0 {
            diagnostic.get_or_insert(LiveStepError::InvalidInput(
                "color mixing rate must be non-negative",
            ));
        } else {
            limit = limit.min(1.0 / (rate + LIVE_CFL_EPSILON));
        }
    }
    if !limit.is_finite() || limit <= 0.0 {
        diagnostic.get_or_insert(LiveStepError::NonFinite("CFL limit"));
        return LiveStepOutcome {
            value: frame_duration,
            diagnostic,
        };
    }
    let count = (frame_duration.0 / limit).ceil().max(1.0);
    if !count.is_finite() || frame_duration.0 / count <= 0.0 {
        diagnostic.get_or_insert(LiveStepError::NonFinite("CFL subdivision count"));
        return LiveStepOutcome {
            value: frame_duration,
            diagnostic,
        };
    }
    LiveStepOutcome {
        value: Seconds(frame_duration.0 / count),
        diagnostic,
    }
}

/// Native extreme-particle-removal parameters. These are the pinned engine
/// defaults, including the MANIFOLD floor in `_getMarkerParticleSpeedLimit`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MarkerSpeedLimitConfig {
    pub max_extreme_velocity_removal_percent: f64,
    pub max_extreme_velocity_removal_absolute: usize,
    pub min_time_step_increase_for_removal: u32,
    pub extreme_particle_velocity_threshold_lower: f64,
    pub extreme_particle_velocity_threshold: f64,
    pub max_extreme_velocity_outlier_removal_absolute: usize,
}

impl Default for MarkerSpeedLimitConfig {
    fn default() -> Self {
        Self {
            max_extreme_velocity_removal_percent: 0.0005,
            max_extreme_velocity_removal_absolute: 35,
            min_time_step_increase_for_removal: 4,
            extreme_particle_velocity_threshold_lower: 0.90,
            extreme_particle_velocity_threshold: 0.99999,
            max_extreme_velocity_outlier_removal_absolute: 6,
        }
    }
}

/// Port `_getMarkerParticleSpeedLimit`. `dt` is the whole accepted frame, not
/// the substep: the engine passes `_currentFrameDeltaTime` at every substep.
/// `histogram` is caller-owned scratch with one bin per allowed frame step;
/// this function performs no allocation.
pub fn marker_particle_speed_limit(
    speeds: &[f64],
    dt: Seconds,
    cell_size: f64,
    cfl: f64,
    max_frame_steps: u32,
    config: MarkerSpeedLimitConfig,
    histogram: &mut [usize],
) -> LiveStepOutcome<f64> {
    let mut diagnostic = None;
    let dt = if dt.0.is_finite() && dt.0 > 0.0 {
        dt.0
    } else {
        diagnostic = Some(if dt.0.is_finite() {
            LiveStepError::InvalidInput("speed-limit duration")
        } else {
            LiveStepError::NonFinite("speed-limit duration")
        });
        NOMINAL_TICK_SECONDS
    };
    let cell_size = if cell_size.is_finite() && cell_size > 0.0 {
        cell_size
    } else {
        diagnostic.get_or_insert(if cell_size.is_finite() {
            LiveStepError::InvalidInput("cell size")
        } else {
            LiveStepError::NonFinite("cell size")
        });
        1.0
    };
    let cfl = if cfl.is_finite() && cfl > 0.0 {
        cfl
    } else {
        diagnostic.get_or_insert(if cfl.is_finite() {
            LiveStepError::InvalidInput("CFL number")
        } else {
            LiveStepError::NonFinite("CFL number")
        });
        LIVE_DEFAULT_CFL
    };
    if max_frame_steps == 0 {
        diagnostic.get_or_insert(LiveStepError::InvalidInput(
            "cell size, CFL, and max frame steps must be positive",
        ));
    }
    let max_frame_steps = max_frame_steps.max(1);
    let requested_bins = max_frame_steps as usize;
    let bins = requested_bins.min(histogram.len());
    if histogram.len() < requested_bins {
        diagnostic.get_or_insert(LiveStepError::ScratchTooSmall {
            required: requested_bins,
            actual: histogram.len(),
        });
    }
    let mut removal_percent = config.max_extreme_velocity_removal_percent;
    let mut lower_threshold = config.extreme_particle_velocity_threshold_lower;
    let mut upper_threshold = config.extreme_particle_velocity_threshold;
    if [
        config.max_extreme_velocity_removal_percent,
        config.extreme_particle_velocity_threshold_lower,
        config.extreme_particle_velocity_threshold,
    ]
    .iter()
    .any(|value| !value.is_finite())
    {
        diagnostic.get_or_insert(LiveStepError::NonFinite("speed-limit configuration"));
        removal_percent = MarkerSpeedLimitConfig::default().max_extreme_velocity_removal_percent;
        lower_threshold =
            MarkerSpeedLimitConfig::default().extreme_particle_velocity_threshold_lower;
        upper_threshold = MarkerSpeedLimitConfig::default().extreme_particle_velocity_threshold;
    }

    if bins == 0 {
        let floor = f64::from(max_frame_steps) * cfl * cell_size / dt;
        return LiveStepOutcome {
            value: floor,
            diagnostic,
        };
    }
    histogram[..bins].fill(0);
    let speed_limit_step = cfl * cell_size / dt;
    if !speed_limit_step.is_finite() || speed_limit_step <= 0.0 {
        diagnostic.get_or_insert(LiveStepError::NonFinite("speed-limit bin width"));
        return LiveStepOutcome {
            value: 0.0,
            diagnostic,
        };
    }
    let mut max_particle_speed: f64 = 0.0;
    for &speed in speeds {
        if !speed.is_finite() || speed < 0.0 {
            diagnostic.get_or_insert(LiveStepError::NonFinite("marker speed"));
            continue;
        }
        let index = (speed / speed_limit_step).floor() as usize;
        histogram[index.min(bins - 1)] += 1;
        max_particle_speed = max_particle_speed.max(speed);
    }

    let max_removal = ((speeds.len() as f64 * removal_percent) as usize)
        .min(config.max_extreme_velocity_removal_absolute);
    let floor = f64::from(max_frame_steps) * speed_limit_step;
    let mut maxspeed = floor;
    let mut current_removal = 0usize;
    for index in (1..bins).rev() {
        if current_removal + histogram[index] > max_removal {
            break;
        }
        current_removal += histogram[index];
        maxspeed = f64::from(
            (index as u32 + config.min_time_step_increase_for_removal).max(max_frame_steps),
        ) * speed_limit_step;
    }

    let lower = lower_threshold * max_particle_speed;
    let upper = upper_threshold * max_particle_speed;
    let lower_count = speeds
        .iter()
        .filter(|&&speed| speed >= lower && speed < upper)
        .count();
    let outlier_count = speeds.iter().filter(|&&speed| speed >= upper).count();
    if outlier_count <= config.max_extreme_velocity_outlier_removal_absolute
        && lower_count <= config.max_extreme_velocity_outlier_removal_absolute
    {
        maxspeed = maxspeed.min(upper);
    }

    // MANIFOLD: a relative outlier in a small population can still be slow.
    // Never remove particles that fit within the configured frame's CFL and
    // substep budget merely because they are the fastest remaining particles.
    LiveStepOutcome {
        value: maxspeed.max(floor),
        diagnostic,
    }
}

/// Box3D's longer accepted interval is internally kept at no more than one
/// quarter of the nominal 60 Hz tick, with the reference minimum of four.
pub fn box3d_substep_count(duration: Seconds) -> LiveStepOutcome<u32> {
    let duration = if duration.0.is_finite() && duration.0 > 0.0 {
        duration.0
    } else {
        return LiveStepOutcome::diagnostic(
            4,
            if duration.0.is_finite() {
                LiveStepError::InvalidInput("Box3D duration")
            } else {
                LiveStepError::NonFinite("Box3D duration")
            },
        );
    };
    let count = (4.0 * duration / NOMINAL_TICK_SECONDS).ceil().max(4.0);
    if !count.is_finite() || count > f64::from(u32::MAX) {
        return LiveStepOutcome::diagnostic(
            u32::MAX,
            LiveStepError::NonFinite("Box3D substep count"),
        );
    }
    LiveStepOutcome::ok(count as u32)
}

/// Pressure coupling is an impulse in mass-scaled units. Convert it once to a
/// delta velocity; callers must not multiply this captured impulse by duration.
#[inline]
pub fn pressure_impulse_delta_velocity(impulse: f64, inverse_mass: f64) -> f64 {
    impulse * inverse_mass
}

/// A backend participating in the rigid owner's fixed ticks. The returned
/// guard exclusively borrows its native state until the complete tick finishes.
pub trait StepCoupling {
    type Error: Display;
    type Frame<'a>: SubstepExchange<Error = Self::Error>
    where
        Self: 'a;

    fn begin_tick(
        &mut self,
        stamp: TickStamp,
        duration: Seconds,
    ) -> Result<Self::Frame<'_>, Self::Error>;
}

/// One unpublished tick. An owner must consume each selected interval once,
/// step Box3D once for it, then finish before publishing either solver's output.
/// Implementations invalidate incomplete native work when dropped.
pub trait SubstepExchange {
    type Error: Display;

    /// Read fresh rigid state after authored motion and forces are queued.
    /// Return a finite positive interval no greater than `maximum`.
    fn next_substep(
        &mut self,
        rigid: &PhysicsWorld,
        maximum: Seconds,
    ) -> Result<Seconds, Self::Error>;

    /// Advance the other solver and apply its reaction to the rigid world.
    /// Do not advance Box3D here: the existing owner does so immediately after.
    fn exchange(&mut self, rigid: &mut PhysicsWorld, duration: Seconds) -> Result<(), Self::Error>;

    /// Complete the participant and capture the paired native rigid state.
    /// This runs after the final rigid substep and before later authored
    /// edits or release events can change the world. A failed capture must
    /// leave both published outputs at their previous accepted tick.
    fn finish(self, rigid: &PhysicsWorld) -> Result<(), Self::Error>;
}

/// Rigid-only scenes use the identical tick owner without a second backend.
pub struct Uncoupled;

impl StepCoupling for Uncoupled {
    type Error = Infallible;
    type Frame<'a> = Self;

    fn begin_tick(&mut self, _: TickStamp, _: Seconds) -> Result<Self::Frame<'_>, Self::Error> {
        Ok(Self)
    }
}

impl SubstepExchange for Uncoupled {
    type Error = Infallible;

    fn next_substep(&mut self, _: &PhysicsWorld, maximum: Seconds) -> Result<Seconds, Self::Error> {
        Ok(maximum)
    }

    fn exchange(&mut self, _: &mut PhysicsWorld, _: Seconds) -> Result<(), Self::Error> {
        Ok(())
    }

    fn finish(self, _: &PhysicsWorld) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TICK: f64 = 1.0 / 60.0;

    #[test]
    fn live_marker_speed_limit_replaces_each_histogram_threshold() {
        // Reference assignment descends from nine cells/frame to six.
        let mut speeds = vec![1.5; 4000];
        speeds[0] = 200.0;
        speeds[1] = 200.0;
        let mut histogram = [0; 6];
        let limit = marker_particle_speed_limit(
            &speeds,
            Seconds(1.0),
            0.2,
            5.0,
            6,
            MarkerSpeedLimitConfig::default(),
            &mut histogram,
        )
        .value;
        assert_eq!(limit, 6.0);
    }

    #[test]
    fn live_step_cap_keeps_time_without_error() {
        let mut schedule = LiveStepSchedule::new(Seconds::ZERO, Seconds(0.2), 1, 6).value;
        let mut covered = 0.0;
        let mut count = 0;
        let mut capped = false;
        while let Some(step) = schedule.next(Seconds(0.001)).value {
            covered += step.interval.duration().0;
            count += 1;
            capped |= step.hit_cap;
        }
        assert_eq!(count, 6);
        assert!(capped);
        assert!((covered - 0.2).abs() < 1e-12);
        assert_eq!(schedule.next(Seconds(0.001)).value, None);
    }

    #[test]
    fn live_interval_hits_land_inside_capped_step() {
        let mut schedule = LiveStepSchedule::new(Seconds::ZERO, Seconds(0.2), 1, 1).value;
        let step = schedule.next(Seconds(0.001)).value.unwrap();
        assert!(step.hit_cap);
        let hits = [Seconds(0.03), Seconds(0.03), Seconds(0.19)];
        let mut actions = Vec::new();
        step.interval
            .visit_hits(&hits, |action| actions.push(action));
        assert_eq!(
            actions,
            vec![
                StepAction::Integrate(StepInterval::new(Seconds::ZERO, Seconds(0.03))),
                StepAction::Hit(0),
                StepAction::Hit(1),
                StepAction::Integrate(StepInterval::new(Seconds(0.03), Seconds(0.19))),
                StepAction::Hit(2),
                StepAction::Integrate(StepInterval::new(Seconds(0.19), Seconds(0.2))),
            ]
        );
    }

    #[test]
    fn live_step_minimum_schedule_matches_reference() {
        let mut schedule = LiveStepSchedule::new(Seconds::ZERO, Seconds(0.1), 2, 6).value;
        let mut durations = Vec::new();
        while let Some(step) = schedule.next(Seconds(0.08)).value {
            durations.push(step.interval.duration().0);
        }
        assert_eq!(durations, vec![0.05, 0.05]);
    }

    #[test]
    fn live_cfl_epsilon_and_ceil_match_reference() {
        let duration =
            cfl_step_duration(Seconds(1.0), 2.0, 5.0, 3.0, CflRestrictions::default()).value;
        let limit: f64 = 5.0 * 2.0 / (3.0 + 1e-6);
        let expected = 1.0 / (1.0 / limit).ceil().max(1.0);
        assert_eq!(duration, Seconds(expected));
    }

    #[test]
    fn live_nonfinite_speed_reports_diagnostic_and_keeps_time() {
        let outcome =
            cfl_step_duration(Seconds(0.1), 0.1, 2.0, f64::NAN, CflRestrictions::default());
        assert_eq!(
            outcome.diagnostic,
            Some(LiveStepError::NonFinite("maximum speed"))
        );
        assert!(outcome.value.0.is_finite() && outcome.value.0 > 0.0);

        let mut histogram = [0; 6];
        let speed = marker_particle_speed_limit(
            &[1.0, f64::NAN, 2.0],
            Seconds(TICK),
            1.0,
            5.0,
            6,
            MarkerSpeedLimitConfig::default(),
            &mut histogram,
        );
        assert_eq!(
            speed.diagnostic,
            Some(LiveStepError::NonFinite("marker speed"))
        );
        assert_eq!(speed.value, 6.0 * 5.0 / TICK);
    }

    #[test]
    fn live_invalid_schedule_stays_non_panicking_and_reports_diagnostic() {
        let mut schedule = LiveStepSchedule::new(Seconds::ZERO, Seconds(0.1), 1, 6).value;
        let outcome = schedule.next(Seconds(f64::NAN));
        assert_eq!(
            outcome.diagnostic,
            Some(LiveStepError::NonFinite("CFL duration"))
        );
        assert!(outcome.value.is_some());
        let overflow = LiveStepSchedule::new(Seconds(f64::MAX), Seconds(f64::MAX), 1, 6);
        assert_eq!(overflow.diagnostic, Some(LiveStepError::NonFinite("schedule endpoint")));
        assert!(overflow.value.is_complete());
    }

    #[test]
    fn live_marker_speed_limit_preserves_engine_floor() {
        let mut histogram = [0; 6];
        let floor = marker_particle_speed_limit(
            &[0.0],
            Seconds(TICK),
            1.0,
            5.0,
            6,
            MarkerSpeedLimitConfig::default(),
            &mut histogram,
        )
        .value;
        assert_eq!(floor, 6.0 * 5.0 / TICK);
    }

    #[test]
    fn live_pressure_units_and_box3d_substeps() {
        assert_eq!(pressure_impulse_delta_velocity(12.0, 0.25), 3.0);
        assert_eq!(box3d_substep_count(Seconds(TICK)).value, 4);
        assert_eq!(box3d_substep_count(Seconds(0.1)).value, 24);
    }

    #[test]
    fn live_interval_completion_receipts() {
        let mut ledger = CompletionLedger::new(7, Seconds::ZERO).value;
        let first = ledger
            .submit(7, 0, StepInterval::new(Seconds::ZERO, Seconds(0.1)))
            .value;
        let second = ledger
            .submit(7, 1, StepInterval::new(Seconds(0.1), Seconds(0.2)))
            .value;
        assert!(!ledger.retire(second).value);
        assert!(ledger.retire(first).value);
        assert!(ledger.retire(second).value);
        assert_eq!(ledger.completed(), Seconds(0.2));
        let stale = ledger
            .submit(6, 2, StepInterval::new(Seconds(0.2), Seconds(0.3)))
            .value;
        assert!(!ledger.retire(stale).value);
    }

    #[test]
    fn live_interval_completion_rejects_public_nonfinite_receipt() {
        let mut ledger = CompletionLedger::new(1, Seconds::ZERO).value;
        let receipt = CompletionReceipt {
            epoch: 1,
            sequence: 0,
            interval: StepInterval::new(Seconds::ZERO, Seconds(f64::NAN)),
        };
        let outcome = ledger.retire(receipt);
        assert!(!outcome.value);
        assert_eq!(
            outcome.diagnostic,
            Some(LiveStepError::NonFinite("completion interval"))
        );
    }

    #[test]
    fn live_interval_coupled_endpoints() {
        let plan = FramePlan::new(Seconds(2.0), Seconds(2.3), 3).value;
        let mut fluid = CompletionLedger::new(4, Seconds(2.0)).value;
        let mut rigid = CompletionLedger::new(4, Seconds(2.0)).value;
        for sequence in 0..3 {
            let interval = plan.interval(sequence).unwrap();
            let fluid_receipt = fluid.submit(4, sequence, interval).value;
            let rigid_receipt = rigid.submit(4, sequence, interval).value;
            assert!(fluid.retire(fluid_receipt).value);
            assert!(rigid.retire(rigid_receipt).value);
            assert_eq!(fluid.completed(), rigid.completed());
        }
        assert_eq!(fluid.completed(), Seconds(2.3));
    }
    #[test]
    fn live_cap_takes_precedence_over_a_larger_minimum_step_setting() {
        // Native FLIP setters accept these independently. A large authored
        // minimum must not silently increase the maximum allowed work.
        let outcome = LiveStepSchedule::new(Seconds::ZERO, Seconds(0.1), 64, 6);
        assert!(outcome.diagnostic.is_none());
        let mut schedule = outcome.value;
        let mut count = 0;
        let mut final_step = None;
        while let Some(step) = schedule.next(Seconds(0.1)).value {
            count += 1;
            final_step = Some(step);
        }
        assert_eq!(count, 6);
        let step = final_step.unwrap();
        assert!(step.hit_cap);
        assert_eq!(step.interval.end, Seconds(0.1));
        assert!(step.interval.duration().0 > 0.09);
    }
}
