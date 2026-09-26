//! CPU FLIP reference runtime. Native state belongs exclusively to a worker;
//! the content thread retains bounded control history and immutable mesh frames.
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};

use manifold_core::Seconds;
use manifold_fluids::{
    Bounds, FluidWorld, FrameStats, LiquidOptions, SurfaceOptions, SurfaceVertex, TimeStepOptions,
    WhitewaterKind, WhitewaterOptions, WhitewaterParticle,
};

use super::fluid_cache::{CacheMode, CacheReader, CacheWriter};
use super::fluid_role::FluidRole;
use super::transform::Transform;
use crate::generators::mesh_common::{InstanceTransform, MeshVertex};

mod domain;
mod roles;
pub use domain::FluidDomainLayout;

pub const TICK: f64 = 1.0 / 60.0;
const HISTORY_CAPACITY: usize = 8192;
const BATCH: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FluidSettings {
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
            let bounds = domain.bounds(volume);
            if bounds.min.iter().any(|v| *v < 0.0)
                || bounds
                    .max
                    .iter()
                    .zip(domain.size)
                    .any(|(v, size)| *v > size)
            {
                return Err("Water: initial volume must be fully contained in the domain".into());
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FluidControls {
    pub emitter: Transform,
    pub obstacle: Transform,
    pub obstacle_enabled: bool,
    pub gravity: f32,
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
            gravity: -9.81,
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
        if !self.gravity.is_finite() || !self.inflow_speed.is_finite() || self.inflow_speed < 0.0 {
            return Err(
                "Water: gravity and inflow speed must be finite; inflow speed cannot be negative"
                    .into(),
            );
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct Sample {
    time: f64,
    controls: FluidControls,
}

#[derive(Clone, Copy)]
struct Step {
    previous: FluidControls,
    current: FluidControls,
    next: FluidControls,
}

fn request_count(
    cache_mode: CacheMode,
    blocking: bool,
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
    let due = if blocking { due.min(BATCH as u64) } else { due };
    usize::try_from(due).map_err(|_| "Water preview catch-up request is too large".to_owned())
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
            // Native lifetime is remaining time. Shrink the last 0.2 seconds
            // instead of leaving a full-sized particle until its removal.
            let fade = (particle.lifetime / 0.2).clamp(0.0, 1.0);
            let position = domain.to_scene(particle.position);
            let instance = InstanceTransform {
                pos_scale: [position[0], position[1], position[2], fade.sqrt()],
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

struct Request {
    epoch: u64,
    settings: FluidSettings,
    initial: FluidControls,
    start_tick: u64,
    count: usize,
    history: Vec<Sample>,
    role_setup: Arc<roles::Setup>,
    role_history: Vec<roles::Controls>,
    recycle: Vec<MeshVertex>,
    recycle_whitewater: WhitewaterFrame,
    cache_mode: CacheMode,
    cache_path: Arc<PathBuf>,
}

struct Reply {
    epoch: u64,
    tick: u64,
    history: Vec<Sample>,
    role_history: Vec<roles::Controls>,
    vertices: Vec<MeshVertex>,
    whitewater: WhitewaterFrame,
    obstacle: Transform,
    stats: FrameStats,
    error: Option<String>,
}

fn cancelled_reply(request: Request) -> Reply {
    Reply {
        epoch: request.epoch,
        tick: request.start_tick,
        history: request.history,
        role_history: request.role_history,
        vertices: request.recycle,
        whitewater: request.recycle_whitewater,
        obstacle: request.initial.obstacle,
        stats: FrameStats::default(),
        error: None,
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
        std::thread::Builder::new().name("fluid-reference".into()).spawn(move || {
            let mut world: Option<(u64, FluidWorld)> = None;
            let mut native_roles = roles::NativeRoles::default();
            let mut surface = Vec::<SurfaceVertex>::new();
            let mut whitewater = Vec::<WhitewaterParticle>::new();
            let mut writer: Option<CacheWriter> = None;
            let mut playback: Option<CacheReader> = None;
            let mut cache_epoch = None;
            while let Ok(mut request) = receiver.recv() {
                if worker_cancel_epoch.load(Ordering::Acquire) != request.epoch {
                    if sender.send(cancelled_reply(request)).is_err() {
                        break;
                    }
                    continue;
                }
                let mut stats = FrameStats::default();
                let mut pose = request.initial.obstacle;
                let mut setup_error = None;
                let mut completed_count = 0usize;
                if cache_epoch != Some(request.epoch) {
                    writer = None;
                    world = None;
                    playback = None;
                    cache_epoch = Some(request.epoch);
                    if setup_error.is_none() {
                        match request.cache_mode {
                            CacheMode::Live => {}
                            CacheMode::Record => match CacheWriter::create(
                                request.cache_path.clone(),
                                request.settings,
                            ) {
                                Ok(new_writer) => writer = Some(new_writer),
                                Err(error) => setup_error = Some(error),
                            },
                            CacheMode::Playback => match CacheReader::open(
                                request.cache_path.clone(),
                                request.settings,
                            ) {
                                Ok(reader) => playback = Some(reader),
                                Err(error) => setup_error = Some(error),
                            },
                        }
                    }
                }
                if worker_cancel_epoch.load(Ordering::Acquire) != request.epoch {
                    if sender.send(cancelled_reply(request)).is_err() {
                        break;
                    }
                    continue;
                }
                let result = (|| -> Result<(), String> {
                    if let Some(error) = setup_error {
                        return Err(error);
                    }
                    if request.cache_mode == CacheMode::Playback {
                        request.recycle.clear();
                        request.recycle_whitewater.clear();
                        if request.count > 0 {
                            let cache = playback.as_ref().expect("playback initialized");
                            (pose, stats) = cache.read_into(
                                request.start_tick + request.count as u64,
                                &mut request.recycle,
                                &mut request.recycle_whitewater,
                            )?;
                            completed_count = request.count;
                            stats.simulation_ms = 0.0;
                            stats.meshing_ms = 0.0;
                        }
                        return Ok(());
                    }
                    let domain = request.settings.domain_layout()?;
                    if world.as_ref().is_none_or(|(epoch, _)| *epoch != request.epoch) {
                        // Dropping/rebuilding an old world can take time too; keep
                        // it off the content thread along with all native work.
                        world = None;
                        let mut new = FluidWorld::new(domain.config(request.settings)).map_err(|e| e.to_string())?;
                        new.set_liquid_options(request.settings.liquid).map_err(|e| e.to_string())?;
                        new.set_time_step_options(request.settings.time_steps).map_err(|e| e.to_string())?;
                        new.set_surface_options(request.settings.surface).map_err(|e| e.to_string())?;
                        new.set_whitewater_options(request.settings.whitewater).map_err(|e| e.to_string())?;
                        new.set_boundary_collisions(request.settings.boundary_collisions).map_err(|e| e.to_string())?;
                        if request.settings.fill_height > 0.0 {
                            new.add_fluid_box(Bounds { min: [0.0; 3], max: [domain.size[0], request.settings.fill_height, domain.size[2]] }, [0.0; 3])
                                .map_err(|e| e.to_string())?;
                        }
                        if let Some(volume) = request.settings.initial_volume {
                            new.add_fluid_box(domain.bounds(volume), [0.0; 3])
                                .map_err(|e| e.to_string())?;
                        }
                        native_roles = roles::NativeRoles::prepare(&mut new, &request.role_setup, domain)?;
                        world = Some((request.epoch, new));
                    }
                    request.recycle.clear();
                    request.recycle_whitewater.prepare(if request.settings.whitewater.enabled {
                        request.settings.whitewater.max_particles as usize
                    } else { 0 });
                    let native = &mut world.as_mut().expect("world initialized").1;
                    for index in 0..request.count {
                        if worker_cancel_epoch.load(Ordering::Acquire) != request.epoch {
                            break;
                        }
                        let tick = request.start_tick + index as u64;
                        let step = FluidRuntime::step_at(&request.history, tick);
                        native.set_gravity([0.0, step.current.gravity, 0.0]).map_err(|e| e.to_string())?;
                        native.set_emitter(domain.bounds(step.current.emitter),
                            [0.0, -step.current.inflow_speed, 0.0], step.current.emission).map_err(|e| e.to_string())?;
                        if step.current.obstacle_enabled {
                            native.set_obstacle(domain.bounds(step.previous.obstacle),
                                domain.bounds(step.current.obstacle), domain.bounds(step.next.obstacle))
                                .map_err(|e| e.to_string())?;
                        } else {
                            native.clear_obstacle().map_err(|e| e.to_string())?;
                        }
                        native_roles.apply(native, &request.role_setup, &request.history,
                            &request.role_history, tick, domain)?;
                        stats = native.step(Seconds(TICK)).map_err(|e| e.to_string())?;
                        completed_count += 1;
                        pose = step.next.obstacle;
                        let last = index + 1 == request.count;
                        if request.cache_mode == CacheMode::Record || last {
                            native.surface(&mut surface).map_err(|e| e.to_string())?;
                            if surface.len() > request.settings.max_vertices {
                                return Err(format!("Water surface needs {} vertices; capacity is {}. Lower resolution/detail or increase mesh capacity and reset.", surface.len(), request.settings.max_vertices));
                            }
                            request.recycle.extend(surface.iter().map(|v| MeshVertex {
                                position: domain.to_scene(v.position),
                                _pad0: 0.0, normal: v.normal, _pad1: 0.0,
                                uv: [v.position[0] / domain.size[0], v.position[2] / domain.size[2]],
                                _pad2: [0.0; 2], tangent: [0.0; 4], color: [1.0; 4],
                            }));
                            if request.settings.whitewater.enabled {
                                native.whitewater(&mut whitewater).map_err(|e| e.to_string())?;
                                if whitewater.len() > request.settings.whitewater.max_particles as usize {
                                    return Err("Water whitewater snapshot exceeds its configured capacity".into());
                                }
                                request.recycle_whitewater.fill(&whitewater, domain);
                            }
                            if request.cache_mode == CacheMode::Record {
                                writer.as_ref().expect("record writer initialized").append(
                                    request.start_tick + index as u64 + 1,
                                    &request.recycle,
                                    &request.recycle_whitewater,
                                    pose,
                                    stats,
                                )?;
                            }
                            if !last {
                                request.recycle.clear();
                                request.recycle_whitewater.clear();
                            }
                        }
                    }
                    Ok(())
                })();
                let reply = Reply { epoch: request.epoch, tick: request.start_tick + completed_count as u64,
                    history: request.history,
                    role_history: request.role_history,
                    vertices: request.recycle, whitewater: request.recycle_whitewater,
                    obstacle: pose, stats, error: result.err() };
                if sender.send(reply).is_err() { break; }
            }
        }).map_err(|e| format!("Water worker could not start: {e}"))?;
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
    worker: Option<Worker>,
    settings: Option<FluidSettings>,
    history: VecDeque<Sample>,
    role_setup: Arc<roles::Setup>,
    role_history: roles::History,
    last_transport: Option<f64>,
    previous_reset: Option<f32>,
    target_time: f64,
    epoch: u64,
    cancel_epoch: Arc<AtomicU64>,
    busy: bool,
    initialized: bool,
    spare: Option<Vec<MeshVertex>>,
    spare_whitewater: Option<WhitewaterFrame>,
    spare_history: Option<Vec<Sample>>,
    spare_role_history: Option<Vec<roles::Controls>>,
    failure: Option<String>,
    pub vertices: Vec<MeshVertex>,
    pub whitewater: WhitewaterFrame,
    pub version: u64,
    pub completed_tick: u64,
    pub obstacle: Transform,
    pub stats: FrameStats,
    cache_mode: CacheMode,
    cache_path: Arc<PathBuf>,
}

impl Default for FluidRuntime {
    fn default() -> Self {
        Self {
            worker: None,
            settings: None,
            history: VecDeque::with_capacity(HISTORY_CAPACITY),
            role_setup: Arc::new(roles::Setup::default()),
            role_history: roles::History::default(),
            last_transport: None,
            previous_reset: None,
            target_time: 0.0,
            epoch: 0,
            cancel_epoch: Arc::new(AtomicU64::new(0)),
            busy: false,
            initialized: false,
            spare: Some(Vec::new()),
            spare_whitewater: Some(WhitewaterFrame::default()),
            spare_history: Some(Vec::with_capacity(HISTORY_CAPACITY)),
            spare_role_history: Some(Vec::new()),
            failure: None,
            vertices: Vec::new(),
            whitewater: WhitewaterFrame::default(),
            version: 0,
            completed_tick: 0,
            obstacle: FluidControls::default().obstacle,
            stats: FrameStats::default(),
            cache_mode: CacheMode::Live,
            cache_path: Arc::new(PathBuf::new()),
        }
    }
}

impl Drop for FluidRuntime {
    fn drop(&mut self) {
        self.cancel_epoch.fetch_add(1, Ordering::Release);
    }
}

impl FluidRuntime {
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

    pub fn clear(&mut self) {
        self.settings = None;
        self.last_transport = None;
        self.history.clear();
        self.role_history.clear();
        self.target_time = 0.0;
        self.completed_tick = 0;
        self.epoch = self.epoch.wrapping_add(1);
        self.cancel_epoch.store(self.epoch, Ordering::Release);
        self.initialized = false;
        self.failure = None;
        self.vertices.clear();
        self.whitewater.clear();
        self.stats = FrameStats::default();
        self.version = self.version.wrapping_add(1);
    }

    pub fn simulation_time(&self) -> f64 {
        self.completed_tick as f64 * TICK
    }
    pub fn lag_seconds(&self) -> f64 {
        (self.target_time - self.simulation_time()).max(0.0)
    }
    pub fn warmup_pending(&self) -> bool {
        self.busy && !self.initialized && self.failure.is_none()
    }

    /// Historical graph evaluations call only observe; native stepping and
    /// publication happen once in the real render frame.
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
        if self.cache_mode != CacheMode::Playback {
            controls.validate()?;
        }
        if !transport.0.is_finite()
            || !speed.is_finite()
            || !(0.0..=4.0).contains(&speed)
            || !reset.is_finite()
        {
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
            || reset_edge
            || self
                .last_transport
                .is_some_and(|previous| transport.0 < previous - 1e-9)
        {
            self.clear();
            self.settings = Some(settings);
            self.obstacle = controls.obstacle;
        }
        if role_topology_changed {
            self.role_setup = Arc::new(roles::Setup::new(scene_roles));
            self.role_history.prepare(self.role_setup.len());
        }
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
        if self.cache_mode == CacheMode::Playback {
            self.target_time = (transport.0 * speed as f64).max(0.0);
        } else if let Some(previous) = self.last_transport {
            self.target_time += (transport.0 - previous).max(0.0) * speed as f64;
        }
        self.last_transport = Some(transport.0);
        if let Some(last) = self.history.back_mut()
            && (last.time - self.target_time).abs() < 1e-10
        {
            last.controls = controls;
            self.role_history
                .observe(&self.role_setup, scene_roles, true);
            return Ok(());
        }
        self.prune_history();
        if self.history.len() == HISTORY_CAPACITY {
            let error = "Water preview is too far behind to retain its control history. Lower resolution and reset.".to_owned();
            self.failure = Some(error.clone());
            return Err(error);
        }
        self.history.push_back(Sample {
            time: self.target_time,
            controls,
        });
        self.role_history
            .observe(&self.role_setup, scene_roles, false);
        Ok(())
    }

    pub fn hold_pending(&mut self, transport: Seconds) {
        self.last_transport = Some(transport.0);
    }

    fn prune_history(&mut self) {
        let retain_from = (self.simulation_time() - TICK).max(0.0);
        while self.history.len() > 2 && self.history[1].time < retain_from - 1e-10 {
            self.history.pop_front();
            self.role_history.pop_front();
        }
    }

    fn controls_at<'a>(mut history: impl Iterator<Item = &'a Sample>, time: f64) -> FluidControls {
        let mut previous = *history.next().expect("observe before advance");
        for next in history {
            if next.time >= time {
                let alpha = if next.time > previous.time {
                    ((time - previous.time) / (next.time - previous.time)).clamp(0.0, 1.0) as f32
                } else {
                    1.0
                };
                let interpolate = |a: Transform, b: Transform| Transform {
                    pos: std::array::from_fn(|i| a.pos[i] + alpha * (b.pos[i] - a.pos[i])),
                    scale: std::array::from_fn(|i| a.scale[i] + alpha * (b.scale[i] - a.scale[i])),
                    ..a
                };
                // Continuous pose/force values interpolate; switches are held
                // until their exact authored time rather than smeared in time.
                return FluidControls {
                    emitter: interpolate(previous.controls.emitter, next.controls.emitter),
                    obstacle: interpolate(previous.controls.obstacle, next.controls.obstacle),
                    obstacle_enabled: if alpha >= 1.0 {
                        next.controls.obstacle_enabled
                    } else {
                        previous.controls.obstacle_enabled
                    },
                    gravity: previous.controls.gravity
                        + alpha * (next.controls.gravity - previous.controls.gravity),
                    inflow_speed: previous.controls.inflow_speed
                        + alpha * (next.controls.inflow_speed - previous.controls.inflow_speed),
                    emission: if alpha >= 1.0 {
                        next.controls.emission
                    } else {
                        previous.controls.emission
                    },
                };
            }
            previous = *next;
        }
        previous.controls
    }

    fn step_at(history: &[Sample], tick: u64) -> Step {
        let current = tick as f64 * TICK;
        Step {
            previous: Self::controls_at(history.iter(), (current - TICK).max(0.0)),
            current: Self::controls_at(history.iter(), current),
            next: Self::controls_at(history.iter(), current + TICK),
        }
    }

    fn accept(&mut self, reply: Reply) -> Result<(), String> {
        self.busy = false;
        self.spare_role_history = Some(reply.role_history);
        if reply.epoch != self.epoch {
            self.spare = Some(reply.vertices);
            self.spare_whitewater = Some(reply.whitewater);
            self.spare_history = Some(reply.history);
            return Ok(());
        }
        if let Some(error) = reply.error {
            self.spare = Some(reply.vertices);
            self.spare_whitewater = Some(reply.whitewater);
            self.spare_history = Some(reply.history);
            self.failure = Some(error.clone());
            return Err(error);
        }
        self.spare = Some(std::mem::replace(&mut self.vertices, reply.vertices));
        self.spare_whitewater = Some(std::mem::replace(&mut self.whitewater, reply.whitewater));
        self.spare_history = Some(reply.history);
        self.completed_tick = reply.tick;
        self.obstacle = reply.obstacle;
        self.stats = reply.stats;
        self.initialized = true;
        self.version = self.version.wrapping_add(1);
        self.prune_history();
        Ok(())
    }

    pub fn advance(&mut self, blocking: bool) -> Result<(), String> {
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
            let target_tick = (self.target_time / TICK + 1e-8).floor() as u64;
            let due = target_tick.saturating_sub(self.completed_tick);
            if self.initialized
                && (if self.cache_mode == CacheMode::Playback {
                    target_tick == self.completed_tick
                } else {
                    due == 0
                })
            {
                return Ok(());
            }
            let initial = self.history.front().expect("observed controls").controls;
            let count = request_count(
                self.cache_mode,
                blocking,
                due,
                target_tick,
                self.initialized,
            )?;
            let mut history = self
                .spare_history
                .take()
                .expect("one recycled history snapshot per request");
            history.clear();
            history.extend(self.history.iter().copied());
            let mut role_history = self
                .spare_role_history
                .take()
                .expect("one recycled role history per request");
            self.role_history.snapshot(&mut role_history);
            let request = Request {
                epoch: self.epoch,
                settings,
                initial,
                start_tick: if self.cache_mode == CacheMode::Playback {
                    target_tick.saturating_sub(count as u64)
                } else {
                    self.completed_tick
                },
                count,
                history,
                role_setup: Arc::clone(&self.role_setup),
                role_history,
                recycle: self.spare.take().expect("one recycled mesh per request"),
                recycle_whitewater: self
                    .spare_whitewater
                    .take()
                    .expect("one recycled whitewater frame per request"),
                cache_mode: self.cache_mode,
                cache_path: self.cache_path.clone(),
            };
            let worker = self.worker.as_ref().expect("worker exists");
            if let Err(error) = worker.requests.send(request) {
                let request = error.0;
                self.spare = Some(request.recycle);
                self.spare_whitewater = Some(request.recycle_whitewater);
                self.spare_history = Some(request.history);
                self.spare_role_history = Some(request.role_history);
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

    #[test]
    fn fluid_preview_scheduler_requests_all_due_ticks_across_display_rates() {
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
                    epoch: init.epoch,
                    tick: init.start_tick + init.count as u64,
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
                match request_receiver.try_recv() {
                    Ok(request) => {
                        counts.push(request.count);
                        reply_sender
                            .send(Reply {
                                epoch: request.epoch,
                                tick: request.start_tick + request.count as u64,
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
                    Err(TryRecvError::Empty) => {}
                    Err(TryRecvError::Disconnected) => panic!("request channel disconnected"),
                }
            }
            assert_eq!(runtime.completed_tick, 240, "display FPS {display_fps}");
            assert!(counts.iter().all(|count| *count <= 60));
            if display_fps == 120.0 {
                assert_eq!(counts.first(), Some(&1));
            }
        }

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
                epoch: init.epoch,
                tick: init.start_tick,
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
        runtime
            .observe(
                FluidSettings::default(),
                FluidControls::default(),
                Seconds(17.0 * TICK),
                1.0,
                0.0,
            )
            .unwrap();
        runtime.advance(false).unwrap();
        let hitch = request_receiver.recv().unwrap();
        assert_eq!(hitch.count, 17);
        reply_sender
            .send(Reply {
                epoch: hitch.epoch,
                tick: hitch.start_tick + hitch.count as u64,
                history: hitch.history,
                role_history: hitch.role_history,
                vertices: hitch.recycle,
                whitewater: hitch.recycle_whitewater,
                obstacle: hitch.initial.obstacle,
                stats: FrameStats::default(),
                error: None,
            })
            .unwrap();
        runtime.advance(false).unwrap();
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
        assert_eq!(whitewater.foam[0].pos_scale, [-1.0, 0.5, 1.0, 1.0]);
        assert_eq!(whitewater.bubbles[0].pos_scale, [0.0, 0.4, 0.0, 0.5]);
        assert_eq!(whitewater.spray[0].pos_scale, [1.0, 2.5, -1.0, 0.0]);
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
                epoch: runtime.epoch,
                tick: 7,
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
                epoch: old_epoch,
                tick: 8,
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
            next.gravity += (seconds * 2.0).cos() as f32;
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
    fn fluid_mesh_overflow_fails_until_reset_instead_of_truncating() {
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
        assert!(runtime.advance(true).unwrap_err().contains("capacity"));
        assert!(runtime.vertices.is_empty());
        assert!(runtime.advance(true).is_err());
        runtime
            .observe(settings, FluidControls::default(), Seconds(TICK), 1.0, 1.0)
            .unwrap();
        runtime.advance(true).unwrap();
        assert_eq!(runtime.completed_tick, 0);
    }

    #[test]
    fn fluid_clock_retains_time_pause_reset_and_backward_seek() {
        let mut runtime = FluidRuntime::default();
        let settings = FluidSettings::default();
        let controls = FluidControls::default();
        runtime
            .observe(settings, controls, Seconds(10.0), 1.0, 0.0)
            .unwrap();
        runtime
            .observe(settings, controls, Seconds(10.25), 2.0, 0.0)
            .unwrap();
        assert!((runtime.target_time - 0.5).abs() < 1e-9);
        runtime
            .observe(settings, controls, Seconds(10.25), 2.0, 0.0)
            .unwrap();
        assert!((runtime.target_time - 0.5).abs() < 1e-9);
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

    #[test]
    fn fluid_playback_uses_absolute_transport_and_supports_backward_seek() {
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
        assert_ne!(runtime.epoch, epoch);
        assert!((runtime.target_time - 2.0).abs() < 1e-9);
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
                epoch: old,
                tick: 123,
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
            gravity: 0.0,
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
                gravity: 0.0,
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
}
