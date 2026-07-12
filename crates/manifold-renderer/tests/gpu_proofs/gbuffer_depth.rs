//! `docs/GBUFFER_DESIGN.md` I2 — `node.render_scene`'s stored `depth`
//! output equals the CPU oracle (`Camera::project_to_pixel().depth`).
//!
//! Five known meshes at five depths: a flat quad, rotated 90° about Z
//! (`node.transform_3d`'s `rot_z`) so its normal points along `+X` instead
//! of `grid_mesh`'s native `+Y`, rendered through `render_scene` with a
//! camera on `+X` at `orbit=0, tilt=0` looking straight at the origin —
//! i.e. the quad directly FACES the camera, dead-on, rather than lying
//! flat under a tilted view. `node.orbit_camera` always orbits *around*
//! the origin, so `dot(origin - cam.pos, cam.fwd) == distance` exactly —
//! the camera's `distance` param IS the oracle's `view_z` for the origin
//! point, giving five clean, distinct depths with zero extra geometry work.
//!
//! The dead-on view matters because this test samples a SINGLE pixel (not
//! a weighted centroid over many, unlike `camera_conformance.rs`): at ANY
//! tilt short of exactly perpendicular, depth still has a real gradient
//! across the visible footprint, and `Sample0`'s specific in-pixel MSAA
//! sample position (offset from the pixel's geometric centre, which is
//! what the CPU oracle assumes) picks up part of that gradient as error —
//! empirically ~6e-5 at a shallow-tilt `orbit_camera` config, well over
//! the 1e-5 gate, and orbit_camera's up-vector derivation degenerates
//! (crashed the GPU driver empirically) as tilt approaches the `PI/2`
//! gimbal pole, so "tilt the camera instead" doesn't reach a safe, exact
//! configuration. Rotating the MESH to face an axis-aligned camera reaches
//! a TRUE dead-on view with zero gimbal risk (the same well-conditioned
//! `orbit=0, tilt=0` config used throughout this test suite) — depth is
//! then locally constant to first order in every screen direction, so
//! `Sample0`'s in-pixel offset costs nothing measurable.
//!
//! Reading the `depth` output: going through `PresetRuntime` (needed for
//! its `pre_allocate_resources` step that `Array<MeshVertex>` producers
//! like `grid_mesh`/`make_triangles` require) means the generator's own
//! single-output tracking follows exactly ONE `system.final_output` node,
//! found via `graph.nodes().find(...)` over an `AHashMap` — a SECOND
//! `system.final_output` (tried first) makes that lookup pick either node
//! nondeterministically per run, occasionally handing `depth`'s resource
//! the host's Rgba16Float canvas texture instead of its own R32Float pool
//! allocation (a real format-mismatch that reproduced as intermittent GPU
//! command-buffer faults during this phase's development — see the
//! session's escalation notes). The fix used here: `execution_plan.rs`'s
//! `consumed_outputs` mechanism marks a producer's output "wired" from the
//! wire's mere EXISTENCE, independent of whether the wire's destination
//! node is itself reachable from any liveness root (`compile()`'s own
//! comment: validate() skips required-input checks on the same
//! reachability grounds). So `depth` wires into `node.invert`'s `in` —
//! never wired further, hence dead/unreachable/never executed — which
//! gives `depth` a genuine step-output binding (same rule I1's compile-
//! only unit test in `render_scene.rs` proves) without a second
//! `system.final_output` anywhere in the graph. `PresetRuntime::dump_textures_all`
//! (the generator-path "dump every output" retrieval, gated on
//! `set_dump_all`) then reads back `depth`'s specific texture by port name.

use half::f16;
use manifold_gpu::{GpuTexture, GpuTextureFormat};
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::camera::{Camera, linearize_depth};
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;

use crate::harness;

/// Five distinct depths, near through far, all comfortably inside
/// `NEAR`/`FAR` below.
const DISTANCES: [f32; 5] = [1.0, 3.0, 8.0, 20.0, 50.0];
const ORBIT: f32 = 0.0;
const TILT: f32 = 0.0;
const FOV_Y: f32 = 0.9;
const NEAR: f32 = 0.05;
const FAR: f32 = 200.0;
/// `PI/2` radians — rotates the quad's normal from grid_mesh's native `+Y`
/// to `+X`, directly facing the `orbit=0, tilt=0` camera (which sits on
/// `+X` looking toward `-X`). See module doc.
const ROT_Z: f32 = std::f32::consts::FRAC_PI_2;
/// World point every camera looks at — `node.orbit_camera` always orbits
/// around the origin, so this is also the point whose `view_z` equals the
/// camera's `distance` param exactly.
const WORLD_POINT: [f32; 3] = [0.0, 0.0, 0.0];

