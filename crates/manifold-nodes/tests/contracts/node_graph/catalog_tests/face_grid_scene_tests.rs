//! The face grid on whole scenes: the matter component fused with a consumer
//! against its standalone kernels (`docs/GPU_WHITEWATER_DESIGN.md` P1 (Grid
//! outputs)).

use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_gpu::{GpuBuffer, GpuTextureFormat};

use manifold_nodes_image::node_graph::primitives::divide_by_value::DivideByValue;
use manifold_nodes_water::primitives::dot_products::DotProducts;
use manifold_nodes_water::primitives::face_grid_scenes::{DIVISOR_ROW, matter_dam_break_faces};
use manifold_node_engine::testkit::array_harness::{Harness, params, read};
use manifold_nodes_water::primitives::matter_face_component::MatterFaceComponent;
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder;
use manifold_nodes_water::liquid::grid::face_len;
use manifold_nodes_water::liquid::lattice::PADDING_NODES;
use manifold_nodes_water::matter::MatterGridNode;
use manifold_node_engine::parameters::ParamValue;
use manifold_node_engine::{exec::effect_node::NodeInstanceId, persistence::PrimitiveRegistry, exec::execution_plan::ResourceId};
use manifold_node_engine::runtime::preset_context::PresetContext;
use manifold_node_engine::runtime::PresetRuntime;
use manifold_node_engine::gpu::render_target::RenderTarget;

/// WaterDamBreakMatter's lattice at its default Resolution, in its unwired
/// 4 m domain.
const CELLS: [u32; 3] = [64; 3];
const FMT: GpuTextureFormat = GpuTextureFormat::Rgba16Float;

/// A matter scene on the app's generator path, every output held past its
/// frame so any array reads back after it.
struct MatterRun {
    device: manifold_gpu::testkit::TestDevice,
    runtime: PresetRuntime,
    target: RenderTarget,
    frames: u32,
}

impl MatterRun {
    fn new(def: EffectGraphDef) -> Self {
        let device = manifold_gpu::testkit::test_device();
        let registry = PrimitiveRegistry::with_builtin();
        let mut runtime =
            PresetRuntime::from_def_with_device(def, &registry, device.arc(), 64, 64, FMT, None).expect("matter scene builds");
        runtime.set_dump_all(true);
        let target = RenderTarget::new(&device, 64, 64, FMT, "face-grid-scene");
        Self { device, runtime, target, frames: 0 }
    }

    fn frame(&mut self) {
        let ctx = PresetContext {
            time: f64::from(self.frames) / 60.0,
            beat: 0.0,
            dt: 1.0 / 60.0,
            width: 64,
            height: 64,
            output_width: 64,
            output_height: 64,
            aspect: 1.0,
            owner_key: 0,
            is_clip_level: false,
            frame_count: i64::from(self.frames),
            anim_progress: 0.0,
            trigger_count: 0,
        };
        let mut enc = self.device.create_encoder("face-grid-scene");
        {
            let mut gpu = GpuEncoder::new(&mut enc, &self.device);
            self.runtime.render(&mut gpu, &self.target.texture, &ctx, &manifold_core::params::ParamManifest::default());
        }
        enc.commit_and_wait_completed();
        self.frames += 1;
    }

    fn type_of(&self, node: NodeInstanceId) -> String {
        self.runtime.graph.get_node(node).map(|n| n.node.type_id().as_str().to_string()).unwrap_or_default()
    }

    /// The one step running `type_id`.
    fn step_of(&self, type_id: &str) -> &manifold_node_engine::exec::execution_plan::ExecutionStep {
        let mut steps = self.runtime.plan.steps().iter().filter(|s| self.type_of(s.node) == type_id);
        let step = steps.next().unwrap_or_else(|| panic!("no {type_id} step"));
        assert!(steps.next().is_none(), "two {type_id} steps");
        step
    }

    /// The resource on `input` of the one `type_id` node, and the type of the
    /// node that wrote it.
    fn input_of(&self, type_id: &str, input: &str) -> (ResourceId, String) {
        let res = self.step_of(type_id).inputs.iter().find(|(p, _)| *p == input).map(|&(_, r)| r).expect("input wired");
        let producer = self.runtime.plan.steps().iter().find(|s| s.outputs.iter().any(|&(_, r)| r == res)).expect("input produced");
        (res, self.type_of(producer.node))
    }

    fn output_of(&self, type_id: &str, port: &str) -> ResourceId {
        self.step_of(type_id).outputs.iter().find(|(p, _)| *p == port).map(|&(_, r)| r).expect("output port")
    }

