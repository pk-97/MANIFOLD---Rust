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
    BakeEquirectEnvmap, CameraOrbit, GenerateCubeMesh, GltfMeshSource, GltfTextureSource, LightNode,
    PbrMaterial, Render3DMesh,
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

    // Sun placed in the camera's own octant — the physically intuitive
    // placement now that the M6-D4 flip is view-based (`if dot(N, V) < 0.0
    // { N = -N; }`): a convex cube's camera-visible faces already have
    // outward normals facing the camera, so a light near the camera (orbit
    // 0.7, tilt 0.35 → camera in (+X,+Y,+Z)) lights the visible faces
    // directly.
    let light = g.add_node(Box::new(LightNode::new()));
    g.set_param(light, "mode", ParamValue::Enum(0)).unwrap(); // Sun
    g.set_param(light, "pos_x", ParamValue::Float(3.0)).unwrap();
    g.set_param(light, "pos_y", ParamValue::Float(4.0)).unwrap();
    g.set_param(light, "pos_z", ParamValue::Float(3.0)).unwrap();
    g.set_param(light, "aim_x", ParamValue::Float(0.0)).unwrap();
    g.set_param(light, "aim_y", ParamValue::Float(0.0)).unwrap();
    g.set_param(light, "aim_z", ParamValue::Float(0.0)).unwrap();
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

    let mut backend = MetalBackend::new(device.arc(), w, h, format);

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
use crate::node_graph::primitives::scene_object::SceneObjectNode;
use super::{PhongMaterial, RenderScene, Transform3D, UnlitMaterial};

/// `Graph::connect` needs a `&'static str` port name; these helpers only
/// ever address the first handful of scene objects, so a small literal
/// table avoids leaking a formatted `String` just to satisfy the lifetime.
fn object_port_name(index: usize) -> &'static str {
    match index {
        0 => "object_0",
        1 => "object_1",
        2 => "object_2",
        3 => "object_3",
        4 => "object_4",
        _ => panic!("mesh_snapshot test helper: add an object_{{index}} literal for index {index}"),
    }
}

/// SCENE_OBJECT_AND_PANEL_V2_DESIGN.md D1/D4: `render_scene`'s per-object
/// surface is one `Object` wire now, not a family of parallel ports. Mint
/// one `node.scene_object` for object `index`, wire its `object` output
/// into `render`'s `object_{index}` port, and hand back the scene_object's
/// id — callers wire mesh/material/maps/transform into ITS inputs instead
/// of directly into `render`.
fn add_scene_object(g: &mut Graph, render: NodeInstanceId, index: usize) -> NodeInstanceId {
    let obj = g.add_node(Box::new(SceneObjectNode::new()));
    g.connect((obj, "object"), (render, object_port_name(index))).unwrap();
    obj
}

/// Add a `node.transform_3d` node, set its `pos_x` param, and wire its
/// `transform` output into `scene_object`'s `transform` input. Test helper
/// replacing the retired `render_scene` per-object `pos_x_{index}` param
/// (SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md §2 D3 — TRS lives on
/// `node.scene_object`'s `transform` input now, fed by `node.transform_3d`).
fn wire_pos_x(g: &mut Graph, scene_object: NodeInstanceId, pos_x: f32) {
    let t = g.add_node(Box::new(Transform3D::new()));
    g.set_param(t, "pos_x", ParamValue::Float(pos_x)).unwrap();
    g.connect((t, "transform"), (scene_object, "transform")).unwrap();
}

/// Same as [`wire_pos_x`] but for a full `(pos_x, pos_y, pos_z)` triple —
/// replaces three retired `pos_x_{index}`/`pos_y_{index}`/`pos_z_{index}`
/// param sets with one `node.transform_3d` node.
fn wire_pos(g: &mut Graph, scene_object: NodeInstanceId, pos: [f32; 3]) {
    let t = g.add_node(Box::new(Transform3D::new()));
    g.set_param(t, "pos_x", ParamValue::Float(pos[0])).unwrap();
    g.set_param(t, "pos_y", ParamValue::Float(pos[1])).unwrap();
    g.set_param(t, "pos_z", ParamValue::Float(pos[2])).unwrap();
    g.connect((t, "transform"), (scene_object, "transform")).unwrap();
}

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
                name: std::borrow::Cow::Borrowed("out"),
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

/// A single triangle in the z=0 plane, geometric normal +z. A camera on
/// the −z side therefore views this triangle from the side OPPOSITE its
/// stored normal — exactly the two-sided (dot(N, V) < 0) case the
/// view-facing lighting flip has to handle.
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

    let mut backend = MetalBackend::new(device.arc(), w, h, format);
    let color_target = RenderTarget::new(&device, w, h, format, "m6-color");
    let color_slot = backend.pre_bind_texture_2d(r_color, color_target);

    // Mesh vertex buffer, pre-filled with the caller's geometry.
    let vert_bytes = std::mem::size_of_val(verts) as u64;
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