/// Quad world half-size at `distance` — angular footprint stays constant
/// (`quad_size / distance` fixed) so the rasterized shape never shrinks to
/// sub-pixel at the far end of `DISTANCES`.
fn quad_size(distance: f32) -> f32 {
    0.1 * distance
}

/// `grid_mesh(2x2) -> make_triangles -> render_scene(1 object, 0 lights,
/// unlit white, transform_0 = rot_z(PI/2))`, camera on `+X` at `distance`
/// looking straight at the origin. `color` feeds the sole
/// `system.final_output`; `depth` feeds `node.invert` (never wired
/// further — a dead-end that still counts as "wired" at the
/// `execution_plan.rs` level, per module doc).
fn scene_json(distance: f32) -> String {
    let size = quad_size(distance);
    format!(
        r#"{{"version":2,"name":"GbufferDepthConformance","nodes":[
        {{"id":0,"typeId":"system.generator_input","nodeId":"input"}},
        {{"id":1,"typeId":"node.grid_mesh","nodeId":"grid","params":{{
            "max_capacity":{{"type":"Int","value":16}},
            "resolution_x":{{"type":"Int","value":2}},
            "resolution_y":{{"type":"Int","value":2}},
            "size_x":{{"type":"Float","value":{size}}},
            "size_y":{{"type":"Float","value":{size}}}}}}},
        {{"id":2,"typeId":"node.make_triangles","nodeId":"tris","params":{{
            "src_cols":{{"type":"Int","value":2}},
            "src_rows":{{"type":"Int","value":2}}}}}},
        {{"id":3,"typeId":"node.orbit_camera","nodeId":"cam","params":{{
            "orbit":{{"type":"Float","value":{ORBIT}}},
            "tilt":{{"type":"Float","value":{TILT}}},
            "distance":{{"type":"Float","value":{distance}}},
            "fov_y":{{"type":"Float","value":{FOV_Y}}},
            "look_y":{{"type":"Float","value":0.0}},
            "roll":{{"type":"Float","value":0.0}},
            "near":{{"type":"Float","value":{NEAR}}},
            "far":{{"type":"Float","value":{FAR}}}}}}},
        {{"id":4,"typeId":"node.unlit_material","nodeId":"mat","params":{{
            "color_r":{{"type":"Float","value":1.0}},
            "color_g":{{"type":"Float","value":1.0}},
            "color_b":{{"type":"Float","value":1.0}},
            "color_a":{{"type":"Float","value":1.0}}}}}},
        {{"id":5,"typeId":"node.transform_3d","nodeId":"xf","params":{{
            "rot_z":{{"type":"Float","value":{ROT_Z}}}}}}},
        {{"id":20,"typeId":"node.render_scene","nodeId":"scene","params":{{
            "objects":{{"type":"Int","value":1}},
            "lights":{{"type":"Int","value":0}}}}}},
        {{"id":21,"typeId":"node.invert","nodeId":"depth_sink","params":{{}}}},
        {{"id":99,"typeId":"system.final_output","nodeId":"color_out"}}
        ],"wires":[
        {{"fromNode":1,"fromPort":"vertices","toNode":2,"toPort":"in"}},
        {{"fromNode":2,"fromPort":"out","toNode":20,"toPort":"mesh_0"}},
        {{"fromNode":3,"fromPort":"out","toNode":20,"toPort":"camera"}},
        {{"fromNode":4,"fromPort":"out","toNode":20,"toPort":"material_0"}},
        {{"fromNode":5,"fromPort":"transform","toNode":20,"toPort":"transform_0"}},
        {{"fromNode":20,"fromPort":"color","toNode":99,"toPort":"in"}},
        {{"fromNode":20,"fromPort":"depth","toNode":21,"toPort":"in"}}
        ]}}"#
    )
}

