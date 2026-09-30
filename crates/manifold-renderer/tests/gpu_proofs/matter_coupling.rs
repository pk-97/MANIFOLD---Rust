//! GPU_MPM_SOLVER_DESIGN.md section 12 (Invariants), the coupling row: the
//! Live Matter liquid and a Box3D body exchange momentum through the reaction
//! words, run end to end through `WaterFloatingBoxMatter.json` offline.
//!
//! Every readback is a whole-graph array dump after the frame has completed:
//! the domain's `bodies` row 0 is the coupled body's Box3D state at the
//! frame's display time, `reaction` is the tick's Σ Δv words, and the state's
//! `stats` are the liquid at the end of the tick. A probe node records the
//! box pose the scene draws and the scalars the domain and frame publish.

use std::borrow::Cow;
use std::cell::Cell;
use std::sync::Arc;

use manifold_core::params::ParamManifest;
use manifold_gpu::{GpuDevice, GpuTextureFormat};
use manifold_renderer::frame_status::FrameRenderStatus;
use manifold_renderer::gpu_encoder::GpuEncoder;
use manifold_renderer::node_graph::fluid::TICK;
use manifold_renderer::node_graph::fluid_particles::FluidParticle;
use manifold_renderer::node_graph::matter::{MatterBody, MatterTickStats, REACTION_WORDS, STATS_WORDS, WATER_DENSITY};
use manifold_renderer::node_graph::physics::PhysicsStepScope;
use manifold_renderer::node_graph::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType, ScalarType};
use manifold_renderer::node_graph::{
    ArrayType, EffectNode, EffectNodeContext, EffectNodeType, ParamDef, PrimitiveRegistry, Transform,
};
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;
use manifold_renderer::render_target::RenderTarget;
use serde_json::{Value, json};

use crate::harness;

const PRESET: &str = include_str!("../../assets/generator-presets/WaterFloatingBoxMatter.json");
const PROBE_TYPE: &str = "test.matter_coupling_probe";
const SIZE: u32 = 64;
const G: f64 = 9.81;
/// rigid_body's cube edge per unit transform scale.
const CUBE_EDGE_PER_SCALE: f32 = 1.154_700_5;
/// Reaction words are value·2^24/U.
const WORD_SCALE: f64 = 16_777_216.0;

#[derive(Clone, Copy, Debug)]
struct Probe {
    pose: Option<Transform>,
    display_time: f32,
    simulation_time: f32,
    momentum_unit: f32,
    blend: f32,
    count_a: f32,
    ticks: f32,
    body_count: f32,
}

const EMPTY_PROBE: Probe = Probe {
    pose: None,
    display_time: f32::NAN,
    simulation_time: f32::NAN,
    momentum_unit: f32::NAN,
    blend: f32::NAN,
    count_a: f32::NAN,
    ticks: f32::NAN,
    body_count: f32::NAN,
};

thread_local! {
    static PROBE: Cell<Probe> = const { Cell::new(EMPTY_PROBE) };
}

const fn optional(name: &'static str, ty: PortType) -> NodeInput {
    NodePort { name: Cow::Borrowed(name), ty, kind: PortKind::Input, required: false }
}

const SCALAR: PortType = PortType::Scalar(ScalarType::F32);

/// Records whichever of its inputs are wired. One instance sits at the top
/// level on the box pose, one inside the Live Matter group on the domain and
/// frame scalars. Its `particles_b` input only keeps frame B allocated, so
/// the dump carries it.
struct CouplingProbe {
    type_id: EffectNodeType,
    inputs: Vec<NodeInput>,
}

impl CouplingProbe {
    fn new() -> Self {
        Self {
            type_id: EffectNodeType::new(PROBE_TYPE),
            inputs: vec![
                optional("pose", PortType::Transform),
                optional("display_time", SCALAR),
                optional("simulation_time", SCALAR),
                optional("momentum_unit", SCALAR),
                optional("blend", SCALAR),
                optional("count_a", SCALAR),
                optional("ticks", SCALAR),
                optional("body_count", SCALAR),
                optional("particles_b", PortType::Array(ArrayType::of_known::<FluidParticle>())),
            ],
        }
    }
}

