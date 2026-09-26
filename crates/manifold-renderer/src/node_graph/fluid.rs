//! CPU FLIP reference runtime. Native state belongs exclusively to a worker;
//! the content thread retains bounded control history and immutable mesh frames.
use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};

use manifold_core::Seconds;
use manifold_fluids::{Bounds, Config, FluidWorld, FrameStats, SurfaceVertex};

use super::transform::Transform;
use crate::generators::mesh_common::MeshVertex;

pub const TICK: f64 = 1.0 / 60.0;
const HISTORY_CAPACITY: usize = 8192;
const BATCH: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FluidSettings {
    pub resolution: u32,
    pub domain_size: f32,
    pub fill_height: f32,
    pub surface_subdivisions: u32,
    pub apic: bool,
    pub max_vertices: usize,
}

impl Default for FluidSettings {
    fn default() -> Self {
        Self {
            resolution: 24,
            domain_size: 4.0,
            fill_height: 0.4,
            surface_subdivisions: 0,
            apic: false,
            max_vertices: 786432,
        }
    }
}

impl FluidSettings {
    pub fn validate(self) -> Result<(), String> {
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

struct Request {
    epoch: u64,
    settings: FluidSettings,
    initial: FluidControls,
    start_tick: u64,
    count: usize,
    steps: [Step; BATCH],
    recycle: Vec<MeshVertex>,
}

struct Reply {
    epoch: u64,
    tick: u64,
    vertices: Vec<MeshVertex>,
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
            while let Ok(mut request) = receiver.recv() {
                let mut stats = FrameStats::default();
                let mut pose = request.initial.obstacle;
                let result = (|| -> Result<(), String> {
                    if world.as_ref().is_none_or(|(epoch, _)| *epoch != request.epoch) {
                        // Dropping/rebuilding an old world can take time too; keep
                        // it off the content thread along with all native work.
                        world = None;
                        let mut new = FluidWorld::new(request.settings.config()).map_err(|e| e.to_string())?;
                        let size = request.settings.domain_size;
                        if request.settings.fill_height > 0.0 {
                            new.add_fluid_box(Bounds { min: [0.0; 3], max: [size, request.settings.fill_height, size] }, [0.0; 3])
                                .map_err(|e| e.to_string())?;
                        }
                        world = Some((request.epoch, new));
                    }
                    let native = &mut world.as_mut().expect("world initialized").1;
                    for step in request.steps.iter().take(request.count) {
                        native.set_gravity([0.0, step.current.gravity, 0.0]).map_err(|e| e.to_string())?;
                        native.set_emitter(request.settings.bounds(step.current.emitter),
                            [0.0, -step.current.inflow_speed, 0.0], step.current.emission).map_err(|e| e.to_string())?;
                        native.set_obstacle(request.settings.bounds(step.previous.obstacle),
                            request.settings.bounds(step.current.obstacle), request.settings.bounds(step.next.obstacle))
                            .map_err(|e| e.to_string())?;
                        stats = native.step(Seconds(TICK)).map_err(|e| e.to_string())?;
                        pose = step.next.obstacle;
                    }
                    request.recycle.clear();
                    if request.count > 0 {
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
                    }
                    Ok(())
                })();
                let reply = Reply { epoch: request.epoch, tick: request.start_tick + request.count as u64,
                    vertices: request.recycle, obstacle: pose, stats, error: result.err() };
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
    failure: Option<String>,
    pub vertices: Vec<MeshVertex>,
    pub version: u64,
    pub completed_tick: u64,
    pub obstacle: Transform,
    pub stats: FrameStats,
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
            failure: None,
            vertices: Vec::new(),
            version: 0,
            completed_tick: 0,
            obstacle: FluidControls::default().obstacle,
            stats: FrameStats::default(),
        }
    }
}

impl FluidRuntime {
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
        controls.validate()?;
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
        if let Some(previous) = self.last_transport {
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
            return Ok(());
        }
        if let Some(error) = reply.error {
            self.spare = Some(reply.vertices);
            self.failure = Some(error.clone());
            return Err(error);
        }
        self.spare = Some(std::mem::replace(&mut self.vertices, reply.vertices));
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
            let due = ((self.target_time / TICK + 1e-8).floor() as u64)
                .saturating_sub(self.completed_tick);
            if self.initialized && due == 0 {
                return Ok(());
            }
            let initial = self.history.front().expect("observed controls").controls;
            let mut steps = [Step {
                previous: initial,
                current: initial,
                next: initial,
            }; BATCH];
            let count = if self.initialized {
                due.min(if blocking { BATCH as u64 } else { 1 }) as usize
            } else {
                0
            };
            for (index, step) in steps.iter_mut().take(count).enumerate() {
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
                start_tick: self.completed_tick,
                count,
                steps,
                recycle: self.spare.take().expect("one recycled mesh per request"),
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
                obstacle: Transform::default(),
                stats: FrameStats::default(),
                error: Some("stale failure".into()),
            })
            .unwrap();
        assert_eq!(runtime.completed_tick, 0);
        assert!(!runtime.initialized);
        assert!(runtime.failure.is_none());
        assert!(runtime.spare.is_some());
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
}
