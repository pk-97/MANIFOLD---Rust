//! MLS-MPM matter (`docs/GPU_MPM_SOLVER_DESIGN.md`): the point and grid
//! records the matter atoms share, the water constants, the lattice, the
//! fixed-point encoding of grid accumulation (D5), the substep rule (D4) and
//! the fixed-tick clock (D8).

use crate::node_graph::channel_names::well_known;
use crate::node_graph::fluid::{FluidDomainLayout, TICK};
use crate::node_graph::ports::{ChannelElementType, ChannelSpec, KnownItem};
use crate::node_graph::transform::Transform;

/// The f64 CPU oracle, compiled for unit tests and the `gpu-proofs` binary.
#[cfg(any(test, feature = "gpu-proofs"))]
#[doc(hidden)]
pub mod reference;

/// One material point. 80 bytes. Storage order is id order and never changes
/// (D9): the published frame must be id-sorted.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct MatterPoint {
    /// Scene metres.
    pub position: [f32; 3],
    /// Birth ordinal within the identity epoch; 0 marks an unused slot.
    pub id: u32,
    /// Metres per second.
    pub velocity: [f32; 3],
    /// J, current over rest volume.
    pub volume_ratio: f32,
    /// Affine velocity C, row 0 (1/s); w = plastic volume ratio (1 when unused).
    pub affine_x: [f32; 4],
    /// C row 1; w = rest volume V0 in cubic metres.
    pub affine_y: [f32; 4],
    /// C row 2; w = 0.
    pub affine_z: [f32; 4],
}

/// Std430: position Vec3F at 0, id at 12, velocity at 16, volume_ratio at 28,
/// the three affine rows at 32, 48, 64. Stride 80.
pub const MATTER_POINT_SPECS: &[ChannelSpec] = &[
    ChannelSpec { name: well_known::POSITION, ty: ChannelElementType::Vec3F },
    ChannelSpec { name: well_known::ID, ty: ChannelElementType::U32 },
    ChannelSpec { name: well_known::VELOCITY, ty: ChannelElementType::Vec3F },
    ChannelSpec { name: well_known::VOLUME_RATIO, ty: ChannelElementType::F32 },
    ChannelSpec { name: well_known::AFFINE_X, ty: ChannelElementType::Vec4F },
    ChannelSpec { name: well_known::AFFINE_Y, ty: ChannelElementType::Vec4F },
    ChannelSpec { name: well_known::AFFINE_Z, ty: ChannelElementType::Vec4F },
];

impl KnownItem for MatterPoint {
    const SPECS: &'static [ChannelSpec] = MATTER_POINT_SPECS;
}

/// One resolved grid node. 32 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct MatterGridNode {
    /// Velocity after forces and boundaries; w = mass in kg (0 = empty).
    pub velocity_mass: [f32; 4],
    /// Velocity before forces (momentum / mass), for Liveliness; w = 1 when the
    /// resolve clamped this node's velocity.
    pub velocity_before: [f32; 4],
}

pub const MATTER_GRID_NODE_SPECS: &[ChannelSpec] = &[
    ChannelSpec { name: well_known::VELOCITY_MASS, ty: ChannelElementType::Vec4F },
    ChannelSpec { name: well_known::VELOCITY_BEFORE, ty: ChannelElementType::Vec4F },
];

impl KnownItem for MatterGridNode {
    const SPECS: &'static [ChannelSpec] = MATTER_GRID_NODE_SPECS;
}

const _: () = {
    assert!(std::mem::size_of::<MatterPoint>() == 80);
    assert!(std::mem::size_of::<MatterGridNode>() == 32);
};

/// Accumulator words per grid node: momentum x, y, z, then mass.
pub const ACCUM_WORDS_PER_NODE: u32 = 4;

/// Signed fixed-point scale of the accumulators (D5; the prototype's measured
/// choice, 0–0.0125% mass error where 2^12 gave 2.4–4.8%).
pub const FIXED_POINT_SCALE: f32 = 1_048_576.0;

/// Rest density of water, kg/m³ (taichi_elements `p_rho`).
pub const WATER_DENSITY: f32 = 1000.0;

/// Mass unit of the accumulators: `1000 · dx³ / 8` kg (D5), so a full node of
/// water sums to about 8 units.
pub fn mass_unit(cell_size: f32) -> f32 {
    125.0 * cell_size * cell_size * cell_size
}

/// Young's modulus per metre of the longest domain side and Poisson's ratio
/// (taichi_elements: `E = 1e6 · size`, `nu = 0.2`).
const YOUNG_PER_METRE: f64 = 1.0e6;
const POISSON_RATIO: f64 = 0.2;

