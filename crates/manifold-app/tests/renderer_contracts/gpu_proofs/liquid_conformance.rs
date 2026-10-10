//! Checked against FLIP Fluids the engine's coupled tank (MIT); see THIRD_PARTY_NOTICES.md.
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
use manifold_node_engine::runtime::frame_status::{FrameRenderFailure, FrameRenderStatus};
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder;
use manifold_physics::clock::TICK;
use manifold_node_engine::particles::FluidParticle;
use manifold_nodes_water::liquid::bodies::LiquidBody;
use manifold_nodes_water::liquid::coupling::HANDOVER_BOUND;
use manifold_nodes_water::liquid::grid::{FACE_GRID_PORTS, face_len};
use manifold_nodes_water::testkit::conformance::{BoxScene, Check, FIXTURE_DENSITY, Fixture, LiquidSolverRow, LiquidTotals, STACK_HEIGHT, set_type_param};
use manifold_water_rigid::physics::{SimStep, native_ticks_on_this_thread};
use manifold_node_engine::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType, ScalarType};
use {manifold_node_engine::ports::ArrayType, manifold_node_engine::exec::effect_node::EffectNode, manifold_node_engine::exec::effect_node::EffectNodeContext, manifold_node_engine::exec::effect_node::EffectNodeType, manifold_node_engine::exec::effect_node::NodeErrorTap, manifold_node_engine::parameters::ParamDef, manifold_node_engine::persistence::PrimitiveRegistry, manifold_node_engine::scene::transform::Transform, manifold_nodes::bundled_presets::bundled_preset_def, manifold_nodes::bundled_presets::bundled_preset_type_ids};
use manifold_node_engine::runtime::preset_context::PresetContext;
use manifold_node_engine::runtime::PresetRuntime;
use manifold_nodes_water::runtime::WaterRuntimeExt;
use manifold_compositor::preset_thumbnail::{THUMBNAIL_HEIGHT, THUMBNAIL_WIDTH, render_preset_thumbnail};
use manifold_node_engine::gpu::render_target::RenderTarget;
use serde_json::json;


const PROBE_TYPE: &str = "test.liquid_probe";
const SIZE: u32 = 64;
/// Box3D's gravity in every fixture, m/s².
const G: f64 = 9.81;

/// Domain scalars the probe records, where the domain publishes them.
const DOMAIN_SCALARS: [&str; 8] =
    ["simulation_time", "display_time", "ticks", "epoch", "body_count", "handover_position", "handover_velocity", "handover_rotation"];
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

    fn depth_rule(&self) -> manifold_node_engine::scene::depth_rule::DepthRule {
        manifold_node_engine::scene::depth_rule::DepthRule::Terminal
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
    // Runtime expansion owns the impulse routes as well as the derived graph.
    // Keep the authored stack for that build, but rebase every host reference
    // through the flatten map so it still names the same leaf after grouping
    // is removed. Recipe-local references never cross the host boundary.
    let index = (!def.scene_modifiers.is_empty()).then(||
        manifold_core::scene_index::FlatSceneIndex::build(def).expect("modifier host indexes"));
    let mut bare = def.clone();
    let mut modifiers = std::mem::take(&mut bare.scene_modifiers);
    let metadata = bare.preset_metadata.take();
    let mut def = manifold_core::flatten::flatten_groups(&bare).expect("a liquid scene flattens");
    if let Some(index) = index {
        let rebase = |reference: &mut manifold_core::SceneNodeRef| {
            let mapped = index.node(reference).expect("modifier host reference maps to a leaf");
            let leaf = def.nodes.iter().find(|node| node.id == mapped.id).expect("flattened leaf exists");
            assert_eq!(leaf.node_id, mapped.node_id, "index and probe flatten must agree");
            reference.scope.clear();
            reference.node = leaf.node_id.clone();
        };
        for modifier in &mut modifiers {
            rebase(&mut modifier.scene);
            if let manifold_core::scene_modifier_preset::SceneTargetSelection::Explicit { objects } = &mut modifier.targets {
                objects.iter_mut().for_each(&rebase);
            }
            for frame in &mut modifier.mesh_frames {
                rebase(&mut frame.target);
                rebase(&mut frame.source);
            }
        }
    }
    def.scene_modifiers = modifiers;
    // Modifier IDs and recipe-local param IDs are unchanged: scalar and string
    // SceneModifier bindings keep their exact targets, values and order.
    def.preset_metadata = metadata;
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
    transport: f64,
    /// Ticks per frame: 1 is 60 fps, 2 is 30 fps, 2.5 is 24 fps.
    stride: f64,
    live: bool,
    /// The check provokes a node error, so a frame it fails is expected.
    errors_expected: bool,
    /// What the last warm-up frame published.
    start: Probe,
    clock: Option<Clocked>,
    /// The step every frame of this run passes the runtime.
    step: SimStep,
}

/// A device with a frame clock, as the app runs: every frame signals the
/// clock's event and drains what retired.
/// The shared harness device with the app's disk shader caches loaded once:
/// a cold device spends most of a liquid run compiling the solver's kernels.
fn shared_device() -> Arc<GpuDevice> {
    static LOADED: std::sync::Once = std::sync::Once::new();
    let device = &manifold_node_engine::testkit::gpu_harness::shared().device;
    LOADED.call_once(|| manifold_gpu::testkit::load_disk_shader_caches(device));
    Arc::clone(device)
}

struct Clocked {
    device: Arc<GpuDevice>,
    event: GpuEvent,
    retired: RetireQueue,
}

impl Clocked {
    fn new() -> Self {
        let device = Arc::new(GpuDevice::new_queued("gpu_proofs"));
        manifold_gpu::testkit::load_disk_shader_caches(&device);
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
        Self::on_fractional(row, def, f64::from(stride), live, dry, clock)
    }

    fn on_fractional(
        row: &'static LiquidSolverRow,
        def: EffectGraphDef,
        stride: f64,
        live: bool,
        dry: bool,
        clock: Option<Clocked>,
    ) -> Self {
        Self::on_project_rate(row, def, stride, live, dry, clock, 60.0)
    }

    fn on_project_rate(
        row: &'static LiquidSolverRow,
        def: EffectGraphDef,
        stride: f64,
        live: bool,
        dry: bool,
        clock: Option<Clocked>,
        project_fps: f64,
    ) -> Self {
        let device = clock.as_ref().map_or_else(shared_device, |clock| Arc::clone(&clock.device));
        let interval = manifold_core::Seconds(
            manifold_physics::SimRate::try_from(project_fps as u32).expect("authored rate").interval(),
        );
        let step = if live { SimStep::live(interval) } else { SimStep::export(interval) };
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
        runtime.set_sim_step(step);
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
            transport: 0.0,
            stride,
            live,
            errors_expected: false,
            start: Probe::EMPTY,
            clock,
            step,
        };
        // A poll count, not a wall-clock budget, so a loaded machine cannot fail it.
        let mut polls = 0u32;
        loop {
            run.render(true);
            if !run.runtime.warmup_pending() {
                break;
            }
            polls += 1;
            assert!(polls < 6000, "{}: asset warm-up did not finish", row.type_id);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        run.start = PROBE.get();
        run
    }

