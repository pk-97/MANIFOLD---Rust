//! The face grid on whole scenes: the matter component fused with a consumer
//! against its standalone kernels, and the seam's P10 demo, a face-speed
//! slice of GPU FLIP and MPM Dam Break side by side
//! (`docs/GPU_WHITEWATER_DESIGN.md` P1 (Grid outputs)).

use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_gpu::{GpuBuffer, GpuTextureFormat};

use crate::node_graph::primitives::divide_by_value::DivideByValue;
use manifold_node_engine::water::primitives::dot_products::DotProducts;
use manifold_node_engine::water::primitives::face_grid_scenes::{DIVISOR_ROW, matter_dam_break_faces};
use manifold_node_engine::testkit::liquid_surface::{Harness, params, read};
use manifold_node_engine::water::primitives::matter_face_component::MatterFaceComponent;
use manifold_node_engine::water::primitives::gpu_flip_preset::WaterScene;
use manifold_node_engine::water::primitives::gpu_flip_scene_tests::Run;
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder;
use manifold_node_engine::water::liquid::grid::{face_coords, face_dims, face_index, face_len};
use manifold_node_engine::water::liquid::lattice::PADDING_NODES;
use manifold_node_engine::water::matter::{MatterGridNode, MatterPoint};
use manifold_node_engine::parameters::ParamValue;
use manifold_node_engine::{exec::effect_node::NodeInstanceId, persistence::PrimitiveRegistry, exec::execution_plan::ResourceId};
use manifold_node_engine::runtime::preset_context::PresetContext;
use manifold_node_engine::runtime::PresetRuntime;
use manifold_node_engine::gpu::render_target::RenderTarget;

/// WaterDamBreakMatter's lattice at its default Resolution, in its unwired
/// 4 m domain.
const CELLS: [u32; 3] = [64; 3];
const FMT: GpuTextureFormat = GpuTextureFormat::Rgba16Float;
const FACE_INPUTS: [&str; 3] = ["face_u_in", "face_v_in", "face_w_in"];

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

    /// Every record `res` holds.
    fn read_all<T: bytemuck::Pod>(&self, res: ResourceId) -> Vec<T> {
        self.read(res, self.buffer(res).size as usize / std::mem::size_of::<T>())
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

    /// The matter frame's face grid, x, y and z.
    fn face_grid(&self) -> [Vec<f32>; 3] {
        std::array::from_fn(|axis| self.read(self.input_of("node.matter_frame", FACE_INPUTS[axis]).0, face_len(CELLS, axis) as usize))
    }

    /// Cells of the authored box holding a live point, x fastest.
    fn liquid_cells(&self) -> Vec<bool> {
        let layout = manifold_node_engine::water::fluid::domain_layout(None, 4.0, CELLS[0]).expect("the preset's domain");
        assert_eq!(layout.cells, CELLS);
        let points: Vec<MatterPoint> = self.read_all(self.output_of("node.matter_state", "out"));
        let mut liquid = vec![false; CELLS.iter().product::<u32>() as usize];
        for p in points.iter().filter(|p| p.id != 0) {
            let c: [u32; 3] = std::array::from_fn(|a| {
                let at = (f64::from(p.position[a] - layout.min[a]) / layout.cell_size).floor();
                (at.max(0.0) as u32).min(CELLS[a] - 1)
            });
            liquid[(c[0] + CELLS[0] * (c[1] + CELLS[1] * c[2])) as usize] = true;
        }
        liquid
    }
}

/// Faces carrying velocity around the liquid, as (carrying, counted), by
/// layer: the liquid cells' own faces; one layer out across the face's axis
/// (sharing an edge with an own face); one layer out along it. Tank-wall
/// faces are left out, since the wall condition zeroes them: the grid's edge,
/// and `padding` more layers where the walls stand inside the grid (GPU
/// FLIP's native grid, walls 1.5 cells in).
fn layer_shares(faces: &[Vec<f32>; 3], liquid: &[bool], cells: [u32; 3], padding: u32) -> [(usize, usize); 3] {
    let cell = |c: [u32; 3]| liquid[(c[0] + cells[0] * (c[1] + cells[1] * c[2])) as usize];
    let mut shares = [(0, 0); 3];
    for (axis, values) in faces.iter().enumerate() {
        let dims = face_dims(cells, axis);
        let own = |f: [u32; 3]| {
            let mut below = f;
            below[axis] = below[axis].wrapping_sub(1);
            (f[axis] < cells[axis] && cell(f)) || (f[axis] > 0 && cell(below))
        };
        let near = |f: [u32; 3], along: bool| {
            (0..3).filter(|&b| (b == axis) == along).any(|b| {
                [f[b].checked_sub(1), Some(f[b] + 1).filter(|&x| x < dims[b])].into_iter().flatten().any(|x| {
                    let mut g = f;
                    g[b] = x;
                    own(g)
                })
            })
        };
        for (index, &value) in values.iter().enumerate() {
            let f = face_coords(cells, axis, index);
            let in_wall = (0..3).any(|b| if b == axis {
                f[b] <= padding || f[b] >= cells[b] - padding
            } else {
                f[b] < padding || f[b] >= cells[b] - padding
            });
            if in_wall {
                continue;
            }
            let layer = if own(f) {
                0
            } else if near(f, false) {
                1
            } else if near(f, true) {
                2
            } else {
                continue;
            };
            shares[layer].1 += 1;
            shares[layer].0 += usize::from(value != 0.0);
        }
    }
    shares
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

/// Cell-centre speed through the middle depth, rows from the top of the tank.
fn speed_slice(faces: &[Vec<f32>; 3], cells: [u32; 3]) -> Vec<f32> {
    let k = cells[2] / 2;
    let mut slice = Vec::with_capacity((cells[0] * cells[1]) as usize);
    for j in (0..cells[1]).rev() {
        for i in 0..cells[0] {
            let v: [f32; 3] = std::array::from_fn(|a| {
                let mut far = [i, j, k];
                far[a] += 1;
                0.5 * (faces[a][face_index(cells, a, [i, j, k])] + faces[a][face_index(cells, a, far)])
            });
            slice.push(v.iter().map(|c| c * c).sum::<f32>().sqrt());
        }
    }
    slice
}

/// Black through red and yellow to white.
fn heat(t: f32) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0) * 3.0;
    [t, t - 1.0, t - 2.0].map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8)
}