/// Water's bulk term λ in Pa at `stiffness` (D3: λ scales by s²).
pub fn water_lambda(longest_side_m: f64, stiffness: f64) -> f64 {
    let e = YOUNG_PER_METRE * longest_side_m;
    let lambda0 = e * POISSON_RATIO / ((1.0 + POISSON_RATIO) * (1.0 - 2.0 * POISSON_RATIO));
    lambda0 * stiffness * stiffness
}

/// Acoustic wave speed `√(λ/ρ)` in m/s.
pub fn wave_speed(lambda: f64, density: f64) -> f64 {
    (lambda / density).sqrt()
}

/// Acoustic CFL (taichi_elements' default dt at L = 1 gives c·dt/dx = 0.33).
pub const ACOUSTIC_CFL: f64 = 1.0 / 3.0;
/// The substep cap (D4).
pub const MAX_SUBSTEPS: u32 = 128;
/// Grid velocity clamp per component, in cells per substep
/// (taichi_elements `g2p2g_allowed_cfl`).
pub const VELOCITY_CLAMP_CFL: f32 = 0.9;
/// Gravity used for the free-fall speed estimate. Live gravity is not used:
/// changing it must not change the substep count (D4).
const ESTIMATE_GRAVITY: f64 = 9.81;

/// Free-fall speed from rest over `height` metres, the D4 `v_est` floor.
pub fn free_fall_speed(height_m: f64) -> f64 {
    (2.0 * ESTIMATE_GRAVITY * height_m).sqrt()
}

/// Substeps per 1/60 s tick (D4): the acoustic CFL limit plus optional
/// body-coupling and viscous limits (seconds). Uncapped; the caller fits the
/// dials to [`MAX_SUBSTEPS`] with [`stiffness_fitting_cap`].
pub fn substeps_per_tick(
    dx: f32,
    wave_speed: f32,
    v_est: f32,
    body_limit: Option<f32>,
    viscous_limit: Option<f32>,
) -> u32 {
    let acoustic = ACOUSTIC_CFL * f64::from(dx) / (f64::from(wave_speed) + f64::from(v_est));
    let dt = [body_limit, viscous_limit]
        .into_iter()
        .flatten()
        .map(f64::from)
        .fold(acoustic, f64::min);
    ((TICK / dt) - 1e-9).ceil().max(1.0) as u32
}

/// The largest Stiffness whose substep count fits [`MAX_SUBSTEPS`] (D4:
/// limited, never silently). `unit_wave_speed` is the wave speed at
/// Stiffness 1.
pub fn stiffness_fitting_cap(dx: f64, unit_wave_speed: f64, v_est: f64) -> f64 {
    let max_wave = ACOUSTIC_CFL * dx * f64::from(MAX_SUBSTEPS) / TICK - v_est;
    // A hair inside the cap, so f32 rounding of the wave speed downstream
    // cannot tip the count to one past it.
    (max_wave / unit_wave_speed * (1.0 - 1e-5)).max(0.0)
}

/// Nodes added outside the authored box on every side (taichi `padding = 3`).
pub const PADDING_NODES: u32 = 3;

/// The matter lattice: the domain layout's box grown by [`PADDING_NODES`] per
/// side (D5), node (i, j, k) at `min + (i, j, k) · cell_size`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MatterLattice {
    pub min: [f32; 3],
    pub nodes: [u32; 3],
    pub cell_size: f32,
    /// Cells of the authored box per axis.
    pub cells: [u32; 3],
}

impl MatterLattice {
    pub fn from_layout(layout: &FluidDomainLayout) -> Self {
        let dx = layout.cell_size as f32;
        let pad = PADDING_NODES as f32 * dx;
        Self {
            min: layout.min.map(|v| v - pad),
            nodes: layout.cells.map(|n| n + 1 + 2 * PADDING_NODES),
            cell_size: dx,
            cells: layout.cells,
        }
    }

    pub fn node_count(&self) -> u32 {
        self.nodes[0] * self.nodes[1] * self.nodes[2]
    }

    /// Scene AABB of the lattice nodes (the seam's `grid_bounds`).
    pub fn bounds(&self) -> Transform {
        let size: [f32; 3] =
            std::array::from_fn(|i| (self.nodes[i] - 1) as f32 * self.cell_size);
        Transform {
            pos: std::array::from_fn(|i| self.min[i] + size[i] * 0.5),
            scale: size,
            ..Transform::default()
        }
    }
}

/// One frame of the D8 clock: how many fixed ticks to run and where the
/// display sits.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClockFrame {
    pub ticks: u32,
    pub epoch: u32,
    /// This frame starts a new simulation (first frame, reset, setup change or
    /// backward seek); the state reseeds before any tick runs.
    pub restarted: bool,
    /// Simulated seconds at the end of this frame's ticks.
    pub simulation_time: f64,
    /// Display time `s = target − tick` (surface design D10).
    pub display_time: f64,
    /// Simulated time dropped under live overload since the epoch began.
    pub dropped_seconds: f64,
}

