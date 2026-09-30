//! The liquid conformance suite on the GPU (`docs/LIQUID_SOLVER_SEAM_DESIGN.md`
//! section 4 (Invariants & enforcement)): I4–I8, I11, I13, Speed 0.5 and
//! Reset, run for every row of `LIQUID_SOLVERS` that names no exemption, on
//! the row's own scenes. A check reads only what every liquid shares: the
//! domain's clock and body outputs, the particle frame, the Box3D pose, and
//! the row's totals and state readouts. Every run renders offline unless it
//! says live, and waits for the GPU after each frame.

use std::borrow::Cow;
use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use manifold_core::effect_graph_def::{BindingTarget, EffectGraphDef, EffectGraphNode, EffectGraphWire};
use manifold_core::liquid_domain::is_liquid_domain;
use manifold_core::params::{Param, ParamManifest};
use manifold_core::preset_def::PresetKind;
use manifold_gpu::{FrameClock, GpuDevice, GpuEvent, GpuTextureFormat, RetireMark, RetireQueue};
use manifold_renderer::frame_status::FrameRenderStatus;
use manifold_renderer::gpu_encoder::GpuEncoder;
use manifold_renderer::node_graph::fluid::TICK;
use manifold_renderer::node_graph::fluid_particles::FluidParticle;
use manifold_renderer::node_graph::liquid::bodies::LiquidBody;
use manifold_renderer::node_graph::liquid::conformance::{
    BoxScene, Check, FIXTURE_DENSITY, Fixture, LIQUID_SOLVERS, LiquidSolverRow, LiquidTotals,
};
use manifold_renderer::node_graph::physics::{PhysicsStepScope, native_ticks_on_this_thread};
use manifold_renderer::node_graph::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType, ScalarType};
use manifold_renderer::node_graph::{
    ArrayType, EffectNode, EffectNodeContext, EffectNodeType, NodeErrorTap, ParamDef, PrimitiveRegistry, Transform,
    bundled_preset_def, bundled_preset_type_ids,
};
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;
use manifold_renderer::preset_thumbnail::{THUMBNAIL_HEIGHT, THUMBNAIL_WIDTH, render_preset_thumbnail};
use manifold_renderer::render_target::RenderTarget;
use serde_json::json;

use crate::harness;

const PROBE_TYPE: &str = "test.liquid_probe";
const SIZE: u32 = 64;
/// Box3D's gravity in every fixture, m/s².
const G: f64 = 9.81;

/// Domain scalars the probe records, where the domain publishes them.
const DOMAIN_SCALARS: [&str; 5] = ["simulation_time", "display_time", "ticks", "epoch", "body_count"];
/// The particle frame's scalars (GPU_FLUID_SURFACE_DESIGN.md section 3 (The
/// particle-frame contract)).
const FRAME_SCALARS: [&str; 6] = ["count_a", "count_b", "identity_a", "identity_b", "blend", "span"];
const SCALARS: usize = DOMAIN_SCALARS.len() + FRAME_SCALARS.len();

/// What one frame published: the drawn Box3D pose and the probed scalars
/// (NaN where nothing is wired).
#[derive(Clone, Copy, Debug)]
struct Probe {
    pose: Option<Transform>,
    scalars: [f32; SCALARS],
}

impl Probe {
    const EMPTY: Self = Self { pose: None, scalars: [f32::NAN; SCALARS] };

    fn get(&self, name: &str) -> f32 {
        let index = DOMAIN_SCALARS
            .iter()
            .chain(&FRAME_SCALARS)
            .position(|probed| *probed == name)
            .unwrap_or_else(|| panic!("{name} is not probed"));
        self.scalars[index]
    }

    /// The scalars as bits, every one but the frame's tick count.
    fn held_bits(&self) -> Vec<u32> {
        DOMAIN_SCALARS
            .iter()
            .chain(&FRAME_SCALARS)
            .zip(self.scalars)
            .filter(|(name, _)| **name != "ticks")
            .map(|(_, value)| value.to_bits())
            .collect()
    }
}

thread_local! {
    static PROBE: Cell<Probe> = const { Cell::new(Probe::EMPTY) };
}

/// Records whichever of its inputs are wired. Its particle inputs keep both
/// frames allocated, so the array dump carries them.
struct LiquidProbe {
    type_id: EffectNodeType,
    inputs: Vec<NodeInput>,
}

impl LiquidProbe {
    fn new() -> Self {
        let port = |name: &'static str, ty: PortType| NodePort { name: Cow::Borrowed(name), ty, kind: PortKind::Input, required: false };
        let mut inputs: Vec<NodeInput> = DOMAIN_SCALARS
            .iter()
            .chain(&FRAME_SCALARS)
            .map(|&name| port(name, PortType::Scalar(ScalarType::F32)))
            .collect();
        inputs.push(port("pose", PortType::Transform));
        for name in ["particles_a", "particles_b"] {
            inputs.push(port(name, PortType::Array(ArrayType::of_known::<FluidParticle>())));
        }
        Self { type_id: EffectNodeType::new(PROBE_TYPE), inputs }
    }
}

impl EffectNode for LiquidProbe {
    fn is_liveness_root(&self) -> bool {
        true
    }

    fn type_id(&self) -> &EffectNodeType {
        &self.type_id
    }

    fn depth_rule(&self) -> manifold_renderer::node_graph::depth_rule::DepthRule {
        manifold_renderer::node_graph::depth_rule::DepthRule::Terminal
    }

    fn inputs(&self) -> &[NodeInput] {
        &self.inputs
    }

    fn outputs(&self) -> &[NodeOutput] {
        &[]
    }

    fn parameters(&self) -> &[ParamDef] {
        &[]
    }

    fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let mut probe = Probe::EMPTY;
        probe.pose = ctx.inputs.transform("pose");
        for (slot, name) in DOMAIN_SCALARS.iter().chain(&FRAME_SCALARS).enumerate() {
            if let Some(value) = ctx.inputs.scalar(name).and_then(|value| value.as_scalar()) {
                probe.scalars[slot] = value;
            }
        }
        PROBE.set(probe);
    }
}

fn registry() -> PrimitiveRegistry {
    let mut registry = PrimitiveRegistry::with_builtin();
    registry.register(PROBE_TYPE, || Box::new(LiquidProbe::new()));
    registry
}

/// A row's scene, flattened, with the probe wired to the domain, the particle
/// frame and the Box3D world's first body. `dry` removes the domain and
/// everything downstream of it up to the render, which draws the scene
/// without the liquid, leaving Box3D alone.
struct Prepared {
    def: EffectGraphDef,
    /// Domain param → the card bound to it.
    cards: Vec<(String, String)>,
    publisher: String,
}

