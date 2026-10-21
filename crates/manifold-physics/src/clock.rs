//! Transport-owned simulation intervals on the Sim Rate grid. Live and export
//! accept fixed boundaries. Live accepts at most two intervals (one after a
//! late frame), then discards excess transport time and reanchors. Sequence is identity, never elapsed time. Submission and fenced
//! completion belong to consumers.

use std::sync::Arc;

use crate::{PlayedTime, Seconds};
use crate::stepping::{FramePlan, StepInterval};

pub const TICK: f64 = 1.0 / 60.0;
pub const MAX_LIVE_INTERVALS: u64 = 2;
/// Transport time going back by more than this is a seek or loop: the clock
/// restarts, and so does anything integrating over the same transport.
pub const BACKWARD_SEEK: f64 = 1e-9;
/// The physics history replay evaluates authored inputs at every multiple of
/// 1 / REPLAY_RATE seconds of transport, whatever the display rate. Every
/// Sim Rate divides it.
pub const REPLAY_RATE: f64 = 240.0;

/// The first replay grid point at or after `played`, by the replay's own
/// formula (`k / REPLAY_RATE`), so the two agree bit for bit.
pub fn next_replay_point(played: PlayedTime) -> PlayedTime {
    PlayedTime::from_played((played.seconds() * REPLAY_RATE - 1e-9).ceil() / REPLAY_RATE)
}

/// The previous live frame's measured load (render work plus GPU surface wait)
/// against the display frame budget. A live frame takes a second interval only
/// after an on-time frame. After a late frame a second interval would make this
/// one late too and the next owe two again, halving the frame rate without
/// keeping real time; one interval plays the overload as smooth slow motion.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LiveLoad {
    pub previous: Seconds,
    pub budget: Seconds,
}


/// Accepted frame intervals and transport/display endpoints.
#[derive(Clone, Debug, PartialEq)]
pub struct ClockFrame {
    pub plan: FramePlan,
    pub first_sequence: u64,
    pub numerical_error: bool,
    pub offline: bool,
    simulation_interval: f64,
    transport_origin: f64,
    transport_first: u64,
    transport_intervals: u64,
    speed_history: Arc<Vec<SpeedAnchor>>,
    pub ticks: u32,
    pub epoch: u32,
    /// This frame starts a new simulation (first frame, reset, setup change or
    /// backward seek); the state reseeds before any tick runs.
    pub restarted: bool,
    /// Excess live transport was discarded after this frame's accepted steps.
    /// The next tick starts at the current transport, independently of the
    /// previous tick's end pose.
    pub reanchored: bool,
    /// Transport paused or Simulation Speed 0 now. Impulses fired now are
    /// discarded, so resume never bursts.
    pub held: bool,
    /// Simulated seconds at the end of this frame's ticks.
    pub simulation_time: f64,
    /// Continuous simulation target for observed transport. Accepted work
    /// stops at the last Sim Rate boundary transport has reached.
    pub target_time: f64,
    /// Display time: the simulation one Sim Rate interval of transport ago,
    /// through the Speed history (surface design D10).
    pub display_time: f64,
    /// Simulated seconds discarded under overload since this epoch began.
    pub dropped_seconds: f64,
    /// Simulated seconds this frame's reanchor discarded; 0 without one.
    pub fresh_dropped_seconds: f64,
    /// Whole Sim Rate boundaries transport crossed since the last accepted
    /// one. 0 on a frame that ticks nothing because no boundary arrived.
    pub due: u32,
    /// The live acceptance cap in force (1 after a late frame, else 2).
    /// None offline, where every owed interval runs.
    pub live_cap: Option<u32>,
    /// The played time this frame observed.
    pub transport: PlayedTime,
}

/// One transport clock shared by physics consumers.
#[derive(Clone, Debug, Default)]
pub struct SimulationClock {
    /// Process-unique identity for telemetry, assigned at the first advance.
    /// 0 until then. Never reused, unlike an address.
    instance: ClockInstance,
    /// 0 before the first start, so outputs a domain holds while it waits
    /// (for a role's geometry, say) never share an epoch with the first
    /// simulation, which then seeds.
    epoch: u32,
    started: bool,
    last_transport: f64,
    last_accepted_transport: f64,
    offline: bool,
    previous_reset: Option<f32>,
    target_time: f64,
    ticks_done: u64,
    simulation_time: f64,
    dropped_seconds: f64,
    simulation_interval: f64,
    transport_origin: f64,
    transport_done: u64,
    /// The last valid Speed a frame passed in, held over an invalid one.
    speed: f64,
    /// Retain the continuity anchor and all speed edits after accepted time.
    speed_history: Vec<SpeedAnchor>,
    history_snapshots: Vec<Arc<Vec<SpeedAnchor>>>,
    /// This frame's live load, set by the owner before it observes or
    /// advances. None (export, tests, previews) keeps the two-interval cap.
    live_load: Option<LiveLoad>,
    /// The played-time target of the jump this frame landed on, set by the
    /// owner with the live load. A restart starts its grid there.
    played_target: Option<f64>,
    /// Boundaries the restarting frame had already reached when a restart
    /// began its grid at an earlier transport target. The jump owes them,
    /// not a late frame, so the live cap counts from there.
    restart_reached: u64,
    /// The transport of the latest replay observation. The frame's own Speed
    /// at that same transport does not replace it: one rule evaluates Speed
    /// at a tick start whether or not a frame ends there.
    sampled_at: Option<f64>,
    /// The newest placement's target and the transport its frame landed on,
    /// while the transport stays there: a restart for any reason on that
    /// frame starts its grid at the target.
    placement: Option<(f64, f64)>,
}

/// A clone advances on its own, so it never inherits the original's identity:
/// it starts unassigned and takes a fresh id at its first advance.
#[derive(Debug, Default)]
struct ClockInstance(u64);

impl Clone for ClockInstance {
    fn clone(&self) -> Self {
        Self(0)
    }
}

/// Piecewise-constant speed, integrated once at each edit. Within an epoch an
/// edit sits on a tick start, so every tick runs at one speed. Accepted
/// frames retain an immutable view; later edits cannot rewrite their endpoints.
#[derive(Clone, Copy, Debug, PartialEq)]
struct SpeedAnchor {
    transport: f64,
    simulation: f64,
    speed: f64,
}

fn live_cap_for(load: Option<LiveLoad>) -> u64 {
    match load {
        Some(load) if load.previous.0 > load.budget.0 => 1,
        _ => MAX_LIVE_INTERVALS,
    }
}

fn map_transport(anchors: &[SpeedAnchor], transport: f64) -> f64 {
    let index = anchors.partition_point(|anchor| anchor.transport <= transport);
    anchors.get(index.saturating_sub(1)).map_or(0.0, |anchor| {
        anchor.simulation + (transport - anchor.transport).max(0.0) * anchor.speed
    })
}

impl SimulationClock {
    /// Observe controls without accepting a render interval or consuming ticks.
    /// History/source capture and rendering share this exact time mapping.
    /// A replay sample observes here; the frame's own observation at the
    /// same transport, in [`Self::advance`], leaves it standing.
    pub fn observe_speed(&mut self, played: PlayedTime, speed: f32) -> f64 {
        let transport = played.seconds();
        if transport.is_finite() {
            self.sampled_at = Some(transport);
        }
        self.anchor_speed(transport, speed)
    }