impl EffectNode for CouplingProbe {
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
        let mut probe = PROBE.get();
        if let Some(pose) = ctx.inputs.transform("pose") {
            probe.pose = Some(pose);
        }
        let scalar = |name: &str| ctx.inputs.scalar(name).and_then(|v| v.as_scalar());
        for (name, field) in [
            ("display_time", &mut probe.display_time),
            ("simulation_time", &mut probe.simulation_time),
            ("momentum_unit", &mut probe.momentum_unit),
            ("blend", &mut probe.blend),
            ("count_a", &mut probe.count_a),
            ("ticks", &mut probe.ticks),
            ("body_count", &mut probe.body_count),
        ] {
            if let Some(value) = scalar(name) {
                *field = value;
            }
        }
        PROBE.set(probe);
    }
}

/// One coupled scenario: the domain, the pool and the box.
#[derive(Clone, Copy)]
struct Scene {
    domain_size: f32,
    resolution: i64,
    fill: f32,
    liquid_gravity: f32,
    open_faces: bool,
    centre: [f32; 3],
    rotation: [f32; 3],
    edge: f32,
    mass: f32,
}

impl Scene {
    fn dx(&self) -> f64 {
        f64::from(self.domain_size) / self.resolution as f64
    }
}

fn float(value: f32) -> Value {
    json!({"type": "Float", "value": value})
}

fn node_mut(nodes: &mut [Value], id: u64) -> &mut Value {
    nodes.iter_mut().find(|n| n["id"] == id).unwrap_or_else(|| panic!("preset has node {id}"))
}

/// The Floating Box preset with the scene's settings and both probes; `dry`
/// removes the liquid (Live Matter group, surface and water object), leaving
/// the box in plain Box3D.
fn preset(scene: &Scene, dry: bool) -> String {
    let mut doc: Value = serde_json::from_str(PRESET).expect("preset parses");
    let nodes = doc["nodes"].as_array_mut().expect("nodes");
    {
        let group = &mut node_mut(nodes, 51)["group"];
        let inner = group["nodes"].as_array_mut().expect("group nodes");
        let params = &mut node_mut(inner, 1)["params"];
        params["domain_size"] = float(scene.domain_size);
        params["resolution"] = json!({"type": "Int", "value": scene.resolution});
        params["fill_height"] = float(scene.fill);
        params["gravity"] = float(scene.liquid_gravity);
        for face in ["closed_neg_x", "closed_pos_x", "closed_neg_y", "closed_pos_y", "closed_neg_z", "closed_pos_z"] {
            params[face] = json!({"type": "Bool", "value": !scene.open_faces});
        }
        inner.push(json!({"id": 90, "typeId": PROBE_TYPE, "nodeId": "coupling_probe_liquid"}));
        let wires = group["wires"].as_array_mut().expect("group wires");
        for (from, port) in [
            (1, "display_time"),
            (1, "simulation_time"),
            (1, "momentum_unit"),
            (1, "ticks"),
            (1, "body_count"),
            (9, "blend"),
            (9, "count_a"),
            (9, "particles_b"),
        ] {
            wires.push(json!({"fromNode": from, "fromPort": port, "toNode": 90, "toPort": port}));
        }
    }
    {
        let params = &mut node_mut(nodes, 71)["params"];
        let scale = scene.edge / CUBE_EDGE_PER_SCALE;
        for (axis, i) in [("x", 0), ("y", 1), ("z", 2)] {
            params[format!("pos_{axis}")] = float(scene.centre[i]);
            params[format!("rot_{axis}")] = float(scene.rotation[i]);
            params[format!("scale_{axis}")] = float(scale);
        }
    }
    node_mut(nodes, 72)["params"]["mass"] = float(scene.mass);
    // Card params own their bound node params, so the scene sets them there.
    for (id, value) in [("resolution", scene.resolution as f64), ("box_mass", f64::from(scene.mass))] {
        for list in ["params", "bindings"] {
            for card in doc["presetMetadata"][list].as_array_mut().expect("card list").iter_mut() {
                if card["id"] == id {
                    card["defaultValue"] = json!(value);
                }
            }
        }
    }
    let nodes = doc["nodes"].as_array_mut().expect("nodes");
    nodes.push(json!({"id": 90, "typeId": PROBE_TYPE, "nodeId": "coupling_probe_box", "handle": "coupling_probe_box"}));
    doc["wires"]
        .as_array_mut()
        .expect("wires")
        .push(json!({"fromNode": 70, "fromPort": "pose_0", "toNode": 90, "toPort": "pose"}));
    if dry {
        let liquid = |id: &Value| [51, 60, 62].iter().any(|l| id == l);
        doc["nodes"].as_array_mut().expect("nodes").retain(|n| !liquid(&n["id"]));
        doc["wires"]
            .as_array_mut()
            .expect("wires")
            .retain(|w| !liquid(&w["fromNode"]) && !liquid(&w["toNode"]));
        if let Some(bindings) = doc["presetMetadata"]["bindings"].as_array_mut() {
            bindings.retain(|b| b["target"]["nodeId"] != "matter_domain");
        }
    }
    serde_json::to_string(&doc).expect("preset serialises")
}

