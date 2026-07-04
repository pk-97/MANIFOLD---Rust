//! Headless "render a 3D mesh graph to a PNG" harness (test-only).
//!
//! Builds a bare `Graph` + `Executor` (no content thread, no generator
//! wrapper), pre-binds the mesh vertex array + the readback color target the
//! way the parity tests do, runs one frame on the real GPU, and reads the
//! Rgba16Float color texture back as f16 → Reinhard-tonemapped Rgba8 PNG.
//!
//! The first user is a single PBR-lit cube: it proves the shipped Material
//! M1–M5 PBR path (pbr_material → render_mesh, envmap-IBL + direct light)
//! actually produces a lit image, and establishes the raw-executor mesh→PNG
//! pattern later phases reuse.

use manifold_core::{Beats, Seconds};
use manifold_gpu::GpuTextureFormat;

use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use crate::generators::mesh_common::MeshVertex;
use crate::node_graph::execution_plan::ResourceId;
use crate::node_graph::{
    Executor, FinalOutput, FrameTime, Graph, MetalBackend, NodeInstanceId, ParamValue, compile,
};
use crate::render_target::RenderTarget;

use super::{
    BakeEquirectEnvmap, CameraOrbit, GenerateCubeMesh, LightNode, PbrMaterial, Render3DMesh,
};

fn frame_time() -> FrameTime {
    FrameTime {
        beats: Beats(0.0),
        seconds: Seconds(0.0),
        delta: Seconds(1.0 / 60.0),
        frame_count: 0,
    }
}

/// Resolve the `ResourceId` allocated for a node's named output port.
fn output_resource(
    plan: &crate::node_graph::ExecutionPlan,
    node: NodeInstanceId,
    port: &str,
) -> ResourceId {
    for step in plan.steps() {
        if step.node == node {
            for &(name, id) in &step.outputs {
                if name == port {
                    return id;
                }
            }
        }
    }
    panic!("no output `{port}` on node {node:?}");
}

/// Decode one IEEE-754 binary16 value to f32 (no `half` dependency).
fn half_to_f32(h: u16) -> f32 {
    let sign = if (h >> 15) & 1 == 1 { -1.0f32 } else { 1.0f32 };
    let exp = (h >> 10) & 0x1f;
    let mant = h & 0x3ff;
    let mag = if exp == 0 {
        (mant as f32) * 2f32.powi(-24)
    } else if exp == 0x1f {
        if mant == 0 {
            f32::INFINITY
        } else {
            f32::NAN
        }
    } else {
        (1.0 + (mant as f32) / 1024.0) * 2f32.powi(exp as i32 - 15)
    };
    sign * mag
}

/// Reinhard-tonemap an HDR channel to 8-bit: `out = (v/(1+v)).clamp(0,1)*255`.
fn tonemap_channel(v: f32) -> u8 {
    let ldr = (v / (1.0 + v)).clamp(0.0, 1.0);
    (ldr * 255.0).round() as u8
}

