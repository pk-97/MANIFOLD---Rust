//! `docs/CAMERA_AND_LENS_DESIGN.md` I1 — every GPU projection path agrees
//! with `Camera::project_to_pixel` within 1.0 px (P1 gate).
//!
//! Design note on the fixture (a deliberate substitution from the phase
//! brief's literal "5 known world points, one camera" framing): there is no
//! primitive in this codebase that translates an `Array<MeshVertex>` by an
//! arbitrary world-space offset before `node.flatten_3d` (`node.grid_mesh` is
//! always origin-centred; `node.rotate_3d` rotates, never translates;
//! `node.push_along_normals` only shifts along the mesh's own normal, which
//! is a fixed `(0,1,0)` for a flat grid). Building 5 independently-translated
//! points would mean either inventing a new primitive (out of this phase's
//! scope) or chaining rotate+push in a way that's fragile to verify by hand.
//! Fixing the geometry at the world origin and varying the CAMERA across 5
//! configurations instead exercises exactly the same invariant — "does the
//! GPU path agree with `Camera::project_to_pixel` for this camera and this
//! point" — via 5 genuinely different (view_z, ndc) pairs (different orbit,
//! tilt, distance, fov_y, look_y), with zero new infrastructure. See the
//! session's escalation note for the fuller reasoning.
//!
//! Both sub-proofs render the SAME tiny (0.05 world-unit) quad, centred at
//! the world origin, through the SAME 5 cameras:
//! (a) `node.grid_mesh` → `node.flatten_3d(camera)` → `node.draw_lines`
//!     (`closed_loop=true`, drawing the quad's 4-corner outline as one
//!     compact blob — the "dot" the phase brief calls for, sized so its
//!     footprint stays a few pixels wide regardless of camera distance).
//! (b) The same quad → `node.make_triangles` → `node.render_scene` with an
//!     unlit white material — the same point rendered as geometry instead of
//!     a wireframe dot.
//! Centroid = intensity-weighted mean over the WHOLE readback buffer (only
//! one shape is ever drawn per frame, so no region-of-interest windowing is
//! needed) — arithmetic, not eyeballing. Both centroids are asserted within
//! 1.0 px of `Camera::project_to_pixel([0,0,0], W, H)`, which transitively
//! proves (a) and (b) agree with each other (I1c).

use half::f16;
use manifold_gpu::GpuTextureFormat;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::camera::Camera;
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;

use crate::harness;

/// One camera configuration: `(orbit, tilt, distance, fov_y, look_y)`, all
/// radians / world units, `roll = 0`, `near = 0.05`, `far = 200.0` for every
/// case (matching `node.orbit_camera`'s own defaults for the params this
/// fixture doesn't vary). Chosen to spread across distinct orbit angles,
/// tilts (including negative), distances, and FOVs — different `view_z`,
/// different `proj_f`, different NDC offsets per case.
// `tilt` is never exactly 0: the quad lies flat in the world XZ plane
// (`node.grid_mesh`'s Y=0 convention), so a `tilt = 0` camera sits in that
// same plane and views the quad exactly edge-on — zero projected area for
// render_scene's filled triangles (draw_lines' wireframe outline still
// renders at any tilt, since a line has thickness regardless of viewing
// angle, which is why this only broke the render_scene half — see the
// escalation note this session's report carries).
const CAMERAS: [(f32, f32, f32, f32, f32); 5] = [
    (0.0, 0.15, 5.0, 0.9, 0.0),
    (0.6, 0.3, 4.0, 0.9, 0.0),
    (1.2, 0.5, 8.0, 0.6, 0.2),
    (-0.8, -0.4, 3.0, 1.3, -0.3),
    (2.5, 0.9, 6.0, 0.4, 0.5),
];

const NEAR: f32 = 0.05;
const FAR: f32 = 200.0;