fn prepare(row: &LiquidSolverRow, def: &EffectGraphDef, registry: &PrimitiveRegistry, dry: bool) -> Prepared {
    let mut def = manifold_core::flatten::flatten_groups(def).expect("a liquid scene flattens");
    let outputs = |type_id: &str| -> Vec<String> {
        registry
            .construct(type_id)
            .map(|node| node.outputs().iter().map(|port| port.name.to_string()).collect())
            .unwrap_or_default()
    };
    let domains: Vec<&EffectGraphNode> = def.nodes.iter().filter(|node| node.type_id == row.type_id).collect();
    assert_eq!(domains.len(), 1, "{}: the scene holds {} domains", row.type_id, domains.len());
    let (domain, domain_node) = (domains[0].id, domains[0].node_id.as_str().to_owned());
    let domain_outputs = outputs(row.type_id);
    let publisher = def
        .nodes
        .iter()
        .find(|node| outputs(&node.type_id).iter().any(|port| port == "particles_b"))
        .unwrap_or_else(|| panic!("{}: no node publishes a particle frame", row.type_id));
    let (publisher_id, publisher_type) = (publisher.id, publisher.type_id.clone());
    let world = def.nodes.iter().find(|node| node.type_id == "node.physics_world").map(|node| node.id);
    let cards = def
        .preset_metadata
        .iter()
        .flat_map(|metadata| &metadata.bindings)
        .filter_map(|binding| match &binding.target {
            BindingTarget::Node { node_id, param } if node_id.as_str() == domain_node => {
                Some((param.clone(), binding.id.clone()))
            }
            _ => None,
        })
        .collect();
    if dry {
        let renders: Vec<u32> = def.nodes.iter().filter(|node| node.type_id == "node.render_scene").map(|node| node.id).collect();
        let mut removed = vec![domain];
        let mut next = 0;
        while next < removed.len() {
            let from = removed[next];
            next += 1;
            for wire in def.wires.iter().filter(|wire| wire.from_node == from) {
                if !removed.contains(&wire.to_node) && !renders.contains(&wire.to_node) {
                    removed.push(wire.to_node);
                }
            }
        }
        def.nodes.retain(|node| !removed.contains(&node.id));
        let kept: Vec<u32> = def.nodes.iter().map(|node| node.id).collect();
        def.wires.retain(|wire| kept.contains(&wire.from_node) && kept.contains(&wire.to_node));
        let present: Vec<String> = def.nodes.iter().map(|node| node.node_id.as_str().to_owned()).collect();
        if let Some(metadata) = def.preset_metadata.as_mut() {
            metadata.bindings.retain(|binding| match &binding.target {
                BindingTarget::Node { node_id, .. } => present.iter().any(|id| id == node_id.as_str()),
                _ => true,
            });
        }
    }
    let probe = def.nodes.iter().map(|node| node.id).max().unwrap_or(0) + 1;
    def.nodes.push(
        serde_json::from_value(json!({"id": probe, "typeId": PROBE_TYPE, "nodeId": "liquid_probe"})).expect("probe node"),
    );
    let mut wire = |from_node: u32, from_port: &str, to_port: &str| {
        def.wires.push(EffectGraphWire { from_node, from_port: from_port.into(), to_node: probe, to_port: to_port.into() });
    };
    if !dry {
        for name in DOMAIN_SCALARS.iter().filter(|name| domain_outputs.iter().any(|port| port == *name)) {
            wire(domain, name, name);
        }
        for name in FRAME_SCALARS.iter().chain(&["particles_a", "particles_b"]) {
            wire(publisher_id, name, name);
        }
    }
    if let Some(world) = world {
        wire(world, "pose_0", "pose");
    }
    Prepared { def, cards, publisher: publisher_type }
}

struct LiquidRun {
    runtime: PresetRuntime,
    target: RenderTarget,
    device: Arc<GpuDevice>,
    manifest: ParamManifest,
    cards: Vec<(String, String)>,
    domain_type: &'static str,
    publisher: String,
    frame: u32,
    /// Transport position in ticks.
    transport: u32,
    /// Ticks per frame: 1 is 60 fps, 2 is 30 fps.
    stride: u32,
    live: bool,
    /// What the last warm-up frame published.
    start: Probe,
    clock: Option<Clocked>,
    _scope: PhysicsStepScope,
}

/// A device with a frame clock, as the app runs: every frame signals the
/// clock's event and drains what retired.
struct Clocked {
    device: Arc<GpuDevice>,
    event: GpuEvent,
    retired: RetireQueue,
}

impl Clocked {
    fn new() -> Self {
        let device = Arc::new(GpuDevice::new());
        let event = device.create_event();
        let (sender, retired) = RetireQueue::new();
        device.set_retirement(RetireMark::new(event.second_handle(), sender));
        Self { device, event, retired }
    }
}

impl LiquidRun {
    fn offline(row: &'static LiquidSolverRow, def: EffectGraphDef, stride: u32) -> Self {
        Self::new(row, def, stride, false, false)
    }


    fn new(row: &'static LiquidSolverRow, def: EffectGraphDef, stride: u32, live: bool, dry: bool) -> Self {
        Self::on(row, def, stride, live, dry, None)
    }