/// Two-sided lighting: a single triangle whose STORED vertex normal (+z)
/// points away from the camera, lit by a light on the camera's side.
/// M6-D4's flip is view-based (`if dot(N, V) < 0.0 { N = -N; }`), not
/// winding-based — it faces the shading normal toward the viewer
/// regardless of which side the rasterizer thinks is "front". Without the
/// flip this triangle would shade with its stored +z normal (N·L < 0 →
/// clamped to 0 → black, since ambient = 0); with the flip it shades
/// correctly. A non-black footprint proves the flip fired.
#[test]
fn back_face_lit_by_two_sided_normal_faces_viewer() {
    let (w, h) = (128u32, 128u32);
    // Camera on the −z side (orbit = −π/2 → pos ≈ [0, 0, −d]) views the
    // triangle from the side OPPOSITE its stored normal (+z), so
    // dot(N, V) < 0 and the view-facing flip engages.
    let orbit = -std::f32::consts::FRAC_PI_2;

    let out = render_mesh_scene(w, h, &back_facing_tri(), orbit, 3.0, |g, render| {
        let mat = g.add_node(Box::new(PhongMaterial::new()));
        g.set_param(mat, "color_r", ParamValue::Float(1.0)).unwrap();
        g.set_param(mat, "color_g", ParamValue::Float(1.0)).unwrap();
        g.set_param(mat, "color_b", ParamValue::Float(1.0)).unwrap();
        // Ambient 0 so a wrong (unflipped) back face is BLACK, not just dim.
        g.set_param(mat, "ambient", ParamValue::Float(0.0)).unwrap();
        g.connect((mat, "out"), (render, "material")).unwrap();

        // Sun on the camera's own side (pos on −z, aim at origin) → L =
        // (0, 0, -1), matching the flipped (camera-facing) normal exactly.
        // This is the physically intuitive placement: a light near the
        // camera lights what the camera sees.
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
        "back face should be LIT (two-sided view-facing flip), got {lit} non-black pixels — silhouette-black means the flip did not fire"
    );
}

// ============================================================================
// REALTIME_3D P1 — `node.render_scene`: shared-depth occlusion between
// objects, and multi-light accumulation. Same headless raw-executor
// render + f16 readback as the M6 tests above.
// ============================================================================

/// Pre-bind an allocated (but CPU-unfilled) `Array<MeshVertex>` buffer for
/// a `GenerateCubeMesh` node's `vertices` output — the node's own
/// `evaluate` is a GPU compute dispatch that fills it. Mirrors
/// `render_pbr_cube`'s cube pre-bind.
fn pre_bind_cube_output(
    device: &manifold_gpu::GpuDevice,
    backend: &mut MetalBackend,
    resource: ResourceId,
) {
    use crate::node_graph::primitive::PrimitiveSpec;
    let capacity = GenerateCubeMesh::PARAMS
        .iter()
        .find(|p| p.name == "max_capacity")
        .and_then(|p| match p.default {
            ParamValue::Float(n) => Some(n.round() as u64),
            _ => None,
        })
        .expect("cube max_capacity default");
    let buf = device.create_buffer_shared(capacity * std::mem::size_of::<MeshVertex>() as u64);
    backend.pre_bind_array(resource, buf);
}

/// Two unlit cubes, front-on from a camera parked on +X looking at the
/// origin (`orbit = tilt = 0` on `node.orbit_camera` → `pos = (distance,
/// 0, 0)`, `fwd = -X`) — so `pos_x` is exactly the depth axis. Object 0
/// stays at the origin; object 1 sits `behind` it at `x = -offset`
/// (farther from the camera), same y/z, so both cubes are centred on the
/// optical axis and overlap on screen. `lights = 0` (Unlit needs none).
fn render_scene_occlusion_frame(w: u32, h: u32, offset: f32) -> Vec<[f32; 4]> {
    let device = crate::test_device();
    let format = GpuTextureFormat::Rgba16Float;

    let mut g = Graph::new();
    let cube0 = g.add_node(Box::new(GenerateCubeMesh::new()));
    let cube1 = g.add_node(Box::new(GenerateCubeMesh::new()));

    let cam = g.add_node(Box::new(CameraOrbit::new()));
    g.set_param(cam, "orbit", ParamValue::Float(0.0)).unwrap();
    g.set_param(cam, "tilt", ParamValue::Float(0.0)).unwrap();
    g.set_param(cam, "distance", ParamValue::Float(8.0)).unwrap();
    g.set_param(cam, "fov_y", ParamValue::Float(0.9)).unwrap();

    let mat0 = g.add_node(Box::new(UnlitMaterial::new()));
    g.set_param(mat0, "color_r", ParamValue::Float(1.0)).unwrap();
    g.set_param(mat0, "color_g", ParamValue::Float(0.0)).unwrap();
    g.set_param(mat0, "color_b", ParamValue::Float(0.0)).unwrap();
    g.set_param(mat0, "color_a", ParamValue::Float(1.0)).unwrap();

    let mat1 = g.add_node(Box::new(UnlitMaterial::new()));
    g.set_param(mat1, "color_r", ParamValue::Float(0.0)).unwrap();
    g.set_param(mat1, "color_g", ParamValue::Float(1.0)).unwrap();
    g.set_param(mat1, "color_b", ParamValue::Float(0.0)).unwrap();
    g.set_param(mat1, "color_a", ParamValue::Float(1.0)).unwrap();

    let render = g.add_node(Box::new(RenderScene::new()));
    g.set_param(render, "objects", ParamValue::Float(2.0)).unwrap();
    g.set_param(render, "lights", ParamValue::Float(0.0)).unwrap();
    let obj0 = add_scene_object(&mut g, render, 0);
    let obj1 = add_scene_object(&mut g, render, 1);
    wire_pos_x(&mut g, obj1, -offset);

    let sink = g.add_node(Box::new(FinalOutput::new()));

    g.connect((cube0, "vertices"), (obj0, "vertices")).unwrap();
    g.connect((cube1, "vertices"), (obj1, "vertices")).unwrap();
    g.connect((mat0, "out"), (obj0, "material")).unwrap();
    g.connect((mat1, "out"), (obj1, "material")).unwrap();
    g.connect((cam, "out"), (render, "camera")).unwrap();
    g.connect((render, "color"), (sink, "in")).unwrap();

    let plan = compile(&g).unwrap();
    let r_color = output_resource(&plan, render, "color");
    let r_cube0 = output_resource(&plan, cube0, "vertices");
    let r_cube1 = output_resource(&plan, cube1, "vertices");

    let mut backend = MetalBackend::new(device.arc(), w, h, format);
    let color_target = RenderTarget::new(&device, w, h, format, "render-scene-occlusion-color");
    let color_slot = backend.pre_bind_texture_2d(r_color, color_target);
    pre_bind_cube_output(&device, &mut backend, r_cube0);
    pre_bind_cube_output(&device, &mut backend, r_cube1);

    let mut native_enc = device.create_encoder("render-scene-occlusion");
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

/// Value-level occlusion gate (REALTIME_3D §5 P1): two overlapping cube
/// meshes through `node.render_scene`, sharing one depth buffer. Object 0
/// (red, nearer) and object 1 (green, farther) are both centred on the
/// camera's optical axis, so the exact centre pixel is covered by both
/// silhouettes — it must show object 0's colour (nearer wins) regardless
/// of draw order, which is exactly what a correctly load-action-managed
/// shared depth buffer guarantees.
#[test]
fn render_scene_shared_depth_resolves_occlusion_between_objects() {
    let (w, h) = (128u32, 128u32);
    let out = render_scene_occlusion_frame(w, h, 3.0);
    let center = out[(h / 2 * w + w / 2) as usize];
    println!("render_scene occlusion: centre pixel rgba = {center:?}");
    assert!(
        center[0] > 0.5 && center[1] < 0.2,
        "expected nearer (red) object at centre pixel, got {center:?} — occlusion/shared-depth broken"
    );
}

/// One Phong-lit quad (facing the camera, normal aligned with the
/// light so `N·L == 1` at every covered fragment) rendered through
/// `node.render_scene` with `objects = 1` and either 1 or 2 IDENTICAL
/// lights wired to `light_0` (/ `light_1`). Ambient = 0 so the readback
/// is pure per-light diffuse accumulation.
fn render_scene_phong_quad_frame(w: u32, h: u32, num_lights: u32) -> Vec<[f32; 4]> {
    let device = crate::test_device();
    let format = GpuTextureFormat::Rgba16Float;

    let mut g = Graph::new();
    let mesh = g.add_node(Box::new(MeshSource::new()));
    let cam = g.add_node(Box::new(CameraOrbit::new()));
    g.set_param(cam, "orbit", ParamValue::Float(std::f32::consts::FRAC_PI_2))
        .unwrap();
    g.set_param(cam, "tilt", ParamValue::Float(0.0)).unwrap();
    g.set_param(cam, "distance", ParamValue::Float(4.0)).unwrap();
    g.set_param(cam, "fov_y", ParamValue::Float(1.0)).unwrap();

    let mat = g.add_node(Box::new(PhongMaterial::new()));
    g.set_param(mat, "color_r", ParamValue::Float(1.0)).unwrap();
    g.set_param(mat, "color_g", ParamValue::Float(1.0)).unwrap();
    g.set_param(mat, "color_b", ParamValue::Float(1.0)).unwrap();
    g.set_param(mat, "ambient", ParamValue::Float(0.0)).unwrap();
    // Zero specular tint so the readback isolates the diffuse term the
    // gate cares about (specular would also double correctly with
    // identical lights, but a flat diffuse-only comparison is cleaner).
    g.set_param(mat, "specular_color_r", ParamValue::Float(0.0)).unwrap();
    g.set_param(mat, "specular_color_g", ParamValue::Float(0.0)).unwrap();
    g.set_param(mat, "specular_color_b", ParamValue::Float(0.0)).unwrap();

    let render = g.add_node(Box::new(RenderScene::new()));
    g.set_param(render, "objects", ParamValue::Float(1.0)).unwrap();
    g.set_param(render, "lights", ParamValue::Float(num_lights as f32))
        .unwrap();
    let obj0 = add_scene_object(&mut g, render, 0);

    let sink = g.add_node(Box::new(FinalOutput::new()));

    g.connect((mesh, "out"), (obj0, "vertices")).unwrap();
    g.connect((mat, "out"), (obj0, "material")).unwrap();
    g.connect((cam, "out"), (render, "camera")).unwrap();

    // Sun light on the camera's own side — the physically intuitive
    // placement. Camera orbit = FRAC_PI_2 puts it at (0, 0, +distance); the
    // quad's stored normal is +z, already facing the camera (dot(N, V) > 0),
    // so the view-facing flip does not fire here — no two-sided geometry
    // involved, just a plain front-lit quad. A light at pos_z > 0 aimed at
    // the origin gives dir = (0, 0, -1), so L = -dir = (0, 0, 1) — matching
    // the (unflipped) normal exactly, N·L == 1 everywhere it's lit.
    let light0 = g.add_node(Box::new(LightNode::new()));
    g.set_param(light0, "mode", ParamValue::Enum(0)).unwrap();
    g.set_param(light0, "pos_x", ParamValue::Float(0.0)).unwrap();
    g.set_param(light0, "pos_y", ParamValue::Float(0.0)).unwrap();
    g.set_param(light0, "pos_z", ParamValue::Float(10.0)).unwrap();
    g.set_param(light0, "aim_x", ParamValue::Float(0.0)).unwrap();
    g.set_param(light0, "aim_y", ParamValue::Float(0.0)).unwrap();
    g.set_param(light0, "aim_z", ParamValue::Float(0.0)).unwrap();
    g.set_param(light0, "intensity", ParamValue::Float(1.0)).unwrap();
    g.connect((light0, "out"), (render, "light_0")).unwrap();

    if num_lights == 2 {
        let light1 = g.add_node(Box::new(LightNode::new()));
        g.set_param(light1, "mode", ParamValue::Enum(0)).unwrap();
        g.set_param(light1, "pos_x", ParamValue::Float(0.0)).unwrap();
        g.set_param(light1, "pos_y", ParamValue::Float(0.0)).unwrap();
        g.set_param(light1, "pos_z", ParamValue::Float(10.0)).unwrap();
        g.set_param(light1, "aim_x", ParamValue::Float(0.0)).unwrap();
        g.set_param(light1, "aim_y", ParamValue::Float(0.0)).unwrap();
        g.set_param(light1, "aim_z", ParamValue::Float(0.0)).unwrap();
        g.set_param(light1, "intensity", ParamValue::Float(1.0)).unwrap();
        g.connect((light1, "out"), (render, "light_1")).unwrap();
    }

    g.connect((render, "color"), (sink, "in")).unwrap();

    let plan = compile(&g).unwrap();
    let r_color = output_resource(&plan, render, "color");
    let r_mesh = output_resource(&plan, mesh, "out");

    let mut backend = MetalBackend::new(device.arc(), w, h, format);
    let color_target = RenderTarget::new(&device, w, h, format, "render-scene-multilight-color");
    let color_slot = backend.pre_bind_texture_2d(r_color, color_target);

    let verts = quad_verts();
    let vert_bytes = (verts.len() * std::mem::size_of::<MeshVertex>()) as u64;
    let vert_buf = device.create_buffer_shared(vert_bytes);
    unsafe {
        vert_buf.write(0, bytemuck::cast_slice(&verts));
    }
    backend.pre_bind_array(r_mesh, vert_buf);

    let mut native_enc = device.create_encoder("render-scene-multilight");
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

/// Value-level multi-light gate (REALTIME_3D §5 P1): 2 IDENTICAL lights
/// must sum to (approximately) 2× the diffuse of 1 light at a
/// directly-lit pixel, within f16 round-trip tolerance.
#[test]
fn render_scene_multi_light_accumulates_diffuse_linearly() {
    let (w, h) = (64u32, 64u32);
    let one = render_scene_phong_quad_frame(w, h, 1);
    let two = render_scene_phong_quad_frame(w, h, 2);

    let center_idx = (h / 2 * w + w / 2) as usize;
    let p1 = one[center_idx];
    let p2 = two[center_idx];
    println!("render_scene multi-light: 1-light = {p1:?}, 2-light = {p2:?}");

    assert!(p1[0] > 0.1, "1-light centre pixel should be lit, got {p1:?}");
    for c in 0..3 {
        assert!(
            (p2[c] - 2.0 * p1[c]).abs() < 0.05,
            "channel {c}: 2-light {} should be ≈2× 1-light {} (got {})",
            p2[c],
            p1[c],
            p2[c] / p1[c].max(1e-6)
        );
    }
}

/// Two cubes, offset in X and one slightly nearer the camera, phong-lit
/// by one sun light, through `node.render_scene`. Reinhard-tonemapped
/// Rgba8 PNG bytes — visual gate for the orchestrator to eyeball.
fn render_scene_two_cubes_png(w: u32, h: u32) -> Vec<u8> {
    let device = crate::test_device();
    let format = GpuTextureFormat::Rgba16Float;

    let mut g = Graph::new();
    let cube0 = g.add_node(Box::new(GenerateCubeMesh::new()));
    let cube1 = g.add_node(Box::new(GenerateCubeMesh::new()));

    // Same orbit/tilt/fov as `render_pbr_cube` above (a camera angle
    // already proven to light a cube well), just pulled back a bit
    // (distance 4 → 9) to fit two objects in frame.
    let cam = g.add_node(Box::new(CameraOrbit::new()));
    g.set_param(cam, "orbit", ParamValue::Float(0.7)).unwrap();
    g.set_param(cam, "tilt", ParamValue::Float(0.35)).unwrap();
    g.set_param(cam, "distance", ParamValue::Float(9.0)).unwrap();
    g.set_param(cam, "fov_y", ParamValue::Float(0.9)).unwrap();

    let mat0 = g.add_node(Box::new(PhongMaterial::new()));
    g.set_param(mat0, "color_r", ParamValue::Float(0.85)).unwrap();
    g.set_param(mat0, "color_g", ParamValue::Float(0.3)).unwrap();
    g.set_param(mat0, "color_b", ParamValue::Float(0.3)).unwrap();
    g.set_param(mat0, "ambient", ParamValue::Float(0.2)).unwrap();

    let mat1 = g.add_node(Box::new(PhongMaterial::new()));
    g.set_param(mat1, "color_r", ParamValue::Float(0.3)).unwrap();
    g.set_param(mat1, "color_g", ParamValue::Float(0.55)).unwrap();
    g.set_param(mat1, "color_b", ParamValue::Float(0.85)).unwrap();
    g.set_param(mat1, "ambient", ParamValue::Float(0.2)).unwrap();

    let render = g.add_node(Box::new(RenderScene::new()));
    g.set_param(render, "objects", ParamValue::Float(2.0)).unwrap();
    g.set_param(render, "lights", ParamValue::Float(1.0)).unwrap();
    let obj0 = add_scene_object(&mut g, render, 0);
    let obj1 = add_scene_object(&mut g, render, 1);
    // Object 0 offset to one side. Object 1 offset toward the camera
    // along roughly this orbit/tilt's eye-to-origin direction (0.72,
    // 0.34, 0.60 — the reverse of the camera's forward vector) with only
    // a modest same-side X offset, so its (larger, nearer) silhouette
    // partially overlaps object 0's — the occlusion render_scene exists
    // for, made visible.
    wire_pos_x(&mut g, obj0, -1.5);
    wire_pos(&mut g, obj1, [0.8, 0.3, 1.2]);

    // Angled key light in the camera's OWN octant. The M6-D4 flip is
    // view-based (`if dot(N, V) < 0.0 { N = -N; }`), so a convex cube's
    // camera-visible faces already have outward normals that face the
    // camera — the flip does not fire, and the naively "obvious" light
    // placement (same octant as the camera) is exactly right: N·L > 0 on
    // the visible faces instead of the all-ambient result the opposite-
    // octant placement used to require before the fix.
    let light = g.add_node(Box::new(LightNode::new()));
    g.set_param(light, "pos_x", ParamValue::Float(6.0)).unwrap();
    g.set_param(light, "pos_y", ParamValue::Float(5.0)).unwrap();
    g.set_param(light, "pos_z", ParamValue::Float(4.0)).unwrap();
    g.set_param(light, "intensity", ParamValue::Float(1.6)).unwrap();

    let sink = g.add_node(Box::new(FinalOutput::new()));

    g.connect((cube0, "vertices"), (obj0, "vertices")).unwrap();
    g.connect((cube1, "vertices"), (obj1, "vertices")).unwrap();
    g.connect((mat0, "out"), (obj0, "material")).unwrap();
    g.connect((mat1, "out"), (obj1, "material")).unwrap();
    g.connect((cam, "out"), (render, "camera")).unwrap();
    g.connect((light, "out"), (render, "light_0")).unwrap();
    g.connect((render, "color"), (sink, "in")).unwrap();

    let plan = compile(&g).unwrap();
    let r_color = output_resource(&plan, render, "color");
    let r_cube0 = output_resource(&plan, cube0, "vertices");
    let r_cube1 = output_resource(&plan, cube1, "vertices");

    let mut backend = MetalBackend::new(device.arc(), w, h, format);
    let color_target = RenderTarget::new(&device, w, h, format, "render-scene-two-cubes-color");
    let color_slot = backend.pre_bind_texture_2d(r_color, color_target);
    pre_bind_cube_output(&device, &mut backend, r_cube0);
    pre_bind_cube_output(&device, &mut backend, r_cube1);

    let mut native_enc = device.create_encoder("render-scene-two-cubes");
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
    let mut readback_enc = device.create_encoder("render-scene-two-cubes-readback");
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
        let a = half_to_f32(px[3]).clamp(0.0, 1.0);
        rgba.push((a * 255.0).round() as u8);
    }
    rgba
}

/// Visual gate (REALTIME_3D §5 P1): render the two-cube scene to a PNG
/// for the orchestrator to eyeball. Ignored by default: needs a GPU and
/// writes a file. Point `MESH_SNAP_OUT` at an absolute path to control
/// the output location; defaults to `target/mesh-snap/scene_two_cubes.png`.
#[test]
#[ignore]
fn scene_two_cubes_renders_to_png() {
    let w = 512u32;
    let h = 512u32;
    let rgba = render_scene_two_cubes_png(w, h);

    let mut non_black = 0usize;
    for px in rgba.chunks_exact(4) {
        if px[0] != 0 || px[1] != 0 || px[2] != 0 {
            non_black += 1;
        }
    }
    let total = (w * h) as usize;
    let fraction = non_black as f64 / total as f64;
    println!("render_scene two-cubes: non-black pixel fraction = {fraction:.4} ({non_black}/{total})");
    assert!(
        fraction > 0.02,
        "expected >2% non-black pixels, got {fraction:.4} ({non_black}/{total}) — likely a broken render_scene dispatch"
    );

    let out_path = std::env::var("MESH_SNAP_OUT")
        .unwrap_or_else(|_| "target/mesh-snap/scene_two_cubes.png".to_string());
    if let Some(parent) = std::path::Path::new(&out_path).parent() {
        std::fs::create_dir_all(parent).expect("create output dir");
    }
    image::save_buffer(&out_path, &rgba, w, h, image::ExtendedColorType::Rgba8)
        .unwrap_or_else(|e| panic!("save {out_path}: {e}"));
    println!("render_scene two-cubes: wrote {out_path}");
}

/// One unlit quad through `node.render_scene` (`objects = 1`, `lights = 0`)
/// with an 8×8 checkerboard-alpha `base_color_map_0` wired via a `Source`
/// placeholder node, pre-bound the same way `render_mesh_scene`'s optional
/// `bcmap` wiring works for `node.render_mesh`. Mirrors
/// `render_scene_phong_quad_frame`'s camera/mesh setup, minus lights.
fn render_scene_bcmap_quad_frame(w: u32, h: u32, mask: bool, checker: &[[f32; 4]], cw: u32, ch: u32) -> Vec<[f32; 4]> {
    let device = crate::test_device();
    let format = GpuTextureFormat::Rgba16Float;
    let map = upload_f16_rgba(&device, cw, ch, checker);

    let mut g = Graph::new();
    let mesh = g.add_node(Box::new(MeshSource::new()));
    let cam = g.add_node(Box::new(CameraOrbit::new()));
    g.set_param(cam, "orbit", ParamValue::Float(std::f32::consts::FRAC_PI_2))
        .unwrap();
    g.set_param(cam, "tilt", ParamValue::Float(0.0)).unwrap();
    g.set_param(cam, "distance", ParamValue::Float(4.0)).unwrap();
    g.set_param(cam, "fov_y", ParamValue::Float(1.0)).unwrap();

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

    let render = g.add_node(Box::new(RenderScene::new()));
    g.set_param(render, "objects", ParamValue::Float(1.0)).unwrap();
    g.set_param(render, "lights", ParamValue::Float(0.0)).unwrap();
    let obj0 = add_scene_object(&mut g, render, 0);

    let src = g.add_node(Box::new(Source::new()));
    let sink = g.add_node(Box::new(FinalOutput::new()));

    g.connect((mesh, "out"), (obj0, "vertices")).unwrap();
    g.connect((mat, "out"), (obj0, "material")).unwrap();
    g.connect((cam, "out"), (render, "camera")).unwrap();
    g.connect((src, "out"), (obj0, "base_color_map")).unwrap();
    g.connect((render, "color"), (sink, "in")).unwrap();

    let plan = compile(&g).unwrap();
    let r_color = output_resource(&plan, render, "color");
    let r_mesh = output_resource(&plan, mesh, "out");
    let r_map = output_resource(&plan, src, "out");

    let mut backend = MetalBackend::new(device.arc(), w, h, format);
    let color_target = RenderTarget::new(&device, w, h, format, "render-scene-bcmap-color");
    let color_slot = backend.pre_bind_texture_2d(r_color, color_target);

    let verts = quad_verts();
    let vert_bytes = (verts.len() * std::mem::size_of::<MeshVertex>()) as u64;
    let vert_buf = device.create_buffer_shared(vert_bytes);
    unsafe {
        vert_buf.write(0, bytemuck::cast_slice(&verts));
    }
    backend.pre_bind_array(r_mesh, vert_buf);
    backend.pre_bind_texture_2d(r_map, map);

    let mut native_enc = device.create_encoder("render-scene-bcmap");
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

/// Value-level cutout gate for `node.render_scene`'s per-object
/// `base_color_map_n` port (M6 addendum): same checkerboard-alpha cutout
/// proof as `alpha_mask_cutout_discards_transparent_texels`, routed through
/// `RenderScene` instead of `Render3DMesh` — proves the sampled
/// `base_color_map_0` alpha (not a blanket discard) gates the Mask-mode
/// cutout through render_scene's per-object `texture_flags.z` wiring.
#[test]
fn render_scene_base_color_map_alpha_cutout_discards() {
    let (w, h) = (128u32, 128u32);
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

    let opaque = render_scene_bcmap_quad_frame(w, h, false, &checker, cw, ch);
    let mask = render_scene_bcmap_quad_frame(w, h, true, &checker, cw, ch);

    let lum = |p: [f32; 4]| p[0] + p[1] + p[2];
    let mut footprint = 0usize;
    let mut discarded = 0usize;
    let mut shaded = 0usize;
    for (o, m) in opaque.iter().zip(mask.iter()) {
        if lum(*o) > 0.1 {
            footprint += 1;
            if lum(*m) < 0.05 {
                discarded += 1;
            } else if lum(*m) > 0.5 {
                shaded += 1;
            }
        }
    }

    println!(
        "render_scene base_color_map cutout: discarded={discarded} shaded={shaded} footprint={footprint}"
    );
    assert!(footprint > 200, "quad should cover a real area, got {footprint}");
    assert!(
        discarded * 5 > footprint && shaded * 5 > footprint,
        "expected a roughly balanced cutout gated on sampled base_color_map alpha: discarded={discarded} shaded={shaded} footprint={footprint}"
    );
}

// ============================================================================
// IMPORT wave — real glTF geometry proof: load a `.glb` fixture, flatten its
// node tree + mesh primitives into ONE combined `Array<MeshVertex>` buffer
// (world-space, recentred at the origin), and render it lit through
// `node.render_scene`. Proves the render path handles real imported geometry
// end to end, not just hand-built cubes/quads. NOT a production import node —
// the `gltf` crate is a dev-dependency for this test only.
// ============================================================================

/// A 4×4 column-major matrix: `m[col][row]`, matching both the `gltf` crate's
/// `Transform::matrix()` convention and `render_scene.rs`'s `model_matrix`.
type Mat4 = [[f32; 4]; 4];

const MAT4_IDENTITY: Mat4 = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

fn mat4_mul(a: &Mat4, b: &Mat4) -> Mat4 {
    let mut out = [[0.0f32; 4]; 4];
    for col in 0..4 {
        for row in 0..4 {
            let mut sum = 0.0;
            for k in 0..4 {
                sum += a[k][row] * b[col][k];
            }
            out[col][row] = sum;
        }
    }
    out
}

fn mat4_transform_point(m: &Mat4, p: [f32; 3]) -> [f32; 3] {
    [
        m[0][0] * p[0] + m[1][0] * p[1] + m[2][0] * p[2] + m[3][0],
        m[0][1] * p[0] + m[1][1] * p[1] + m[2][1] * p[2] + m[3][1],
        m[0][2] * p[0] + m[1][2] * p[1] + m[2][2] * p[2] + m[3][2],
    ]
}

/// Upper-left 3×3 (rotation + scale) block of a column-major `Mat4`,
/// returned row-major (`m3[row][col]`) for the inverse below.
fn mat3_upper_row_major(m: &Mat4) -> [[f32; 3]; 3] {
    [
        [m[0][0], m[1][0], m[2][0]],
        [m[0][1], m[1][1], m[2][1]],
        [m[0][2], m[1][2], m[2][2]],
    ]
}

fn mat3_inverse(a: [[f32; 3]; 3]) -> [[f32; 3]; 3] {
    let det = a[0][0] * (a[1][1] * a[2][2] - a[1][2] * a[2][1])
        - a[0][1] * (a[1][0] * a[2][2] - a[1][2] * a[2][0])
        + a[0][2] * (a[1][0] * a[2][1] - a[1][1] * a[2][0]);
    if det.abs() < 1e-12 {
        // Degenerate (zero-scale) transform — identity fallback so
        // normals don't come out NaN.
        return [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    }
    let inv_det = 1.0 / det;
    [
        [
            (a[1][1] * a[2][2] - a[1][2] * a[2][1]) * inv_det,
            (a[0][2] * a[2][1] - a[0][1] * a[2][2]) * inv_det,
            (a[0][1] * a[1][2] - a[0][2] * a[1][1]) * inv_det,
        ],
        [
            (a[1][2] * a[2][0] - a[1][0] * a[2][2]) * inv_det,
            (a[0][0] * a[2][2] - a[0][2] * a[2][0]) * inv_det,
            (a[0][2] * a[1][0] - a[0][0] * a[1][2]) * inv_det,
        ],
        [
            (a[1][0] * a[2][1] - a[1][1] * a[2][0]) * inv_det,
            (a[0][1] * a[2][0] - a[0][0] * a[2][1]) * inv_det,
            (a[0][0] * a[1][1] - a[0][1] * a[1][0]) * inv_det,
        ],
    ]
}

fn mat3_transpose(a: [[f32; 3]; 3]) -> [[f32; 3]; 3] {
    [
        [a[0][0], a[1][0], a[2][0]],
        [a[0][1], a[1][1], a[2][1]],
        [a[0][2], a[1][2], a[2][2]],
    ]
}

fn mat3_mul_vec3(m: [[f32; 3]; 3], v: [f32; 3]) -> [f32; 3] {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

fn normalize3(v: [f32; 3]) -> [f32; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if len < 1e-12 {
        [0.0, 0.0, 1.0]
    } else {
        [v[0] / len, v[1] / len, v[2] / len]
    }
}

fn sub3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

/// Recursively flatten a glTF node's mesh primitives (world-transformed) into
/// `out`, then recurse into children with the composed world matrix. Every
/// primitive must be `Mode::Triangles` — anything else is a structural
/// surprise worth stopping on, not silently reinterpreting.
fn walk_gltf_node(
    node: &gltf::Node,
    parent_world: Mat4,
    buffers: &[gltf::buffer::Data],
    out: &mut Vec<MeshVertex>,
) {
    let local = node.transform().matrix();
    let world = mat4_mul(&parent_world, &local);

    if let Some(mesh) = node.mesh() {
        // Normal matrix = transpose(inverse(upper3x3(world))) — correct
        // under non-uniform scale, not just rotation + uniform scale.
        let normal_mat = mat3_transpose(mat3_inverse(mat3_upper_row_major(&world)));

        for primitive in mesh.primitives() {
            assert_eq!(
                primitive.mode(),
                gltf::mesh::Mode::Triangles,
                "azalea glb primitive uses non-Triangles mode {:?} — unsupported by this proof harness",
                primitive.mode()
            );

            let reader = primitive.reader(|b| buffers.get(b.index()).map(|d| d.0.as_slice()));

            let positions: Vec<[f32; 3]> = reader
                .read_positions()
                .unwrap_or_else(|| panic!("primitive missing required POSITION accessor"))
                .collect();
            let normals: Option<Vec<[f32; 3]>> = reader.read_normals().map(|it| it.collect());
            let uvs: Option<Vec<[f32; 2]>> =
                reader.read_tex_coords(0).map(|it| it.into_f32().collect());

            let world_positions: Vec<[f32; 3]> = positions
                .iter()
                .map(|p| mat4_transform_point(&world, *p))
                .collect();
            let world_normals: Option<Vec<[f32; 3]>> = normals
                .as_ref()
                .map(|ns| ns.iter().map(|n| normalize3(mat3_mul_vec3(normal_mat, *n))).collect());

            let indices: Vec<u32> = match reader.read_indices() {
                Some(idx) => idx.into_u32().collect(),
                None => (0..world_positions.len() as u32).collect(),
            };

            for tri in indices.chunks_exact(3) {
                let (i0, i1, i2) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);
                let p0 = world_positions[i0];
                let p1 = world_positions[i1];
                let p2 = world_positions[i2];
                // Face-normal fallback when NORMAL is absent on this
                // primitive — computed post-transform, in world space.
                let face_normal = normalize3(cross3(sub3(p1, p0), sub3(p2, p0)));

                for &i in &[i0, i1, i2] {
                    let normal = world_normals.as_ref().map_or(face_normal, |ns| ns[i]);
                    let uv = uvs.as_ref().map_or([0.0, 0.0], |u| u[i]);
                    out.push(MeshVertex {
                        position: world_positions[i],
                        _pad0: 0.0,
                        normal,
                        _pad1: 0.0,
                        uv,
                        _pad2: [0.0, 0.0],
                    });
                }
            }
        }
    }

    for child in node.children() {
        walk_gltf_node(&child, world, buffers, out);
    }
}

/// Load the azalea `.glb` fixture, flatten it to one combined world-space
/// `Array<MeshVertex>` buffer, and render it lit through `node.render_scene`
/// with a `PhongMaterial` + a default Sun light + a bbox-framed orbit
/// camera. Ignored by default: needs a GPU, a large fixture, and writes a
/// file. Point `MESH_SNAP_OUT` at an absolute path to control the output
/// location; defaults to `target/mesh-snap/azalea_render_scene.png`.
///
/// This is a PROOF, not a production import node: one combined mesh (no
/// multi-material split), no base-colour/alpha-cutout textures wired — the
/// point is that real imported geometry renders lit through render_scene at
/// all, not a faithful material reproduction.
#[test]
#[ignore]
fn azalea_glb_renders_lit_through_render_scene() {
    // Workspace-root-relative fixture, resolved off `CARGO_MANIFEST_DIR`
    // (`cargo test`'s cwd is the package dir, not the workspace root) —
    // same convention as `preset_loader.rs` / `bundled_presets.rs`.
    // Input fixture is overridable via `MESH_SNAP_GLB` (mirrors the
    // `MESH_SNAP_OUT` output override below) so the same proven render-scene
    // harness can be pointed at held-out `.glb` fixtures for the VD-003
    // import-correctness gate, not just the azalea dev fixture.
    let path = match std::env::var("MESH_SNAP_GLB") {
        Ok(p) => std::path::PathBuf::from(p),
        Err(_) => std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/gltf/cc0__oomurasaki_azalea_r._x_pulchrum.glb"),
    };
    if !path.exists() {
        eprintln!(
            "azalea_glb_renders_lit_through_render_scene: fixture not found at {}, skipping",
            path.display()
        );
        return;
    }

    let (document, buffers, _images) = crate::node_graph::gltf_load::import_glb(&path)
        .unwrap_or_else(|e| panic!("import_glb({}): {e}", path.display()));

    let mut verts: Vec<MeshVertex> = Vec::new();
    for node in crate::node_graph::gltf_load::resolve_import_nodes(&document) {
        walk_gltf_node(&node, MAT4_IDENTITY, &buffers, &mut verts);
    }
    assert!(
        !verts.is_empty(),
        "parsed zero vertices from {} — mesh/primitive traversal found nothing",
        path.display()
    );

    // Axis-aligned bbox in world space, then recentre so the model sits at
    // the origin (CameraOrbit's target is fixed at `(0, look_y, 0)`).
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for v in &verts {
        for i in 0..3 {
            min[i] = min[i].min(v.position[i]);
            max[i] = max[i].max(v.position[i]);
        }
    }
    let center = [
        (min[0] + max[0]) * 0.5,
        (min[1] + max[1]) * 0.5,
        (min[2] + max[2]) * 0.5,
    ];
    let dims = [max[0] - min[0], max[1] - min[1], max[2] - min[2]];
    let radius = (dims[0] * dims[0] + dims[1] * dims[1] + dims[2] * dims[2]).sqrt() * 0.5;
    println!(
        "azalea: parsed {} vertices ({} triangles), bbox dims = {dims:?}, radius = {radius:.4}",
        verts.len(),
        verts.len() / 3
    );

    for v in &mut verts {
        v.position[0] -= center[0];
        v.position[1] -= center[1];
        v.position[2] -= center[2];
    }

    let distance = 2.2 * radius;

    let w = 768u32;
    let h = 768u32;
    let device = crate::test_device();
    let format = GpuTextureFormat::Rgba16Float;

    let mut g = Graph::new();
    let mesh = g.add_node(Box::new(MeshSource::new()));

    let cam = g.add_node(Box::new(CameraOrbit::new()));
    g.set_param(cam, "orbit", ParamValue::Float(0.7)).unwrap();
    g.set_param(cam, "tilt", ParamValue::Float(0.3)).unwrap();
    g.set_param(cam, "distance", ParamValue::Float(distance)).unwrap();
    g.set_param(cam, "fov_y", ParamValue::Float(0.9)).unwrap();
    g.set_param(cam, "look_y", ParamValue::Float(0.0)).unwrap();

    let mat = g.add_node(Box::new(PhongMaterial::new()));
    g.set_param(mat, "color_r", ParamValue::Float(0.45)).unwrap();
    g.set_param(mat, "color_g", ParamValue::Float(0.55)).unwrap();
    g.set_param(mat, "color_b", ParamValue::Float(0.35)).unwrap();
    g.set_param(mat, "ambient", ParamValue::Float(0.35)).unwrap();

    // Key light front-lighting the CAMERA-FACING side. The M6-D4 flip is
    // view-based (`if dot(N, V) < 0.0 { N = -N; }`): a convex mesh's
    // camera-visible faces already have outward normals that face the
    // camera, so the flip does not fire on them and the naively "obvious"
    // choice — a light in the camera's OWN octant — is exactly right. The
    // camera (orbit 0.7, tilt 0.3) is in (+X,+Y,+Z), so the Sun goes in
    // (+X,+Y,+Z) too. The offset is raked (more +X/+Z than +Y) for a clear
    // left-to-right light/shadow gradient across the foliage rather than
    // flat frontal fill. Intensity 1.5.
    let light = g.add_node(Box::new(LightNode::new()));
    g.set_param(light, "mode", ParamValue::Enum(0)).unwrap(); // Sun
    g.set_param(light, "pos_x", ParamValue::Float(5.0)).unwrap();
    g.set_param(light, "pos_y", ParamValue::Float(2.0)).unwrap();
    g.set_param(light, "pos_z", ParamValue::Float(3.0)).unwrap();
    g.set_param(light, "aim_x", ParamValue::Float(0.0)).unwrap();
    g.set_param(light, "aim_y", ParamValue::Float(0.0)).unwrap();
    g.set_param(light, "aim_z", ParamValue::Float(0.0)).unwrap();
    g.set_param(light, "intensity", ParamValue::Float(1.5)).unwrap();

    let render = g.add_node(Box::new(RenderScene::new()));
    g.set_param(render, "objects", ParamValue::Float(1.0)).unwrap();
    g.set_param(render, "lights", ParamValue::Float(1.0)).unwrap();
    let obj0 = add_scene_object(&mut g, render, 0);

    let sink = g.add_node(Box::new(FinalOutput::new()));

    g.connect((mesh, "out"), (obj0, "vertices")).unwrap();
    g.connect((mat, "out"), (obj0, "material")).unwrap();
    g.connect((cam, "out"), (render, "camera")).unwrap();
    g.connect((light, "out"), (render, "light_0")).unwrap();
    g.connect((render, "color"), (sink, "in")).unwrap();

    let plan = compile(&g).unwrap();
    let r_color = output_resource(&plan, render, "color");
    let r_mesh = output_resource(&plan, mesh, "out");

    let mut backend = MetalBackend::new(device.arc(), w, h, format);
    let color_target = RenderTarget::new(&device, w, h, format, "azalea-render-scene-color");
    let color_slot = backend.pre_bind_texture_2d(r_color, color_target);

    let vert_bytes = (verts.len() * std::mem::size_of::<MeshVertex>()) as u64;
    let vert_buf = device.create_buffer_shared(vert_bytes);
    unsafe {
        vert_buf.write(0, bytemuck::cast_slice(&verts));
    }
    backend.pre_bind_array(r_mesh, vert_buf);

    let mut native_enc = device.create_encoder("azalea-render-scene");
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
    let mut readback_enc = device.create_encoder("azalea-render-scene-readback");
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
        let a = half_to_f32(px[3]).clamp(0.0, 1.0);
        rgba.push((a * 255.0).round() as u8);
    }

    let mut non_black = 0usize;
    for px in rgba.chunks_exact(4) {
        if px[0] != 0 || px[1] != 0 || px[2] != 0 {
            non_black += 1;
        }
    }
    let total = (w * h) as usize;
    let fraction = non_black as f64 / total as f64;
    println!("azalea render_scene: non-black pixel fraction = {fraction:.4} ({non_black}/{total})");
    assert!(
        fraction > 0.02,
        "expected >2% non-black pixels, got {fraction:.4} ({non_black}/{total}) — likely a broken render_scene dispatch or empty geometry"
    );

    let out_path = std::env::var("MESH_SNAP_OUT")
        .unwrap_or_else(|_| "target/mesh-snap/azalea_render_scene.png".to_string());
    if let Some(parent) = std::path::Path::new(&out_path).parent() {
        std::fs::create_dir_all(parent).expect("create output dir");
    }
    image::save_buffer(&out_path, &rgba, w, h, image::ExtendedColorType::Rgba8)
        .unwrap_or_else(|e| panic!("save {out_path}: {e}"));
    println!("azalea render_scene: wrote {out_path}");
}

// ============================================================================
// node.gltf_mesh_source — end-to-end proof of the PRODUCTION primitive
// (as opposed to `azalea_glb_renders_lit_through_render_scene`'s inline
// test-only parse above). Proves the primitive's background-thread load +
// GPU buffer copy actually feeds `node.render_scene` a lit, recognisable
// mesh, not just that the CPU flatten function returns non-empty data.
// ============================================================================

/// Render the azalea fixture through the real `node.gltf_mesh_source`
/// primitive (whole-scene mode) into `node.render_scene`, lit with the
/// same `PhongMaterial` + Sun-light + orbit-camera setup as
/// `azalea_glb_renders_lit_through_render_scene`. Ignored by default:
/// needs a GPU, a large fixture, and writes a file. Point `MESH_SNAP_OUT`
/// at an absolute path to control the output location.
///
/// `GltfMeshSource` doesn't recenter its world-combined output at the
/// origin (that's the caller's job via `node.render_scene`'s per-object
/// transform) — this test does a SEPARATE read-only parse via
/// `gltf_load::load_gltf_mesh` purely to compute a framing bbox center +
/// radius, then feeds `-center` into `render_scene`'s `pos_*_0` params so
/// `CameraOrbit`'s origin-fixed target still frames the model. That
/// framing parse is not the thing under test — the primitive's own
/// background-thread parse (wired through the graph) is.
///
/// The primitive's file load runs on a background thread, so the graph
/// is executed in a polling loop (bounded, with a short sleep between
/// attempts) until the vertex buffer actually has non-black content —
/// mirroring how the real content thread converges over several frames
/// after a path change, rather than assuming the ~50 MB parse completes
/// within one synthetic frame.
#[test]
#[ignore]
fn gltf_mesh_source_renders_azalea_to_png() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/gltf/cc0__oomurasaki_azalea_r._x_pulchrum.glb");
    if !path.exists() {
        eprintln!(
            "gltf_mesh_source_renders_azalea_to_png: fixture not found at {}, skipping",
            path.display()
        );
        return;
    }

    // Framing-only parse (NOT the primitive under test) — bbox center +
    // radius so the camera distance / recenter-translate can be set up
    // before the graph runs. Reuses the same shared `gltf_load` module
    // the primitive itself calls on its background thread.
    let framing_verts = crate::node_graph::gltf_load::load_gltf_mesh(
        &path,
        crate::node_graph::gltf_load::GltfMeshSelector::WholeScene,
    )
    .unwrap_or_else(|e| panic!("framing parse of {}: {e}", path.display()));
    assert!(!framing_verts.is_empty(), "framing parse produced zero vertices");

    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for v in &framing_verts {
        for i in 0..3 {
            min[i] = min[i].min(v.position[i]);
            max[i] = max[i].max(v.position[i]);
        }
    }
    let center = [
        (min[0] + max[0]) * 0.5,
        (min[1] + max[1]) * 0.5,
        (min[2] + max[2]) * 0.5,
    ];
    let dims = [max[0] - min[0], max[1] - min[1], max[2] - min[2]];
    let radius = (dims[0] * dims[0] + dims[1] * dims[1] + dims[2] * dims[2]).sqrt() * 0.5;
    let distance = 2.2 * radius;
    println!(
        "gltf_mesh_source: framing parse {} vertices, bbox dims = {dims:?}, radius = {radius:.4}",
        framing_verts.len()
    );

    let w = 768u32;
    let h = 768u32;
    let device = crate::test_device();
    let format = GpuTextureFormat::Rgba16Float;

    let mut g = Graph::new();
    let src = g.add_node(Box::new(GltfMeshSource::new()));
    g.set_param(
        src,
        "path",
        ParamValue::String(std::sync::Arc::new(path.to_string_lossy().into_owned())),
    )
    .unwrap();
    g.set_param(src, "mesh_index", ParamValue::Float(-1.0)).unwrap();
    g.set_param(src, "max_capacity", ParamValue::Float(8_000_000.0))
        .unwrap();

    let cam = g.add_node(Box::new(CameraOrbit::new()));
    g.set_param(cam, "orbit", ParamValue::Float(0.7)).unwrap();
    g.set_param(cam, "tilt", ParamValue::Float(0.3)).unwrap();
    g.set_param(cam, "distance", ParamValue::Float(distance)).unwrap();
    g.set_param(cam, "fov_y", ParamValue::Float(0.9)).unwrap();
    g.set_param(cam, "look_y", ParamValue::Float(0.0)).unwrap();

    let mat = g.add_node(Box::new(PhongMaterial::new()));
    g.set_param(mat, "color_r", ParamValue::Float(0.45)).unwrap();
    g.set_param(mat, "color_g", ParamValue::Float(0.55)).unwrap();
    g.set_param(mat, "color_b", ParamValue::Float(0.35)).unwrap();
    g.set_param(mat, "ambient", ParamValue::Float(0.35)).unwrap();

    let light = g.add_node(Box::new(LightNode::new()));
    g.set_param(light, "mode", ParamValue::Enum(0)).unwrap(); // Sun
    g.set_param(light, "pos_x", ParamValue::Float(5.0)).unwrap();
    g.set_param(light, "pos_y", ParamValue::Float(2.0)).unwrap();
    g.set_param(light, "pos_z", ParamValue::Float(3.0)).unwrap();
    g.set_param(light, "aim_x", ParamValue::Float(0.0)).unwrap();
    g.set_param(light, "aim_y", ParamValue::Float(0.0)).unwrap();
    g.set_param(light, "aim_z", ParamValue::Float(0.0)).unwrap();
    g.set_param(light, "intensity", ParamValue::Float(1.5)).unwrap();

    let render = g.add_node(Box::new(RenderScene::new()));
    g.set_param(render, "objects", ParamValue::Float(1.0)).unwrap();
    g.set_param(render, "lights", ParamValue::Float(1.0)).unwrap();
    let obj0 = add_scene_object(&mut g, render, 0);
    // Recenter the (not-recentered) primitive output at the origin so
    // CameraOrbit's fixed target frames it.
    wire_pos(&mut g, obj0, [-center[0], -center[1], -center[2]]);

    let sink = g.add_node(Box::new(FinalOutput::new()));

    g.connect((src, "vertices"), (obj0, "vertices")).unwrap();
    g.connect((mat, "out"), (obj0, "material")).unwrap();
    g.connect((cam, "out"), (render, "camera")).unwrap();
    g.connect((light, "out"), (render, "light_0")).unwrap();
    g.connect((render, "color"), (sink, "in")).unwrap();

    let plan = compile(&g).unwrap();
    let r_color = output_resource(&plan, render, "color");
    let r_vertices = output_resource(&plan, src, "vertices");

    let mut backend = MetalBackend::new(device.arc(), w, h, format);
    let color_target = RenderTarget::new(&device, w, h, format, "gltf-mesh-source-color");
    let color_slot = backend.pre_bind_texture_2d(r_color, color_target);

    // The raw Executor path does NOT auto-allocate Array buffers — normally
    // the chain-build pre-allocator reads the primitive's declared
    // `array_output_capacity` (driven by its `max_capacity` param) and
    // allocates for us. Mirror that sizing directly since we're bypassing
    // the chain builder here. NOT pre-filled — `GltfMeshSource::run` fills
    // it via its own background parse + `copy_buffer_to_buffer`.
    let vertex_capacity = 8_000_000u64;
    let vert_buf =
        device.create_buffer_shared(vertex_capacity * std::mem::size_of::<MeshVertex>() as u64);
    backend.pre_bind_array(r_vertices, vert_buf);

    let mut exec = Executor::new(Box::new(backend));

    // Poll until the primitive's background-thread parse lands (or we
    // give up). Each iteration is one full graph execution — the
    // primitive's `run()` drains its `mpsc` receiver via `try_recv()`
    // every call, same as the real per-frame content-thread loop.
    let max_attempts = 200;
    let mut rgba = Vec::new();
    let mut fraction = 0.0f64;
    for attempt in 0..max_attempts {
        let mut native_enc = device.create_encoder("gltf-mesh-source-render-scene");
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
        let mut readback_enc = device.create_encoder("gltf-mesh-source-readback");
        readback_enc.copy_texture_to_buffer(out_tex, &readback_buf, w, h, bytes_per_row);
        readback_enc.commit_and_wait_completed();

        let ptr = readback_buf.mapped_ptr().expect("shared readback");
        let halves: &[u16] =
            unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };

        rgba = Vec::with_capacity((w * h * 4) as usize);
        let mut non_black = 0usize;
        for px in halves.chunks_exact(4) {
            let r = tonemap_channel(half_to_f32(px[0]));
            let g_ = tonemap_channel(half_to_f32(px[1]));
            let b = tonemap_channel(half_to_f32(px[2]));
            if r != 0 || g_ != 0 || b != 0 {
                non_black += 1;
            }
            rgba.push(r);
            rgba.push(g_);
            rgba.push(b);
            let a = half_to_f32(px[3]).clamp(0.0, 1.0);
            rgba.push((a * 255.0).round() as u8);
        }
        let total = (w * h) as usize;
        fraction = non_black as f64 / total as f64;
        if fraction > 0.02 {
            println!("gltf_mesh_source: converged on attempt {attempt} (non-black fraction {fraction:.4})");
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    let total = (w * h) as usize;
    println!(
        "gltf_mesh_source render_scene: non-black pixel fraction = {fraction:.4} ({} attempts budget, total {total})",
        max_attempts
    );
    assert!(
        fraction > 0.02,
        "expected >2% non-black pixels after polling for the background parse, got {fraction:.4} \
         — likely a broken node.gltf_mesh_source dispatch, a parse that never completed, or empty geometry"
    );

    let out_path = std::env::var("MESH_SNAP_OUT")
        .unwrap_or_else(|_| "target/mesh-snap/gltf_mesh_source_azalea.png".to_string());
    if let Some(parent) = std::path::Path::new(&out_path).parent() {
        std::fs::create_dir_all(parent).expect("create output dir");
    }
    image::save_buffer(&out_path, &rgba, w, h, image::ExtendedColorType::Rgba8)
        .unwrap_or_else(|e| panic!("save {out_path}: {e}"));
    println!("gltf_mesh_source render_scene: wrote {out_path}");
}

/// Full-stack textured proof: `node.gltf_mesh_source` (whole-scene azalea
/// geometry) + `node.gltf_texture_source` (the glb's embedded texture 0)
/// wired into `node.render_scene`'s `base_color_map_0`, with a WHITE-base
/// material so the surface shows the raw sampled texture rather than a flat
/// tint. This is the combined verification that the two import primitives
/// and the render_scene texture input compose end to end — the mesh reader,
/// the texture reader, and the albedo sampling all on the real GPU path.
///
/// Note this applies ONE texture to the WHOLE combined mesh (every
/// primitive's UVs sample texture 0). That is not a faithful azalea render
/// — faithful per-primitive material/texture mapping is the importer's job
/// (P1c). The point here is that an embedded glb texture flows through
/// `gltf_texture_source` → `base_color_map_0` and modulates the surface,
/// not a correct multi-material reproduction. Alpha is left Opaque: the
/// cutout-discard path through render_scene is already proven value-level
/// by `render_scene_base_color_map_alpha_cutout_discards`.
///
/// BOTH sources parse on background threads, so the graph runs in the same
/// bounded polling loop as `gltf_mesh_source_renders_azalea_to_png` — and
/// because `base_color_map_0` is wired, a white surface stays black until
/// the texture actually decodes (white × black-texture = black), so the
/// non-black convergence check waits for the texture too, not just the mesh.
#[test]
#[ignore]
fn gltf_textured_azalea_renders_through_render_scene_to_png() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/gltf/cc0__oomurasaki_azalea_r._x_pulchrum.glb");
    if !path.exists() {
        eprintln!(
            "gltf_textured_azalea_renders_through_render_scene_to_png: fixture not found at {}, skipping",
            path.display()
        );
        return;
    }

    // Framing-only parse (NOT under test) — bbox center + radius so the
    // camera frames the not-recentered primitive output.
    let framing_verts = crate::node_graph::gltf_load::load_gltf_mesh(
        &path,
        crate::node_graph::gltf_load::GltfMeshSelector::WholeScene,
    )
    .unwrap_or_else(|e| panic!("framing parse of {}: {e}", path.display()));
    assert!(!framing_verts.is_empty(), "framing parse produced zero vertices");

    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for v in &framing_verts {
        for i in 0..3 {
            min[i] = min[i].min(v.position[i]);
            max[i] = max[i].max(v.position[i]);
        }
    }
    let center = [
        (min[0] + max[0]) * 0.5,
        (min[1] + max[1]) * 0.5,
        (min[2] + max[2]) * 0.5,
    ];
    let dims = [max[0] - min[0], max[1] - min[1], max[2] - min[2]];
    let radius = (dims[0] * dims[0] + dims[1] * dims[1] + dims[2] * dims[2]).sqrt() * 0.5;
    let distance = 2.2 * radius;

    let w = 768u32;
    let h = 768u32;
    let device = crate::test_device();
    let format = GpuTextureFormat::Rgba16Float;

    let mut g = Graph::new();
    let src = g.add_node(Box::new(GltfMeshSource::new()));
    g.set_param(
        src,
        "path",
        ParamValue::String(std::sync::Arc::new(path.to_string_lossy().into_owned())),
    )
    .unwrap();
    g.set_param(src, "mesh_index", ParamValue::Float(-1.0)).unwrap();
    g.set_param(src, "max_capacity", ParamValue::Float(8_000_000.0))
        .unwrap();

    // The embedded texture 0 (the azalea's baked albedo, 1024×1024) as the
    // base_color_map. sRGB (the default) so the hardware linearizes on read.
    let tex = g.add_node(Box::new(GltfTextureSource::new()));
    g.set_param(
        tex,
        "path",
        ParamValue::String(std::sync::Arc::new(path.to_string_lossy().into_owned())),
    )
    .unwrap();
    g.set_param(tex, "texture_index", ParamValue::Float(0.0)).unwrap();
    g.set_param(tex, "color_space", ParamValue::Enum(0)).unwrap();
    g.set_param(tex, "width", ParamValue::Float(1024.0)).unwrap();
    g.set_param(tex, "height", ParamValue::Float(1024.0)).unwrap();

    let cam = g.add_node(Box::new(CameraOrbit::new()));
    g.set_param(cam, "orbit", ParamValue::Float(0.7)).unwrap();
    g.set_param(cam, "tilt", ParamValue::Float(0.3)).unwrap();
    g.set_param(cam, "distance", ParamValue::Float(distance)).unwrap();
    g.set_param(cam, "fov_y", ParamValue::Float(0.9)).unwrap();
    g.set_param(cam, "look_y", ParamValue::Float(0.0)).unwrap();

    // WHITE base color so albedo == the sampled texture (base_color × map),
    // making the raw texture unmistakable rather than a green-tinted blend.
    let mat = g.add_node(Box::new(PhongMaterial::new()));
    g.set_param(mat, "color_r", ParamValue::Float(1.0)).unwrap();
    g.set_param(mat, "color_g", ParamValue::Float(1.0)).unwrap();
    g.set_param(mat, "color_b", ParamValue::Float(1.0)).unwrap();
    g.set_param(mat, "ambient", ParamValue::Float(0.35)).unwrap();

    let light = g.add_node(Box::new(LightNode::new()));
    g.set_param(light, "mode", ParamValue::Enum(0)).unwrap(); // Sun
    g.set_param(light, "pos_x", ParamValue::Float(5.0)).unwrap();
    g.set_param(light, "pos_y", ParamValue::Float(2.0)).unwrap();
    g.set_param(light, "pos_z", ParamValue::Float(3.0)).unwrap();
    g.set_param(light, "aim_x", ParamValue::Float(0.0)).unwrap();
    g.set_param(light, "aim_y", ParamValue::Float(0.0)).unwrap();
    g.set_param(light, "aim_z", ParamValue::Float(0.0)).unwrap();
    g.set_param(light, "intensity", ParamValue::Float(1.5)).unwrap();

    let render = g.add_node(Box::new(RenderScene::new()));
    g.set_param(render, "objects", ParamValue::Float(1.0)).unwrap();
    g.set_param(render, "lights", ParamValue::Float(1.0)).unwrap();
    let obj0 = add_scene_object(&mut g, render, 0);
    wire_pos(&mut g, obj0, [-center[0], -center[1], -center[2]]);

    let sink = g.add_node(Box::new(FinalOutput::new()));

    g.connect((src, "vertices"), (obj0, "vertices")).unwrap();
    g.connect((tex, "out"), (obj0, "base_color_map")).unwrap();
    g.connect((mat, "out"), (obj0, "material")).unwrap();
    g.connect((cam, "out"), (render, "camera")).unwrap();
    g.connect((light, "out"), (render, "light_0")).unwrap();
    g.connect((render, "color"), (sink, "in")).unwrap();

    let plan = compile(&g).unwrap();
    let r_color = output_resource(&plan, render, "color");
    let r_vertices = output_resource(&plan, src, "vertices");

    let mut backend = MetalBackend::new(device.arc(), w, h, format);
    let color_target = RenderTarget::new(&device, w, h, format, "gltf-textured-color");
    let color_slot = backend.pre_bind_texture_2d(r_color, color_target);

    // Mesh vertex buffer (chain-bypass) — gltf_texture_source's `out` is a
    // Texture2D output the backend auto-allocates at its output_dims
    // (1024²), so only the Array output needs manual pre-binding.
    let vertex_capacity = 8_000_000u64;
    let vert_buf =
        device.create_buffer_shared(vertex_capacity * std::mem::size_of::<MeshVertex>() as u64);
    backend.pre_bind_array(r_vertices, vert_buf);

    let mut exec = Executor::new(Box::new(backend));

    let max_attempts = 200;
    let mut rgba = Vec::new();
    let mut fraction = 0.0f64;
    for attempt in 0..max_attempts {
        let mut native_enc = device.create_encoder("gltf-textured-render-scene");
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
        let mut readback_enc = device.create_encoder("gltf-textured-readback");
        readback_enc.copy_texture_to_buffer(out_tex, &readback_buf, w, h, bytes_per_row);
        readback_enc.commit_and_wait_completed();

        let ptr = readback_buf.mapped_ptr().expect("shared readback");
        let halves: &[u16] =
            unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };

        rgba = Vec::with_capacity((w * h * 4) as usize);
        let mut non_black = 0usize;
        for px in halves.chunks_exact(4) {
            let r = tonemap_channel(half_to_f32(px[0]));
            let g_ = tonemap_channel(half_to_f32(px[1]));
            let b = tonemap_channel(half_to_f32(px[2]));
            if r != 0 || g_ != 0 || b != 0 {
                non_black += 1;
            }
            rgba.push(r);
            rgba.push(g_);
            rgba.push(b);
            let a = half_to_f32(px[3]).clamp(0.0, 1.0);
            rgba.push((a * 255.0).round() as u8);
        }
        let total = (w * h) as usize;
        fraction = non_black as f64 / total as f64;
        if fraction > 0.02 {
            println!("gltf_textured: converged on attempt {attempt} (non-black fraction {fraction:.4})");
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    let total = (w * h) as usize;
    println!("gltf_textured render_scene: non-black pixel fraction = {fraction:.4} (total {total})");
    assert!(
        fraction > 0.02,
        "expected >2% non-black pixels after polling for both background parses, got {fraction:.4} \
         — a broken gltf_texture_source dispatch, base_color_map sampling, or a parse that never landed"
    );

    let out_path = std::env::var("MESH_SNAP_OUT")
        .unwrap_or_else(|_| "target/mesh-snap/gltf_textured_azalea.png".to_string());
    if let Some(parent) = std::path::Path::new(&out_path).parent() {
        std::fs::create_dir_all(parent).expect("create output dir");
    }
    image::save_buffer(&out_path, &rgba, w, h, image::ExtendedColorType::Rgba8)
        .unwrap_or_else(|e| panic!("save {out_path}: {e}"));
    println!("gltf_textured render_scene: wrote {out_path}");
}