/// The one fixed world point every camera looks at — the tiny quad's
/// geometric centre. `node.grid_mesh` centres at the world origin with no
/// translate primitive available, so the fixture keeps this point fixed and
/// varies the camera instead (see module doc).
const WORLD_POINT: [f32; 3] = [0.0, 0.0, 0.0];

/// Quad size in world units. Small enough that its footprint stays a
/// compact blob (not a visibly-extended shape) at every tested camera
/// distance, large enough that `node.render_scene`'s filled-triangle
/// rasterization reliably covers real MSAA sample points — draw_lines'
/// wireframe path has no such floor (a capsule SDF has a guaranteed minimum
/// pixel footprint regardless of world size) but plain triangle rasterization
/// of a sub-pixel triangle can legitimately cover zero samples, which a
/// smaller value here (0.05) hit for every camera in this fixture.
const QUAD_SIZE: f32 = 0.3;

fn camera_json_node(id: u32, node_id: &str, cam: (f32, f32, f32, f32, f32)) -> String {
    let (orbit, tilt, distance, fov_y, look_y) = cam;
    format!(
        r#"{{"id":{id},"typeId":"node.orbit_camera","nodeId":"{node_id}","params":{{
            "orbit":{{"type":"Float","value":{orbit}}},
            "tilt":{{"type":"Float","value":{tilt}}},
            "distance":{{"type":"Float","value":{distance}}},
            "fov_y":{{"type":"Float","value":{fov_y}}},
            "look_y":{{"type":"Float","value":{look_y}}},
            "roll":{{"type":"Float","value":0.0}},
            "near":{{"type":"Float","value":{NEAR}}},
            "far":{{"type":"Float","value":{FAR}}}}}}}"#
    )
}

/// `grid_mesh(2x2, size_x=size_y=QUAD_SIZE) -> flatten_3d(camera) ->
/// draw_lines(closed_loop=true)` — the quad's 4-corner outline drawn as one
/// compact blob centred on `WORLD_POINT`.
fn flatten_3d_scene_json(cam: (f32, f32, f32, f32, f32)) -> String {
    let cam_node = camera_json_node(2, "cam", cam);
    format!(
        r#"{{"version":2,"name":"CameraConformanceFlatten3D","nodes":[
        {{"id":0,"typeId":"system.generator_input","nodeId":"input"}},
        {{"id":1,"typeId":"node.grid_mesh","nodeId":"grid","params":{{
            "max_capacity":{{"type":"Int","value":16}},
            "resolution_x":{{"type":"Int","value":2}},
            "resolution_y":{{"type":"Int","value":2}},
            "size_x":{{"type":"Float","value":{QUAD_SIZE}}},
            "size_y":{{"type":"Float","value":{QUAD_SIZE}}}}}}},
        {cam_node},
        {{"id":3,"typeId":"node.flatten_3d","nodeId":"proj","params":{{
            "mode":{{"type":"Enum","value":0}}}}}},
        {{"id":4,"typeId":"node.draw_lines","nodeId":"lines","params":{{
            "edge_thickness":{{"type":"Float","value":0.03}},
            "closed_loop":{{"type":"Bool","value":true}},
            "show_verts":{{"type":"Bool","value":false}},
            "beat_flash_amount":{{"type":"Float","value":0.0}},
            "color_r":{{"type":"Float","value":1.0}},
            "color_g":{{"type":"Float","value":1.0}},
            "color_b":{{"type":"Float","value":1.0}},
            "color_a":{{"type":"Float","value":1.0}}}}}},
        {{"id":99,"typeId":"system.final_output","nodeId":"out"}}
        ],"wires":[
        {{"fromNode":1,"fromPort":"vertices","toNode":3,"toPort":"in"}},
        {{"fromNode":2,"fromPort":"out","toNode":3,"toPort":"camera"}},
        {{"fromNode":3,"fromPort":"out","toNode":4,"toPort":"points"}},
        {{"fromNode":4,"fromPort":"color","toNode":99,"toPort":"in"}}
        ]}}"#
    )
}

