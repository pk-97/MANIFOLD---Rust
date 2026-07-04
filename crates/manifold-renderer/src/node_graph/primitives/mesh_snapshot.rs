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
use manifold_gpu::{
    GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
};

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

// ============================================================================
// MATERIAL M6 — value-level gpu_tests: albedo/metallic maps, alpha cutout,
// and the back-face lighting flip. Same headless raw-executor render + f16
// readback as `render_pbr_cube`, but each renders a hand-built quad/triangle
// with a controlled `base_color_map` and reads the linear f16 colour back
// (no tone-map) so we can assert on values, not just non-blackness.
// ============================================================================

use crate::node_graph::effect_node::{
    EffectNode, EffectNodeContext, EffectNodeType, ParamValues,
};
use crate::node_graph::parameters::ParamDef;
use crate::node_graph::ports::{ArrayType, NodeInput, NodeOutput, NodePort, PortKind, PortType};
use crate::node_graph::Source;
use super::{PhongMaterial, UnlitMaterial};

/// Test-only no-op source for `Array<MeshVertex>`. The caller pre-binds a
/// shared buffer to this node's `out` resource and CPU-writes the mesh
/// before executing — `evaluate` is intentionally empty (data already
/// lives in the buffer). Mirrors project_4d's `Vec4Source`.
struct MeshSource {
    type_id: EffectNodeType,
    inputs: Vec<NodeInput>,
    outputs: Vec<NodeOutput>,
}

impl MeshSource {
    fn new() -> Self {
        Self {
            type_id: EffectNodeType::new("test.mesh_source"),
            inputs: vec![],
            outputs: vec![NodePort {
                name: "out",
                ty: PortType::Array(ArrayType::of_known::<MeshVertex>()),
                kind: PortKind::Output,
                required: false,
            }],
        }
    }
}

impl EffectNode for MeshSource {
    fn type_id(&self) -> &EffectNodeType {
        &self.type_id
    }
    fn inputs(&self) -> &[NodeInput] {
        &self.inputs
    }
    fn outputs(&self) -> &[NodeOutput] {
        &self.outputs
    }
    fn parameters(&self) -> &[ParamDef] {
        &[]
    }
    fn evaluate(&mut self, _ctx: &mut EffectNodeContext<'_, '_>) {}
    fn array_output_capacity(
        &self,
        _port_name: &str,
        _params: &ParamValues,
        _input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        Some(0)
    }
}

/// A camera-facing quad in the z=0 plane spanning [-1, 1]², normal +z,
/// UVs 0..1 across the face. Two triangles, 6 vertices.
fn quad_verts() -> Vec<MeshVertex> {
    let n = [0.0, 0.0, 1.0];
    let v = |x: f32, y: f32, u: f32, w: f32| MeshVertex {
        position: [x, y, 0.0],
        _pad0: 0.0,
        normal: n,
        _pad1: 0.0,
        uv: [u, w],
        _pad2: [0.0, 0.0],
    };
    vec![
        v(-1.0, -1.0, 0.0, 0.0),
        v(1.0, -1.0, 1.0, 0.0),
        v(1.0, 1.0, 1.0, 1.0),
        v(-1.0, -1.0, 0.0, 0.0),
        v(1.0, 1.0, 1.0, 1.0),
        v(-1.0, 1.0, 0.0, 1.0),
    ]
}

/// A single triangle in the z=0 plane, geometric normal +z. Wound so its
/// front face (per the pipeline's winding) points toward +z; a camera on
/// the −z side therefore sees the BACK face (front_facing == false), which
/// is exactly what the back-face lighting flip has to handle.
fn back_facing_tri() -> Vec<MeshVertex> {
    let n = [0.0, 0.0, 1.0];
    let v = |x: f32, y: f32| MeshVertex {
        position: [x, y, 0.0],
        _pad0: 0.0,
        normal: n,
        _pad1: 0.0,
        uv: [0.5, 0.5],
        _pad2: [0.0, 0.0],
    };
    vec![v(-1.0, -1.0), v(1.0, -1.0), v(0.0, 1.0)]
}