/// Most live ticks one display frame may run: 1 at 60 fps, 2 at 30, 3 at 24.
/// A slow frame never earns more than this, so live cannot spiral (D8).
pub const MAX_LIVE_TICKS: u32 = 3;

/// The fixed 60 Hz clock the domain node owns (D8). Transport and Speed build
/// a target time; live runs at most the frame's share of ticks and drops the
/// rest (reported), keeping at most one tick of jitter debt; offline runs
/// every due tick.
#[derive(Clone, Debug, Default)]
pub struct MatterClock {
    epoch: u32,
    started: bool,
    last_transport: f64,
    previous_reset: Option<f32>,
    target_time: f64,
    ticks_done: u64,
    dropped_seconds: f64,
}

impl MatterClock {
    /// Advance by one display frame. `frame_interval` is this frame's host
    /// delta in seconds; it only sets the live tick allowance.
    pub fn advance(
        &mut self,
        transport: f64,
        frame_interval: f64,
        speed: f32,
        reset: f32,
        setup_changed: bool,
        offline: bool,
    ) -> ClockFrame {
        // A trigger publishes a counter; any change (undo included) resets once.
        let reset_edge = self.previous_reset.is_some_and(|previous| previous != reset);
        self.previous_reset = Some(reset);
        let restarted = !self.started
            || setup_changed
            || reset_edge
            || transport < self.last_transport - 1e-9;
        if restarted {
            if self.started {
                self.epoch = self.epoch.wrapping_add(1);
            }
            self.started = true;
            self.target_time = 0.0;
            self.ticks_done = 0;
            self.dropped_seconds = 0.0;
        } else {
            self.target_time += (transport - self.last_transport).max(0.0) * f64::from(speed);
        }
        self.last_transport = transport;
        let due = ((self.target_time / TICK + 1e-9).floor() as u64).saturating_sub(self.ticks_done);
        let ticks = if offline {
            due
        } else {
            let allowance = ((frame_interval / TICK) - 1e-6)
                .ceil()
                .clamp(1.0, f64::from(MAX_LIVE_TICKS)) as u64;
            let run = due.min(allowance);
            // Keep one tick of scheduling jitter; drop the rest visibly.
            let dropped = due.saturating_sub(run).saturating_sub(1);
            if dropped > 0 {
                let seconds = dropped as f64 * TICK;
                self.target_time -= seconds;
                self.dropped_seconds += seconds;
            }
            run
        };
        self.ticks_done += ticks;
        ClockFrame {
            ticks: ticks as u32,
            epoch: self.epoch,
            restarted,
            simulation_time: self.ticks_done as f64 * TICK,
            display_time: (self.target_time - TICK).max(0.0),
            dropped_seconds: self.dropped_seconds,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::ports::std430_stride;

    #[test]
    fn matter_records_match_their_channel_layouts() {
        assert_eq!(std430_stride(MATTER_POINT_SPECS), 80);
        assert_eq!(std430_stride(MATTER_GRID_NODE_SPECS), 32);
    }

    /// D4's worked numbers at the Dam Break setup: L = H = 4 m, 64 cells,
    /// Stiffness 1 → λ = 1.11e6 Pa, c = 33.3 m/s, v_est = 8.9 m/s, n = 34.
    #[test]
    fn matter_substep_rule_matches_worked_example() {
        let dx = 4.0 / 64.0;
        let lambda = water_lambda(4.0, 1.0);
        assert!((lambda - 1.111e6).abs() < 1.0e3, "{lambda}");
        let c = wave_speed(lambda, 1000.0);
        assert!((c - 33.33).abs() < 0.01, "{c}");
        let v = free_fall_speed(4.0);
        assert!((v - 8.86).abs() < 0.01, "{v}");
        assert_eq!(substeps_per_tick(dx, c as f32, v as f32, None, None), 34);
        let dt = TICK / 34.0;
        assert!((dt - 4.90e-4).abs() < 1.0e-6, "{dt}");
    }

    #[test]
    fn matter_substeps_follow_stiffness() {
        let dx = 4.0f32 / 64.0;
        let v = free_fall_speed(4.0) as f32;
        let n = |s: f64| substeps_per_tick(dx, wave_speed(water_lambda(4.0, s), 1000.0) as f32, v, None, None);
        assert_eq!(n(0.5), 21);
        assert_eq!(n(1.0), 34);
        assert_eq!(n(2.0), 61);
    }

    #[test]
    fn matter_dials_limited_to_substep_cap() {
        // Resolution 256 in a 4 m domain: Stiffness 3 needs more than 128.
        let dx = 4.0f64 / 256.0;
        let v = free_fall_speed(4.0);
        let unit = wave_speed(water_lambda(4.0, 1.0), 1000.0);
        let requested = 3.0;
        let n_requested =
            substeps_per_tick(dx as f32, (unit * requested) as f32, v as f32, None, None);
        assert!(n_requested > MAX_SUBSTEPS, "{n_requested}");
        let fitted = stiffness_fitting_cap(dx, unit, v);
        assert!(fitted < requested);
        let n_fitted = substeps_per_tick(dx as f32, (unit * fitted) as f32, v as f32, None, None);
        assert!(n_fitted <= MAX_SUBSTEPS, "{n_fitted}");
        // The largest fitting value: slightly stiffer needs one more substep.
        let n_above =
            substeps_per_tick(dx as f32, (unit * fitted * 1.001) as f32, v as f32, None, None);
        assert!(n_above > MAX_SUBSTEPS, "{n_above}");
    }

    #[test]
    fn matter_lattice_pads_the_authored_box() {
        let layout = crate::node_graph::fluid::domain_layout(None, 4.0, 64).unwrap();
        let lattice = MatterLattice::from_layout(&layout);
        assert_eq!(lattice.nodes, [71; 3]);
        assert_eq!(lattice.cell_size, 0.0625);
        assert_eq!(lattice.min, [-2.1875, -0.1875, -2.1875]);
        let bounds = lattice.bounds();
        assert_eq!(bounds.scale, [4.375; 3]);
    }

    fn run(clock: &mut MatterClock, frames: &[(f64, f64)], offline: bool) -> Vec<ClockFrame> {
        frames
            .iter()
            .map(|&(t, dt)| clock.advance(t, dt, 1.0, 0.0, false, offline))
            .collect()
    }

    #[test]
    fn matter_live_caps_ticks_per_frame() {
        let mut clock = MatterClock::default();
        // 60 fps: one tick per frame after the first.
        let frames: Vec<(f64, f64)> = (0..=10).map(|i| (i as f64 * TICK, TICK)).collect();
        let out = run(&mut clock, &frames, false);
        assert!(out[0].restarted);
        assert!(out[1..].iter().all(|f| f.ticks == 1 && !f.restarted));
        // A one-second stall: the frame runs its allowance, keeps one tick of
        // debt and drops the rest, reported.
        let stalled = clock.advance(10.0 * TICK + 1.0, 1.0, 1.0, 0.0, false, false);
        assert_eq!(stalled.ticks, MAX_LIVE_TICKS);
        let owed = 60u32;
        let dropped_ticks = owed - MAX_LIVE_TICKS - 1;
        assert!((stalled.dropped_seconds - f64::from(dropped_ticks) * TICK).abs() < 1e-9);
        // 30 fps runs two ticks per frame.
        let mut clock = MatterClock::default();
        let frames: Vec<(f64, f64)> = (0..=6).map(|i| (i as f64 * 2.0 * TICK, 2.0 * TICK)).collect();
        let out = run(&mut clock, &frames, false);
        assert!(out[1..].iter().all(|f| f.ticks == 2));
    }

    #[test]
    fn matter_export_runs_every_tick() {
        let mut clock = MatterClock::default();
        clock.advance(0.0, TICK, 1.0, 0.0, false, true);
        let frame = clock.advance(1.0, 1.0, 1.0, 0.0, false, true);
        assert_eq!(frame.ticks, 60);
        assert_eq!(frame.dropped_seconds, 0.0);
    }

    #[test]
    fn matter_clock_pause_reset_speed_and_seek() {
        let mut clock = MatterClock::default();
        clock.advance(0.0, TICK, 1.0, 0.0, false, false);
        let a = clock.advance(TICK, TICK, 1.0, 0.0, false, false);
        assert_eq!(a.ticks, 1);
        // Paused transport holds.
        let held = clock.advance(TICK, 0.0, 1.0, 0.0, false, false);
        assert_eq!(held.ticks, 0);
        assert_eq!(held.simulation_time, a.simulation_time);
        // Speed 0.5 runs a tick every other frame.
        let ticks: u32 = (2..6)
            .map(|i| clock.advance(i as f64 * TICK, TICK, 0.5, 0.0, false, false).ticks)
            .sum();
        assert_eq!(ticks, 2);
        // A changed reset counter restarts in a new epoch.
        let reset = clock.advance(6.0 * TICK, TICK, 1.0, 1.0, false, false);
        assert!(reset.restarted);
        assert_eq!(reset.epoch, 1);
        assert_eq!(reset.ticks, 0);
        // Seeking backwards restarts too.
        let seek = clock.advance(2.0 * TICK, TICK, 1.0, 1.0, false, false);
        assert!(seek.restarted);
        assert_eq!(seek.epoch, 2);
        // Display sits one tick behind the target.
        let next = clock.advance(3.0 * TICK, TICK, 1.0, 1.0, false, false);
        assert!((next.display_time - 0.0).abs() < 1e-12);
        assert_eq!(next.ticks, 1);
    }
}