/// Build, render two warm-up frames + one measured frame with dump mode on
/// (mirrors `camera_conformance.rs`'s `render_readback`), and return the
/// `depth` port's raw `R32Float` texture bytes plus its dims.
fn render_and_dump_depth(json: &str) -> (Vec<u8>, u32, u32) {
    let h = harness::shared();
    let registry = PrimitiveRegistry::with_builtin();
    let mut runtime = PresetRuntime::from_json_str_with_device(
        json,
        &registry,
        &h.device,
        h.width,
        h.height,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .unwrap_or_else(|e| panic!("gbuffer_depth graph must build: {e}\n{json}"));
    runtime.set_dump_all(true);

    let target = h.make_target("gbuffer-depth-conformance");
    for frame in 0..2 {
        let ctx = PresetContext {
            time: 0.0,
            beat: 0.0,
            dt: 1.0 / 60.0,
            width: h.width,
            height: h.height,
            output_width: h.width,
            output_height: h.height,
            aspect: h.width as f32 / h.height as f32,
            owner_key: 0,
            is_clip_level: false,
            frame_count: frame,
            anim_progress: 0.0,
            trigger_count: 0,
        };
        let mut enc = h.device.create_encoder("gbuffer-depth-enc");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, &h.device);
            runtime.render(&mut gpu, &target.texture, &ctx, &manifold_core::params::ParamManifest::default());
        }
        enc.commit_and_wait_completed();
    }

    let dumped = runtime.dump_textures_all();
    let (_, _, _, depth_tex): &(String, String, String, &GpuTexture) = dumped
        .iter()
        .find(|(node_id, port, _, _)| node_id == "scene" && port == "depth")
        .unwrap_or_else(|| {
            panic!(
                "no dumped `depth` output on node `scene` — dumped ports: {:?}",
                dumped.iter().map(|(n, p, _, _)| format!("{n}.{p}")).collect::<Vec<_>>()
            )
        });
    assert_eq!(
        depth_tex.format,
        GpuTextureFormat::R32Float,
        "depth's allocated texture must be R32Float (output_format override)"
    );
    let bytes = readback_r32float(&h.device, depth_tex);
    (bytes, depth_tex.width, depth_tex.height)
}

/// Read an `R32Float` texture back to host memory as raw little-endian
/// bytes. Mirrors `harness::ParityHarness::readback` but for 4 bytes/pixel
/// instead of the harness's hardcoded `Rgba16Float` (8 bytes/pixel)
/// assumption.
fn readback_r32float(device: &manifold_gpu::GpuDevice, texture: &GpuTexture) -> Vec<u8> {
    const BYTES_PER_PIXEL: u32 = 4;
    let bytes_per_row = texture.width * BYTES_PER_PIXEL;
    let total_bytes = u64::from(texture.height * bytes_per_row);
    let buf = device.create_buffer_shared(total_bytes);

    let mut enc = device.create_encoder("gbuffer-depth-readback");
    enc.copy_texture_to_buffer(texture, &buf, texture.width, texture.height, bytes_per_row);
    enc.commit_and_wait_completed();

    let ptr = buf
        .mapped_ptr()
        .expect("shared readback buffer must expose mapped pointer");
    let bytes: &[u8] = unsafe {
        std::slice::from_raw_parts(ptr.cast::<std::ffi::c_void>().cast::<u8>(), total_bytes as usize)
    };
    bytes.to_vec()
}

fn sample_r32float(bytes: &[u8], width: u32, x: u32, y: u32) -> f32 {
    let idx = ((y * width + x) * 4) as usize;
    f32::from_le_bytes([bytes[idx], bytes[idx + 1], bytes[idx + 2], bytes[idx + 3]])
}

#[test]
fn gbuffer_depth_conformance() {
    for &distance in &DISTANCES {
        let cam = Camera::orbit_perspective(ORBIT, TILT, distance, FOV_Y, 0.0, 0.0, NEAR, FAR);
        let h = harness::shared();
        let oracle = cam
            .project_to_pixel(WORLD_POINT, h.width, h.height)
            .unwrap_or_else(|| panic!("distance {distance}: origin unexpectedly behind camera"));
        assert!(
            (oracle.view_z - distance).abs() < 1e-3,
            "distance {distance}: oracle.view_z ({}) should equal the camera's own \
             distance param (orbit_camera always orbits the origin)",
            oracle.view_z
        );

        let (bytes, width, height) = render_and_dump_depth(&scene_json(distance));
        assert_eq!(bytes.len() as u32, width * height * 4, "R32Float byte count");
        assert_eq!((width, height), (h.width, h.height), "depth output dims must match the canvas");

        // Pixel-CENTER convention (matches `camera_conformance.rs`'s
        // `intensity_centroid`): pixel index `i` occupies continuous
        // range `[i, i+1)`, centred at `i + 0.5`. The containing index
        // for a continuous coordinate is `floor`, not `round` — `round`
        // would map `i + 0.5` (a pixel's OWN exact centre) to `i + 1`.
        let px = (oracle.px.floor() as i64).clamp(0, width as i64 - 1) as u32;
        let py = (oracle.py.floor() as i64).clamp(0, height as i64 - 1) as u32;
        let sampled = sample_r32float(&bytes, width, px, py);

        assert!(
            sampled.is_finite(),
            "distance {distance}: sampled depth at ({px},{py}) is not finite: {sampled}"
        );
        assert!(
            (sampled - oracle.depth).abs() < 1e-5,
            "distance {distance}: sampled depth {sampled} at ({px},{py}) vs oracle \
             {} — diff {} exceeds 1e-5",
            oracle.depth,
            (sampled - oracle.depth).abs()
        );

        // I3 cross-check, in the same fixture: the shared linearize_depth
        // helper recovers this camera's own `distance` from the SAME raw
        // sample that just passed the raw-depth conformance check above.
        let lin = linearize_depth(sampled, cam.near, cam.far);
        assert!(
            (lin - distance).abs() < 1e-2,
            "distance {distance}: linearize_depth({sampled}, {}, {}) = {lin}, expected ~{distance}",
            cam.near,
            cam.far,
        );
    }
}