struct Run {
    runtime: PresetRuntime,
    target: RenderTarget,
    device: Arc<GpuDevice>,
    frame: u32,
    /// Ticks per exported frame: 1 is 60 fps, 2 is 30 fps.
    stride: u32,
    last_simulation_time: f32,
    _offline: PhysicsStepScope,
}

impl Run {
    fn new(scene: &Scene, dry: bool) -> Self {
        Self::at_stride(scene, dry, 1)
    }

    fn at_stride(scene: &Scene, dry: bool, stride: u32) -> Self {
        let harness = harness::shared();
        let device = Arc::clone(&harness.device);
        let mut registry = PrimitiveRegistry::with_builtin();
        registry.register(PROBE_TYPE, || Box::new(CouplingProbe::new()));
        let offline = PhysicsStepScope::for_render(true);
        let json = preset(scene, dry);
        let mut runtime = PresetRuntime::from_json_str_with_device(
            &json,
            &registry,
            Arc::clone(&device),
            SIZE,
            SIZE,
            GpuTextureFormat::Rgba16Float,
            None,
        )
        .unwrap_or_else(|e| panic!("coupled preset builds: {e}"));
        runtime.set_dump_all(true);
        let target = RenderTarget::new(&device, SIZE, SIZE, GpuTextureFormat::Rgba16Float, "matter-coupling");
        let mut run = Self { runtime, target, device, frame: 0, stride, last_simulation_time: 0.0, _offline: offline };
        let started = std::time::Instant::now();
        loop {
            run.render(0, true);
            if !run.runtime.warmup_pending() {
                break;
            }
            assert!(started.elapsed().as_secs() < 60, "asset warmup did not finish");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        run
    }

    fn render(&mut self, frame: u32, warming: bool) -> FrameRenderStatus {
        let frame_time = f64::from(self.stride) * TICK;
        let time = f64::from(frame) * frame_time;
        let ctx = PresetContext {
            time,
            beat: time * 2.0,
            dt: if warming { 0.0 } else { frame_time as f32 },
            width: SIZE,
            height: SIZE,
            output_width: SIZE,
            output_height: SIZE,
            aspect: 1.0,
            owner_key: 0x3C0,
            is_clip_level: false,
            frame_count: i64::from(frame),
            anim_progress: 0.0,
            trigger_count: 0,
        };
        let mut encoder = self.device.create_encoder("matter-coupling-frame");
        let status = {
            let mut gpu = GpuEncoder::new(&mut encoder, &self.device);
            self.runtime.render(&mut gpu, &self.target.texture, &ctx, &ParamManifest::default());
            gpu.frame_status()
        };
        encoder.commit_and_wait_completed();
        assert!(
            status == FrameRenderStatus::Complete || (warming && status == FrameRenderStatus::PendingGeometry),
            "frame {frame} rendered with status {status:?}"
        );
        status
    }

    /// Render the next frame; returns the probe and whether a liquid tick ran.
    fn step(&mut self) -> (Probe, bool) {
        self.frame += 1;
        PROBE.set(EMPTY_PROBE);
        self.render(self.frame, false);
        let probe = PROBE.get();
        let ticked = probe.simulation_time > self.last_simulation_time;
        if probe.simulation_time.is_finite() {
            self.last_simulation_time = probe.simulation_time;
        }
        (probe, ticked)
    }

    fn steps(&mut self, n: u32) {
        for _ in 0..n {
            self.step();
        }
    }

    fn read<T: bytemuck::Pod>(&self, type_id: &str, port: &str) -> Vec<T> {
        let dumps = self.runtime.dump_arrays_all();
        let dump = dumps
            .iter()
            .rev()
            .find(|d| d.type_id == type_id && d.port == port)
            .unwrap_or_else(|| panic!("no {type_id}.{port} in the dump"));
        let bytes = dump.buffer.size();
        let staging = self.device.create_buffer_shared(bytes);
        let mut encoder = self.device.create_encoder("matter-coupling-readback");
        encoder.copy_buffer_to_buffer(dump.buffer, &staging, bytes);
        encoder.commit_and_wait_completed();
        let ptr = staging.mapped_ptr().expect("shared staging buffer");
        // SAFETY: the copy has completed and nothing else writes the staging buffer.
        let raw = unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), bytes as usize) };
        bytemuck::pod_collect_to_vec(raw)
    }

    /// The coupled body's Box3D state at this frame's display time.
    fn body(&self) -> MatterBody {
        self.read::<MatterBody>("node.matter_domain", "bodies")[0]
    }

    /// This frame's tick: the body's velocity change and angular impulse
    /// (per unit mass, over dx) summed over the substeps, m/s.
    fn reaction(&self, momentum_unit: f32) -> ([f64; 3], [f64; 3]) {
        let words: Vec<i32> = self.read("node.matter_domain", "reaction");
        assert!(words.len() >= REACTION_WORDS as usize);
        let decode = |w: i32| f64::from(w) * f64::from(momentum_unit) / WORD_SCALE;
        (std::array::from_fn(|i| decode(words[i])), std::array::from_fn(|i| decode(words[6 + i])))
    }

    fn stats(&self) -> MatterTickStats {
        let words: Vec<u32> = self.read("node.matter_state", "stats");
        MatterTickStats::from_words(&words[..STATS_WORDS as usize])
    }

    fn frame_particles(&self, port: &str, count: usize) -> Vec<FluidParticle> {
        let mut all: Vec<FluidParticle> = self.read("node.matter_frame", port);
        all.truncate(count);
        all
    }
}