    /// A run on `clock`'s device, or the shared one, which has no frame
    /// clock.
    fn on(
        row: &'static LiquidSolverRow,
        def: EffectGraphDef,
        stride: u32,
        live: bool,
        dry: bool,
        clock: Option<Clocked>,
    ) -> Self {
        let device = clock.as_ref().map_or_else(|| Arc::clone(&harness::shared().device), |clock| Arc::clone(&clock.device));
        let scope = PhysicsStepScope::for_render(!live);
        let registry = registry();
        let Prepared { def, cards, publisher } = prepare(row, &def, &registry, dry);
        let manifest = ParamManifest::from_params(
            def.preset_metadata.iter().flat_map(|metadata| metadata.params.iter().cloned().map(Param::bundled)).collect(),
        );
        let mut runtime = PresetRuntime::from_def_with_device(
            def,
            &registry,
            Arc::clone(&device),
            SIZE,
            SIZE,
            GpuTextureFormat::Rgba16Float,
            None,
        )
        .unwrap_or_else(|error| panic!("{} scene builds: {error}", row.type_id));
        runtime.set_dump_all(true);
        let target = RenderTarget::new(&device, SIZE, SIZE, GpuTextureFormat::Rgba16Float, "liquid-conformance");
        let mut run = Self {
            runtime,
            target,
            device,
            manifest,
            cards,
            domain_type: row.type_id,
            publisher,
            frame: 0,
            transport: 0,
            stride,
            live,
            start: Probe::EMPTY,
            clock,
            _scope: scope,
        };
        let started = Instant::now();
        loop {
            run.render(true);
            if !run.runtime.warmup_pending() {
                break;
            }
            assert!(started.elapsed().as_secs() < 60, "{}: asset warm-up did not finish", row.type_id);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        run.start = PROBE.get();
        run
    }

    fn render(&mut self, warming: bool) -> Probe {
        let time = f64::from(self.transport) * TICK;
        let ctx = PresetContext {
            time,
            beat: time * 2.0,
            dt: if warming { 0.0 } else { (f64::from(self.stride) * TICK) as f32 },
            width: SIZE,
            height: SIZE,
            output_width: SIZE,
            output_height: SIZE,
            aspect: 1.0,
            owner_key: 0x1C0,
            is_clip_level: false,
            frame_count: i64::from(self.frame),
            anim_progress: 0.0,
            trigger_count: 0,
        };
        PROBE.set(Probe::EMPTY);
        let mut encoder = self.device.create_encoder("liquid-conformance");
        let status = {
            let mut gpu = GpuEncoder::new(&mut encoder, &self.device);
            self.runtime.render(&mut gpu, &self.target.texture, &ctx, &self.manifest);
            gpu.frame_status()
        };
        if let Some(clock) = &self.clock {
            encoder.signal_event(&clock.event);
        }
        encoder.commit_and_wait_completed();
        if let Some(clock) = &mut self.clock {
            clock.retired.drain();
        }
        let pending_allowed = warming || self.live;
        assert!(
            status == FrameRenderStatus::Complete || (pending_allowed && status == FrameRenderStatus::PendingGeometry),
            "{}: frame {} rendered with status {status:?}",
            self.domain_type,
            self.frame
        );
        PROBE.get()
    }

    /// The next frame, the transport one frame on.
    fn step(&mut self) -> Probe {
        self.frame += 1;
        self.transport += self.stride;
        self.render(false)
    }

    fn steps(&mut self, n: u32) -> Probe {
        let mut last = self.start;
        for _ in 0..n {
            last = self.step();
        }
        last
    }

    /// The next frame with the transport paused.
    fn hold(&mut self) -> Probe {
        self.frame += 1;
        self.render(false)
    }

    /// Move the card bound to the domain's `param`, as a hand on it would.
    fn set_card(&mut self, param: &str, value: f32) {
        let card = self
            .cards
            .iter()
            .find(|(bound, _)| bound == param)
            .map(|(_, card)| card.clone())
            .unwrap_or_else(|| panic!("{}: no card drives the domain's {param}", self.domain_type));
        let entry = self.manifest.get_mut(&card).unwrap_or_else(|| panic!("card {card} is in the manifest"));
        entry.value = value;
        entry.base = value;
    }

    fn read<T: bytemuck::Pod>(&self, type_id: &str, port: &str) -> Vec<T> {
        let dumps = self.runtime.dump_arrays_all();
        let dump = dumps
            .iter()
            .rev()
            .find(|dump| dump.type_id == type_id && dump.port == port)
            .unwrap_or_else(|| panic!("no {type_id}.{port} in the dump"));
        let bytes = dump.buffer.size();
        let staging = self.device.create_buffer_shared(bytes);
        let mut encoder = self.device.create_encoder("liquid-conformance-readback");
        encoder.copy_buffer_to_buffer(dump.buffer, &staging, bytes);
        encoder.commit_and_wait_completed();
        let ptr = staging.mapped_ptr().expect("shared staging buffer");
        // SAFETY: the copy has completed and nothing else writes the staging buffer.
        let raw = unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), bytes as usize) };
        bytemuck::pod_collect_to_vec(raw)
    }

    /// The fixture's one body at this frame's display time.
    fn body(&self, probe: &Probe) -> LiquidBody {
        assert_eq!(probe.get("body_count"), 1.0, "{}: a box scene holds one body", self.domain_type);
        self.read::<LiquidBody>(self.domain_type, "bodies")[0]
    }

    /// Every body row this frame's ticks ran with, tick after tick.
    fn body_words(&self, probe: &Probe) -> Vec<u32> {
        let mut words: Vec<u32> = self.read(self.domain_type, "bodies");
        let rows = probe.get("body_count") as usize * probe.get("ticks").max(1.0) as usize;
        words.truncate(rows * std::mem::size_of::<LiquidBody>() / 4);
        words
    }

    fn totals_words(&self, row: &LiquidSolverRow) -> Vec<u32> {
        let readout = row.totals.as_ref().unwrap_or_else(|| panic!("{}: no totals readout", row.type_id));
        let mut words: Vec<u32> = self.read(readout.type_id, readout.port);
        words.truncate(readout.words);
        words
    }

    fn totals(&self, row: &LiquidSolverRow) -> LiquidTotals {
        let readout = row.totals.as_ref().unwrap_or_else(|| panic!("{}: no totals readout", row.type_id));
        (readout.read)(&self.totals_words(row))
    }

    fn frame_words(&self, port: &str) -> Vec<u32> {
        self.read(&self.publisher, port)
    }

    fn particles(&self, port: &str) -> Vec<FluidParticle> {
        self.read(&self.publisher, port)
    }

    /// Corrupt one record's position in the solver state with NaN (the GPU
    /// is idle: every frame waits for completion).
    fn poison(&self, row: &LiquidSolverRow, record: usize) {
        let state = row.state.as_ref().unwrap_or_else(|| panic!("{}: no state array", row.type_id));
        let dumps = self.runtime.dump_arrays_all();
        let dump = dumps
            .iter()
            .rev()
            .find(|dump| dump.type_id == state.type_id && dump.port == state.port)
            .unwrap_or_else(|| panic!("no {}.{} in the dump", state.type_id, state.port));
        assert!(dump.buffer.mapped_ptr().is_some(), "{}.{} is not in shared storage", state.type_id, state.port);
        let offset = (record * state.record_bytes) as u64;
        assert!(offset + 12 <= dump.buffer.size(), "record {record} is past the state");
        // SAFETY: shared storage in bounds, and no GPU work is in flight.
        unsafe { dump.buffer.write(offset, bytemuck::bytes_of(&[f32::NAN; 3])) };
    }
}

/// The rows that run `check`; each exemption is printed with its reason.
fn running(check: Check) -> Vec<&'static LiquidSolverRow> {
    let rows: Vec<_> = LIQUID_SOLVERS
        .iter()
        .filter(|row| match row.exemption(check) {
            Some(reason) => {
                eprintln!("{check:?}: {} is exempt: {reason}", row.type_id);
                false
            }
            None => true,
        })
        .collect();
    assert!(!rows.is_empty(), "no row runs {check:?}");
    rows
}

fn scene(row: &LiquidSolverRow, fixture: Fixture) -> EffectGraphDef {
    (row.fixture)(fixture).unwrap_or_else(|| panic!("{}: no {fixture:?} scene", row.type_id))
}

fn box_scene(fixture: Fixture) -> BoxScene {
    BoxScene::of(fixture).unwrap_or_else(|| panic!("{fixture:?} is not a box scene"))
}

fn cell(scene: &BoxScene) -> f64 {
    f64::from(scene.domain_size) / f64::from(scene.resolution)
}

/// How far the box's bottom face sits above the still surface.
fn clearance(scene: &BoxScene, body: &LiquidBody) -> f64 {
    f64::from(body.position_inv_mass[1]) - 0.5 * f64::from(scene.edge) - f64::from(scene.fill)
}