/// Encode `texels` (row-major linear RGBA f32) as Rgba16Float bytes and
/// upload into a fresh SHADER_READ | CPU_UPLOAD source texture. Returns a
/// `RenderTarget` view ready to `pre_bind_texture_2d` into a graph wire.
fn upload_f16_rgba(
    device: &manifold_gpu::GpuDevice,
    w: u32,
    h: u32,
    texels: &[[f32; 4]],
) -> RenderTarget {
    assert_eq!(texels.len(), (w * h) as usize);
    let tex = device.create_texture(&GpuTextureDesc {
        width: w,
        height: h,
        depth: 1,
        format: GpuTextureFormat::Rgba16Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::SHADER_READ | GpuTextureUsage::CPU_UPLOAD,
        label: "m6-src-map",
        mip_levels: 1,
    });
    let mut bytes = Vec::with_capacity(texels.len() * 8);
    for t in texels {
        for &c in t {
            bytes.extend_from_slice(&half::f16::from_f32(c).to_bits().to_le_bytes());
        }
    }
    device.upload_texture(&tex, &bytes);
    RenderTarget::view_of(tex, "m6-src-map")
}

/// Read an Rgba16Float texture back as row-major linear `[f32; 4]` (no
/// tone-map — raw values for value-level assertions).
fn readback_rgba_f32(
    device: &manifold_gpu::GpuDevice,
    tex: &manifold_gpu::GpuTexture,
    w: u32,
    h: u32,
) -> Vec<[f32; 4]> {
    let bytes_per_row = w * 8;
    let total = u64::from(h * bytes_per_row);
    let buf = device.create_buffer_shared(total);
    let mut enc = device.create_encoder("m6-readback");
    enc.copy_texture_to_buffer(tex, &buf, w, h, bytes_per_row);
    enc.commit_and_wait_completed();
    let ptr = buf.mapped_ptr().expect("shared readback");
    let halves: &[u16] =
        unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };
    halves
        .chunks_exact(4)
        .map(|px| {
            [
                half_to_f32(px[0]),
                half_to_f32(px[1]),
                half_to_f32(px[2]),
                half_to_f32(px[3]),
            ]
        })
        .collect()
}

/// Shared plumbing: build `mesh_source → render_mesh → sink` plus the
/// caller-provided material/light/base_color_map wiring, pre-bind the mesh
/// buffer + colour target, run one frame, return the linear RGBA readback.
///
/// `configure` receives the mutable `Graph` and the `render` node id so the
/// caller can add + wire a material (and optionally a light + base_color_map
/// source node id, returned via the closure) before compilation. To keep the
/// pre-bind of a base_color_map source simple, the closure returns the
/// optional `(source_node, RenderTarget)` for that map.
fn render_mesh_scene(
    w: u32,
    h: u32,
    verts: &[MeshVertex],
    orbit: f32,
    distance: f32,
    build: impl FnOnce(&mut Graph, NodeInstanceId) -> (NodeInstanceId, Option<(NodeInstanceId, RenderTarget)>),
) -> Vec<[f32; 4]> {
    let device = crate::test_device();
    let format = GpuTextureFormat::Rgba16Float;

    let mut g = Graph::new();
    let mesh = g.add_node(Box::new(MeshSource::new()));
    let cam = g.add_node(Box::new(CameraOrbit::new()));
    g.set_param(cam, "orbit", ParamValue::Float(orbit)).unwrap();
    g.set_param(cam, "tilt", ParamValue::Float(0.0)).unwrap();
    g.set_param(cam, "distance", ParamValue::Float(distance)).unwrap();
    g.set_param(cam, "fov_y", ParamValue::Float(1.0)).unwrap();
    let render = g.add_node(Box::new(Render3DMesh::new()));
    let sink = g.add_node(Box::new(FinalOutput::new()));

    g.connect((mesh, "out"), (render, "vertices")).unwrap();
    g.connect((cam, "out"), (render, "camera")).unwrap();
    g.connect((render, "color"), (sink, "in")).unwrap();

    let (_material, bcmap) = build(&mut g, render);

    let plan = compile(&g).unwrap();
    let r_color = output_resource(&plan, render, "color");
    let r_mesh = output_resource(&plan, mesh, "out");

    let mut backend = MetalBackend::new(&device, w, h, format);
    let color_target = RenderTarget::new(&device, w, h, format, "m6-color");
    let color_slot = backend.pre_bind_texture_2d(r_color, color_target);

    // Mesh vertex buffer, pre-filled with the caller's geometry.
    let vert_bytes = (verts.len() * std::mem::size_of::<MeshVertex>()) as u64;
    let vert_buf = device.create_buffer_shared(vert_bytes);
    unsafe {
        vert_buf.write(0, bytemuck::cast_slice(verts));
    }
    backend.pre_bind_array(r_mesh, vert_buf);

    // Optional base_color_map source: pre-bind its uploaded texture.
    if let Some((src_node, rt)) = bcmap {
        let r_bcmap = output_resource(&plan, src_node, "out");
        backend.pre_bind_texture_2d(r_bcmap, rt);
    }

    let mut native_enc = device.create_encoder("m6-render");
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
    readback_rgba_f32(&device, out_tex, w, h)
}

