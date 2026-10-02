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

use manifold_core::effect_graph_def::{BindingTarget, EffectGraphDef, EffectGraphNode, EffectGraphWire, SerializedParamValue};
use manifold_core::liquid_domain::{GPU_FLIP_DOMAIN_TYPE_ID, is_liquid_domain};
use manifold_core::params::{Param, ParamManifest};
use manifold_core::preset_def::PresetKind;
use manifold_gpu::{FrameClock, GpuDevice, GpuEvent, GpuTextureFormat, RetireMark, RetireQueue};
use manifold_renderer::frame_status::{FrameRenderFailure, FrameRenderStatus};
use manifold_renderer::gpu_encoder::GpuEncoder;
use manifold_renderer::node_graph::fluid::TICK;
use manifold_renderer::node_graph::fluid_particles::FluidParticle;
use manifold_renderer::node_graph::liquid::bodies::LiquidBody;
use manifold_renderer::node_graph::liquid::grid::{FACE_GRID_PORTS, face_len};
use manifold_renderer::node_graph::liquid::conformance::{
    BoxScene, Check, FIXTURE_DENSITY, Fixture, LIQUID_SOLVERS, LiquidSolverRow, LiquidTotals, set_type_param,
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
/// The face grid's scalars, where the frame publishes them.
const FACE_SCALARS: [&str; 4] = ["face_cells_x", "face_cells_y", "face_cells_z", "face_valid_layers"];
const SCALARS: usize = DOMAIN_SCALARS.len() + FRAME_SCALARS.len() + FACE_SCALARS.len();

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
        let index = probed_scalars().position(|probed| *probed == name).unwrap_or_else(|| panic!("{name} is not probed"));
        self.scalars[index]
    }

    /// The scalars as bits, every one but the frame's tick count.
    fn held_bits(&self) -> Vec<u32> {
        probed_scalars()
            .zip(self.scalars)
            .filter(|(name, _)| **name != "ticks")
            .map(|(_, value)| value.to_bits())
            .collect()
    }
}

fn probed_scalars() -> impl Iterator<Item = &'static &'static str> {
    DOMAIN_SCALARS.iter().chain(&FRAME_SCALARS).chain(&FACE_SCALARS)
}

thread_local! {
    static PROBE: Cell<Probe> = const { Cell::new(Probe::EMPTY) };
}

/// Records whichever of its inputs are wired. Its particle and face inputs
/// keep both frames and the face grid allocated, so the array dump carries
/// them.
struct LiquidProbe {
    type_id: EffectNodeType,
    inputs: Vec<NodeInput>,
}