fn v3(v: [f32; 4]) -> [f64; 3] {
    [f64::from(v[0]), f64::from(v[1]), f64::from(v[2])]
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn solve3(rows: [[f64; 3]; 3], b: [f64; 3]) -> Option<[f64; 3]> {
    let det3 = |[a, b, c]: [[f64; 3]; 3]| {
        a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0]) + a[2] * (b[0] * c[1] - b[1] * c[0])
    };
    let det = det3(rows);
    if det.abs() < 1e-30 {
        return None;
    }
    let with = |column: usize| {
        let mut m = rows;
        for (row, value) in m.iter_mut().zip(b) {
            row[column] = value;
        }
        det3(m) / det
    };
    Some([with(0), with(1), with(2)])
}

/// Kinetic energy (linear and rotational) plus gravitational potential
/// relative to `y_ref`, in joules.
fn body_energy(body: &LiquidBody, mass: f64, y_ref: f64) -> f64 {
    let v = v3(body.linear_velocity);
    let w = v3(body.angular_velocity);
    let rows = [v3(body.inv_inertia_x), v3(body.inv_inertia_y), v3(body.inv_inertia_z)];
    let rotational = solve3(rows, w).map_or(0.0, |iw| 0.5 * dot(w, iw));
    0.5 * mass * dot(v, v) + rotational + mass * G * (f64::from(body.position_inv_mass[1]) - y_ref)
}

/// What the liquid gave the body over one Box3D tick, per unit mass: the
/// velocity change less gravity's.
fn liquid_push(before: &LiquidBody, after: &LiquidBody) -> [f64; 3] {
    let (a, b) = (v3(before.linear_velocity), v3(after.linear_velocity));
    [b[0] - a[0], b[1] - a[1] + G * TICK, b[2] - a[2]]
}

/// The first word where two dumps differ, as (index, left, right).
fn first_difference(a: &[u32], b: &[u32]) -> Option<(usize, u32, u32)> {
    if a.len() != b.len() {
        return Some((a.len().min(b.len()), a.len() as u32, b.len() as u32));
    }
    a.iter().zip(b).position(|(x, y)| x != y).map(|i| (i, a[i], b[i]))
}

/// I4: a coupled Box3D world steps once per settled liquid tick and only
/// through its owner, at 60 and 30 fps. The counter sees every Box3D world
/// on this thread, the scene's own included. Negative half: `rg -n
/// '\.advance_worker\(' crates/manifold-renderer/src -g '!**/tests/**'`
/// hits only the two owners and the worker's inline tests.
#[test]
fn liquid_coupled_world_steps_once_per_tick() {
    for row in running(Check::CoupledWorldStepsOnce) {
        for &fixture in Check::CoupledWorldStepsOnce.fixtures(row.coupled) {
            for stride in [1u32, 2] {
                let mut run = LiquidRun::offline(row, scene(row, fixture), stride);
                run.steps(3);
                let mut ticks = 0u64;
                for _ in 0..30 {
                    let before = native_ticks_on_this_thread();
                    let probe = run.step();
                    let stepped = native_ticks_on_this_thread() - before;
                    let ran = probe.get("ticks") as u64;
                    assert_eq!(ran, u64::from(stride), "{} {fixture:?}: frame {} ran {ran} liquid ticks", row.type_id, run.frame);
                    assert_eq!(
                        stepped, ran,
                        "{} {fixture:?} at {} fps: frame {} stepped Box3D {stepped} times for {ran} liquid ticks",
                        row.type_id,
                        60 / stride,
                        run.frame
                    );
                    ticks += ran;
                }
                eprintln!(
                    "liquid_coupled_world_steps_once_per_tick {} {fixture:?} at {} fps: {ticks} liquid ticks, {ticks} Box3D ticks",
                    row.type_id,
                    60 / stride
                );
            }
        }
    }
}

/// Most ticks each collision is followed for from first contact.
const COLLISION_TICKS: u32 = 30;
/// Fewest: the scene must keep its premise at least this long.
const COLLISION_FLOOR: u32 = 8;
/// How far the liquid's mass may move, as a fraction, before liquid counts as
/// lost through an open face. The stats fold's rounding stays far below it.
const MASS_HELD: f64 = 1e-5;
/// How far body plus liquid momentum, less gravity's impulse on the body, may
/// drift over the collision, as a fraction of the momentum exchanged. A
/// missing exchange term shows as a share of the exchange; the smallest ever
/// measured on MPM, the uncounted push-out of BUG-n97i (coupled MPM loses
/// momentum at the collider push-out), was 4.8%.
const MOMENTUM_BALANCE: f64 = 0.01;
/// Energy may rise at most this far above the starting total.
const ENERGY_BOUND: f64 = 1.01;

