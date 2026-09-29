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

// ============================================================================
// MATERIAL M6 — value-level gpu_tests: albedo/metallic maps, alpha cutout,
// and the back-face lighting flip. Headless raw-executor render + f16
// readback: each renders a hand-built quad/triangle with a controlled
// `base_color_map` and reads the linear f16 colour back (no tone-map) so we
// can assert on values, not just non-blackness.
// ============================================================================

use crate::node_graph::effect_node::{
    EffectNode, EffectNodeContext, EffectNodeType, ParamValues,
};
use crate::node_graph::parameters::ParamDef;
use crate::node_graph::ports::{ArrayType, NodeInput, NodeOutput, NodePort, PortKind, PortType};
use crate::node_graph::Source;
use crate::node_graph::primitives::scene_object::SceneObjectNode;
use super::{CelMaterial, RenderScene, Transform3D, UnlitMaterial};

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
/// (SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md section 2 D3 — TRS lives on
/// `node.scene_object`'s `transform` input now, fed by `node.transform_3d`).
fn wire_pos_x(g: &mut Graph, scene_object: NodeInstanceId, pos_x: f32) {
    let t = g.add_node(Box::new(Transform3D::new()));
    g.set_param(t, "pos_x", ParamValue::Float(pos_x)).unwrap();
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
        fn depth_rule(&self) -> crate::node_graph::depth_rule::DepthRule { crate::node_graph::depth_rule::DepthRule::Terminal } // test fixture
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
        tangent: [0.0; 4],
            color: [1.0; 4],
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
        tangent: [0.0; 4],
            color: [1.0; 4],
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
    let env = g.add_node(Box::new(BakeEquirectEnvmap::new()));
    let sink = g.add_node(Box::new(FinalOutput::new()));

    g.connect((mesh, "out"), (render, "vertices")).unwrap();
    g.connect((cam, "out"), (render, "camera")).unwrap();
    g.connect((env, "envmap"), (render, "envmap")).unwrap();
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
        let mat = g.add_node(Box::new(PbrMaterial::new()));
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
/// `evaluate` is a GPU compute dispatch that fills it.
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

/// Value-level occlusion gate (REALTIME_3D section 5 P1): two overlapping cube
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

/// One cel-lit quad (facing the camera, normal aligned with the
/// light so `N·L == 1` at every covered fragment) rendered through
/// `node.render_scene` with `objects = 1` and either 1 or 2 IDENTICAL
/// lights wired to `light_0` (/ `light_1`). Ambient = 0 so the readback
/// is pure per-light diffuse accumulation.
fn render_scene_cel_quad_frame(w: u32, h: u32, num_lights: u32) -> Vec<[f32; 4]> {
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

    let mat = g.add_node(Box::new(CelMaterial::new()));
    g.set_param(mat, "color_r", ParamValue::Float(1.0)).unwrap();
    g.set_param(mat, "color_g", ParamValue::Float(1.0)).unwrap();
    g.set_param(mat, "color_b", ParamValue::Float(1.0)).unwrap();
    g.set_param(mat, "cel_bands", ParamValue::Float(4.0)).unwrap();
    g.set_param(mat, "band_low", ParamValue::Float(0.0)).unwrap();
    g.set_param(mat, "band_high", ParamValue::Float(1.0)).unwrap();

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

/// Value-level multi-light gate (REALTIME_3D section 5 P1): 2 IDENTICAL lights
/// must sum to (approximately) 2× the diffuse of 1 light at a
/// directly-lit pixel, within f16 round-trip tolerance.
#[test]
fn render_scene_multi_light_accumulates_diffuse_linearly() {
    let (w, h) = (64u32, 64u32);
    let one = render_scene_cel_quad_frame(w, h, 1);
    let two = render_scene_cel_quad_frame(w, h, 2);

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

/// One unlit quad through `node.render_scene` (`objects = 1`, `lights = 0`)
/// with an 8×8 checkerboard-alpha `base_color_map_0` wired via a `Source`
/// placeholder node, pre-bound the same way `render_mesh_scene`'s optional
/// `bcmap` wiring works for `node.render_mesh`. Mirrors
/// `render_scene_cel_quad_frame`'s camera/mesh setup, minus lights.
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