    fn buffer(&self, res: ResourceId) -> &GpuBuffer {
        let backend = self.runtime.backend_for_test();
        backend.array_buffer(backend.slot_for(res).expect("resource bound")).expect("array resource")
    }

    /// The first `len` records of `res`, copied out of private storage.
    fn read<T: bytemuck::Pod>(&self, res: ResourceId, len: usize) -> Vec<T> {
        let buffer = self.buffer(res);
        let bytes = (len * std::mem::size_of::<T>()) as u64;
        assert!(buffer.size >= bytes, "{res:?} holds fewer than {len} records");
        let shared = self.device.create_buffer_shared(bytes.max(16));
        let mut enc = self.device.create_encoder("face-grid-readback");
        enc.copy_buffer_to_buffer(buffer, &shared, bytes);
        enc.commit_and_wait_completed();
        read(&shared, len)
    }

}

/// The v component folded with its consumer into one fused dispatch, on the
/// app's generator path (the region build, the fused node, axis 1 packed into
/// the fused uniforms, the domain's wired node counts over decoy params),
/// gives what the two standalone kernels give on the grid that dispatch read.
/// Comparing on the same grid keeps the proof clear of the simulation's own
/// run-to-run differences (BUG-qssh, matter render nondeterminism).
#[test]
fn matter_face_component_fused_matches_unfused() {
    let def = matter_dam_break_faces(Some(1), false);
    let fused = manifold_node_engine::freeze::install::fused_generator_view_for(&def).expect("the scene fuses");
    let mut run = MatterRun::new((*fused.def).clone());
    for _ in 0..3 {
        run.frame();
    }
    let (res, producer) = run.input_of("node.matter_frame", "face_v_in");
    assert_eq!(producer, "node.wgsl_compute", "the v component and its consumer run as one fused node");
    let len = face_len(CELLS, 1) as usize;
    let fused: Vec<f32> = run.read(res, len);

    let nodes = CELLS.map(|c| c + 1 + 2 * PADDING_NODES);
    let grid: Vec<MatterGridNode> = run.read(run.output_of("node.matter_state", "grid"), nodes.iter().product::<u32>() as usize);
    let mut harness = Harness::new();
    let input = harness.array(&grid, grid.len());
    let component_faces = |harness: &mut Harness, axis: u32| {
        let faces = harness.array::<f32>(&[], grid.len());
        let mut component = params(&[("nodes_x", nodes[0] as f32), ("nodes_y", nodes[1] as f32), ("nodes_z", nodes[2] as f32)]);
        component.insert("axis".into(), ParamValue::Enum(axis));
        let (_, errors) = harness.run(&mut MatterFaceComponent::new(), &[("grid", input.0)], &[("out", faces.0)], &component);
        assert!(errors.is_empty(), "{errors:?}");
        faces
    };
    let (u, v) = (component_faces(&mut harness, 0), component_faces(&mut harness, 1));
    assert_eq!(face_len(CELLS, 0), u64::from(DIVISOR_ROW), "the divisor's row is the u faces");
    let length = harness.array::<f32>(&[], 1);
    let dot = params(&[("row_length", DIVISOR_ROW as f32), ("rows", 1.0), ("max_rows", 1.0), ("root", 1.0)]);
    let (_, errors) = harness.run(&mut DotProducts::new(), &[("matrix", u.0), ("vector", u.0)], &[("out", length.0)], &dot);
    assert!(errors.is_empty(), "{errors:?}");
    let out = harness.array::<f32>(&[], grid.len());
    let (_, errors) = harness.run(&mut DivideByValue::new(), &[("values", v.0), ("divisor", length.0)], &[("out", out.0)], &params(&[]));
    assert!(errors.is_empty(), "{errors:?}");
    let unfused: Vec<f32> = read(&out.1, len);

    let peak = unfused.iter().fold(0.0_f32, |m, v| m.max(v.abs()));
    let moving = unfused.iter().filter(|v| **v != 0.0).count();
    assert!(moving > len / 100, "the grid moves after three frames: {moving} of {len} faces");
    let (worst, at) = fused.iter().zip(&unfused).enumerate().map(|(i, (a, b))| ((a - b).abs(), i)).fold((0.0, 0), |m, d| if d.0 > m.0 { d } else { m });
    println!("fused against standalone: worst {worst:e} at face {at}, peak {peak:e}, {moving} of {len} faces nonzero");
    assert!(worst <= 1e-6 * peak, "fused differs from the standalone kernels by {worst} at face {at} (peak {peak})");
}