/// I5 (section 3.3 (Two-way Box3D coupling)): a box falls onto a still,
/// weightless pool with open faces at 0.1, 1 and 10 times the liquid's
/// density. From first contact the check follows the collision for up to
/// [`COLLISION_TICKS`], ending at the first tick its premise breaks (liquid
/// lost through an open face, or the box out of the domain), and fails if
/// that comes before [`COLLISION_FLOOR`]. Over the ticks followed, neither
/// the body's energy nor body plus liquid rises above 1.01× the starting
/// total, and body plus liquid momentum less gravity's impulse holds within
/// [`MOMENTUM_BALANCE`] of the momentum exchanged. The section's own scene, a
/// weightless blob striking a free box at 1 m/s, needs a starting velocity
/// (BUG-8zxh (initial velocity on rigid bodies and liquid fills)); an
/// impulse cannot reach a body coupled to Live Matter yet (BUG-5svo (impulse
/// cannot be fired at a body coupled to Live Matter water)).
#[test]
fn liquid_coupling_collision() {
    for row in running(Check::Collision) {
        for &fixture in Check::Collision.fixtures(row.coupled) {
            let Fixture::Collision { density_ratio } = fixture else { panic!("{fixture:?} is not a collision") };
            let scene = box_scene(fixture);
            let mass = f64::from(scene.mass);
            let mut run = LiquidRun::offline(row, self::scene(row, fixture), 1);
            // Fall until the bottom face is within a cell of the surface.
            let mut before: Option<(LiquidBody, LiquidTotals)> = None;
            let mut contact: Option<(LiquidBody, LiquidTotals)> = None;
            for _ in 0..60 {
                let probe = run.step();
                let body = run.body(&probe);
                let liquid = run.totals(row);
                if clearance(&scene, &body) <= cell(&scene) {
                    contact = Some((body, liquid));
                    break;
                }
                before = Some((body, liquid));
            }
            let (start_body, start_liquid) = before.expect("the box starts above the pool");
            let (mut previous, mut liquid) = contact.expect("the box reaches the pool");
            let y_ref = f64::from(start_body.position_inv_mass[1]);
            let initial = body_energy(&start_body, mass, y_ref) + start_liquid.energy;
            let momentum = |body: &LiquidBody, liquid: &LiquidTotals| -> [f64; 3] {
                std::array::from_fn(|i| mass * f64::from(body.linear_velocity[i]) + liquid.momentum[i])
            };
            // Frame n's body row is Box3D at the end of the tick frame n−1's
            // liquid ran (section 3.3), so a body row pairs with the previous
            // frame's totals, the contact frame's included.
            let mut total_start: Option<[f64; 3]> = None;
            let (mut body_peak, mut total_peak) = (f64::MIN, f64::MIN);
            let (mut exchanged, mut residual) = (0.0f64, [0.0f64; 3]);
            let (mut followed, mut broke) = (0u32, None::<String>);
            for tick in 0..COLLISION_TICKS {
                let probe = run.step();
                assert_eq!(probe.get("ticks"), 1.0, "offline coupled frames each run a tick");
                let body = run.body(&probe);
                if (liquid.mass - start_liquid.mass).abs() > MASS_HELD * start_liquid.mass {
                    broke = Some(format!("liquid mass {:.4} kg of {:.4} kg", liquid.mass, start_liquid.mass));
                    break;
                }
                let at = [0, 1, 2].map(|i| body.position_inv_mass[i]);
                if !scene.holds_box(at) {
                    broke = Some(format!("the box out of the domain at {at:?}"));
                    break;
                }
                let e_body = body_energy(&body, mass, y_ref);
                body_peak = body_peak.max(e_body / initial);
                total_peak = total_peak.max((e_body + liquid.energy) / initial);
                let total = momentum(&body, &liquid);
                match total_start {
                    None => total_start = Some(total),
                    Some(start) => {
                        let gravity = [0.0, -mass * G * f64::from(tick) * TICK, 0.0];
                        residual = std::array::from_fn(|i| total[i] - start[i] - gravity[i]);
                    }
                }
                let push = liquid_push(&previous, &body);
                exchanged += mass * dot(push, push).sqrt();
                previous = body;
                liquid = run.totals(row);
                assert_eq!(liquid.nonfinite, 0, "{} ratio {density_ratio}: a non-finite tick", row.type_id);
                followed = tick + 1;
            }
            let residual_norm = dot(residual, residual).sqrt();
            let ended = broke.as_deref().map_or_else(|| "premise held".to_string(), |why| format!("ended by {why}"));
            eprintln!(
                "liquid_coupling_collision {} ratio {density_ratio}: {followed} ticks followed ({ended}); initial \
                 {initial:.4} J, body peak {body_peak:.4}×, body+liquid peak {total_peak:.4}×; momentum error {:.3}% \
                 (|R| {residual_norm:.4e} of {exchanged:.4e} kg·m/s exchanged)",
                row.type_id,
                100.0 * residual_norm / exchanged
            );
            assert!(
                followed >= COLLISION_FLOOR,
                "{} ratio {density_ratio}: the scene broke its premise after {followed} ticks ({ended})",
                row.type_id
            );
            assert!(initial > 0.0);
            assert!(body_peak <= ENERGY_BOUND, "{} ratio {density_ratio}: body energy reached {body_peak:.4}× the start", row.type_id);
            assert!(
                total_peak <= ENERGY_BOUND,
                "{} ratio {density_ratio}: body plus liquid energy reached {total_peak:.4}× the start",
                row.type_id
            );
            assert!(exchanged > 0.5, "{} ratio {density_ratio}: the box barely touched the pool ({exchanged:.3} kg·m/s)", row.type_id);
            assert!(
                residual_norm <= MOMENTUM_BALANCE * exchanged,
                "{} ratio {density_ratio}: body plus liquid momentum drifted {residual:?} (|R| {residual_norm:.4e}) \
                 against {exchanged:.4e} kg·m/s exchanged",
                row.type_id
            );
        }
    }
}

/// I5: a box at half the liquid's density, dropped tilted into the pool,
/// settles with its centre at the waterline (a half-density cube's draft in
/// any orientation), within half a cell. The waterline is the free surface
/// the published particles show, over the columns clear of the box.
#[test]
fn liquid_floating_draft() {
    for row in running(Check::FloatingDraft) {
        for &fixture in Check::FloatingDraft.fixtures(row.coupled) {
            let scene = box_scene(fixture);
            let dx = cell(&scene);
            let mut run = LiquidRun::offline(row, self::scene(row, fixture), 1);
            run.steps(240);
            let (mut sum, mut n) = (0.0, 0.0);
            let (mut lo, mut hi) = (f64::MAX, f64::MIN);
            let mut probe = run.start;
            for _ in 0..60 {
                probe = run.step();
                let y = f64::from(run.body(&probe).position_inv_mass[1]);
                sum += y;
                n += 1.0;
                lo = lo.min(y);
                hi = hi.max(y);
            }
            let centre = sum / n;
            let at = v3(run.body(&probe).position_inv_mass);
            let size = f64::from(scene.domain_size);
            let columns = (size / dx).round() as usize;
            let mut tops = vec![f64::MIN; columns * columns];
            for p in run.particles("particles_b").iter().filter(|p| p.position_radius[3] > 0.0) {
                let [x, y, z] = [0, 1, 2].map(|i| f64::from(p.position_radius[i]));
                if (x - at[0]).abs() < f64::from(scene.edge) || (z - at[2]).abs() < f64::from(scene.edge) {
                    continue;
                }
                let cx = (((x + 0.5 * size) / dx) as usize).min(columns - 1);
                let cz = (((z + 0.5 * size) / dx) as usize).min(columns - 1);
                // A particle's column top: its centre plus half the spacing of
                // two points per cell.
                tops[cz * columns + cx] = tops[cz * columns + cx].max(y + 0.25 * dx);
            }
            let open: Vec<f64> = tops.into_iter().filter(|top| *top > f64::MIN).collect();
            assert!(open.len() > columns, "{}: too few open columns ({}) to read the surface", row.type_id, open.len());
            let waterline = open.iter().sum::<f64>() / open.len() as f64;
            eprintln!(
                "liquid_floating_draft {}: centre {centre:.4} m, waterline {waterline:.4} m over {} open columns, \
                 draft error {:.3}·dx, bob amplitude {:.4} m over the last second",
                row.type_id,
                open.len(),
                (centre - waterline) / dx,
                0.5 * (hi - lo)
            );
            assert!(
                (centre - waterline).abs() <= 0.5 * dx,
                "{}: box centre {centre:.4} is not within half a cell ({:.4}) of the waterline {waterline:.4}",
                row.type_id,
                0.5 * dx
            );
        }
    }
}

/// I5: a box as dense as the liquid, under 0.8 m of it, feels the weight of
/// the liquid it displaces, ρ·|g|·V, within 5%. The force is what the body
/// rows show beyond gravity, averaged over two seconds.
#[test]
fn liquid_hydrostatic_lift() {
    for row in running(Check::HydrostaticLift) {
        for &fixture in Check::HydrostaticLift.fixtures(row.coupled) {
            let scene = box_scene(fixture);
            let mass = f64::from(scene.mass);
            let mut run = LiquidRun::offline(row, self::scene(row, fixture), 1);
            let probe = run.steps(120);
            let mut previous = run.body(&probe);
            let (mut force, mut n) = (0.0, 0.0);
            let (mut lo, mut hi) = (f64::MAX, f64::MIN);
            for _ in 0..120 {
                let probe = run.step();
                assert_eq!(probe.get("ticks"), 1.0, "offline coupled frames each run a tick");
                let body = run.body(&probe);
                force += mass * liquid_push(&previous, &body)[1] / TICK;
                n += 1.0;
                let y = f64::from(body.position_inv_mass[1]);
                lo = lo.min(y);
                hi = hi.max(y);
                previous = body;
            }
            let force = force / n;
            let expected = f64::from(FIXTURE_DENSITY) * G * f64::from(scene.edge).powi(3);
            let error = (force - expected) / expected;
            eprintln!(
                "liquid_hydrostatic_lift {}: {force:.1} N against ρ·g·V {expected:.1} N, lift error {:.2}%, box y range {lo:.4}..{hi:.4}",
                row.type_id,
                error * 100.0
            );
            assert!(error.abs() <= 0.05, "{}: lift {force:.1} N is not within 5% of {expected:.1} N", row.type_id);
        }
    }
}