impl LiquidProbe {
    fn new() -> Self {
        let port = |name: &'static str, ty: PortType| NodePort { name: Cow::Borrowed(name), ty, kind: PortKind::Input, required: false };
        let mut inputs: Vec<NodeInput> = probed_scalars().map(|&name| port(name, PortType::Scalar(ScalarType::F32))).collect();
        inputs.push(port("pose", PortType::Transform));
        for name in ["particles_a", "particles_b"] {
            inputs.push(port(name, PortType::Array(ArrayType::of_known::<FluidParticle>())));
        }
        for &name in &FACE_GRID_PORTS[..3] {
            inputs.push(port(name, PortType::Array(ArrayType::of_known::<f32>())));
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
        for (slot, name) in probed_scalars().enumerate() {
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
    // Flattening refuses modifier data, so it rides around the flatten; the
    // runtime expands it, and its refs name top-level scene nodes, which
    // keep their ids.
    let mut bare = def.clone();
    let modifiers = std::mem::take(&mut bare.scene_modifiers);
    let is_modifier = |target: &BindingTarget| matches!(target, BindingTarget::SceneModifier { .. });
    let modifier_bindings: Vec<_> = bare.preset_metadata.as_mut().map_or_else(Vec::new, |metadata| {
        let (modifier, node): (Vec<_>, Vec<_>) =
            std::mem::take(&mut metadata.bindings).into_iter().partition(|binding| is_modifier(&binding.target));
        metadata.bindings = node;
        modifier
    });
    let mut def = manifold_core::flatten::flatten_groups(&bare).expect("a liquid scene flattens");
    def.scene_modifiers = modifiers;
    if let Some(metadata) = def.preset_metadata.as_mut() {
        metadata.bindings.extend(modifier_bindings);
    }
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
        let publisher_outputs = outputs(&publisher_type);
        for name in FACE_SCALARS.iter().chain(&FACE_GRID_PORTS[..3]).filter(|name| publisher_outputs.iter().any(|port| port == *name)) {
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
    /// The check provokes a node error, so a frame it fails is expected.
    errors_expected: bool,
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
        let device = Arc::new(GpuDevice::new_queued("gpu_proofs"));
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
            errors_expected: false,
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
            status == FrameRenderStatus::Complete
                || (pending_allowed && status == FrameRenderStatus::PendingGeometry)
                || (self.errors_expected && status == FrameRenderStatus::Failed(FrameRenderFailure::NodeError)),
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

    /// The frame's face grid over `cells`, x, y and z.
    fn faces(&self, cells: [u32; 3]) -> [Vec<f32>; 3] {
        std::array::from_fn(|axis| {
            let port = FACE_GRID_PORTS[axis];
            let mut faces: Vec<f32> = self.read(&self.publisher, port);
            let len = face_len(cells, axis) as usize;
            assert!(faces.len() >= len, "{}: {port} holds {} of {len} faces", self.domain_type, faces.len());
            faces.truncate(len);
            faces
        })
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
/// rows show beyond gravity, averaged over two seconds. GPU FLIP runs on
/// the FLIP Fluids engine's own tank (`gpu_flip_engine_tank`): the 32-cell
/// Submerged Box is no valid reference, as the engine itself collapses on
/// it within 16 frames.
#[test]
fn liquid_hydrostatic_lift() {
    for row in running(Check::HydrostaticLift) {
        for &fixture in Check::HydrostaticLift.fixtures(row.coupled) {
            let (def, scene) = if row.type_id == GPU_FLIP_DOMAIN_TYPE_ID {
                manifold_renderer::node_graph::liquid::conformance::gpu_flip_engine_tank()
            } else {
                (self::scene(row, fixture), box_scene(fixture))
            };
            let mass = f64::from(scene.mass);
            let mut run = LiquidRun::offline(row, def, 1);
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

/// One coupled substep of a neutral box, as either solver saw it: vertical
/// components, SI units.
#[derive(Clone, Copy, Debug)]
struct BodySubstep {
    dt: f64,
    /// The pressure's impulse on the body, N·s.
    impulse: f64,
    /// The body velocity the solve was offered: v + g·dt.
    predicted: f64,
    /// The body velocity after the reaction and gravity.
    after: f64,
}

/// Frames the side by side compares: the box starts at rest in still water.
const SIDE_BY_SIDE_TICKS: usize = 30;
/// GPU FLIP substeps a frame: the engine tank's floor of two. The engine may
/// take more under its CFL limit; both sides are summed per 60 Hz frame.
const SIDE_BY_SIDE_SUBSTEPS: u32 = 2;

/// The FLIP Fluids engine on its own coupled tank, summed per 60 Hz frame:
/// the CPU expected side of the side by side.
fn engine_tank(scene: &BoxScene) -> Vec<BodySubstep> {
    use manifold_fluids::{Bounds, Config, FluidWorld, LiquidOptions, MeshRole, RigidBodyState, TimeStepOptions};
    use manifold_physics::{BodyConfig, PhysicsWorld, Seconds, TriangleMesh};
    let half = 0.5 * scene.edge;
    let corner = |k: usize| {
        [
            if k & 1 == 1 { half } else { -half },
            if k & 2 == 2 { half } else { -half },
            if k & 4 == 4 { half } else { -half },
        ]
    };
    let mesh = TriangleMesh {
        vertices: [0, 1, 3, 2, 4, 5, 7, 6].map(corner).to_vec(),
        triangles: vec![
            [0, 2, 1],
            [0, 3, 2],
            [4, 5, 6],
            [4, 6, 7],
            [0, 1, 5],
            [0, 5, 4],
            [3, 7, 6],
            [3, 6, 2],
            [0, 4, 7],
            [0, 7, 3],
            [1, 2, 6],
            [1, 6, 5],
        ],
    };
    // The engine's domain starts at the origin; the scene's is centred in x
    // and z with its floor at 0.
    let shift = 0.5 * scene.domain_size;
    let position = [scene.centre[0] + shift, scene.centre[1], scene.centre[2] + shift];
    let mut rigid = PhysicsWorld::new([0.0, -G as f32, 0.0]).unwrap();
    let body = rigid.add_hull(&mesh.vertices, BodyConfig { position, mass: scene.mass, ..BodyConfig::default() }).unwrap();
    let mut fluid = FluidWorld::new(Config {
        cells: [scene.resolution as u32; 3],
        cell_size: f64::from(scene.domain_size) / f64::from(scene.resolution),
        surface_subdivisions: 0,
        apic: false,
    })
    .unwrap();
    fluid.set_gravity([0.0; 3]).unwrap();
    fluid.set_time_step_options(TimeStepOptions { min_substeps: SIDE_BY_SIDE_SUBSTEPS, max_substeps: 32, cfl: 1, adaptive_obstacles: false }).unwrap();
    fluid.set_liquid_options(LiquidOptions { viscosity: 0.0, surface_tension: 0.0 }).unwrap();
    let collider = fluid.add_mesh(&mesh, MeshRole::Collider, rigid.pose(body).unwrap()).unwrap();
    let size = scene.domain_size;
    fluid.add_fluid_box(Bounds { min: [0.0; 3], max: [size, scene.fill, size] }, [0.0; 3]).unwrap();
    let frame_dt = Seconds(TICK);
    // Seed the particles without gravity, as the engine's own tank does.
    fluid.step(frame_dt).unwrap();
    fluid.set_gravity([0.0, -G as f32, 0.0]).unwrap();
    fluid.prepare_rigid_coupling(&[collider], f64::from(FIXTURE_DENSITY)).unwrap();
    let mut out = Vec::with_capacity(SIDE_BY_SIDE_TICKS);
    for _ in 0..SIDE_BY_SIDE_TICKS {
        let mut frame = fluid.begin_frame(frame_dt).unwrap();
        let start = rigid.dynamics(body).unwrap();
        let (mut elapsed, mut impulse) = (0.0, 0.0);
        // The body goes back into the solve every substep, as the engine's
        // own tank does.
        while elapsed < frame_dt.0 - 1e-12 {
            let dynamics = rigid.dynamics(body).unwrap();
            frame.set_rigid_bodies(&[RigidBodyState { pose: rigid.pose(body).unwrap(), dynamics }]).unwrap();
            let dt = frame.next_substep().unwrap().expect("a substep remains");
            frame.advance(dt).unwrap();
            let reaction = frame.rigid_reactions().unwrap()[0];
            rigid.apply_impulses(&[reaction.body_impulse(body).unwrap()]).unwrap();
            rigid.step(dt, 1).unwrap();
            impulse += reaction.linear[1];
            elapsed += dt.0;
        }
        frame.finish().unwrap();
        out.push(BodySubstep {
            dt: elapsed,
            impulse,
            predicted: f64::from(start.linear_velocity[1]) + elapsed * f64::from(start.external_linear_acceleration[1]),
            after: f64::from(rigid.dynamics(body).unwrap().linear_velocity[1]),
        });
    }
    out
}

/// GPU FLIP on the same tank: the actual side.
fn gpu_flip_tank(mut def: EffectGraphDef, mass: f64) -> Vec<BodySubstep> {
    let row = LIQUID_SOLVERS.iter().find(|row| row.type_id == GPU_FLIP_DOMAIN_TYPE_ID).expect("GPU FLIP is a liquid row");
    set_type_param(&mut def, "node.gpu_flip_step", "steps", SerializedParamValue::Int { value: SIDE_BY_SIDE_SUBSTEPS as i32 });
    let mut run = LiquidRun::offline(row, def, 1);
    // A frame publishes the rows its tick ran with: Box3D runs a tick behind,
    // so tick k's outcome is the next frame's row.
    let first = run.step();
    let mut before = run.body(&first);
    let mut out = Vec::with_capacity(SIDE_BY_SIDE_TICKS);
    for _ in 0..SIDE_BY_SIDE_TICKS {
        let probe = run.step();
        let after = run.body(&probe);
        let predicted = f64::from(before.linear_velocity[1]) + f64::from(before.accel_shape[1]) * TICK;
        let velocity = f64::from(after.linear_velocity[1]);
        // Box3D applies the tick's reaction as one impulse: what it added
        // beyond gravity is the impulse.
        out.push(BodySubstep { dt: TICK, impulse: mass * (velocity - predicted), predicted, after: velocity });
        before = after;
    }
    out
}

/// The coupled pressure reaction on a density-neutral box, frame by frame,
/// against the FLIP Fluids engine on its own tank: dt, the pressure's
/// impulse, the velocity offered to the solve and the velocity after it.
/// The two are different particle discretizations, so the bound is a share
/// of the engine's impulse, not ulps; a body missing part of its pressure
/// faces misses by far more.
#[test]
fn gpu_flip_body_reaction_matches_engine_substeps() {
    /// Impulse bound, as a share of the engine's impulse through the frame.
    const IMPULSE_SHARE: f64 = 0.02;
    /// Velocity bound, as a share of g·dt per frame run.
    const VELOCITY_SHARE: f64 = 0.02;
    let (def, scene) = manifold_renderer::node_graph::liquid::conformance::gpu_flip_engine_tank();
    let expected = engine_tank(&scene);
    let mass = f64::from(scene.mass);
    let actual = gpu_flip_tank(def, mass);
    eprintln!("frame  dt(cpu/gpu)  impulse N·s (cpu/gpu)  predicted m/s (cpu/gpu)  after m/s (cpu/gpu)");
    for (k, (e, a)) in expected.iter().zip(&actual).enumerate() {
        eprintln!(
            "{k:>3}  {:.5}/{:.5}  {:>8.3}/{:>8.3}  {:>8.4}/{:>8.4}  {:>8.4}/{:>8.4}",
            e.dt, a.dt, e.impulse, a.impulse, e.predicted, a.predicted, e.after, a.after
        );
    }
    let mut divergence = None;
    let (mut cpu_total, mut gpu_total) = (0.0, 0.0);
    for (k, (e, a)) in expected.iter().zip(&actual).enumerate() {
        // The impulse is bounded cumulatively: the engine's adaptive substeps
        // bin a substep into a different frame than GPU FLIP's fixed ones, and
        // the momentum delivered is the invariant.
        cpu_total += e.impulse;
        gpu_total += a.impulse;
        let velocity_bound = VELOCITY_SHARE * G * TICK * (k + 1) as f64;
        let columns = [
            ("dt", e.dt, a.dt, 1e-9),
            ("impulse through this frame", cpu_total, gpu_total, IMPULSE_SHARE * cpu_total.abs()),
            ("predicted velocity", e.predicted, a.predicted, velocity_bound),
            ("velocity after", e.after, a.after, velocity_bound),
        ];
        if let Some((name, cpu, gpu, bound)) = columns.into_iter().find(|(_, cpu, gpu, bound)| (cpu - gpu).abs() > *bound) {
            divergence = Some(format!(
                "frame {k}: {name} is {gpu:.4} on the GPU against the engine's {cpu:.4} (bound {bound:.4}); \
                 this frame's impulse {:.4} against {:.4}, velocity after {:.4} against {:.4}",
                a.impulse, e.impulse, a.after, e.after
            ));
            break;
        }
    }
    assert!(divergence.is_none(), "GPU FLIP body reaction leaves the engine's: {}", divergence.unwrap_or_default());
}

/// The pressure iterations the body push is measured at, and the count taken
/// as converged.
const PUSH_ITERATIONS: [u32; 5] = [4, 6, 8, 12, 16];
const CONVERGED_ITERATIONS: u32 = 64;

/// What the liquid did to a box over a run's last two seconds.
#[derive(Clone, Debug)]
struct Push {
    /// Mean force and torque per tick, N and N·m.
    force: [f64; 3],
    torque: [f64; 3],
    /// The box's centre and one corner in world space, tick after tick, m.
    track: Vec<([f64; 3], [f64; 3])>,
    /// Each tick's pressure iterations and solves that reached the cap.
    solves: Vec<[u32; 2]>,
}

/// The step's solver words for the tick: pressure iterations, density
/// iterations, capped solves (liquid_stats.rs `SOLVER_WORDS`, the tail of the
/// capped array).
fn solver_words(run: &LiquidRun) -> [u32; 3] {
    let words: Vec<u32> = run.read("node.gpu_flip_step", "capped");
    let tail = &words[words.len() - 7..];
    [tail[0], tail[1], tail[2]]
}

/// `v` rotated by the unit quaternion `q` (xyzw).
fn rotate(q: [f32; 4], v: [f64; 3]) -> [f64; 3] {
    let [x, y, z, w] = q.map(f64::from);
    let t = [
        2.0 * (y * v[2] - z * v[1]),
        2.0 * (z * v[0] - x * v[2]),
        2.0 * (x * v[1] - y * v[0]),
    ];
    [
        v[0] + w * t[0] + (y * t[2] - z * t[1]),
        v[1] + w * t[1] + (z * t[0] - x * t[2]),
        v[2] + w * t[2] + (x * t[1] - y * t[0]),
    ]
}

fn push_at(row: &'static LiquidSolverRow, fixture: Fixture, iterations: u32) -> Push {
    let scene = box_scene(fixture);
    let mass = f64::from(scene.mass);
    let half = 0.5 * f64::from(scene.edge);
    let mut def = self::scene(row, fixture);
    set_type_param(&mut def, "node.gpu_flip_step", "iterations", SerializedParamValue::Int { value: iterations as i32 });
    let mut run = LiquidRun::offline(row, def, 1);
    let settled = run.steps(180);
    let mut previous = run.body(&settled);
    let (mut pushes, mut torques, mut track, mut solves) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for _ in 0..120 {
        let probe = run.step();
        assert_eq!(probe.get("ticks"), 1.0, "offline coupled frames each run a tick");
        let [pressure, _, capped] = solver_words(&run);
        solves.push([pressure, capped]);
        let body = run.body(&probe);
        pushes.push(liquid_push(&previous, &body));
        let spin = [0, 1, 2].map(|i| v3(body.angular_velocity)[i] - v3(previous.angular_velocity)[i]);
        let rows = [v3(body.inv_inertia_x), v3(body.inv_inertia_y), v3(body.inv_inertia_z)];
        let torque = solve3(rows, spin).expect("the box has a finite inertia");
        torques.push(torque.map(|l| l / TICK));
        let centre = v3(body.position_inv_mass);
        let arm = rotate(body.rotation, [half, half, half]);
        track.push((centre, [0, 1, 2].map(|i| centre[i] + arm[i])));
        previous = body;
    }
    let mean = |xs: &[[f64; 3]]| [0, 1, 2].map(|i| xs.iter().map(|x| x[i]).sum::<f64>() / xs.len() as f64);
    Push { force: mean(&pushes).map(|dv| mass * dv / TICK), torque: mean(&torques), track, solves }
}

fn distance(a: [f64; 3], b: [f64; 3]) -> f64 {
    let d = [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    dot(d, d).sqrt()
}

/// How far a run's box strays from the converged run's, tick by tick: the
/// largest centre and corner distance over the window, m.
fn stray(push: &Push, converged: &Push) -> [f64; 2] {
    push.track.iter().zip(&converged.track).fold([0.0, 0.0], |[c, k], ((a, ak), (b, bk))| {
        [c.max(distance(*a, *b)), k.max(distance(*ak, *bk))]
    })
}

/// GPU_FLIP_PRESSURE_SOLVE.md section 8 (Solids in the water): the net force
/// and torque the liquid puts on a submerged and a floating box at 4, 6, 8,
/// 12 and 16 pressure iterations and at the step's Auto, against 64 (at 32³
/// the solve reaches the f32 floor by 16). A count is steady when its mean
/// force and torque are within 1% of the converged run's (of the box's
/// weight, and weight times edge); the smallest steady count is the count
/// bodies need. Auto must be steady and must converge like a high fixed
/// count: on every tick of the window its solve meets the tolerance (never
/// the cap), and the counts it took are reported against the fixed ones.
/// Both bars are deterministic reads of the runs. Two statistics were tried
/// and rejected: a frame-to-frame shake threshold swings about 0.02 cells
/// between neighbouring counts with the solve's last bits, and the box's
/// stray from the 64-iteration trajectory measures the floating box's
/// sensitivity, not convergence — 16 tracks 64 bit for bit while 6, 8, 12
/// and Auto all stray 0.7–1.6 cells in no order. The strays are printed for
/// the record only.
#[test]
fn gpu_flip_body_push_against_iterations() {
    let row = LIQUID_SOLVERS.iter().find(|row| row.type_id == GPU_FLIP_DOMAIN_TYPE_ID).expect("the GPU FLIP row");
    let mut failed_auto = Vec::new();
    for fixture in [Fixture::SubmergedBox, Fixture::FloatingBox] {
        let scene = box_scene(fixture);
        let weight = f64::from(scene.mass) * G;
        let lever = weight * f64::from(scene.edge);
        let dx = cell(&scene);
        let converged = push_at(row, fixture, CONVERGED_ITERATIONS);
        eprintln!(
            "gpu_flip_body_push {fixture:?} at {CONVERGED_ITERATIONS}: force {:?} N against weight {weight:.1} N, torque {:?} N·m",
            converged.force, converged.torque
        );
        let mut smallest = None;
        // 0 is the step's Auto.
        for iterations in PUSH_ITERATIONS.into_iter().chain([0]) {
            let push = push_at(row, fixture, iterations);
            let off = [distance(push.force, converged.force) / weight, distance(push.torque, converged.torque) / lever];
            let steady = off.iter().all(|x| *x <= 0.01);
            let strays = stray(&push, &converged);
            let counts = push.solves.iter().fold([u32::MAX, 0], |[lo, hi], s| [lo.min(s[0]), hi.max(s[0])]);
            let capped: u32 = push.solves.iter().map(|s| s[1]).sum();
            let label = if iterations == 0 { "Auto".to_string() } else { iterations.to_string() };
            eprintln!(
                "gpu_flip_body_push {fixture:?} at {label}: force {:?} N, torque {:?} N·m; off converged by force {:.3}%, \
                 torque {:.3}%; {}..{} iterations a tick, {capped} capped; strays from {CONVERGED_ITERATIONS} by centre {:.4} / corner {:.4} cells{}",
                push.force,
                push.torque,
                off[0] * 100.0,
                off[1] * 100.0,
                counts[0],
                counts[1],
                strays[0] / dx,
                strays[1] / dx,
                if steady { ", steady" } else { "" }
            );
            assert!(counts[0] > 0, "{fixture:?} at {label}: a tick ran no pressure iterations");
            if iterations == 0 {
                if !steady || capped > 0 {
                    failed_auto.push((fixture, steady, capped));
                }
            } else if steady && smallest.is_none() {
                smallest = Some(iterations);
            }
        }
        eprintln!("gpu_flip_body_push {fixture:?}: smallest steady count {smallest:?}");
    }
    assert!(
        failed_auto.is_empty(),
        "Auto pressure iterations are not steady or hit the cap (fixture, steady, capped solves): {failed_auto:?}"
    );
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

/// `def` with a UniformForce impulse modifier aimed at the water, and the
/// binding id that fires it.
fn with_impulse(def: &EffectGraphDef) -> (EffectGraphDef, String) {
    use manifold_core::NodeId;
    use manifold_core::scene_modifier_preset::{SceneNodeRef, SceneTargetSelection};
    let mut recipe: EffectGraphDef =
        serde_json::from_str(include_str!("../../assets/scene-modifier-presets/UniformForce.json")).unwrap();
    let metadata = recipe.preset_metadata.as_mut().unwrap();
    for (id, value) in [("strength", 0.0), ("impulse_strength", 3.0), ("direction_x", 1.0), ("direction_y", 0.0)] {
        metadata.params.iter_mut().find(|param| param.id == id).unwrap().default_value = value;
        metadata.bindings.iter_mut().find(|binding| binding.id == id).unwrap().default_value = value;
    }
    let top = |node: &str| SceneNodeRef { scope: vec![], node: NodeId::new(node) };
    let instance = manifold_renderer::node_graph::scene_modifier_authoring::prepare_new_scene_modifier(
        def,
        &recipe,
        NodeId::new("impulse"),
        top("scene"),
        SceneTargetSelection::Explicit { objects: vec![top("water_object")] },
    )
    .expect("the impulse modifier targets the water");
    let def = manifold_core::scene_modifier_edit::insert_scene_modifier(def, 0, instance).unwrap().graph;
    let fire = def
        .preset_metadata
        .as_ref()
        .unwrap()
        .bindings
        .iter()
        .find(|binding| matches!(&binding.target, BindingTarget::SceneModifier { param_id, .. } if param_id == "fire"))
        .expect("the modifier exposes Fire")
        .id
        .clone();
    (def, fire)
}

impl LiquidRun {
    /// Fire `param` at the current transport position.
    fn fire(&mut self, param: &str, sequence: &mut u64) {
        let seconds = f64::from(self.transport) * TICK;
        let source = manifold_renderer::node_graph::FrameTime {
            seconds: manifold_core::Seconds(seconds),
            beats: manifold_core::Beats(seconds * 2.0),
            delta: manifold_core::Seconds::ZERO,
            frame_count: i64::from(self.frame),
        };
        let fired = self.runtime.fire_scene_impulse(param, source, sequence);
        assert_eq!(fired, Ok(true), "{}: the impulse was not accepted", self.domain_type);
    }

    fn applied_receipts(&mut self) -> usize {
        let mut count = 0;
        self.runtime.drain_scene_impulses(|_, _| count += 1);
        count
    }

    fn discarded_receipts(&mut self) -> usize {
        let mut count = 0;
        self.runtime.drain_discarded_scene_impulses(|_, _| count += 1);
        count
    }
}

/// I6: a hit fired while the liquid is held is discarded at once, never
/// moves the held frame, and never lands on resume: the resumed liquid is
/// bit-equal to a run that was never hit. The same hit fired while playing
/// lands exactly once, so the route is live.
#[test]
fn liquid_pause_discards_impulses() {
    for row in running(Check::PauseDiscardsImpulses) {
        for &fixture in Check::PauseDiscardsImpulses.fixtures(row.coupled) {
            let (def, fire) = with_impulse(&scene(row, fixture));
            let mut hit = LiquidRun::offline(row, def.clone(), 1);
            let mut control = LiquidRun::offline(row, def, 1);
            let mut sequence = 0;
            assert!(hit.steps(PLAY).get("count_b") > 0.0, "{} {fixture:?}: no liquid published", row.type_id);
            control.steps(PLAY);
            hit.hold();
            control.hold();
            let held = (hit.frame_words("particles_a"), hit.frame_words("particles_b"));

            hit.fire(&fire, &mut sequence);
            assert_eq!(hit.discarded_receipts(), 1, "{}: a held hit must be discarded at once", row.type_id);
            for frame in 1..=HOLD {
                hit.hold();
                control.hold();
                for (port, before) in [("particles_a", &held.0), ("particles_b", &held.1)] {
                    if let Some((i, x, y)) = first_difference(&hit.frame_words(port), before) {
                        panic!("{}: held frame {frame} after the hit changed {port} at word {i}: {x:#010x} against {y:#010x}", row.type_id);
                    }
                }
            }
            for frame in 1..=PLAY {
                hit.step();
                control.step();
                assert_eq!(hit.applied_receipts(), 0, "{}: resumed frame {frame} applied a discarded hit", row.type_id);
                assert_eq!(hit.discarded_receipts(), 0, "{}: resumed frame {frame} discarded again", row.type_id);
                for port in ["particles_a", "particles_b"] {
                    if let Some((i, x, y)) = first_difference(&hit.frame_words(port), &control.frame_words(port)) {
                        panic!("{}: resumed frame {frame} differs from the unhit run at {port} word {i}: {x:#010x} against {y:#010x}", row.type_id);
                    }
                }
            }

            hit.fire(&fire, &mut sequence);
            let mut applied = 0;
            for _ in 0..4 {
                hit.step();
                control.step();
                applied += hit.applied_receipts();
            }
            assert_eq!(applied, 1, "{}: a hit fired while playing must land exactly once", row.type_id);
            assert_eq!(hit.discarded_receipts(), 0, "{}: a playing hit was discarded", row.type_id);
            assert!(
                first_difference(&hit.frame_words("particles_b"), &control.frame_words("particles_b")).is_some(),
                "{}: a hit fired while playing did not move the liquid",
                row.type_id
            );
            eprintln!(
                "liquid_pause_discards_impulses {} {fixture:?}: held hit discarded, {PLAY} resumed frames equal the unhit run, playing hit landed once",
                row.type_id
            );
        }
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
            run.errors_expected = true;
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
        run.errors_expected = true;
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
            // Each run's physics scope restores the mode it found, so the live
            // run goes before the offline one starts.
            let held = live.clock.take();
            drop(live);
            let mut offline = LiquidRun::on(row, scene(row, fixture), 1, false, false, held);
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

/// Units in the last place between two finite f32 of one sign; `u64::MAX`
/// across signs or for a non-finite value.
fn ulps(a: f32, b: f32) -> u64 {
    if a.to_bits() == b.to_bits() || (a == 0.0 && b == 0.0) {
        return 0;
    }
    if !a.is_finite() || !b.is_finite() || a.is_sign_negative() != b.is_sign_negative() {
        return u64::MAX;
    }
    (i64::from(a.to_bits()) - i64::from(b.to_bits())).unsigned_abs()
}

/// Faces of `published` whose bits differ from `expected`, the most units
/// in the last place any sits off, and the first few.
fn face_mismatches(published: &[Vec<f32>; 3], expected: &[Vec<f32>; 3]) -> (usize, u64, Vec<String>) {
    let mut count = 0;
    let mut worst = 0;
    let mut first = Vec::new();
    for axis in 0..3 {
        assert_eq!(published[axis].len(), expected[axis].len(), "axis {axis} lengths");
        for (index, (got, want)) in published[axis].iter().zip(&expected[axis]).enumerate() {
            if got.to_bits() != want.to_bits() {
                count += 1;
                worst = worst.max(ulps(*got, *want));
                if first.len() < 5 {
                    first.push(format!("axis {axis} face {index}: {got:e} against {want:e}"));
                }
            }
        }
    }
    (count, worst, first)
}

/// P10 (D5), and GPU FLIP's half of seam P10: the frame publishes the faces the
/// solver's own grid gives at the frame's last tick, bit for bit where the
/// resample is a gather (the row's `ulps` otherwise, with its reason), over
/// the domain's cells with the solver's valid layers. Paused frames hold
/// them bit for bit; the next tick moves them, again as the grid gives them.
#[test]
fn liquid_face_grid_published() {
    for row in running(Check::FaceGridPublished) {
        let source = row.faces.as_ref().unwrap_or_else(|| panic!("{}: no face source", row.type_id));
        let (allowed, reason) = source.ulps;
        assert!(allowed == 0 || !reason.is_empty(), "{}: {allowed} ulps without a reason", row.type_id);
        let mut run = LiquidRun::offline(row, scene(row, Fixture::FaceGrid), 1);
        let played = run.steps(PLAY);
        let cells = ["face_cells_x", "face_cells_y", "face_cells_z"].map(|name| played.get(name) as u32);
        assert!(cells.iter().all(|&n| n > 0), "{}: the frame publishes {cells:?} cells", row.type_id);
        assert_eq!(played.get("face_valid_layers"), source.valid_layers as f32, "{}: valid layers", row.type_id);

        let published = run.faces(cells);
        let expected = (source.resample)(&run.read::<u8>(source.type_id, source.port), cells);
        let (differ, worst, first) = face_mismatches(&published, &expected);
        let total: usize = published.iter().map(Vec::len).sum();
        let moving = published.iter().flatten().filter(|v| **v != 0.0).count();
        let finite = published.iter().flatten().all(|v| v.is_finite());
        eprintln!(
            "liquid_face_grid_published {}: {cells:?} cells, {moving} of {total} faces moving; {differ} differ from {}.{} by at most {worst} ulps (allowed {allowed}) {first:?}",
            row.type_id, source.type_id, source.port
        );
        assert!(finite, "{}: a published face is not finite", row.type_id);
        assert!(moving > total / 100, "{}: only {moving} of {total} faces move after {PLAY} ticks", row.type_id);
        assert!(worst <= u64::from(allowed), "{}: published faces sit {worst} ulps from the solver's: {first:?}", row.type_id);

        for _ in 0..3 {
            let held = run.hold();
            assert_eq!(held.get("ticks"), 0.0, "{}: a paused frame ran a tick", row.type_id);
        }
        let held = run.faces(cells);
        let (changed, _, first) = face_mismatches(&held, &published);
        assert_eq!(changed, 0, "{}: paused frames moved the faces: {first:?}", row.type_id);

        run.step();
        let next = run.faces(cells);
        let (moved, _, _) = face_mismatches(&next, &published);
        let expected = (source.resample)(&run.read::<u8>(source.type_id, source.port), cells);
        let (differ, worst, first) = face_mismatches(&next, &expected);
        eprintln!(
            "liquid_face_grid_published {}: the next tick moved {moved} faces; {differ} differ by at most {worst} ulps",
            row.type_id
        );
        assert!(moved > 0, "{}: the next tick left the faces as they were", row.type_id);
        assert!(worst <= u64::from(allowed), "{}: the next tick's faces sit {worst} ulps off: {first:?}", row.type_id);
    }
}
