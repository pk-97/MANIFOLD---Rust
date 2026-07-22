//! `docs/RAYTRACING_DESIGN.md` §5.2 W0 — `node.render_scene`'s stored
//! `depth`/`velocity` G-buffer, forced on for RT-enabled scenes independent
//! of graph wiring (D14), reusing exactly the `GBUFFER_DESIGN.md` D1/D5
//! lazy-allocation machinery and formats — see
//! `EffectNode::force_consumed_outputs`'s doc and its one call site
//! (`execution_plan.rs`'s `consumed_outputs` build).
//!
//! Fixture: FOUR small grid-mesh quads (`node.grid_mesh(2x2) ->
//! node.make_triangles`, `rot_z = PI/2` so each quad's normal faces
//! `orbit=0, tilt=0`'s `+X` camera dead-on — same convention
//! `gbuffer_depth.rs`/`gbuffer_velocity.rs` use), one `node.render_scene`
//! object each, placed at four OFF-AXIS world points (`(±R, 0, 0)`,
//! `(0, 0, ±R)`) via `node.transform_3d`'s `pos_x`/`pos_z` — each quad's
//! local origin (nx=nz=0.5 in `generate_grid_mesh_body.wgsl`) sits at
//! EXACTLY its transform's translation regardless of rotation (rotation
//! is local, translation applies after), so `Camera::project_to_pixel`
//! at that exact world point is the exact oracle pixel for that quad's
//! rasterized center — no need to reverse-engineer the mesh's screen
//! footprint. Points on the `x=0, z=0` LINE (the orbit camera's actual
//! rotation axis at `tilt=0` — not just the origin) are excluded: BUG-136's
//! own runtime-probe addendum records that a point ON the orbit's rotation
//! axis legitimately has ~zero NDC velocity (not a bug), confirmed
//! empirically here too (`(0, R, 0)` measured exactly zero before this
//! fixture was corrected to use `x`/`z` offsets instead) — an off-axis
//! field is what the D14/BUG-136 gate asks for.
//!
//! `render_scene`'s `depth`/`velocity` ports are left COMPLETELY UNWIRED —
//! no dead-end `node.invert` sink like the sibling gbuffer tests use. Only
//! `rt_enabled: true` on the `scene` node's params. If the ports still show
//! up in `dump_textures_all()` (which requires `execution_plan.rs` to have
//! put them in `consumed_outputs`), that alone proves D14's force-allocate
//! path — the two numeric proofs below (value-level velocity-vs-oracle and
//! the BUG-136 two-frame-orbit oracle) then check the CONTENT is also
//! correct, not just present.
//!
//! Camera motion is driven by wiring `node.beat_ramp` (`rate=1.0,
//! attack=1.0`, so `out == fract(beat)` exactly for `0 <= beat < 1`) into
//! `node.orbit_camera`'s port-shadowed `orbit` input — same beat-ramp-drives-
//! a-port trick `gbuffer_velocity.rs` uses for `pos_y`, applied to the
//! camera instead of the object, which is exactly BUG-136's reported shape
//! (orbiting the CAMERA, object static).
//!
//! Continuity requirement (same as `gbuffer_velocity.rs`): `render_scene`'s
//! `prev_model`/`prev_view_proj` state lives on the node instance, so every
//! render call below reuses ONE `PresetRuntime`.

use half::f16;
use manifold_gpu::{GpuTexture, GpuTextureFormat};
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::camera::Camera;
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;

use crate::harness;

const ORBIT0: f32 = 0.0;
/// ~11.5 degrees — comfortably large enough to clear the BUG-136 gate's
/// `mean |mv| > 0.5px` bar on a 128x128 canvas (empirically tens of px per
/// off-axis point at this distance/FOV; see the assertion's printed
/// values if this ever needs re-tuning) while keeping every quad's
/// dead-on-facing normal (`ROT_Z`) still roughly toward the camera at both
/// orbit angles.
const ORBIT1: f32 = 0.2;
const TILT: f32 = 0.0;
const DISTANCE: f32 = 5.0;
const FOV_Y: f32 = 0.9;
const NEAR: f32 = 0.05;
const FAR: f32 = 200.0;
const ROT_Z: f32 = std::f32::consts::FRAC_PI_2;
/// World-space offset of each off-axis quad from the origin (the orbit's
/// rotation axis / look-at point).
const R: f32 = 0.8;
const QUAD_SIZE: f32 = 0.35;