/// I5: before it touches the liquid, the coupled box is drawn exactly where
/// Box3D alone puts it at the frame's display time, bit for bit.
#[test]
fn liquid_free_flight() {
    for row in running(Check::FreeFlight) {
        for &fixture in Check::FreeFlight.fixtures(row.coupled) {
            let scene = box_scene(fixture);
            let mut dry = LiquidRun::new(row, self::scene(row, fixture), 1, false, true);
            // Box3D alone presents the pose after f ticks at frame f.
            let mut reference = vec![dry.start.pose.expect("dry start pose")];
            reference.extend((0..60).map(|_| dry.step().pose.expect("dry pose")));
            let mut coupled = LiquidRun::offline(row, self::scene(row, fixture), 1);
            let mut compared = 0;
            let mut worst = 0.0f32;
            loop {
                let probe = coupled.step();
                if clearance(&scene, &coupled.body(&probe)) <= cell(&scene) {
                    break;
                }
                let pose = probe.pose.expect("coupled pose");
                let k = (f64::from(probe.get("display_time")) / TICK).round() as usize;
                let want = reference[k];
                for i in 0..3 {
                    worst = worst.max((pose.pos[i] - want.pos[i]).abs()).max((pose.rot_euler[i] - want.rot_euler[i]).abs());
                }
                compared += 1;
                assert!(compared < reference.len(), "{}: the box never reached the pool", row.type_id);
            }
            eprintln!(
                "liquid_free_flight {} {fixture:?}: {compared} ticks before contact, worst pose difference {worst:e}",
                row.type_id
            );
            assert!(compared >= 5, "{}: only {compared} ticks of free flight", row.type_id);
            assert_eq!(worst, 0.0, "{}: coupled free flight differs from Box3D alone by {worst}", row.type_id);
        }
    }
}

/// Frames played before a pause, and frames held.
const PLAY: u32 = 12;
const HOLD: u32 = 6;

/// I6: with the transport paused the liquid publishes the same frame, word
/// for word, with the same scalars; on resume it runs again.
#[test]
fn liquid_pause_holds_frames() {
    for row in running(Check::PauseHoldsFrames) {
        for &fixture in Check::PauseHoldsFrames.fixtures(row.coupled) {
            let mut run = LiquidRun::offline(row, scene(row, fixture), 1);
            let played = run.steps(PLAY);
            let (a, b) = (run.frame_words("particles_a"), run.frame_words("particles_b"));
            assert!(played.get("count_b") > 0.0, "{} {fixture:?}: no liquid published", row.type_id);
            for held in 1..=HOLD {
                let probe = run.hold();
                // NaN: the domain publishes no tick count.
                let ticks = probe.get("ticks");
                assert!(ticks.is_nan() || ticks == 0.0, "{}: paused frame {held} ran {ticks} ticks", row.type_id);
                assert_eq!(probe.held_bits(), played.held_bits(), "{}: paused frame {held} published other scalars", row.type_id);
                assert_eq!(probe.pose, played.pose, "{}: paused frame {held} moved the box", row.type_id);
                for (port, before) in [("particles_a", &a), ("particles_b", &b)] {
                    if let Some((i, x, y)) = first_difference(&run.frame_words(port), before) {
                        panic!("{} {fixture:?}: paused frame {held} changed {port} at word {i}: {x:#010x} against {y:#010x}", row.type_id);
                    }
                }
            }
            let resumed = run.step();
            assert!(
                resumed.get("simulation_time") > played.get("simulation_time"),
                "{}: the liquid did not run again after the pause",
                row.type_id
            );
            eprintln!(
                "liquid_pause_holds_frames {} {fixture:?}: {HOLD} paused frames equal frame {PLAY} word for word",
                row.type_id
            );
        }
    }
}

/// Section 3.4 (Clock, pause, speed, reset, export) has pause discard
/// impulses; no GPU check exists until P8 routes impulses to GPU liquids, so
/// every row must name its exemption.
#[test]
fn liquid_pause_discards_impulses() {
    for row in LIQUID_SOLVERS {
        let reason = row.exemption(Check::PauseDiscardsImpulses);
        assert!(reason.is_some(), "{}: pause-discards-impulses has no GPU check until P8", row.type_id);
        eprintln!("PauseDiscardsImpulses: {} is exempt: {}", row.type_id, reason.unwrap_or_default());
    }
}

/// What one exported frame leaves behind, as raw words.
struct FrameDump {
    probe: Probe,
    rows: Vec<u32>,
    totals: Vec<u32>,
    particles: Vec<u32>,
}

impl LiquidRun {
    fn dump(&mut self, row: &LiquidSolverRow) -> FrameDump {
        let probe = self.step();
        FrameDump {
            probe,
            rows: self.body_words(&probe),
            totals: self.totals_words(row),
            particles: self.frame_words("particles_b"),
        }
    }
}