#[test]
fn gbuffer_depth_unwired_scene_bundled_smoke_stays_finite() {
    // Sanity companion to the I1 compile-only gate in render_scene.rs:
    // an ordinary scene that never wires `depth` (every bundled 3D preset
    // today) still renders a finite `color` frame — i.e. adding the port
    // didn't perturb the unwired path's own dims/target resolution.
    let h = harness::shared();
    let registry = PrimitiveRegistry::with_builtin();
    let json = r#"{"version":2,"name":"GbufferDepthUnwiredSmoke","nodes":[
        {"id":0,"typeId":"system.generator_input","nodeId":"input"},
        {"id":1,"typeId":"node.grid_mesh","nodeId":"grid","params":{
            "max_capacity":{"type":"Int","value":16},
            "resolution_x":{"type":"Int","value":2},
            "resolution_y":{"type":"Int","value":2},
            "size_x":{"type":"Float","value":1.0},
            "size_y":{"type":"Float","value":1.0}}},
        {"id":2,"typeId":"node.make_triangles","nodeId":"tris","params":{
            "src_cols":{"type":"Int","value":2},
            "src_rows":{"type":"Int","value":2}}},
        {"id":3,"typeId":"node.orbit_camera","nodeId":"cam","params":{
            "orbit":{"type":"Float","value":0.3},
            "tilt":{"type":"Float","value":0.3},
            "distance":{"type":"Float","value":4.0},
            "fov_y":{"type":"Float","value":0.9}}},
        {"id":4,"typeId":"node.unlit_material","nodeId":"mat","params":{
            "color_r":{"type":"Float","value":1.0},
            "color_g":{"type":"Float","value":1.0},
            "color_b":{"type":"Float","value":1.0},
            "color_a":{"type":"Float","value":1.0}}},
        {"id":20,"typeId":"node.render_scene","nodeId":"scene","params":{
            "objects":{"type":"Int","value":1},
            "lights":{"type":"Int","value":0}}},
        {"id":99,"typeId":"system.final_output","nodeId":"out"}
        ],"wires":[
        {"fromNode":1,"fromPort":"vertices","toNode":2,"toPort":"in"},
        {"fromNode":2,"fromPort":"out","toNode":20,"toPort":"mesh_0"},
        {"fromNode":3,"fromPort":"out","toNode":20,"toPort":"camera"},
        {"fromNode":4,"fromPort":"out","toNode":20,"toPort":"material_0"},
        {"fromNode":20,"fromPort":"color","toNode":99,"toPort":"in"}
        ]}"#;
    let mut runtime = PresetRuntime::from_json_str_with_device(
        json,
        &registry,
        &h.device,
        h.width,
        h.height,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .unwrap_or_else(|e| panic!("unwired-depth smoke graph must build: {e}\n{json}"));

    let target = h.make_target("gbuffer-depth-unwired-smoke");
    let ctx = PresetContext {
        time: 0.0,
        beat: 0.0,
        dt: 1.0 / 60.0,
        width: h.width,
        height: h.height,
        output_width: h.width,
        output_height: h.height,
        aspect: h.width as f32 / h.height as f32,
        owner_key: 0,
        is_clip_level: false,
        frame_count: 0,
        anim_progress: 0.0,
        trigger_count: 0,
    };
    let mut enc = h.device.create_encoder("gbuffer-depth-unwired-enc");
    {
        let mut gpu = RendererGpuEncoder::new(&mut enc, &h.device);
        runtime.render(&mut gpu, &target.texture, &ctx, &manifold_core::params::ParamManifest::default());
    }
    enc.commit_and_wait_completed();

    let bytes = h.readback(&target.texture);
    for (i, px) in bytes.chunks_exact(8).enumerate() {
        for c in 0..4 {
            let v = f16::from_le_bytes([px[c * 2], px[c * 2 + 1]]).to_f32();
            assert!(v.is_finite(), "pixel {i} channel {c} is not finite: {v}");
        }
    }
}