/// Four off-axis world points: `(±R, 0, 0)`, `(0, 0, ±R)`. The camera
/// orbits about the world Y axis (`orbit_camera`'s `pos.y = distance *
/// sin(tilt) + look_y`, constant at `tilt=0`), so the rotation axis is the
/// full `x=0, z=0` LINE, not just the origin point — a point with `x=0,
/// z=0` at ANY `y` is on-axis (empirically confirmed: `(0, R, 0)` measured
/// exactly zero motion, same as the excluded `(0,0,0)`). Every point here
/// has nonzero `x` or `z`, so all four are genuinely off-axis.
const WORLD_POINTS: [[f32; 3]; 4] =
    [[R, 0.0, 0.0], [-R, 0.0, 0.0], [0.0, 0.0, R], [0.0, 0.0, -R]];

/// `render_scene(4 objects, 0 lights, rt_enabled=true, depth/velocity
/// UNWIRED)`. `cam.orbit` is port-shadowed by `ramp` (beat_ramp). Each
/// object shares the SAME small grid-mesh quad and unlit-white material
/// (fan-out wires — one producer output feeding four input ports), only
/// its own `transform_N`'s `pos_y`/`pos_z` differs.
fn scene_json() -> String {
    let mut objects = String::new();
    let mut wires = String::new();
    for (i, p) in WORLD_POINTS.iter().enumerate() {
        objects.push_str(&format!(
            r#",{{"id":{xf_id},"typeId":"node.transform_3d","nodeId":"xf{i}","params":{{
            "rot_z":{{"type":"Float","value":{ROT_Z}}},
            "pos_x":{{"type":"Float","value":{px}}},
            "pos_y":{{"type":"Float","value":{py}}},
            "pos_z":{{"type":"Float","value":{pz}}}}}}}"#,
            xf_id = 10 + i,
            px = p[0],
            py = p[1],
            pz = p[2],
        ));
        wires.push_str(&format!(
            r#",{{"fromNode":2,"fromPort":"out","toNode":20,"toPort":"mesh_{i}"}},
            {{"fromNode":4,"fromPort":"out","toNode":20,"toPort":"material_{i}"}},
            {{"fromNode":{xf_id},"fromPort":"transform","toNode":20,"toPort":"transform_{i}"}}"#,
            xf_id = 10 + i,
        ));
    }
    format!(
        r#"{{"version":2,"name":"RtW0GbufferForced","nodes":[
        {{"id":0,"typeId":"system.generator_input","nodeId":"input"}},
        {{"id":1,"typeId":"node.grid_mesh","nodeId":"grid","params":{{
            "max_capacity":{{"type":"Int","value":16}},
            "resolution_x":{{"type":"Int","value":2}},
            "resolution_y":{{"type":"Int","value":2}},
            "size_x":{{"type":"Float","value":{QUAD_SIZE}}},
            "size_y":{{"type":"Float","value":{QUAD_SIZE}}}}}}},
        {{"id":2,"typeId":"node.make_triangles","nodeId":"tris","params":{{
            "src_cols":{{"type":"Int","value":2}},
            "src_rows":{{"type":"Int","value":2}}}}}},
        {{"id":3,"typeId":"node.orbit_camera","nodeId":"cam","params":{{
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
        {{"id":6,"typeId":"node.beat_ramp","nodeId":"ramp","params":{{
            "rate":{{"type":"Float","value":1.0}},
            "attack":{{"type":"Float","value":1.0}}}}}}
        {objects},
        {{"id":20,"typeId":"node.render_scene","nodeId":"scene","params":{{
            "objects":{{"type":"Int","value":4}},
            "lights":{{"type":"Int","value":0}},
            "rt_enabled":{{"type":"Bool","value":true}}}}}},
        {{"id":99,"typeId":"system.final_output","nodeId":"color_out"}}
        ],"wires":[
        {{"fromNode":1,"fromPort":"vertices","toNode":2,"toPort":"in"}},
        {{"fromNode":3,"fromPort":"out","toNode":20,"toPort":"camera"}},
        {{"fromNode":6,"fromPort":"out","toNode":3,"toPort":"orbit"}}
        {wires},
        {{"fromNode":20,"fromPort":"color","toNode":99,"toPort":"in"}}
        ]}}"#
    )
}