/// I7 (export loses no time): the scene exported at 30 fps runs two ticks a
/// frame, exchanging with Box3D between them on a coupled row, and matches
/// the 60 fps export word for word at every shared instant: the body rows of
/// both ticks, the liquid totals, the particles, display and simulation time
/// and the drawn box.
#[test]
fn liquid_export_frame_rate_independent() {
    for row in running(Check::ExportFrameRateIndependent) {
        for &fixture in Check::ExportFrameRateIndependent.fixtures(row.coupled) {
            let mut at_60 = LiquidRun::offline(row, scene(row, fixture), 1);
            let mut at_30 = LiquidRun::offline(row, scene(row, fixture), 2);
            let mut contact_frames = 0;
            for frame in 1..=60u32 {
                let earlier = at_60.dump(row);
                let later = at_60.dump(row);
                let both = at_30.dump(row);
                assert_eq!(both.probe.get("ticks"), 2.0, "30 fps frame {frame} ran {} ticks", both.probe.get("ticks"));
                assert_eq!(later.probe.get("ticks"), 1.0);
                assert_eq!(both.rows.len(), 2 * earlier.rows.len(), "30 fps frame {frame} holds two ticks of body rows");
                // Uncoupled, the display sits one tick behind the target
                // (section 3.4), the same at every frame rate. Coupled, it is
                // the tick Box3D settled before the frame's ticks, so a
                // 30 fps frame shows what the 60 fps frame before it showed.
                let shown = if row.coupled { &earlier } else { &later };
                let checks = [
                    ("first tick's body rows", first_difference(&both.rows[..earlier.rows.len()], &earlier.rows)),
                    ("second tick's body rows", first_difference(&both.rows[earlier.rows.len()..], &later.rows)),
                    ("liquid totals", first_difference(&both.totals, &later.totals)),
                    ("particles", first_difference(&both.particles, &later.particles)),
                    (
                        "display and simulation time",
                        first_difference(
                            &[both.probe.get("display_time").to_bits(), both.probe.get("simulation_time").to_bits()],
                            &[shown.probe.get("display_time").to_bits(), later.probe.get("simulation_time").to_bits()],
                        ),
                    ),
                ];
                for (what, difference) in checks {
                    if let Some((i, a, b)) = difference {
                        panic!(
                            "{} {fixture:?}: 30 fps frame {frame} differs from 60 fps in {what} at word {i}: \
                             {a:#010x} ({}) against {b:#010x} ({})",
                            row.type_id,
                            f32::from_bits(a),
                            f32::from_bits(b)
                        );
                    }
                }
                assert_eq!(both.probe.pose, earlier.probe.pose, "30 fps frame {frame} draws the box elsewhere");
                if !later.rows.is_empty() {
                    let rows: &[LiquidBody] = bytemuck::cast_slice(&both.rows);
                    let push = liquid_push(&rows[0], &rows[rows.len() - 1]);
                    if dot(push, push).sqrt() > 1e-4 {
                        contact_frames += 1;
                    }
                }
            }
            eprintln!(
                "liquid_export_frame_rate_independent {} {fixture:?}: 60 frames at 30 fps equal 120 at 60 fps word for word; \
                 {contact_frames} frames exchange with the box",
                row.type_id
            );
            if row.coupled && BoxScene::of(fixture).is_some() {
                assert!(contact_frames >= 30, "{}: the box touched the liquid in only {contact_frames} of 60 frames", row.type_id);
            }
        }
    }
}

/// I8 (section 3.1 (Particle frame), amendment 2): a tick with a non-finite
/// position is never published. The totals flag it, the frame keeps the last
/// good tick, the domain names the error, the solver halts, and Reset starts
/// a fresh epoch that runs again.
#[test]
fn liquid_nonfinite_tick_not_published() {
    for row in running(Check::NonfiniteTickNotPublished) {
        for &fixture in Check::NonfiniteTickNotPublished.fixtures(row.coupled) {
            let tap = NodeErrorTap::new();
            let mut run = LiquidRun::offline(row, scene(row, fixture), 1);
            let before = run.steps(10);
            let (a, b) = (run.frame_words("particles_a"), run.frame_words("particles_b"));
            let state = row.state.as_ref().expect("a state array");
            assert!(tap.take().is_empty(), "{}: errors before the corruption", row.type_id);
            run.poison(row, 100);
            run.step();
            assert!(run.totals(row).nonfinite > 0, "{}: the totals do not flag the NaN record", row.type_id);
            // B keeps the last good tick; A is the last good A, or the last
            // good B when the ring turned over the repeat.
            if let Some((i, _, _)) = first_difference(&run.frame_words("particles_b"), &b) {
                panic!("{}: a non-finite tick reached particles_b (word {i})", row.type_id);
            }
            let now_a = run.frame_words("particles_a");
            assert!(
                first_difference(&now_a, &a).is_none() || first_difference(&now_a, &b).is_none(),
                "{}: a non-finite tick reached particles_a",
                row.type_id
            );
            let halted: Vec<u32> = {
                run.step();
                run.read(state.type_id, state.port)
            };
            run.step();
            assert!(
                first_difference(&halted, &run.read::<u32>(state.type_id, state.port)).is_none(),
                "{}: the solver ran on after a non-finite tick",
                row.type_id
            );
            let errors = tap.take();
            let named = errors.iter().find(|error| error.contains("non-finite"));
            assert!(named.is_some(), "{}: no error names the non-finite tick: {errors:?}", row.type_id);
            run.set_card("reset", 1.0);
            let fresh = run.steps(2);
            assert_eq!(run.totals(row).nonfinite, 0, "{}: Reset did not clear the fault", row.type_id);
            assert_ne!(fresh.get("identity_b"), before.get("identity_b"), "{}: Reset kept the epoch", row.type_id);
            assert!(
                run.particles("particles_b").iter().all(|p| p.position_radius.iter().all(|v| v.is_finite())),
                "{}: the fresh epoch published non-finite particles",
                row.type_id
            );
            eprintln!("liquid_nonfinite_tick_not_published {}: {}", row.type_id, named.map_or("", String::as_str));
        }
    }
}

/// I11: a run-time capacity overflow is counted and reported as a named
/// error carrying the count, within a frame of the liquid first needing it.
#[test]
fn liquid_overflow_is_reported() {
    for row in running(Check::OverflowReported) {
        let case = row.overflow.as_ref().expect("an overflow case");
        let mut def = scene(row, case.fixture);
        (case.edit)(&mut def);
        let tap = NodeErrorTap::new();
        let mut run = LiquidRun::offline(row, def, 1);
        let mut reported = None;
        for _ in 0..3 {
            run.step();
            if let Some(error) = tap.take().into_iter().find(|error| case.names.iter().all(|name| error.contains(name))) {
                reported = Some((run.frame, error));
                break;
            }
        }
        let (frame, error) = reported.unwrap_or_else(|| panic!("{}: {} is not reported by name", row.type_id, case.what));
        assert!(error.chars().any(|c| c.is_ascii_digit()), "{}: the report carries no count: {error}", row.type_id);
        eprintln!("liquid_overflow_is_reported {}: {} → frame {frame}: {error}", row.type_id, case.what);
    }
}

/// I13: 120 live frames never wait on the GPU. The counter sits on the frame
/// clock's one wait, so it sees every waiter on this thread. The runs use a
/// device with a frame clock, as the app does; offline, the same scene does
/// wait, which shows the counter counts.
#[test]
fn liquid_live_frames_never_wait() {
    let mut clock = Some(Clocked::new());
    for row in running(Check::LiveFramesNeverWait) {
        for &fixture in Check::LiveFramesNeverWait.fixtures(row.coupled) {
            let mut live = LiquidRun::on(row, scene(row, fixture), 1, true, false, clock.take());
            let before = FrameClock::waits_on_this_thread();
            let mut ticks = 0.0;
            for _ in 0..120 {
                ticks += live.step().get("ticks");
            }
            let waits = FrameClock::waits_on_this_thread() - before;
            let mut offline = LiquidRun::on(row, scene(row, fixture), 1, false, false, live.clock.take());
            drop(live);
            let before = FrameClock::waits_on_this_thread();
            offline.steps(10);
            let offline_waits = FrameClock::waits_on_this_thread() - before;
            clock = offline.clock.take();
            eprintln!(
                "liquid_live_frames_never_wait {} {fixture:?}: 120 live frames ran {ticks} ticks with {waits} waits; \
                 10 offline frames waited {offline_waits} times",
                row.type_id
            );
            assert_eq!(waits, 0, "{}: live frames waited on the GPU {waits} times", row.type_id);
            assert!(ticks >= 60.0, "{}: the live liquid barely ran ({ticks} ticks in 120 frames)", row.type_id);
            if row.coupled {
                assert!(offline_waits > 0, "{}: the wait counter saw no offline wait", row.type_id);
            }
        }
    }
}