    fn render(&mut self, warming: bool) -> Probe {
        self.runtime.set_sim_step(self.step);
        let time = self.transport * TICK;
        let ctx = PresetContext {
            time,
            beat: time * 2.0,
            dt: if warming { 0.0 } else { (self.stride * TICK) as f32 },
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

/// Shows exactly the latest retired publication live. Schedule proofs compare
/// what the solver published; the display delay has its own proofs
/// (`display_cursor` tests and `liquid_frame_live_held_frame_matches_offline`).
/// An authored 0 is wired in because the loader re-wires an unwired frame to
/// its domain's cursor.
fn without_display_delay(def: &mut EffectGraphDef) {
    *def = manifold_core::flatten::flatten_groups(def).expect("a liquid preset flattens");
    let frames: Vec<u32> = def.nodes.iter().filter(|node| node.type_id == "node.liquid_frame").map(|node| node.id).collect();
    def.wires.retain(|wire| !(frames.contains(&wire.to_node) && wire.to_port == "display_cursor"));
    let exact = def.nodes.iter().map(|node| node.id).max().unwrap_or(0) + 1;
    def.nodes.push(serde_json::from_value(json!({"id": exact, "typeId": "node.value", "nodeId": "exact_display",
        "params": {"value": {"type": "Float", "value": 0.0}}})).expect("value node"));
    for frame in frames {
        def.wires.push(EffectGraphWire { from_node: exact, from_port: "out".into(), to_node: frame, to_port: "display_cursor".into() });
    }
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
/// '\.advance_worker\(' crates/manifold-nodes/src -g '!**/tests/**'`
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
fn liquid_coupling_collision_momentum() {
    coupling_collision(Check::CollisionMomentum);
}

/// The energy half of the collision proof above (D20: each exemption covers
/// one assertion).
#[test]
fn liquid_coupling_collision_energy() {
    coupling_collision(Check::CollisionEnergy);
}

fn coupling_collision(check: Check) {
    for row in running(check) {
        for &fixture in check.fixtures(row.coupled) {
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
            assert!(exchanged > 0.5, "{} ratio {density_ratio}: the box barely touched the pool ({exchanged:.3} kg·m/s)", row.type_id);
            if check == Check::CollisionEnergy {
                assert!(body_peak <= ENERGY_BOUND, "{} ratio {density_ratio}: body energy reached {body_peak:.4}× the start", row.type_id);
                assert!(
                    total_peak <= ENERGY_BOUND,
                    "{} ratio {density_ratio}: body plus liquid energy reached {total_peak:.4}× the start",
                    row.type_id
                );
                continue;
            }
            assert!(
                residual_norm <= MOMENTUM_BALANCE * exchanged,
                "{} ratio {density_ratio}: body plus liquid momentum drifted {residual:?} (|R| {residual_norm:.4e}) \
                 against {exchanged:.4e} kg·m/s exchanged",
                row.type_id
            );
        }
    }
}

/// The lowest and highest authored Sim Rates exercise the production scope,
/// clock, CFL scheduler, narrow-band history and renderer output sampling
/// without output-rate retuning; the rates between run the same path.
#[test]
fn liquid_export_matches_live_project_schedule() {
    let row = LIQUID_SOLVERS.iter().find(|row| row.type_id == GPU_FLIP_DOMAIN_TYPE_ID).unwrap();
    let rates = manifold_physics::SimRate::ALL;
    for rate in [rates[0], rates[rates.len() - 1]] {
        let project_fps = f64::from(rate.hz());
        for narrow in [false, true] {
            let make = |fps: f64, live| {
                let mut def = scene(row, Fixture::FaceGrid);
                set_type_param(&mut def, GPU_FLIP_DOMAIN_TYPE_ID, "resolution", SerializedParamValue::Int { value: 8 });
                set_type_param(&mut def, "node.gpu_flip_step", "narrow_band", SerializedParamValue::Float { value: if narrow { 1.0 } else { 0.0 } });
                if live {
                    without_display_delay(&mut def);
                }
                LiquidRun::on_project_rate(row, def, 60.0 / fps, live, false, None, project_fps)
            };
            let mut live = make(60.0, true);
            live.steps(30);
            // A live frame never waits: it publishes a tick once the tick's
            // fence retires, on a later frame. A paused frame retires the
            // 0.5 s publication without accepting more work.
            live.hold();
            let expected = live.particles("particles_b");
            for export_fps in [20.0, 24.0, 30.0, 60.0] {
                let mut export = make(export_fps, false);
                // Use the same half-second span at every output cadence.
                // Capture only at a shared endpoint; presentation delay is separate.
                export.steps(export_fps as u32 / 2);
                assert_eq!(bytemuck::cast_slice::<_, u32>(&export.particles("particles_b")),
                    bytemuck::cast_slice::<_, u32>(&expected),
                    "project {project_fps}, export {export_fps}, narrow {narrow}");
            }
            assert!(!expected.is_empty());
        }
    }
}

/// I5: a box at half the liquid's density, dropped tilted into the pool,
/// settles with its centre at the waterline (a half-density cube's draft in
/// any orientation), within half a cell. The waterline is the free surface
/// the solver's pressure sees, over the columns clear of the box, averaged
/// over the same second as the centre.
#[test]
fn liquid_floating_draft() {
    let mut misses = Misses::new(Check::FloatingDraft);
    for row in running(Check::FloatingDraft) {
        for &fixture in Check::FloatingDraft.fixtures(row.coupled) {
            let scene = box_scene(fixture);
            let dx = cell(&scene);
            let size = f64::from(scene.domain_size);
            let columns = (size / dx).round() as usize;
            // GPU FLIP's liquid is the union of balls of radius √3·dx/2
            // around its particles (the engine's liquid distance,
            // gpu_flip_step.wgsl particle_distance): over a flat layer on the
            // half-cell seeding lattice it stands 0.54 to 0.62 cells above
            // the top particle. Matter's particle fills the half-cell cube
            // around it.
            let reach = if row.type_id == GPU_FLIP_DOMAIN_TYPE_ID { 0.75f64.sqrt() * dx } else { 0.0 };
            let column = |c: f64| ((c + 0.5 * size) / dx).floor().clamp(0.0, (columns - 1) as f64) as usize;
            let column_centre = |i: usize| (i as f64 + 0.5) * dx - 0.5 * size;
            let surface = |particles: &[FluidParticle], at: [f64; 3]| {
                let mut tops = vec![f64::MIN; columns * columns];
                for p in particles.iter().filter(|p| p.position_radius[3] > 0.0) {
                    let [x, y, z] = [0, 1, 2].map(|i| f64::from(p.position_radius[i]));
                    if reach == 0.0 {
                        let top = &mut tops[column(z) * columns + column(x)];
                        *top = top.max(y + 0.25 * dx);
                        continue;
                    }
                    for cz in column(z - reach)..=column(z + reach) {
                        for cx in column(x - reach)..=column(x + reach) {
                            let d2 = (column_centre(cx) - x).powi(2) + (column_centre(cz) - z).powi(2);
                            if d2 < reach * reach {
                                let top = &mut tops[cz * columns + cx];
                                *top = top.max(y + (reach * reach - d2).sqrt());
                            }
                        }
                    }
                }
                let edge = f64::from(scene.edge);
                let mut open = Vec::new();
                for cz in 0..columns {
                    for cx in 0..columns {
                        let top = tops[cz * columns + cx];
                        let clear = (column_centre(cx) - at[0]).abs() >= edge && (column_centre(cz) - at[2]).abs() >= edge;
                        if clear && top > f64::MIN {
                            open.push(top);
                        }
                    }
                }
                (open.iter().sum::<f64>() / open.len().max(1) as f64, open.len())
            };
            let mut run = LiquidRun::offline(row, self::scene(row, fixture), 1);
            run.steps(240);
            let (mut sum, mut waterline, mut n) = (0.0, 0.0, 0.0);
            let (mut lo, mut hi) = (f64::MAX, f64::MIN);
            let mut fewest = usize::MAX;
            for _ in 0..60 {
                let probe = run.step();
                let at = v3(run.body(&probe).position_inv_mass);
                let (line, open) = surface(&run.particles("particles_b"), at);
                waterline += line;
                fewest = fewest.min(open);
                sum += at[1];
                n += 1.0;
                lo = lo.min(at[1]);
                hi = hi.max(at[1]);
            }
            let centre = sum / n;
            let waterline = waterline / n;
            assert!(fewest > columns, "{}: too few open columns ({fewest}) to read the surface", row.type_id);
            eprintln!(
                "liquid_floating_draft {}: centre {centre:.4} m, waterline {waterline:.4} m over at least {fewest} open \
                 columns, draft error {:.3}·dx, bob amplitude {:.4} m over the last second",
                row.type_id,
                (centre - waterline) / dx,
                0.5 * (hi - lo)
            );
            misses.check(row, "draft", (centre - waterline).abs() <= 0.5 * dx, || {
                format!(
                    "{}: box centre {centre:.4} is not within half a cell ({:.4}) of the waterline {waterline:.4}",
                    row.type_id,
                    0.5 * dx
                )
            });
        }
    }
    misses.assert_none("liquid_floating_draft");
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
                manifold_nodes::testkit::liquid_conformance_fixtures::gpu_flip_engine_tank()
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

/// A rest proof's run at Sim Rate `hz`, one tick per frame.
fn rest_run(row: &'static LiquidSolverRow, fixture: Fixture, hz: u32) -> LiquidRun {
    LiquidRun::on_project_rate(row, scene(row, fixture), f64::from(60 / hz), false, false, None, f64::from(hz))
}

/// GPU FLIP's stats word counting particles a solid would not release this
/// tick (`liquid_stats.rs`'s layout; word 9).
const PUSH_REFUSED_WORD: usize = 9;

/// One tick of a rest proof: every body row, the liquid's totals, the
/// particles left inside a solid (GPU FLIP; 0 for a liquid without the count),
/// and the handover error of the tick it settled (m, m/s, rad; NaN where the
/// domain does not publish it).
struct RestTick {
    bodies: Vec<LiquidBody>,
    liquid: LiquidTotals,
    refused: u32,
    handover: [f32; 3],
}

fn rest_tick(run: &mut LiquidRun, row: &LiquidSolverRow, bodies: usize) -> RestTick {
    let probe = run.step();
    assert_eq!(probe.get("ticks"), 1.0, "{}: a rest proof's frames each run one tick", row.type_id);
    assert_eq!(probe.get("body_count"), bodies as f32, "{}: the scene holds {bodies} bodies", row.type_id);
    let liquid = run.totals(row);
    assert_eq!(liquid.nonfinite, 0, "{}: a non-finite tick", row.type_id);
    let mut rows: Vec<LiquidBody> = run.read(run.domain_type, "bodies");
    rows.truncate(bodies);
    let refused = if row.type_id == GPU_FLIP_DOMAIN_TYPE_ID { run.totals_words(row)[PUSH_REFUSED_WORD] } else { 0 };
    let handover = ["handover_position", "handover_velocity", "handover_rotation"].map(|name| probe.get(name));
    RestTick { bodies: rows, liquid, refused, handover }
}

fn refused(ticks: &[RestTick]) -> u32 {
    ticks.iter().map(|t| t.refused).sum()
}

fn rest_ticks(run: &mut LiquidRun, row: &LiquidSolverRow, bodies: usize, n: u32) -> Vec<RestTick> {
    (0..n).map(|_| rest_tick(run, row, bodies)).collect()
}

/// How fast a box moved over one tick, m/s: its centre's travel plus its
/// turn times the half diagonal, from the tick-start rows either side.
fn tick_motion(before: &LiquidBody, after: &LiquidBody, edge: f64, dt: f64) -> f64 {
    let (a, b) = (v3(before.position_inv_mass), v3(after.position_inv_mass));
    let travel = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2) + (b[2] - a[2]).powi(2)).sqrt();
    let q = |body: &LiquidBody| body.rotation.map(f64::from);
    let (p, r) = (q(before), q(after));
    let cos_half = (p[0] * r[0] + p[1] * r[1] + p[2] * r[2] + p[3] * r[3]).abs().min(1.0);
    let turn = 2.0 * cos_half.acos();
    (travel + turn * 0.5 * edge * 3f64.sqrt()) / dt
}

/// What a window of ticks shows of body `k` at rest.
struct Rest {
    rms: f64,
    peak: f64,
    drift: f64,
    mean_y: f64,
}

fn rest_over(ticks: &[RestTick], k: usize, edge: f64, dt: f64) -> Rest {
    let motions: Vec<f64> = ticks.windows(2).map(|w| tick_motion(&w[0].bodies[k], &w[1].bodies[k], edge, dt)).collect();
    let rms = (motions.iter().map(|m| m * m).sum::<f64>() / motions.len() as f64).sqrt();
    let peak = motions.iter().copied().fold(0.0, f64::max);
    let at = |t: &RestTick| v3(t.bodies[k].position_inv_mass);
    let (first, last) = (at(&ticks[0]), at(&ticks[ticks.len() - 1]));
    let drift = ((last[0] - first[0]).powi(2) + (last[1] - first[1]).powi(2) + (last[2] - first[2]).powi(2)).sqrt();
    let mean_y = ticks.iter().map(|t| f64::from(t.bodies[k].position_inv_mass[1])).sum::<f64>() / ticks.len() as f64;
    Rest { rms, peak, drift, mean_y }
}

/// The share of the first tick's liquid mass gone by the lowest tick: in a
/// closed tank, lost mass is water removed. The mass is a fixed-order sum
/// over the particles, so an unchanged count gives the same bits.
fn water_lost(ticks: &[RestTick]) -> f64 {
    let start = ticks[0].liquid.mass;
    let least = ticks.iter().map(|t| t.liquid.mass).fold(f64::MAX, f64::min);
    assert!(start > 0.0, "the first tick weighs the liquid");
    ((start - least) / start).max(0.0)
}

/// A proof's misses, every row and rate reported before it fails. A miss a
/// row lists as known red is printed instead; one that never fails is a miss.
struct Misses {
    check: Check,
    misses: Vec<String>,
    known_failed: Vec<(&'static str, &'static str)>,
}

impl Misses {
    fn new(check: Check) -> Self {
        Self { check, misses: Vec::new(), known_failed: Vec::new() }
    }

    fn check(&mut self, row: &LiquidSolverRow, miss: &'static str, held: bool, what: impl FnOnce() -> String) {
        if held {
            return;
        }
        match row.known_red(self.check, miss) {
            Some(reason) => {
                eprintln!("known red, {reason}: {}", what());
                self.known_failed.push((row.type_id, miss));
            }
            None => self.misses.push(what()),
        }
    }

    fn assert_none(mut self, proof: &str) {
        for row in running(self.check) {
            for known in row.known_red.iter().filter(|known| known.check == self.check) {
                if !self.known_failed.contains(&(row.type_id, known.miss)) {
                    self.misses.push(format!(
                        "{}: the known red miss {:?} held in every case; if {} is fixed, drop it from the table",
                        row.type_id, known.miss, known.reason
                    ));
                }
            }
        }
        assert!(self.misses.is_empty(), "{proof}:\n{}", self.misses.join("\n"));
    }
}

/// I19 and I20, the Sim Rate the rest and handover proofs run at: the rest
/// motion grows with the tick, so the slowest rate is the hardest, and the
/// faster rates add nothing it does not already prove.
const REST_HZ: u32 = 15;

/// I20: a box at 0.05 and 0.5 of the liquid's density, let go 5 cm above
/// where it floats, comes to rest at 15 Hz: over the last 2 s of 6, RMS
/// motion under 1 cm/s and drift under 1 cm, its centre within a cell
/// of where it floats, and no water removed. Guards against a run that
/// passes by not moving: every frame runs a tick, the drop sets the water
/// moving, and the water holds the box up (its push over the window within
/// 30% of the box's weight).
#[test]
fn liquid_floating_rest() {
    let mut misses = Misses::new(Check::FloatingRest);
    for row in running(Check::FloatingRest) {
        for &fixture in Check::FloatingRest.fixtures(row.coupled) {
            let Fixture::FloatingAt { density_ratio } = fixture else { panic!("{fixture:?} is not a floating box") };
            let scene = box_scene(fixture);
            let (edge, dx) = (f64::from(scene.edge), cell(&scene));
            let floats_at = f64::from(scene.fill) + edge * (0.5 - f64::from(density_ratio));
            {
                let hz = REST_HZ;
                let dt = 1.0 / f64::from(hz);
                let mut run = rest_run(row, fixture, hz);
                // Drift is the gap between the window's ends, so a sustained bob
                // (BUG-u8nqr (GPU FLIP floating boxes keep bobbing at rest)) reads
                // only where the ends land off phase: after the 4 s settle a 2 s
                // window reads 2.7 cm on the light box, a 2 s settle only 0.8 cm.
                let mut ticks = rest_ticks(&mut run, row, 1, 4 * hz);
                let stirred = ticks.iter().map(|t| t.liquid.energy).fold(0.0, f64::max);
                let window = rest_ticks(&mut run, row, 1, 2 * hz);
                let push = window.windows(2).map(|w| w[1].bodies[0].linear_velocity[1] - w[0].bodies[0].linear_velocity[1]).sum::<f32>();
                let lift = (f64::from(push) / (window.len() - 1) as f64 / dt + G) / G;
                let rest = rest_over(&window, 0, edge, dt);
                ticks.extend(window);
                let lost = water_lost(&ticks);
                let what = format!("{} ratio {density_ratio} at {hz} Hz", row.type_id);
                eprintln!(
                    "liquid_floating_rest {what}: RMS motion {:.4} m/s, peak {:.4} m/s, drift {:.4} m, centre {:.4} m \
                     against {floats_at:.4} m, liquid lift {lift:.2} g, water stirred to {stirred:.3e} J, water lost {:.4}%, \
                     left inside a solid {}",
                    rest.rms, rest.peak, rest.drift, rest.mean_y, 100.0 * lost, refused(&ticks)
                );
                misses.check(row, "stirred", stirred > 1e-4, || format!("{what}: the drop never set the water moving"));
                misses.check(row, "lift", (lift - 1.0).abs() <= 0.3, || format!("{what}: the water holds up {lift:.2} of the box's weight"));
                misses.check(row, "centre", (rest.mean_y - floats_at).abs() <= dx, || {
                    format!("{what}: centre {:.4} m is not within a cell of {floats_at:.4} m", rest.mean_y)
                });
                misses.check(row, "lost", lost == 0.0, || format!("{what}: {:.4}% of the water removed", 100.0 * lost));
                let stuck = refused(&ticks);
                misses.check(row, "refused", stuck == 0, || format!("{what}: {stuck} particles left inside a solid"));
                misses.check(row, "rms", rest.rms <= 0.01, || format!("{what}: RMS motion {:.4} m/s at rest", rest.rms));
                misses.check(row, "drift", rest.drift <= 0.01, || format!("{what}: drifted {:.4} m in 2 s", rest.drift));
            }
        }
    }
    misses.assert_none("liquid_floating_rest");
}

/// I20: a box twice the liquid's density, flat on the floor against a wall
/// under 1 m of water, stays put at 15 Hz: over 2 s after a 1 s settle, RMS
/// motion under 1 mm/s, no tick faster than 5 mm/s (Box3D's soft
/// contact and the water's pressure leave sub-millimetre jitter; a visible
/// twitch is centimetres a second), and no water removed.
#[test]
fn liquid_resting_contact() {
    let mut misses = Misses::new(Check::RestingContact);
    for row in running(Check::RestingContact) {
        for &fixture in Check::RestingContact.fixtures(row.coupled) {
            let scene = box_scene(fixture);
            let edge = f64::from(scene.edge);
            {
                let hz = REST_HZ;
                let dt = 1.0 / f64::from(hz);
                let mut run = rest_run(row, fixture, hz);
                let mut ticks = rest_ticks(&mut run, row, 1, hz);
                let window = rest_ticks(&mut run, row, 1, 2 * hz);
                let rest = rest_over(&window, 0, edge, dt);
                ticks.extend(window);
                let lost = water_lost(&ticks);
                let what = format!("{} at {hz} Hz", row.type_id);
                eprintln!(
                    "liquid_resting_contact {what}: RMS motion {:.5} m/s, peak {:.5} m/s, drift {:.5} m, water lost {:.4}%, \
                     left inside a solid {}",
                    rest.rms, rest.peak, rest.drift, 100.0 * lost, refused(&ticks)
                );
                misses.check(row, "lost", lost == 0.0, || format!("{what}: {:.4}% of the water removed", 100.0 * lost));
                let stuck = refused(&ticks);
                misses.check(row, "refused", stuck == 0, || format!("{what}: {stuck} particles left inside a solid"));
                misses.check(row, "rms", rest.rms <= 1e-3, || format!("{what}: the resting box moves at {:.5} m/s RMS", rest.rms));
                misses.check(row, "peak", rest.peak <= 5e-3, || format!("{what}: the resting box twitched at {:.5} m/s", rest.peak));
            }
        }
    }
    misses.assert_none("liquid_resting_contact");
}

/// I20: a box at 0.3 of the liquid's density resting on one bottom edge on
/// the floor under 1 m of water, so water reaches under it, leaves the floor
/// within a second and is at the surface by 4 s, with no water removed.
#[test]
fn liquid_lift_off() {
    let mut misses = Misses::new(Check::LiftOff);
    for row in running(Check::LiftOff) {
        for &fixture in Check::LiftOff.fixtures(row.coupled) {
            let scene = box_scene(fixture);
            let dx = cell(&scene);
            let hz = 30;
            let mut run = rest_run(row, fixture, hz);
            let ticks = rest_ticks(&mut run, row, 1, 4 * hz);
            let height = |t: &RestTick| f64::from(t.bodies[0].position_inv_mass[1]);
            let start = height(&ticks[0]);
            let left = ticks.iter().position(|t| height(t) > start + 2.0 * dx);
            let end = height(&ticks[ticks.len() - 1]);
            let lost = water_lost(&ticks);
            let what = row.type_id.to_string();
            eprintln!(
                "liquid_lift_off {what}: left the floor at tick {left:?} of {hz} a second, centre {start:.4} m to {end:.4} m \
                 at 4 s, water lost {:.4}%, left inside a solid {}",
                100.0 * lost,
                refused(&ticks)
            );
            misses.check(row, "left", left.is_some_and(|tick| tick < hz as usize), || format!("{what}: the light box did not leave the floor within a second"));
            misses.check(row, "surface", end > f64::from(scene.fill) - f64::from(scene.edge), || format!("{what}: the light box sits at {end:.4} m, not at the surface"));
            misses.check(row, "lost", lost == 0.0, || format!("{what}: {:.4}% of the water removed", 100.0 * lost));
            let stuck = refused(&ticks);
            misses.check(row, "refused", stuck == 0, || format!("{what}: {stuck} particles left inside a solid"));
        }
    }
    misses.assert_none("liquid_lift_off");
}

/// I20: three boxes half again as dense as the liquid, stacked on the floor
/// under 1 m of water, settle at 30 Hz: over 3 s no body moves faster than
/// 2 m/s, over the last 1 s each body's RMS motion is under 1 cm/s, and no
/// water is removed. The evidence for repeated swaps within a tick
/// (section 7 (Deferred)).
#[test]
fn liquid_submerged_stack() {
    let mut misses = Misses::new(Check::SubmergedStack);
    for row in running(Check::SubmergedStack) {
        for &fixture in Check::SubmergedStack.fixtures(row.coupled) {
            let scene = box_scene(fixture);
            let (edge, hz) = (f64::from(scene.edge), 30);
            let dt = 1.0 / f64::from(hz);
            let mut run = rest_run(row, fixture, hz);
            let boxes = STACK_HEIGHT as usize;
            let ticks = rest_ticks(&mut run, row, boxes, 3 * hz);
            let peak = ticks
                .iter()
                .flat_map(|t| t.bodies.iter())
                .map(|b| dot(v3(b.linear_velocity), v3(b.linear_velocity)).sqrt())
                .fold(0.0, f64::max);
            let last = &ticks[ticks.len() - hz as usize..];
            let rests: Vec<Rest> = (0..boxes).map(|k| rest_over(last, k, edge, dt)).collect();
            let lost = water_lost(&ticks);
            let what = row.type_id.to_string();
            eprintln!(
                "liquid_submerged_stack {what}: peak speed {peak:.4} m/s, RMS motion over the last 1 s {:.4?} m/s, \
                 heights {:.4?} m, water lost {:.4}%, left inside a solid {}",
                rests.iter().map(|r| r.rms).collect::<Vec<_>>(),
                rests.iter().map(|r| r.mean_y).collect::<Vec<_>>(),
                100.0 * lost,
                refused(&ticks)
            );
            misses.check(row, "lost", lost == 0.0, || format!("{what}: {:.4}% of the water removed under the stack", 100.0 * lost));
            let stuck = refused(&ticks);
            misses.check(row, "refused", stuck == 0, || format!("{what}: {stuck} particles left inside a solid"));
            misses.check(row, "peak", peak <= 2.0, || format!("{what}: a stacked box reached {peak:.3} m/s"));
            for (k, rest) in rests.iter().enumerate() {
                misses.check(row, "rms", rest.rms <= 0.01, || format!("{what}: stacked box {k} still moves at {:.4} m/s", rest.rms));
            }
        }
    }
    misses.assert_none("liquid_submerged_stack");
}

/// I19 (D18): the floating rest boxes and the light box lifting off, at 15 Hz
/// over 1 s: Box3D ends every tick within 0.5 mm, 5 mm/s and
/// 0.1° of where the coupled motion law put the body. Guards: the box moves
/// more than a centimetre, so the law and the handoff are exercised, and the
/// check measures a nonzero error on some tick (one that never ran reads 0).
#[test]
fn liquid_handover_agreement() {
    let mut misses = Misses::new(Check::HandoverAgreement);
    let bound = [HANDOVER_BOUND.position, HANDOVER_BOUND.velocity, HANDOVER_BOUND.rotation];
    for row in running(Check::HandoverAgreement) {
        for &fixture in Check::HandoverAgreement.fixtures(row.coupled) {
            {
                let hz = REST_HZ;
                let mut run = rest_run(row, fixture, hz);
                let ticks = rest_ticks(&mut run, row, 1, hz);
                // The first frame's figures belong to no settled tick.
                let worst = ticks[1..].iter().fold([0.0f32; 3], |w, t| std::array::from_fn(|k| w[k].max(t.handover[k])));
                let start = v3(ticks[0].bodies[0].position_inv_mass);
                let travel = ticks
                    .iter()
                    .map(|t| {
                        let at = v3(t.bodies[0].position_inv_mass);
                        (0..3).map(|k| (at[k] - start[k]).powi(2)).sum::<f64>().sqrt()
                    })
                    .fold(0.0, f64::max);
                let what = format!("{} {fixture:?} at {hz} Hz", row.type_id);
                eprintln!(
                    "liquid_handover_agreement {what}: worst {:.2e} m, {:.2e} m/s, {:.3}°; travelled {travel:.3} m",
                    worst[0],
                    worst[1],
                    worst[2].to_degrees()
                );
                misses.check(row, "published", ticks.iter().all(|t| t.handover.iter().all(|v| v.is_finite())), || format!("{what}: no handover error published"));
                misses.check(row, "travel", travel > 0.01, || format!("{what}: the box never moved ({travel:.4} m)"));
                misses.check(row, "measured", worst[0] > 0.0 || worst[1] > 0.0, || format!("{what}: the check never measured"));
                for (k, unit) in ["m", "m/s", "rad"].into_iter().enumerate() {
                    misses.check(row, "bound", worst[k] <= bound[k], || format!("{what}: handover error {:.3e} {unit} over {:.1e}", worst[k], bound[k]));
                }
            }
        }
    }
    misses.assert_none("liquid_handover_agreement");
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
    with_force_and_impulse(def, 0.0)
}

/// CPU-only: probe preparation must preserve runtime force/Fire admission for
/// every solver, including objects authored behind group outputs.
#[test]
fn liquid_conformance_prepare_keeps_modifier_routes_cpu() {
    let registry = registry();
    for row in LIQUID_SOLVERS {
        let (owner, fire) = with_force_and_impulse(&scene(row, Fixture::DamBreak), 1.0);
        let prepared = prepare(row, &owner, &registry, false);
        assert_eq!(prepared.def.preset_metadata, owner.preset_metadata,
            "{}: probe preparation must preserve every binding", row.type_id);
        let expanded = manifold_node_engine::load::expand::prepare_scene_modifiers(
            &prepared.def, &registry,
        ).expect("rebased force and impulse expand");
        assert!(!expanded.impulse_routes.is_empty(), "{}: Fire has a runtime route", row.type_id);
        assert!(prepared.def.preset_metadata.as_ref().unwrap().bindings.iter()
            .any(|binding| binding.id == fire && matches!(binding.target, BindingTarget::SceneModifier { .. })));
        PresetRuntime::from_def(prepared.def, &registry, None)
            .unwrap_or_else(|error| panic!("{}: CPU runtime build after probe preparation: {error}", row.type_id));
    }
}

fn with_force_and_impulse(def: &EffectGraphDef, strength: f32) -> (EffectGraphDef, String) {
    use manifold_core::NodeId;
    use manifold_core::scene_modifier_preset::{SceneNodeRef, SceneTargetSelection};
    let mut recipe: EffectGraphDef =
        serde_json::from_str(manifold_nodes::testkit::assets::ASSETS_SCENE_MODIFIER_PRESETS_UNIFORMFORCE_JSON).unwrap();
    let metadata = recipe.preset_metadata.as_mut().unwrap();
    for (id, value) in [("strength", strength), ("impulse_strength", 3.0), ("direction_x", 1.0), ("direction_y", 0.0)] {
        metadata.params.iter_mut().find(|param| param.id == id).unwrap().default_value = value;
        metadata.bindings.iter_mut().find(|binding| binding.id == id).unwrap().default_value = value;
    }
    let top = |node: &str| SceneNodeRef { scope: vec![], node: NodeId::new(node) };
    let instance = manifold_nodes_scene::node_graph::scene_modifier_authoring::prepare_new_scene_modifier(
        def,
        &recipe,
        NodeId::new("impulse"),
        top("scene"),
        SceneTargetSelection::Explicit { objects: vec![SceneNodeRef::locate(def, &NodeId::new("water_object")).expect("water object in its authored scope")] },
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
        let seconds = self.transport * TICK;
        let source = manifold_node_engine::exec::effect_node::FrameTime {
            seconds: manifold_core::Seconds(seconds),
            beats: manifold_core::Beats(seconds * 2.0),
            delta: manifold_core::Seconds::ZERO,
            frame_count: i64::from(self.frame),
        };
        let fired = self.runtime.water().fire_scene_impulse(param, source, sequence);
        assert_eq!(fired, Ok(true), "{}: the impulse was not accepted", self.domain_type);
    }

    fn applied_receipts(&mut self) -> usize {
        let mut count = 0;
        self.runtime.water().drain_scene_impulses(|_, _| count += 1);
        count
    }

    fn discarded_receipts(&mut self) -> usize {
        let mut count = 0;
        self.runtime.water().drain_discarded_scene_impulses(|_, _| count += 1);
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
            // The domain counts the epoch. A frame's identity epoch changes
            // only when live ids are renumbered and starts again with each
            // simulation, so it may repeat across a Reset.
            assert!(fresh.get("epoch") > before.get("epoch"), "{}: Reset kept the epoch", row.type_id);
            assert!(
                first_difference(&run.frame_words("particles_b"), &b).is_some(),
                "{}: the fresh epoch never published",
                row.type_id
            );
            assert!(
                run.particles("particles_b").iter().all(|p| p.position_radius.iter().all(|v| v.is_finite())),
                "{}: the fresh epoch published non-finite particles",
                row.type_id
            );
            eprintln!("liquid_nonfinite_tick_not_published {}: {}", row.type_id, named.map_or("", String::as_str));
        }
    }
}

/// BUG-g75v.11: a late displayed frame at 128 keeps every resting particle
/// and performs exactly the same work as two ordinary fixed intervals.
#[test]
fn liquid_live_700ms_frame_preserves_128_pool_and_matches_two_fixed_steps() {
    let row = LIQUID_SOLVERS.iter().find(|row| row.type_id == GPU_FLIP_DOMAIN_TYPE_ID).unwrap();
    let make = || {
        let mut def = scene(row, Fixture::StillPool);
        set_type_param(&mut def, GPU_FLIP_DOMAIN_TYPE_ID, "resolution", SerializedParamValue::Int { value: 128 });
        LiquidRun::on(row, def, 1, true, false, Some(Clocked::new()))
    };
    let (expected, expected_totals) = {
        let mut reference = make();
        reference.steps(2);
        (reference.read::<FluidParticle>("node.liquid_state", "out"), reference.totals(row))
    };
    let mut overloaded = make();
    // Read the solver state directly: the live display ring may not have
    // published its first fenced frame during asset warm-up.
    let initial = overloaded.read::<FluidParticle>("node.liquid_state", "out");
    let live_count = |particles: &[FluidParticle]| particles.iter().filter(|p| p.position_radius[3] > 0.0).count();
    let seeded = live_count(&initial);
    assert!(seeded > 0);
    overloaded.stride = 42.0;
    let frame = overloaded.step();
    assert_eq!(frame.get("ticks"), 2.0);
    assert!((f64::from(frame.get("simulation_time")) - 2.0 * TICK).abs() < 1e-8);
    let particles = overloaded.read::<FluidParticle>("node.liquid_state", "out");
    let totals = overloaded.totals(row);
    assert_eq!(live_count(&particles), seeded, "overload removed resting water");
    assert_eq!(totals.nonfinite, 0);
    assert_eq!(bytemuck::cast_slice::<_, u32>(&particles), bytemuck::cast_slice::<_, u32>(&expected),
        "a 700 ms display frame must produce exactly the same water as two fixed intervals");
    assert_eq!(totals, expected_totals);
    // No resting-speed limit: native water never fully rests (the still-pool
    // bound comes from native measurements at 64), and the overload contract
    // is the bitwise match with two fixed intervals above.
    let fastest = particles.iter().filter(|p| p.position_radius[3] > 0.0)
        .map(|p| p.velocity.iter().map(|&v| f64::from(v).powi(2)).sum::<f64>().sqrt())
        .fold(0.0, f64::max);
    println!("128 pool after 700 ms overload: {seeded} particles retained, fastest {fastest:.3e} m/s, energy {:.3e} J; bitwise equal to two fixed steps", totals.energy);
}

/// Live GPU FLIP reports a bad state and recovers without an epoch reset or
/// lost transport time. Offline I8 above deliberately retains its old policy.
#[test]
fn liquid_nonfinite_live_flip_reseeds_without_stopping_clock() {
    let row = LIQUID_SOLVERS.iter().find(|row| row.type_id == GPU_FLIP_DOMAIN_TYPE_ID).unwrap();
    let mut def = scene(row, Fixture::StillPool);
    set_type_param(&mut def, GPU_FLIP_DOMAIN_TYPE_ID, "resolution", SerializedParamValue::Int { value: 16 });
    let tap = NodeErrorTap::new();
    let mut run = LiquidRun::new(row, def, 1, true, false);
    let before = run.steps(3);
    assert!(tap.take().is_empty());
    run.errors_expected = true;
    run.poison(row, 100);
    run.step();
    assert!(run.totals(row).nonfinite > 0, "the fixture must reach the non-finite path");
    let recovered = run.steps(3);
    assert_eq!(recovered.get("epoch"), before.get("epoch"), "recovery must not reset time");
    let expected = f64::from(before.get("simulation_time")) + 4.0 * TICK;
    assert!((f64::from(recovered.get("simulation_time")) - expected).abs() < 1e-6);
    assert_eq!(run.totals(row).nonfinite, 0, "reseeded state must be finite");
    assert!(run.particles("particles_b").iter().all(|p| {
        p.position_radius.iter().chain(p.velocity.iter()).all(|v| v.is_finite())
    }));
    assert!(tap.take().iter().any(|error| error.contains("non-finite") && error.contains("show continues")));
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

/// I13: 10 uncoupled live frames never wait on the GPU. The counter sits on
/// the frame clock's one wait, so it sees every waiter on this thread. The
/// runs use a device with a frame clock, as the app does.
#[test]
fn liquid_live_frames_never_wait() {
    let mut clock = Some(Clocked::new());
    for row in running(Check::LiveFramesNeverWait) {
        for &fixture in Check::LiveFramesNeverWait.fixtures(false) {
            let mut live = LiquidRun::on(row, scene(row, fixture), 1, true, false, clock.take());
            let before = FrameClock::waits_on_this_thread();
            let mut ticks = 0.0;
            for _ in 0..10 {
                ticks += live.step().get("ticks");
            }
            let waits = FrameClock::waits_on_this_thread() - before;
            clock = live.clock.take();
            eprintln!("liquid_live_frames_never_wait {}: {ticks} ticks, {waits} waits", row.type_id);
            assert_eq!(waits, 0, "{}: live frames waited on the GPU {waits} times", row.type_id);
            assert!(ticks >= 5.0, "{}: the live liquid barely ran ({ticks} ticks in 10 frames)", row.type_id);
        }
    }
}

/// Grouping accepted intervals into display frames preserves the water's
/// constant force, stamped hit and coupled body history, even when live drops
/// excess transport. Read solver state directly rather than the display ring.
#[test]
fn liquid_live_flip_force_and_coupling_match_accepted_progress() {
    const BODY_WORDS: usize = std::mem::size_of::<LiquidBody>() / 4;
    let row = LIQUID_SOLVERS.iter().find(|row| row.type_id == GPU_FLIP_DOMAIN_TYPE_ID).unwrap();
    let mut def = scene(row, Fixture::FloatingBox);
    set_type_param(&mut def, GPU_FLIP_DOMAIN_TYPE_ID, "resolution", SerializedParamValue::Int { value: 8 });
    let (def, fire) = with_force_and_impulse(&def, 2.0);
    let raw_dump = |run: &mut LiquidRun| {
        let probe = run.step();
        FrameDump {
            probe,
            rows: run.body_words(&probe),
            totals: run.totals_words(row),
            particles: run.read("node.liquid_state", "out"),
        }
    };
    // 20 fps is the rate that drops transport: two ticks a frame, the rest discarded.
    {
        let fps = 20u32;
        let make = |stride, clock| LiquidRun::on_fractional(
            row, def.clone(), stride, true, false, clock,
        );
        let mut reference = make(1.0, Some(Clocked::new()));
        let mut grouped = make(60.0 / f64::from(fps), Some(Clocked::new()));
        let initial = grouped.read::<FluidParticle>("node.liquid_state", "out");
        let seeded = initial.iter().filter(|particle| particle.position_radius[3] > 0.0).count();
        assert!(seeded > 0, "{fps} fps: floating-body fixture seeded no water");
        let (mut reference_sequence, mut grouped_sequence) = (0, 0);
        let (mut reference_receipts, mut grouped_receipts) = (0, 0);
        let mut previous_body = None;
        let mut coupling = 0.0;
        for frame in 1..=4u32 {
            if frame == 3 {
                // Both runs have accepted four ticks. Their transport differs
                // at 20 fps, but the hit belongs to the same simulation boundary.
                reference.fire(&fire, &mut reference_sequence);
                grouped.fire(&fire, &mut grouped_sequence);
            }
            let earlier = raw_dump(&mut reference);
            let later = raw_dump(&mut reference);
            let both = raw_dump(&mut grouped);
            reference_receipts += reference.applied_receipts();
            grouped_receipts += grouped.applied_receipts();
            assert_eq!(earlier.probe.get("ticks"), 1.0);
            assert_eq!(later.probe.get("ticks"), 1.0);
            assert_eq!(both.probe.get("ticks"), 2.0);
            let endpoint = f64::from(2 * frame) * TICK;
            for probe in [&later.probe, &both.probe] {
                assert!((f64::from(probe.get("simulation_time")) - endpoint).abs() < 1e-8,
                    "{fps} fps frame {frame}: wrong accepted endpoint");
            }
            assert_eq!(earlier.rows.len(), BODY_WORDS);
            assert_eq!(later.rows.len(), BODY_WORDS);
            assert_eq!(both.rows.len(), 2 * BODY_WORDS);
            for (label, grouped_words, reference_words) in [
                ("first body row", &both.rows[..BODY_WORDS], earlier.rows.as_slice()),
                ("second body row", &both.rows[BODY_WORDS..], later.rows.as_slice()),
                ("raw water", both.particles.as_slice(), later.particles.as_slice()),
                ("full totals", both.totals.as_slice(), later.totals.as_slice()),
            ] {
                assert_eq!(first_difference(grouped_words, reference_words), None,
                    "{fps} fps frame {frame}: {label} differs at equal accepted time");
            }
            let particles: &[FluidParticle] = bytemuck::cast_slice(&both.particles);
            assert_eq!(particles.iter().filter(|particle| particle.position_radius[3] > 0.0).count(), seeded,
                "{fps} fps frame {frame}: water was removed");
            assert!(particles.iter().all(|particle| {
                particle.position_radius.iter().chain(particle.velocity.iter()).all(|value| value.is_finite())
            }), "{fps} fps frame {frame}: nonfinite water");
            // The step's clock output closes into the boundary and is live;
            // the boundary's outward status is not wired in this fixture.
            let reference_clock: Vec<u32> = reference.read("node.gpu_flip_step", "clock_status");
            let grouped_clock: Vec<u32> = grouped.read("node.gpu_flip_step", "clock_status");
            assert_eq!(reference_clock.len(), 8);
            assert_eq!(grouped_clock.len(), 8);
            // The first word is the final recorded slot's dt: the one-step
            // shortcut has no inactive tail. Completed time and decisions agree.
            assert_eq!(&grouped_clock[1..], &reference_clock[1..],
                "{fps} fps frame {frame}: clock completion differs");
            assert_eq!(grouped_clock[1], (TICK as f32).to_bits());
            assert_eq!(grouped_clock[2], 0.0f32.to_bits());
            assert_eq!(grouped_clock[5], 0);
            assert_eq!(grouped.totals(row).nonfinite, 0);
            let bodies: &[LiquidBody] = bytemuck::cast_slice(&both.rows);
            for body in bodies {
                if let Some(previous) = previous_body.as_ref() {
                    coupling += liquid_push(previous, body).iter()
                        .map(|value| value * value).sum::<f64>().sqrt();
                }
                previous_body = Some(*body);
            }
        }
        assert_eq!(reference_receipts, 1, "{fps} fps reference: hit must apply once");
        assert_eq!(grouped_receipts, 1, "{fps} fps: hit must apply once");
        assert_eq!(reference.discarded_receipts(), 0);
        assert_eq!(grouped.discarded_receipts(), 0);
        assert!(coupling > 1e-4, "{fps} fps: no real body momentum exchange");
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

/// The liquid solvers `nodes` holds, at any group depth.
fn liquid_solvers(nodes: &[EffectGraphNode], found: &mut Vec<String>) {
    for node in nodes {
        if is_liquid_domain(&node.type_id) && !found.contains(&node.type_id) {
            found.push(node.type_id.clone());
        }
        if let Some(group) = &node.group {
            liquid_solvers(&group.nodes, found);
        }
    }
}

/// The cheapest bundled preset of each liquid solver (the 64-cell GPU FLIP
/// dam break, not the 112-cell cliff the catalogue lists first: 4 s against
/// 54 s a render). The test fails if a bundled solver is left uncovered.
const THUMBNAIL_PRESETS: [&str; 2] = ["WaterDamBreakParticles", "WaterDamBreakMatter"];

/// BUG-qssh (thumbnail differs run to run): one bundled preset of each
/// liquid solver renders the same thumbnail bytes with every core busy as with
/// the machine idle. Contention reaches the thumbnail through the solver, not
/// the preset, so one preset per solver covers it.
#[test]
fn liquid_thumbnail_ignores_contention() {
    let device = &shared_device();
    let mut bundled = Vec::new();
    for id in bundled_preset_type_ids(PresetKind::Generator) {
        liquid_solvers(&bundled_preset_def(&id).expect("bundled preset").nodes, &mut bundled);
    }
    let mut changed = Vec::new();
    let mut covered = Vec::new();
    for id in THUMBNAIL_PRESETS {
        let def = bundled_preset_def(&manifold_core::PresetTypeId::new(id)).unwrap_or_else(|| panic!("{id} is not a bundled preset"));
        liquid_solvers(&def.nodes, &mut covered);
        let render = || {
            render_preset_thumbnail(device, PresetKind::Generator, def.as_ref(), THUMBNAIL_WIDTH, THUMBNAIL_HEIGHT, false)
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
    }
    let missing: Vec<_> = bundled.iter().filter(|solver| !covered.contains(solver)).collect();
    assert!(missing.is_empty(), "no thumbnail preset covers {missing:?}");
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

/// A box a hundredth as dense as water in the GPU FLIP Dam Break stays
/// bounded in speed for 60 frames. At that ratio the body's per-step friction
/// gain ρ·h·f·A_wet/m sits far past 2, where an explicit friction reaction on
/// the body diverged; the pressure, implicit in the solve, is its only
/// reaction, as in the FLIP Fluids engine (`rigidfluidcoupling.cpp`). The
/// 40 m/s bound retains the fixture's existing physical runaway check;
/// direct native RK3 does not impose an authored speed limiter.
#[test]
fn gpu_flip_light_body_stays_bounded_in_the_dam_break() {
    const FRAMES: u32 = 60;
    const BOUND: f64 = 40.0;
    let row = LIQUID_SOLVERS.iter().find(|row| row.type_id == GPU_FLIP_DOMAIN_TYPE_ID).expect("the GPU FLIP row");
    let (def, scene) = manifold_nodes::testkit::liquid_conformance_fixtures::gpu_flip_dam_break_with_box(0.01);
    let mut run = LiquidRun::offline(row, def, 1);
    let mut peak = 0.0f64;
    for frame in 0..FRAMES {
        let probe = run.step();
        let body = run.body(&probe);
        let v = v3(body.linear_velocity);
        let speed = dot(v, v).sqrt();
        assert!(speed.is_finite(), "frame {frame}: the box's speed is not finite");
        assert!(speed <= BOUND, "frame {frame}: the box runs at {speed:.2} m/s, past {BOUND:.1}");
        assert_eq!(run.totals(row).nonfinite, 0, "frame {frame}: a non-finite tick");
        peak = peak.max(speed);
    }
    eprintln!("gpu_flip_light_body_stays_bounded_in_the_dam_break: {} kg box, peak {peak:.2} m/s", scene.mass);
}

use manifold_nodes::testkit::liquid_conformance_fixtures::LIQUID_SOLVERS;