/// The same quad rendered as geometry: `grid_mesh -> make_triangles ->
/// render_scene` (one unlit-white object, no lights, `transform_0` unwired =
/// identity, so the object renders exactly where `grid_mesh` places it —
/// centred on `WORLD_POINT`).
fn render_scene_json(cam: (f32, f32, f32, f32, f32)) -> String {
    let cam_node = camera_json_node(3, "cam", cam);
    format!(
        r#"{{"version":2,"name":"CameraConformanceRenderScene","nodes":[
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
        {cam_node},
        {{"id":4,"typeId":"node.unlit_material","nodeId":"mat","params":{{
            "color_r":{{"type":"Float","value":1.0}},
            "color_g":{{"type":"Float","value":1.0}},
            "color_b":{{"type":"Float","value":1.0}},
            "color_a":{{"type":"Float","value":1.0}}}}}},
        {{"id":20,"typeId":"node.render_scene","nodeId":"scene","params":{{
            "objects":{{"type":"Int","value":1}},
            "lights":{{"type":"Int","value":0}}}}}},
        {{"id":99,"typeId":"system.final_output","nodeId":"out"}}
        ],"wires":[
        {{"fromNode":1,"fromPort":"vertices","toNode":2,"toPort":"in"}},
        {{"fromNode":2,"fromPort":"out","toNode":20,"toPort":"mesh_0"}},
        {{"fromNode":3,"fromPort":"out","toNode":20,"toPort":"camera"}},
        {{"fromNode":4,"fromPort":"out","toNode":20,"toPort":"material_0"}},
        {{"fromNode":20,"fromPort":"color","toNode":99,"toPort":"in"}}
        ]}}"#
    )
}

/// Build, render two warm-up + one measured frame, and read back a preset
/// graph's `Rgba16Float` output. Mirrors `render_scene_lights.rs`'s
/// `render_scene_readback` helper.
fn render_readback(json: &str) -> Vec<u8> {
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
    .unwrap_or_else(|e| panic!("camera_conformance graph must build: {e}\n{json}"));

    let target = h.make_target("camera-conformance");
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
        let mut enc = h.device.create_encoder("camera-conformance-enc");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, &h.device);
            runtime.render(&mut gpu, &target.texture, &ctx, &manifold_core::params::ParamManifest::default());
        }
        enc.commit_and_wait_completed();
    }
    h.readback(&target.texture)
}

/// Intensity-weighted centroid (pixel-CENTER convention: pixel `(c, r)`'s
/// weight is sampled at `(c + 0.5, r + 0.5)`, matching how
/// `Camera::project_to_pixel`'s continuous `px`/`py` relate to a rasterized
/// shape's mass distribution) over an `Rgba16Float` readback. Weight = R+G+B
/// (the fixture only ever draws white). Returns `None` when nothing rendered
/// (all-zero frame) rather than dividing by zero.
fn intensity_centroid(bytes: &[u8], width: u32, height: u32) -> Option<(f32, f32)> {
    let mut sum_w = 0.0f64;
    let mut sum_wx = 0.0f64;
    let mut sum_wy = 0.0f64;
    for (i, px) in bytes.chunks_exact(8).enumerate() {
        let r = f16::from_le_bytes([px[0], px[1]]).to_f32();
        let g = f16::from_le_bytes([px[2], px[3]]).to_f32();
        let b = f16::from_le_bytes([px[4], px[5]]).to_f32();
        assert!(r.is_finite() && g.is_finite() && b.is_finite(), "non-finite pixel at {i}");
        let w = (r + g + b).max(0.0) as f64;
        if w == 0.0 {
            continue;
        }
        let col = (i as u32) % width;
        let row = (i as u32) / width;
        let x = col as f64 + 0.5;
        let y = row as f64 + 0.5;
        sum_w += w;
        sum_wx += w * x;
        sum_wy += w * y;
    }
    let _ = height;
    if sum_w < 1e-6 {
        return None;
    }
    Some(((sum_wx / sum_w) as f32, (sum_wy / sum_w) as f32))
}

