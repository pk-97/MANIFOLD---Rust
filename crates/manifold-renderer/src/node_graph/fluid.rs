//! CPU FLIP reference runtime. Native state belongs exclusively to a worker;
//! the content thread retains bounded control history and immutable mesh frames.
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};

use manifold_core::Seconds;
use manifold_fluids::{
    Bounds, Config, FluidWorld, FrameStats, SurfaceOptions, SurfaceVertex, WhitewaterKind,
    WhitewaterOptions, WhitewaterParticle,
};

use super::fluid_cache::{CacheMode, CacheReader, CacheWriter};
use super::transform::Transform;
use crate::generators::mesh_common::{InstanceTransform, MeshVertex};

pub const TICK: f64 = 1.0 / 60.0;
const HISTORY_CAPACITY: usize = 8192;
const BATCH: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FluidSettings {
    pub resolution: u32,
    pub domain_size: f32,
    pub fill_height: f32,
    pub initial_volume: Option<Transform>,
    pub surface_subdivisions: u32,
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
            fill_height: 0.4,
            initial_volume: None,
            surface_subdivisions: 0,
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
        self.surface.validate().map_err(|error| error.to_string())?;
        self.whitewater
            .validate()
            .map_err(|error| error.to_string())?;
        if self.whitewater.max_particles > 250_000 {
            return Err("Water: whitewater capacity must not exceed 250000 particles".into());
        }
        if !(8..=96).contains(&self.resolution)
            || !self.domain_size.is_finite()
            || !(0.5..=20.0).contains(&self.domain_size)
            || !self.fill_height.is_finite()
            || !(0.0..self.domain_size).contains(&self.fill_height)
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
            let bounds = self.bounds(volume);
            if bounds.min.iter().any(|v| *v < 0.0)
                || bounds.max.iter().any(|v| *v > self.domain_size)
            {
                return Err("Water: initial volume must be fully contained in the domain".into());
            }
        }
        Ok(())
    }

    fn config(self) -> Config {
        Config {
            cells: [self.resolution; 3],
            cell_size: self.domain_size as f64 / self.resolution as f64,
            surface_subdivisions: self.surface_subdivisions,
            apic: self.apic,
        }
    }

    fn bounds(self, pose: Transform) -> Bounds {
        let offset = [self.domain_size * 0.5, 0.0, self.domain_size * 0.5];
        Bounds {
            min: std::array::from_fn(|i| pose.pos[i] + offset[i] - 0.5 * pose.scale[i]),
            max: std::array::from_fn(|i| pose.pos[i] + offset[i] + 0.5 * pose.scale[i]),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FluidControls {
    pub emitter: Transform,
    pub obstacle: Transform,
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

    fn fill(&mut self, particles: &[WhitewaterParticle], half_domain: f32) {
        self.clear();
        for particle in particles {
            // Native lifetime is remaining time. Shrink the last 0.2 seconds
            // instead of leaving a full-sized particle until its removal.
            let fade = (particle.lifetime / 0.2).clamp(0.0, 1.0);
            let instance = InstanceTransform {
                pos_scale: [
                    particle.position[0] - half_domain,
                    particle.position[1],
                    particle.position[2] - half_domain,
                    fade.sqrt(),
                ],
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
    steps: [Step; BATCH],
    recycle: Vec<MeshVertex>,
    recycle_whitewater: WhitewaterFrame,
    cache_mode: CacheMode,
    cache_path: Arc<PathBuf>,
}

struct Reply {
    epoch: u64,
    tick: u64,
    vertices: Vec<MeshVertex>,
    whitewater: WhitewaterFrame,
    obstacle: Transform,
    stats: FrameStats,
    error: Option<String>,
}

struct Worker {
    requests: SyncSender<Request>,
    replies: Receiver<Reply>,
}

impl Worker {
    fn spawn() -> Result<Self, String> {
        let (requests, receiver) = mpsc::sync_channel::<Request>(1);
        let (sender, replies) = mpsc::sync_channel::<Reply>(1);
        std::thread::Builder::new().name("fluid-reference".into()).spawn(move || {
            let mut world: Option<(u64, FluidWorld)> = None;
            let mut surface = Vec::<SurfaceVertex>::new();
            let mut whitewater = Vec::<WhitewaterParticle>::new();
            let mut writer: Option<CacheWriter> = None;
            let mut playback: Option<CacheReader> = None;
            let mut cache_epoch = None;
            while let Ok(mut request) = receiver.recv() {
                let mut stats = FrameStats::default();
                let mut pose = request.initial.obstacle;
                let mut setup_error = None;
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
                            stats.simulation_ms = 0.0;
                            stats.meshing_ms = 0.0;
                        }
                        return Ok(());
                    }
                    if world.as_ref().is_none_or(|(epoch, _)| *epoch != request.epoch) {
                        // Dropping/rebuilding an old world can take time too; keep
                        // it off the content thread along with all native work.
                        world = None;
                        let mut new = FluidWorld::new(request.settings.config()).map_err(|e| e.to_string())?;
                        new.set_surface_options(request.settings.surface).map_err(|e| e.to_string())?;
                        new.set_whitewater_options(request.settings.whitewater).map_err(|e| e.to_string())?;
                        let size = request.settings.domain_size;
                        if request.settings.fill_height > 0.0 {
                            new.add_fluid_box(Bounds { min: [0.0; 3], max: [size, request.settings.fill_height, size] }, [0.0; 3])
                                .map_err(|e| e.to_string())?;
                        }
                        if let Some(volume) = request.settings.initial_volume {
                            new.add_fluid_box(request.settings.bounds(volume), [0.0; 3])
                                .map_err(|e| e.to_string())?;
                        }
                        world = Some((request.epoch, new));
                    }
                    request.recycle.clear();
                    request.recycle_whitewater.prepare(if request.settings.whitewater.enabled {
                        request.settings.whitewater.max_particles as usize
                    } else { 0 });
                    let native = &mut world.as_mut().expect("world initialized").1;
                    for (index, step) in request.steps.iter().take(request.count).enumerate() {
                        native.set_gravity([0.0, step.current.gravity, 0.0]).map_err(|e| e.to_string())?;
                        native.set_emitter(request.settings.bounds(step.current.emitter),
                            [0.0, -step.current.inflow_speed, 0.0], step.current.emission).map_err(|e| e.to_string())?;
                        native.set_obstacle(request.settings.bounds(step.previous.obstacle),
                            request.settings.bounds(step.current.obstacle), request.settings.bounds(step.next.obstacle))
                            .map_err(|e| e.to_string())?;
                        stats = native.step(Seconds(TICK)).map_err(|e| e.to_string())?;
                        pose = step.next.obstacle;
                        let last = index + 1 == request.count;
                        if request.cache_mode == CacheMode::Record || last {
                            native.surface(&mut surface).map_err(|e| e.to_string())?;
                            if surface.len() > request.settings.max_vertices {
                                return Err(format!("Water surface needs {} vertices; capacity is {}. Lower resolution/detail or increase mesh capacity and reset.", surface.len(), request.settings.max_vertices));
                            }
                            let half = request.settings.domain_size * 0.5;
                            request.recycle.extend(surface.iter().map(|v| MeshVertex {
                                position: [v.position[0] - half, v.position[1], v.position[2] - half],
                                _pad0: 0.0, normal: v.normal, _pad1: 0.0,
                                uv: [v.position[0] / request.settings.domain_size, v.position[2] / request.settings.domain_size],
                                _pad2: [0.0; 2], tangent: [0.0; 4],
                            }));
                            if request.settings.whitewater.enabled {
                                native.whitewater(&mut whitewater).map_err(|e| e.to_string())?;
                                if whitewater.len() > request.settings.whitewater.max_particles as usize {
                                    return Err("Water whitewater snapshot exceeds its configured capacity".into());
                                }
                                request.recycle_whitewater.fill(&whitewater, half);
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
                let reply = Reply { epoch: request.epoch, tick: request.start_tick + request.count as u64,
                    vertices: request.recycle, whitewater: request.recycle_whitewater,
                    obstacle: pose, stats, error: result.err() };
                if sender.send(reply).is_err() { break; }
            }
        }).map_err(|e| format!("Water worker could not start: {e}"))?;
        Ok(Self { requests, replies })
    }
}

pub struct FluidRuntime {
    worker: Option<Worker>,
    settings: Option<FluidSettings>,
    history: VecDeque<Sample>,
    last_transport: Option<f64>,
    previous_reset: f32,
    target_time: f64,
    epoch: u64,
    busy: bool,
    initialized: bool,
    spare: Option<Vec<MeshVertex>>,
    spare_whitewater: Option<WhitewaterFrame>,
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
            last_transport: None,
            previous_reset: 0.0,
            target_time: 0.0,
            epoch: 0,
            busy: false,
            initialized: false,
            spare: Some(Vec::new()),
            spare_whitewater: Some(WhitewaterFrame::default()),
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
        self.target_time = 0.0;
        self.completed_tick = 0;
        self.epoch = self.epoch.wrapping_add(1);
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
        settings.validate()?;
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
        let reset_edge = reset > 0.5 && self.previous_reset <= 0.5;
        self.previous_reset = reset;
        if self.settings != Some(settings)
            || reset_edge
            || self
                .last_transport
                .is_some_and(|previous| transport.0 < previous - 1e-9)
        {
            self.clear();
            self.settings = Some(settings);
            self.obstacle = controls.obstacle;
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
        Ok(())
    }

    fn prune_history(&mut self) {
        let retain_from = (self.simulation_time() - TICK).max(0.0);
        while self.history.len() > 2 && self.history[1].time < retain_from - 1e-10 {
            self.history.pop_front();
        }
    }

    fn at(&self, time: f64) -> FluidControls {
        let mut previous = *self.history.front().expect("observe before advance");
        for next in self.history.iter().skip(1) {
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

    fn accept(&mut self, reply: Reply) -> Result<(), String> {
        self.busy = false;
        if reply.epoch != self.epoch {
            self.spare = Some(reply.vertices);
            self.spare_whitewater = Some(reply.whitewater);
            return Ok(());
        }
        if let Some(error) = reply.error {
            self.spare = Some(reply.vertices);
            self.spare_whitewater = Some(reply.whitewater);
            self.failure = Some(error.clone());
            return Err(error);
        }
        self.spare = Some(std::mem::replace(&mut self.vertices, reply.vertices));
        self.spare_whitewater = Some(std::mem::replace(&mut self.whitewater, reply.whitewater));
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
            self.worker = Some(Worker::spawn()?);
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
            let mut steps = [Step {
                previous: initial,
                current: initial,
                next: initial,
            }; BATCH];
            let count = if self.cache_mode == CacheMode::Playback {
                usize::from(target_tick > 0)
            } else if self.initialized {
                due.min(if blocking { BATCH as u64 } else { 1 }) as usize
            } else {
                0
            };
            for (index, step) in steps
                .iter_mut()
                .take(if self.cache_mode == CacheMode::Playback {
                    0
                } else {
                    count
                })
                .enumerate()
            {
                let current = (self.completed_tick + index as u64) as f64 * TICK;
                *step = Step {
                    previous: self.at((current - TICK).max(0.0)),
                    current: self.at(current),
                    next: self.at(current + TICK),
                };
            }
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
                steps,
                recycle: self.spare.take().expect("one recycled mesh per request"),
                recycle_whitewater: self
                    .spare_whitewater
                    .take()
                    .expect("one recycled whitewater frame per request"),
                cache_mode: self.cache_mode,
                cache_path: self.cache_path.clone(),
            };
            self.worker
                .as_ref()
                .expect("worker exists")
                .requests
                .send(request)
                .map_err(|_| "Water worker disconnected".to_owned())?;
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
            2.0,
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
        for sample in 0..=32 {
            let seconds = sample as f64 / 240.0;
            let mut controls = FluidControls::default();
            controls.obstacle.pos[0] += (seconds * 4.0).sin() as f32 * 0.3;
            for runtime in [&mut offline, &mut preview] {
                runtime
                    .observe(settings, controls, Seconds(seconds), 1.0, 0.0)
                    .unwrap();
            }
            if sample % 4 == 0 {
                offline.advance(true).unwrap();
                // This only polls/submits. Native work may still be pending
                // when the next authored control sample arrives.
                preview.advance(false).unwrap();
            }
        }
        preview.advance(true).unwrap();
        assert_eq!(offline.completed_tick, 8);
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
        for tick in [2, 120] {
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
        runtime
            .observe(
                settings,
                FluidControls::default(),
                Seconds(3.0 * TICK),
                1.0,
                0.0,
            )
            .unwrap();
        assert!(runtime.advance(true).unwrap_err().contains("tick 3"));
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
        runtime.clear();
        runtime.busy = true;
        runtime.spare = None;
        runtime
            .accept(Reply {
                epoch: old,
                tick: 123,
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
        second.emission = false;
        runtime
            .observe(settings, second, Seconds(1.0), 1.0, 0.0)
            .unwrap();
        let middle = runtime.at(0.5);
        assert!((middle.obstacle.pos[0] - first.obstacle.pos[0] - 0.5).abs() < 1e-6);
        assert!(middle.emission);
        assert!(!runtime.at(1.0).emission);
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
}