/// Build the PBR-cube graph, render one 512×512 frame, return the color
/// texture as Reinhard-tonemapped row-major top-down Rgba8 bytes.
fn render_pbr_cube(w: u32, h: u32) -> Vec<u8> {
    let device = crate::test_device();
    let format = GpuTextureFormat::Rgba16Float;

    let mut g = Graph::new();

    let cube = g.add_node(Box::new(GenerateCubeMesh::new()));

    let cam = g.add_node(Box::new(CameraOrbit::new()));
    g.set_param(cam, "orbit", ParamValue::Float(0.7)).unwrap();
    g.set_param(cam, "tilt", ParamValue::Float(0.35)).unwrap();
    g.set_param(cam, "distance", ParamValue::Float(4.0)).unwrap();
    g.set_param(cam, "fov_y", ParamValue::Float(0.9)).unwrap();

    let mat = g.add_node(Box::new(PbrMaterial::new()));
    g.set_param(mat, "metallic", ParamValue::Float(1.0)).unwrap();
    g.set_param(mat, "roughness", ParamValue::Float(0.15)).unwrap();
    g.set_param(mat, "color_r", ParamValue::Float(0.80)).unwrap();
    g.set_param(mat, "color_g", ParamValue::Float(0.80)).unwrap();
    g.set_param(mat, "color_b", ParamValue::Float(0.82)).unwrap();
    g.set_param(mat, "color_a", ParamValue::Float(1.0)).unwrap();

    let light = g.add_node(Box::new(LightNode::new()));
    let env = g.add_node(Box::new(BakeEquirectEnvmap::new()));
    let render = g.add_node(Box::new(Render3DMesh::new()));
    let sink = g.add_node(Box::new(FinalOutput::new()));

    g.connect((cube, "vertices"), (render, "vertices")).unwrap();
    g.connect((cam, "out"), (render, "camera")).unwrap();
    g.connect((mat, "out"), (render, "material")).unwrap();
    g.connect((light, "out"), (render, "light")).unwrap();
    g.connect((env, "envmap"), (render, "envmap")).unwrap();
    g.connect((render, "color"), (sink, "in")).unwrap();

    let plan = compile(&g).unwrap();
    let r_color = output_resource(&plan, render, "color");
    let r_vertices = output_resource(&plan, cube, "vertices");

    let mut backend = MetalBackend::new(&device, w, h, format);

    // Readback target for the rendered color texture.
    let color_target = RenderTarget::new(&device, w, h, format, "mesh-snap-color");
    let color_slot = backend.pre_bind_texture_2d(r_color, color_target);

    // Pre-allocate the mesh vertex Array wire (cube → render). The bare
    // Graph + Executor path does not auto-allocate array buffers. Capacity =
    // the cube's `max_capacity` param default (read from the spec, not hard-
    // coded), which is 36 = exactly one cube.
    use crate::node_graph::primitive::PrimitiveSpec;
    let capacity = GenerateCubeMesh::PARAMS
        .iter()
        .find(|p| p.name == "max_capacity")
        .and_then(|p| match p.default {
            ParamValue::Float(n) => Some(n.round() as u64),
            _ => None,
        })
        .expect("cube max_capacity default");
    let vertex_buf =
        device.create_buffer_shared(capacity * std::mem::size_of::<MeshVertex>() as u64);
    backend.pre_bind_array(r_vertices, vertex_buf);

    let mut native_enc = device.create_encoder("mesh-snap-render");
    let mut exec = Executor::new(Box::new(backend));
    {
        let mut gpu = RendererGpuEncoder::new(&mut native_enc, &device);
        exec.execute_frame_with_gpu(&mut g, &plan, frame_time(), &mut gpu);
    }
    native_enc.commit_and_wait_completed();

    let out_tex = exec
        .backend()
        .texture_2d(color_slot)
        .expect("color texture retained");
    let bytes_per_row = w * 8;
    let total_bytes = u64::from(h * bytes_per_row);
    let readback_buf = device.create_buffer_shared(total_bytes);
    let mut readback_enc = device.create_encoder("mesh-snap-readback");
    readback_enc.copy_texture_to_buffer(out_tex, &readback_buf, w, h, bytes_per_row);
    readback_enc.commit_and_wait_completed();

    let ptr = readback_buf.mapped_ptr().expect("shared readback");
    let halves: &[u16] =
        unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };

    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for px in halves.chunks_exact(4) {
        rgba.push(tonemap_channel(half_to_f32(px[0])));
        rgba.push(tonemap_channel(half_to_f32(px[1])));
        rgba.push(tonemap_channel(half_to_f32(px[2])));
        // Alpha: no tonemap, just clamp.
        let a = half_to_f32(px[3]).clamp(0.0, 1.0);
        rgba.push((a * 255.0).round() as u8);
    }
    rgba
}

/// Render one PBR-lit cube to a PNG and assert the frame is actually lit.
///
/// Ignored by default: needs a GPU and writes a file. Point `MESH_SNAP_OUT`
/// at an absolute path to control the output location.
#[test]
#[ignore]
fn pbr_cube_renders_lit_frame_to_png() {
    let w = 512u32;
    let h = 512u32;
    let rgba = render_pbr_cube(w, h);

    // Non-black pixel fraction (any of R/G/B > 0). A broken dispatch — depth
    // state, vertex emission, missing envmap/light, transform composition —
    // shows up as an all-black frame here.
    let mut non_black = 0usize;
    for px in rgba.chunks_exact(4) {
        if px[0] != 0 || px[1] != 0 || px[2] != 0 {
            non_black += 1;
        }
    }
    let total = (w * h) as usize;
    let fraction = non_black as f64 / total as f64;
    println!("mesh-snap: non-black pixel fraction = {fraction:.4} ({non_black}/{total})");
    assert!(
        fraction > 0.01,
        "expected >1% non-black pixels, got {fraction:.4} ({non_black}/{total}) — likely a broken PBR dispatch"
    );

    let out_path = std::env::var("MESH_SNAP_OUT")
        .unwrap_or_else(|_| "target/mesh-snap/pbr_cube.png".to_string());
    if let Some(parent) = std::path::Path::new(&out_path).parent() {
        std::fs::create_dir_all(parent).expect("create output dir");
    }
    image::save_buffer(&out_path, &rgba, w, h, image::ExtendedColorType::Rgba8)
        .unwrap_or_else(|e| panic!("save {out_path}: {e}"));
    println!("mesh-snap: wrote {out_path}");
}
