//! GPU_MPM_SOLVER_DESIGN.md section 12 (Invariants), the coupling row's
//! presentation: the Live Matter liquid and a Box3D body are drawn at the same
//! instant, run end to end through `WaterFloatingBoxMatter.json` offline. The
//! coupling physics (momentum, draft, lift, free flight, export, one Box3D
//! step per tick) runs for every coupled liquid in `liquid_conformance.rs`.
//!
//! Every readback is a whole-graph array dump after the frame has completed:
//! the domain's `bodies` row 0 is the coupled body's Box3D state at the
//! frame's display time. A probe node records the box pose the scene draws
//! and the scalars the domain and frame publish.

use std::borrow::Cow;
use std::cell::Cell;
use std::sync::Arc;

use manifold_core::params::ParamManifest;
use manifold_gpu::{GpuDevice, GpuTextureFormat};
use manifold_node_engine::runtime::frame_status::FrameRenderStatus;
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder;
use manifold_node_engine::water::fluid::TICK;
use manifold_node_engine::water::fluid_particles::FluidParticle;
use manifold_node_engine::water::liquid::bodies::LiquidBody;
use manifold_node_engine::water::physics::PhysicsStepScope;
use manifold_node_engine::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType, ScalarType};
use manifold_node_engine::{ports::ArrayType, exec::effect_node::EffectNode, exec::effect_node::EffectNodeContext, exec::effect_node::EffectNodeType, parameters::ParamDef, persistence::PrimitiveRegistry, scene::transform::Transform};
use manifold_node_engine::runtime::preset_context::PresetContext;
use manifold_node_engine::runtime::PresetRuntime;
use manifold_node_engine::gpu::render_target::RenderTarget;
use serde_json::{Value, json};


const PRESET: &str = include_str!("../../assets/generator-presets/WaterFloatingBoxMatter.json");
const PROBE_TYPE: &str = "test.matter_coupling_probe";
const SIZE: u32 = 64;
const G: f32 = 9.81;
/// rigid_body's cube edge per unit transform scale.
const CUBE_EDGE_PER_SCALE: f32 = 1.154_700_5;

#[derive(Clone, Copy, Debug)]
struct Probe {
    pose: Option<Transform>,
    display_time: f32,
    simulation_time: f32,
    blend: f32,
    count_a: f32,
}

const EMPTY_PROBE: Probe = Probe {
    pose: None,
    display_time: f32::NAN,
    simulation_time: f32::NAN,
    blend: f32::NAN,
    count_a: f32::NAN,
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
                optional("blend", SCALAR),
                optional("count_a", SCALAR),
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
        let mut probe = PROBE.get();
        if let Some(pose) = ctx.inputs.transform("pose") {
            probe.pose = Some(pose);
        }
        let scalar = |name: &str| ctx.inputs.scalar(name).and_then(|v| v.as_scalar());
        for (name, field) in [
            ("display_time", &mut probe.display_time),
            ("simulation_time", &mut probe.simulation_time),
            ("blend", &mut probe.blend),
            ("count_a", &mut probe.count_a),
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

fn float(value: f32) -> Value {
    json!({"type": "Float", "value": value})
}

fn node_mut(nodes: &mut [Value], id: u64) -> &mut Value {
    nodes.iter_mut().find(|n| n["id"] == id).unwrap_or_else(|| panic!("preset has node {id}"))
}

/// The Floating Box preset with the scene's settings and both probes.
fn preset(scene: &Scene) -> String {
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
        for (from, port) in [(1, "display_time"), (1, "simulation_time"), (9, "blend"), (9, "count_a"), (9, "particles_b")] {
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
    // The body takes density; the cube's volume is edge³.
    let density = scene.mass / scene.edge.powi(3);
    node_mut(nodes, 72)["params"]["density"] = float(density);
    // Card params own their bound node params, so the scene sets them there.
    for (id, value) in [("resolution", scene.resolution as f64), ("box_density", f64::from(density))] {
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
    serde_json::to_string(&doc).expect("preset serialises")
}

struct Run {
    runtime: PresetRuntime,
    target: RenderTarget,
    device: Arc<GpuDevice>,
    frame: u32,
    _offline: PhysicsStepScope,
}

impl Run {
    fn new(scene: &Scene) -> Self {
        let harness = manifold_node_engine::testkit::gpu_harness::shared();
        let device = Arc::clone(&harness.device);
        let mut registry = PrimitiveRegistry::with_builtin();
        registry.register(PROBE_TYPE, || Box::new(CouplingProbe::new()));
        let offline = PhysicsStepScope::for_render(true);
        let json = preset(scene);
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
        let mut run = Self { runtime, target, device, frame: 0, _offline: offline };
        let wait = manifold_node_engine::testkit::gpu_harness::BackgroundWait::new("matter coupling asset warmup");
        loop {
            run.render(0, true);
            if !run.runtime.warmup_pending() {
                break;
            }
            wait.hold();
        }
        run
    }

    fn render(&mut self, frame: u32, warming: bool) -> FrameRenderStatus {
        let time = f64::from(frame) * TICK;
        let ctx = PresetContext {
            time,
            beat: time * 2.0,
            dt: if warming { 0.0 } else { TICK as f32 },
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

    /// Render the next frame; returns the probe.
    fn step(&mut self) -> Probe {
        self.frame += 1;
        PROBE.set(EMPTY_PROBE);
        self.render(self.frame, false);
        PROBE.get()
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
    fn body(&self) -> LiquidBody {
        self.read::<LiquidBody>(manifold_core::liquid_domain::MATTER_DOMAIN_TYPE_ID, "bodies")[0]
    }

    fn frame_particles(&self, port: &str, count: usize) -> Vec<FluidParticle> {
        let mut all: Vec<FluidParticle> = self.read("node.matter_frame", port);
        all.truncate(count);
        all
    }
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

    // A tilted box released over a shallow pool, in and out of contact.
    let scene = Scene {
        domain_size: 2.0,
        resolution: 32,
        fill: 0.5,
        liquid_gravity: -G,
        open_faces: false,
        centre: [0.2, 0.9, 0.1],
        rotation: [0.21, 0.35, 0.13],
        edge: 0.5,
        mass: 62.5,
    };
    let mut run = Run::new(&scene);
    let mut previous_b: Option<Vec<FluidParticle>> = None;
    let mut last_simulation_time = 0.0;
    for _ in 0..40 {
        let probe = run.step();
        assert!(probe.simulation_time > last_simulation_time, "an offline coupled frame ran no tick");
        last_simulation_time = probe.simulation_time;
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
