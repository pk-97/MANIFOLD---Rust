//! CPU FLIP reference runtime. Native state belongs exclusively to a worker;
//! the content thread retains bounded control history and immutable mesh frames.
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};

use manifold_core::Seconds;
use manifold_fluids::{
    FrameStats, LiquidOptions, SurfaceOptions, TimeStepOptions, WhitewaterKind, WhitewaterOptions,
    WhitewaterParticle,
};
use manifold_physics::FieldValue;
use manifold_physics::stepping::StepInterval;
use manifold_physics::input::{
    AppliedEvent, EventQueue, HistoryWrite, InputHistory, Timestamped, input_span,
    input_span_before,
};

use super::fluid_cache::CacheMode;
#[cfg(test)]
use super::fluid_cache::{CacheReader, CacheWriter};
use super::fluid_role::FluidRole;
use super::physics_events::ResolvedNodeImpulse;
use super::transform::Transform;
use super::vector_field::ContinuousField;
use crate::generators::mesh_common::{InstanceTransform, MeshVertex};

mod coupled;
mod domain;
pub(super) mod identity;
mod impulses;
mod native;
pub(crate) mod particle_ring;
#[cfg(test)]
mod playback_tests;
#[cfg(all(test, feature = "water-race-probes"))]
mod race_probe;
mod roles;
mod take;
pub use coupled::{CoupledRigidFrame, CoupledRigidInputs};
pub(crate) use coupled::Layout as CoupledRigidLayout;
pub use domain::{FluidDomainLayout, domain_layout};
use impulses::IMPULSE_CAPACITY;
use native::NativeSimulation;
pub use take::{FluidTakeFrame, FluidTakeIdentity, FluidTakeReplay, TakeRange, TakeTime};
pub(super) use take::{PlaybackClock, PreparedGeometry};

pub const TICK: f64 = 1.0 / 60.0;

pub(super) fn simulation_tick(time: f64) -> u64 {
    (time / TICK + 1e-8).floor() as u64
}

/// GPU_FLUID_SURFACE_DESIGN.md D10: the blend presenting display time `s`
/// between frames at `t_a` and `t_b`, and their span. Display time never
/// passes the newest frame; one frame (`t_a == t_b`) presents it fully.
pub(crate) fn display_blend(s: f64, t_a: f64, t_b: f64) -> (f32, f32) {
    let span = t_b - t_a;
    if span <= 0.0 {
        return (1.0, 0.0);
    }
    (((s - t_a) / span).clamp(0.0, 1.0) as f32, span as f32)
}
// Initial retained-input allocation; histories grow without discarding debt.
const HISTORY_CAPACITY: usize = 8192;
const BATCH: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FluidDomainState {
    Initializing,
    Ready,
    PendingInputs,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FluidDomainSnapshot {
    pub epoch: u64,
    pub state: FluidDomainState,
    pub accepted_layout: Option<FluidDomainLayout>,
}

#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FluidSettings {
    pub seed: u64,
    pub resolution: u32,
    pub domain_size: f32,
    /// Explicit axis-aligned scene-space domain. Scale is full XYZ size.
    /// None preserves the legacy cube centred on X/Z with its floor at Y=0.
    pub domain: Option<Transform>,
    /// Closed faces in native order: -X, +X, -Y, +Y, -Z, +Z.
    pub boundary_collisions: [bool; 6],
    pub fill_height: f32,
    pub initial_volume: Option<Transform>,
    pub surface_subdivisions: u32,
    pub liquid: LiquidOptions,
    pub time_steps: TimeStepOptions,
    pub surface: SurfaceOptions,
    pub whitewater: WhitewaterOptions,
    pub apic: bool,
    pub max_vertices: usize,
}

impl Default for FluidSettings {
    fn default() -> Self {
        Self {
            seed: manifold_fluids::DEFAULT_SEED,
            resolution: 24,
            domain_size: 4.0,
            domain: None,
            boundary_collisions: [true; 6],
            fill_height: 0.4,
            initial_volume: None,
            surface_subdivisions: 0,
            liquid: LiquidOptions::default(),
            time_steps: TimeStepOptions::default(),
            surface: SurfaceOptions::default(),
            whitewater: WhitewaterOptions {
                max_particles: 100_000,
                ..WhitewaterOptions::default()
            },
            apic: false,
            max_vertices: 786432,
        }
    }
}