/// Cutout: an unlit quad with a checkerboard-alpha `base_color_map`. In
/// Opaque mode every covered fragment is written (footprint). In Mask mode,
/// fragments whose sampled alpha is below the cutoff `discard`, leaving the
/// clear colour. Comparing the two passes isolates "discarded" (shaded in
/// Opaque, clear in Mask) from true background (clear in both), so the test
/// reads back BOTH sides of the cutoff without pixel-precise UV mapping.
#[test]
fn alpha_mask_cutout_discards_transparent_texels() {
    let (w, h) = (128u32, 128u32);
    // 8×8 checkerboard: rgb = white, alpha alternates 1 / 0.
    let cw = 8u32;
    let ch = 8u32;
    let checker: Vec<[f32; 4]> = (0..ch)
        .flat_map(|y| {
            (0..cw).map(move |x| {
                let a = if (x + y) % 2 == 0 { 1.0 } else { 0.0 };
                [1.0, 1.0, 1.0, a]
            })
        })
        .collect();

    let render_mode = |mask: bool| {
        let device = crate::test_device();
        let map = upload_f16_rgba(&device, cw, ch, &checker);
        drop(device);
        render_mesh_scene(w, h, &quad_verts(), std::f32::consts::FRAC_PI_2, 4.0, move |g, render| {
            let mat = g.add_node(Box::new(UnlitMaterial::new()));
            g.set_param(mat, "color_r", ParamValue::Float(1.0)).unwrap();
            g.set_param(mat, "color_g", ParamValue::Float(1.0)).unwrap();
            g.set_param(mat, "color_b", ParamValue::Float(1.0)).unwrap();
            g.set_param(
                mat,
                "alpha_mode",
                ParamValue::Enum(if mask { 1 } else { 0 }),
            )
            .unwrap();
            g.set_param(mat, "alpha_cutoff", ParamValue::Float(0.5)).unwrap();
            g.connect((mat, "out"), (render, "material")).unwrap();

            let src = g.add_node(Box::new(Source::new()));
            g.connect((src, "out"), (render, "base_color_map")).unwrap();
            (mat, Some((src, map)))
        })
    };

    let opaque = render_mode(false);
    let mask = render_mode(true);

    let lum = |p: [f32; 4]| p[0] + p[1] + p[2];
    let mut footprint = 0usize;
    let mut discarded = 0usize;
    let mut shaded = 0usize;
    let mut saw_opaque_texel = false; // shaded in both passes
    let mut saw_transparent_texel = false; // shaded in Opaque, clear in Mask
    for (o, m) in opaque.iter().zip(mask.iter()) {
        if lum(*o) > 0.1 {
            footprint += 1;
            if lum(*m) < 0.05 {
                discarded += 1;
                saw_transparent_texel = true;
            } else if lum(*m) > 0.5 {
                shaded += 1;
                saw_opaque_texel = true;
            }
        }
    }

    assert!(footprint > 200, "quad should cover a real area, got {footprint}");
    assert!(
        saw_transparent_texel,
        "no transparent texel discarded — cutout did not fire"
    );
    assert!(
        saw_opaque_texel,
        "no opaque texel shaded — Mask discarded everything"
    );
    // Both sides should be substantial (checkerboard ≈ half/half), proving
    // the discard is gated on alpha, not blanket.
    assert!(
        discarded * 5 > footprint && shaded * 5 > footprint,
        "expected a roughly balanced cutout: discarded={discarded} shaded={shaded} footprint={footprint}"
    );
}