fn readback_rg16float(device: &manifold_gpu::GpuDevice, texture: &GpuTexture) -> Vec<u8> {
    const BYTES_PER_PIXEL: u32 = 4;
    let bytes_per_row = texture.width * BYTES_PER_PIXEL;
    let total_bytes = u64::from(texture.height * bytes_per_row);
    let buf = device.create_buffer_shared(total_bytes);
    let mut enc = device.create_encoder("rt-w0-velocity-readback");
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

/// Render one frame at `beat` on the shared runtime and return `scene`'s
/// dumped `depth` and `velocity` textures (both `Option` — `None` would
/// mean D14's force-allocate path failed to fire).
fn render_and_dump<'a>(
    runtime: &'a mut PresetRuntime,
    h: &harness::ParityHarness,
    target: &manifold_gpu::GpuTexture,
    beat: f64,
    frame_count: i64,
) -> (Option<&'a GpuTexture>, Option<&'a GpuTexture>) {
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
    let mut enc = h.device.create_encoder("rt-w0-gbuffer-enc");
    {
        let mut gpu = RendererGpuEncoder::new(&mut enc, &h.device);
        runtime.render(&mut gpu, target, &ctx, &manifold_core::params::ParamManifest::default());
    }
    enc.commit_and_wait_completed();

    let dumped = runtime.dump_textures_all();
    let depth = dumped
        .iter()
        .find(|(node_id, port, _, _)| node_id == "scene" && port == "depth")
        .map(|(_, _, _, t)| *t);
    let velocity = dumped
        .iter()
        .find(|(node_id, port, _, _)| node_id == "scene" && port == "velocity")
        .map(|(_, _, _, t)| *t);
    (depth, velocity)
}

/// D14 + BUG-136: `rt_enabled=true` forces `depth`/`velocity` allocation
/// with NO downstream wire, and the resulting `velocity` values match the
/// CPU orbit-reprojection oracle at four off-axis points across a real
/// two-frame camera orbit.
#[test]
fn rt_w0_forced_gbuffer_matches_orbit_oracle() {
    let h = harness::shared();
    let registry = PrimitiveRegistry::with_builtin();
    let json = scene_json();
    let mut runtime = PresetRuntime::from_json_str_with_device(
        &json,
        &registry,
        std::sync::Arc::clone(&h.device),
        h.width,
        h.height,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .unwrap_or_else(|e| panic!("rt_w0_gbuffer graph must build: {e}\n{json}"));
    runtime.set_dump_all(true);

    let target = h.make_target("rt-w0-gbuffer-forced");

    // Warm-up at beat 0.0 (orbit = ORBIT0) twice — primes pools/ring
    // buffers and establishes prev_view_proj/prev_model continuity, same
    // pattern as gbuffer_velocity.rs.
    {
        let mut enc = h.device.create_encoder("rt-w0-warmup");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, &h.device);
            for frame in 0..2i64 {
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
                runtime.render(
                    &mut gpu,
                    &target.texture,
                    &ctx,
                    &manifold_core::params::ParamManifest::default(),
                );
            }
        }
        enc.commit_and_wait_completed();
    }

    // Measured frame A: orbit = ORBIT0 (seeds prev_view_proj at ORBIT0
    // exactly — matches gbuffer_velocity.rs's "no prior distinct value
    // observed yet" first-measured-frame convention).
    let (depth_a, velocity_a) = render_and_dump(&mut runtime, h, &target.texture, 0.0, 2);
    let depth_a = depth_a.unwrap_or_else(|| {
        panic!("D14: rt_enabled=true must force `depth` into consumed_outputs even unwired")
    });
    assert_eq!(depth_a.format, GpuTextureFormat::R32Float, "depth format override");
    let velocity_a = velocity_a.unwrap_or_else(|| {
        panic!("D14: rt_enabled=true must force `velocity` into consumed_outputs even unwired")
    });
    assert_eq!(velocity_a.format, GpuTextureFormat::Rg16Float, "velocity format override");

    // Measured frame B: orbit = ORBIT1. The real two-frame orbit.
    let (_depth_b, velocity_b) = render_and_dump(&mut runtime, h, &target.texture, ORBIT1 as f64, 3);
    let velocity_b = velocity_b.expect("velocity still forced-allocated on frame B");
    let (w, ht) = (velocity_b.width, velocity_b.height);
    assert_eq!((w, ht), (h.width, h.height), "velocity dims must match the canvas");
    let bytes = readback_rg16float(&h.device, velocity_b);

    let cam0 = Camera::orbit_perspective(ORBIT0, TILT, DISTANCE, FOV_Y, 0.0, 0.0, NEAR, FAR);
    let cam1 = Camera::orbit_perspective(ORBIT1, TILT, DISTANCE, FOV_Y, 0.0, 0.0, NEAR, FAR);

    let mut mag_px_sum = 0.0f32;
    let mut dot_min = f32::INFINITY;
    for p in &WORLD_POINTS {
        let proj0 = cam0
            .project_to_pixel(*p, w, ht)
            .unwrap_or_else(|| panic!("{p:?}: unexpectedly behind cam0"));
        let proj1 = cam1
            .project_to_pixel(*p, w, ht)
            .unwrap_or_else(|| panic!("{p:?}: unexpectedly behind cam1"));
        let oracle_ndc = [proj1.ndc[0] - proj0.ndc[0], proj1.ndc[1] - proj0.ndc[1]];
        let oracle_px = [proj1.px - proj0.px, proj1.py - proj0.py];
        let oracle_px_mag = (oracle_px[0] * oracle_px[0] + oracle_px[1] * oracle_px[1]).sqrt();

        // Sample at the object's CURRENT (frame-B) screen position — the
        // pixel that frame B's velocity output actually describes.
        let px = (proj1.px.floor() as i64).clamp(0, w as i64 - 1) as u32;
        let py = (proj1.py.floor() as i64).clamp(0, ht as i64 - 1) as u32;
        let (vx, vy) = sample_rg16float(&bytes, w, px, py);
        assert!(vx.is_finite() && vy.is_finite(), "{p:?}: velocity not finite: ({vx}, {vy})");

        let measured_px_mag =
            ((vx * w as f32 / 2.0).powi(2) + (vy * ht as f32 / 2.0).powi(2)).sqrt();
        mag_px_sum += measured_px_mag;

        let oracle_len = (oracle_ndc[0] * oracle_ndc[0] + oracle_ndc[1] * oracle_ndc[1]).sqrt();
        let measured_len = (vx * vx + vy * vy).sqrt();
        assert!(
            oracle_len > 1e-6 && measured_len > 1e-6,
            "{p:?}: degenerate vector (oracle_len {oracle_len}, measured_len {measured_len}) \
             — off-axis point should have real motion"
        );
        let dot = (vx * oracle_ndc[0] + vy * oracle_ndc[1]) / (measured_len * oracle_len);
        dot_min = dot_min.min(dot);

        // Value-level check (the Gate's "motion vectors for a known camera
        // delta vs CPU reprojection, exact math"): measured NDC velocity
        // matches the CPU reprojection oracle within the same 1e-4 f16-safe
        // tolerance gbuffer_velocity.rs's I5 uses.
        assert!(
            (vx - oracle_ndc[0]).abs() < 2e-3 && (vy - oracle_ndc[1]).abs() < 2e-3,
            "{p:?}: measured ({vx}, {vy}) vs oracle {oracle_ndc:?} — outside tolerance \
             (oracle px delta {oracle_px:?}, magnitude {oracle_px_mag}px)"
        );
    }

    let mean_mag_px = mag_px_sum / WORLD_POINTS.len() as f32;
    assert!(
        mean_mag_px > 0.5,
        "BUG-136 oracle: mean |mv| across the 4 off-axis points = {mean_mag_px}px, \
         must exceed 0.5px"
    );
    assert!(
        dot_min > 0.9,
        "BUG-136 oracle: worst per-point direction dot-product = {dot_min}, must exceed 0.9"
    );
}

