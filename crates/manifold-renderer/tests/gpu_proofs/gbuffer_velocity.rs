//! `docs/GBUFFER_DESIGN.md` I5 — `node.render_scene`'s stored `velocity`
//! output: first-frame velocity is EXACTLY zero, and a translated object's
//! second-frame velocity matches the CPU NDC-delta oracle within 1e-4.
//!
//! Same fixture shape as `gbuffer_depth.rs` (grid_mesh(2x2) quad, rotated
//! 90° about Z so its normal faces the `orbit=0, tilt=0` camera on `+X`
//! dead-on) plus one addition: `node.transform_3d`'s `pos_y` port-shadow is
//! wired to `node.beat_ramp` (`rate=1.0, attack=1.0`, so `out ==
//! fract(beat)` exactly whenever `0 <= beat < 1` — no clamp engages),
//! giving the object a beat-driven, exactly-computable Y position. The
//! camera is static across every render call — this isolates the
//! rigid-OBJECT-motion term of D5's `vel = camera + rigid motion` formula,
//! which is exactly what I5 asks for ("velocity of a translated object");
//! it does not exercise the camera-motion term (no gate requires that).
//!
//! `velocity` reads back through the same `node.invert` dead-end trick
//! `gbuffer_depth.rs` uses for `depth` (module doc there; the underlying
//! fix is `execution_plan.rs`'s `consumed_outputs`, BUG-125): wiring
//! `velocity -> node.invert.in` gives it a genuine step-output binding
//! without a second `system.final_output`, whose presence would make
//! `PresetRuntime`'s single-output tracking nondeterministic.
//!
//! Continuity requirement: `render_scene`'s `prev_model`/`prev_view_proj`
//! state lives on the node instance, so all three render calls below reuse
//! ONE `PresetRuntime` — a fresh runtime per call would make every call
//! look like "no history yet".

use half::f16;
use manifold_gpu::{GpuTexture, GpuTextureFormat};
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::camera::Camera;
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;

use crate::harness;

const ORBIT: f32 = 0.0;
const TILT: f32 = 0.0;
const DISTANCE: f32 = 5.0;
const FOV_Y: f32 = 0.9;
const NEAR: f32 = 0.05;
const FAR: f32 = 200.0;
/// Matches `gbuffer_depth.rs`'s `ROT_Z` — rotates the quad's normal from
/// grid_mesh's native `+Y` to `+X`, directly facing the `orbit=0, tilt=0`
/// camera (which sits on `+X` looking toward `-X`). Translating the
/// quad along world Y then shifts it laterally on screen (roughly the
/// image-plane "up" axis for this camera), not along the view axis.
const ROT_Z: f32 = std::f32::consts::FRAC_PI_2;
/// `pos_y` at the "measured frame 1" beat. Small on purpose: `velocity` is
/// `Rg16Float` (D5 — committed format, not a test choice), and f16's
/// precision is RELATIVE, not absolute — its quantization step at a value
/// scales with that value's own magnitude. `POS_Y_FRAME1 = 0.5` (measured
/// empirically) produces an NDC delta around 0.2, whose f16 ulp
/// (~1.2e-4) straddles the I5 gate's 1e-4 tolerance and fails on
/// quantization alone, not a computation error (confirmed: the sampled
/// value differs from the oracle by LESS than one f16 ulp at that
/// magnitude). `0.02` produces an NDC delta around 0.008, whose f16 ulp
/// (~7.6e-6) sits safely inside the gate — still a real, finite,
/// non-zero measured motion, just scaled so the format's own precision
/// doesn't dominate the comparison.
const POS_Y_FRAME1: f32 = 0.02;
/// `beat_ramp` with `rate=1.0, attack=1.0` emits `out == fract(beat)`
/// exactly for `0 <= beat < 1` (no clamp engages) — so this beat value
/// drives `pos_y` to exactly `POS_Y_FRAME1`.
const BEAT_FRAME1: f32 = POS_Y_FRAME1;

fn quad_size() -> f32 {
    0.1 * DISTANCE
}