fn v3(v: [f32; 4]) -> [f64; 3] {
    [f64::from(v[0]), f64::from(v[1]), f64::from(v[2])]
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn solve3(rows: [[f64; 3]; 3], b: [f64; 3]) -> Option<[f64; 3]> {
    let [r0, r1, r2] = rows;
    let det = r0[0] * (r1[1] * r2[2] - r1[2] * r2[1]) - r0[1] * (r1[0] * r2[2] - r1[2] * r2[0])
        + r0[2] * (r1[0] * r2[1] - r1[1] * r2[0]);
    if det.abs() < 1e-30 {
        return None;
    }
    let with = |c: usize| {
        let mut m = rows;
        for (row, value) in m.iter_mut().zip(b) {
            row[c] = value;
        }
        let [a, b, c] = m;
        a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0]) + a[2] * (b[0] * c[1] - b[1] * c[0])
    };
    Some([with(0) / det, with(1) / det, with(2) / det])
}

/// Kinetic energy (linear and rotational) plus gravitational potential
/// relative to `y_ref`, in joules.
fn body_energy(body: &MatterBody, mass: f64, y_ref: f64) -> f64 {
    let v = v3(body.linear_velocity);
    let w = v3(body.angular_velocity);
    let rows = [v3(body.inv_inertia_x), v3(body.inv_inertia_y), v3(body.inv_inertia_z)];
    let rotational = solve3(rows, w).map_or(0.0, |iw| 0.5 * dot(w, iw));
    0.5 * mass * dot(v, v) + rotational + mass * G * (f64::from(body.position_inv_mass[1]) - y_ref)
}