/// L2, the seam's P10 demo: GPU FLIP and MPM Dam Break at 64 after the same 45
/// ticks, each solver's face grid as a cell-centre speed slice through the
/// middle depth on one colour scale, GPU FLIP left. Set FACE_GRID_DEMO_PNG to a
/// path to write the picture.
#[test]
fn face_grid_demo_gpu_flip_and_matter_side_by_side() {
    const FRAMES: usize = 45;
    const SCALE: usize = 4;
    const GAP: usize = 8;
    let scene = WaterScene::race_dam_break(64).with_faces();
    let mut gpu_flip = Run::new(scene);
    for _ in 0..FRAMES {
        gpu_flip.frame();
    }
    // The native solver grid: the authored 64 plus three cells of wall padding.
    let gpu_flip_cells = [gpu_flip.n() as u32; 3];
    assert_eq!(gpu_flip_cells, [67; 3]);
    let gpu_flip_faces = gpu_flip.face_grid();
    let gpu_flip_liquid: Vec<bool> = gpu_flip.water().iter().map(|&w| w > 0.5).collect();
    drop(gpu_flip);
    let mut matter = MatterRun::new(matter_dam_break_faces(None, false));
    for _ in 0..FRAMES {
        matter.frame();
    }
    let matter_faces = matter.face_grid();
    let matter_liquid = matter.liquid_cells();

    let slices = [speed_slice(&gpu_flip_faces, gpu_flip_cells), speed_slice(&matter_faces, CELLS)];
    let runs = [
        ("GPU FLIP", &gpu_flip_faces, &gpu_flip_liquid, &slices[0], gpu_flip_cells, 1),
        ("MPM", &matter_faces, &matter_liquid, &slices[1], CELLS, 0),
    ];
    for (name, faces, liquid, slice, cells, padding) in runs {
        for (axis, face) in faces.iter().enumerate() {
            assert!(face.iter().all(|v| v.is_finite()), "{name} axis {axis} holds a non-finite face");
        }
        let fastest = slice.iter().copied().fold(0.0_f32, f32::max);
        let moving: Vec<f32> = slice.iter().copied().filter(|&s| s > 0.0).collect();
        let mean = moving.iter().sum::<f32>() / moving.len().max(1) as f32;
        let shares = layer_shares(faces, liquid, cells, padding);
        let share = shares.map(|(carrying, counted)| format!("{carrying}/{counted} ({:.1}%)", 100.0 * carrying as f64 / counted.max(1) as f64));
        println!(
            "{name}: {} liquid cells; faces carrying velocity: own {}, one out across {}, one out along {}; slice: {} of {} cells moving, fastest {fastest:.3} m/s, mean {mean:.3} m/s",
            liquid.iter().filter(|&&l| l).count(),
            share[0],
            share[1],
            share[2],
            moving.len(),
            slice.len()
        );
        assert!((0.1..20.0).contains(&fastest), "{name} slice speed {fastest} m/s is not a falling column's");
        // Both solvers carry velocity on the liquid's own faces and across
        // one layer; only GPU FLIP's extension also fills the layer along the
        // axis (MATTER_FACE_VALID_LAYERS is 0 for that reason).
        // Keep the original 1% allowance for physically stationary faces:
        // this readback contains velocity, not validity. Wall zeros may
        // contribute to extension averages but must not seed valid zero
        // fronts ahead of the fluid (BUG-2dxjf).
        let gpu_flip = name == "GPU FLIP";
        let full = if gpu_flip { 3 } else { 2 };
        for (layer, &(carrying, counted)) in shares.iter().enumerate().take(full) {
            let allowed = if gpu_flip { counted / 100 } else { 0 };
            assert!(
                counted > 0 && counted - carrying <= allowed,
                "{name} layer {layer}: {carrying} of {counted} faces carry velocity"
            );
        }
    }

    let sides = [gpu_flip_cells[0] as usize, CELLS[0] as usize];
    let side = sides[0].max(sides[1]);
    let top = slices.iter().flatten().copied().fold(0.0_f32, f32::max);
    let (w, h) = (2 * side * SCALE + GAP, side * SCALE);
    let mut rgba = [48_u8, 48, 48, 255].repeat(w * h);
    for (panel, slice) in slices.iter().enumerate() {
        let n = sides[panel];
        for y in 0..n * SCALE {
            for x in 0..n * SCALE {
                let [r, g, b] = heat(slice[(y / SCALE) * n + x / SCALE] / top);
                let at = (y * w + panel * (side * SCALE + GAP) + x) * 4;
                rgba[at..at + 4].copy_from_slice(&[r, g, b, 255]);
            }
        }
    }
    println!("colour scale: white = {top:.3} m/s");
    if let Ok(path) = std::env::var("FACE_GRID_DEMO_PNG") {
        std::fs::write(&path, crate::headless_readback::encode_rgba8_png(&rgba, w as u32, h as u32)).expect("demo PNG writes");
        println!("wrote {path}");
    }
}