/// Speed 0.5 runs half the water time: the card at 0.5 reaches half the
/// simulated seconds of the card at 1 over the same transport, within a tick.
#[test]
fn liquid_half_speed_runs_half_the_water_time() {
    for row in running(Check::HalfSpeed) {
        for &fixture in Check::HalfSpeed.fixtures(row.coupled) {
            let time = |speed: f32| {
                let mut run = LiquidRun::offline(row, scene(row, fixture), 1);
                run.set_card("speed", speed);
                f64::from(run.steps(PLAY).get("simulation_time"))
            };
            let (full, half) = (time(1.0), time(0.5));
            eprintln!(
                "liquid_half_speed_runs_half_the_water_time {} {fixture:?}: {PLAY} frames ran {full:.4} s at Speed 1 \
                 and {half:.4} s at Speed 0.5",
                row.type_id
            );
            assert!(full > 0.5 * f64::from(PLAY) * TICK, "{}: the liquid barely ran ({full} s)", row.type_id);
            assert!(
                (half - 0.5 * full).abs() <= TICK + 1e-6,
                "{}: Speed 0.5 ran {half:.4} s against half of {full:.4} s",
                row.type_id
            );
        }
    }
}

/// Reset starts a new epoch: water time starts again from zero and the liquid
/// runs on. A frame that carries an identity (0 is none, by the frame
/// contract) carries a new one, and a domain that counts epochs counts one
/// more.
#[test]
fn liquid_reset_starts_a_new_epoch() {
    for row in running(Check::Reset) {
        for &fixture in Check::Reset.fixtures(row.coupled) {
            let mut run = LiquidRun::offline(row, scene(row, fixture), 1);
            let played = run.steps(PLAY);
            run.set_card("reset", 1.0);
            let reset = run.steps(2);
            let resumed = run.steps(4);
            eprintln!(
                "liquid_reset_starts_a_new_epoch {} {fixture:?}: identity {} → {}, epoch {} → {}, water time {:.4} s → {:.4} s → {:.4} s",
                row.type_id,
                played.get("identity_b"),
                reset.get("identity_b"),
                played.get("epoch"),
                reset.get("epoch"),
                played.get("simulation_time"),
                reset.get("simulation_time"),
                resumed.get("simulation_time")
            );
            if played.get("identity_b") != 0.0 {
                assert_ne!(reset.get("identity_b"), played.get("identity_b"), "{}: Reset kept the frame identity", row.type_id);
                assert_eq!(resumed.get("identity_b"), reset.get("identity_b"), "{}: one Reset started two epochs", row.type_id);
            }
            if !played.get("epoch").is_nan() {
                assert_eq!(reset.get("epoch"), played.get("epoch") + 1.0, "{}: Reset did not count one epoch", row.type_id);
                assert_eq!(resumed.get("epoch"), reset.get("epoch"), "{}: one Reset started two epochs", row.type_id);
            }
            assert!(
                f64::from(reset.get("simulation_time")) <= 2.0 * TICK + 1e-6,
                "{}: water time did not restart ({} s)",
                row.type_id,
                reset.get("simulation_time")
            );
            assert!(resumed.get("simulation_time") > reset.get("simulation_time"), "{}: the new epoch does not run", row.type_id);

            // The runtime's state reset (export start, resize) restarts the
            // same way while the transport runs on.
            let device = Arc::clone(&run.device);
            run.runtime.reset_state(&device);
            let cleared = run.steps(1);
            eprintln!(
                "liquid_reset_starts_a_new_epoch {} {fixture:?}: after reset_state identity {}, epoch {}, water time {:.4} s",
                row.type_id,
                cleared.get("identity_b"),
                cleared.get("epoch"),
                cleared.get("simulation_time")
            );
            if resumed.get("identity_b") != 0.0 {
                assert_ne!(cleared.get("identity_b"), resumed.get("identity_b"), "{}: reset_state kept the frame identity", row.type_id);
            }
            if !resumed.get("epoch").is_nan() {
                assert_eq!(cleared.get("epoch"), resumed.get("epoch") + 1.0, "{}: reset_state did not count one epoch", row.type_id);
            }
            assert!(
                f64::from(cleared.get("simulation_time")) <= TICK + 1e-6,
                "{}: reset_state did not restart the water time ({} s)",
                row.type_id,
                cleared.get("simulation_time")
            );
        }
    }
}

/// Every core busy twice over while `f` runs.
fn contended<T>(f: impl FnOnce() -> T) -> T {
    let stop = Arc::new(AtomicBool::new(false));
    let threads = 2 * std::thread::available_parallelism().map_or(8, |n| n.get());
    let burners: Vec<_> = (0..threads)
        .map(|_| {
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                let mut x = 0u64;
                while !stop.load(Ordering::Relaxed) {
                    x = std::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(1));
                }
            })
        })
        .collect();
    let result = f();
    stop.store(true, Ordering::Relaxed);
    for burner in burners {
        burner.join().expect("burner thread");
    }
    result
}

fn holds_liquid(nodes: &[EffectGraphNode]) -> bool {
    nodes
        .iter()
        .any(|node| is_liquid_domain(&node.type_id) || node.group.as_ref().is_some_and(|group| holds_liquid(&group.nodes)))
}

/// BUG-qssh (thumbnail differs run to run): every bundled liquid preset's
/// thumbnail is the same bytes with every core busy as with the machine idle.
#[test]
fn liquid_thumbnail_ignores_contention() {
    let device = &harness::shared().device;
    let mut changed = Vec::new();
    let mut rendered = 0;
    for id in bundled_preset_type_ids(PresetKind::Generator) {
        let def = bundled_preset_def(&id).expect("bundled preset");
        if !holds_liquid(&def.nodes) {
            continue;
        }
        let render = || {
            render_preset_thumbnail(device, PresetKind::Generator, def, THUMBNAIL_WIDTH, THUMBNAIL_HEIGHT, false)
                .unwrap_or_else(|error| panic!("{id}: {error}"))
        };
        let start = Instant::now();
        let idle = render();
        let idle_time = start.elapsed();
        let busy = contended(render);
        let differing = idle.iter().zip(&busy).filter(|(a, b)| a != b).count();
        eprintln!(
            "{id}: {differing} of {} bytes differ; idle {idle_time:.1?}, busy {:.1?}",
            idle.len(),
            start.elapsed() - idle_time
        );
        if idle != busy {
            changed.push(id);
        }
        rendered += 1;
    }
    assert!(rendered > 0, "no bundled liquid preset");
    assert!(changed.is_empty(), "thumbnails changed under contention: {changed:?}");
}