/// A density-1 box held under 0.8 m of water: the liquid pushes up on it
/// with ρ0·|g|·V, within 5%.
#[test]
fn matter_coupling_hydrostatic_force() {
    let scene = Scene {
        domain_size: 2.0,
        resolution: 32,
        fill: 0.8,
        liquid_gravity: -G as f32,
        open_faces: false,
        centre: [0.0, 0.4, 0.0],
        rotation: [0.0; 3],
        edge: 0.4,
        mass: 64.0,
    };
    let mut run = Run::new(&scene, false);
    run.steps(120);
    let mass = f64::from(scene.mass);
    let (mut impulse, mut ticks) = (0.0, 0u32);
    let mut row_force = 0.0;
    let mut previous: Option<MatterBody> = None;
    let (mut lo, mut hi) = (f64::MAX, f64::MIN);
    for _ in 0..120 {
        let (probe, ticked) = run.step();
        let body = run.body();
        if let Some(prev) = previous {
            // The row's change over a tick is gravity plus the previous tick's reaction.
            row_force += mass * ((f64::from(body.linear_velocity[1]) - f64::from(prev.linear_velocity[1])) / TICK + G);
        }
        previous = Some(body);
        let y = f64::from(body.position_inv_mass[1]);
        lo = lo.min(y);
        hi = hi.max(y);
        if ticked {
            impulse += mass * run.reaction(probe.momentum_unit).0[1];
            ticks += 1;
        }
    }
    assert!(ticks >= 119, "offline coupled frames each run a tick ({ticks} of 120)");
    let force = impulse / (f64::from(ticks) * TICK);
    let row_force = row_force / 119.0;
    let expected = f64::from(WATER_DENSITY) * G * f64::from(scene.edge).powi(3);
    let error = (force - expected) / expected;
    eprintln!(
        "matter_coupling_hydrostatic_force: reaction {force:.1} N, from the body rows {row_force:.1} N, \
         expected ρ0·g·V {expected:.1} N, error {:.2}%, box y range {lo:.4}..{hi:.4}",
        error * 100.0
    );
    assert!(error.abs() <= 0.05, "buoyancy {force:.1} N is not within 5% of {expected:.1} N");
}

/// A density-0.5 box dropped into the pool floats with its centre at the
/// waterline, within half a cell. A cube's centre sits at the waterline at
/// half density in any orientation.
#[test]
fn matter_coupling_floating_equilibrium() {
    let scene = Scene {
        domain_size: 2.0,
        resolution: 32,
        fill: 0.5,
        liquid_gravity: -G as f32,
        open_faces: false,
        centre: [0.2, 0.78, 0.1],
        rotation: [0.21, 0.35, 0.13],
        edge: 0.5,
        mass: 62.5,
    };
    let mut run = Run::new(&scene, false);
    run.steps(240);
    let area = f64::from(scene.domain_size).powi(2);
    let displaced = f64::from(scene.mass) / f64::from(WATER_DENSITY);
    let (mut sum_y, mut sum_h, mut n) = (0.0, 0.0, 0.0);
    let (mut lo, mut hi) = (f64::MAX, f64::MIN);
    for _ in 0..60 {
        run.step();
        let y = f64::from(run.body().position_inv_mass[1]);
        let h = (f64::from(run.stats().volume) + displaced) / area;
        sum_y += y;
        sum_h += h;
        n += 1.0;
        lo = lo.min(y);
        hi = hi.max(y);
    }
    let (mean_y, volume_level) = (sum_y / n, sum_h / n);
    let dx = scene.dx();
    // The waterline is the free surface the particles show: the top of each dx
    // column clear of the box (centre height plus half the 2-per-cell spacing).
    // The volume level Σ V0·J reads low because Cohesion 0 caps J at 1 (D3),
    // dropping expansion, so it is printed but not gated on.
    let box_at = v3(run.body().position_inv_mass);
    let columns = (f64::from(scene.domain_size) / dx).round() as usize;
    let mut tops = vec![f64::MIN; columns * columns];
    let (mut min_x, mut max_x) = (f64::MAX, f64::MIN);
    for p in run.frame_particles("particles_b", usize::MAX).iter().filter(|p| p.position_radius[3] > 0.0) {
        let [x, y, z] = [0, 1, 2].map(|i| f64::from(p.position_radius[i]));
        min_x = min_x.min(x);
        max_x = max_x.max(x);
        if (x - box_at[0]).abs() < f64::from(scene.edge) || (z - box_at[2]).abs() < f64::from(scene.edge) {
            continue;
        }
        let half = 0.5 * f64::from(scene.domain_size);
        let cx = (((x + half) / dx) as usize).min(columns - 1);
        let cz = (((z + half) / dx) as usize).min(columns - 1);
        tops[cz * columns + cx] = tops[cz * columns + cx].max(y + 0.25 * dx);
    }
    let open: Vec<f64> = tops.into_iter().filter(|t| *t > f64::MIN).collect();
    assert!(open.len() > columns, "too few open columns ({}) to read the surface", open.len());
    let waterline = open.iter().sum::<f64>() / open.len() as f64;
    let incompressible = f64::from(scene.fill) + displaced / area;
    eprintln!(
        "matter_coupling_floating_equilibrium: centre {mean_y:.4} m, waterline {waterline:.4} m over {} open columns, \
         offset {:.3}·dx (volume level {volume_level:.4} m, incompressible level {incompressible:.4} m, \
         particle x extent {min_x:.4}..{max_x:.4}), bob amplitude {:.4} m over the last second",
        open.len(),
        (mean_y - waterline) / dx,
        0.5 * (hi - lo)
    );
    assert!(
        (mean_y - waterline).abs() <= 0.5 * dx,
        "box centre {mean_y:.4} is not within half a cell ({:.4}) of the waterline {waterline:.4}",
        0.5 * dx
    );
}