impl FluidSettings {
    pub fn validate(self) -> Result<(), String> {
        self.liquid.validate().map_err(|error| error.to_string())?;
        self.time_steps
            .validate()
            .map_err(|error| error.to_string())?;
        self.surface.validate().map_err(|error| error.to_string())?;
        self.whitewater
            .validate()
            .map_err(|error| error.to_string())?;
        if self.whitewater.max_particles > 250_000 {
            return Err("Water: whitewater capacity must not exceed 250000 particles".into());
        }
        let domain = self.domain_layout()?;
        if !self.fill_height.is_finite()
            || !(0.0..domain.size[1]).contains(&self.fill_height)
            || self.surface_subdivisions > 2
            || self.max_vertices < 3
            || self.max_vertices > 3_145_728
            || !self.max_vertices.is_multiple_of(3)
        {
            return Err(
                "Water: invalid domain, resolution, fill height, surface detail or mesh capacity"
                    .into(),
            );
        }
        if let Some(volume) = self.initial_volume {
            if volume.billboard
                || volume
                    .rot_euler
                    .iter()
                    .any(|v| !v.is_finite() || v.abs() > 1e-6)
            {
                return Err(
                    "Water: initial volume must be a translating axis-aligned box; rotation and billboarding are not supported".into(),
                );
            }
            if volume.pos.iter().any(|v| !v.is_finite())
                || volume.scale.iter().any(|v| !v.is_finite() || *v <= 0.0)
            {
                return Err(
                    "Water: initial volume positions must be finite and sizes must be positive"
                        .into(),
                );
            }
            // Containment belongs to the authored scene domain. Native bounds
            // include the solver's padded boundary cells and have another origin.
            if (0..3).any(|axis| {
                let half = volume.scale[axis] * 0.5;
                volume.pos[axis] - half < domain.min[axis]
                    || volume.pos[axis] + half > domain.min[axis] + domain.size[axis]
            }) {
                return Err("Water: initial volume must be fully contained in the domain".into());
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FluidControls {
    pub emitter: Transform,
    pub obstacle: Transform,
    pub obstacle_enabled: bool,
    pub gravity: [f32; 3],
    pub emission: bool,
    pub inflow_speed: f32,
}

impl Default for FluidControls {
    fn default() -> Self {
        Self {
            emitter: Transform {
                pos: [-0.9, 2.5, 0.0],
                scale: [0.55, 0.4, 0.55],
                ..Transform::default()
            },
            obstacle: Transform {
                pos: [0.65, 0.65, 0.0],
                scale: [0.65, 0.8, 0.65],
                ..Transform::default()
            },
            obstacle_enabled: true,
            gravity: [0.0, -9.81, 0.0],
            emission: true,
            inflow_speed: 1.5,
        }
    }
}

impl FluidControls {
    fn validate(self) -> Result<(), String> {
        for pose in [self.emitter, self.obstacle] {
            if pose.billboard
                || pose
                    .rot_euler
                    .iter()
                    .any(|v| !v.is_finite() || v.abs() > 1e-6)
            {
                return Err("Water: this CPU reference accepts translating axis-aligned emitter and obstacle boxes; rotation is not supported yet".into());
            }
            if pose.pos.iter().any(|v| !v.is_finite())
                || pose.scale.iter().any(|v| !v.is_finite() || *v <= 0.0)
            {
                return Err(
                    "Water: box positions must be finite and sizes must be positive".into(),
                );
            }
        }
        if self.gravity.iter().any(|value| !value.is_finite())
            || !self.inflow_speed.is_finite()
            || self.inflow_speed < 0.0
        {
            return Err(
                "Water: gravity and inflow speed must be finite; inflow speed cannot be negative"
                    .into(),
            );
        }
        Ok(())
    }
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Sample {
    time: f64,
    controls: FluidControls,
    acceleration_field: Option<FieldValue>,
}

impl Timestamped for Sample {
    fn time(&self) -> manifold_physics::Seconds {
        manifold_physics::Seconds(self.time)
    }
}

#[derive(Clone, Copy)]
struct Step {
    previous: FluidControls,
    current: FluidControls,
    next: FluidControls,
}

fn request_count(
    cache_mode: CacheMode,
    due: u64,
    target_tick: u64,
    initialized: bool,
) -> Result<usize, String> {
    if cache_mode == CacheMode::Playback {
        return Ok(usize::from(target_tick > 0));
    }
    if !initialized {
        return Ok(0);
    }
    // Publish progress between short batches even when preview is behind.
    // The remaining debt stays in target_time; no simulation ticks are dropped.
    let due = due.min(BATCH as u64);
    usize::try_from(due).map_err(|_| "Water preview catch-up request is too large".to_owned())
}

/// A whitewater particle's draw scale from its remaining lifetime: FLIP's
/// native path and the GPU whitewater both shrink the last 0.2 seconds
/// instead of leaving a full-sized particle until its removal.
pub(crate) fn whitewater_fade(lifetime: f32) -> f32 {
    (lifetime / 0.2).clamp(0.0, 1.0).sqrt()
}

/// Separate populations share the mesh publication epoch and tick. Each can use
/// an ordinary scene object with its own material, mesh and live instance count.
#[derive(Default, Clone)]
pub struct WhitewaterFrame {
    pub foam: Vec<InstanceTransform>,
    pub bubbles: Vec<InstanceTransform>,
    pub spray: Vec<InstanceTransform>,
}

impl WhitewaterFrame {
    fn clear(&mut self) {
        self.foam.clear();
        self.bubbles.clear();
        self.spray.clear();
    }

    fn prepare(&mut self, capacity: usize) {
        self.clear();
        // Reserve on the worker when a world is initialized. Every population
        // may contain the whole bounded native population; publication reuses
        // both banks without allocations as its mix changes.
        for values in [&mut self.foam, &mut self.bubbles, &mut self.spray] {
            if values.capacity() < capacity {
                values.reserve_exact(capacity);
            }
        }
    }

    fn fill(&mut self, particles: &[WhitewaterParticle], domain: FluidDomainLayout) {
        self.clear();
        for particle in particles {
            let position = domain.to_scene(particle.position);
            let instance = InstanceTransform {
                pos_scale: [position[0], position[1], position[2], whitewater_fade(particle.lifetime)],
                rot_pad: [0.0; 4],
            };
            match particle.kind {
                WhitewaterKind::Foam => self.foam.push(instance),
                WhitewaterKind::Bubble => self.bubbles.push(instance),
                WhitewaterKind::Spray => self.spray.push(instance),
            }
        }
    }
}

/// The project address is distinct from the native tick returned by playback.
/// Untimed legacy caches keep their historical seconds-times-speed address.
#[derive(Clone, Copy, Debug, PartialEq)]
struct PlaybackAddress {
    transport: Seconds,
    legacy_tick: u64,
}

#[derive(Clone, Copy)]
struct PlaybackRequest {
    address: PlaybackAddress,
    published_tick: Option<u64>,
}

#[derive(Clone, Copy)]
struct PlaybackCompletion {
    address: PlaybackAddress,
    unchanged: bool,
}

/// What a request publishes besides the mesh (GPU_FLUID_SURFACE_DESIGN.md
/// D7, D13, D19). Travels in the request and comes back in its reply.
pub(crate) struct Outputs {
    /// Per-tick CPU surface reconstruction; off only when nothing reads
    /// `vertices` (D13). Fixed for a world's lifetime.
    surface_meshing: bool,
    /// Ring slot loaned for this request's particle frame.
    particles: Option<particle_ring::ParticleSlot>,
    /// Capture the current tick into `particles` without stepping.
    capture_only: bool,
    /// Worker answer: the slot was too small (particles, solid nodes).
    growth: Option<(u32, usize)>,
}

impl Default for Outputs {
    fn default() -> Self {
        Self {
            surface_meshing: true,
            particles: None,
            capture_only: false,
            growth: None,
        }
    }
}

struct Request {
    outputs: Outputs,
    source_identity: Option<[u8; 32]>,
    project_tempo: Option<crate::preset_context::ProjectTempo>,
    epoch: u64,
    settings: FluidSettings,
    initial: FluidControls,
    start_tick: u64,
    count: usize,
    interval: Option<StepInterval>,
    history: Vec<Sample>,
    impulses: Vec<AppliedEvent<ResolvedNodeImpulse>>,
    role_setup: Arc<roles::Setup>,
    role_history: Vec<roles::Controls>,
    recycle: Vec<MeshVertex>,
    recycle_whitewater: WhitewaterFrame,
    cache_mode: CacheMode,
    cache_path: Arc<PathBuf>,
    coupled: Option<coupled::Request>,
    timing: take::TimingHandoff,
    playback: Option<PlaybackRequest>,
}

struct Reply {
    outputs: Outputs,
    source_identity: Option<[u8; 32]>,
    epoch: u64,
    tick: u64,
    accepted_interval: Option<StepInterval>,
    /// Exclusive boundary of native ticks begun, including a failed tick.
    started_tick: u64,
    impulses: Vec<AppliedEvent<ResolvedNodeImpulse>>,
    history: Vec<Sample>,
    role_history: Vec<roles::Controls>,
    vertices: Vec<MeshVertex>,
    whitewater: WhitewaterFrame,
    obstacle: Transform,
    stats: FrameStats,
    error: Option<String>,
    coupled: Option<coupled::Request>,
    timing: take::TimingHandoff,
    playback: Option<PlaybackCompletion>,
}

fn cancelled_reply(request: Request) -> Reply {
    Reply {
        outputs: Outputs {
            growth: None,
            ..request.outputs
        },
        source_identity: request.source_identity,
        epoch: request.epoch,
        tick: request.start_tick,
        accepted_interval: None,
        started_tick: request.start_tick,
        impulses: request.impulses,
        history: request.history,
        role_history: request.role_history,
        vertices: request.recycle,
        whitewater: request.recycle_whitewater,
        obstacle: request.initial.obstacle,
        stats: FrameStats::default(),
        error: None,
        coupled: request.coupled,
        timing: request.timing,
        playback: request.playback.map(|request| PlaybackCompletion {
            address: request.address,
            unchanged: false,
        }),
    }
}

struct Worker {
    requests: SyncSender<Request>,
    replies: Receiver<Reply>,
    cancel_epoch: Arc<AtomicU64>,
}

impl Worker {
    fn spawn(cancel_epoch: Arc<AtomicU64>) -> Result<Self, String> {
        let worker_cancel_epoch = Arc::clone(&cancel_epoch);
        let (requests, receiver) = mpsc::sync_channel::<Request>(1);
        let (sender, replies) = mpsc::sync_channel::<Reply>(1);
        std::thread::Builder::new()
            .name("fluid-reference".into())
            .spawn(move || {
                let mut simulation = NativeSimulation::default();
                while let Ok(request) = receiver.recv() {
                    if sender
                        .send(simulation.process(request, &worker_cancel_epoch))
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .map_err(|e| format!("Water worker could not start: {e}"))?;
        Ok(Self {
            requests,
            replies,
            cancel_epoch,
        })
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.cancel_epoch.fetch_add(1, Ordering::Release);
    }
}

pub struct FluidRuntime {
    source_identity: Option<[u8; 32]>,
    committed_source_identity: Option<[u8; 32]>,
    source_error: Option<String>,
    project_tempo: Option<crate::preset_context::ProjectTempo>,
    recording_project_timing: Option<bool>,
    worker: Option<Worker>,
    settings: Option<FluidSettings>,
    history: InputHistory<Sample>,
    timing: take::Capture,
    impulses: EventQueue<ResolvedNodeImpulse>,
    applied_impulses: Vec<AppliedEvent<ResolvedNodeImpulse>>,
    spare_impulses: Option<Vec<AppliedEvent<ResolvedNodeImpulse>>>,
    impulse_outstanding: usize,
    role_setup: Arc<roles::Setup>,
    role_history: roles::History,
    last_transport: Option<f64>,
    previous_reset: Option<f32>,
    reset_requested: bool,
    target_time: f64,
    clock: manifold_physics::clock::SimulationClock,
    export_frames: VecDeque<manifold_physics::clock::ClockFrame>,
    held: super::physics::HeldClock,
    epoch: u64,
    cancel_epoch: Arc<AtomicU64>,
    busy: bool,
    initialized: bool,
    spare: Option<Vec<MeshVertex>>,
    spare_whitewater: Option<WhitewaterFrame>,
    spare_history: Option<Vec<Sample>>,
    spare_role_history: Option<Vec<roles::Controls>>,
    failure: Option<String>,
    accepted_observation: Option<(f64, f64)>,
    completed_playback: Option<PlaybackAddress>,
    pub vertices: Vec<MeshVertex>,
    pub whitewater: WhitewaterFrame,
    pub version: u64,
    pub completed_tick: u64,
    completed_time: f64,
    pub obstacle: Transform,
    pub stats: FrameStats,
    cache_mode: CacheMode,
    cache_path: Arc<PathBuf>,
    coupled: Option<coupled::Runtime>,
    /// Particle-frame slots published as `particles_a/b` and `solid_a/b`.
    pub(crate) particles: particle_ring::ParticleRing,
    /// Something reads the particle-frame outputs this frame.
    particle_outputs: bool,
    /// Something reads `vertices`; Record meshes regardless (D13).
    surface_meshing: bool,
}

impl Default for FluidRuntime {
    fn default() -> Self {
        Self {
            source_identity: None,
            committed_source_identity: None,
            source_error: None,
            project_tempo: None,
            recording_project_timing: None,
            worker: None,
            settings: None,
            history: InputHistory::with_growing_capacity(HISTORY_CAPACITY)
                .expect("FLIP history capacity must be at least two"),
            timing: take::Capture::new(HISTORY_CAPACITY),
            impulses: impulses::new_queue(),
            applied_impulses: Vec::with_capacity(IMPULSE_CAPACITY),
            spare_impulses: Some(Vec::with_capacity(IMPULSE_CAPACITY)),
            impulse_outstanding: 0,
            role_setup: Arc::new(roles::Setup::default()),
            role_history: roles::History::default(),
            last_transport: None,
            previous_reset: None,
            reset_requested: false,
            target_time: 0.0,
            clock: Default::default(),
            export_frames: VecDeque::with_capacity(HISTORY_CAPACITY),
            held: Default::default(),
            epoch: 0,
            cancel_epoch: Arc::new(AtomicU64::new(0)),
            busy: false,
            initialized: false,
            spare: Some(Vec::new()),
            spare_whitewater: Some(WhitewaterFrame::default()),
            spare_history: Some(Vec::with_capacity(HISTORY_CAPACITY)),
            spare_role_history: Some(Vec::new()),
            failure: None,
            accepted_observation: None,
            completed_playback: None,
            vertices: Vec::new(),
            whitewater: WhitewaterFrame::default(),
            version: 0,
            completed_tick: 0,
            completed_time: 0.0,
            obstacle: FluidControls::default().obstacle,
            stats: FrameStats::default(),
            cache_mode: CacheMode::Live,
            cache_path: Arc::new(PathBuf::new()),
            coupled: None,
            particles: particle_ring::ParticleRing::default(),
            particle_outputs: false,
            surface_meshing: true,
        }
    }
}

impl Drop for FluidRuntime {
    fn drop(&mut self) {
        self.cancel_epoch.fetch_add(1, Ordering::Release);
    }
}

impl FluidRuntime {
    pub(crate) fn set_source_identity(&mut self, identity: Result<[u8; 32], String>) {
        let identity = match identity {
            Ok(identity) => identity,
            Err(error) => {
                if self.source_error.as_ref() != Some(&error) {
                    if self.cache_mode == CacheMode::Playback {
                        self.clear();
                    }
                    self.source_error = Some(error);
                }
                return;
            }
        };
        let recovering = self.source_error.take().is_some();
        if self.source_identity == Some(identity) && !recovering {
            return;
        }
        self.source_identity = Some(identity);
        if self.cache_mode == CacheMode::Playback {
            self.clear();
        }
    }

    pub(crate) fn set_project_tempo(
        &mut self,
        tempo: Option<&crate::preset_context::ProjectTempo>,
    ) {
        let unchanged = match (self.project_tempo.as_ref(), tempo) {
            (Some(current), Some(next)) => current.shares_mapping(next),
            (None, None) => true,
            _ => false,
        };
        if unchanged {
            return;
        }
        self.project_tempo = tempo.cloned();
        if self.cache_mode == CacheMode::Playback {
            // Cancel replies validated against the previous project. Reopen
            // the same committed cache on its worker with the new tempo view.
            self.clear();
        }
    }

    /// Select a worker cache. The path is copied only when the mode or path
    /// changes; ordinary frame evaluation therefore does not churn path
    /// allocations. Record and playback never fall back to live simulation.
    pub(crate) fn set_cache(&mut self, mode: CacheMode, path: &str) -> Result<(), String> {
        if mode != CacheMode::Live && path.is_empty() {
            return Err("Water cache path is required for record and playback".into());
        }
        if self.cache_mode == mode && self.cache_path.as_path() == std::path::Path::new(path) {
            return Ok(());
        }
        self.cache_mode = mode;
        if self.cache_path.as_path() != std::path::Path::new(path) {
            self.cache_path = Arc::new(PathBuf::from(path));
        }
        self.clear();
        Ok(())
    }

    /// Reset the shared owner at its next valid full observation. Multiple
    /// participant reset edges collapse into the same epoch transition.
    pub(crate) fn request_reset(&mut self) {
        self.reset_requested = true;
    }

    pub fn clear(&mut self) {
        self.reset_requested = false;
        self.settings = None;
        self.accepted_observation = None;
        self.completed_playback = None;
        self.last_transport = None;
        self.history.clear();
        self.timing.clear();
        self.committed_source_identity = None;
        self.recording_project_timing = None;
        self.role_history.clear();
        if let Some(coupled) = &mut self.coupled {
            coupled.clear();
        }
        self.target_time = 0.0;
        self.clock.restart();
        self.export_frames.clear();
        self.held = Default::default();
        self.completed_tick = 0;
        self.completed_time = 0.0;
        self.epoch = self.epoch.checked_add(1).expect("fluid epoch exhausted");
        if self.epoch > 1 {
            self.impulses
                .reset(self.epoch, Seconds::ZERO)
                .expect("fluid reset uses a strictly newer epoch");
        }
        self.applied_impulses.clear();
        if let Some(events) = &mut self.spare_impulses {
            events.clear();
        }
        self.impulse_outstanding = 0;
        self.cancel_epoch.store(self.epoch, Ordering::Release);
        self.initialized = false;
        self.failure = None;
        self.vertices.clear();
        self.whitewater.clear();
        self.particles.clear();
        self.stats = FrameStats::default();
        self.version = self.version.wrapping_add(1);
    }

    /// Which outputs the graph reads. Particle outputs publish frames from the
    /// next accepted tick. Meshing is fixed before a world's first step, so a
    /// change restarts the simulation (D13); Record always meshes.
    pub(crate) fn set_outputs(&mut self, particles: bool, vertices: bool) {
        self.particle_outputs = particles;
        let meshing = |mode, vertices| vertices || mode == CacheMode::Record;
        if meshing(self.cache_mode, vertices) != meshing(self.cache_mode, self.surface_meshing) {
            self.clear();
        }
        self.surface_meshing = vertices;
    }

    fn effective_surface_meshing(&self) -> bool {
        self.surface_meshing || self.cache_mode == CacheMode::Record
    }

    /// Allocate or regrow particle slots. Content thread, before `advance`.
    pub(crate) fn prepare_particles(&mut self, device: &manifold_gpu::GpuDevice) -> Result<(), String> {
        let Some(settings) = self.settings else {
            return Ok(());
        };
        let layout = settings.domain_layout()?;
        let nodes = layout.cells.iter().map(|&n| n as usize + 4).product();
        self.particles.prepare(device, nodes)
    }

    /// Scene box and node counts of the solid lattice: the padded native
    /// grid, node (i, j, k) at `min + (i, j, k)·size/(nodes − 1)`.
    pub(crate) fn particle_lattice(&self) -> Option<(Transform, [u32; 3])> {
        Some(self.settings?.domain_layout().ok()?.solid_lattice())
    }

    /// The published tick is not the completed tick: a capture is owed.
    pub(crate) fn particle_capture_pending(&self) -> bool {
        self.particle_outputs
            && self.initialized
            && self.completed_tick > 0
            && self.particles.newest_tick() != Some(self.completed_tick)
    }

    /// D10 display clock: `s = target − tick`, blended between the two
    /// newest published frames. Returns (blend, span); one frame gives (1, 0).
    pub(crate) fn particle_blend(&self) -> (f32, f32) {
        let Some((a, b)) = self.particles.pair() else {
            return (1.0, 0.0);
        };
        let tick_time = |slot: &particle_ring::ParticleSlot| {
            slot.frame().map_or(0.0, |frame| frame.time)
        };
        display_blend(self.target_time - TICK, tick_time(a), tick_time(b))
    }

    pub fn simulation_time(&self) -> f64 {
        self.completed_time
    }

    /// Read the rigid poses belonging to the currently accepted liquid mesh.
    /// Graph hosts must latch this pair before evaluating participant outputs.
    pub fn coupled_rigid_frame(&self) -> Option<&CoupledRigidFrame> {
        self.coupled.as_ref()?.accepted.as_ref()
    }
    pub fn lag_seconds(&self) -> f64 {
        if self.cache_mode == CacheMode::Playback {
            // A take can start anywhere in the project and retain speed-zero
            // spans. Its simulation tick is not a project-time progress clock.
            return match (self.last_transport, self.completed_playback) {
                (Some(target), Some(completed)) => (target - completed.transport.0).max(0.0),
                _ => 0.0,
            };
        }
        (self.target_time - self.simulation_time()).max(0.0)
    }
    pub fn warmup_pending(&self) -> bool {
        self.busy
            && !self.initialized
            && self.failure.is_none()
            && (self.cache_mode == CacheMode::Live || self.source_error.is_none())
    }

    pub fn domain_snapshot(&self) -> FluidDomainSnapshot {
        let state = if self.failure.is_some()
            || (self.cache_mode != CacheMode::Live && self.source_error.is_some())
        {
            FluidDomainState::Failed
        } else if self.initialized && self.settings.is_some() {
            FluidDomainState::Ready
        } else {
            FluidDomainState::Initializing
        };
        let accepted_layout = if state == FluidDomainState::Ready {
            self.settings
                .expect("ready fluid runtime has settings")
                .domain_layout()
                .ok()
        } else {
            None
        };
        FluidDomainSnapshot {
            epoch: self.epoch,
            state,
            accepted_layout,
        }
    }

    /// Historical graph evaluations retain inputs. Explicit offline drains may
    /// advance the worker between bounded batches; graph output publication
    /// happens only in the real render frame.
    pub fn observe(
        &mut self,
        settings: FluidSettings,
        controls: FluidControls,
        transport: Seconds,
        speed: f32,
        reset: f32,
    ) -> Result<(), String> {
        self.observe_scene(settings, controls, &[], transport, speed, reset)
    }

    /// Scene roles share the fixed-tick input history and worker ownership.
    pub fn observe_scene(
        &mut self,
        settings: FluidSettings,
        controls: FluidControls,
        scene_roles: &[Option<FluidRole>],
        transport: Seconds,
        speed: f32,
        reset: f32,
    ) -> Result<(), String> {
        self.observe_scene_with_field(
            settings,
            controls,
            scene_roles,
            None,
            transport,
            speed,
            reset,
        )
    }

    /// Retain scene-space acceleration with the same input intervals as sources
    /// and colliders. Field edits affect subsequent ticks without restarting FLIP.
    #[allow(clippy::too_many_arguments)]
    pub fn observe_scene_with_field(
        &mut self,
        settings: FluidSettings,
        controls: FluidControls,
        scene_roles: &[Option<FluidRole>],
        acceleration_field: Option<FieldValue>,
        transport: Seconds,
        speed: f32,
        reset: f32,
    ) -> Result<(), String> {
        self.observe_coupled_scene_with_field(
            settings,
            controls,
            scene_roles,
            acceleration_field,
            None,
            transport,
            speed,
            reset,
        )
    }

    /// Record the host's existing beat/second pair alongside its accepted
    /// simulation time. Standalone callers without project timing keep using
    /// the seconds-only observation API; their takes have no beat-range map.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn observe_coupled_frame(
        &mut self,
        settings: FluidSettings,
        controls: FluidControls,
        scene_roles: &[Option<FluidRole>],
        acceleration_field: Option<FieldValue>,
        rigid: Option<CoupledRigidInputs<'_>>,
        frame: super::FrameTime,
        speed: f32,
        reset: f32,
    ) -> Result<(), String> {
        self.observe_coupled_scene_with_field(
            settings,
            controls,
            scene_roles,
            acceleration_field,
            rigid,
            frame.seconds,
            speed,
            reset,
        )?;
        if self.cache_mode == CacheMode::Record
            && let Some((transport, simulation)) = self.accepted_observation
        {
            let project_timing = self.project_tempo.is_some();
            if self
                .recording_project_timing
                .is_some_and(|prior| prior != project_timing)
            {
                let error = "Physics take: project timing provenance changed during recording; restart the take".to_owned();
                self.failure = Some(error.clone());
                return Err(error);
            }
            self.recording_project_timing = Some(project_timing);
            let result = self.timing.record(take::TakeTime {
                beat: frame.beats,
                transport: Seconds(transport),
                simulation: Seconds(simulation),
            });
            if let Err(error) = &result {
                self.failure = Some(error.clone());
            }
            result?;
        }
        Ok(())
    }

    /// Feed a connected rigid/liquid scene to the same exclusive native worker.
    /// Both sets of controls use this runtime's transport mapping and epoch.
    /// Ordinary standalone fluids pass `None` for the rigid participant.
    #[allow(clippy::too_many_arguments)]
    pub fn observe_coupled_scene_with_field(
        &mut self,
        settings: FluidSettings,
        controls: FluidControls,
        scene_roles: &[Option<FluidRole>],
        acceleration_field: Option<FieldValue>,
        rigid: Option<CoupledRigidInputs<'_>>,
        transport: Seconds,
        speed: f32,
        reset: f32,
    ) -> Result<(), String> {
        self.accepted_observation = None;
        if super::physics::authored_sample_only() && self.reset_requested {
            return Ok(());
        }
        if let Some(rigid) = rigid {
            rigid.validate()?;
            if self.cache_mode != CacheMode::Live {
                return Err("Coupled physics requires Live mode until paired poses and its input take are recorded in the cache manifest".into());
            }
        }
        let coupling_changed = match (&self.coupled, rigid) {
            (Some(current), Some(inputs)) => !current.matches(inputs),
            (None, None) => false,
            _ => true,
        };
        if super::physics::authored_sample_only() && coupling_changed {
            // Structural membership changes are accepted only by the current
            // full graph evaluation, never spliced into historical playback.
            return Ok(());
        }
        let authored_only_settings_withheld = super::physics::authored_sample_only()
            && self.settings.is_some()
            && self.settings != Some(settings);
        // Setup edits take effect at the current render evaluation. Replaying
        // live controls between frames must not move/rebuild the domain at
        // historical timestamps using newly authored setup values.
        let settings = if super::physics::authored_sample_only() {
            self.settings.unwrap_or(settings)
        } else {
            settings
        };
        settings.validate()?;
        roles::Setup::validate(scene_roles)?;
        if self.cache_mode != CacheMode::Live && scene_roles.iter().any(Option::is_some) {
            return Err("Fluid scene roles require Live mode until their geometry and input take are recorded in the cache manifest".into());
        }
        if self.cache_mode != CacheMode::Live && acceleration_field.is_some() {
            return Err("Fluid vector fields require Live mode until their input take is recorded in the cache manifest".into());
        }
        if self.cache_mode != CacheMode::Playback {
            controls.validate()?;
        }
        if !transport.0.is_finite()
            || !speed.is_finite()
            || !(0.0..=4.0).contains(&speed)
            || !reset.is_finite()
        {
            if self.cache_mode == CacheMode::Live && !super::physics::offline_simulation() {
                // Retain the previous accepted observation. The next valid
                // transport still owns the whole elapsed span.
                super::physics_metrics::record_simulation(
                    self.target_time, self.completed_time, false, true,
                );
                return Ok(());
            }
            return Err("Water: invalid transport, speed or reset value".into());
        }
        // Trigger buttons publish a counter, just like Physics World. Every
        // changed count (including undo) resets once; a held count is inert.
        let reset_edge = self
            .previous_reset
            .is_some_and(|previous| previous != reset);
        self.previous_reset = Some(reset);
        let role_topology_changed = !self.role_setup.matches(scene_roles);
        if self.settings != Some(settings)
            || role_topology_changed
            || coupling_changed
            || self.reset_requested
            || reset_edge
            || (self.cache_mode != CacheMode::Playback
                && self
                    .last_transport
                    .is_some_and(|previous| transport.0 < previous - 1e-9))
        {
            self.clear();
            self.settings = Some(settings);
            self.obstacle = controls.obstacle;
            match (self.coupled.as_mut(), rigid) {
                (Some(current), Some(inputs)) => current.reseed(inputs),
                (None, Some(inputs)) => self.coupled = Some(coupled::Runtime::new(inputs)),
                (_, None) => self.coupled = None,
            }
        }
        if role_topology_changed {
            self.role_setup = Arc::new(roles::Setup::new(scene_roles));
            self.role_history.prepare(self.role_setup.len());
        }
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
        // Source/history samples map inputs to simulation time without
        // consuming frame sequences. Only a render or an explicit offline
        // drain accepts intervals that the worker will actually execute.
        let clock_frame = if self.cache_mode != CacheMode::Playback
            && (!super::physics::authored_sample_only()
                || super::physics::history_drain_requested())
        {
            Some(self.clock.advance(
                transport.0,
                super::physics::project_frame_interval(),
                speed,
                reset,
                false,
                super::physics::offline_simulation(),
            ))
        } else {
            None
        };
        if self.cache_mode == CacheMode::Live && super::physics::offline_simulation()
            && let Some(frame) = clock_frame.as_ref().filter(|frame| frame.ticks > 0)
        {
            if self.export_frames.back_mut().is_some_and(|previous| previous.append(frame)) {
                // Adjacent observations share one retained project schedule.
            } else if self.export_frames.len() == HISTORY_CAPACITY {
                return Err("Water export interval history is full; drain accepted intervals before observing more transport".into());
            } else {
                self.export_frames.push_back(frame.clone());
            }
        }
        if clock_frame.as_ref().is_some_and(|frame| frame.numerical_error)
            && !crate::node_graph::physics::offline_simulation()
        {
            crate::node_graph::physics_metrics::record_simulation(0.0, 0.0, false, true);
        }
        let target_time = if self.cache_mode == CacheMode::Playback {
            (transport.0 * speed as f64).max(0.0)
        } else if clock_frame.is_some() {
            self.clock.simulation_at(transport.0)
        } else if self.last_transport.is_some() {
            self.clock.observe_speed(transport.0, speed)
        } else {
            self.target_time
        };
        self.held.observe(target_time);
        if self.cache_mode == CacheMode::Playback {
            // The worker resolves timed takes from project transport. Retain
            // the absolute speed-scaled address only for untimed legacy caches.
            self.history.clear();
            self.role_history.clear();
        }
        self.prune_history()?;
        if self.history.back().is_some_and(|last| {
            last.time == target_time
                && last.controls == controls
                && last.acceleration_field == acceleration_field
        }) && self
            .role_history
            .latest_matches(&self.role_setup, scene_roles)
            && match (&self.coupled, rigid) {
                (Some(current), Some(inputs)) => current.latest_matches(inputs),
                (None, None) => true,
                _ => false,
            }
        {
            // A held transport with unchanged controls needs no extra endpoint,
            // even when history is full and the worker is still catching up.
            self.target_time = target_time;
            self.last_transport = Some(transport.0);
            if !authored_only_settings_withheld {
                self.accepted_observation = Some((transport.0, self.target_time));
            }
            return Ok(());
        }
        let write = match self.history.record(
            Sample {
                time: target_time,
                controls,
                acceleration_field,
            },
            manifold_physics::Seconds(self.simulation_time()),
        ) {
            Ok(write) => write,
            Err(error) => {
                let message = format!("Water: {error}");
                self.failure = Some(message.clone());
                return Err(message);
            }
        };
        self.target_time = target_time;
        self.last_transport = Some(transport.0);
        self.role_history.observe(
            &self.role_setup,
            scene_roles,
            matches!(write, HistoryWrite::Replaced),
        );
        let completed = Seconds(self.simulation_time());
        if let (Some(current), Some(inputs)) = (&mut self.coupled, rigid)
            && let Err(error) = current.observe(inputs, Seconds(target_time), completed)
        {
            self.failure = Some(error.clone());
            return Err(error);
        }
        if !authored_only_settings_withheld {
            self.accepted_observation = Some((transport.0, self.target_time));
        }
        Ok(())
    }

    pub fn hold_pending(&mut self, transport: Seconds) {
        self.accepted_observation = None;
        self.last_transport = Some(transport.0);
    }

    fn prune_history(&mut self) -> Result<(), String> {
        let retain_from = (self.simulation_time() - TICK).max(0.0);
        let removed = self
            .history
            .prune_before(manifold_physics::Seconds(retain_from))
            .map_err(|error| format!("Water preview history could not be pruned: {error}"))?;
        self.role_history.pop_front(removed);
        if let Some(coupled) = &mut self.coupled {
            coupled.prune(Seconds(retain_from))?;
        }
        Ok(())
    }

    fn controls_at<'a>(history: impl Iterator<Item = &'a Sample>, time: f64) -> FluidControls {
        let span =
            input_span(history, manifold_physics::Seconds(time)).expect("observe before advance");
        Self::controls_from_span(span)
    }

    fn controls_at_before<'a>(
        history: impl Iterator<Item = &'a Sample>,
        time: f64,
    ) -> FluidControls {
        let span = input_span_before(history, manifold_physics::Seconds(time))
            .expect("observe before advance");
        Self::controls_from_span(span)
    }

    fn controls_from_span(span: manifold_physics::input::InputSpan<'_, Sample>) -> FluidControls {
        let previous = span.before.controls;
        let next = span.after.controls;
        let alpha = span.alpha;
        let interpolate = |a: Transform, b: Transform| Transform {
            pos: std::array::from_fn(|i| a.pos[i] + alpha * (b.pos[i] - a.pos[i])),
            scale: std::array::from_fn(|i| a.scale[i] + alpha * (b.scale[i] - a.scale[i])),
            ..a
        };
        // Continuous pose/force values interpolate; switches are held until
        // their exact authored time rather than smeared in time.
        FluidControls {
            emitter: interpolate(previous.emitter, next.emitter),
            obstacle: interpolate(previous.obstacle, next.obstacle),
            obstacle_enabled: if alpha >= 1.0 {
                next.obstacle_enabled
            } else {
                previous.obstacle_enabled
            },
            gravity: std::array::from_fn(|axis| {
                previous.gravity[axis] + alpha * (next.gravity[axis] - previous.gravity[axis])
            }),
            inflow_speed: previous.inflow_speed
                + alpha * (next.inflow_speed - previous.inflow_speed),
            emission: if alpha >= 1.0 {
                next.emission
            } else {
                previous.emission
            },
        }
    }

    fn step_at(history: &[Sample], tick: u64) -> Step {
        let current = tick as f64 * TICK;
        Self::step_at_time(history, Seconds(current))
    }

    fn step_at_time(history: &[Sample], current: Seconds) -> Step {
        Step {
            previous: Self::controls_at(history.iter(), (current.0 - TICK).max(0.0)),
            current: Self::controls_at(history.iter(), current.0),
            next: Self::controls_at_before(history.iter(), current.0 + TICK),
        }
    }

    fn step_at_interval(history: &[Sample], interval: StepInterval) -> Step {
        let separation = interval.duration().0;
        Step {
            previous: Self::controls_at(
                history.iter(),
                (interval.start.0 - separation).max(0.0),
            ),
            current: Self::controls_at(history.iter(), interval.start.0),
            next: Self::controls_at_before(history.iter(), interval.end.0),
        }
    }

    fn field_at(history: &[Sample], tick: u64, domain: FluidDomainLayout) -> ContinuousField<'_> {
        Self::field_at_time(history, Seconds(tick as f64 * TICK), domain)
    }

    fn field_at_time(
        history: &[Sample],
        time: Seconds,
        domain: FluidDomainLayout,
    ) -> ContinuousField<'_> {
        let span = input_span(history.iter(), time)
            .expect("observe before advance");
        ContinuousField {
            before: span.before.acceleration_field.as_ref(),
            after: span.after.acceleration_field.as_ref(),
            alpha: span.alpha,
            origin: domain.native_origin(),
        }
    }

    fn accept(&mut self, mut reply: Reply) -> Result<(), String> {
        self.busy = false;
        // A capture-only reply carries a particle frame and nothing else.
        let has_output = !reply.outputs.capture_only
            && !reply.timing.metadata_only
            && !reply.playback.is_some_and(|completed| completed.unchanged);
        if let Err(error) = self.timing.recycle(
            std::mem::take(&mut reply.timing),
            reply.epoch == self.epoch && reply.error.is_none(),
        ) {
            reply.error = Some(error);
        }
        let mut publish = has_output && reply.epoch == self.epoch && reply.error.is_none();
        if publish {
            match (&self.coupled, &reply.coupled) {
                (Some(_), Some(coupled))
                    if coupled.output.stamp
                        == (manifold_physics::TickStamp {
                            epoch: reply.epoch,
                            tick: reply.tick,
                        }) => {}
                (None, None) => {}
                _ => {
                    reply.error = Some(
                        "Fluid coupling: worker returned an unmatched rigid/liquid frame".into(),
                    );
                    publish = false;
                }
            }
        }
        if let Some(current) = &mut self.coupled {
            if let Some(coupled) = reply.coupled {
                current.accept(coupled, publish);
            } else if reply.epoch == self.epoch {
                current.recover_missing_request();
            }
        }
        if let Some(slot) = reply.outputs.particles.take() {
            let fresh = reply.epoch == self.epoch && reply.error.is_none();
            if fresh && let Some((particles, solid)) = reply.outputs.growth {
                self.particles.grow(particles, solid);
            }
            self.particles.accept(slot, fresh);
        }
        self.accept_impulse_batch(reply.epoch, reply.started_tick, reply.impulses);
        self.spare_role_history = Some(reply.role_history);
        if reply.epoch != self.epoch {
            self.spare = Some(reply.vertices);
            self.spare_whitewater = Some(reply.whitewater);
            self.spare_history = Some(reply.history);
            return Ok(());
        }
        if let Some(error) = reply.error {
            if self.cache_mode == CacheMode::Live
                && !crate::node_graph::physics::offline_simulation()
                && reply.epoch == self.epoch
            {
                crate::node_graph::physics_metrics::record_simulation(
                    self.target_time,
                    self.completed_time,
                    reply.stats.cap_hit,
                    reply.stats.numerical_recovery,
                );
            }
            self.spare = Some(reply.vertices);
            self.spare_whitewater = Some(reply.whitewater);
            self.spare_history = Some(reply.history);
            self.failure = Some(error.clone());
            return Err(error);
        }
        if let Some(completed) = reply.playback {
            self.completed_playback = Some(completed.address);
        }
        if self.cache_mode == CacheMode::Record {
            self.committed_source_identity = reply.source_identity;
        }
        if !has_output {
            self.spare = Some(reply.vertices);
            self.spare_whitewater = Some(reply.whitewater);
            self.spare_history = Some(reply.history);
            return Ok(());
        }
        self.spare = Some(std::mem::replace(&mut self.vertices, reply.vertices));
        self.spare_whitewater = Some(std::mem::replace(&mut self.whitewater, reply.whitewater));
        self.spare_history = Some(reply.history);
        self.completed_tick = reply.tick;
        let completed_time = reply
            .accepted_interval
            .map_or(reply.tick as f64 * TICK, |interval| interval.end.0);
        if self.cache_mode == CacheMode::Live
            && !crate::node_graph::physics::offline_simulation()
        {
            crate::node_graph::physics_metrics::record_simulation(
                self.target_time,
                completed_time,
                reply.stats.cap_hit,
                reply.stats.numerical_recovery,
            );
        }
        self.completed_time = completed_time;
        self.obstacle = reply.obstacle;
        self.stats = reply.stats;
        self.initialized = true;
        self.version = self.version.wrapping_add(1);
        self.prune_history()?;
        Ok(())
    }

    pub fn advance(&mut self, blocking: bool) -> Result<(), String> {
        if self.cache_mode != CacheMode::Live
            && let Some(error) = &self.source_error
        {
            return Err(error.clone());
        }
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
        let Some(settings) = self.settings else {
            return Ok(());
        };
        if self.worker.is_none() {
            self.worker = Some(Worker::spawn(Arc::clone(&self.cancel_epoch))?);
        }
        loop {
            if self.busy {
                let replies = &self.worker.as_ref().expect("worker exists").replies;
                let reply = if blocking {
                    Some(
                        replies
                            .recv()
                            .map_err(|_| "Water worker disconnected".to_owned())?,
                    )
                } else {
                    match replies.try_recv() {
                        Ok(reply) => Some(reply),
                        Err(TryRecvError::Empty) => None,
                        Err(TryRecvError::Disconnected) => {
                            return Err("Water worker disconnected".into());
                        }
                    }
                };
                if let Some(reply) = reply {
                    self.accept(reply)?;
                } else {
                    return Ok(());
                }
            }
            // Pause and Simulation Speed 0 send no new live request, so
            // retained time debt cannot drain while held. The batch already
            // in flight covers played time and still publishes. Offline drains
            // each frame's debt inside that frame.
            if self.held.is_held() && self.initialized && !blocking {
                return Ok(());
            }
        let live_mode = self.cache_mode == CacheMode::Live;
        let target_time = if live_mode && super::physics::offline_simulation() {
            self.clock.accepted_time()
        } else { self.target_time };
        let target_tick = simulation_tick(target_time);
            let playback = (self.cache_mode == CacheMode::Playback).then(|| PlaybackAddress {
                transport: Seconds(self.last_transport.expect("observed transport")),
                legacy_tick: target_tick,
            });
            let due = target_tick.saturating_sub(self.completed_tick);
            while self.export_frames.front().is_some_and(|frame|
                self.completed_tick >= frame.first_sequence + u64::from(frame.ticks))
            {
                self.export_frames.pop_front();
            }
            let live_interval = if live_mode && target_time > self.simulation_time() {
                Some(if super::physics::offline_simulation() {
                    self.export_frames.front()
                        .and_then(|frame| self.completed_tick.checked_sub(frame.first_sequence)
                            .and_then(|ordinal| frame.interval(ordinal)))
                        .ok_or("Water export is missing its accepted project interval")?
                } else {
                    StepInterval::new(Seconds(self.simulation_time()), Seconds(target_time))
                })
            } else { None };
            // A owed particle capture goes before any further stepping, so
            // the frame is the completed tick itself (growth, late wiring).
            let capture_only = self.particle_capture_pending();
            if self.initialized
                && !capture_only
                && (if self.cache_mode == CacheMode::Playback {
                    playback == self.completed_playback
                } else {
                    (if live_mode {
                        target_time <= self.simulation_time()
                    } else {
                        due == 0
                    })
                        && !(self.cache_mode == CacheMode::Record
                            && (self.timing.pending()
                                || self.source_identity != self.committed_source_identity))
                })
            {
                return Ok(());
            }
            let initial = self.history.front().expect("observed controls").controls;
            let count = if capture_only {
                0
            } else if live_mode {
                usize::from(live_interval.is_some())
            } else {
                request_count(self.cache_mode, due, target_tick, self.initialized)?
            };
            let particles = if self.particle_outputs && (capture_only || count > 0) {
                match self.particles.take(blocking) {
                    Some(slot) => Some(slot),
                    // Live never waits: ring exhaustion skips this request
                    // (D19). An owed capture waits for a prepared slot.
                    None if capture_only || !blocking => return Ok(()),
                    // Offline keeps stepping; the capture is then owed.
                    None => None,
                }
            } else {
                None
            };
            let impulses = if self.cache_mode == CacheMode::Live {
                match self.prepare_impulse_batch(
                    self.completed_tick,
                    count,
                    live_mode.then_some(live_interval).flatten(),
                ) {
                    Ok(events) => events,
                    Err(error) => {
                        if let Some(slot) = particles {
                            self.particles.accept(slot, false);
                        }
                        return Err(error);
                    }
                }
            } else {
                // Legacy cache playback can seek; it has no live impulse clock.
                let mut events = self.spare_impulses.take().expect("recycled impulse batch");
                events.clear();
                events
            };
            let mut history = self
                .spare_history
                .take()
                .expect("one recycled history snapshot per request");
            history.clear();
            history.extend(self.history.iter().cloned());
            let mut role_history = self
                .spare_role_history
                .take()
                .expect("one recycled role history per request");
            self.role_history.snapshot(&mut role_history);
            let request = Request {
                outputs: Outputs {
                    surface_meshing: self.effective_surface_meshing(),
                    particles,
                    capture_only,
                    growth: None,
                },
                source_identity: self.source_identity,
                project_tempo: self.project_tempo.clone(),
                epoch: self.epoch,
                settings,
                initial,
                start_tick: if self.cache_mode == CacheMode::Playback {
                    target_tick.saturating_sub(count as u64)
                } else {
                    self.completed_tick
                },
                count,
                interval: if count > 0 { live_interval } else { None },
                history,
                impulses,
                role_setup: Arc::clone(&self.role_setup),
                role_history,
                recycle: self.spare.take().expect("one recycled mesh per request"),
                recycle_whitewater: self
                    .spare_whitewater
                    .take()
                    .expect("one recycled whitewater frame per request"),
                cache_mode: self.cache_mode,
                cache_path: self.cache_path.clone(),
                // A capture-only reply publishes no rigid frame.
                coupled: if capture_only {
                    None
                } else {
                    self.coupled.as_mut().map(coupled::Runtime::request)
                },
                timing: self.timing.snapshot(
                    self.initialized && self.cache_mode == CacheMode::Record && count == 0,
                ),
                playback: playback.map(|address| PlaybackRequest {
                    address,
                    published_tick: self.initialized.then_some(self.completed_tick),
                }),
            };
            let worker = self.worker.as_ref().expect("worker exists");
            if let Err(error) = worker.requests.send(request) {
                let request = error.0;
                if let Some(slot) = request.outputs.particles {
                    self.particles.accept(slot, false);
                }
                self.spare = Some(request.recycle);
                self.spare_whitewater = Some(request.recycle_whitewater);
                self.spare_history = Some(request.history);
                self.spare_impulses = Some(request.impulses);
                self.spare_role_history = Some(request.role_history);
                self.timing.recycle(request.timing, false)?;
                if let (Some(current), Some(coupled)) = (&mut self.coupled, request.coupled) {
                    current.accept(coupled, false);
                }
                let message = "Water worker disconnected".to_owned();
                self.failure = Some(message.clone());
                return Err(message);
            }
            self.busy = true;
            if !blocking {
                return Ok(());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_physics::VectorField;

    #[test]
    fn fluid_shared_field_history_preserves_pending_edits_and_scene_coordinates() {
        let settings = FluidSettings {
            domain: Some(Transform {
                pos: [10.0, -3.0, 7.0],
                scale: [4.0; 3],
                ..Transform::default()
            }),
            ..FluidSettings::default()
        };
        let domain = settings.domain_layout().unwrap();
        let mut runtime = FluidRuntime::default();
        let controls = FluidControls::default();
        for (time, strength) in [(0.0, 0.0), (2.0 * TICK, 4.0), (2.0 * TICK, 10.0)] {
            runtime
                .observe_scene_with_field(
                    settings,
                    controls,
                    &[],
                    Some(FieldValue::uniform([strength, 0.0, 0.0]).unwrap()),
                    Seconds(time),
                    1.0,
                    0.0,
                )
                .unwrap();
        }
        let epoch = runtime.epoch;
        let samples: Vec<_> = runtime.history.iter().cloned().collect();
        assert_eq!(
            FluidRuntime::field_at(&samples, 1, domain).sample([1.0; 3]),
            [2.0, 0.0, 0.0]
        );
        assert_eq!(
            FluidRuntime::field_at(&samples, 2, domain).sample([1.0; 3]),
            [10.0, 0.0, 0.0]
        );

        let center = settings.domain.unwrap().pos;
        let radial = FieldValue::radial(center, 4.0, 1.0).unwrap();
        runtime
            .observe_scene_with_field(
                settings,
                controls,
                &[],
                Some(radial),
                Seconds(2.0 * TICK),
                1.0,
                0.0,
            )
            .unwrap();
        assert_eq!(
            runtime.epoch, epoch,
            "a live field edit must preserve FLIP state"
        );
        let samples: Vec<_> = runtime.history.iter().cloned().collect();
        let position = domain.to_native([center[0] + 1.0, center[1], center[2]]);
        assert_eq!(
            FluidRuntime::field_at(&samples, 2, domain).sample(position),
            [0.75, 0.0, 0.0]
        );
        runtime
            .observe_scene_with_field(settings, controls, &[], None, Seconds(2.0 * TICK), 1.0, 1.0)
            .unwrap();
        assert_ne!(runtime.epoch, epoch);
        assert_eq!(runtime.history.len(), 1);
        assert!(
            runtime
                .history
                .front()
                .unwrap()
                .acceleration_field
                .is_none()
        );
    }

    #[test]
    fn fluid_shared_field_requires_a_recorded_input_identity_for_cache_modes() {
        for mode in [CacheMode::Record, CacheMode::Playback] {
            let mut runtime = FluidRuntime::default();
            runtime.set_cache(mode, "not-opened-by-observe").unwrap();
            let error = runtime
                .observe_scene_with_field(
                    FluidSettings::default(),
                    FluidControls::default(),
                    &[],
                    Some(FieldValue::uniform([1.0; 3]).unwrap()),
                    Seconds::ZERO,
                    1.0,
                    0.0,
                )
                .unwrap_err();
            assert!(error.contains("input take"));
            assert!(runtime.history.is_empty());
        }
    }

    #[test]
    fn fluid_shared_field_worker_matches_gravity_without_restarting() {
        const TICKS: u64 = 8;
        fn run(gravity: [f32; 3], field: Option<FieldValue>, stalled: bool) -> [f32; 3] {
            let settings = FluidSettings {
                // At 8³ this seed's reconstructed surface reaches every native
                // domain wall. Leave air around it so surface motion measures
                // acceleration instead of a boundary-clipped mesh.
                resolution: 12,
                fill_height: 0.0,
                initial_volume: Some(Transform {
                    pos: [0.0, 2.0, 0.0],
                    scale: [1.5; 3],
                    ..Transform::default()
                }),
                ..FluidSettings::default()
            };
            let controls = FluidControls {
                gravity,
                emission: false,
                obstacle_enabled: false,
                ..FluidControls::default()
            };
            let mut runtime = FluidRuntime::default();
            runtime
                .observe_scene_with_field(
                    settings,
                    controls,
                    &[],
                    field.clone(),
                    Seconds::ZERO,
                    1.0,
                    0.0,
                )
                .unwrap();
            runtime.advance(true).unwrap();
            let epoch = runtime.epoch;
            // Compare regular drains with the same history retained across a display stall.
            for tick in 1..=TICKS {
                runtime
                    .observe_scene_with_field(
                        settings,
                        controls,
                        &[],
                        field.clone(),
                        Seconds(tick as f64 * TICK),
                        1.0,
                        0.0,
                    )
                    .unwrap();
                if !stalled {
                    runtime.advance(true).unwrap();
                }
            }
            runtime.advance(true).unwrap();
            assert_eq!(runtime.epoch, epoch);
            assert_eq!(runtime.completed_tick, TICKS);
            assert!(!runtime.vertices.is_empty());
            std::array::from_fn(|axis| {
                runtime
                    .vertices
                    .iter()
                    .map(|v| v.position[axis])
                    .sum::<f32>()
                    / runtime.vertices.len() as f32
            })
        }
        let resting = run([0.0; 3], None, false);
        let gravity = run([6.0, 0.0, -4.0], None, false);
        let gravity_stalled = run([6.0, 0.0, -4.0], None, true);
        let field = run(
            [0.0; 3],
            Some(FieldValue::uniform([6.0, 0.0, -4.0]).unwrap()),
            false,
        );
        let field_stalled = run(
            [0.0; 3],
            Some(FieldValue::uniform([6.0, 0.0, -4.0]).unwrap()),
            true,
        );
        assert!(
            gravity[0] - resting[0] > 0.005 && resting[2] - gravity[2] > 0.005,
            "field must move the native fluid: resting={resting:?}, gravity={gravity:?}, field={field:?}"
        );
        for axis in 0..3 {
            assert!(
                (gravity[axis] - field[axis]).abs() < 0.005,
                "gravity {gravity:?} differs from equivalent field {field:?}"
            );
            assert!((gravity[axis] - gravity_stalled[axis]).abs() < 0.005);
            assert!((field[axis] - field_stalled[axis]).abs() < 0.005);
        }
    }

    #[test]
    fn fluid_controls_reject_nonfinite_gravity_component() {
        let mut controls = FluidControls::default();
        controls.gravity[0] = f32::NAN;
        assert!(controls.validate().is_err());
        controls.gravity = [0.0, f32::NEG_INFINITY, 0.0];
        assert!(controls.validate().is_err());
    }

    #[test]
    fn fluid_preview_publishes_bounded_batches_without_dropping_debt() {
        for display_fps in [5.0, 15.0, 24.0, 30.0, 60.0, 120.0] {
            let (request_sender, request_receiver) = mpsc::sync_channel::<Request>(1);
            let (reply_sender, reply_receiver) = mpsc::sync_channel::<Reply>(1);
            let mut runtime = FluidRuntime::default();
            runtime
                .observe(
                    FluidSettings::default(),
                    FluidControls::default(),
                    Seconds(0.0),
                    1.0,
                    0.0,
                )
                .unwrap();
            runtime.worker = Some(Worker {
                requests: request_sender,
                replies: reply_receiver,
                cancel_epoch: Arc::clone(&runtime.cancel_epoch),
            });
            runtime.advance(false).unwrap();
            let init = request_receiver.recv().unwrap();
            reply_sender
                .send(Reply {
                    outputs: Default::default(),
                    source_identity: None,
                    timing: Default::default(),
                    playback: None,
                    coupled: None,
                    started_tick: 0,
                    impulses: Vec::with_capacity(IMPULSE_CAPACITY),
                    epoch: init.epoch,
                    tick: init.start_tick + init.count as u64,
                    accepted_interval: None,
                    history: init.history,
                    role_history: init.role_history,
                    vertices: init.recycle,
                    whitewater: init.recycle_whitewater,
                    obstacle: init.initial.obstacle,
                    stats: FrameStats::default(),
                    error: None,
                })
                .unwrap();
            runtime.advance(false).unwrap();

            let frame_count = (4.0 * display_fps) as usize;
            let mut counts = Vec::new();
            for frame in 1..=frame_count {
                let elapsed = frame as f64 / display_fps;
                runtime
                    .observe(
                        FluidSettings::default(),
                        FluidControls::default(),
                        Seconds(elapsed),
                        1.0,
                        0.0,
                    )
                    .unwrap();
                runtime.advance(false).unwrap();
                loop {
                    match request_receiver.try_recv() {
                        Ok(request) => {
                            counts.push(request.count);
                            reply_sender
                                .send(Reply {
                                    outputs: Default::default(),
                                    source_identity: None,
                                    timing: Default::default(),
                                    playback: None,
                                    coupled: None,
                                    started_tick: 0,
                                    impulses: Vec::with_capacity(IMPULSE_CAPACITY),
                                    epoch: request.epoch,
                                    tick: request.start_tick + request.count as u64,
                                    accepted_interval: None,
                                    history: request.history,
                                    role_history: request.role_history,
                                    vertices: request.recycle,
                                    whitewater: request.recycle_whitewater,
                                    obstacle: request.initial.obstacle,
                                    stats: FrameStats::default(),
                                    error: None,
                                })
                                .unwrap();
                            runtime.advance(false).unwrap();
                        }
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => panic!("request channel disconnected"),
                    }
                }
            }
            assert_eq!(runtime.completed_tick, 240, "display FPS {display_fps}");
            assert!(counts.iter().all(|count| *count <= BATCH));
            if display_fps == 120.0 {
                assert_eq!(counts.first(), Some(&1));
            }
        }
    }

    #[test]
    fn fluid_whitewater_classifies_and_publishes_with_mesh_epoch() {
        let mut whitewater = WhitewaterFrame::default();
        whitewater.prepare(3);
        whitewater.fill(
            &[
                WhitewaterParticle {
                    position: [1.0, 0.5, 3.0],
                    velocity: [0.0; 3],
                    lifetime: 1.0,
                    kind: WhitewaterKind::Foam,
                },
                WhitewaterParticle {
                    position: [2.0, 0.4, 2.0],
                    velocity: [0.0; 3],
                    lifetime: 0.05,
                    kind: WhitewaterKind::Bubble,
                },
                WhitewaterParticle {
                    position: [3.0, 2.5, 1.0],
                    velocity: [0.0; 3],
                    lifetime: 0.0,
                    kind: WhitewaterKind::Spray,
                },
            ],
            FluidSettings::default().domain_layout().unwrap(),
        );
        assert_eq!(whitewater.foam[0].pos_scale, [-1.25, 0.25, 0.75, 1.0]);
        assert_eq!(whitewater.bubbles[0].pos_scale, [-0.25, 0.15, -0.25, 0.5]);
        assert_eq!(whitewater.spray[0].pos_scale, [0.75, 2.25, -1.25, 0.0]);
        let mut runtime = FluidRuntime::default();
        runtime
            .observe(
                FluidSettings::default(),
                FluidControls::default(),
                Seconds(0.0),
                1.0,
                0.0,
            )
            .unwrap();
        runtime
            .accept(Reply {
                outputs: Default::default(),
                source_identity: None,
                timing: Default::default(),
                playback: None,
                coupled: None,
                started_tick: 0,
                impulses: Vec::with_capacity(IMPULSE_CAPACITY),
                epoch: runtime.epoch,
                tick: 7,
                accepted_interval: None,
                history: Vec::new(),
                role_history: Vec::new(),
                vertices: Vec::new(),
                whitewater,
                obstacle: Transform::default(),
                stats: FrameStats::default(),
                error: None,
            })
            .unwrap();
        assert_eq!(runtime.completed_tick, 7);
        assert_eq!(runtime.whitewater.foam.len(), 1);
        assert_eq!(runtime.whitewater.bubbles.len(), 1);
        assert_eq!(runtime.whitewater.spray.len(), 1);
        let old_epoch = runtime.epoch;
        let stale = std::mem::take(&mut runtime.whitewater);
        runtime.clear();
        runtime
            .accept(Reply {
                outputs: Default::default(),
                source_identity: None,
                timing: Default::default(),
                playback: None,
                coupled: None,
                started_tick: 0,
                impulses: Vec::with_capacity(IMPULSE_CAPACITY),
                epoch: old_epoch,
                tick: 8,
                accepted_interval: None,
                history: Vec::new(),
                role_history: Vec::new(),
                vertices: Vec::new(),
                whitewater: stale,
                obstacle: Transform::default(),
                stats: FrameStats::default(),
                error: None,
            })
            .unwrap();
        assert_eq!(runtime.completed_tick, 0);
        assert!(runtime.whitewater.foam.is_empty());
        assert!(runtime.whitewater.bubbles.is_empty());
        assert!(runtime.whitewater.spray.is_empty());
    }

    #[test]
    fn fluid_preview_debt_and_offline_batches_reach_same_state() {
        let settings = FluidSettings {
            resolution: 12,
            fill_height: 0.8,
            ..FluidSettings::default()
        };
        let mut offline = FluidRuntime::default();
        let mut preview = FluidRuntime::default();
        let initial = FluidControls::default();
        for runtime in [&mut offline, &mut preview] {
            runtime
                .observe(settings, initial, Seconds(0.0), 1.0, 0.0)
                .unwrap();
            runtime.advance(true).unwrap();
        }

        let mut controls = Vec::with_capacity(12);
        for sample in 1..=12 {
            let seconds = sample as f64 * TICK;
            let mut next = initial;
            next.obstacle.pos[0] += (seconds * 4.0).sin() as f32 * 0.3;
            next.gravity[1] += (seconds * 2.0).cos() as f32;
            controls.push((seconds, next));
            for runtime in [&mut offline, &mut preview] {
                runtime
                    .observe(settings, next, Seconds(seconds), 1.0, 0.0)
                    .unwrap();
            }
        }

        // The reference consumes one tick at a time from the complete control
        // history, while the preview submits the same history as one hitch.
        offline.target_time = 0.0;
        for (index, _) in controls.iter().enumerate() {
            offline.target_time = (index + 1) as f64 * TICK;
            offline.advance(true).unwrap();
        }
        preview.advance(false).unwrap();
        preview.advance(true).unwrap();
        assert_eq!(offline.completed_tick, 12);
        assert_eq!(preview.completed_tick, offline.completed_tick);
        assert_eq!(preview.obstacle, offline.obstacle);
        assert!(preview.lag_seconds() < 1e-8);
        assert!(!preview.vertices.is_empty());
        assert_eq!(preview.stats.particles, offline.stats.particles);
        assert_eq!(preview.vertices.len(), offline.vertices.len());
        let worst = preview
            .vertices
            .iter()
            .zip(&offline.vertices)
            .flat_map(|(a, b)| {
                a.position
                    .iter()
                    .zip(b.position)
                    .map(|(a, b)| (a - b).abs())
            })
            .fold(0.0_f32, f32::max);
        assert!(
            worst < 1e-5,
            "frame partition changed the fluid surface: {worst}"
        );
    }

    #[test]
    fn fluid_mesh_outgrows_initial_allocation_without_reset_or_truncation() {
        let mut runtime = FluidRuntime::default();
        let settings = FluidSettings {
            resolution: 12,
            fill_height: 0.8,
            max_vertices: 3,
            ..FluidSettings::default()
        };
        runtime
            .observe(settings, FluidControls::default(), Seconds(0.0), 1.0, 0.0)
            .unwrap();
        runtime
            .observe(settings, FluidControls::default(), Seconds(TICK), 1.0, 0.0)
            .unwrap();
        runtime.advance(true).unwrap();
        assert!(runtime.vertices.len() > settings.max_vertices);
        assert_eq!(runtime.completed_tick, 1);
        runtime.advance(true).unwrap();
        assert_eq!(runtime.completed_tick, 1);
        runtime
            .observe(settings, FluidControls::default(), Seconds(TICK), 1.0, 1.0)
            .unwrap();
        runtime.advance(true).unwrap();
        assert_eq!(runtime.completed_tick, 0);
    }

    #[test]
    fn shallow_liquid_remains_present_at_low_resolution() {
        for resolution in [8, 16] {
            let settings = FluidSettings {
                resolution,
                fill_height: 0.16,
                ..FluidSettings::default()
            };
            let controls = FluidControls {
                emission: false,
                obstacle_enabled: false,
                ..FluidControls::default()
            };
            let mut runtime = FluidRuntime::default();
            runtime
                .observe(settings, controls, Seconds::ZERO, 1.0, 0.0)
                .unwrap();
            let mut initial_particles = 0;
            for tick in [1, 30, 120] {
                runtime
                    .observe(settings, controls, Seconds(tick as f64 * TICK), 1.0, 0.0)
                    .unwrap();
                runtime.advance(true).unwrap();
                if tick == 1 {
                    initial_particles = runtime.stats.particles;
                }
                assert!(
                    !runtime.vertices.is_empty(),
                    "empty surface at {resolution} cells, tick {tick}"
                );
                assert!(
                    runtime.stats.particles > 0
                        && runtime.stats.particles as f64 >= initial_particles as f64 * 0.95,
                    "shallow water lost at {resolution} cells, tick {tick}"
                );
            }
        }
    }

    #[test]
    fn fluid_clock_retains_time_pause_reset_and_backward_seek() {
        let _live = crate::node_graph::physics::PhysicsStepScope::for_render(false);
        let mut runtime = FluidRuntime::default();
        let settings = FluidSettings::default();
        let controls = FluidControls::default();
        runtime
            .observe(settings, controls, Seconds(10.0), 1.0, 0.0)
            .unwrap();
        runtime
            .observe(settings, controls, Seconds(10.25), 2.0, 0.0)
            .unwrap();
        assert!((runtime.target_time - 0.25).abs() < 1e-9);
        runtime
            .observe(settings, controls, Seconds(10.25), 2.0, 0.0)
            .unwrap();
        assert!((runtime.target_time - 0.25).abs() < 1e-9);
        runtime
            .observe(settings, controls, Seconds(10.5), 1.0, 1.0)
            .unwrap();
        assert_eq!(runtime.target_time, 0.0);
        runtime
            .observe(settings, controls, Seconds(10.6), 1.0, 1.0)
            .unwrap();
        assert!(
            (runtime.target_time - 0.1).abs() < 1e-9,
            "held trigger must not reset repeatedly"
        );
        let first_reset_epoch = runtime.epoch;
        runtime
            .observe(settings, controls, Seconds(10.7), 1.0, 2.0)
            .unwrap();
        assert_ne!(
            runtime.epoch, first_reset_epoch,
            "a second button press resets"
        );
        assert_eq!(runtime.target_time, 0.0);
        let second_reset_epoch = runtime.epoch;
        runtime
            .observe(settings, controls, Seconds(10.8), 1.0, 2.0)
            .unwrap();
        assert_eq!(runtime.epoch, second_reset_epoch);
        assert!((runtime.target_time - 0.1).abs() < 1e-9);
        runtime
            .observe(settings, controls, Seconds(10.8), 1.0, 1.0)
            .unwrap();
        assert_ne!(
            runtime.epoch, second_reset_epoch,
            "undo follows the same reset rule"
        );
        assert_eq!(runtime.target_time, 0.0);
        runtime
            .observe(settings, controls, Seconds(1.0), 1.0, 0.0)
            .unwrap();
        assert_eq!(runtime.target_time, 0.0);
    }

    /// WATER_SIMULATION_DESIGN.md "Transport pause / water speed zero": a held
    /// target publishes only the batch already in flight, never drains preview
    /// time debt, and discards impulses; moving again resumes from the held
    /// tick without a jump.
    #[test]
    fn fluid_held_transport_freezes_live_water_after_accepted_span() {
        let _live = crate::node_graph::physics::PhysicsStepScope::for_render(false);
        let settings = FluidSettings::default();
        let controls = FluidControls::default();
        let mut runtime = FluidRuntime::default();
        runtime.observe(settings, controls, Seconds(0.0), 1.0, 0.0).unwrap();
        runtime.advance(true).unwrap();
        assert!(runtime.initialized);
        // The live request carries the complete observed second.
        runtime.observe(settings, controls, Seconds(1.0), 1.0, 0.0).unwrap();
        runtime.advance(false).unwrap();
        assert!(runtime.busy);
        let start_tick = runtime.completed_tick;
        let held = |runtime: &FluidRuntime| {
            (
                runtime.version,
                runtime.completed_tick,
                runtime.busy,
                bytemuck::cast_slice::<MeshVertex, u8>(&runtime.vertices).to_vec(),
            )
        };
        // Wall-clock waits only bound a poll; they never decide an outcome.
        let wait_for_tick_change = |runtime: &mut FluidRuntime, transport: f64, speed: f32| {
            let from = runtime.completed_tick;
            let started = std::time::Instant::now();
            while runtime.completed_tick == from {
                assert!(
                    started.elapsed() < std::time::Duration::from_secs(120),
                    "in-flight batch never replied"
                );
                runtime
                    .observe(settings, controls, Seconds(transport), speed, 0.0)
                    .unwrap();
                runtime.advance(false).unwrap();
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        };
        // The batch in flight at pause covers played time, so it publishes;
        // nothing is requested after it.
        wait_for_tick_change(&mut runtime, 1.0, 1.0);
        assert_eq!(runtime.completed_tick, start_tick + 1);
        assert!((runtime.completed_time - 1.0).abs() < 1e-9);
        assert!(!runtime.busy, "held water requested more steps");
        let before = held(&runtime);
        let hold = |runtime: &mut FluidRuntime, transport: f64, speed: f32| {
            let started = std::time::Instant::now();
            while started.elapsed() < std::time::Duration::from_millis(1500) {
                runtime
                    .observe(settings, controls, Seconds(transport), speed, 0.0)
                    .unwrap();
                runtime.advance(false).unwrap();
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        };
        // Held water discards incoming events instead of bursting on resume.
        let strike = |runtime: &mut FluidRuntime, transport: f64, sequence: u64| {
            let stamp = runtime.impulse_stamp(Seconds(transport), sequence).unwrap();
            let field = manifold_physics::FieldValue::uniform([4.0, 0.0, 0.0]).unwrap();
            runtime.enqueue_impulse(stamp, field).unwrap();
        };
        hold(&mut runtime, 1.0, 1.0);
        assert!(before == held(&runtime), "paused water moved");
        strike(&mut runtime, 1.0, 0);
        // Simulation Speed 0 holds while the transport keeps running.
        hold(&mut runtime, 2.0, 0.0);
        // The first observation at speed zero completes the preceding speed-1
        // interval. Only subsequent held observations must remain unchanged.
        let after_transition = held(&runtime);
        hold(&mut runtime, 2.0, 0.0);
        assert!(after_transition == held(&runtime), "speed-zero water moved");
        assert!((runtime.target_time - 2.0).abs() < 1e-9, "held time adds no debt");
        strike(&mut runtime, 2.0, 1);
        assert_eq!(runtime.impulse_outstanding, 0, "held impulses are discarded");

        // Resume continues from the held tick one batch at a time.
        runtime
            .observe(settings, controls, Seconds(3.0), 1.0, 0.0)
            .unwrap();
        runtime.advance(false).unwrap();
        // The first resumed observation at transport 3.0 sees the held
        // speed-zero interval and remains at time 2.0. Transport 4.0 then
        // advances one live interval at speed 1.
        wait_for_tick_change(&mut runtime, 4.0, 1.0);
        assert_eq!(
            runtime.completed_tick,
            after_transition.1 + 1,
            "resume publishes one batch, not a jump"
        );
        assert!((runtime.completed_time - 3.0).abs() < 1e-9);
        assert_eq!(runtime.drain_applied_impulses().count(), 0, "no discarded impulse ran");
    }

    #[test]
    fn fluid_live_runtime_preserves_20_24_30_and_60_fps_endpoints() {
        let _live = crate::node_graph::physics::PhysicsStepScope::for_render(false);
        let settings = FluidSettings {
            resolution: 8,
            fill_height: 0.0,
            ..FluidSettings::default()
        };
        let controls = FluidControls::default();
        for fps in [20.0, 24.0, 30.0, 60.0] {
            let mut runtime = FluidRuntime::default();
            runtime.observe(settings, controls, Seconds::ZERO, 1.0, 0.0).unwrap();
            runtime.advance(true).unwrap();
            for frame in 1..=fps as u64 {
                let transport = frame as f64 / fps;
                runtime
                    .observe(settings, controls, Seconds(transport), 1.0, 0.0)
                    .unwrap();
                runtime.advance(false).unwrap();
                let started = std::time::Instant::now();
                while runtime.completed_time + 1e-9 < transport {
                    assert!(
                        started.elapsed() < std::time::Duration::from_secs(30),
                        "live fluid endpoint stalled at {transport}s for {fps}fps"
                    );
                    runtime
                        .observe(settings, controls, Seconds(transport), 1.0, 0.0)
                        .unwrap();
                    runtime.advance(false).unwrap();
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                assert!((runtime.completed_time - transport).abs() < 1e-9);
                assert_eq!(runtime.completed_tick, frame);
            }
        }
    }

    #[test]
    fn fluid_playback_keeps_legacy_address_and_reuses_worker_for_backward_seek() {
        let mut runtime = FluidRuntime::default();
        runtime
            .set_cache(CacheMode::Playback, "/tmp/fluid-playback-test")
            .unwrap();
        let settings = FluidSettings::default();
        let controls = FluidControls::default();
        runtime
            .observe(settings, controls, Seconds(2.0), 2.0, 0.0)
            .unwrap();
        assert!((runtime.target_time - 4.0).abs() < 1e-9);
        let epoch = runtime.epoch;
        runtime
            .observe(settings, controls, Seconds(1.0), 2.0, 0.0)
            .unwrap();
        assert_eq!(runtime.epoch, epoch);
        assert!((runtime.target_time - 2.0).abs() < 1e-9);
        // A speed edit changes the absolute cache address without seeking the
        // transport. It must not be rejected as backward authored input.
        runtime
            .observe(settings, controls, Seconds(1.1), 0.5, 0.0)
            .unwrap();
        assert!((runtime.target_time - 0.55).abs() < 1e-9);
        assert_eq!(runtime.history.len(), 1);
    }

    #[test]
    fn fluid_liquid_settings_change_restarts_the_worker_epoch() {
        let mut runtime = FluidRuntime::default();
        let settings = FluidSettings::default();
        let controls = FluidControls::default();
        runtime
            .observe(settings, controls, Seconds(0.0), 1.0, 0.0)
            .unwrap();
        let epoch = runtime.epoch;

        let changed = FluidSettings {
            liquid: LiquidOptions {
                viscosity: 0.25,
                surface_tension: 0.1,
            },
            ..settings
        };
        runtime
            .observe(changed, controls, Seconds(TICK), 1.0, 0.0)
            .unwrap();
        assert_ne!(runtime.epoch, epoch);
        assert_eq!(runtime.completed_tick, 0);
    }

    #[test]
    fn fluid_time_step_settings_default_validate_and_restart() {
        let defaults = FluidSettings::default();
        assert_eq!(defaults.time_steps, TimeStepOptions::default());
        assert!(defaults.validate().is_ok());

        for time_steps in [
            TimeStepOptions {
                min_substeps: 0,
                ..TimeStepOptions::default()
            },
            TimeStepOptions {
                max_substeps: 0,
                ..TimeStepOptions::default()
            },
            TimeStepOptions {
                cfl: 0,
                ..TimeStepOptions::default()
            },
            TimeStepOptions {
                min_substeps: 7,
                max_substeps: 6,
                ..TimeStepOptions::default()
            },
        ] {
            let invalid = FluidSettings {
                time_steps,
                ..defaults
            };
            assert!(invalid.validate().is_err());
        }

        let mut runtime = FluidRuntime::default();
        runtime
            .observe(defaults, FluidControls::default(), Seconds(0.0), 1.0, 0.0)
            .unwrap();
        let epoch = runtime.epoch;
        let changed = FluidSettings {
            time_steps: TimeStepOptions {
                adaptive_obstacles: true,
                ..defaults.time_steps
            },
            ..defaults
        };
        runtime
            .observe(changed, FluidControls::default(), Seconds(TICK), 1.0, 0.0)
            .unwrap();
        assert_ne!(runtime.epoch, epoch);
    }

    #[test]
    fn fluid_playback_loads_only_requested_ticks_and_reports_no_solver_work() {
        let directory = std::env::temp_dir().join(format!(
            "manifold-fluid-runtime-cache-{}",
            std::process::id()
        ));
        let settings = FluidSettings::default();
        let writer = CacheWriter::create(Arc::new(directory.clone()), settings).unwrap();
        let vertices = vec![bytemuck::Zeroable::zeroed(); 3];
        let pose = Transform {
            pos: [0.25, 0.5, 0.75],
            ..Transform::default()
        };
        let stats = FrameStats {
            particles: 17,
            triangles: 1,
            substeps: 1,
            simulation_ms: 12.0,
            meshing_ms: 6.0,
            ..FrameStats::default()
        };
        for tick in [2, 3, 4, 120] {
            writer
                .append(tick, &vertices, &WhitewaterFrame::default(), pose, stats)
                .unwrap();
        }
        let mut runtime = FluidRuntime::default();
        runtime
            .set_cache(CacheMode::Playback, directory.to_str().unwrap())
            .unwrap();
        for tick in [120, 2, 120] {
            runtime
                .observe(
                    settings,
                    FluidControls::default(),
                    Seconds(tick as f64 * TICK),
                    1.0,
                    0.0,
                )
                .unwrap();
            runtime.advance(true).unwrap();
            assert_eq!(runtime.completed_tick, tick);
            assert_eq!(runtime.vertices.len(), 3);
            assert_eq!(runtime.obstacle, pose);
            assert_eq!(runtime.stats.particles, 17);
            assert_eq!(runtime.stats.simulation_ms, 0.0);
            assert_eq!(runtime.stats.meshing_ms, 0.0);
        }
        // A positive absolute playback target still needs a request when a
        // speed change makes it earlier than the completed tick.
        runtime
            .observe(
                settings,
                FluidControls::default(),
                Seconds(2.0 * TICK),
                2.0,
                0.0,
            )
            .unwrap();
        runtime.advance(true).unwrap();
        assert_eq!(runtime.completed_tick, 4);
        runtime
            .observe(
                settings,
                FluidControls::default(),
                Seconds(3.0 * TICK),
                1.0,
                0.0,
            )
            .unwrap();
        runtime.advance(true).unwrap();
        assert_eq!(runtime.completed_tick, 3);
        runtime
            .observe(
                settings,
                FluidControls::default(),
                Seconds(5.0 * TICK),
                1.0,
                0.0,
            )
            .unwrap();
        assert!(runtime.advance(true).unwrap_err().contains("tick 5"));
        drop(runtime);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn fluid_record_bakes_every_tick_in_a_batch_without_accumulating_meshes() {
        let directory = std::env::temp_dir().join(format!(
            "manifold-fluid-record-batch-{}",
            std::process::id()
        ));
        let settings = FluidSettings {
            resolution: 8,
            ..FluidSettings::default()
        };
        let controls = FluidControls {
            emission: false,
            ..FluidControls::default()
        };
        let mut runtime = FluidRuntime::default();
        runtime
            .set_cache(CacheMode::Record, directory.to_str().unwrap())
            .unwrap();
        runtime
            .observe(settings, controls, Seconds(0.0), 1.0, 0.0)
            .unwrap();
        runtime.advance(true).unwrap();
        runtime
            .observe(settings, controls, Seconds(4.0 * TICK), 1.0, 0.0)
            .unwrap();
        runtime.advance(true).unwrap();
        let reader = CacheReader::open(Arc::new(directory.clone()), settings).unwrap();
        let mut vertices = Vec::new();
        let mut whitewater = WhitewaterFrame::default();
        for tick in 1..=4 {
            let (_, stats) = reader
                .read_into(tick, &mut vertices, &mut whitewater)
                .unwrap();
            assert_eq!(vertices.len(), stats.triangles as usize * 3);
        }
        assert_eq!(
            bytemuck::cast_slice::<_, u8>(&vertices),
            bytemuck::cast_slice::<_, u8>(&runtime.vertices)
        );
        drop(runtime);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn fluid_reset_discards_old_worker_reply() {
        let mut runtime = FluidRuntime::default();
        runtime
            .observe(
                FluidSettings::default(),
                FluidControls::default(),
                Seconds(0.0),
                1.0,
                0.0,
            )
            .unwrap();
        let old = runtime.epoch;
        let old_cancel = runtime.cancel_epoch.load(Ordering::Acquire);
        runtime.clear();
        assert_ne!(runtime.cancel_epoch.load(Ordering::Acquire), old_cancel);
        runtime.busy = true;
        runtime.spare = None;
        runtime.spare_history = None;
        runtime
            .accept(Reply {
                outputs: Default::default(),
                source_identity: None,
                timing: Default::default(),
                playback: None,
                coupled: None,
                started_tick: 0,
                impulses: Vec::with_capacity(IMPULSE_CAPACITY),
                epoch: old,
                tick: 123,
                accepted_interval: None,
                history: Vec::new(),
                role_history: Vec::new(),
                vertices: Vec::new(),
                whitewater: WhitewaterFrame::default(),
                obstacle: Transform::default(),
                stats: FrameStats::default(),
                error: Some("stale failure".into()),
            })
            .unwrap();
        assert_eq!(runtime.completed_tick, 0);
        assert!(!runtime.initialized);
        assert!(runtime.failure.is_none());
        assert!(runtime.spare.is_some());
        assert!(runtime.spare_whitewater.is_some());
        assert!(runtime.spare_history.is_some());
    }

    #[test]
    fn fluid_domain_snapshot_waits_for_current_accepted_reply() {
        let mut runtime = FluidRuntime::default();
        assert_eq!(
            runtime.domain_snapshot(),
            FluidDomainSnapshot {
                epoch: 0,
                state: FluidDomainState::Initializing,
                accepted_layout: None,
            }
        );
        runtime
            .observe(
                FluidSettings::default(),
                FluidControls::default(),
                Seconds(0.0),
                1.0,
                0.0,
            )
            .unwrap();
        let epoch = runtime.epoch;
        runtime
            .accept(Reply {
                outputs: Default::default(),
                source_identity: None,
                timing: Default::default(),
                playback: None,
                coupled: None,
                started_tick: 0,
                impulses: Vec::with_capacity(IMPULSE_CAPACITY),
                epoch: epoch.wrapping_add(1),
                tick: 1,
                accepted_interval: None,
                history: Vec::new(),
                role_history: Vec::new(),
                vertices: Vec::new(),
                whitewater: WhitewaterFrame::default(),
                obstacle: Transform::default(),
                stats: FrameStats::default(),
                error: None,
            })
            .unwrap();
        assert_eq!(
            runtime.domain_snapshot().state,
            FluidDomainState::Initializing
        );
        assert!(runtime.domain_snapshot().accepted_layout.is_none());

        runtime
            .accept(Reply {
                outputs: Default::default(),
                source_identity: None,
                timing: Default::default(),
                playback: None,
                coupled: None,
                started_tick: 0,
                impulses: Vec::with_capacity(IMPULSE_CAPACITY),
                epoch,
                tick: 1,
                accepted_interval: None,
                history: Vec::new(),
                role_history: Vec::new(),
                vertices: Vec::new(),
                whitewater: WhitewaterFrame::default(),
                obstacle: Transform::default(),
                stats: FrameStats::default(),
                error: None,
            })
            .unwrap();
        let snapshot = runtime.domain_snapshot();
        assert_eq!(snapshot.epoch, epoch);
        assert_eq!(snapshot.state, FluidDomainState::Ready);
        assert_eq!(
            snapshot.accepted_layout,
            Some(FluidSettings::default().domain_layout().unwrap())
        );

        runtime.clear();
        assert_eq!(
            runtime.domain_snapshot().state,
            FluidDomainState::Initializing
        );
        assert!(runtime.domain_snapshot().accepted_layout.is_none());
        runtime
            .accept(Reply {
                outputs: Default::default(),
                source_identity: None,
                timing: Default::default(),
                playback: None,
                coupled: None,
                started_tick: 0,
                impulses: Vec::with_capacity(IMPULSE_CAPACITY),
                epoch,
                tick: 2,
                accepted_interval: None,
                history: Vec::new(),
                role_history: Vec::new(),
                vertices: Vec::new(),
                whitewater: WhitewaterFrame::default(),
                obstacle: Transform::default(),
                stats: FrameStats::default(),
                error: None,
            })
            .unwrap();
        assert_eq!(
            runtime.domain_snapshot().state,
            FluidDomainState::Initializing
        );
        assert!(runtime.domain_snapshot().accepted_layout.is_none());
    }

    #[test]
    fn fluid_domain_snapshot_reports_runtime_failure_and_recovers_after_reset() {
        let mut runtime = FluidRuntime::default();
        runtime
            .observe(
                FluidSettings::default(),
                FluidControls::default(),
                Seconds(0.0),
                1.0,
                0.0,
            )
            .unwrap();
        let epoch = runtime.epoch;
        assert!(
            runtime
                .accept(Reply {
                    outputs: Default::default(),
                    source_identity: None,
                    timing: Default::default(),
                    playback: None,
                    coupled: None,
                    started_tick: 0,
                    impulses: Vec::with_capacity(IMPULSE_CAPACITY),
                    epoch,
                    tick: 0,
                    accepted_interval: None,
                    history: Vec::new(),
                    role_history: Vec::new(),
                    vertices: Vec::new(),
                    whitewater: WhitewaterFrame::default(),
                    obstacle: Transform::default(),
                    stats: FrameStats::default(),
                    error: Some("native setup failed".into()),
                })
                .is_err()
        );
        assert_eq!(runtime.domain_snapshot().state, FluidDomainState::Failed);
        assert!(runtime.domain_snapshot().accepted_layout.is_none());

        runtime.clear();
        assert_eq!(
            runtime.domain_snapshot().state,
            FluidDomainState::Initializing
        );
    }

    #[test]
    fn fluid_historical_controls_interpolate_and_reject_rotation() {
        let mut runtime = FluidRuntime::default();
        let settings = FluidSettings::default();
        let first = FluidControls::default();
        runtime
            .observe(settings, first, Seconds(0.0), 1.0, 0.0)
            .unwrap();
        let mut second = first;
        second.obstacle.pos[0] += 1.0;
        second.obstacle_enabled = false;
        second.emission = false;
        runtime
            .observe(settings, second, Seconds(1.0), 1.0, 0.0)
            .unwrap();
        let middle = FluidRuntime::controls_at(runtime.history.iter(), 0.5);
        assert!((middle.obstacle.pos[0] - first.obstacle.pos[0] - 0.5).abs() < 1e-6);
        assert!(middle.emission);
        assert!(middle.obstacle_enabled);
        assert!(!FluidRuntime::controls_at(runtime.history.iter(), 1.0).emission);
        assert!(!FluidRuntime::controls_at(runtime.history.iter(), 1.0).obstacle_enabled);
        second.obstacle.rot_euler[1] = 0.1;
        assert!(
            runtime
                .observe(settings, second, Seconds(1.1), 1.0, 0.0)
                .is_err()
        );
    }

    #[test]
    fn fluid_same_time_edit_keeps_unfinished_ramp_endpoint() {
        let mut runtime = FluidRuntime::default();
        let settings = FluidSettings::default();
        let initial = FluidControls::default();
        runtime
            .observe(settings, initial, Seconds(0.0), 1.0, 0.0)
            .unwrap();
        let epoch = runtime.epoch;
        let mut authored = initial;
        authored.gravity = [2.0, 0.0, 4.0];
        authored.obstacle.pos[0] = 1.0;
        runtime
            .observe(settings, authored, Seconds(1.0), 1.0, 0.0)
            .unwrap();
        authored.gravity = [-2.0, 10.0, -4.0];
        authored.obstacle.pos[0] = 2.0;
        runtime
            .observe(settings, authored, Seconds(1.0), 1.0, 0.0)
            .unwrap();
        assert_eq!(runtime.epoch, epoch);

        assert_eq!(runtime.history.len(), 3);
        assert_eq!(
            FluidRuntime::controls_at(runtime.history.iter(), 0.5).gravity,
            [1.0, -4.905, 2.0]
        );
        assert_eq!(
            FluidRuntime::controls_at(runtime.history.iter(), 1.0).gravity,
            [-2.0, 10.0, -4.0]
        );
        let samples: Vec<_> = runtime.history.iter().cloned().collect();
        assert_eq!(
            FluidRuntime::step_at(&samples, 59).next.gravity,
            [2.0, 0.0, 4.0]
        );
        assert_eq!(
            FluidRuntime::step_at(&samples, 60).current.gravity,
            [-2.0, 10.0, -4.0]
        );
        assert_eq!(
            FluidRuntime::step_at(&samples, 59).next.obstacle.pos[0],
            1.0
        );
        assert_eq!(
            FluidRuntime::step_at(&samples, 60).current.obstacle.pos[0],
            2.0
        );
    }

    #[test]
    fn fluid_lagging_history_grows_without_losing_unread_inputs() {
        let mut runtime = FluidRuntime::default();
        let settings = FluidSettings::default();
        let controls = FluidControls::default();
        for index in 0..HISTORY_CAPACITY + 32 {
            runtime
                .observe(settings, controls, Seconds(index as f64 * TICK), 1.0, 0.0)
                .unwrap();
        }
        assert_eq!(runtime.history.len(), HISTORY_CAPACITY + 32);
        assert_eq!(runtime.history.front().unwrap().time, 0.0);
        assert_eq!(runtime.target_time, (HISTORY_CAPACITY + 31) as f64 * TICK);
        assert_eq!(runtime.simulation_time(), 0.0);
        assert!(runtime.failure.is_none());
        runtime.clear();
        runtime
            .observe(settings, controls, Seconds(0.0), 1.0, 0.0)
            .unwrap();
        assert_eq!(runtime.history.len(), 1);
    }

    #[test]
    fn fluid_initial_volume_containment_is_independent_of_grid_padding() {
        for resolution in [8, 16, 64] {
            let domain = Transform {
                pos: [3.0, -2.0, 7.0],
                scale: [4.0; 3],
                ..Transform::default()
            };
            let mut settings = FluidSettings {
                resolution,
                domain: Some(domain),
                initial_volume: Some(domain),
                ..FluidSettings::default()
            };
            settings
                .validate()
                .expect("a volume touching authored domain faces is valid");
            for axis in 0..3 {
                for offset in [-0.01, 0.01] {
                    let mut outside = domain;
                    outside.pos[axis] += offset;
                    settings.initial_volume = Some(outside);
                    assert!(
                        settings.validate().unwrap_err().contains("contained"),
                        "resolution {resolution}, axis {axis}, offset {offset}"
                    );
                }
            }
        }
    }

    #[test]
    fn fluid_initial_volume_validation_rejects_rotation_and_out_of_domain_boxes() {
        let mut settings = FluidSettings {
            initial_volume: Some(Transform {
                rot_euler: [0.0, 0.1, 0.0],
                ..Transform::default()
            }),
            ..FluidSettings::default()
        };
        assert!(settings.validate().is_err());
        settings.initial_volume = Some(Transform {
            pos: [0.0, 0.5, 0.0],
            scale: [4.1, 1.0, 1.0],
            ..Transform::default()
        });
        assert!(settings.validate().is_err());
        for volume in [
            Transform {
                pos: [f32::NAN, 1.0, 0.0],
                ..Transform::default()
            },
            Transform {
                pos: [0.0, 1.0, 0.0],
                scale: [0.0, 1.0, 1.0],
                ..Transform::default()
            },
            Transform {
                pos: [0.0, 1.0, 0.0],
                billboard: true,
                ..Transform::default()
            },
        ] {
            settings.initial_volume = Some(volume);
            assert!(settings.validate().is_err());
        }
    }

    #[test]
    fn fluid_initial_volume_seeds_a_localized_column_without_emission() {
        let settings = FluidSettings {
            resolution: 12,
            fill_height: 0.0,
            initial_volume: Some(Transform {
                pos: [-1.0, 1.0, 0.0],
                scale: [0.8, 1.0, 1.0],
                ..Transform::default()
            }),
            ..FluidSettings::default()
        };
        let controls = FluidControls {
            gravity: [0.0; 3],
            emission: false,
            inflow_speed: 0.0,
            ..FluidControls::default()
        };
        let mut runtime = FluidRuntime::default();
        runtime
            .observe(settings, controls, Seconds(0.0), 1.0, 0.0)
            .expect("initial volume should validate");
        runtime
            .advance(true)
            .expect("initial volume should initialize");
        runtime
            .observe(settings, controls, Seconds(TICK), 1.0, 0.0)
            .unwrap();
        runtime.advance(true).expect("initial volume should step");
        assert!(runtime.stats.particles > 0);
        assert!(!runtime.vertices.is_empty());
        assert!(
            runtime
                .vertices
                .iter()
                .all(|vertex| vertex.position[0] < -0.1)
        );
        let epoch = runtime.epoch;
        let mut relocated = settings;
        relocated.initial_volume.as_mut().unwrap().pos[0] = 1.0;
        runtime
            .observe(relocated, controls, Seconds(2.0 * TICK), 1.0, 0.0)
            .unwrap();
        assert_ne!(runtime.epoch, epoch, "changing the seed restarts the world");
        runtime.advance(true).unwrap();
        runtime
            .observe(relocated, controls, Seconds(3.0 * TICK), 1.0, 0.0)
            .unwrap();
        runtime.advance(true).unwrap();
        assert!(!runtime.vertices.is_empty());
        assert!(
            runtime
                .vertices
                .iter()
                .all(|vertex| vertex.position[0] > 0.1)
        );
    }
    #[test]
    fn scene_physics_domain_translation_keeps_native_surface_coherent() {
        let delta = [8.0, -3.0, 5.0];
        let initial = Transform {
            pos: [-1.0, 1.0, 0.0],
            scale: [0.8, 1.0, 0.8],
            ..Transform::default()
        };
        let settings = FluidSettings {
            resolution: 12,
            domain: Some(Transform {
                pos: [0.0, 1.5, 0.0],
                scale: [4.0, 3.0, 2.0],
                ..Transform::default()
            }),
            fill_height: 0.0,
            initial_volume: Some(initial),
            ..FluidSettings::default()
        };
        let shifted = FluidSettings {
            domain: settings.domain.map(|mut pose| {
                for (position, offset) in pose.pos.iter_mut().zip(delta) {
                    *position += offset;
                }
                pose
            }),
            initial_volume: Some(Transform {
                pos: std::array::from_fn(|i| initial.pos[i] + delta[i]),
                ..initial
            }),
            ..settings
        };
        let mut base = FluidRuntime::default();
        let mut moved = FluidRuntime::default();
        for (runtime, setup) in [(&mut base, settings), (&mut moved, shifted)] {
            let controls = FluidControls {
                emission: false,
                obstacle_enabled: false,
                gravity: [0.0; 3],
                ..FluidControls::default()
            };
            runtime
                .observe(setup, controls, Seconds(0.0), 1.0, 0.0)
                .unwrap();
            runtime.advance(true).unwrap();
            runtime
                .observe(setup, controls, Seconds(TICK), 1.0, 0.0)
                .unwrap();
            runtime.advance(true).unwrap();
        }
        assert!(!base.vertices.is_empty());
        assert_eq!(base.stats.particles, moved.stats.particles);
        assert_eq!(base.vertices.len(), moved.vertices.len());
        for (a, b) in base.vertices.iter().zip(&moved.vertices) {
            for (i, offset) in delta.iter().enumerate() {
                assert!((b.position[i] - a.position[i] - offset).abs() < 2e-5);
            }
            assert_eq!(a.uv, b.uv);
        }
        let epoch = moved.epoch;
        moved
            .observe(settings, FluidControls::default(), Seconds(TICK), 1.0, 0.0)
            .unwrap();
        assert_ne!(moved.epoch, epoch, "domain edit starts a new world");
        assert!(
            moved.vertices.is_empty(),
            "old domain surface must not survive the reset"
        );
    }

    #[test]
    fn scene_physics_domain_setup_does_not_rewrite_historical_inputs() {
        let mut runtime = FluidRuntime::default();
        let before = FluidSettings::default();
        let after = FluidSettings {
            domain: Some(Transform {
                pos: [10.0, 2.0, 0.0],
                scale: [4.0; 3],
                ..Transform::default()
            }),
            ..before
        };
        let controls = FluidControls::default();
        runtime
            .observe(before, controls, Seconds(0.0), 1.0, 0.0)
            .unwrap();
        let epoch = runtime.epoch;
        {
            let _scope = super::super::physics::PhysicsAuthoredSampleScope::new();
            runtime
                .observe(after, controls, Seconds(TICK), 1.0, 0.0)
                .unwrap();
        }
        assert_eq!(runtime.epoch, epoch);
        assert_eq!(runtime.settings, Some(before));
        runtime
            .observe(after, controls, Seconds(2.0 * TICK), 1.0, 0.0)
            .unwrap();
        assert_ne!(runtime.epoch, epoch);
        assert_eq!(runtime.settings, Some(after));
    }

    #[test]
    fn fluid_display_time_never_passes_newest_tick() {
        let (t_a, t_b) = (3.0 * TICK, 5.0 * TICK);
        for step in 0..=80 {
            let s = step as f64 * TICK / 8.0;
            let (blend, span) = display_blend(s, t_a, t_b);
            assert!((0.0..=1.0).contains(&blend), "{s}: {blend}");
            assert!((f64::from(span) - 2.0 * TICK).abs() < 1e-6);
            // `span` is published as f32; allow its rounding, nothing more.
            let presented = t_a + f64::from(blend) * f64::from(span);
            assert!(presented <= t_b + 1e-6, "display time {presented} passed {t_b}");
            if s >= t_b {
                assert_eq!(blend, 1.0);
            }
            if s <= t_a {
                assert_eq!(blend, 0.0);
            }
        }
        // One frame (or no newer frame) presents that frame fully.
        assert_eq!(display_blend(10.0, 1.0, 1.0), (1.0, 0.0));
        // Offline, one tick per display frame: s = target − tick lands on A.
        let target = 7.0 * TICK;
        assert_eq!(display_blend(target - TICK, 6.0 * TICK, 7.0 * TICK).0, 0.0);
        // Half a tick later the display is halfway between the ticks.
        let (blend, _) = display_blend(target + 0.5 * TICK - TICK, 6.0 * TICK, 7.0 * TICK);
        assert!((blend - 0.5).abs() < 1e-5);
        // Without particle frames the runtime reports a full blend over no span.
        assert_eq!(FluidRuntime::default().particle_blend(), (1.0, 0.0));
    }

    #[test]
    fn fluid_engine_mesh_skipped_when_vertices_unconsumed() {
        let settings = FluidSettings {
            resolution: 12,
            ..FluidSettings::default()
        };
        let controls = FluidControls {
            emission: false,
            obstacle_enabled: false,
            ..FluidControls::default()
        };
        let run = |vertices: bool| {
            let mut runtime = FluidRuntime::default();
            runtime.set_outputs(false, vertices);
            for tick in 0..=4 {
                runtime
                    .observe(settings, controls, Seconds(tick as f64 * TICK), 1.0, 0.0)
                    .unwrap();
                runtime.advance(true).unwrap();
            }
            assert_eq!(runtime.completed_tick, 4);
            assert!(runtime.stats.particles > 0);
            runtime
        };
        let meshed = run(true);
        assert!(meshed.stats.triangles > 0);
        assert!(!meshed.vertices.is_empty());
        let skipped = run(false);
        assert_eq!(skipped.stats.triangles, 0);
        assert!(skipped.vertices.is_empty());
        assert_eq!(skipped.stats.particles, meshed.stats.particles);

        // Meshing is fixed before a world's first step: rewiring restarts it.
        let mut runtime = run(false);
        let epoch = runtime.epoch;
        runtime.set_outputs(false, false);
        assert_eq!(runtime.epoch, epoch, "an unchanged wiring keeps the world");
        runtime.set_outputs(true, false);
        assert_eq!(runtime.epoch, epoch, "particle outputs do not restart the world");
        runtime.set_outputs(true, true);
        assert_ne!(runtime.epoch, epoch);
    }

    #[test]
    fn physics_world_uncoupled_advances_while_fluid_worker_stalls() {
        use crate::node_graph::physics::{MAX_BODIES, RigidBody, RigidSimulation};
        // A fluid whose worker received a request and never answers.
        let (request_sender, request_receiver) = mpsc::sync_channel::<Request>(1);
        let (_reply_sender, reply_receiver) = mpsc::sync_channel::<Reply>(1);
        let mut fluid = FluidRuntime::default();
        let settings = FluidSettings::default();
        let controls = FluidControls::default();
        fluid.observe(settings, controls, Seconds(0.0), 1.0, 0.0).unwrap();
        fluid.worker = Some(Worker {
            requests: request_sender,
            replies: reply_receiver,
            cancel_epoch: Arc::clone(&fluid.cancel_epoch),
        });
        fluid.advance(false).unwrap();
        let _stalled = request_receiver.recv().unwrap();

        let mut bodies: [Option<RigidBody>; MAX_BODIES] = std::array::from_fn(|_| None);
        bodies[0] = Some(RigidBody {
            transform: Transform {
                pos: [0.0, 5.0, 0.0],
                ..Transform::default()
            },
            ..RigidBody::default()
        });
        let mut rigid = RigidSimulation::default();
        let started = std::time::Instant::now();
        for frame in 0..=30 {
            let time = Seconds(frame as f64 * TICK);
            fluid.observe(settings, controls, time, 1.0, 0.0).unwrap();
            fluid.advance(false).unwrap();
            rigid
                .advance(bodies.clone(), [0.0, -9.81, 0.0], time, 1.0, 0.0)
                .unwrap();
        }
        assert!(fluid.busy && !fluid.initialized, "the fluid worker never answered");
        assert_eq!(fluid.completed_tick, 0);
        let fallen = 5.0 - rigid.poses[0].pos[1];
        assert!(fallen > 0.5, "the uncoupled body fell {fallen} m in half a second");
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod particle_tests;