    fn anchor_speed(&mut self, transport: f64, speed: f32) -> f64 {
        if !transport.is_finite() || !speed.is_finite() || speed < 0.0 {
            return self.target_time;
        }
        let observed_simulation = self.simulation_at_seconds(transport, self.live_load);
        // A late source/audio observation may read accepted time, but may
        // only change the speed of time we have not accepted yet.
        let accepted_transport = if self.started {
            self.last_accepted_transport
        } else {
            self.last_transport
        };
        // Speed applies from tick starts only (D4): an observation between
        // two starts takes effect at the next one, so where speed changes
        // depends on the Sim Rate grid, never on when frames land.
        let transport = if self.started && !self.speed_history.is_empty() {
            self.next_tick_start(transport.max(accepted_transport))
        } else {
            transport
        };
        let speed = f64::from(speed);
        let history = &mut self.speed_history;
        // The latest observation for a tick start wins. Dropping the earlier
        // one, rather than overwriting it, keeps an unchanged speed from
        // leaving an anchor behind, so equal speeds give equal anchors and
        // bit-equal intervals whatever frames observed them.
        if history.len() > 1 && history.last().is_some_and(|last| last.transport == transport) {
            history.pop();
        }
        match history.last_mut() {
            Some(last) if last.transport == transport => last.speed = speed,
            Some(last) if last.transport > transport || last.speed == speed => {}
            _ => {
                let simulation = map_transport(history, transport);
                history.push(SpeedAnchor { transport, simulation, speed });
            }
        }
        observed_simulation
    }

    /// The first tick start at or after `transport` on this epoch's grid,
    /// rounded as `advance` counts reached boundaries.
    fn next_tick_start(&self, transport: f64) -> f64 {
        let index = ((transport - self.transport_origin) / self.simulation_interval - 1e-9).ceil().max(0.0);
        self.transport_origin + index * self.simulation_interval
    }

    /// Intervals this live frame may accept. The load is fixed for the whole
    /// frame, so observers and `advance` agree on the cap.
    fn live_cap(&self) -> u64 {
        live_cap_for(self.live_load)
    }

    /// The boundary the live cap counts from: the last one accepted, or one
    /// a restart had already reached at its first frame.
    fn cap_from(&self) -> u64 {
        self.transport_done.max(self.restart_reached)
    }

    /// The last boundary of this epoch's grid that `transport` has reached,
    /// by the rounding `advance` accepts boundaries with; negative before
    /// the origin.
    fn reached_index(&self, transport: f64) -> f64 {
        ((transport - self.transport_origin) / self.simulation_interval + 1e-9).floor()
    }

    /// Set once per frame, before `simulation_at`, `tick_starts` or
    /// `advance`, so observers and `advance` agree on the cap.
    pub fn set_live_load(&mut self, load: Option<LiveLoad>) {
        self.live_load = load;
    }

    /// Set once per frame with the live load: the time the transport jumped
    /// to when this frame is the one the jump landed on, else None.
    pub fn set_played_target(&mut self, target: Option<PlayedTime>) {
        self.played_target = target.map(PlayedTime::seconds);
    }

    pub fn rate_changed(&self, interval: f64) -> bool {
        self.started && self.simulation_interval != interval
    }

    /// Process-unique identity for telemetry; 0 before the first advance.
    pub fn instance(&self) -> u64 {
        self.instance.0
    }

    pub fn accepted_time(&self) -> f64 {
        self.simulation_time
    }

    /// Simulation time at `played`, under this frame's `load`. The caller
    /// passes the load from its own frame step, so an observation made before
    /// the owner's [`Self::set_live_load`] still uses this frame's cap.
    pub fn simulation_at(&self, played: PlayedTime, load: Option<LiveLoad>) -> f64 {
        self.simulation_at_seconds(played.seconds(), load)
    }

    fn simulation_at_seconds(&self, transport: f64, load: Option<LiveLoad>) -> f64 {
        // Source observations may arrive before the render accepts its steps.
        // They use the same live ceiling: later input holds at the next
        // accepted boundary instead of creating timestamps in discarded time.
        let transport = if self.started && !self.offline {
            transport.min(self.transport_origin
                + (self.cap_from() + live_cap_for(load)) as f64 * self.simulation_interval)
        } else { transport };
        map_transport(&self.speed_history, transport)
    }

    // Reuse retired immutable snapshots instead of allocating in the frame
    // path. Consumers keep their accepted view until they have drained it.
    fn snapshot_history(&mut self) -> Arc<Vec<SpeedAnchor>> {
        let index = self.history_snapshots.iter().position(|slot| Arc::strong_count(slot) == 1)
            .unwrap_or_else(|| {
                self.history_snapshots.push(Arc::new(Vec::with_capacity(256)));
                self.history_snapshots.len() - 1
            });
        let snapshot = Arc::get_mut(&mut self.history_snapshots[index]).expect("retired snapshot");
        snapshot.clear();
        snapshot.extend_from_slice(&self.speed_history);
        self.history_snapshots[index].clone()
    }

    /// The transport time not-yet-run tick `tick` starts at, under the
    /// speeds observed so far. A transport interval at Speed 0 runs no tick,
    /// so it is skipped. None before the clock starts, for a tick already
    /// run, or once Speed stays 0.
    pub fn tick_start(&self, tick: u64) -> Option<PlayedTime> {
        self.tick_start_at(tick).map(PlayedTime::from_played)
    }

    fn tick_start_at(&self, tick: u64) -> Option<f64> {
        if !self.started || tick < self.ticks_done {
            return None;
        }
        let at = |index: u64| self.boundary(index);
        let mut remaining = tick - self.ticks_done;
        let mut index = self.transport_done;
        let mut next = self.speed_history.partition_point(|anchor| anchor.transport <= at(index));
        let mut speed = self.speed_history.get(next.checked_sub(1)?)?.speed;
        loop {
            // Anchors sit on tick starts, so each transport interval runs at
            // the speed of the last anchor at or before its start.
            if next == self.speed_history.len() {
                return (speed > 0.0).then(|| at(index + remaining));
            }
            if speed > 0.0 {
                if remaining == 0 {
                    return Some(at(index));
                }
                remaining -= 1;
            }
            index += 1;
            while self.speed_history.get(next).is_some_and(|anchor| anchor.transport <= at(index)) {
                speed = self.speed_history[next].speed;
                next += 1;
            }
        }
    }

    /// The played time of the last frame.
    pub fn transport(&self) -> PlayedTime {
        PlayedTime::from_played(self.last_transport)
    }

    /// Ticks run since the epoch began: the next tick to run.
    pub fn ticks_done(&self) -> u64 {
        self.ticks_done
    }

    /// Every not-yet-run Sim Rate boundary in `(from, until]` of transport
    /// time, ascending: each place a tick may start. Speed does not narrow
    /// it, since a Speed the replay is still to observe can start or stop a
    /// tick at any of them; [`Self::tick_at`] names the tick once the
    /// boundary's own Speed is known. Live visits only the accepted
    /// intervals and their closing endpoint, never discarded time.
    /// `from` and `until` reach a boundary by the rounding `advance` accepts
    /// it with, so a boundary a rounding past a frame belongs to that frame
    /// and to no other.
    pub fn tick_starts(&self, from: PlayedTime, until: PlayedTime, mut visit: impl FnMut(PlayedTime)) {
        let (from, until) = (from.seconds(), until.seconds());
        if !self.started {
            return;
        }
        let last = self.reached_index(until);
        if last.is_nan() || last < 0.0 {
            return;
        }
        let first = (self.reached_index(from) + 1.0).max(self.transport_done as f64) as u64;
        let mut last = last as u64;
        if !self.offline {
            last = last.min(self.cap_from() + self.live_cap());
        }
        for index in first..=last {
            visit(PlayedTime::from_played(self.boundary(index)));
        }
    }

    /// The tick starting at boundary `transport` (one [`Self::tick_starts`]
    /// visited), under the speeds observed so far. Call it once Speed at that
    /// boundary has been observed: every anchor at or before it is then
    /// final. None when that interval runs at Speed 0, the boundary has
    /// already run, or `transport` is not a boundary of this epoch.
    pub fn tick_at(&self, played: PlayedTime) -> Option<u64> {
        let index = self.boundary_index(played.seconds())?;
        if index < self.transport_done || !self.interval_runs(index) {
            return None;
        }
        Some(self.ticks_before(index))
    }