/// The first word where two dumps differ, as (index, left, right).
fn first_difference(a: &[u32], b: &[u32]) -> Option<(usize, u32, u32)> {
    if a.len() != b.len() {
        return Some((a.len().min(b.len()), a.len() as u32, b.len() as u32));
    }
    a.iter().zip(b).position(|(x, y)| x != y).map(|i| (i, a[i], b[i]))
}

/// What one exported frame leaves behind, as raw words.
struct FrameDump {
    probe: Probe,
    rows: Vec<u32>,
    reaction: Vec<u32>,
    stats: Vec<u32>,
    particles: Vec<u32>,
}

impl Run {
    fn dump(&mut self) -> FrameDump {
        let (probe, ticked) = self.step();
        assert!(ticked, "offline coupled frame {} ran no tick", self.frame);
        let mut stats: Vec<u32> = self.read("node.matter_state", "stats");
        stats.truncate(STATS_WORDS as usize);
        let mut reaction: Vec<u32> = self.read("node.matter_domain", "reaction");
        reaction.truncate(REACTION_WORDS as usize);
        FrameDump {
            probe,
            rows: self.read("node.matter_domain", "bodies"),
            reaction,
            stats,
            particles: self.read("node.matter_frame", "particles_b"),
        }
    }
}

/// D8 (export loses no time): a coupled scene exported at 30 fps runs two
/// ticks a frame, exchanging with Box3D between them, and matches the same
/// scene exported at 60 fps word for word at every shared instant: the body
/// rows of both ticks, the last tick's reaction, the liquid stats, the
/// particles and the drawn box pose.
#[test]
fn matter_coupling_export_frame_rate_independent() {
    let scene = Scene {
        domain_size: 2.0,
        resolution: 32,
        fill: 0.5,
        liquid_gravity: -G as f32,
        open_faces: false,
        centre: [0.2, 0.78, 0.1],
        rotation: [0.21, 0.35, 0.13],
        edge: 0.5,
        mass: 62.5,
    };
    let mut at_60 = Run::at_stride(&scene, false, 1);
    let mut at_30 = Run::at_stride(&scene, false, 2);
    let (mut contact_frames, mut exchanged) = (0u32, 0.0f64);
    for frame in 1..=60u32 {
        let earlier = at_60.dump();
        let later = at_60.dump();
        let both = at_30.dump();
        assert_eq!(both.probe.ticks, 2.0, "30 fps frame {frame} ran {} ticks", both.probe.ticks);
        assert_eq!(later.probe.ticks, 1.0);
        let count = both.probe.body_count as usize;
        let row_words = count * std::mem::size_of::<MatterBody>() / 4;
        assert!(count > 0 && both.rows.len() >= 2 * row_words);
        let checks = [
            ("first tick's body rows", first_difference(&both.rows[..row_words], &earlier.rows[..row_words])),
            (
                "second tick's body rows",
                first_difference(&both.rows[row_words..2 * row_words], &later.rows[..row_words]),
            ),
            ("reaction", first_difference(&both.reaction, &later.reaction)),
            ("stats", first_difference(&both.stats, &later.stats)),
            ("particles", first_difference(&both.particles, &later.particles)),
            (
                "display and simulation time",
                first_difference(
                    &[both.probe.display_time.to_bits(), both.probe.simulation_time.to_bits()],
                    &[earlier.probe.display_time.to_bits(), later.probe.simulation_time.to_bits()],
                ),
            ),
        ];
        for (what, difference) in checks {
            if let Some((i, a, b)) = difference {
                panic!(
                    "30 fps frame {frame} differs from 60 fps in {what} at word {i}: {a:#010x} ({}) against {b:#010x} ({})",
                    f32::from_bits(a),
                    f32::from_bits(b)
                );
            }
        }
        assert_eq!(both.probe.pose, earlier.probe.pose, "30 fps frame {frame} draws the box elsewhere");
        let reaction: Vec<i32> = bytemuck::cast_slice(&both.reaction).to_vec();
        if reaction.iter().any(|w| *w != 0) {
            contact_frames += 1;
            exchanged += reaction[..3].iter().map(|w| f64::from(*w).powi(2)).sum::<f64>().sqrt();
        }
    }
    eprintln!(
        "matter_coupling_export_frame_rate_independent: 60 frames at 30 fps equal 120 at 60 fps word for word; \
         {contact_frames} frames in contact, Σ|reaction| {exchanged:.3e} words"
    );
    assert!(contact_frames >= 30, "the box touched the water in only {contact_frames} of 60 frames");
}