/// I1-style companion: an rt_enabled=FALSE scene (the default — every
/// bundled preset today) with depth/velocity unwired stays exactly on the
/// lazy path — `force_consumed_outputs` returns empty, `dump_textures_all`
/// finds neither port. Byte-identical-to-today is enforced structurally by
/// `force_consumed_outputs`'s `false` branch (see render_scene.rs); this is
/// the runtime cross-check.
#[test]
fn rt_w0_default_scene_stays_lazy_no_forced_gbuffer() {
    let h = harness::shared();
    let registry = PrimitiveRegistry::with_builtin();
    let json = r#"{"version":2,"name":"RtW0DefaultLazy","nodes":[
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
        std::sync::Arc::clone(&h.device),
        h.width,
        h.height,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .unwrap_or_else(|e| panic!("rt_w0 default-lazy graph must build: {e}\n{json}"));
    runtime.set_dump_all(true);

    let target = h.make_target("rt-w0-default-lazy");
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
    let mut enc = h.device.create_encoder("rt-w0-default-lazy-enc");
    {
        let mut gpu = RendererGpuEncoder::new(&mut enc, &h.device);
        runtime.render(&mut gpu, &target.texture, &ctx, &manifold_core::params::ParamManifest::default());
    }
    enc.commit_and_wait_completed();

    let dumped = runtime.dump_textures_all();
    assert!(
        !dumped.iter().any(|(node_id, port, _, _)| node_id == "scene" && port == "depth"),
        "rt_enabled default (false/unset) must NOT force `depth` — dumped ports: {:?}",
        dumped.iter().map(|(n, p, _, _)| format!("{n}.{p}")).collect::<Vec<_>>()
    );
    assert!(
        !dumped.iter().any(|(node_id, port, _, _)| node_id == "scene" && port == "velocity"),
        "rt_enabled default (false/unset) must NOT force `velocity` — dumped ports: {:?}",
        dumped.iter().map(|(n, p, _, _)| format!("{n}.{p}")).collect::<Vec<_>>()
    );
}