/// `grid_mesh(2x2) -> make_triangles -> render_scene(1 object, 0 lights,
/// unlit white, transform_0 = rot_z(PI/2) + pos_y port-shadowed by
/// beat_ramp)`. `color` feeds the sole `system.final_output`; `velocity`
/// feeds `node.invert` (never wired further — a dead end that still
/// counts as "wired" at the `execution_plan.rs` level, per module doc).
fn scene_json() -> String {
    let size = quad_size();
    format!(
        r#"{{"version":2,"name":"GbufferVelocityConformance","nodes":[
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
            "distance":{{"type":"Float","value":{DISTANCE}}},
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
        {{"id":6,"typeId":"node.beat_ramp","nodeId":"ramp","params":{{
            "rate":{{"type":"Float","value":1.0}},
            "attack":{{"type":"Float","value":1.0}}}}}},
        {{"id":20,"typeId":"node.render_scene","nodeId":"scene","params":{{
            "objects":{{"type":"Int","value":1}},
            "lights":{{"type":"Int","value":0}}}}}},
        {{"id":21,"typeId":"node.invert","nodeId":"vel_sink","params":{{}}}},
        {{"id":99,"typeId":"system.final_output","nodeId":"color_out"}}
        ],"wires":[
        {{"fromNode":1,"fromPort":"vertices","toNode":2,"toPort":"in"}},
        {{"fromNode":2,"fromPort":"out","toNode":20,"toPort":"mesh_0"}},
        {{"fromNode":3,"fromPort":"out","toNode":20,"toPort":"camera"}},
        {{"fromNode":4,"fromPort":"out","toNode":20,"toPort":"material_0"}},
        {{"fromNode":6,"fromPort":"out","toNode":5,"toPort":"pos_y"}},
        {{"fromNode":5,"fromPort":"transform","toNode":20,"toPort":"transform_0"}},
        {{"fromNode":20,"fromPort":"color","toNode":99,"toPort":"in"}},
        {{"fromNode":20,"fromPort":"velocity","toNode":21,"toPort":"in"}}
        ]}}"#
    )
}