/// A box falls onto a still, weightless pool at 0.1, 1 and 10 times the
/// water's density: from first contact, the body's energy never rises above
/// 1.01 × the initial total (body plus liquid) over 8 ticks. The same runs
/// report the momentum the exchange loses.
#[test]
fn matter_coupling_energy_light_body() {
    for ratio in [0.1f32, 1.0, 10.0] {
        let edge = 0.2f32;
        let scene = Scene {
            domain_size: 1.0,
            resolution: 32,
            fill: 0.5,
            liquid_gravity: 0.0,
            open_faces: true,
            centre: [0.0, 0.75, 0.0],
            rotation: [0.0; 3],
            edge,
            mass: ratio * WATER_DENSITY * edge.powi(3),
        };
        let mass = f64::from(scene.mass);
        let dx = scene.dx();
        let mut run = Run::new(&scene, false);
        // Fall until the bottom face is within a cell of the surface.
        let mut before_contact: Option<(MatterBody, MatterTickStats)> = None;
        for _ in 0..60 {
            run.step();
            let body = run.body();
            let bottom = f64::from(body.position_inv_mass[1]) - 0.5 * f64::from(edge);
            if bottom - f64::from(scene.fill) <= dx {
                break;
            }
            before_contact = Some((body, run.stats()));
        }
        let (start_body, start_liquid) = before_contact.expect("the box starts above the pool");
        let y_ref = f64::from(start_body.position_inv_mass[1]);
        let initial = body_energy(&start_body, mass, y_ref) + f64::from(start_liquid.kinetic + start_liquid.elastic);
        let momentum = |body: &MatterBody, liquid: &MatterTickStats| -> [f64; 3] {
            std::array::from_fn(|i| mass * f64::from(body.linear_velocity[i]) + f64::from(liquid.momentum[i]))
        };
        // Liquid stats are at the tick's end; the next frame's row is the body then.
        let mut liquid = start_liquid;
        let mut total_start: Option<[f64; 3]> = None;
        let (mut worst, mut worst_total) = (f64::MIN, f64::MIN);
        let (mut transferred, mut residual) = (0.0f64, [0.0f64; 3]);
        for tick in 0..8 {
            let (probe, ticked) = run.step();
            assert!(ticked, "offline coupled frames each run a tick");
            let body = run.body();
            let e_body = body_energy(&body, mass, y_ref);
            let e_total = e_body + f64::from(liquid.kinetic + liquid.elastic);
            worst = worst.max(e_body / initial);
            worst_total = worst_total.max(e_total / initial);
            let total = momentum(&body, &liquid);
            match total_start {
                None => total_start = Some(total),
                Some(start) => {
                    let gravity = [0.0, -mass * G * f64::from(tick) * TICK, 0.0];
                    residual = std::array::from_fn(|i| total[i] - start[i] - gravity[i]);
                }
            }
            let (dv, _) = run.reaction(probe.momentum_unit);
            transferred += mass * dot(dv, dv).sqrt();
            liquid = run.stats();
        }
        let residual_norm = dot(residual, residual).sqrt();
        eprintln!(
            "matter_coupling_energy_light_body ratio {ratio}: initial {initial:.4} J, body peak {worst:.4}×, \
             body+liquid peak {worst_total:.4}×; momentum residual {residual:?} (|R| {residual_norm:.4e}) \
             against {transferred:.4e} kg·m/s exchanged"
        );
        assert!(initial > 0.0);
        assert!(worst <= 1.01, "ratio {ratio}: body energy reached {worst:.4}× the initial total");
    }
}