/// Albedo modulation: an unlit quad with a uniform `base_color_map`. The
/// resolved surface colour is `base_color.rgb × map.rgb`; read back a
/// covered pixel and check it equals the product within f16 tolerance.
#[test]
fn base_color_map_modulates_albedo() {
    let (w, h) = (128u32, 128u32);
    let base = [0.4_f32, 0.5, 0.25, 1.0];
    let texel = [0.5_f32, 0.4, 0.6, 1.0];
    let expected = [base[0] * texel[0], base[1] * texel[1], base[2] * texel[2]];

    let device = crate::test_device();
    let map = upload_f16_rgba(&device, 2, 2, &[texel; 4]);
    drop(device);

    let out = render_mesh_scene(w, h, &quad_verts(), std::f32::consts::FRAC_PI_2, 4.0, move |g, render| {
        let mat = g.add_node(Box::new(UnlitMaterial::new()));
        g.set_param(mat, "color_r", ParamValue::Float(base[0])).unwrap();
        g.set_param(mat, "color_g", ParamValue::Float(base[1])).unwrap();
        g.set_param(mat, "color_b", ParamValue::Float(base[2])).unwrap();
        g.set_param(mat, "color_a", ParamValue::Float(1.0)).unwrap();
        g.connect((mat, "out"), (render, "material")).unwrap();
        let src = g.add_node(Box::new(Source::new()));
        g.connect((src, "out"), (render, "base_color_map")).unwrap();
        (mat, Some((src, map)))
    });

    // Average the covered pixels (uniform map → all equal) and compare.
    let mut sum = [0.0f64; 3];
    let mut n = 0u32;
    for p in &out {
        if p[0] + p[1] + p[2] > 0.05 {
            sum[0] += p[0] as f64;
            sum[1] += p[1] as f64;
            sum[2] += p[2] as f64;
            n += 1;
        }
    }
    assert!(n > 200, "quad should cover a real area, got {n}");
    let avg = [
        (sum[0] / n as f64) as f32,
        (sum[1] / n as f64) as f32,
        (sum[2] / n as f64) as f32,
    ];
    for i in 0..3 {
        assert!(
            (avg[i] - expected[i]).abs() < 0.02,
            "channel {i}: got {}, expected {} (base×map)",
            avg[i],
            expected[i]
        );
    }
}

/// Back-face lighting: a single triangle seen from behind, lit by a light
/// on the camera's side. Without the `front_facing` normal flip the back
/// face would shade with the +z front normal (N·L < 0 → clamped to 0 →
/// black, since ambient = 0); with the flip it shades correctly. A non-black
/// footprint proves the flip fired.
#[test]
fn back_face_is_lit_by_front_facing_flip() {
    let (w, h) = (128u32, 128u32);
    // The triangle's winding-front faces −z (verified empirically). Viewing
    // from the +z side (orbit = π/2 → pos ≈ [0, 0, +d]) therefore presents
    // its BACK face: front_facing == false, which is what the flip handles.
    let orbit = std::f32::consts::FRAC_PI_2;

    let out = render_mesh_scene(w, h, &back_facing_tri(), orbit, 3.0, |g, render| {
        let mat = g.add_node(Box::new(PhongMaterial::new()));
        g.set_param(mat, "color_r", ParamValue::Float(1.0)).unwrap();
        g.set_param(mat, "color_g", ParamValue::Float(1.0)).unwrap();
        g.set_param(mat, "color_b", ParamValue::Float(1.0)).unwrap();
        // Ambient 0 so a wrong (unflipped) back face is BLACK, not just dim.
        g.set_param(mat, "ambient", ParamValue::Float(0.0)).unwrap();
        g.connect((mat, "out"), (render, "material")).unwrap();

        // Sun light travelling +z (pos on −z, aim at origin) → L = -dir = -z,
        // which lights the flipped (camera-facing) back normal.
        let light = g.add_node(Box::new(LightNode::new()));
        g.set_param(light, "mode", ParamValue::Enum(0)).unwrap(); // Sun
        g.set_param(light, "pos_x", ParamValue::Float(0.0)).unwrap();
        g.set_param(light, "pos_y", ParamValue::Float(0.0)).unwrap();
        g.set_param(light, "pos_z", ParamValue::Float(-10.0)).unwrap();
        g.set_param(light, "aim_x", ParamValue::Float(0.0)).unwrap();
        g.set_param(light, "aim_y", ParamValue::Float(0.0)).unwrap();
        g.set_param(light, "aim_z", ParamValue::Float(0.0)).unwrap();
        g.set_param(light, "intensity", ParamValue::Float(1.0)).unwrap();
        g.connect((light, "out"), (render, "light")).unwrap();
        (mat, None)
    });

    let mut lit = 0usize;
    for p in &out {
        if p[0] + p[1] + p[2] > 0.1 {
            lit += 1;
        }
    }
    assert!(
        lit > 200,
        "back face should be LIT (front-facing flip), got {lit} non-black pixels — silhouette-black means the flip did not fire"
    );
}