#[test]
fn flatten_3d_camera_mode_matches_project_to_pixel_oracle() {
    let h = harness::shared();
    for &cam_params in &CAMERAS {
        let (orbit, tilt, distance, fov_y, look_y) = cam_params;
        let cam = Camera::orbit_perspective(orbit, tilt, distance, fov_y, look_y, 0.0, NEAR, FAR);
        let oracle = cam
            .project_to_pixel(WORLD_POINT, h.width, h.height)
            .unwrap_or_else(|| panic!("camera {cam_params:?}: WORLD_POINT unexpectedly behind camera"));

        let bytes = render_readback(&flatten_3d_scene_json(cam_params));
        let (mx, my) = intensity_centroid(&bytes, h.width, h.height)
            .unwrap_or_else(|| panic!("camera {cam_params:?}: flatten_3d/draw_lines rendered nothing"));

        let dist = ((mx - oracle.px).powi(2) + (my - oracle.py).powi(2)).sqrt();
        assert!(
            dist < 1.0,
            "camera {cam_params:?}: flatten_3d centroid ({mx:.3},{my:.3}) vs oracle \
             ({:.3},{:.3}) — {dist:.3}px apart (limit 1.0px)",
            oracle.px,
            oracle.py
        );
    }
}

#[test]
fn render_scene_matches_project_to_pixel_oracle() {
    let h = harness::shared();
    for &cam_params in &CAMERAS {
        let (orbit, tilt, distance, fov_y, look_y) = cam_params;
        let cam = Camera::orbit_perspective(orbit, tilt, distance, fov_y, look_y, 0.0, NEAR, FAR);
        let oracle = cam
            .project_to_pixel(WORLD_POINT, h.width, h.height)
            .unwrap_or_else(|| panic!("camera {cam_params:?}: WORLD_POINT unexpectedly behind camera"));

        let bytes = render_readback(&render_scene_json(cam_params));
        let (mx, my) = intensity_centroid(&bytes, h.width, h.height)
            .unwrap_or_else(|| panic!("camera {cam_params:?}: render_scene rendered nothing"));

        let dist = ((mx - oracle.px).powi(2) + (my - oracle.py).powi(2)).sqrt();
        assert!(
            dist < 1.0,
            "camera {cam_params:?}: render_scene centroid ({mx:.3},{my:.3}) vs oracle \
             ({:.3},{:.3}) — {dist:.3}px apart (limit 1.0px)",
            oracle.px,
            oracle.py
        );
    }
}

/// I1c: both GPU paths agree with each other, transitively, because both
/// were just independently shown to agree with the SAME oracle point per
/// camera above. This test re-renders both and asserts they agree with each
/// other directly (not just via the oracle transitively) as an explicit,
/// separate proof.
#[test]
fn flatten_3d_and_render_scene_agree_with_each_other() {
    let h = harness::shared();
    for &cam_params in &CAMERAS {
        let a_bytes = render_readback(&flatten_3d_scene_json(cam_params));
        let b_bytes = render_readback(&render_scene_json(cam_params));
        let (ax, ay) = intensity_centroid(&a_bytes, h.width, h.height)
            .unwrap_or_else(|| panic!("camera {cam_params:?}: flatten_3d rendered nothing"));
        let (bx, by) = intensity_centroid(&b_bytes, h.width, h.height)
            .unwrap_or_else(|| panic!("camera {cam_params:?}: render_scene rendered nothing"));
        let dist = ((ax - bx).powi(2) + (ay - by).powi(2)).sqrt();
        assert!(
            dist < 1.0,
            "camera {cam_params:?}: flatten_3d centroid ({ax:.3},{ay:.3}) vs render_scene \
             centroid ({bx:.3},{by:.3}) — {dist:.3}px apart (limit 1.0px)"
        );
    }
}