/// Read an `Rg16Float` texture back to host memory as `(vx, vy)` f32 pairs,
/// row-major. Mirrors `gbuffer_depth.rs`'s `readback_r32float` but for the
/// 2-channel f16 velocity format (4 bytes/pixel, like R32Float's byte count
/// but a different channel layout).
fn readback_rg16float(device: &manifold_gpu::GpuDevice, texture: &GpuTexture) -> Vec<u8> {
    const BYTES_PER_PIXEL: u32 = 4;
    let bytes_per_row = texture.width * BYTES_PER_PIXEL;
    let total_bytes = u64::from(texture.height * bytes_per_row);
    let buf = device.create_buffer_shared(total_bytes);

    let mut enc = device.create_encoder("gbuffer-velocity-readback");
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

fn sample_rg16float(bytes: &[u8], width: u32, x: u32, y: u32) -> (f32, f32) {
    let idx = ((y * width + x) * 4) as usize;
    let vx = f16::from_le_bytes([bytes[idx], bytes[idx + 1]]).to_f32();
    let vy = f16::from_le_bytes([bytes[idx + 2], bytes[idx + 3]]).to_f32();
    (vx, vy)
}

#[test]
fn gbuffer_velocity_two_frame_conformance() {
    let h = harness::shared();
    let registry = PrimitiveRegistry::with_builtin();
    let json = scene_json();
    let mut runtime = PresetRuntime::from_json_str_with_device(
        &json,
        &registry,
        &h.device,
        h.width,
        h.height,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .unwrap_or_else(|e| panic!("gbuffer_velocity graph must build: {e}\n{json}"));
    runtime.set_dump_all(true);

    let target = h.make_target("gbuffer-velocity-conformance");

    // One render call at `beat`, returning the `velocity` port's raw
    // `Rg16Float` bytes + dims. Reuses the SAME `runtime` (and therefore
    // the SAME `RenderScene` node instance) across every call — the
    // `prev_model`/`prev_view_proj` continuity I5 depends on.
    let render_at_beat = |runtime: &mut PresetRuntime, beat: f64, frame_count: i64| -> (Vec<u8>, u32, u32) {
        let ctx = PresetContext {
            time: 0.0,
            beat,
            dt: 1.0 / 60.0,
            width: h.width,
            height: h.height,
            output_width: h.width,
            output_height: h.height,
            aspect: h.width as f32 / h.height as f32,
            owner_key: 0,
            is_clip_level: false,
            frame_count,
            anim_progress: 0.0,
            trigger_count: 0,
        };
        let mut enc = h.device.create_encoder("gbuffer-velocity-enc");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, &h.device);
            runtime.render(&mut gpu, &target.texture, &ctx, &manifold_core::params::ParamManifest::default());
        }
        enc.commit_and_wait_completed();

        let dumped = runtime.dump_textures_all();
        let (_, _, _, vel_tex): &(String, String, String, &GpuTexture) = dumped
            .iter()
            .find(|(node_id, port, _, _)| node_id == "scene" && port == "velocity")
            .unwrap_or_else(|| {
                panic!(
                    "no dumped `velocity` output on node `scene` — dumped ports: {:?}",
                    dumped.iter().map(|(n, p, _, _)| format!("{n}.{p}")).collect::<Vec<_>>()
                )
            });
        assert_eq!(
            vel_tex.format,
            GpuTextureFormat::Rg16Float,
            "velocity's allocated texture must be Rg16Float (output_format override)"
        );
        let bytes = readback_rg16float(&h.device, vel_tex);
        (bytes, vel_tex.width, vel_tex.height)
    };

    // Warm-up at beat 0.0 (pos_y = 0) — primes the texture pool / ring
    // buffers. Harmless to the frame-0 exactness check below: prev and
    // current both resolve to the SAME pos_y=0 model matrix regardless of
    // how many beat=0.0 calls precede the measured one (bit-identical
    // inputs subtract to bit-exact zero either way).
    render_at_beat(&mut runtime, 0.0, 0);
    render_at_beat(&mut runtime, 0.0, 1);

    // ---- I5, part 1: first-frame (in the sense of "no prior distinct
    // pos_y has been observed") velocity is EXACTLY zero. ----
    let cam = Camera::orbit_perspective(ORBIT, TILT, DISTANCE, FOV_Y, 0.0, 0.0, NEAR, FAR);
    let proj0 = cam
        .project_to_pixel([0.0, 0.0, 0.0], h.width, h.height)
        .unwrap_or_else(|| panic!("origin unexpectedly behind camera"));
    let px0 = (proj0.px.floor() as i64).clamp(0, h.width as i64 - 1) as u32;
    let py0 = (proj0.py.floor() as i64).clamp(0, h.height as i64 - 1) as u32;

    let (bytes0, w, ht) = render_at_beat(&mut runtime, 0.0, 2);
    assert_eq!((w, ht), (h.width, h.height), "velocity output dims must match the canvas");
    let (vx0, vy0) = sample_rg16float(&bytes0, w, px0, py0);
    assert_eq!(vx0, 0.0, "frame-0 velocity.x must be BIT-EXACT zero, not approximate — got {vx0}");
    assert_eq!(vy0, 0.0, "frame-0 velocity.y must be BIT-EXACT zero, not approximate — got {vy0}");

    // ---- I5, part 2: a translated object's velocity matches the CPU
    // NDC-delta oracle within 1e-4. ----
    let proj1 = cam
        .project_to_pixel([0.0, POS_Y_FRAME1, 0.0], h.width, h.height)
        .unwrap_or_else(|| panic!("translated point unexpectedly behind camera"));
    let oracle_vel = [proj1.ndc[0] - proj0.ndc[0], proj1.ndc[1] - proj0.ndc[1]];
    let px1 = (proj1.px.floor() as i64).clamp(0, h.width as i64 - 1) as u32;
    let py1 = (proj1.py.floor() as i64).clamp(0, h.height as i64 - 1) as u32;

    let (bytes1, w1, ht1) = render_at_beat(&mut runtime, BEAT_FRAME1 as f64, 3);
    assert_eq!((w1, ht1), (h.width, h.height), "velocity output dims must match the canvas");
    let (vx1, vy1) = sample_rg16float(&bytes1, w1, px1, py1);
    assert!(vx1.is_finite() && vy1.is_finite(), "frame-1 velocity is not finite: ({vx1}, {vy1})");
    assert!(
        (vx1 - oracle_vel[0]).abs() < 1e-4,
        "frame-1 velocity.x {vx1} vs oracle {} — diff {} exceeds 1e-4",
        oracle_vel[0],
        (vx1 - oracle_vel[0]).abs()
    );
    assert!(
        (vy1 - oracle_vel[1]).abs() < 1e-4,
        "frame-1 velocity.y {vy1} vs oracle {} — diff {} exceeds 1e-4",
        oracle_vel[1],
        (vy1 - oracle_vel[1]).abs()
    );
}

#[test]
fn gbuffer_velocity_unwired_scene_bundled_smoke_stays_finite() {
    // I1 re-proof companion (sibling of `gbuffer_depth_unwired_scene_
    // bundled_smoke_stays_finite`): an ordinary scene that never wires
    // `velocity` (every bundled 3D preset today) still renders a finite
    // `color` frame — adding the port didn't perturb the unwired path.
    let h = harness::shared();
    let registry = PrimitiveRegistry::with_builtin();
    let json = r#"{"version":2,"name":"GbufferVelocityUnwiredSmoke","nodes":[
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
    .unwrap_or_else(|e| panic!("unwired-velocity smoke graph must build: {e}\n{json}"));

    let target = h.make_target("gbuffer-velocity-unwired-smoke");
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
    let mut enc = h.device.create_encoder("gbuffer-velocity-unwired-enc");
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