    /// The tick that would follow a tick ending at boundary `transport` where
    /// no tick starts, because Speed there is 0: its value is that tick's
    /// end. Same calling rule as [`Self::tick_at`].
    pub fn tick_closing_at(&self, played: PlayedTime) -> Option<u64> {
        let index = self.boundary_index(played.seconds())?;
        if index <= self.transport_done || self.interval_runs(index) || !self.interval_runs(index - 1) {
            return None;
        }
        Some(self.ticks_before(index))
    }

    /// The index of boundary `transport` of this epoch's grid, exactly.
    fn boundary_index(&self, transport: f64) -> Option<u64> {
        if !self.started {
            return None;
        }
        let index = ((transport - self.transport_origin) / self.simulation_interval).round();
        if index.is_nan() || index < 0.0 || self.boundary(index as u64) != transport {
            return None;
        }
        Some(index as u64)
    }

    /// Ticks run, and to run, before not-yet-run boundary `index`.
    fn ticks_before(&self, index: u64) -> u64 {
        let earlier = (self.transport_done..index).filter(|&i| self.interval_runs(i)).count();
        self.ticks_done + earlier as u64
    }

    /// Transport time of Sim Rate boundary `index` of this epoch's grid, by
    /// the one formula every boundary comparison uses.
    fn boundary(&self, index: u64) -> f64 {
        self.transport_origin + index as f64 * self.simulation_interval
    }

    /// Whether transport interval `index` runs a tick: its start's Speed is
    /// above 0.
    fn interval_runs(&self, index: u64) -> bool {
        let start = self.boundary(index);
        let anchors = self.speed_history.partition_point(|anchor| anchor.transport <= start);
        anchors.checked_sub(1).is_some_and(|last| self.speed_history[last].speed > 0.0)
    }

    /// Make the next frame a restart in a new epoch. The domain's
    /// `clear_state` calls this: the runtime's state reset (export start,
    /// resize, an idle chain) must reseed the liquid even when the transport
    /// runs on, and the epoch never repeats, so the state node reseeds too.
    pub fn restart(&mut self) {
        self.started = false;
    }