fn free_fall_scene() -> Scene {
    Scene {
        domain_size: 2.0,
        resolution: 32,
        fill: 0.2,
        liquid_gravity: -G as f32,
        open_faces: false,
        centre: [0.2, 1.4, 0.1],
        rotation: [0.21, 0.35, 0.13],
        edge: 0.5,
        mass: 62.5,
    }
}

/// Before it touches the water the coupled box follows the same Box3D
/// trajectory as the box with no liquid at all, at the frame's display time.
#[test]
fn matter_coupling_free_flight_matches_box3d() {
    let scene = free_fall_scene();
    PROBE.set(EMPTY_PROBE);
    let mut dry = Run::new(&scene, true);
    // Plain Box3D presents the pose after f ticks at frame f; frame 0 is the start.
    let mut reference = vec![PROBE.get().pose.expect("dry start pose")];
    reference.extend((0..24).map(|_| dry.step().0.pose.expect("dry pose")));
    let distance = |a: &Transform, b: &Transform| {
        (0..3).map(|i| (a.pos[i] - b.pos[i]).abs().max((a.rot_euler[i] - b.rot_euler[i]).abs())).fold(0.0f32, f32::max)
    };
    let mut coupled = Run::new(&scene, false);
    let mut worst = 0.0f32;
    for _ in 0..18 {
        let (probe, ticked) = coupled.step();
        assert!(ticked);
        let (dv, dl) = coupled.reaction(probe.momentum_unit);
        assert!(dot(dv, dv) == 0.0 && dot(dl, dl) == 0.0, "the box touched the water in free flight");
        let pose = probe.pose.expect("coupled pose");
        let k = (f64::from(probe.display_time) / TICK).round() as usize;
        let nearest = (0..reference.len())
            .min_by(|&a, &b| distance(&pose, &reference[a]).total_cmp(&distance(&pose, &reference[b])))
            .expect("reference poses");
        assert_eq!(nearest, k, "the coupled box at display tick {k} matches Box3D after {nearest} ticks");
        worst = worst.max(distance(&pose, &reference[k]));
    }
    eprintln!("matter_coupling_free_flight_matches_box3d: worst pose difference {worst:.3e}");
    assert!(worst <= 1e-5, "coupled free flight differs from Box3D by {worst}");
}

/// The box and the liquid are drawn at the same instant: the domain presents
/// its display time (one tick behind the simulation), the frame blends
/// nothing, the drawn box sits at the Box3D state of that time, the preset
/// draws frame A, and frame A is the liquid published a tick earlier.
#[test]
fn matter_coupling_presentation_shares_display_time() {
    let doc: Value = serde_json::from_str(PRESET).expect("preset parses");
    let group = doc["nodes"].as_array().expect("nodes").iter().find(|n| n["id"] == 51).expect("Live Matter group");
    let drawn = group["group"]["wires"]
        .as_array()
        .expect("group wires")
        .iter()
        .find(|w| w["toNode"] == 10 && w["toPort"] == "particles")
        .expect("group output particles wire");
    assert_eq!((drawn["fromNode"].as_u64(), drawn["fromPort"].as_str()), (Some(9), Some("particles_a")));

    let scene = Scene { centre: [0.2, 0.9, 0.1], fill: 0.5, ..free_fall_scene() };
    let mut run = Run::new(&scene, false);
    let mut previous_b: Option<Vec<FluidParticle>> = None;
    for _ in 0..40 {
        let (probe, ticked) = run.step();
        assert!(ticked);
        let display = f64::from(probe.display_time);
        assert!(
            (f64::from(probe.simulation_time) - TICK - display).abs() < 1e-6,
            "display {display} is not one tick behind simulation {}",
            probe.simulation_time
        );
        assert_eq!(probe.blend, 0.0, "the frame blends between ticks");
        let pose = probe.pose.expect("box pose");
        let body = run.body();
        for i in 0..3 {
            assert!(
                (pose.pos[i] - body.position_inv_mass[i]).abs() < 1e-6,
                "drawn box {:?} is not the Box3D state {:?} at display time",
                pose.pos,
                body.position_inv_mass
            );
        }
        let count = probe.count_a as usize;
        let a = run.frame_particles("particles_a", count);
        if let Some(b) = previous_b.take() {
            assert!(count > 0 && a == b[..count.min(b.len())], "frame A is not the previous tick's frame B");
        }
        previous_b = Some(run.frame_particles("particles_b", usize::MAX));
    }
}