    /// Accept work up to the last Sim Rate boundary transport has reached.
    /// Export runs every owed interval; live runs at most two (one after a late
    /// frame, see `LiveLoad`), then reanchors without retaining a catch-up
    /// debt. Interval boundaries never depend on display or output fps. A rate
    /// edit starts one epoch; Speed is integrated exactly once.
    pub fn advance(
        &mut self,
        played: PlayedTime,
        simulation_interval: f64,
        speed: f32,
        reset: f32,
        setup_changed: bool,
        offline: bool,
    ) -> ClockFrame {
        let transport = played.seconds();
        if self.instance.0 == 0 {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
            self.instance.0 = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        let mut numerical_error =
            !transport.is_finite() || !speed.is_finite() || speed < 0.0 || !reset.is_finite();
        // Invalid authored clock input has no meaningful elapsed duration. Keep
        // the previous endpoint and expose the fault; the next valid observation
        // still accounts for its full transport span.
        let transport = if transport.is_finite() {
            transport
        } else {
            self.last_transport
        };
        let speed = if speed.is_finite() && speed >= 0.0 {
            speed
        } else {
            self.speed as f32
        };
        let reset = if reset.is_finite() {
            reset
        } else {
            self.previous_reset.unwrap_or(0.0)
        };
        self.offline = offline;
        let rate_changed = self.started && self.simulation_interval != simulation_interval;
        {
            if simulation_interval.is_finite() && simulation_interval > 0.0 {
                self.simulation_interval = simulation_interval;
            } else {
                numerical_error = true;
                // No schedule can be inferred from an invalid simulation rate.
                return ClockFrame {
                    plan: FramePlan {
                        start: Seconds(self.simulation_time),
                        end: Seconds(self.simulation_time),
                        intervals: 0,
                    },
                    first_sequence: self.ticks_done,
                    numerical_error,
                    offline,
                    simulation_interval: 0.0,
                    transport_origin: self.transport_origin,
                    transport_first: self.transport_done,
                    transport_intervals: 0,
                    speed_history: self.snapshot_history(),
                    ticks: 0,
                    epoch: self.epoch,
                    restarted: false,
                    reanchored: false,
                    held: true,
                    simulation_time: self.simulation_time,
                    target_time: self.target_time,
                    display_time: self.simulation_time,
                    dropped_seconds: self.dropped_seconds,
                    fresh_dropped_seconds: 0.0,
                    due: 0,
                    live_cap: (!offline).then(|| self.live_cap() as u32),
                    transport: PlayedTime::from_played(transport),
                };
            }
        }
        // A trigger publishes a counter; any change (undo included) resets once.
        let reset_edge = self
            .previous_reset
            .is_some_and(|previous| previous != reset);
        self.previous_reset = Some(reset);
        // A placement reaches one frame. Its target stands while the
        // transport stays on the frame that took it.
        let target = self.played_target.filter(|&target| target.is_finite() && target <= transport + 1e-6);
        match target {
            Some(target) => self.placement = Some((target, transport)),
            None => {
                if self.placement.is_some_and(|(_, landed)| landed != transport) {
                    self.placement = None;
                }
            }
        }
        // A seek back is decided by its target, not by where its frame
        // landed: one shorter than a frame may land on the frame it left.
        let restarted = !self.started
            || rate_changed
            || setup_changed
            || reset_edge
            || transport < self.last_transport - BACKWARD_SEEK
            || target.is_some_and(|target| target < self.last_transport - BACKWARD_SEEK);
        // A seek forward keeps the simulation and starts a fresh grid at its
        // target: the skipped span is neither simulated nor dropped, and the
        // boundaries from the target to this frame run next frame, as after a
        // restart, so where the frame landed decides nothing.
        let seek_origin = if restarted { None } else { target.filter(|&target| target > self.last_transport) };
        let mut held = false;
        // Within an epoch the target never moves back past the last frame's.
        // Accepted work ends on a Sim Rate boundary in both modes: equal clock
        // inputs give equal intervals, and the display time, one interval
        // behind the target, always lies inside the accepted pair however the
        // frames jitter. A partial interval stays owed until its boundary.
        let speed = f64::from(speed);
        let transport_first = if restarted { 0 } else { self.transport_done };
        // A restart on the frame a placement landed on starts its grid at the
        // placement's target, which no frame rate moves.
        let origin = if restarted {
            self.placement.map_or(transport, |(target, _)| target)
        } else {
            self.transport_origin
        };
        let transport_reached = if !restarted && seek_origin.is_none() {
            (((transport - self.transport_origin) / self.simulation_interval + 1e-9).floor() as u64)
                .max(self.transport_done)
        } else {
            transport_first
        };
        let live_cap = (!offline).then(|| self.live_cap());
        let cap_from = transport_first.max(self.restart_reached);
        // A paused frame runs nothing, even the boundaries a jump to an
        // earlier target left owed; they run once transport moves on.
        let transport_reached = if !restarted && transport <= self.last_transport { transport_first } else { transport_reached };
        let transport_end = live_cap.map_or(transport_reached, |cap| transport_reached.min(cap_from + cap));
        let accepted_transport = if restarted {
            origin
        } else {
            self.transport_origin + transport_end as f64 * self.simulation_interval
        };
        // A fractional interval is ordinary pacing jitter, not an owed step.
        // Discard only when at least one whole interval exceeds the cap; once
        // overloaded, drop the entire remainder and start a fresh grid now.
        let reanchored = seek_origin.is_some() || (!restarted && transport_end < transport_reached);
        if restarted {
            self.epoch = self.epoch.wrapping_add(1);
            self.started = true;
            self.target_time = 0.0;
            self.ticks_done = 0;
            self.simulation_time = 0.0;
            self.dropped_seconds = 0.0;
            self.speed = speed;
            self.transport_origin = origin;
            self.last_accepted_transport = origin;
            self.transport_done = 0;
            self.restart_reached = self.reached_index(transport).max(0.0) as u64;
            self.speed_history.clear();
            self.anchor_speed(origin, speed as f32);
        } else {
            // This frame's Speed applies from the next tick start; the
            // intervals already begun keep the speed of their own start.
            held = transport <= self.last_transport || speed <= 0.0;
            if self.sampled_at != Some(transport) {
                self.anchor_speed(transport, speed as f32);
            }
            let reached = self.simulation_at_seconds(transport, self.live_load);
            if reached.is_finite() {
                self.target_time = reached.max(self.target_time);
            } else {
                numerical_error = true;
            }
            self.speed = speed;
        }
        self.last_transport = transport;
        let first_sequence = self.ticks_done;
        let start = self.simulation_time;
        let ticks = if self.speed_history.iter().all(|anchor| anchor.speed > 0.0) {
            transport_end - transport_first
        } else {
            (transport_first..transport_end).filter(|&index| {
                let at = |i| self.simulation_at_seconds(self.transport_origin + i as f64 * self.simulation_interval, self.live_load);
                at(index + 1) > at(index)
            }).count() as u64
        };
        self.ticks_done += ticks;
        self.simulation_time = self.simulation_at_seconds(accepted_transport, self.live_load);
        self.transport_done = transport_end;
        self.last_accepted_transport = accepted_transport;
        if !reanchored {
            // Accepting the steps releases the observation ceiling. Retain the
            // ordinary fractional remainder until its fixed boundary arrives.
            self.target_time = self.simulation_at_seconds(transport, self.live_load).max(self.simulation_time);
        }
        let plan = FramePlan {
            start: Seconds(start),
            end: Seconds(self.simulation_time),
            intervals: ticks,
        };
        // The display shows the simulation as it stood one Sim Rate interval of
        // transport ago, through the Speed history: one interval of latency at
        // any Speed, and never past the newest accepted state.
        let display_from = transport - self.simulation_interval;
        let display_time = self.simulation_at_seconds(display_from, self.live_load).clamp(0.0, self.simulation_time);
        let speed_history = self.snapshot_history();
        let frame_transport_origin = self.transport_origin;
        let mut fresh_dropped_seconds = 0.0;
        if reanchored {
            let origin = seek_origin.unwrap_or(transport);
            if seek_origin.is_none() {
                fresh_dropped_seconds = (map_transport(&self.speed_history, transport) - self.simulation_time).max(0.0);
                self.dropped_seconds += fresh_dropped_seconds;
            }
            // Keep accepted history intact. The discarded transport span is a
            // plateau, followed by the current Speed at the fresh anchor.
            let retained = self.speed_history.partition_point(|anchor| anchor.transport < accepted_transport);
            self.speed_history.truncate(retained);
            self.speed_history.push(SpeedAnchor { transport: accepted_transport, simulation: self.simulation_time, speed: 0.0 });
            self.speed_history.push(SpeedAnchor { transport: origin, simulation: self.simulation_time, speed });
            self.transport_origin = origin;
            self.transport_done = 0;
            self.restart_reached = if seek_origin.is_some() { self.reached_index(transport).max(0.0) as u64 } else { 0 };
            self.last_accepted_transport = origin;
            self.target_time = self.simulation_time;
        }
        let retained = self.speed_history
            .partition_point(|anchor| anchor.transport <= accepted_transport.min(display_from))
            .saturating_sub(1);
        self.speed_history.drain(..retained);
        ClockFrame {
            plan,
            first_sequence,
            numerical_error,
            offline,
            simulation_interval: self.simulation_interval,
            transport_origin: frame_transport_origin,
            transport_first,
            transport_intervals: transport_end - transport_first,
            speed_history,
            ticks: ticks as u32,
            epoch: self.epoch,
            restarted,
            reanchored,
            held,
            simulation_time: self.simulation_time,
            target_time: self.target_time,
            display_time,
            dropped_seconds: self.dropped_seconds,
            fresh_dropped_seconds,
            due: (transport_reached - transport_first) as u32,
            live_cap: live_cap.map(|cap| cap as u32),
            transport: PlayedTime::from_played(transport),
        }
    }
}

impl ClockFrame {
    /// Coalesce unread export schedules without one allocation per observation.
    pub fn append(&mut self, next: &Self) -> bool {
        if !self.offline || !next.offline
            || self.epoch != next.epoch
            || self.simulation_interval != next.simulation_interval
            || self.transport_origin != next.transport_origin
            || self.speed_history != next.speed_history
            || self.first_sequence + u64::from(self.ticks) != next.first_sequence
            || self.transport_first + self.transport_intervals != next.transport_first
        {
            return false;
        }
        let Some(ticks) = self.ticks.checked_add(next.ticks) else {
            return false;
        };
        self.ticks = ticks;
        self.transport_intervals += next.transport_intervals;
        self.plan.end = next.plan.end;
        self.plan.intervals += next.plan.intervals;
        self.due = self.due.saturating_add(next.due);
        self.fresh_dropped_seconds += next.fresh_dropped_seconds;
        self.transport = next.transport;
        self.simulation_time = next.simulation_time;
        self.target_time = next.target_time;
        self.display_time = next.display_time;
        true
    }
    pub fn interval(&self, ordinal: u64) -> Option<StepInterval> {
        {
            let endpoint = |index: u64| Seconds(map_transport(&self.speed_history,
                self.transport_origin + index as f64 * self.simulation_interval));
            if self.transport_intervals == u64::from(self.ticks) {
                return (ordinal < u64::from(self.ticks)).then(|| {
                    let index = self.transport_first + ordinal;
                    StepInterval::new(endpoint(index), endpoint(index + 1))
                });
            }
            (self.transport_first..self.transport_first + self.transport_intervals)
                .map(|index| StepInterval::new(endpoint(index), endpoint(index + 1)))
                .filter(|interval| interval.end.0 > interval.start.0)
                .nth(ordinal as usize)
        }
    }
    pub fn duration(&self) -> Seconds {
        self.interval(0).map_or(Seconds::ZERO, |interval| interval.duration())
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    /// These tests drive the clock directly on played time.
    fn p(seconds: f64) -> PlayedTime {
        PlayedTime::from_played(seconds)
    }
    use crate::stepping::{CflPolicy, CflRestrictions, LiveStepSchedule};

    #[test]
    fn source_speed_observations_cannot_rewrite_accepted_intervals() {
        let mut clock = SimulationClock::default();
        clock.advance(p(2.0), TICK, 1.0, 0.0, false, true);
        assert!((clock.observe_speed(p(2.05), 2.0) - 0.05).abs() < 1e-12);
        assert!((clock.observe_speed(p(2.10), 2.0) - 0.15).abs() < 1e-12);
        let accepted = clock.advance(p(2.10), TICK, 2.0, 0.0, false, true);
        let intervals: Vec<_> = (0..u64::from(accepted.ticks))
            .map(|i| accepted.interval(i).unwrap()).collect();
        assert!((accepted.simulation_time - 0.15).abs() < 1e-12);
        assert!((intervals[2].end.0 - 0.05).abs() < 1e-12);
        assert!((intervals[3].duration().0 - 2.0 * TICK).abs() < 1e-12);
        clock.observe_speed(p(2.10), 0.5);
        clock.advance(p(2.2), TICK, 0.5, 0.0, false, true);
        assert_eq!(intervals, (0..u64::from(accepted.ticks))
            .map(|i| accepted.interval(i).unwrap()).collect::<Vec<_>>());
    }

    #[test]
    fn discarded_transport_hits_use_next_boundary_once_in_source_order() {
        use crate::input::{EventQueue, EventStamp};
        use crate::TickStamp;

        let mut clock = SimulationClock::default();
        let epoch = u64::from(clock.advance(p(0.0), TICK, 1.0, 0.0, false, false).epoch);
        let mut hits = EventQueue::new(epoch, Seconds::ZERO, Seconds(TICK), 8).unwrap();
        for (sequence, transport) in [TICK * 0.5, TICK * 1.5, 0.3, 0.6].into_iter().enumerate() {
            let time = Seconds(clock.observe_speed(p(transport), 1.0));
            hits.enqueue(EventStamp { epoch, time, sequence: sequence as u64 }, sequence).unwrap();
        }
        let stalled = clock.advance(p(0.7), TICK, 1.0, 0.0, false, false);
        assert_eq!(stalled.ticks, 2);
        assert!((stalled.dropped_seconds - (0.7 - 2.0 * TICK)).abs() < 1e-12);
        let mut receipts = Vec::new();
        for ordinal in 0..2 {
            hits.begin_interval(TickStamp { epoch, tick: ordinal }, stalled.interval(ordinal).unwrap(),
                |event| receipts.push((event.value, event.applied.tick))).unwrap();
        }
        assert_eq!(receipts, [(0, 0), (1, 1)]);
        let resumed = clock.advance(p(0.7 + TICK), TICK, 1.0, 0.0, false, false);
        assert_eq!(resumed.ticks, 1);
        assert_eq!(resumed.interval(0).unwrap().start, stalled.plan.end);
        hits.begin_interval(TickStamp { epoch, tick: 2 }, resumed.interval(0).unwrap(),
            |event| receipts.push((event.value, event.applied.tick))).unwrap();
        assert_eq!(receipts, [(0, 0), (1, 1), (2, 2), (3, 2)]);
        let next = clock.advance(p(0.7 + 2.0 * TICK), TICK, 1.0, 0.0, false, false);
        hits.begin_interval(TickStamp { epoch, tick: 3 }, next.interval(0).unwrap(),
            |event| receipts.push((event.value, event.applied.tick))).unwrap();
        assert_eq!(receipts.len(), 4);
    }

    fn late_speed_observation_preserves_accepted_time(offline: bool) {
        let mut clock = SimulationClock::default();
        clock.advance(p(2.0), TICK, 1.0, 0.0, false, offline);
        // A partial interval is retained when no overload ceiling was reached.
        let previous = clock.advance(p(2.027), TICK, 1.0, 0.0, false, offline);
        let end = 2.0 + TICK;
        let samples: Vec<_> = (0..=100).map(|i| {
            let x = 2.0 + (end - 2.0) * f64::from(i) / 100.0;
            (x, clock.simulation_at(p(x), None))
        }).collect();
        let observed = clock.simulation_at(p(2.005), None);
        assert_eq!(clock.observe_speed(p(2.005), 3.0), observed);
        for (x, expected) in samples {
            assert_eq!(clock.simulation_at(p(x), None), expected, "past transport {x}");
        }
        assert_eq!(clock.simulation_at(p(end), None), previous.simulation_time);
        let future = end + TICK;
        assert!((clock.simulation_at(p(future), None) - (previous.simulation_time + (future - end) * 3.0)).abs() < 1e-12);
        let next = clock.advance(p(2.2), TICK, 3.0, 0.0, false, offline);
        assert_eq!(next.interval(0).unwrap().start.0, previous.simulation_time);
    }

    #[test]
    fn late_live_speed_observation_preserves_accepted_time() {
        late_speed_observation_preserves_accepted_time(false);
    }

    #[test]
    fn late_offline_speed_observation_preserves_accepted_time() {
        late_speed_observation_preserves_accepted_time(true);
    }

    #[test]
    fn historical_speed_schedule_is_independent_of_export_partition() {
        let run = |fps: u32| {
            let mut clock = SimulationClock::default();
            clock.advance(p(0.0), TICK, 1.0, 0.0, false, true);
            let mut intervals = Vec::new();
            for sample in 1..=240 {
                let transport = sample as f64 / 240.0;
                let speed = if sample < 53 { 1.0 } else if sample < 149 { 2.0 } else { 0.5 };
                clock.observe_speed(p(transport), speed);
                if sample % (240 / fps) == 0 {
                    let frame = clock.advance(p(transport), TICK, speed, 0.0, false, true);
                    intervals.extend((0..u64::from(frame.ticks)).map(|i| frame.interval(i).unwrap()));
                }
            }
            intervals
        };
        let expected = run(60);
        assert_eq!(expected.len(), 60);
        for fps in [1, 20, 24, 30, 120] {
            assert_eq!(run(fps), expected, "export {fps} fps");
        }
    }

    /// Frames at `fps` over one second, driven as the host drives them: the
    /// history replay observes Speed on its 240 Hz grid and at each coming
    /// tick's start, then the frame observes it at its own time and advances.
    /// Returns every accepted interval's endpoints as bits.
    fn smooth_ramp_run(fps: u32, interval: f64, offline: bool, speed: impl Fn(f64) -> f32) -> Vec<(u64, u64)> {
        let mut clock = SimulationClock::default();
        clock.advance(p(0.0), interval, speed(0.0), 0.0, false, offline);
        let mut intervals = Vec::new();
        let mut previous = 0.0;
        let mut samples = Vec::new();
        for frame in 1..=fps {
            let now = f64::from(frame) / f64::from(fps);
            samples.clear();
            clock.tick_starts(p(previous), p(now), |transport| samples.push(transport.seconds()));
            let mut grid = (previous * 240.0).floor() + 1.0;
            while grid / 240.0 < now {
                samples.push(grid / 240.0);
                grid += 1.0;
            }
            samples.sort_by(f64::total_cmp);
            for &at in &samples {
                clock.observe_speed(p(at), speed(at));
            }
            let accepted = clock.advance(p(now), interval, speed(now), 0.0, false, offline);
            intervals.extend((0..u64::from(accepted.ticks)).map(|i| {
                let interval = accepted.interval(i).unwrap();
                (interval.start.0.to_bits(), interval.end.0.to_bits())
            }));
            previous = now;
        }
        intervals
    }

    /// BUG-aatmc (smooth Speed and display rate): a Speed curve that moves
    /// between ticks gives the same tick lengths, bit for bit, at every
    /// display rate, live and exported, and each tick runs at the Speed of
    /// its own start.
    #[test]
    fn smooth_speed_ramp_ticks_the_same_at_30_60_120_144_fps_live_and_export() {
        let ramp = |t: f64| 0.5 + 0.75 * t as f32;
        for rate in crate::SimRate::ALL {
            let reference = smooth_ramp_run(60, rate.interval(), true, ramp);
            assert_eq!(reference.len() as u32, rate.hz(), "{} Hz", rate.hz());
            for (k, &(start, end)) in reference.iter().enumerate() {
                let length = f64::from_bits(end) - f64::from_bits(start);
                let expected = rate.interval() * f64::from(ramp(k as f64 * rate.interval()));
                assert!((length - expected).abs() < 1e-12, "{} Hz tick {k}: {length} against {expected}", rate.hz());
            }
            for fps in [30, 60, 120, 144] {
                for offline in [false, true] {
                    assert_eq!(
                        smooth_ramp_run(fps, rate.interval(), offline, ramp),
                        reference,
                        "{} Hz at {fps} fps, offline {offline}",
                        rate.hz()
                    );
                }
            }
        }
    }

    /// A Speed step seen mid-tick leaves that tick at the old speed and takes
    /// effect from the next tick start: at most one Sim Rate interval late.
    #[test]
    fn speed_step_takes_effect_at_the_next_tick_start() {
        let interval = 1.0 / 30.0;
        let mut clock = SimulationClock::default();
        clock.advance(p(0.0), interval, 1.0, 0.0, false, false);
        let seen = clock.advance(p(0.4 * interval), interval, 2.0, 0.0, false, false);
        assert_eq!(seen.ticks, 0);
        let frame = clock.advance(p(2.0 * interval), interval, 2.0, 0.0, false, false);
        assert_eq!(frame.ticks, 2);
        assert!((frame.interval(0).unwrap().duration().0 - interval).abs() < 1e-12, "the tick the step landed in keeps its speed");
        assert!((frame.interval(1).unwrap().duration().0 - 2.0 * interval).abs() < 1e-12, "the next tick runs at the new speed");
    }

    /// A replay sample at a tick start that a frame also ends on sets that
    /// tick's Speed; the frame's own reading there, taken with its sampling
    /// offset, does not replace it, so the tick runs as it would at a rate
    /// whose frames miss that start.
    #[test]
    fn replay_speed_at_a_frame_tick_start_beats_the_frame_reading() {
        let mut clock = SimulationClock::default();
        clock.advance(p(0.0), TICK, 1.0, 0.0, false, true);
        clock.observe_speed(p(TICK), 2.0);
        clock.advance(p(TICK), TICK, 3.0, 0.0, false, true);
        let frame = clock.advance(p(2.0 * TICK), TICK, 3.0, 0.0, false, true);
        assert!((frame.interval(0).unwrap().duration().0 - 2.0 * TICK).abs() < 1e-15, "the replay's Speed at the shared start");
    }

    /// After a seek, a restart starts its grid at the seek's target, not at
    /// the frame the transport landed on, and owes the boundaries between
    /// them to the next frame without reading as an overload.
    #[test]
    fn restart_on_a_jump_starts_the_grid_at_its_target() {
        for fps in [30.0_f64, 60.0, 144.0] {
            let target: f64 = 0.2137;
            let landed = (target * fps).ceil() / fps;
            let mut clock = SimulationClock::default();
            clock.advance(p(0.9), TICK, 1.0, 0.0, false, false);
            clock.set_played_target(Some(p(target)));
            let restart = clock.advance(p(landed), TICK, 1.0, 0.0, false, false);
            assert!(restart.restarted && restart.ticks == 0 && restart.simulation_time == 0.0);
            clock.set_played_target(None);
            let mut starts = Vec::new();
            clock.tick_starts(p(target - 1e-6), p(landed + 1.0 / 30.0), |transport| starts.push(transport.seconds()));
            assert_eq!(starts[0], target, "{fps} fps: the first tick starts at the target");
            assert!(starts.windows(2).all(|pair| ((pair[1] - pair[0]) - TICK).abs() < 1e-12));
            let next = clock.advance(p(landed + 1.0 / 30.0), TICK, 1.0, 0.0, false, false);
            assert!(!next.reanchored, "{fps} fps: the jump's owed boundaries are not an overload");
            assert_eq!(next.interval(0).unwrap().start.0, 0.0);
            assert_eq!(u64::from(next.ticks), ((landed + 1.0 / 30.0 - target) / TICK + 1e-9).floor() as u64);
        }
    }

    /// Resuming from Speed 0 between tick starts: the tick containing the
    /// resume stays empty, and the first tick to run is the one starting at
    /// the next boundary, which `tick_start` names before the frame runs it.
    #[test]
    fn resume_from_speed_zero_runs_from_the_next_tick_start() {
        let mut clock = SimulationClock::default();
        clock.advance(p(0.0), TICK, 1.0, 0.0, false, false);
        clock.advance(p(2.0 * TICK), TICK, 0.0, 0.0, false, false);
        let held = clock.advance(p(5.0 * TICK), TICK, 0.0, 0.0, false, false);
        assert_eq!((clock.ticks_done(), held.ticks), (2, 0));
        assert_eq!(clock.tick_start(2), None, "Speed 0 runs no tick");
        let resumed = clock.advance(p(5.5 * TICK), TICK, 1.0, 0.0, false, false);
        assert_eq!(resumed.ticks, 0);
        let start = clock.tick_start(2).expect("the next tick is known once Speed is back");
        assert!((start.seconds() - 6.0 * TICK).abs() < 1e-12, "{start:?}");
        let mut starts = Vec::new();
        clock.tick_starts(p(5.5 * TICK), p(7.0 * TICK), |transport| starts.push(clock.tick_at(transport)));
        assert_eq!(starts, [Some(2), Some(3)]);
        let frame = clock.advance(p(7.0 * TICK), TICK, 1.0, 0.0, false, false);
        assert_eq!((frame.first_sequence, frame.ticks), (2, 1));
        assert!((frame.interval(0).unwrap().duration().0 - TICK).abs() < 1e-12);
        assert!((frame.simulation_time - 3.0 * TICK).abs() < 1e-12);
    }

    #[test]
    fn live_clock_covers_every_frame_at_20_24_30_60_fps() {
        for fps in [20, 24, 30, 60] {
            let mut clock = SimulationClock::default();
            let mut completed = 0.0;
            for index in 0..=fps {
                let time = f64::from(index) / f64::from(fps);
                let frame = clock.advance(p(time), 1.0 / f64::from(fps), 1.0, 0.0, false, false);
                assert_eq!(frame.plan.start.0, completed);
                for ordinal in 0..u64::from(frame.ticks) {
                    let interval = frame.interval(ordinal).unwrap();
                    let mut schedule =
                        LiveStepSchedule::new(interval.start, interval.duration(), 1, 6).value;
                    let mut cap = false;
                    while let Some(step) = schedule.next(Seconds(1e-6)).value {
                        assert_eq!(step.interval.start.0, completed);
                        completed = step.interval.end.0;
                        cap |= step.hit_cap;
                    }
                    assert!(cap, "tiny CFL must hit the cap");
                    assert_eq!(schedule.steps_taken(), 6);
                }
                assert!((completed - time).abs() < 1e-12);
                assert_eq!(frame.dropped_seconds, 0.0);
            }
            assert!((completed - 1.0).abs() < 1e-12);
        }
    }

    /// Below the live ceiling, partial intervals stay owed without stretching
    /// an accepted interval. Above it, progress slows and the rest is dropped.
    #[test]
    fn live_frames_between_boundaries_owe_the_partial_interval() {
        for rate in crate::SimRate::ALL {
            for fps in [20u32, 24, 30, 60] {
                let mut clock = SimulationClock::default();
                clock.advance(p(0.0), rate.interval(), 1.0, 0.0, false, false);
                let mut completed = 0.0;
                for frame in 1..=fps {
                    let transport = f64::from(frame) / f64::from(fps);
                    let accepted = clock.advance(p(transport), rate.interval(), 1.0, 0.0, false, false);
                    let at = format!("{} Hz, {fps} fps frame {frame}", rate.hz());
                    assert_eq!(accepted.plan.start.0, completed, "{at}: a gap before the span");
                    assert!(accepted.ticks <= MAX_LIVE_INTERVALS as u32, "{at}");
                    for ordinal in 0..u64::from(accepted.ticks) {
                        assert!((accepted.interval(ordinal).unwrap().duration().0 - rate.interval()).abs() < 1e-12, "{at}: stretched step");
                    }
                    assert!((transport - accepted.dropped_seconds - accepted.target_time).abs() < 1e-12, "{at}: unaccounted time");
                    assert!(accepted.target_time - accepted.simulation_time < rate.interval() + 1e-12, "{at}: catch-up debt");
                    completed = accepted.plan.end.0;
                }
                let expected = (f64::from(fps) * 2.0 * rate.interval()).min(1.0);
                assert!((completed - expected).abs() < 1e-12, "{} Hz, {fps} fps: unexpected progression", rate.hz());
            }
        }
    }

    #[test]
    fn late_live_span_reports_one_interval_of_simulated_time() {
        // Speed scales each fixed transport interval, never the overload span.
        let interval = 1.0 / 30.0;
        for speed in [0.25f32, 1.0, 2.0] {
            let mut clock = SimulationClock::default();
            clock.advance(p(0.0), interval, speed, 0.0, false, false);
            let frame = clock.advance(p(0.4), interval, speed, 0.0, false, false);
            let expected = interval * f64::from(speed);
            assert_eq!(frame.ticks, 2);
            assert!((frame.duration().0 - expected).abs() < 1e-12, "speed {speed}: fixed step");
            assert!((frame.simulation_time - 2.0 * expected).abs() < 1e-12);
        }
    }

    #[test]
    fn export_matches_live_simulation_intervals_and_cfl_steps() {
        for project_fps in [15, 20, 30, 60] {
            let simulation_interval = 1.0 / f64::from(project_fps);
            for speed in [0.5, 1.0, 2.0] {
                let run = |fps, offline| {
                    let mut clock = SimulationClock::default();
                    let mut intervals = Vec::new();
                    let mut steps = Vec::new();
                    for index in 0..=fps {
                        let frame = clock.advance(
                            p(f64::from(index) * (1.0 / f64::from(fps))),
                            simulation_interval,
                            speed,
                            0.0,
                            false,
                            offline,
                        );
                        for ordinal in 0..u64::from(frame.ticks) {
                            let interval = frame.interval(ordinal).unwrap();
                            intervals.push(interval);
                            let mut schedule =
                                LiveStepSchedule::new(interval.start, interval.duration(), 2, 6)
                                    .value;
                            while let Some(step) = schedule
                                .next(
                                    CflPolicy::default()
                                        .duration(
                                            interval.duration(),
                                            if steps.len() % 2 == 0 { 1e7 } else { 1e3 },
                                            CflRestrictions::default(),
                                        )
                                        .value,
                                )
                                .value
                            {
                                steps.push(step);
                            }
                            assert!(schedule.steps_taken() <= 6);
                        }
                    }
                    (intervals, steps)
                };
                let live = run(60, false);
                for export_fps in [20, 24, 30, 60] {
                    let export = run(export_fps, true);
                    assert_eq!(
                        live, export,
                        "project {project_fps}, export {export_fps}, speed {speed}"
                    );
                }
            }
        }
    }

    #[test]
    fn live_clock_invalid_observation_reports_and_retains_transport_span() {
        let mut clock = SimulationClock::default();
        clock.advance(p(0.0), TICK, 1.0, 0.0, false, false);
        let invalid = clock.advance(p(f64::NAN), TICK, 1.0, 0.0, false, false);
        assert!(invalid.numerical_error);
        assert_eq!(invalid.ticks, 0);
        let resumed = clock.advance(p(0.5), TICK, 1.0, 0.0, false, false);
        assert_eq!(resumed.plan.start, Seconds::ZERO);
        assert_eq!(resumed.plan.end, Seconds(2.0 * TICK));
        assert!((resumed.dropped_seconds - (0.5 - 2.0 * TICK)).abs() < 1e-12);
    }
    #[test]
    fn sim_rate_edit_starts_exactly_one_epoch() {
        let mut clock = SimulationClock::default();
        clock.advance(p(0.0), crate::SimRate::Hz30.interval(), 1.0, 0.0, false, false);
        clock.advance(p(1.0), crate::SimRate::Hz30.interval(), 1.0, 0.0, false, false);
        let edited = clock.advance(p(1.0), crate::SimRate::Hz20.interval(), 1.0, 0.0, false, false);
        assert!(edited.restarted);
        assert_eq!(edited.epoch, 2);
        assert_eq!(edited.ticks, 0);
        for index in 1..=60 {
            let next = clock.advance(p(1.0 + f64::from(index) / 60.0), crate::SimRate::Hz20.interval(), 1.0, 0.0, false, false);
            assert!(!next.restarted);
            assert_eq!(next.epoch, edited.epoch);
        }
    }

    #[test]
    fn sim_rate_intervals_and_authored_trigger_samples() {
        for rate in crate::SimRate::ALL {
            let mut clock = SimulationClock::default();
            clock.advance(p(0.0), rate.interval(), 2.0, 0.0, false, false);
            let mut intervals = 0;
            let mut samples = Vec::new();
            let mut cfl_steps = 0;
            for display in 1..=60 {
                let now = f64::from(display) / 60.0;
                clock.tick_starts(p(f64::from(display - 1) / 60.0), p(now), |time| samples.push((time.seconds(), clock.tick_at(time).unwrap())));
                let frame = clock.advance(p(now), rate.interval(), 2.0, 0.0, false, false);
                intervals += frame.ticks;
                for ordinal in 0..u64::from(frame.ticks) {
                    let interval = frame.interval(ordinal).unwrap();
                    assert!((interval.duration().0 - 2.0 * rate.interval()).abs() < 1e-12);
                    let mut schedule = LiveStepSchedule::new(interval.start, interval.duration(), 1, 6).value;
                    while schedule.next(Seconds(1e-9)).value.is_some() { cfl_steps += 1; }
                }
            }
            assert_eq!(intervals, rate.hz());
            assert_eq!(cfl_steps, rate.hz() * 6);
            assert_eq!(samples.len(), rate.hz() as usize);
            for (index, &(time, tick)) in samples.iter().enumerate() {
                assert!((time - (index + 1) as f64 * rate.interval()).abs() < 1e-12);
                assert_eq!(tick, (index + 1) as u64);
            }
            assert_eq!(clock.accepted_time(), 2.0);
        }
    }

    #[test]
    fn sim_rate_audio_trigger_waits_for_next_update() {
        use crate::input::{EventQueue, EventStamp};
        for rate in crate::SimRate::ALL {
            let mut clock = SimulationClock::default();
            let first = clock.advance(p(0.0), rate.interval(), 1.0, 0.0, false, false);
            let mut events = EventQueue::new(u64::from(first.epoch), Seconds::ZERO, Seconds(rate.interval()), 4).unwrap();
            let hit_time = rate.interval() * 0.25;
            events.enqueue(EventStamp { epoch: u64::from(first.epoch), time: Seconds(hit_time), sequence: 0 }, "kick").unwrap();
            let waiting = clock.advance(p(rate.interval() * 0.5), rate.interval(), 1.0, 0.0, false, false);
            assert_eq!(waiting.ticks, 0);
            let ready = clock.advance(p(rate.interval()), rate.interval(), 1.0, 0.0, false, false);
            let mut hits = Vec::new();
            events.begin_interval(events.next_tick(), ready.interval(0).unwrap(), |hit| hits.push(hit)).unwrap();
            assert_eq!(hits.len(), 1);
            assert_eq!(hits[0].value, "kick");
            assert_eq!(hits[0].source.time, Seconds(hit_time));
            assert!(ready.simulation_time - hit_time < rate.interval());
            let next = clock.advance(p(2.0 * rate.interval()), rate.interval(), 1.0, 0.0, false, false);
            events.begin_interval(events.next_tick(), next.interval(0).unwrap(), |_| panic!("hit repeated")).unwrap();
        }
    }

    #[test]
    fn sim_rate_display_target_advances_between_updates() {
        let mut clock = SimulationClock::default();
        clock.advance(p(0.0), crate::SimRate::Hz30.interval(), 1.0, 0.0, false, false);
        let a = clock.advance(p(2.0 * TICK), crate::SimRate::Hz30.interval(), 1.0, 0.0, false, false);
        let b = clock.advance(p(3.0 * TICK), crate::SimRate::Hz30.interval(), 1.0, 0.0, false, false);
        assert_eq!(b.ticks, 0);
        assert_eq!(a.simulation_time, b.simulation_time);
        assert!((b.display_time - a.display_time - TICK).abs() < 1e-12);
    }

    #[test]
    fn sim_rate_late_live_update_caps_steps_and_reanchors_without_debt() {
        for rate in crate::SimRate::ALL {
            let mut clock = SimulationClock::default();
            clock.advance(p(0.0), rate.interval(), 1.0, 0.0, false, false);
            let late = clock.advance(p(1.07), rate.interval(), 1.0, 0.0, false, false);
            assert_eq!(late.ticks, 2);
            assert!(late.reanchored);
            assert!((late.simulation_time - 2.0 * rate.interval()).abs() < 1e-12, "{} Hz", rate.hz());
            assert!((late.dropped_seconds - (1.07 - 2.0 * rate.interval())).abs() < 1e-12);
            let interval = late.interval(0).unwrap();
            let mut schedule = LiveStepSchedule::new(interval.start, interval.duration(), 1, 6).value;
            let mut end = Seconds::ZERO;
            while let Some(step) = schedule.next(Seconds(1e-9)).value { end = step.interval.end; }
            assert_eq!(schedule.steps_taken(), 6);
            assert_eq!(end, interval.end);
            let resumed = clock.advance(p(1.07 + rate.interval()), rate.interval(), 1.0, 0.0, false, false);
            assert_eq!(resumed.ticks, 1);
            assert!(!resumed.reanchored);
            assert_eq!(resumed.dropped_seconds, late.dropped_seconds);
            assert_eq!(resumed.plan.start, late.plan.end);
        }
    }

    /// A late frame takes one interval, so an overloaded show slows down
    /// instead of halving its frame rate; an on-time frame may catch up with
    /// two. Observers agree with `advance`, and export ignores the load.
    #[test]
    fn late_frame_load_takes_one_interval_and_on_time_load_two() {
        let budget = Seconds(1.0 / 60.0);
        let late = LiveLoad { previous: Seconds(0.025), budget };
        let on_time = LiveLoad { previous: Seconds(0.012), budget };
        let mut clock = SimulationClock::default();
        clock.advance(p(0.0), TICK, 1.0, 0.0, false, false);
        {
            clock.set_live_load(Some(late));
            let mut starts = Vec::new();
            clock.tick_starts(p(-1.0), p(2.5 * TICK), |transport| starts.push(clock.tick_at(transport).unwrap()));
            assert_eq!(starts, [0, 1]);
            assert_eq!(clock.simulation_at(p(2.5 * TICK), Some(late)), TICK);
            let frame = clock.advance(p(2.5 * TICK), TICK, 1.0, 0.0, false, false);
            assert_eq!(frame.ticks, 1);
            assert!(frame.reanchored);
        }
        {
            clock.set_live_load(Some(on_time));
            let frame = clock.advance(p(5.0 * TICK), TICK, 1.0, 0.0, false, false);
            assert_eq!(frame.ticks, 2);
            assert!(!frame.reanchored);
        }

        let mut export = SimulationClock::default();
        export.advance(p(0.0), TICK, 1.0, 0.0, false, true);
        export.set_live_load(Some(late));
        assert_eq!(export.advance(p(5.5 * TICK), TICK, 1.0, 0.0, false, true).ticks, 5);
    }

    /// The frame-time counters are the clock's own decisions: accepted ticks
    /// are the due boundaries clipped to the cap in force, a reanchor happens
    /// exactly when due exceeds the cap, and the fresh drop is the step in the
    /// cumulative total. A frame that crosses no boundary ticks nothing.
    #[test]
    fn a_clone_reports_under_its_own_instance() {
        let interval = 1.0 / 30.0;
        let unstarted = SimulationClock::default();
        let mut early = unstarted.clone();
        early.advance(p(0.0), interval, 1.0, 0.0, false, false);
        let mut original = SimulationClock::default();
        original.advance(p(0.0), interval, 1.0, 0.0, false, false);
        let mut late = original.clone();
        assert_eq!(late.instance(), 0, "a clone starts unassigned");
        late.advance(p(interval), interval, 1.0, 0.0, false, false);
        let ids = [early.instance(), original.instance(), late.instance()];
        assert!(ids.iter().all(|&id| id != 0));
        assert!(ids[0] != ids[1] && ids[1] != ids[2] && ids[0] != ids[2]);
    }

    #[test]
    fn decision_counters_match_the_clock_over_an_overload_sequence() {
        let interval = 1.0 / 30.0;
        let budget = Seconds(1.0 / 60.0);
        let late = Some(LiveLoad { previous: Seconds(0.05), budget });
        let on_time = Some(LiveLoad { previous: Seconds(0.01), budget });
        let mut clock = SimulationClock::default();
        let first = clock.advance(p(0.0), interval, 1.0, 0.0, false, false);
        assert!(first.restarted);
        assert_eq!((first.due, first.ticks), (0, 0));
        // (transport offset from the last frame, load, due, cap, reanchored)
        let sequence = [
            (1.0 * interval + 1e-4, on_time, 1, 2, false),
            (0.5 * interval, on_time, 0, 2, false),
            (5.2 * interval, on_time, 5, 2, true),
            (3.1 * interval, late, 3, 1, true),
            (0.4 * interval, late, 0, 1, false),
            (0.7 * interval, on_time, 1, 2, false),
            (2.0 * interval, late, 2, 1, true),
        ];
        let mut transport = 0.0;
        let mut previous_dropped = 0.0;
        for (step, (offset, load, due, cap, reanchored)) in sequence.into_iter().enumerate() {
            transport += offset;
            clock.set_live_load(load);
            let frame = clock.advance(p(transport), interval, 1.0, 0.0, false, false);
            let at = format!("frame {step}");
            assert_eq!(frame.due, due, "{at}");
            assert_eq!(frame.live_cap, Some(cap), "{at}");
            assert_eq!(frame.ticks, due.min(cap), "{at}");
            assert_eq!(frame.reanchored, reanchored, "{at}");
            assert_eq!(frame.reanchored, frame.due > cap, "{at}");
            assert_eq!(frame.epoch, first.epoch, "{at}");
            assert_eq!(frame.transport, p(transport), "{at}");
            assert_eq!(frame.first_sequence + u64::from(frame.ticks), clock.ticks_done(), "{at}");
            assert!((frame.fresh_dropped_seconds - (frame.dropped_seconds - previous_dropped)).abs() < 1e-12, "{at}");
            assert_eq!(frame.fresh_dropped_seconds > 0.0, reanchored, "{at}");
            previous_dropped = frame.dropped_seconds;
        }
        let export = SimulationClock::default().advance(p(0.0), interval, 1.0, 0.0, false, true);
        assert_eq!(export.live_cap, None);
    }

    /// Display frames wobble around their nominal times and Speed changes
    /// mid-show. Live still lands on the export boundaries, and the display
    /// time never leaves the accepted pair, so the blend never holds a frame.
    #[test]
    fn sim_rate_jittered_live_frames_match_export_and_stay_inside_the_pair() {
        const JITTER: [f64; 5] = [0.0009, -0.0007, 0.0003, -0.0009, 0.0006];
        for rate in crate::SimRate::ALL {
            for speeds in [[1.0f32; 4], [2.0; 4], [1.0, 0.0, 0.5, 2.0]] {
                let speed_at = |display: u32| speeds[(display as usize / 7) % speeds.len()];
                let mut live = SimulationClock::default();
                let mut export = SimulationClock::default();
                live.advance(p(0.0), rate.interval(), speeds[0], 0.0, false, false);
                export.advance(p(0.0), rate.interval(), speeds[0], 0.0, false, true);
                let (mut previous, mut current) = (0.0, 0.0);
                let mut last_display = 0.0;
                let mut export_ends = Vec::new();
                for display in 1..=120u32 {
                    let transport = f64::from(display) / 60.0 + JITTER[display as usize % JITTER.len()];
                    let speed = speed_at(display);
                    let frame = live.advance(p(transport), rate.interval(), speed, 0.0, false, false);
                    assert!(frame.display_time >= last_display - 1e-12, "display went back");
                    last_display = frame.display_time;
                    if frame.ticks > 0 {
                        previous = current;
                        current = frame.simulation_time;
                    }
                    assert!(
                        frame.display_time >= previous - 1e-12 && frame.display_time <= current + 1e-12,
                        "{} Hz speed {speed} frame {display}: display {} outside [{previous}, {current}]",
                        rate.hz(),
                        frame.display_time,
                    );
                    let offline = export.advance(p(transport), rate.interval(), speed, 0.0, false, true);
                    for ordinal in 0..u64::from(offline.ticks) {
                        export_ends.push(offline.interval(ordinal).unwrap().end.0);
                    }
                    if frame.ticks > 0 {
                        assert!(
                            export_ends.last().is_some_and(|end| (end - current).abs() < 1e-12),
                            "{} Hz frame {display}: live ended off the export boundaries",
                            rate.hz(),
                        );
                    }
                }
                assert_eq!(live.accepted_time(), export.accepted_time());
            }
        }
    }
}
